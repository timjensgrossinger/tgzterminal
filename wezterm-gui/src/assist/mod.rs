//! The Ask panel's model (TGZTerminal, experimental `assist.ask`).
//!
//! Pure and GUI-free so every decision is unit-testable: what context is
//! sent (`context`), which CLI answers and with which flags (`provider`),
//! how its output is read (`stream`), which commands it suggested
//! (`suggest`) and how risky each one looks (`risk`). The window glue lives
//! in `termwindow/tgz_assist.rs`, the card in `termwindow/render/assist.rs`.

pub mod context;
pub mod provider;
pub mod risk;
pub mod stream;
pub mod suggest;
