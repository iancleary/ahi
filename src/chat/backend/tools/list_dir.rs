use std::path::Path;

pub(super) const MAX_ENTRIES: usize = 500;
/// Entries examined before sorting, so a huge directory stays bounded.
const MAX_SCANNED: usize = 20_000;

pub(super) fn run(path: &Path) -> Result<String, &'static str> {
    let metadata = path
        .metadata()
        .map_err(|_| "cannot read directory metadata")?;
    if !metadata.is_dir() {
        return Err("not a directory");
    }
    let mut names = Vec::new();
    let mut scanned = 0usize;
    for entry in std::fs::read_dir(path).map_err(|_| "cannot read directory")? {
        scanned += 1;
        if scanned > MAX_SCANNED {
            return Err("directory has too many entries to list");
        }
        let entry = entry.map_err(|_| "cannot read directory")?;
        let kind = entry.file_type().map_err(|_| "cannot read directory")?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        // Names come from the filesystem; keep one entry per line.
        name = name.replace(|c: char| c.is_control(), "?");
        if kind.is_symlink() {
            name.push('@');
        } else if kind.is_dir() {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    let hidden = names.len().saturating_sub(MAX_ENTRIES);
    names.truncate(MAX_ENTRIES);
    if hidden > 0 {
        names.push(format!("[{hidden} more entries not shown]"));
    }
    Ok(names.join("\n"))
}
