//! GeckoTerminal public API v2 — the keyless fallback.
//!
//! Roughly 10 calls/minute, cached 1 minute server-side, no API key. Low
//! quota, but it answers the question CoinGecko cannot: it prices **tokens by
//! chain + contract address** (ERC-20s, and Solana tokens by mint), so tokens
//! with no CoinGecko id in the registry are still covered.

use crate::feed::CoinPrice;
use crate::network::{chain_id, gecko_network};
use crate::provider::{PriceMap, Provider, Target};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

const BASE: &str = "https://api.geckoterminal.com/api/v2";
const API_VERSION: &str = "application/json;version=20230203";

/// How many addresses to ask for in one call. The endpoint takes a
/// comma-separated list, so one call covers a whole chain's tokens instead of
/// burning the 10/min budget one token at a time.
const BATCH: usize = 30;

pub struct GeckoTerminal {
    client: reqwest::Client,
}

impl GeckoTerminal {
    /// `user_agent` names the wallet. CoinGecko refuses requests that do not
    /// identify themselves, and it is how a rate-limited operator attributes
    /// usage, so keep it descriptive: `"Nira Wallet/1.0 (+https://example.com)"`.
    pub fn new(user_agent: &str) -> Self {
        let client = reqwest::Client::builder()
        // Several addresses per name, some unreachable here (IPv6 with no
        // route): each gets a slice of this, then the next is tried.
        .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(user_agent)
            .build()
            .unwrap_or_default();
        GeckoTerminal { client }
    }

    /// Prices a batch of addresses on one network.
    ///
    /// Response shape:
    /// `{"data":{"attributes":{"token_prices":{"0x..":{"usd":"1.23"}}}}}` —
    /// the prices are strings, so they must be parsed, not cast.
    ///
    /// `target` turns an address in the answer back into what was asked for.
    async fn fetch_network(
        &self,
        network: &str,
        addresses: &[String],
        target: impl Fn(&str) -> Target,
    ) -> PriceMap {
        let mut out: PriceMap = HashMap::new();
        if addresses.is_empty() {
            return out;
        }
        let url = format!(
            "{BASE}/simple/networks/{network}/token_price/{}?include_24hr_price_change=true",
            addresses.join(",")
        );
        let body: serde_json::Value = match self
            .client
            .get(&url)
            .header("Accept", API_VERSION)
            .send()
            .await
        {
            Ok(r) => match r.json().await {
                Ok(b) => b,
                Err(_) => return out,
            },
            Err(_) => return out,
        };
        let attributes = body.get("data").and_then(|d| d.get("attributes"));
        let Some(prices) = attributes
            .and_then(|a| a.get("token_prices"))
            .and_then(|p| p.as_object())
        else {
            return out;
        };
        // Percent over 24 hours, as a string, for the addresses it has one for.
        let change_of = |address: &str| {
            attributes
                .and_then(|a| a.get("h24_price_change_percentage"))
                .and_then(|c| c.get(address))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|c| c.is_finite())
                .unwrap_or(0.0)
        };
        for (address, entry) in prices {
            // The live endpoint returns each price as a bare string
            // (`{"0xabc": "1.23"}`), but has also shipped the documented
            // `{"0xabc": {"usd": "1.23"}}` shape. Accept either rather than
            // silently resolving nothing if it changes again.
            let raw = match entry {
                serde_json::Value::String(s) => Some(s.as_str()),
                serde_json::Value::Object(_) => entry.get("usd").and_then(|v| v.as_str()),
                _ => None,
            };
            let Some(price) = raw
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|p| p.is_finite() && *p > 0.0)
            else {
                continue;
            };
            out.insert(target(address), CoinPrice { price, change: change_of(address) });
        }
        out
    }
}

impl Provider for GeckoTerminal {
    fn name(&self) -> &'static str {
        "geckoterminal"
    }

    fn prices<'a>(
        &'a self,
        targets: &'a [Target],
    ) -> Pin<Box<dyn Future<Output = PriceMap> + Send + 'a>> {
        Box::pin(async move {
            // Group by network so one call covers many tokens. A chain
            // GeckoTerminal does not list is skipped, not guessed at.
            let mut by_network: HashMap<u64, Vec<String>> = HashMap::new();
            for t in targets {
                if let Target::Contract { chain_id, address } = t {
                    if gecko_network(*chain_id).is_some() {
                        by_network
                            .entry(*chain_id)
                            .or_default()
                            .push(address.clone());
                    }
                }
            }

            let mut out: PriceMap = HashMap::new();
            for (chain, addresses) in by_network {
                let Some(network) = gecko_network(chain) else {
                    continue;
                };
                for chunk in addresses.chunks(BATCH) {
                    out.extend(self.fetch_network(network, chunk, |address| Target::contract(chain, address)).await);
                }
            }
            // Solana is not an EVM chain id, and its addresses keep their case.
            let mints: Vec<String> = targets
                .iter()
                .filter_map(|t| match t {
                    Target::Mint(mint) => Some(mint.clone()),
                    _ => None,
                })
                .collect();
            for chunk in mints.chunks(BATCH) {
                out.extend(self.fetch_network("solana", chunk, |mint| Target::Mint(mint.to_string())).await);
            }
            out
        })
    }
}

/// Reverse lookup, kept for callers that hold a GeckoTerminal network id.
pub fn chain_from_network(network: &str) -> Option<u64> {
    chain_id(network)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chain_without_a_gecko_network_is_skipped() {
        let p = GeckoTerminal::new("price-engine-tests");
        // 80002 is Amoy, which GeckoTerminal does not list.
        let map = p.prices(&[Target::contract(80_002, "0xabc")]).await;
        assert!(map.is_empty());
    }

    /// Run with `cargo test -- --ignored`. A mint is case-sensitive, so this
    /// also proves the answer is matched back without changing its case.
    #[tokio::test]
    #[ignore = "network"]
    async fn a_solana_token_is_priced_by_its_mint() {
        let usdc = Target::Mint("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into());
        let map = GeckoTerminal::new("price-engine-tests").prices(std::slice::from_ref(&usdc)).await;
        let price = map.get(&usdc).expect("USDC on Solana is priced").price;
        assert!((0.9..1.1).contains(&price), "{price}");
    }

    /// The live endpoint returns prices as bare strings. An earlier version of
    /// this parser looked for a `usd` key inside each entry, matched nothing,
    /// and every custom token read as unpriced. Both documented and observed
    /// shapes must parse.
    #[test]
    fn parses_both_price_shapes_the_api_has_shipped() {
        // Observed live.
        let bare = r#"{"data":{"attributes":{"token_prices":{"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48":"0.999375029963408"}}}}"#;
        // Shape shown in the public docs.
        let nested = r#"{"data":{"attributes":{"token_prices":{"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48":{"usd":"0.999375029963408"}}}}}"#;

        for (label, body) in [("bare", bare), ("nested", nested)] {
            let value: serde_json::Value = serde_json::from_str(body).expect("valid json");
            let prices = value
                .get("data")
                .and_then(|d| d.get("attributes"))
                .and_then(|a| a.get("token_prices"))
                .and_then(|p| p.as_object())
                .unwrap_or_else(|| panic!("{label}: token_prices missing"));

            let (_, entry) = prices.iter().next().expect("one price");
            let raw = match entry {
                serde_json::Value::String(s) => Some(s.as_str()),
                serde_json::Value::Object(_) => entry.get("usd").and_then(|v| v.as_str()),
                _ => None,
            };
            let parsed: f64 = raw
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("{label}: price did not parse"));
            assert!((parsed - 0.999375).abs() < 1e-6, "{label}: {parsed}");
        }
    }
}