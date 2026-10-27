//! Content-addressed blob store. No network client lives here.
//!
//! Objects are `objects/sha256/<2 hex>/<rest>`. A put writes a temporary file
//! in the same directory, fsyncs it, then renames. A get re-hashes and errors
//! if the bytes do not match the address.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    writable: bool,
}

impl Store {
    /// Open a cache. An existing `objects/sha256` directory that cannot be
    /// written is a valid read-only store (the image-build bake). Creating the
    /// directory is attempted only when it is absent.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let objects = root.join("objects/sha256");
        if !objects.is_dir() {
            fs::create_dir_all(&objects)
                .map_err(|err| Error::new(format!("failed to create cache: {err}")))?;
        }
        let writable = probe_writable(&objects);
        if writable {
            let readme = root.join("README");
            if !readme.exists() {
                fs::write(
                    &readme,
                    "This directory is maintained by ahi.\nSee https://github.com/iancleary/ahi\nObjects under objects/sha256 are content-addressed. A read-only store is valid when those objects were baked into the image.\n",
                )
                .map_err(|err| Error::new(format!("failed to write cache README: {err}")))?;
            }
        }
        Ok(Self { root, writable })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        if !self.writable {
            return Err(Error::new(
                "cache is read-only; populate it with ahi fetch or bake it into the image before run",
            ));
        }
        let digest = sha256_hex(bytes);
        let path = self.object_path(&digest)?;
        if path.exists() {
            let existing = self.get(&digest)?;
            if existing != bytes {
                return Err(Error::new(format!(
                    "cache object {digest} already exists with different bytes"
                )));
            }
            return Ok(digest);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::new(format!("failed to create object dir: {err}")))?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut file = File::create(&tmp)
                .map_err(|err| Error::new(format!("failed to create cache object: {err}")))?;
            file.write_all(bytes)
                .map_err(|err| Error::new(format!("failed to write cache object: {err}")))?;
            file.sync_all()
                .map_err(|err| Error::new(format!("failed to fsync cache object: {err}")))?;
        }
        fs::rename(&tmp, &path)
            .map_err(|err| Error::new(format!("failed to publish cache object: {err}")))?;
        Ok(digest)
    }

    pub fn get(&self, digest: &str) -> Result<Vec<u8>> {
        let path = self.object_path(digest)?;
        let mut file = File::open(&path)
            .map_err(|err| Error::new(format!("cache object {digest} is missing ({err})")))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| Error::new(format!("failed to read cache object {digest}: {err}")))?;
        let actual = sha256_hex(&bytes);
        if actual != digest {
            return Err(Error::new(format!(
                "cache object {digest} failed verification (got {actual})"
            )));
        }
        Ok(bytes)
    }

    fn object_path(&self, digest: &str) -> Result<PathBuf> {
        if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(Error::new(format!("not a sha256 hex digest: {digest}")));
        }
        let digest = digest.to_ascii_lowercase();
        Ok(self
            .root
            .join("objects/sha256")
            .join(&digest[..2])
            .join(&digest[2..]))
    }
}

/// Cache location. Order: `--cache-dir`, `AHI_HOME`, `$XDG_CACHE_HOME/ahi`
/// when that variable is set, then `std::env::temp_dir()/ahi`. No home-directory
/// fallback. `temp_dir` follows `TMPDIR`.
pub fn cache_dir(explicit: Option<&Path>) -> PathBuf {
    resolve_cache_dir(
        explicit,
        std::env::var_os("AHI_HOME"),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::temp_dir(),
    )
}

pub fn resolve_cache_dir(
    explicit: Option<&Path>,
    ahi_home: Option<std::ffi::OsString>,
    xdg_cache_home: Option<std::ffi::OsString>,
    temp: PathBuf,
) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Some(dir) = ahi_home {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = xdg_cache_home {
        return PathBuf::from(xdg).join("ahi");
    }
    temp.join("ahi")
}

/// Directory for hook scratch files. `std::env::temp_dir` honors `TMPDIR`.
pub fn scratch_dir() -> PathBuf {
    std::env::temp_dir()
}

fn probe_writable(dir: &Path) -> bool {
    let probe = dir.join(".ahi-write-probe");
    match File::create(&probe) {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip_and_detects_tampering() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("object-store");
        let store = Store::open(&dir).unwrap();
        let digest = store.put(b"hello").unwrap();
        assert_eq!(store.get(&digest).unwrap(), b"hello");
        assert_eq!(store.put(b"hello").unwrap(), digest);
        let path = store.object_path(&digest).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert!(store
            .get(&digest)
            .unwrap_err()
            .to_string()
            .contains("verification"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn open_readonly_store_reads_and_refuses_put() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            crate::fixture_directory::create_unique_fixture_directory("object-store-readonly");
        let store = Store::open(&dir).unwrap();
        let digest = store.put(b"cached").unwrap();
        drop(store);
        let objects = dir.join("objects/sha256");
        let mut perms = std::fs::metadata(&objects).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&objects, perms).unwrap();
        let store = Store::open(&dir).unwrap();
        let writable = store.is_writable();
        let got = store.get(&digest);
        let put_err = store.put(b"nope");
        let mut perms = std::fs::metadata(&objects).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&objects, perms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!writable);
        assert_eq!(got.unwrap(), b"cached");
        assert!(put_err.unwrap_err().to_string().contains("read-only"));
    }

    #[test]
    fn cache_dir_prefers_explicit_then_home_then_xdg_then_temp() {
        use std::ffi::OsString;
        let explicit = PathBuf::from("/cache/explicit");
        let home = OsString::from("/cache/ahi-home");
        let xdg = OsString::from("/cache/xdg");
        let temp = PathBuf::from("/cache/tmp");
        assert_eq!(
            resolve_cache_dir(
                Some(&explicit),
                Some(home.clone()),
                Some(xdg.clone()),
                temp.clone()
            ),
            explicit
        );
        assert_eq!(
            resolve_cache_dir(None, Some(home.clone()), Some(xdg.clone()), temp.clone()),
            PathBuf::from("/cache/ahi-home")
        );
        assert_eq!(
            resolve_cache_dir(None, None, Some(xdg), temp.clone()),
            PathBuf::from("/cache/xdg/ahi")
        );
        assert_eq!(
            resolve_cache_dir(None, None, None, temp.clone()),
            temp.join("ahi")
        );
    }
}
