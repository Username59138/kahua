//! Network client for Roblox services used by Kahua.
//!
//! * [`OpenCloud`] uploads assets and publishes places with an Open Cloud API key.
//! * [`Session`] reads assets and the authenticated user with a `.ROBLOSECURITY`
//!   cookie.
//! * [`Credentials`] and [`CredentialStore`] keep both secrets in the system
//!   keyring, falling back to a 0600 file when no keyring is available.
//! * [`find_studio_cookie`] recovers a cookie saved by Roblox Studio under Wine.
//!
//! Every base URL can be replaced, so the crate can be tested against a local
//! mock server.

mod credentials;
mod http;
mod multipart;
mod open_cloud;
mod session;
mod studio;

#[cfg(test)]
mod tests;

pub mod kahua;

pub use credentials::{CredentialStore, Credentials};
pub use open_cloud::{AssetKind, Creator, OpenCloud, Timing};
pub use session::{Session, User};
pub use studio::find_studio_cookie;
pub use ureq::http::{Method, Request, Response};

/// Default Open Cloud base URL.
pub const OPEN_CLOUD_BASE_URL: &str = "https://apis.roblox.com";
/// Default asset delivery base URL.
pub const ASSET_DELIVERY_BASE_URL: &str = "https://assetdelivery.roblox.com";
/// Default users API base URL.
pub const USERS_BASE_URL: &str = "https://users.roblox.com";
