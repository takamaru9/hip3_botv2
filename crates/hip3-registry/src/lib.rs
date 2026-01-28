//! Market discovery and specification management for HIP-3.
//!
//! Manages market specifications from perpDexs, detects parameter changes,
//! and maintains the spec cache.
//!
//! P0-24: Includes user state and fee fetching for HIP-3 2x fee calculation.
//! P0-15: Automatic market discovery from perpDexs API.

pub mod client;
pub mod error;
pub mod preflight;
pub mod spec_cache;
pub mod user_state;

pub use client::MetaClient;
pub use error::{RegistryError, RegistryResult};
pub use preflight::{
    validate_market_keys, DiscoveredMarket, PerpDexInfo, PerpDexsResponse, PerpMarketInfo,
    PreflightChecker, PreflightResult,
};
pub use spec_cache::{RawPerpSpec, SpecCache};
pub use user_state::{
    AssetPositionData, AssetPositionEntry, ClearinghouseStateResponse, ParsedUserFees,
    RawUserFeesResponse, RawUserStateResponse,
};
