//! ahi library: the pre-commit hook runner (`hooks`), the model picker and
//! chat terminal screens (`tui`, `chat`), the provider harness (`harness`), and
//! the development container command (`dev`).
//!
//! The package, the library target, and the binary are all named `ahi`.

#![forbid(unsafe_code)]

pub mod chat;
mod clipboard;
pub mod dev;
pub mod directories;
pub mod error;
pub mod harness;
pub mod hooks;
pub mod program_status;
pub mod sandbox;
pub mod session;
mod shlex;
pub mod term;
mod tool_policy;
pub mod tools;
pub mod tui;
pub mod update;
mod util;
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
