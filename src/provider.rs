//! What the engine is asked to price, and what a provider returns.

use crate::feed::CoinPrice;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// One asset the engine needs a USD price for.
///
/// Two shapes, because providers key on different things: CoinGecko indexes by
/// its own coin id, GeckoTerminal indexes by chain + contract address. The
/// second is what covers tokens whose registry entry has no CoinGecko id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A CoinGecko coin id, from registry metadata.
    GeckoId(String),
    /// An ERC-20 by EVM chain id and contract address.
    Contract { chain_id: u64, address: String },
    /// A Solana token by its mint address. Kept apart from `Contract`
    /// because a mint is case-sensitive, and that one is lower-cased.
    Mint(String),
}

impl Target {
    pub fn contract(chain_id: u64, address: impl Into<String>) -> Self {
        Target::Contract {
            chain_id,
            // Addresses compare case-insensitively (EIP-55); normalising here
            // stops the same token missing on checksum casing.
            address: address.into().to_ascii_lowercase(),
        }
    }
}

/// The target a single asset should be looked up under.
///
/// CoinGecko first, because it reports 24h change and has the larger quota.
/// A token with no registry coin id falls back to its contract address, which
/// is what GeckoTerminal can answer. `None` means the asset carries neither
/// identifier and no provider can be asked.
pub fn target_for(
    gecko_id: Option<&str>,
    chain_id: Option<u64>,
    contract: Option<&str>,
) -> Option<Target> {
    if let Some(id) = gecko_id.filter(|s| !s.trim().is_empty()) {
        return Some(Target::GeckoId(id.to_string()));
    }
    match (chain_id, contract.filter(|c| !c.trim().is_empty())) {
        (Some(chain), Some(address)) => Some(Target::contract(chain, address)),
        _ => None,
    }
}

/// Prices for a set of targets. A missing key means "no provider covered
/// this", which is not the same as a price of zero.
pub type PriceMap = HashMap<Target, CoinPrice>;

/// A source of USD prices.
///
/// Implementations never fail hard: a provider that cannot answer returns an
/// empty (or partial) map and lets the engine fall through to the next one. A
/// dead provider must degrade coverage, not the portfolio.
///
/// The future is boxed by hand rather than via `async fn` in the trait, which
/// would make the trait non-object-safe and so impossible to store as
/// `Box<dyn Provider>`.
pub trait Provider: Send + Sync {
    /// Stable identifier, used to attribute a price to its source.
    fn name(&self) -> &'static str;

    /// Prices whatever subset of `targets` this provider can serve.
    fn prices<'a>(
        &'a self,
        targets: &'a [Target],
    ) -> Pin<Box<dyn Future<Output = PriceMap> + Send + 'a>>;
}

/// Which provider supplied a price. Surfaced so the UI can say "cached" or
/// "unavailable" honestly instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// No provider covered this asset.
    #[default]
    Unknown,
    CoinGecko,
    GeckoTerminal,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Unknown => "unknown",
            Source::CoinGecko => "coingecko",
            Source::GeckoTerminal => "geckoterminal",
        }
    }

    pub fn from_provider_name(name: &str) -> Source {
        match name {
            "coingecko" => Source::CoinGecko,
            _ => Source::GeckoTerminal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_id_wins_over_the_contract() {
        // CoinGecko first: it reports 24h change and has the larger quota.
        assert_eq!(
            target_for(Some("ethereum"), Some(1), Some("0xabc")),
            Some(Target::GeckoId("ethereum".into()))
        );
    }

    #[test]
    fn a_token_with_no_id_falls_back_to_its_contract() {
        // This is the Pexsi case: registry declares the token, no coingeckoId.
        assert_eq!(
            target_for(None, Some(80_002), Some("0xB0c07D361")),
            Some(Target::contract(80_002, "0xB0c07D361"))
        );
    }

    #[test]
    fn contract_addresses_are_matched_case_insensitively() {
        // EIP-55 checksum casing must not make a price go missing.
        let checksummed = target_for(None, Some(1), Some("0xAbC123"));
        let plain = target_for(None, Some(1), Some("0xabc123"));
        assert_eq!(checksummed, plain);
    }

    #[test]
    fn an_asset_with_neither_identifier_has_no_target() {
        // Blank strings are not identifiers.
        assert_eq!(target_for(Some("  "), Some(1), None), None);
        assert_eq!(target_for(None, None, None), None);
        assert_eq!(target_for(None, Some(1), Some("  ")), None);
    }

    #[test]
    fn unknown_is_the_default_source_not_a_real_one() {
        assert_eq!(Source::default(), Source::Unknown);
        assert_ne!(Source::default(), Source::CoinGecko);
    }
}