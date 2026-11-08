//! Small, dependency-free helpers that replaced single-purpose crates:
//! base64 encoding, RFC 3339 timestamps, the `O_NOFOLLOW` flag, and the
//! local UTC offset from TZif zone files. Each is short, with tests against
//! published vectors, which is cheaper to review than the crate it replaced
//! was to audit.

pub(crate) mod base64;
pub(crate) mod localtime;
pub(crate) mod nofollow;
pub(crate) mod rfc3339;
