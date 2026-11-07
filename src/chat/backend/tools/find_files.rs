//! `find_files`: paths under a directory whose name or path matches a glob.

use std::path::Path;

use super::glob::Glob;
use super::walk;
use super::workspace::Workspace;

pub(super) const DEFAULT_LIMIT: usize = 200;
pub(super) const MAX_LIMIT: usize = 1000;

pub(super) fn run(
    workspace: &Workspace,
    directory: &Path,
    relative: &str,
    pattern: &str,
    limit: usize,
) -> Result<String, &'static str> {
    if !directory.is_dir() {
        return Err("not a directory");
    }
    let glob = Glob::new(pattern).ok_or("invalid glob pattern")?;
    let limit = limit.clamp(1, MAX_LIMIT);
    let entries = walk::walk(workspace, directory, relative)?;
    let mut lines = Vec::new();
    let mut matched = 0usize;
    for entry in &entries {
        // Match against the path below the searched directory, so a pattern
        // written for `src/` works the same from the root and from `src`.
        let below = entry
            .relative
            .strip_prefix(relative)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(&entry.relative);
        if !glob.matches(below) {
            continue;
        }
        matched += 1;
        if lines.len() < limit {
            let mut line = entry.relative.clone();
            if entry.is_symlink {
                line.push('@');
            } else if entry.is_dir {
                line.push('/');
            }
            lines.push(line);
        }
    }
    if matched == 0 {
        return Ok("[no matches]".to_string());
    }
    if matched > lines.len() {
        lines.push(format!(
            "[{} more matches not shown; raise limit or narrow the pattern]",
            matched - lines.len()
        ));
    }
    Ok(lines.join("\n"))
}
