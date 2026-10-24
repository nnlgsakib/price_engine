//! The shared price type.

/// A USD price and its 24h change.
///
/// `price == 0.0` means "no price", never "worth nothing" — a token is not
/// free just because no provider covered it. Callers must treat the default as
/// unknown.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CoinPrice {
    pub price: f64,
    pub change: f64,
}

impl CoinPrice {
    pub fn is_known(&self) -> bool {
        self.price.is_finite() && self.price > 0.0
    }
}