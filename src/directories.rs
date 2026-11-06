//! Where ahi keeps its own files, following the XDG Base Directory
//! Specification. Configuration (`models.json`), private data (credentials),
//! session state (`selection`, `thinking`, `audit.log`), and caches
//! (catalogs, content-addressed objects) each get their own base with an
//! `ahi` child. An explicit `--state-dir` or `AHI_STATE_DIR` is a deliberate
//! single-root layout for portable or sandboxed setups and is never chosen
//! implicitly. Nothing here reads or migrates files from older locations.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directories {
    /// `models.json`.
    pub config: PathBuf,
    /// `credentials/` and `openai-device-id`.
    pub data: PathBuf,
    /// `selection`, `thinking`, and `audit.log`.
    pub state: PathBuf,
    /// `catalogs/` and `objects/`.
    pub cache: PathBuf,
}

impl Directories {
    /// Every category under one root. Only for explicit overrides and tests.
    pub fn single_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.clone(),
            data: root.clone(),
            state: root.clone(),
            cache: root,
        }
    }

    /// Resolve from the process environment. `root` is `--state-dir`, which
    /// wins over `AHI_STATE_DIR`; `cache` is `--cache-dir`, which wins over
    /// `AHI_HOME`. Either override applies on top of the XDG defaults.
    pub fn resolve(root: Option<&Path>, cache: Option<&Path>) -> Result<Self> {
        Self::from_env(root, cache, |key| std::env::var_os(key))
    }

    fn from_env(
        root: Option<&Path>,
        cache: Option<&Path>,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        let cache = cache
            .map(Path::to_path_buf)
            .or_else(|| non_empty(env("AHI_HOME")));
        let root = root
            .map(Path::to_path_buf)
            .or_else(|| non_empty(env("AHI_STATE_DIR")));
        if let Some(root) = root {
            let mut directories = Self::single_root(root);
            if let Some(cache) = cache {
                directories.cache = cache;
            }
            return Ok(directories);
        }
        Ok(Self {
            config: base_directory("XDG_CONFIG_HOME", ".config", &env)?,
            data: base_directory("XDG_DATA_HOME", ".local/share", &env)?,
            state: base_directory("XDG_STATE_HOME", ".local/state", &env)?,
            cache: match cache {
                Some(cache) => cache,
                None => base_directory("XDG_CACHE_HOME", ".cache", &env)?,
            },
        })
    }
}

fn non_empty(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    non_empty(value).filter(|path| path.is_absolute())
}

/// One XDG base joined with `ahi`. The variable must be absolute, as the
/// specification requires; otherwise the `HOME` default applies. On Windows
/// without XDG variables, `%LOCALAPPDATA%\ahi\<category>` keeps the
/// categories apart.
fn base_directory(
    key: &str,
    default: &str,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    if let Some(base) = absolute(env(key)) {
        return Ok(base.join("ahi"));
    }
    if cfg!(windows) {
        if let Some(base) = absolute(env("LOCALAPPDATA")) {
            let category = match key {
                "XDG_CONFIG_HOME" => "config",
                "XDG_DATA_HOME" => "data",
                "XDG_STATE_HOME" => "state",
                _ => "cache",
            };
            return Ok(base.join("ahi").join(category));
        }
    }
    absolute(env("HOME"))
        .map(|home| home.join(default).join("ahi"))
        .ok_or_else(|| {
            Error::new(format!(
                "no {key} directory: set an absolute {key} or HOME, or pass --state-dir"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(values: &[(&str, &str)]) -> Result<Directories> {
        Directories::from_env(None, None, |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    #[cfg(unix)]
    fn home_defaults_separate_the_four_categories() {
        let dirs = resolve(&[("HOME", "/home/alice")]).unwrap();
        assert_eq!(dirs.config, Path::new("/home/alice/.config/ahi"));
        assert_eq!(dirs.data, Path::new("/home/alice/.local/share/ahi"));
        assert_eq!(dirs.state, Path::new("/home/alice/.local/state/ahi"));
        assert_eq!(dirs.cache, Path::new("/home/alice/.cache/ahi"));
    }

    #[test]
    #[cfg(unix)]
    fn absolute_xdg_values_win_and_need_no_home() {
        let dirs = resolve(&[
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_DATA_HOME", "/d"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/k"),
        ])
        .unwrap();
        assert_eq!(dirs.config, Path::new("/c/ahi"));
        assert_eq!(dirs.data, Path::new("/d/ahi"));
        assert_eq!(dirs.state, Path::new("/s/ahi"));
        assert_eq!(dirs.cache, Path::new("/k/ahi"));
    }

    #[test]
    #[cfg(unix)]
    fn empty_and_relative_xdg_values_fall_back_to_home() {
        let dirs = resolve(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "relative"),
            ("XDG_DATA_HOME", ""),
            ("XDG_STATE_HOME", "./state"),
            ("XDG_CACHE_HOME", ""),
        ])
        .unwrap();
        assert_eq!(dirs, resolve(&[("HOME", "/home/alice")]).unwrap());
    }

    #[test]
    fn single_root_overrides_colocate_everything_except_an_explicit_cache() {
        let dirs = Directories::from_env(Some(Path::new("sandbox")), None, |key| {
            (key == "AHI_STATE_DIR").then(|| "ignored".into())
        })
        .unwrap();
        assert_eq!(dirs, Directories::single_root("sandbox"));
        let dirs = Directories::from_env(None, None, |key| match key {
            "AHI_STATE_DIR" => Some("portable".into()),
            "AHI_HOME" => Some("objects".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(dirs.config, Path::new("portable"));
        assert_eq!(dirs.state, Path::new("portable"));
        assert_eq!(dirs.cache, Path::new("objects"));
        let dirs =
            Directories::from_env(Some(Path::new("sandbox")), Some(Path::new("flag")), |key| {
                (key == "AHI_HOME").then(|| "ignored".into())
            })
            .unwrap();
        assert_eq!(dirs.cache, Path::new("flag"));
    }

    #[test]
    fn empty_overrides_behave_as_unset_and_no_home_is_an_error() {
        let error = Directories::from_env(None, None, |key| {
            matches!(key, "AHI_STATE_DIR" | "AHI_HOME").then(|| "".into())
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("XDG_CONFIG_HOME"), "{error}");
        assert!(error.contains("--state-dir"), "{error}");
    }

    #[test]
    #[cfg(windows)]
    fn local_app_data_separates_categories_on_windows() {
        let dirs = resolve(&[("LOCALAPPDATA", r"C:\Users\alice\AppData\Local")]).unwrap();
        let base = Path::new(r"C:\Users\alice\AppData\Local\ahi");
        assert_eq!(dirs.config, base.join("config"));
        assert_eq!(dirs.data, base.join("data"));
        assert_eq!(dirs.state, base.join("state"));
        assert_eq!(dirs.cache, base.join("cache"));
    }
}
