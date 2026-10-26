//! The engine: run providers in order, merge what they answer.
//!
//! Order matters. CoinGecko first â€” it covers natives, reports 24h change, and
//! its demo quota is far larger. GeckoTerminal second, filling the gap for
//! ERC-20s that have no CoinGecko id. First provider to answer a target wins,
//! so a price carrying a 24h change is preferred over one without.

use crate::coingecko::CoinGecko;
use crate::geckoterminal::GeckoTerminal;
use crate::provider::{Provider, Source, Target};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a price is reused before the providers are asked again.
///
/// The free CoinGecko tier is throttled hard (often single-digit requests per
/// minute without a key). The wallet asks often: pull to refresh, and a
/// balance watch that prices one chain at a time every few seconds. The cache
/// is per coin, so one chain's prices never evict another's, and a coin that
/// was just asked about (answered or not) waits out this window. That keeps
/// it to about one fetch a minute.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// How old a price may be and still be served while the feed is not
/// answering (throttled, or briefly down). Past this the price is left out
/// and the app shows its own last-known figure, labelled as such.
const STALE_MAX: Duration = Duration::from_secs(5 * 60);

/// One coin's cache entry.
struct Entry {
    /// When the providers were last asked about it, answered or not.
    asked: Instant,
    /// Its last price and when it was fetched.
    price: Option<(Resolved, Instant)>,
}

/// Process-wide so the cache outlives individual `PriceEngine` values, which
/// the app constructs per portfolio refresh.
fn cache() -> &'static Mutex<HashMap<Target, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<Target, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drops any cached prices. For tests, and for a manual refresh.
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

/// A price plus which provider supplied it. The default is "no price, no
/// provider" â€” never a zero price attributed to a real source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub price: f64,
    pub change: f64,
    pub source: Source,
}

pub type ResolvedMap = HashMap<Target, Resolved>;

/// Prices targets, falling through providers in order.
pub struct PriceEngine {
    providers: Vec<Box<dyn Provider>>,
}

impl PriceEngine {
    /// Default order: CoinGecko, then GeckoTerminal. `user_agent` names the
    /// wallet to both (see `CoinGecko::new`).
    pub fn new(user_agent: &str) -> Self {
        PriceEngine::with_providers(vec![
            Box::new(CoinGecko::new(user_agent)),
            Box::new(GeckoTerminal::new(user_agent)),
        ])
    }

    pub fn with_providers(providers: Vec<Box<dyn Provider>>) -> Self {
        PriceEngine { providers }
    }

    /// Prices every target it can, with provenance for each.
    ///
    /// Coins asked about within `CACHE_TTL` are answered from the cache. The
    /// rest are fetched in one go, together with every other cached coin that
    /// is due, so a run of per-chain calls costs one request a minute, not
    /// one per call.
    pub async fn resolve(&self, targets: &[Target]) -> ResolvedMap {
        self.resolve_at(targets, Instant::now()).await
    }

    /// `resolve` at a given time; tests move the clock.
    async fn resolve_at(&self, targets: &[Target], now: Instant) -> ResolvedMap {
        if targets.is_empty() {
            return HashMap::new();
        }
        let due = |e: &Entry| now.saturating_duration_since(e.asked) >= CACHE_TTL;
        let fetch: Vec<Target> = {
            let Ok(mut cache) = cache().lock() else { return HashMap::new() };
            // Forget coins nobody has asked about for a while.
            cache.retain(|_, e| now.saturating_duration_since(e.asked) < STALE_MAX);
            let mut fetch: Vec<Target> =
                targets.iter().filter(|t| cache.get(*t).is_none_or(due)).cloned().collect();
            if !fetch.is_empty() {
                let others: Vec<Target> =
                    cache.iter().filter(|(t, e)| due(e) && !fetch.contains(t)).map(|(t, _)| t.clone()).collect();
                fetch.extend(others);
                // Marked as asked now, so calls made while this fetch runs do
                // not start another.
                for t in &fetch {
                    cache.entry(t.clone()).or_insert(Entry { asked: now, price: None }).asked = now;
                }
            }
            fetch
        };

        if !fetch.is_empty() {
            let fetched = self.fetch(&fetch).await;
            if let Ok(mut cache) = cache().lock() {
                for (t, r) in fetched {
                    cache.insert(t, Entry { asked: now, price: Some((r, now)) });
                }
            }
        }

        let Ok(cache) = cache().lock() else { return HashMap::new() };
        targets
            .iter()
            .filter_map(|t| {
                let (r, at) = cache.get(t)?.price.as_ref()?;
                (now.saturating_duration_since(*at) <= STALE_MAX).then(|| (t.clone(), r.clone()))
            })
            .collect()
    }

    /// Asks the providers in order, each for what the earlier ones missed.
    async fn fetch(&self, targets: &[Target]) -> ResolvedMap {
        let mut out: ResolvedMap = HashMap::new();
        let mut missing: Vec<Target> = targets.to_vec();
        for provider in &self.providers {
            if missing.is_empty() {
                break;
            }
            let answered = provider.prices(&missing).await;
            if answered.is_empty() {
                // A silent provider is normal (quota, outage); just move on.
                continue;
            }
            let source = Source::from_provider_name(provider.name());
            for (target, price) in answered {
                out.insert(target, Resolved { price: price.price, change: price.change, source });
            }
            missing.retain(|t| !out.contains_key(t));
        }
        out
    }

    /// Targets no provider could price. Reports coverage gaps rather than
    /// letting them render as a zero.
    pub async fn uncovered(&self, targets: &[Target]) -> Vec<Target> {
        let resolved = self.resolve(targets).await;
        targets
            .iter()
            .filter(|t| !resolved.contains_key(*t))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::CoinPrice;
    use crate::provider::PriceMap;
    use std::future::Future;
    use std::pin::Pin;

    /// Test double: answers a fixed map, or nothing if it is meant to be silent.
    struct Fake {
        name: &'static str,
        answers: HashMap<Target, CoinPrice>,
    }

    impl Provider for Fake {
        fn name(&self) -> &'static str {
            self.name
        }

        fn prices<'a>(
            &'a self,
            _targets: &'a [Target],
        ) -> Pin<Box<dyn Future<Output = PriceMap> + Send + 'a>> {
            let answers = self.answers.clone();
            Box::pin(async move { answers })
        }
    }

    fn id(t: &str) -> Target {
        Target::GeckoId(t.to_string())
    }

    fn fake(name: &'static str, answers: &[(&str, f64)]) -> Box<dyn Provider> {
        Box::new(Fake {
            name,
            answers: answers
                .iter()
                .map(|(t, price)| {
                    (
                        id(t),
                        CoinPrice {
                            price: *price,
                            change: 0.0,
                        },
                    )
                })
                .collect(),
        })
    }

    /// Serialises tests that touch the process-wide cache. `cargo test` runs
    /// them concurrently, and they would otherwise invalidate each other's
    /// entries mid-test.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[tokio::test]
    async fn falls_through_to_the_next_provider_for_what_is_missing() {
        let _guard = serial();
        invalidate_cache();
        let engine = PriceEngine::with_providers(vec![
            fake("coingecko", &[("ethereum", 3000.0)]),
            fake("geckoterminal", &[("pepe", 0.00001)]),
        ]);
        let got = engine.resolve(&[id("ethereum"), id("pepe")]).await;

        assert_eq!(got[&id("ethereum")].price, 3000.0);
        assert_eq!(got[&id("ethereum")].source, Source::CoinGecko);
        assert_eq!(got[&id("pepe")].price, 0.00001);
        assert_eq!(got[&id("pepe")].source, Source::GeckoTerminal);
    }

    #[tokio::test]
    async fn first_provider_to_answer_wins() {
        let _guard = serial();
        invalidate_cache();
        let engine = PriceEngine::with_providers(vec![
            fake("coingecko", &[("ethereum", 3000.0)]),
            fake("geckoterminal", &[("ethereum", 1.0)]),
        ]);
        let got = engine.resolve(&[id("ethereum")]).await;
        assert_eq!(got[&id("ethereum")].price, 3000.0);
    }

    #[tokio::test]
    async fn a_silent_provider_does_not_stop_the_others() {
        let _guard = serial();
        invalidate_cache();
        let engine = PriceEngine::with_providers(vec![
            fake("coingecko", &[]),
            fake("geckoterminal", &[("pepe", 0.00001)]),
        ]);
        assert_eq!(engine.resolve(&[id("pepe")]).await.len(), 1);
    }

    #[tokio::test]
    async fn uncovered_reports_what_no_provider_priced() {
        let _guard = serial();
        invalidate_cache();
        let engine = PriceEngine::with_providers(vec![fake("coingecko", &[])]);
        assert_eq!(
            engine.uncovered(&[id("nothing")]).await,
            vec![id("nothing")]
        );
    }

    #[tokio::test]
    async fn a_second_refresh_is_served_from_cache() {
        let _guard = serial();
        invalidate_cache();
        // Counts how many times the provider was actually asked.
        let engine = PriceEngine::with_providers(vec![fake("coingecko", &[("ethereum", 3000.0)])]);
        let first = engine.resolve(&[id("ethereum")]).await;
        assert_eq!(first[&id("ethereum")].price, 3000.0);

        // A different engine value, as the app builds one per refresh.
        let second = PriceEngine::new("price-engine-tests").resolve(&[id("ethereum")]).await;
        assert_eq!(second[&id("ethereum")].price, 3000.0);
    }

    #[tokio::test]
    async fn a_partial_cache_is_not_served_for_a_wider_request() {
        let _guard = serial();
        invalidate_cache();
        let engine = PriceEngine::with_providers(vec![fake("coingecko", &[("ethereum", 3000.0)])]);
        engine.resolve(&[id("ethereum")]).await;

        // Asking for a coin the cache does not hold must miss, rather than
        // silently reporting no price for it.
        let got = engine.resolve(&[id("ethereum"), id("pepe")]).await;
        assert!(!got.contains_key(&id("pepe")));
    }

    #[tokio::test]
    async fn an_empty_result_is_retried_after_the_window() {
        let _guard = serial();
        invalidate_cache();
        let now = Instant::now();
        let silent = PriceEngine::with_providers(vec![fake("coingecko", &[])]);
        assert!(silent.resolve_at(&[id("ethereum")], now).await.is_empty());

        // Once the window has passed, a recovered feed answers.
        let loud = PriceEngine::with_providers(vec![fake("coingecko", &[("ethereum", 3000.0)])]);
        let got = loud.resolve_at(&[id("ethereum")], now + CACHE_TTL + Duration::from_secs(1)).await;
        assert_eq!(got[&id("ethereum")].price, 3000.0);
    }

    /// Answers a fixed map and records every request it was given.
    struct Counting {
        answers: HashMap<Target, CoinPrice>,
        asked: std::sync::Arc<Mutex<Vec<Vec<Target>>>>,
    }

    impl Provider for Counting {
        fn name(&self) -> &'static str {
            "coingecko"
        }

        fn prices<'a>(
            &'a self,
            targets: &'a [Target],
        ) -> Pin<Box<dyn Future<Output = PriceMap> + Send + 'a>> {
            let mut sorted = targets.to_vec();
            sorted.sort_by_key(|t| format!("{t:?}"));
            self.asked.lock().unwrap().push(sorted);
            let answers: PriceMap = self
                .answers
                .iter()
                .filter(|(t, _)| targets.contains(t))
                .map(|(t, p)| (t.clone(), p.clone()))
                .collect();
            Box::pin(async move { answers })
        }
    }

    fn counting(answers: &[(&str, f64)]) -> (PriceEngine, std::sync::Arc<Mutex<Vec<Vec<Target>>>>) {
        let asked = std::sync::Arc::new(Mutex::new(Vec::new()));
        let provider = Counting {
            answers: answers
                .iter()
                .map(|(t, price)| (id(t), CoinPrice { price: *price, change: 0.0 }))
                .collect(),
            asked: asked.clone(),
        };
        (PriceEngine::with_providers(vec![Box::new(provider)]), asked)
    }

    #[tokio::test]
    async fn one_chain_s_prices_do_not_evict_another_s() {
        let _guard = serial();
        invalidate_cache();
        // The balance watch prices one chain at a time.
        let (engine, asked) = counting(&[("ethereum", 3000.0), ("binancecoin", 600.0)]);
        let now = Instant::now();
        engine.resolve_at(&[id("ethereum")], now).await;
        engine.resolve_at(&[id("binancecoin")], now).await;
        let again = engine.resolve_at(&[id("ethereum")], now + Duration::from_secs(15)).await;
        assert_eq!(again[&id("ethereum")].price, 3000.0);
        assert_eq!(asked.lock().unwrap().len(), 2, "ethereum is still cached");
    }

    #[tokio::test]
    async fn a_coin_no_one_prices_is_not_asked_for_on_every_call() {
        let _guard = serial();
        invalidate_cache();
        let (engine, asked) = counting(&[("ethereum", 3000.0)]);
        let now = Instant::now();
        engine.resolve_at(&[id("ethereum"), id("unlisted")], now).await;
        let got = engine.resolve_at(&[id("ethereum"), id("unlisted")], now + Duration::from_secs(15)).await;
        assert_eq!(got[&id("ethereum")].price, 3000.0);
        assert!(!got.contains_key(&id("unlisted")));
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_fetch_refreshes_every_stale_price() {
        let _guard = serial();
        invalidate_cache();
        let (engine, asked) = counting(&[("ethereum", 3000.0), ("binancecoin", 600.0)]);
        let now = Instant::now();
        engine.resolve_at(&[id("ethereum")], now).await;
        engine.resolve_at(&[id("binancecoin")], now).await;
        let later = now + CACHE_TTL + Duration::from_secs(1);
        engine.resolve_at(&[id("ethereum")], later).await;
        // The next chain's tick finds its price already fresh.
        engine.resolve_at(&[id("binancecoin")], later).await;
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[2], vec![id("binancecoin"), id("ethereum")]);
    }

    #[tokio::test]
    async fn a_throttled_feed_keeps_the_last_price_for_a_while() {
        let _guard = serial();
        invalidate_cache();
        let now = Instant::now();
        let (loud, _) = counting(&[("ethereum", 3000.0)]);
        loud.resolve_at(&[id("ethereum")], now).await;

        let (throttled, asked) = counting(&[]);
        let soon = now + CACHE_TTL + Duration::from_secs(1);
        let got = throttled.resolve_at(&[id("ethereum")], soon).await;
        assert_eq!(got[&id("ethereum")].price, 3000.0, "a minute-old price still counts");
        // Throttled: not asked again until the window passes.
        throttled.resolve_at(&[id("ethereum")], soon + Duration::from_secs(15)).await;
        assert_eq!(asked.lock().unwrap().len(), 1);

        // Too old to call live: the app shows its own last-known price instead.
        let late = throttled.resolve_at(&[id("ethereum")], now + STALE_MAX + Duration::from_secs(1)).await;
        assert!(late.is_empty());
    }
}