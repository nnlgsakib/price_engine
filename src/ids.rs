//! Resolving which price ids a set of chains needs.

use crate::provider::{target_for, Target};
use fnf_registry::model::ChainInfo;
use std::path::Path;

/// Every distinct CoinGecko id the registry declares for these chains.
///
/// Chains with no id contribute nothing. That is a real gap in coverage, not a
/// zero price, and callers should render it as "unavailable".
pub fn ids_for(chains: &[ChainInfo], base: &Path) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: String| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    for chain in chains {
        match chain {
            ChainInfo::Evm(evm) => {
                if let Some(id) = &evm.native_currency.coingecko_id {
                    push(id.clone());
                }
                for token in fnf_registry::tokens::tokens_for_chain(base, chain) {
                    if let Some(id) = token.coingecko_id {
                        push(id);
                    }
                }
            }
            ChainInfo::Bitcoin(_) => push("bitcoin".to_string()),
            // Devnet coins are worthless, whatever id a token there claims.
            ChainInfo::Solana(sol) if sol.testnet => {}
            ChainInfo::Solana(sol) => {
                if let Some(id) = &sol.native_currency.coingecko_id {
                    push(id.clone());
                }
                for token in fnf_registry::tokens::tokens_for_chain(base, chain) {
                    if let Some(id) = token.coingecko_id {
                        push(id);
                    }
                }
            }
        }
    }
    ids
}

/// Everything worth asking a provider for, across both identifier shapes.
///
/// Tokens are added twice: once by CoinGecko id, once by contract address.
/// The engine de-duplicates by asking each provider only for what it can serve,
/// so a token with an id still costs one CoinGecko lookup, and one without an
/// id is still reachable through GeckoTerminal.
pub fn targets_for(chains: &[ChainInfo], base: &Path) -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();
    let mut push = |t: Target| {
        if !targets.contains(&t) {
            targets.push(t);
        }
    };

    for chain in chains {
        match chain {
            ChainInfo::Evm(evm) => {
                if let Some(t) =
                    target_for(evm.native_currency.coingecko_id.as_deref(), None, None)
                {
                    push(t);
                }
                for token in fnf_registry::tokens::tokens_for_chain(base, chain) {
                    if let Some(t) = target_for(
                        token.coingecko_id.as_deref(),
                        Some(evm.chain_id),
                        Some(&token.address),
                    ) {
                        push(t);
                    }
                    // Also offer the contract even when an id exists: a
                    // provider that is up while CoinGecko is rate limited can
                    // then still cover the token.
                    if token.coingecko_id.is_none() {
                        push(Target::contract(evm.chain_id, &token.address));
                    }
                }
            }
            ChainInfo::Bitcoin(_) => push(Target::GeckoId("bitcoin".to_string())),
            ChainInfo::Solana(sol) if sol.testnet => {}
            ChainInfo::Solana(sol) => {
                if let Some(id) = &sol.native_currency.coingecko_id {
                    push(Target::GeckoId(id.clone()));
                }
                for token in fnf_registry::tokens::tokens_for_chain(base, chain) {
                    match token.coingecko_id.filter(|id| !id.trim().is_empty()) {
                        Some(id) => push(Target::GeckoId(id)),
                        None => push(Target::Mint(token.address)),
                    }
                }
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solana_is_priced_on_mainnet_and_never_on_devnet() {
        let chains = fnf_registry::chains::builtin_only();
        let pick = |id: &str| vec![chains.iter().find(|c| c.id() == id).expect("built in").clone()];
        let base = std::env::temp_dir().join("fnf-price-no-custom");

        let mainnet = targets_for(&pick("solana"), &base);
        assert!(mainnet.contains(&Target::GeckoId("solana".into())));
        assert!(mainnet.contains(&Target::GeckoId("usd-coin".into())));
        assert!(targets_for(&pick("solana-devnet"), &base).is_empty());
        assert!(ids_for(&pick("solana-devnet"), &base).is_empty());
    }

    #[test]
    fn no_chains_means_no_ids() {
        assert!(ids_for(&[], Path::new(".")).is_empty());
        assert!(targets_for(&[], Path::new(".")).is_empty());
    }
}