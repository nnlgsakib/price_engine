//! GeckoTerminal network vocabulary.
//!
//! GeckoTerminal addresses chains by its own slugs (`eth`, `bsc`,
//! `polygon_pos`) that do **not** match EIP-155 chain ids. These are that
//! external API's names, not chain metadata, so they live here rather than in
//! the registry — but they are a hardcoded list, and a chain added to the
//! registry will not be priceable until it is listed here.
//!
//! ponytail: static slug table, single source of truth for GeckoTerminal
//! network names. Replace with the registry carrying a `gecko_network` field
//! if this ever needs to track new chains automatically.

/// EIP-155 chain id → GeckoTerminal network slug.
pub fn gecko_network(chain_id: u64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "eth",
        10 => "optimism",
        56 => "bsc",
        100 => "gnosis",
        137 => "polygon_pos",
        250 => "fantom",
        8453 => "base",
        42161 => "arbitrum",
        42220 => "celo",
        43114 => "avalanche",
        _ => return None,
    })
}

/// GeckoTerminal network slug → EIP-155 chain id.
pub fn chain_id(network: &str) -> Option<u64> {
    Some(match network {
        "eth" => 1,
        "optimism" => 10,
        "bsc" => 56,
        "gnosis" => 100,
        "polygon_pos" => 137,
        "fantom" => 250,
        "base" => 8453,
        "arbitrum" => 42161,
        "celo" => 42220,
        "avalanche" => 43114,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slug_round_trips() {
        for id in [1u64, 10, 56, 100, 137, 250, 8453, 42161, 42220, 43114] {
            let slug = gecko_network(id).expect("listed");
            assert_eq!(chain_id(slug), Some(id), "{slug} did not round-trip");
        }
    }

    #[test]
    fn unlisted_chains_return_none_rather_than_a_wrong_chain() {
        // A testnet must not be silently mapped onto mainnet.
        assert_eq!(gecko_network(80_002), None);
        assert_eq!(gecko_network(999_999), None);
        assert_eq!(chain_id("not-a-network"), None);
    }
}