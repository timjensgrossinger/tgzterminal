//! Compile-time branding parameters.
//!
//! Defaults preserve TGZTerminal behavior exactly. A build with no `BRAND_*`
//! environment variables set resolves to the original TGZTerminal values, so
//! behavior is unchanged. An overlay fork can set these at compile time to
//! rebrand update checks and user-facing product strings.

/// GitHub `owner/repo` slug used for release/update queries.
pub const GITHUB_REPO: &str = match option_env!("BRAND_GITHUB_REPO") {
    Some(v) => v,
    None => "timjensgrossinger/tgzterminal",
};

/// Human-facing product name used in the update User-Agent and notifications.
pub const PRODUCT_NAME: &str = match option_env!("BRAND_PRODUCT_NAME") {
    Some(v) => v,
    None => "TGZTerminal",
};
