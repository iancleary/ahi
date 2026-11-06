//! ahi library: the pre-commit hook runner (`hooks`), the model picker and
//! chat terminal screens (`tui`, `chat`), and the provider harness (`harness`).
//!
//! The package, the library target, and the binary are all named `ahi`.

#![forbid(unsafe_code)]

mod actions;
pub mod chat;
mod clipboard;
pub mod directories;
pub mod error;
pub mod harness;
pub mod hooks;
mod shlex;
pub mod tui;
pub mod update;
mod yaml_safe;

#[cfg(test)]
#[path = "../tests/support/fixture_directory.rs"]
mod fixture_directory;

pub use error::{Error, Result};
pub use hooks::local_hook::{base_env_names, default_toolchain_names};

/// Version baked in from the library package. The `ahi` binary uses the same number.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Schema level of upstream pre-commit this crate claims to parse.
pub const PRE_COMMIT_SCHEMA: &str = "4.7.0";
