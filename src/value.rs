//! Turning a base-unit balance into a USD figure.

use crate::feed::CoinPrice;
use std::collections::HashMap;

/// Looks a price up by CoinGecko id.
///
/// An asset whose registry entry carries no id resolves to the default. It
/// must not borrow another asset's price, and it must not report $0 for an
/// unknown coin — "no feed covers this" and "worth nothing" read very
/// differently on screen.
pub fn price_for(prices: &HashMap<String, CoinPrice>, coingecko_id: Option<&str>) -> CoinPrice {
    coingecko_id
        .and_then(|id| prices.get(id))
        .cloned()
        .unwrap_or_default()
}

/// USD value of a base-unit balance.
///
/// An unparseable balance is worth zero rather than poisoning the portfolio
/// total with NaN.
pub fn usd_value(balance_base: &str, decimals: u8, price: f64) -> f64 {
    let scale = 10u128.checked_pow(decimals as u32).unwrap_or(1) as f64;
    balance_base.parse::<u128>().unwrap_or(0) as f64 / scale * price
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_value_scales_base_units() {
        // 1.5 tokens at 18 decimals, priced at 2.00.
        assert!((usd_value("1500000000000000000", 18, 2.0) - 3.0).abs() < 1e-9);
        // 8 decimals: one satoshi (1e-8 BTC) at $30k is $0.0003.
        assert!((usd_value("1", 8, 30_000.0) - 3e-4).abs() < 1e-12);
        assert_eq!(usd_value("0", 18, 3000.0), 0.0);
    }

    #[test]
    fn unparseable_balance_is_zero_not_nan() {
        assert_eq!(usd_value("not-a-number", 18, 2.0), 0.0);
        assert_eq!(usd_value("", 18, 2.0), 0.0);
    }

    #[test]
    fn missing_id_resolves_to_no_price() {
        let mut prices = HashMap::new();
        prices.insert(
            "ethereum".to_string(),
            CoinPrice { price: 3000.0, change: 1.0 },
        );

        assert_eq!(price_for(&prices, Some("ethereum")).price, 3000.0);
        assert_eq!(price_for(&prices, None), CoinPrice::default());
        assert_eq!(price_for(&prices, Some("no-such-coin")), CoinPrice::default());
    }
}