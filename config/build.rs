fn main() {
    // Build flavour and brand palette (see `src/brand.rs`). Without these, cargo
    // would not rebuild the crate when a brand changes, and a branded build could
    // silently keep the stock defaults baked in from an earlier compile.
    for var in [
        "BRAND_FLAVOR",
        "BRAND_ACCENT",
        "BRAND_ATTENTION",
        "BRAND_NEUTRAL",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
