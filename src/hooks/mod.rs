//! Pre-commit hook runner: config parsing, file selection, builtin checks,
//! local hook policy, and the text/JSON/SARIF reports.
pub mod audit;
pub mod check;
pub mod config;
pub mod filter;
pub mod git;
pub mod identify;
pub mod local_hook;
pub mod migrate;
pub mod output;
pub(crate) mod paths;
pub mod policy;
pub mod runner;
pub mod store;
