//! ahi library: pre-commit config, file filtering, and offline builtin checks.
//!
//! The package, the library target, and the binary are all named `ahi`.

#![forbid(unsafe_code)]

pub mod audit;
pub mod check;
pub mod config;
pub mod error;
pub mod filter;
pub mod git;
pub mod identify;
mod local_hook;
pub mod migrate;
pub mod output;
mod paths;
pub mod policy;
pub mod runner;
mod shlex;
pub mod store;
mod yaml_safe;

#[cfg(test)]
#[path = "../tests/support/fixture_directory.rs"]
mod fixture_directory;

pub use error::{Error, Result};
pub use local_hook::{base_env_names, default_toolchain_names};

/// Version baked in from the library package. The `ahi` binary uses the same number.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Schema level of upstream pre-commit this crate claims to parse.
pub const PRE_COMMIT_SCHEMA: &str = "4.7.0";
