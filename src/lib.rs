//! USD pricing for crypto wallets. Each wallet passes its own User-Agent to
//! `PriceEngine::new`.
//!
//! One concern, deliberately separate from the app and from `registry`:
//! turning a balance into a USD figure.
//!
//! Two providers, in fallthrough order:
//!
//! 1. **CoinGecko Demo** (free, optional key via `COINGECKO_API_KEY`).
//!    10,000 calls/month, ~30-100 req/min, 30-60s latency. Prices by its own
//!    coin id, taken from registry metadata.
//! 2. **GeckoTerminal public v2** (no key, ~10 calls/min, 1 min cache).
//!    Prices ERC-20s by chain + contract address — the gap CoinGecko leaves for
//!    tokens the registry has no coin id for.
//!
//! Ids come from the registry and are never hardcoded, so adding a chain never
//! means editing this crate. The one exception is `network`, which maps EIP-155
//! chain ids to GeckoTerminal's own slugs; that vocabulary belongs to their
//! API, not to our registry, and is flagged there.
//!
//! No keys, no signing, no fee logic. Those stay in the app's tx module.
//!
//! **A missing price is never a zero price.** Providers return an absent entry
//! rather than `0.0`, and `CoinPrice::is_known` exists so callers can tell
//! "no feed covers this" apart from "worth nothing" — they read very
//! differently on screen.

pub mod coingecko;
pub mod engine;
pub mod feed;
pub mod geckoterminal;
pub mod ids;
pub mod network;
pub mod provider;
pub mod value;

pub use coingecko::CoinGecko;
pub use engine::{PriceEngine, Resolved, ResolvedMap};
pub use feed::CoinPrice;
pub use geckoterminal::GeckoTerminal;
pub use ids::{ids_for, targets_for};
pub use network::{chain_id, gecko_network};
pub use provider::{target_for, PriceMap, Provider, Source, Target};
pub use value::{price_for, usd_value};

pub use registry::model::ChainInfo;