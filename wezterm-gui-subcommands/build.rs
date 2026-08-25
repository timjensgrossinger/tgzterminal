fn main() {
    // Branded builds compile in their own default window class (see
    // `DEFAULT_WINDOW_CLASS`); without this, a flavour switch would reuse the
    // stale class and the branded app would attach to the stock instance.
    println!("cargo:rerun-if-env-changed=BRAND_WINDOW_CLASS");
}
