//! Compile-time build flavour and brand palette.
//!
//! A "flavour" is a rebranded build of the same binary: same features, different
//! identity and different *default* chrome colours. It is chosen once, at compile
//! time, mirroring `wezterm-gui/src/brand.rs`, which carries the product name and
//! the update-check repo.
//!
//! Nothing here names a specific brand. A downstream fork supplies its own
//! identity entirely through the environment at build time:
//!
//! ```sh
//! BRAND_FLAVOR=acme \
//! BRAND_ACCENT='#00AAA7' BRAND_ATTENTION='#ED6B06' BRAND_NEUTRAL='#63656A' \
//!   ci/build-macos-bundle.sh --native
//! ```
//!
//! so the fork's diff against this repo stays out of the `.rs` files and upstream
//! merges do not conflict.
//!
//! The palette only ever picks **defaults**: it selects `SidebarTheme::Brand` and
//! `AgentRingColors::Brand`, both of which stay settable from Lua in every build.
//! It touches chrome only — terminal colours (`colors`, `color_scheme`,
//! `resolved_palette`) are never involved.
//!
//! `config/build.rs` emits `cargo:rerun-if-env-changed` for each variable;
//! without that, switching brands would silently reuse a stale build.

/// The flavour this binary was compiled as. `"tgz"` is the stock build.
///
/// Used for identity that has to survive on disk — currently the state-file
/// names — rather than for choosing colours; a build can set a flavour without a
/// palette, and a palette without a flavour.
pub const FLAVOR: &str = match option_env!("BRAND_FLAVOR") {
    Some(v) => v,
    None => "tgz",
};

/// True for the stock, unbranded build.
pub fn is_stock() -> bool {
    matches!(FLAVOR, "tgz")
}

/// Brand accent: what the chrome means by *active / focused / in progress*. Also
/// the cool end of the agent ring.
pub const ACCENT: Option<(u8, u8, u8)> = parsed_env(option_env!("BRAND_ACCENT"));

/// Brand attention colour: *this agent needs you*. Also the warm end of the ring.
pub const ATTENTION: Option<(u8, u8, u8)> = parsed_env(option_env!("BRAND_ATTENTION"));

/// Brand neutral: the grey the dark chrome is tinted with, and the exact colour
/// of meta text.
pub const NEUTRAL: Option<(u8, u8, u8)> = parsed_env(option_env!("BRAND_NEUTRAL"));

/// Whether this build carries a brand palette at all.
///
/// Keyed on the accent alone: a brand that names one colour gets brand chrome,
/// with documented fallbacks for the other two. `const fn` so callers can use it
/// in a const context.
pub const fn has_palette() -> bool {
    ACCENT.is_some()
}

/// Fallback accent for `SidebarTheme::Brand` in a build with no `BRAND_ACCENT`:
/// the cool end of the default ring preset, so the theme is still usable (and
/// testable) unbranded.
pub const DEFAULT_ACCENT: (u8, u8, u8) = (0x58, 0xa6, 0xff);

/// Fallback neutral: the near-grey the fork's own `modern` palette uses for idle
/// text.
pub const DEFAULT_NEUTRAL: (u8, u8, u8) = (0x8a, 0x8a, 0x94);

/// Accent, or the documented fallback.
pub const fn accent() -> (u8, u8, u8) {
    match ACCENT {
        Some(c) => c,
        None => DEFAULT_ACCENT,
    }
}

/// Neutral, or the documented fallback.
pub const fn neutral() -> (u8, u8, u8) {
    match NEUTRAL {
        Some(c) => c,
        None => DEFAULT_NEUTRAL,
    }
}

/// State-file name for this flavour.
///
/// `RUNTIME_DIR`/`DATA_DIR` have no per-brand override, so two flavours running
/// side by side would otherwise fight over the same UI-state and last-session
/// files. The stock build keeps the historical name untouched; a branded build
/// gets its own file rather than a migration.
pub fn state_file_name(stem: &str, extension: &str) -> String {
    if is_stock() {
        format!("{stem}.{extension}")
    } else {
        format!("{stem}-{FLAVOR}.{extension}")
    }
}

/// `#RRGGBB` (or bare `RRGGBB`) at compile time.
///
/// Deliberately a hard `panic!`, which in a const context is a build failure: a
/// typo in a brand colour should stop the build, not silently ship the fallback.
const fn parse_hex(s: &str) -> (u8, u8, u8) {
    let b = s.as_bytes();
    let off = match b.len() {
        6 => 0,
        7 => 1,
        _ => panic!("brand colour must be #RRGGBB or RRGGBB"),
    };
    if off == 1 && b[0] != b'#' {
        panic!("brand colour of 7 characters must start with '#'");
    }
    (
        byte(b[off], b[off + 1]),
        byte(b[off + 2], b[off + 3]),
        byte(b[off + 4], b[off + 5]),
    )
}

const fn parsed_env(value: Option<&str>) -> Option<(u8, u8, u8)> {
    match value {
        Some(s) => Some(parse_hex(s)),
        None => None,
    }
}

const fn byte(hi: u8, lo: u8) -> u8 {
    nibble(hi) * 16 + nibble(lo)
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("brand colour must be hex digits"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_flavor_unless_overridden() {
        // The workspace is built without BRAND_FLAVOR, so the stock build must
        // stay the one every existing default test asserts against.
        if option_env!("BRAND_FLAVOR").is_none() {
            assert_eq!(FLAVOR, "tgz");
            assert!(is_stock());
        }
    }

    #[test]
    fn state_file_names_are_unsuffixed_only_for_stock() {
        if is_stock() {
            assert_eq!(state_file_name("tgz-ui-state", "json"), "tgz-ui-state.json");
        } else {
            assert_eq!(
                state_file_name("tgz-ui-state", "json"),
                format!("tgz-ui-state-{FLAVOR}.json")
            );
        }
    }

    #[test]
    fn hex_parses_both_spellings() {
        assert_eq!(parse_hex("#00AAA7"), (0x00, 0xaa, 0xa7));
        assert_eq!(parse_hex("ed6b06"), (0xed, 0x6b, 0x06));
        assert_eq!(parse_hex("#63656A"), (0x63, 0x65, 0x6a));
    }

    #[test]
    fn unbranded_build_falls_back_but_stays_usable() {
        if !has_palette() {
            assert_eq!(accent(), DEFAULT_ACCENT);
            assert_eq!(neutral(), DEFAULT_NEUTRAL);
        } else {
            assert_eq!(Some(accent()), ACCENT);
        }
    }
}
