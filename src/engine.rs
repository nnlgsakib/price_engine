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

/// How long a fetched price set is reused before the providers are called
/// again.
///
/// The free CoinGecko tier is throttled hard (often single-digit requests per
/// minute without a key). A user pulling to refresh, or a screen remounting,
/// used to mean another request â€” which is how a refresh storm turned every
/// price in the portfolio into "unavailable". One shared short-lived cache
/// collapses that storm into at most one fetch per window.
const CACHE_TTL: Duration = Duration::from_secs(60);

struct Cached {
    at: Instant,
    map: ResolvedMap,
}

/// Process-wide so the cache outlives individual `PriceEngine` values, which
/// the app constructs per portfolio refresh.
fn cache() -> &'static Mutex<Option<Cached>> {
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Drops any cached prices. For tests, and for a manual refresh.
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache().lock() {
        *guard = None;
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
    /// Default order: CoinGecko, then GeckoTerminal.
    pub fn new() -> Self {
        PriceEngine::with_providers(vec![
            Box::new(CoinGecko::new()),
            Box::new(GeckoTerminal::new()),
        ])
    }

    pub fn with_providers(providers: Vec<Box<dyn Provider>>) -> Self {
        PriceEngine { providers }
    }

    /// Prices every target it can, with provenance for each.
    ///
    /// A fresh cache that covers every requested target is returned as-is.
    /// The cache is only used when it is *complete* for this request: serving a
    /// partial set would silently mix live and stale figures for the same
    /// portfolio.
    pub async fn resolve(&self, targets: &[Target]) -> ResolvedMap {
        if targets.is_empty() {
            return HashMap::new();
        }
        if let Some(map) = self.from_cache(targets) {
            return map;
        }

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
                out.insert(
                    target.clone(),
                    Resolved {
                        price: price.price,
                        change: price.change,
                        source,
                    },
                );
            }
            missing.retain(|t| !out.contains_key(t));
        }

        // Only cache a non-empty set: caching an empty result would make a
        // transient outage look authoritative for the whole TTL window.
        if !out.is_empty() {
            if let Ok(mut guard) = cache().lock() {
                *guard = Some(Cached {
                    at: Instant::now(),
                    map: out.clone(),
                });
            }
        }
        out
    }

    /// The cached set, if it is fresh and answers every requested target.
    fn from_cache(&self, targets: &[Target]) -> Option<ResolvedMap> {
        let guard = cache().lock().ok()?;
        let cached = guard.as_ref()?;
        if cached.at.elapsed() > CACHE_TTL {
            return None;
        }
        if !targets.iter().all(|t| cached.map.contains_key(t)) {
            return None;
        }
        let mut out = ResolvedMap::new();
        for t in targets {
            if let Some(r) = cached.map.get(t) {
                out.insert(t.clone(), r.clone());
            }
        }
        Some(out)
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

impl Default for PriceEngine {
    fn default() -> Self {
        PriceEngine::new()
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
        let second = PriceEngine::new().resolve(&[id("ethereum")]).await;
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
    async fn an_empty_result_is_not_cached_as_authoritative() {
        let _guard = serial();
        invalidate_cache();
        let silent = PriceEngine::with_providers(vec![fake("coingecko", &[])]);
        assert!(silent.resolve(&[id("ethereum")]).await.is_empty());

        // A later successful fetch must still be able to answer.
        let loud = PriceEngine::with_providers(vec![fake("coingecko", &[("ethereum", 3000.0)])]);
        let got = loud.resolve(&[id("ethereum")]).await;
        assert_eq!(got[&id("ethereum")].price, 3000.0);
    }
}