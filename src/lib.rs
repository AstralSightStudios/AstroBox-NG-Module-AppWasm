#![cfg(any(target_arch = "wasm32", rust_analyzer))]

pub mod frontapi;
pub mod spp;

pub use frontapi::*;
