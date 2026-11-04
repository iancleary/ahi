//! Replace this ahi binary from the attested Linux archive of the latest
//! release this repository publishes.
//!
//! The flow mirrors `docs/release.md`: resolve the latest release, fetch
//! `ahi-<version>-sha256sums.txt`, download the archive for the target
//! architecture, verify the sha256, and verify the GitHub build-provenance
//! attestation of both files with the `gh` CLI. Only then does it extract
//! the single `ahi` member and atomically rename it over the running
//! binary. Any failure refuses the target and leaves the binary untouched.
//! The command is not named `self-update`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::VERSION;

const REPO_SLUG: &str = "iancleary/ahi";
const RELEASE_WORKFLOW: &str = ".github/workflows/release_binary_assets.yml";
const ATTESTATION_PREDICATE_TYPE: &str = "https://slsa.dev/provenance/v1";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_CAP: u64 = 128 * 1024 * 1024;
const SHA256SUMS_CAP: u64 = 64 * 1024;
const BINARY_CAP: u64 = 64 * 1024 * 1024;

/// Run `ahi update`.
pub fn run() -> Result<()> {
    if !platform_supported(std::env::consts::OS) {
        return Err(Error::new(format!(
            "ahi update replaces the binary from the attested Linux archives; on {} install with `cargo install ahi`",
            std::env::consts::OS
        )));
    }
    println!("Resolving the latest ahi release...");
    let release = latest_release()?;
    let tag = release.tag_name.clone();
    let latest = tag.strip_prefix('v').ok_or_else(|| {
        Error::new(format!(
            "release tag `{tag}` is not a v-prefixed semver tag"
        ))
    })?;
    let current = parse_version(VERSION)?;
    let target = parse_version(latest)?;
    if target <= current {
        println!("ahi {VERSION} is up to date (latest release {tag})");
        return Ok(());
    }
    let artifact = artifact_for(std::env::consts::ARCH, latest)?;
    for name in [&artifact, &format!("ahi-{latest}-sha256sums.txt")] {
        if !release.assets.iter().any(|asset| asset.name == *name) {
            return Err(Error::new(format!(
                "release {tag} does not publish `{name}`"
            )));
        }
    }
    println!("Preparing ahi {latest} ({artifact})...");
    let scratch = scratch_dir()?;
    let result = install(&tag, latest, &artifact, &scratch);
    let _ = fs::remove_dir_all(&scratch);
    result
}

fn install(tag: &str, version: &str, artifact: &str, scratch: &Path) -> Result<()> {
    let sums_name = format!("ahi-{version}-sha256sums.txt");
    println!("[1/6] Fetching {sums_name}...");
    let sums_path = scratch.join(&sums_name);
    download_file(&asset_url(tag, &sums_name), &sums_path, SHA256SUMS_CAP)?;

    println!("[2/6] Verifying the sha256sums attestation with gh...");
    verify_attestation(tag, &sums_path)?;

    println!("[3/6] Checking the checksums file...");
    let body = fs::read_to_string(&sums_path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", sums_path.display())))?;
    let expected = expected_sha256(&body, version, artifact)?;

    println!("[4/6] Downloading {artifact} and verifying its sha256...");
    let archive_path = scratch.join(artifact);
    download_file(&asset_url(tag, artifact), &archive_path, DOWNLOAD_CAP)?;
    let actual = sha256_file(&archive_path)?;
    if actual != expected {
        return Err(Error::new(format!(
            "sha256 mismatch for {artifact}: expected {expected}, got {actual}"
        )));
    }

    println!("[5/6] Verifying the archive attestation with gh...");
    verify_attestation(tag, &archive_path)?;

    println!("[6/6] Replacing the running binary...");
    let bytes = extract_ahi(&archive_path)?;
    let replaced = replace_binary(&bytes)?;
    println!(
        "Replaced {} with ahi {version} ({artifact}, sha256 {actual}, attestation verified).",
        replaced.display()
    );
    Ok(())
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
}

fn latest_release() -> Result<LatestRelease> {
    let url = format!("https://api.github.com/repos/{REPO_SLUG}/releases/latest");
    let body = fetch_json(&url)?;
    serde_json::from_slice(&body).map_err(|err| {
        Error::new(format!(
            "latest release from {url} is not valid JSON: {err}"
        ))
    })
}

fn fetch_json(url: &str) -> Result<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .timeout_write(FETCH_TIMEOUT)
        .build();
    let response = match agent
        .get(url)
        .set("User-Agent", &format!("ahi/{VERSION}"))
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => {
            let hint = match code {
                404 => "; no published release was found".to_string(),
                403 => "; the GitHub API rate limit may be exhausted".to_string(),
                _ => String::new(),
            };
            return Err(Error::new(format!(
                "release lookup {url}: status {code}{hint}"
            )));
        }
        Err(err) => return Err(Error::new(format!("release lookup {url}: {err}"))),
    };
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(Error::new(format!("release lookup {url}: status {status}")));
    }
    let cap = 1024 * 1024;
    let mut buf = Vec::new();
    response
        .into_reader()
        .take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|err| Error::new(format!("release body {url}: {err}")))?;
    if buf.len() as u64 > cap {
        return Err(Error::new(format!("release body from {url} exceeds 1 MiB")));
    }
    Ok(buf)
}

fn download_file(url: &str, dest: &Path, cap: u64) -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .timeout_write(FETCH_TIMEOUT)
        .build();
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => {
            return Err(Error::new(format!("download {url}: status {code}")))
        }
        Err(err) => return Err(Error::new(format!("download {url}: {err}"))),
    };
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(Error::new(format!("download {url}: status {status}")));
    }
    let mut reader = response.into_reader().take(cap + 1);
    let mut file = fs::File::create(dest)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", dest.display())))?;
    let written = std::io::copy(&mut reader, &mut file)
        .map_err(|err| Error::new(format!("download {url}: {err}")))?;
    if written > cap {
        let _ = fs::remove_file(dest);
        return Err(Error::new(format!(
            "download {url} exceeds the {cap} byte cap"
        )));
    }
    file.sync_all()
        .map_err(|err| Error::new(format!("failed to sync {}: {err}", dest.display())))?;
    Ok(())
}

fn asset_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPO_SLUG}/releases/download/{tag}/{name}")
}

fn platform_supported(os: &str) -> bool {
    os == "linux"
}

fn artifact_for(arch: &str, version: &str) -> Result<String> {
    match arch {
        "x86_64" => Ok(format!("ahi-{version}-linux-x86_64.tar.gz")),
        "aarch64" => Ok(format!("ahi-{version}-linux-aarch64.tar.gz")),
        other => Err(Error::new(format!(
            "no published archive for target `{other}`; the release publishes linux-x86_64 and linux-aarch64 only"
        ))),
    }
}

fn parse_version(value: &str) -> Result<(u64, u64, u64)> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(Error::new(format!(
            "version `{value}` is not three dot-separated numbers"
        )));
    }
    let mut numbers = [0_u64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::new(format!(
                "version `{value}` has a non-numeric component"
            )));
        }
        numbers[index] = part
            .parse()
            .map_err(|err| Error::new(format!("version `{value}`: {err}")))?;
    }
    Ok((numbers[0], numbers[1], numbers[2]))
}

fn expected_sha256(body: &str, version: &str, artifact: &str) -> Result<String> {
    let x86 = format!("ahi-{version}-linux-x86_64.tar.gz");
    let aarch64 = format!("ahi-{version}-linux-aarch64.tar.gz");
    let mut seen = std::collections::BTreeSet::new();
    let mut found = None;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (hash, name) = match (parts.next(), parts.next(), parts.next()) {
            (Some(hash), Some(name), None) => (hash, name),
            _ => {
                return Err(Error::new(format!("malformed sha256sums line: {line:?}")));
            }
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::new(format!(
                "sha256sums line has a malformed digest: {line:?}"
            )));
        }
        if name != x86 && name != aarch64 {
            return Err(Error::new(format!(
                "sha256sums names an unexpected artifact {name:?}"
            )));
        }
        if !seen.insert(name.to_string()) {
            return Err(Error::new(format!(
                "sha256sums lists {name} more than once"
            )));
        }
        if name == artifact {
            found = Some(hash.to_string());
        }
    }
    if seen != std::collections::BTreeSet::from([x86, aarch64]) {
        return Err(Error::new(format!(
            "sha256sums does not list exactly the two published archives for {version}"
        )));
    }
    found.ok_or_else(|| Error::new(format!("sha256sums does not list {artifact}")))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .map_err(|err| Error::new(format!("failed to open {}: {err}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_ahi(archive: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(archive)
        .map_err(|err| Error::new(format!("failed to open {}: {err}", archive.display())))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|err| Error::new(format!("failed to read the archive: {err}")))?;
    let mut bytes = None;
    for (index, entry) in entries.enumerate() {
        let index = index + 1;
        let entry = entry.map_err(|err| Error::new(format!("archive entry {index}: {err}")))?;
        let raw = entry
            .path()
            .map_err(|err| Error::new(format!("archive entry {index} path: {err}")))?
            .to_string_lossy()
            .to_string();
        let name = raw.strip_prefix("./").unwrap_or(&raw);
        if name != "ahi" {
            return Err(Error::new(format!(
                "archive member `{raw}` is not `ahi`; refusing the unexpected layout"
            )));
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            return Err(Error::new(format!(
                "archive member `{name}` is not a regular file"
            )));
        }
        if bytes.is_some() {
            return Err(Error::new("archive contains more than one `ahi` member"));
        }
        let mut buf = Vec::new();
        entry
            .take(BINARY_CAP + 1)
            .read_to_end(&mut buf)
            .map_err(|err| Error::new(format!("archive member `{name}`: {err}")))?;
        if buf.len() as u64 > BINARY_CAP {
            return Err(Error::new(format!(
                "archive member `{name}` exceeds the {BINARY_CAP} byte cap"
            )));
        }
        if buf.is_empty() {
            return Err(Error::new("archive member `ahi` is empty"));
        }
        bytes = Some(buf);
    }
    bytes.ok_or_else(|| Error::new("archive contains no `ahi` file"))
}

fn gh_output(args: &[&str]) -> Result<String> {
    let output = Command::new("gh").args(args).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                "the `gh` CLI is required for attestation verification; install https://cli.github.com/ and run `gh auth login`"
                    .to_string(),
            )
        } else {
            Error::new(format!("failed to run gh: {err}"))
        }
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::new(format!(
            "gh {} failed: {detail}",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn verify_attestation(tag: &str, path: &Path) -> Result<()> {
    let commit = gh_output(&[
        "api",
        &format!("repos/{REPO_SLUG}/commits/{tag}"),
        "--jq",
        ".sha",
    ])?;
    let commit = commit.trim();
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::new(format!(
            "release tag {tag} did not resolve to a commit sha: {commit}"
        )));
    }
    let args = [
        "attestation".to_string(),
        "verify".to_string(),
        path.display().to_string(),
        "--repo".to_string(),
        REPO_SLUG.to_string(),
        "--source-ref".to_string(),
        format!("refs/tags/{tag}"),
        "--source-digest".to_string(),
        commit.to_string(),
        "--signer-workflow".to_string(),
        format!("{REPO_SLUG}/{RELEASE_WORKFLOW}"),
        "--predicate-type".to_string(),
        ATTESTATION_PREDICATE_TYPE.to_string(),
    ];
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = Command::new("gh").args(&arg_refs).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                "the `gh` CLI is required for attestation verification; install https://cli.github.com/ and run `gh auth login`"
                    .to_string(),
            )
        } else {
            Error::new(format!("failed to run gh: {err}"))
        }
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::new(format!(
            "attestation verification failed for {}: {detail}",
            path.display()
        )));
    }
    Ok(())
}

fn replace_binary(bytes: &[u8]) -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|err| Error::new(format!("failed to locate the running ahi binary: {err}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::new("the running ahi binary has no parent directory"))?
        .to_path_buf();
    let dir_meta = fs::symlink_metadata(&dir)
        .map_err(|err| Error::new(format!("failed to stat {}: {err}", dir.display())))?;
    if dir_meta.file_type().is_symlink() {
        return Err(Error::new(format!(
            "refusing to replace the binary through a symlinked directory: {}",
            dir.display()
        )));
    }
    let temp = dir.join(format!(".ahi.new.{}", random_token()?));
    let written = (|| -> Result<()> {
        fs::write(&temp, bytes)
            .map_err(|err| Error::new(format!("failed to write {}: {err}", temp.display())))?;
        set_executable(&temp)?;
        let file = fs::File::open(&temp)
            .map_err(|err| Error::new(format!("failed to open {}: {err}", temp.display())))?;
        file.sync_all()
            .map_err(|err| Error::new(format!("failed to sync {}: {err}", temp.display())))?;
        fs::rename(&temp, &exe)
            .map_err(|err| Error::new(format!("failed to replace {}: {err}", exe.display())))?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    Ok(exe)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|err| Error::new(format!("failed to stat {}: {err}", path.display())))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|err| Error::new(format!("failed to chmod {}: {err}", path.display())))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Err(Error::new(
        "binary replacement is only implemented for unix targets",
    ))
}

fn scratch_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("ahi-update-{}", random_token()?));
    fs::create_dir_all(&dir)
        .map_err(|err| Error::new(format!("failed to create {}: {err}", dir.display())))?;
    Ok(dir)
}

fn random_token() -> Result<String> {
    let mut buf = [0_u8; 8];
    getrandom::getrandom(&mut buf).map_err(|err| Error::new(format!("random: {err}")))?;
    Ok(buf.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION_STAMP: &str = "0.4.0";

    #[test]
    fn artifact_names_follow_the_release_contract() {
        assert_eq!(
            artifact_for("x86_64", VERSION_STAMP).unwrap(),
            "ahi-0.4.0-linux-x86_64.tar.gz"
        );
        assert_eq!(
            artifact_for("aarch64", VERSION_STAMP).unwrap(),
            "ahi-0.4.0-linux-aarch64.tar.gz"
        );
        let err = artifact_for("armv7", VERSION_STAMP).unwrap_err();
        assert!(err.to_string().contains("no published archive"));
    }

    #[test]
    fn platform_gate_is_linux_only() {
        assert!(platform_supported("linux"));
        assert!(!platform_supported("macos"));
        assert!(!platform_supported("windows"));
    }

    #[test]
    fn parse_version_accepts_v_prefix_and_rejects_garbage() {
        assert_eq!(parse_version("0.4.0").unwrap(), (0, 4, 0));
        assert_eq!(parse_version("v0.4.0").unwrap(), (0, 4, 0));
        assert!(parse_version("0.4").is_err());
        assert!(parse_version("0.4.0.1").is_err());
        assert!(parse_version("0.4.0-beta").is_err());
        assert!(parse_version("").is_err());
    }

    #[test]
    fn expected_sha256_accepts_exactly_the_two_archives() {
        let x86 = format!("ahi-{VERSION_STAMP}-linux-x86_64.tar.gz");
        let aarch64 = format!("ahi-{VERSION_STAMP}-linux-aarch64.tar.gz");
        let digest = "a".repeat(64);
        let other_digest = "b".repeat(64);
        let body = format!("{digest}  {x86}\n{other_digest}  {aarch64}\n");
        assert_eq!(expected_sha256(&body, VERSION_STAMP, &x86).unwrap(), digest);
        assert_eq!(
            expected_sha256(&body, VERSION_STAMP, &aarch64).unwrap(),
            other_digest
        );
    }

    #[test]
    fn expected_sha256_refuses_malformed_input() {
        let x86 = format!("ahi-{VERSION_STAMP}-linux-x86_64.tar.gz");
        let aarch64 = format!("ahi-{VERSION_STAMP}-linux-aarch64.tar.gz");
        let digest = "a".repeat(64);
        let one_line = format!("{digest}  {x86}\n");
        assert!(expected_sha256(&one_line, VERSION_STAMP, &x86)
            .unwrap_err()
            .to_string()
            .contains("does not list exactly"));
        let three_lines = format!("{digest}  {x86}\n{digest}  {aarch64}\n{digest}  extra.tar.gz\n");
        assert!(expected_sha256(&three_lines, VERSION_STAMP, &x86)
            .unwrap_err()
            .to_string()
            .contains("unexpected artifact"));
        let duplicate = format!("{digest}  {x86}\n{digest}  {x86}\n");
        assert!(expected_sha256(&duplicate, VERSION_STAMP, &x86)
            .unwrap_err()
            .to_string()
            .contains("more than once"));
        let short_hash = format!("abc123  {x86}\n{digest}  {aarch64}\n");
        assert!(expected_sha256(&short_hash, VERSION_STAMP, &x86)
            .unwrap_err()
            .to_string()
            .contains("malformed digest"));
        let junk = "not a checksum line\n";
        assert!(expected_sha256(junk, VERSION_STAMP, &x86)
            .unwrap_err()
            .to_string()
            .contains("malformed sha256sums line"));
    }

    fn build_archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            std::io::Cursor::new(Vec::new()),
            flate2::Compression::default(),
        ));
        for (name, entry_type, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_entry_type(*entry_type);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap().into_inner()
    }

    fn write_temp_archive(bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahi-update-test-{}", random_token().unwrap()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.tar.gz");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn extract_ahi_accepts_the_single_root_member() {
        let path = write_temp_archive(&build_archive(&[(
            "ahi",
            tar::EntryType::Regular,
            b"binary",
        )]));
        assert_eq!(extract_ahi(&path).unwrap(), b"binary");
        let dotted = write_temp_archive(&build_archive(&[(
            "./ahi",
            tar::EntryType::Regular,
            b"binary",
        )]));
        assert_eq!(extract_ahi(&dotted).unwrap(), b"binary");
        let dir = path.parent().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn extract_ahi_refuses_unexpected_layouts() {
        let subdir = write_temp_archive(&build_archive(&[(
            "bin/ahi",
            tar::EntryType::Regular,
            b"binary",
        )]));
        assert!(extract_ahi(&subdir)
            .unwrap_err()
            .to_string()
            .contains("unexpected layout"));

        let directory =
            write_temp_archive(&build_archive(&[("ahi", tar::EntryType::Directory, b"")]));
        assert!(extract_ahi(&directory)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));

        let symlink = write_temp_archive(&build_archive(&[("ahi", tar::EntryType::Symlink, b"")]));
        assert!(extract_ahi(&symlink)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));

        let extra = write_temp_archive(&build_archive(&[
            ("ahi", tar::EntryType::Regular, b"binary"),
            ("other", tar::EntryType::Regular, b"binary"),
        ]));
        assert!(extract_ahi(&extra)
            .unwrap_err()
            .to_string()
            .contains("unexpected layout"));

        let empty = write_temp_archive(&{
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
                std::io::Cursor::new(Vec::new()),
                flate2::Compression::default(),
            ));
            builder.finish().unwrap();
            builder.into_inner().unwrap().finish().unwrap().into_inner()
        });
        assert!(extract_ahi(&empty)
            .unwrap_err()
            .to_string()
            .contains("no `ahi` file"));
        for path in [&subdir, &directory, &symlink, &extra, &empty] {
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[test]
    fn extract_ahi_refuses_an_empty_member() {
        let path = write_temp_archive(&build_archive(&[("ahi", tar::EntryType::Regular, b"")]));
        assert!(extract_ahi(&path)
            .unwrap_err()
            .to_string()
            .contains("empty"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sha256_file_matches_the_sha256sum_format() {
        let dir = std::env::temp_dir().join(format!("ahi-update-test-{}", random_token().unwrap()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe");
        fs::write(&path, b"ahi").unwrap();
        // printf 'ahi' | sha256sum
        assert_eq!(
            sha256_file(&path).unwrap(),
            "c36dbb01b9f272ed3e090a6532cd3cac04f3dd6bf471677dec09bf1b15338276"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
