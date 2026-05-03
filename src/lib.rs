//! Library surface of `kovre`. The `kovre` binary is a thin wrapper around
//! these modules; integration tests link against the library directly.

pub mod backup;
pub mod cli;
pub mod config;
pub mod ludusavi;
pub mod templates;
