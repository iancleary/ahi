//! Join a repo-relative path and refuse anything that escapes the root.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

pub fn join_inside(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.contains('\0') || rel.contains('\n') {
        return Err(Error::new(format!(
            "refusing path that contains a NUL or newline: {rel:?}"
        )));
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(Error::new(format!("refusing absolute path `{rel}`")));
    }
    let mut out = root.to_path_buf();
    for component in rel_path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(Error::new(format!(
                    "refusing path that escapes the repository: {rel}"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::new(format!("refusing absolute path `{rel}`")));
            }
        }
    }
    Ok(out)
}
