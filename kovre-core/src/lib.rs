//! Core library of kovre: configuration parsing, the `rustic_core` wrapper,
//! the Ludusavi manifest layer, and the builtin templates.
//!
//! The `kovre` binary depends on this crate. Integration tests and the
//! upcoming `kovre-wasm` crate also link against it directly. CLI-only code
//! (clap derive structs, terminal formatting) lives in the `kovre` crate.

pub mod backup;
pub mod config;
pub mod dpapi;
pub mod ludusavi;
pub mod templates;
