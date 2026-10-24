//! Fetching USD prices from the public CoinGecko feed.
//!
//! Demo (free) plan: 10,000 calls/month, ~30-100 requests/minute, 30-60s data
//! latency. Authentication is optional here — without a key the public
//! endpoint still answers, just more aggressively rate limited. The key is
//! read from `COINGECKO_API_KEY` at *runtime*, never compiled in.

use crate::feed::CoinPrice;
use crate::provider::{PriceMap, Provider, Target};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

const ENDPOINT: &str = "https://api.coingecko.com/api/v3/simple/price";
const KEY_HEADER: &str = "x-cg-demo-api-key";
const KEY_ENV: &str = "COINGECKO_API_KEY";

/// CoinGecko Demo API client.
pub struct CoinGecko {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl CoinGecko {
    /// `user_agent` names the wallet. CoinGecko refuses requests that do not
    /// identify themselves, and it is how a rate-limited operator attributes
    /// usage, so keep it descriptive: `"Nira Wallet/1.0 (+https://example.com)"`.
    pub fn new(user_agent: &str) -> Self {
        let client = reqwest::Client::builder()
        // Several addresses per name, some unreachable here (IPv6 with no
        // route): each gets a slice of this, then the next is tried.
        .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            // CoinGecko answers 403 "Please add a descriptive User-Agent" to
            // requests without one, and reqwest sends none by default. The
            // response is a 403, not an error, so it was being parsed as "no
            // prices" and every asset read as unpriced.
            .user_agent(user_agent)
            .build()
            .unwrap_or_default();
        CoinGecko {
            client,
            api_key: std::env::var(KEY_ENV).ok().filter(|k| !k.trim().is_empty()),
        }
    }

    /// True when a demo key was supplied. Absent key still works, so this only
    /// reports which quota applies.
    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Fetches by CoinGecko coin id. Empty ids make no request.
    pub async fn fetch_ids(&self, ids: &[String]) -> HashMap<String, CoinPrice> {
        let mut out: HashMap<String, CoinPrice> = HashMap::new();
        if ids.is_empty() {
            return out;
        }
        let url = format!(
            "{ENDPOINT}?ids={}&vs_currencies=usd&include_24hr_change=true",
            ids.join(",")
        );
        let mut req = self.client.get(&url);
        if let Some(key) = &self.api_key {
            req = req.header(KEY_HEADER, key);
        }
        let body: serde_json::Value = match req.send().await {
            Ok(r) => match r.json().await {
                Ok(b) => b,
                Err(_) => return out,
            },
            Err(_) => return out,
        };
        for id in ids {
            let entry = body.get(id);
            let price = entry.and_then(|v| v.get("usd")).and_then(|v| v.as_f64());
            let change = entry
                .and_then(|v| v.get("usd_24h_change"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if let Some(price) = price {
                out.insert(id.clone(), CoinPrice { price, change });
            }
        }
        out
    }
}

impl Provider for CoinGecko {
    fn name(&self) -> &'static str {
        "coingecko"
    }

    /// Serves only `Target::GeckoId`. Contract-addressed targets are this
    /// provider's blind spot — that is what GeckoTerminal exists to cover.
    fn prices<'a>(
        &'a self,
        targets: &'a [Target],
    ) -> Pin<Box<dyn Future<Output = PriceMap> + Send + 'a>> {
        Box::pin(async move {
            let ids: Vec<String> = targets
                .iter()
                .filter_map(|t| match t {
                    Target::GeckoId(id) => Some(id.clone()),
                    Target::Contract { .. } | Target::Mint(_) => None,
                })
                .collect();
            let fetched = self.fetch_ids(&ids).await;
            targets
                .iter()
                .filter_map(|t| match t {
                    Target::GeckoId(id) => fetched.get(id).map(|p| (t.clone(), p.clone())),
                    Target::Contract { .. } | Target::Mint(_) => None,
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_ids_means_no_request_and_no_prices() {
        assert!(CoinGecko::new("price-engine-tests").fetch_ids(&[]).await.is_empty());
    }

    #[tokio::test]
    async fn contract_targets_are_not_this_providers_job() {
        let p = CoinGecko::new("price-engine-tests");
        let map = p.prices(&[Target::contract(1, "0xabc")]).await;
        assert!(map.is_empty());
    }
}