use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(super) const MAX_BYTES: usize = 64 * 1024;

pub(super) fn run(path: &Path) -> Result<String, &'static str> {
    let metadata = path.metadata().map_err(|_| "cannot read file metadata")?;
    if !metadata.is_file() {
        return Err("not a regular file");
    }
    let mut bytes = Vec::with_capacity(MAX_BYTES.min(metadata.len() as usize) + 1);
    File::open(path)
        .and_then(|file| file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| "cannot read file")?;
    let truncated = bytes.len() > MAX_BYTES;
    bytes.truncate(MAX_BYTES);
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // A cap may split a multibyte character; anything else is not text.
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).expect("valid prefix")
        }
        Err(_) => return Err("file is not UTF-8 text"),
    };
    if text.contains('\0') {
        return Err("file is not UTF-8 text");
    }
    if truncated {
        text.push_str(&format!("\n[truncated at {MAX_BYTES} bytes]"));
    }
    Ok(text)
}
