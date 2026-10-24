//! Live check of the price engine against the real public APIs.
//!
//! Run: `cargo test --manifest-path price_engine/Cargo.toml --test live -- --ignored`
//!
//! Ignored by default because it spends real API quota and needs network.

use fnf_price_engine::{PriceEngine, Target};

/// The ids the bundled registry declares. If this passes, the engine resolves
/// the same targets the app asks for on every launch.
#[tokio::test]
#[ignore = "hits the live CoinGecko API"]
async fn bundled_registry_assets_resolve_to_real_prices() {
    let targets = vec![
        Target::GeckoId("ethereum".into()),
        Target::GeckoId("matic-network".into()),
        Target::GeckoId("avalanche-2".into()),
        Target::GeckoId("binancecoin".into()),
        Target::GeckoId("bitcoin".into()),
    ];

    let resolved = PriceEngine::new().resolve(&targets).await;

    for t in &targets {
        let got = resolved
            .get(t)
            .unwrap_or_else(|| panic!("{t:?} resolved to nothing: {resolved:?}"));
        assert!(
            got.price > 0.0,
            "{t:?} resolved to a non-positive price: {}",
            got.price
        );
        println!("{t:?} = ${} via {}", got.price, got.source.as_str());
    }
}

/// A token with no registry CoinGecko id, priced by contract through
/// GeckoTerminal. This is the path that covers user-added tokens.
#[tokio::test]
#[ignore = "hits the live GeckoTerminal API"]
async fn a_contract_only_token_resolves_via_geckoterminal() {
    // USDC on Ethereum, keyed only by address.
    let usdc = Target::contract(1, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    let resolved = PriceEngine::new().resolve(&[usdc.clone()]).await;

    let got = resolved
        .get(&usdc)
        .unwrap_or_else(|| panic!("USDC resolved to nothing: {resolved:?}"));
    assert!(got.price > 0.0, "USDC price was {}", got.price);
    // USDC is a dollar token; a wildly wrong figure means the parse is broken.
    assert!(
        (got.price - 1.0).abs() < 0.2,
        "USDC should be near $1, got {}",
        got.price
    );
    println!("USDC = ${} via {}", got.price, got.source.as_str());
}
