//! Small, dependency-free helpers that replaced single-purpose crates:
//! base64 encoding, RFC 3339 timestamps, and the `O_NOFOLLOW` flag. Each
//! is a few dozen lines with tests against published vectors, which is
//! cheaper to review than the crate it replaced was to audit.

pub(crate) mod base64;
pub(crate) mod nofollow;
pub(crate) mod rfc3339;
