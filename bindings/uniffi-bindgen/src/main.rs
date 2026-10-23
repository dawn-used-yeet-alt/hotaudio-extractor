//! The pinned UniFFI generator for the bindings workspace.
//!
//! `cargo run -p uniffi-bindgen -- generate --language …` keeps codegen in the
//! lockfile with the runtime, rather than relying on a globally installed
//! `uniffi-bindgen` that could drift from the crate version.

fn main() {
    uniffi::uniffi_bindgen_main();
}
