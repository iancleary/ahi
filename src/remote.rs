//! Resolve a hook repository to one tag, one commit, and one tree digest.
//!
//! `ahi lock` records the rev already in the config. `ahi update` may select a
//! newer immutable version tag and rewrite that rev to the tag, not to a SHA.
//! `ahi fetch` materializes the locked tree. `ahi verify` and `ahi run` recompute
//! the digest from the local mirror and the materialized files. They do not fetch.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::{Config, HookOriginKind, Repo};
use crate::error::{Error, Result};
use crate::lockfile::{self, LockedRepo, Lockfile, Signature};
use crate::policy::{self, Policy};
use crate::store::{self, sha256_hex};

pub struct Workspace<'a> {
    pub root: &'a Path,
    pub config_path: &'a Path,
    pub config: &'a Config,
    pub cache: &'a Path,
}

pub fn lock(ws: &Workspace<'_>) -> Result<Vec<String>> {
    pin(ws, false, false)
}

pub fn update(ws: &Workspace<'_>, dry_run: bool) -> Result<Vec<String>> {
    pin(ws, true, dry_run)
}

pub fn verify_for_run(root: &Path, config: &Config, cache: &Path) -> Result<()> {
    let repos = remote_repos(config)?;
    if repos.is_empty() {
        return Ok(());
    }
    let lock = lockfile::load(root)?;
    check_rows(&repos, &lock)?;
    for row in &lock.repos {
        assert_cached(cache, row)?;
    }
    Ok(())
}

pub fn verify(ws: &Workspace<'_>, require_signatures: bool) -> Result<Vec<String>> {
    let repos = remote_repos(ws.config)?;
    if repos.is_empty() {
        return Ok(vec!["no remote repos".to_string()]);
    }
    let lock = lockfile::load(ws.root)?;
    check_rows(&repos, &lock)?;
    let mut lines = Vec::new();
    for row in &lock.repos {
        if row.signature == Signature::Failed {
            return Err(Error::new(format!("signature for `{}` failed", row.repo)));
        }
        if require_signatures && row.signature != Signature::Verified {
            return Err(Error::new(format!(
                "signature for `{}` is {}; --require-signatures needs verified",
                row.repo,
                row.signature.as_str()
            )));
        }
        assert_cached(ws.cache, row)?;
        lines.push(format!(
            "verified {} {} signature {}",
            row.repo,
            row.commit,
            row.signature.as_str()
        ));
    }
    Ok(lines)
}

pub fn fetch(ws: &Workspace<'_>) -> Result<Vec<String>> {
    let repos = remote_repos(ws.config)?;
    if repos.is_empty() {
        return Ok(vec!["no remote repos".to_string()]);
    }
    refuse_dependencies(&repos)?;
    let policy = policy::load_policy(ws.root)?;
    ensure_allowed(&policy, &repos)?;
    let lock = lockfile::load(ws.root)?;
    check_rows(&repos, &lock)?;
    let mut lines = Vec::new();
    for row in &lock.repos {
        let resolved = resolve_recorded(ws.cache, &row.requested, row)?;
        if resolved.signature == Signature::Failed {
            return Err(Error::new(format!("signature for `{}` failed", row.repo)));
        }
        materialize(ws.cache, row, &resolved.listing)?;
        assert_cached(ws.cache, row)?;
        lines.push(format!("fetched {} {}", row.repo, row.commit));
    }
    Ok(lines)
}

struct Plan {
    requested: String,
    commit: String,
    /// Set when `requested` is a newer tag than the config rev.
    previous_rev: Option<String>,
}

fn pin(ws: &Workspace<'_>, select_newer: bool, dry_run: bool) -> Result<Vec<String>> {
    let repos = remote_repos(ws.config)?;
    if repos.is_empty() {
        return Ok(vec!["no remote repos".to_string()]);
    }
    refuse_dependencies(&repos)?;
    let policy = policy::load_policy(ws.root)?;
    ensure_allowed(&policy, &repos)?;
    let existing = lockfile::load_optional(ws.root)?;
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    let mut edits = Vec::new();
    for repo in repos {
        let rev = required_rev(repo)?;
        let plan = plan_rev(ws.root, &repo.repo, rev, select_newer)?;
        let previous = existing
            .as_ref()
            .and_then(|lock| lock.repos.iter().find(|row| row.repo == repo.repo));
        if let Some(previous) = previous {
            if previous.requested == plan.requested && previous.commit != plan.commit {
                return Err(Error::new(format!(
                    "tag `{}` for `{}` moved from `{}` to `{}`",
                    plan.requested, repo.repo, previous.commit, plan.commit
                )));
            }
        }
        if dry_run {
            lines.push(describe(&repo.repo, rev, &plan, true, previous.is_some()));
            continue;
        }
        let resolved = resolve_plan(ws.cache, &repo.repo, &plan)?;
        if let Some(previous) = previous {
            if previous.requested == resolved.requested {
                ensure_same(previous, &resolved)?;
                rows.push(previous.clone());
                lines.push(describe(&repo.repo, rev, &plan, false, true));
                continue;
            }
        }
        if plan.previous_rev.is_some() {
            edits.push((repo.repo.clone(), plan.requested.clone()));
        }
        lines.push(describe(&repo.repo, rev, &plan, false, false));
        rows.push(resolved.into_row());
    }
    if dry_run {
        return Ok(lines);
    }
    publish(ws, &rows, &edits)?;
    Ok(lines)
}

fn describe(repo: &str, rev: &str, plan: &Plan, dry_run: bool, kept: bool) -> String {
    let prefix = if dry_run { "dry-run " } else { "" };
    if let Some(previous) = &plan.previous_rev {
        return format!(
            "{prefix}updated {repo} {previous} -> {} {}",
            plan.requested, plan.commit
        );
    }
    if kept {
        format!(
            "{prefix}unchanged {repo} {} {}",
            plan.requested, plan.commit
        )
    } else if rev == plan.requested {
        format!("{prefix}locked {repo} {} {}", plan.requested, plan.commit)
    } else {
        format!(
            "{prefix}updated {repo} {rev} -> {} {}",
            plan.requested, plan.commit
        )
    }
}

fn publish(ws: &Workspace<'_>, rows: &[LockedRepo], edits: &[(String, String)]) -> Result<()> {
    let lock = Lockfile {
        repos: rows.to_vec(),
    };
    let lock_text = lockfile::render(&lock)?;
    let config_text = if edits.is_empty() {
        None
    } else {
        let original = fs::read_to_string(ws.config_path).map_err(|err| {
            Error::new(format!(
                "failed to read {}: {err}",
                ws.config_path.display()
            ))
        })?;
        let updated = apply_revs(&original, edits)?;
        if updated == original {
            None
        } else {
            Some(updated)
        }
    };
    lockfile::publish(ws.root, ws.config_path, &lock_text, config_text.as_deref())
}

fn apply_revs(text: &str, edits: &[(String, String)]) -> Result<String> {
    let mut text = text.to_string();
    for (repo, rev) in edits {
        text = replace_repo_rev(&text, repo, rev)?;
    }
    Ok(text)
}

struct Resolved {
    repo: String,
    requested: String,
    commit: String,
    tree: String,
    digest: String,
    signature: Signature,
    listing: Vec<u8>,
}

impl Resolved {
    fn into_row(self) -> LockedRepo {
        LockedRepo {
            repo: self.repo,
            requested: self.requested,
            commit: self.commit,
            tree: self.tree,
            digest: self.digest,
            signature: self.signature,
        }
    }
}

fn resolve_plan(cache: &Path, repo: &str, plan: &Plan) -> Result<Resolved> {
    let mirror = open_mirror(cache, repo)?;
    fetch_refs(&mirror, repo)?;
    ensure_commit(&mirror, repo, &plan.commit)?;
    if !is_commit_pin(&plan.requested) {
        let tag_commit = rev_parse(&mirror, &format!("refs/tags/{}^{{commit}}", plan.requested))?;
        if tag_commit != plan.commit {
            return Err(Error::new(format!(
                "tag `{}` for `{repo}` resolved to `{tag_commit}`, not `{}`",
                plan.requested, plan.commit
            )));
        }
    }
    let (tree, digest, listing) = digest_commit(&mirror, &plan.commit)?;
    let signature = classify_signature(&mirror, &plan.commit)?;
    if signature == Signature::Failed {
        return Err(Error::new(format!("signature for `{repo}` failed")));
    }
    Ok(Resolved {
        repo: repo.to_string(),
        requested: plan.requested.clone(),
        commit: plan.commit.clone(),
        tree,
        digest,
        signature,
        listing,
    })
}

fn resolve_recorded(cache: &Path, requested: &str, row: &LockedRepo) -> Result<Resolved> {
    let plan = Plan {
        requested: requested.to_string(),
        commit: row.commit.clone(),
        previous_rev: None,
    };
    let resolved = resolve_plan(cache, &row.repo, &plan)?;
    if resolved.commit != row.commit || resolved.tree != row.tree || resolved.digest != row.digest {
        return Err(Error::new(format!("digest mismatch for `{}`", row.repo)));
    }
    Ok(resolved)
}

fn ensure_same(previous: &LockedRepo, resolved: &Resolved) -> Result<()> {
    if previous.commit != resolved.commit
        || previous.tree != resolved.tree
        || previous.digest != resolved.digest
    {
        return Err(Error::new(format!(
            "tag `{}` for `{}` changed (lock has commit {}, resolved {})",
            resolved.requested, resolved.repo, previous.commit, resolved.commit
        )));
    }
    Ok(())
}

fn plan_rev(root: &Path, repo: &str, rev: &str, select_newer: bool) -> Result<Plan> {
    validate_rev_token(rev)?;
    if is_commit_pin(rev) {
        return Ok(Plan {
            requested: rev.to_string(),
            commit: rev.to_string(),
            previous_rev: None,
        });
    }
    let refs = list_remote(root, repo)?;
    classify_name(rev, &refs)?;
    let current = refs
        .tags
        .get(rev)
        .cloned()
        .ok_or_else(|| Error::new(format!("tag `{rev}` was not found in `{repo}`")))?;
    if !select_newer {
        return Ok(Plan {
            requested: rev.to_string(),
            commit: current,
            previous_rev: None,
        });
    }
    if parse_version(rev).is_none() {
        return Err(Error::new(format!(
            "rev `{rev}` is not an immutable version tag"
        )));
    }
    let names: Vec<String> = refs.tags.keys().cloned().collect();
    let Some(latest) = select_latest(&names) else {
        return Err(Error::new(format!(
            "repo `{repo}` has no immutable version tag"
        )));
    };
    if !is_newer(&latest, rev) {
        return Ok(Plan {
            requested: rev.to_string(),
            commit: current,
            previous_rev: None,
        });
    }
    let commit = refs
        .tags
        .get(&latest)
        .cloned()
        .ok_or_else(|| Error::new(format!("tag `{latest}` was not found in `{repo}`")))?;
    Ok(Plan {
        requested: latest,
        commit,
        previous_rev: Some(rev.to_string()),
    })
}

fn classify_name(rev: &str, refs: &RemoteRefs) -> Result<()> {
    let on_branch = refs.heads.contains_key(rev);
    let on_tag = refs.tags.contains_key(rev);
    match (on_branch, on_tag) {
        (true, true) => Err(Error::new(format!(
            "rev `{rev}` names both a branch and a tag; ahi will not record a branch tip"
        ))),
        (true, false) => Err(Error::new(format!(
            "rev `{rev}` is a branch; ahi will not record a branch tip"
        ))),
        (false, false) => Err(Error::new(format!("tag `{rev}` was not found"))),
        (false, true) => Ok(()),
    }
}

struct RemoteRefs {
    tags: BTreeMap<String, String>,
    heads: BTreeMap<String, String>,
}

fn list_remote(dir: &Path, repo: &str) -> Result<RemoteRefs> {
    let tags = parse_refs(&git_text(dir, &["ls-remote", "--tags", repo])?, true)?;
    let heads = parse_refs(&git_text(dir, &["ls-remote", "--heads", repo])?, false)?;
    Ok(RemoteRefs { tags, heads })
}

fn parse_refs(text: &str, tags: bool) -> Result<BTreeMap<String, String>> {
    let mut objects = BTreeMap::new();
    let mut peeled = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((sha, r#ref)) = line.split_once('\t') else {
            return Err(Error::new(format!("git ls-remote returned `{line}`")));
        };
        if !is_commit_pin(sha) {
            return Err(Error::new(format!(
                "git ls-remote returned a non-commit id `{sha}`"
            )));
        }
        if tags {
            let Some(name) = r#ref.strip_prefix("refs/tags/") else {
                continue;
            };
            if let Some(tag) = name.strip_suffix("^{}") {
                peeled.insert(tag.to_string(), sha.to_string());
            } else {
                objects.insert(name.to_string(), sha.to_string());
            }
        } else if let Some(name) = r#ref.strip_prefix("refs/heads/") {
            objects.insert(name.to_string(), sha.to_string());
        }
    }
    if tags {
        for (name, sha) in peeled {
            objects.insert(name, sha);
        }
    }
    Ok(objects)
}

fn fetch_refs(mirror: &Path, repo: &str) -> Result<()> {
    git(
        mirror,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            repo,
            "+refs/tags/*:refs/tags/*",
            "+refs/heads/*:refs/heads/*",
        ],
    )?;
    Ok(())
}

fn ensure_commit(mirror: &Path, repo: &str, commit: &str) -> Result<()> {
    if rev_parse(mirror, &format!("{commit}^{{commit}}")).is_ok() {
        let kind = object_type(mirror, commit)?;
        if kind != "commit" {
            return Err(Error::new(format!(
                "rev `{commit}` is a `{kind}`, not a commit"
            )));
        }
        return Ok(());
    }
    git(
        mirror,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            repo,
            &format!("{commit}:refs/ahi/pins/{commit}"),
        ],
    )?;
    let actual = rev_parse(mirror, &format!("{commit}^{{commit}}"))?;
    if actual != commit {
        return Err(Error::new(format!(
            "rev `{commit}` resolved to `{actual}`, which is not that commit"
        )));
    }
    Ok(())
}

fn digest_commit(mirror: &Path, commit: &str) -> Result<(String, String, Vec<u8>)> {
    let tree = rev_parse(mirror, &format!("{commit}^{{tree}}"))?;
    let listing = git(mirror, &["ls-tree", "-r", "-z", &tree])?;
    refuse_submodule(&listing)?;
    let digest = format!("sha256:{}", sha256_hex(&listing));
    Ok((tree, digest, listing))
}

fn refuse_submodule(listing: &[u8]) -> Result<()> {
    for entry in parse_listing(listing)? {
        if entry.mode == "160000" || entry.kind == "commit" {
            return Err(Error::new(format!(
                "hook repo contains submodule `{}`, which ahi does not lock",
                entry.path
            )));
        }
    }
    Ok(())
}

fn classify_signature(mirror: &Path, commit: &str) -> Result<Signature> {
    let commit_text = git_text(mirror, &["cat-file", "commit", commit])?;
    if !commit_has_signature(&commit_text) {
        return Ok(Signature::Absent);
    }
    let output = git_raw(mirror, &["verify-commit", commit])?;
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(signature_from_verify(output.status.success(), &text))
}

/// `git verify-commit` on an unsigned commit can exit 1 with empty output.
/// The commit header is the reliable signal that a signature is absent.
fn commit_has_signature(commit_text: &str) -> bool {
    for line in commit_text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with("gpgsig ") || line.starts_with("gpgsig-sha256 ") {
            return true;
        }
    }
    false
}

fn signature_from_verify(ok: bool, text: &str) -> Signature {
    let lower = text.to_ascii_lowercase();
    if ok {
        return Signature::Verified;
    }
    if lower.contains("bad signature") {
        return Signature::Failed;
    }
    if lower.contains("no signature") || lower.contains("does not have a gpg signature") {
        return Signature::Absent;
    }
    if lower.contains("no public key")
        || lower.contains("can't check signature")
        || lower.contains("cannot check signature")
        || lower.contains("public key not found")
        || lower.contains("cannot run gpg")
        || lower.contains("gpg: not found")
        || lower.contains("failed to execute")
        || lower.contains("no such file")
    {
        return Signature::Unverified;
    }
    Signature::Failed
}

fn assert_cached(cache: &Path, row: &LockedRepo) -> Result<()> {
    let mirror = mirror_dir(cache, &row.repo);
    if !mirror.join("config").is_file() {
        return Err(Error::new(format!(
            "mirror for `{}` is missing; run ahi fetch",
            row.repo
        )));
    }
    let commit = rev_parse(&mirror, &format!("{}^{{commit}}", row.commit)).map_err(|_| {
        Error::new(format!(
            "cached commit `{}` for `{}` is missing; run ahi fetch",
            row.commit, row.repo
        ))
    })?;
    if commit != row.commit {
        return Err(Error::new(format!("commit mismatch for `{}`", row.repo)));
    }
    let tree = rev_parse(&mirror, &format!("{}^{{tree}}", row.commit))?;
    if tree != row.tree {
        return Err(Error::new(format!("tree mismatch for `{}`", row.repo)));
    }
    let listing = git(&mirror, &["ls-tree", "-r", "-z", &row.tree])?;
    let digest = format!("sha256:{}", sha256_hex(&listing));
    if digest != row.digest {
        return Err(Error::new(format!("digest mismatch for `{}`", row.repo)));
    }
    let hex = lockfile::digest_hex(&row.digest)?;
    let dest = cache.join("repos").join(hex);
    if !dest.is_dir() {
        return Err(Error::new(format!(
            "cached repo `{}` is missing; run ahi fetch",
            row.repo
        )));
    }
    let saved_path = cache.join("repos").join(format!("{hex}.ls-tree"));
    let saved = fs::read(&saved_path).map_err(|_| {
        Error::new(format!(
            "cached listing for `{}` is missing; run ahi fetch",
            row.repo
        ))
    })?;
    if saved != listing {
        return Err(Error::new(format!("digest mismatch for `{}`", row.repo)));
    }
    check_files(&dest, &parse_listing(&listing)?)?;
    Ok(())
}

fn materialize(cache: &Path, row: &LockedRepo, listing: &[u8]) -> Result<()> {
    let hex = lockfile::digest_hex(&row.digest)?.to_string();
    let mirror = mirror_dir(cache, &row.repo);
    let repos = cache.join("repos");
    fs::create_dir_all(&repos)
        .map_err(|err| Error::new(format!("failed to create cache repos: {err}")))?;
    let partial = repos.join(format!("{hex}.partial"));
    if partial.exists() {
        fs::remove_dir_all(&partial)
            .map_err(|err| Error::new(format!("failed to remove partial cache: {err}")))?;
    }
    fs::create_dir_all(&partial)
        .map_err(|err| Error::new(format!("failed to create cache repo: {err}")))?;
    let entries = parse_listing(listing)?;
    for entry in &entries {
        write_entry(&mirror, &partial, entry)?;
    }
    let dest = repos.join(&hex);
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|err| Error::new(format!("failed to replace cache repo: {err}")))?;
    }
    fs::rename(&partial, &dest)
        .map_err(|err| Error::new(format!("failed to publish cache repo: {err}")))?;
    write_bytes(&repos.join(format!("{hex}.ls-tree")), listing)?;
    Ok(())
}

struct ListingEntry {
    mode: String,
    kind: String,
    object: String,
    path: String,
}

fn parse_listing(bytes: &[u8]) -> Result<Vec<ListingEntry>> {
    let mut out = Vec::new();
    for entry in bytes.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry)
            .map_err(|_| Error::new("git ls-tree returned a path that is not UTF-8"))?;
        let Some((meta, path)) = entry.split_once('\t') else {
            return Err(Error::new(format!("git ls-tree returned `{entry}`")));
        };
        let mut parts = meta.split(' ');
        let mode = parts
            .next()
            .ok_or_else(|| Error::new("git ls-tree entry is missing a mode"))?;
        let kind = parts
            .next()
            .ok_or_else(|| Error::new("git ls-tree entry is missing a type"))?;
        let object = parts
            .next()
            .ok_or_else(|| Error::new("git ls-tree entry is missing an object"))?;
        if parts.next().is_some() {
            return Err(Error::new(format!("git ls-tree returned `{entry}`")));
        }
        if !is_commit_pin(object) {
            return Err(Error::new(format!(
                "git ls-tree object `{object}` is not a commit id"
            )));
        }
        validate_git_path(path)?;
        out.push(ListingEntry {
            mode: mode.to_string(),
            kind: kind.to_string(),
            object: object.to_string(),
            path: path.to_string(),
        });
    }
    Ok(out)
}

fn validate_git_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(Error::new(format!("refusing git path `{path}`")));
    }
    Ok(())
}

fn write_entry(mirror: &Path, dest: &Path, entry: &ListingEntry) -> Result<()> {
    if entry.mode == "160000" || entry.kind == "commit" {
        return Err(Error::new(format!(
            "hook repo contains submodule `{}`, which ahi does not lock",
            entry.path
        )));
    }
    let relative = PathBuf::from(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
    let path = dest.join(&relative);
    if !path.starts_with(dest) {
        return Err(Error::new(format!("refusing git path `{}`", entry.path)));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::new(format!("failed to create cache directory: {err}")))?;
    }
    let bytes = git(mirror, &["cat-file", "blob", &entry.object])?;
    let hashed = hash_object(&bytes)?;
    if hashed != entry.object {
        return Err(Error::new(format!(
            "blob `{}` for `{}` hashed to `{hashed}`",
            entry.object, entry.path
        )));
    }
    match entry.mode.as_str() {
        "120000" => {
            let target = std::str::from_utf8(&bytes)
                .map_err(|_| Error::new(format!("symlink `{}` is not UTF-8", entry.path)))?;
            write_symlink(&path, target)?;
        }
        "100644" | "100755" => {
            fs::write(&path, &bytes).map_err(|err| {
                Error::new(format!("failed to write cache file {}: {err}", entry.path))
            })?;
            set_executable(&path, entry.mode == "100755")?;
        }
        other => {
            return Err(Error::new(format!(
                "hook repo path `{}` has mode `{other}`, which ahi does not materialize",
                entry.path
            )));
        }
    }
    Ok(())
}

fn check_files(dest: &Path, entries: &[ListingEntry]) -> Result<()> {
    let mut found = Vec::new();
    collect_files(dest, "", &mut found)?;
    let mut expected: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    expected.sort_unstable();
    found.sort();
    if found != expected {
        return Err(Error::new("cached repo files do not match the locked tree"));
    }
    for entry in entries {
        let relative = PathBuf::from(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let path = dest.join(relative);
        let bytes = read_entry(&path, &entry.mode)?;
        let hashed = hash_object(&bytes)?;
        if hashed != entry.object {
            return Err(Error::new(format!(
                "cached file `{}` does not match the locked blob",
                entry.path
            )));
        }
    }
    Ok(())
}

fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(dir).map_err(|err| Error::new(format!("failed to read cache repo: {err}")))?
    {
        entries.push(entry.map_err(|err| Error::new(format!("failed to read cache repo: {err}")))?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::new("cache repo contains a path that is not UTF-8"));
        };
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry
            .file_type()
            .map_err(|err| Error::new(format!("failed to read cache repo: {err}")))?;
        if file_type.is_dir() {
            collect_files(&entry.path(), &rel, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

fn read_entry(path: &Path, mode: &str) -> Result<Vec<u8>> {
    if mode == "120000" {
        let target = fs::read_link(path)
            .map_err(|err| Error::new(format!("failed to read cached symlink: {err}")))?;
        let Some(text) = target.to_str() else {
            return Err(Error::new("cached symlink is not UTF-8"));
        };
        return Ok(text.as_bytes().to_vec());
    }
    fs::read(path).map_err(|err| Error::new(format!("failed to read cached file: {err}")))
}

#[cfg(unix)]
fn write_symlink(path: &Path, target: &str) -> Result<()> {
    std::os::unix::fs::symlink(target, path)
        .map_err(|err| Error::new(format!("failed to write cache symlink: {err}")))
}

#[cfg(not(unix))]
fn write_symlink(_path: &Path, _target: &str) -> Result<()> {
    Err(Error::new(
        "hook repo contains a symlink; materializing symlinks is not supported on this platform",
    ))
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !executable {
        return Ok(());
    }
    let mut permissions = fs::metadata(path)
        .map_err(|err| Error::new(format!("failed to read cache file mode: {err}")))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|err| Error::new(format!("failed to set cache file mode: {err}")))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<()> {
    Ok(())
}

fn hash_object(bytes: &[u8]) -> Result<String> {
    let output = Command::new("git")
        .args(["hash-object", "--no-filters", "--stdin"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))?;
    let mut child = output;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| Error::new("failed to write to git hash-object"))?;
        stdin
            .write_all(bytes)
            .map_err(|err| Error::new(format!("failed to write to git hash-object: {err}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| Error::new(format!("failed to run git hash-object: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::new(format!(
            "git hash-object failed: {}",
            stderr.trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if !is_commit_pin(text) {
        return Err(Error::new(format!("git hash-object returned `{text}`")));
    }
    Ok(text.to_string())
}

fn check_rows(repos: &[&Repo], lock: &Lockfile) -> Result<()> {
    let mut seen = BTreeMap::<&str, ()>::new();
    for row in &lock.repos {
        if seen.insert(row.repo.as_str(), ()).is_some() {
            return Err(Error::new(format!(
                "ahi.lock lists `{}` more than once",
                row.repo
            )));
        }
    }
    if lock.repos.len() != repos.len() {
        return Err(Error::new(
            "ahi.lock does not list the same remote repos as the config".to_string(),
        ));
    }
    for repo in repos {
        let rev = required_rev(repo)?;
        let row = lock
            .repos
            .iter()
            .find(|row| row.repo == repo.repo)
            .ok_or_else(|| Error::new(format!("ahi.lock is missing `{}`", repo.repo)))?;
        if row.requested != rev {
            return Err(Error::new(format!(
                "lock entry for `{}` has requested `{}` but config rev is `{rev}`",
                repo.repo, row.requested
            )));
        }
        if row.signature == Signature::Failed {
            return Err(Error::new(format!("signature for `{}` failed", row.repo)));
        }
    }
    Ok(())
}

fn remote_repos(config: &Config) -> Result<Vec<&Repo>> {
    let mut seen = BTreeMap::<&str, ()>::new();
    let mut out = Vec::new();
    for repo in &config.repos {
        if repo.kind != HookOriginKind::Remote {
            continue;
        }
        if seen.insert(repo.repo.as_str(), ()).is_some() {
            return Err(Error::new(format!(
                "repo `{}` is listed more than once",
                repo.repo
            )));
        }
        out.push(repo);
    }
    Ok(out)
}

fn required_rev(repo: &Repo) -> Result<&str> {
    repo.rev
        .as_deref()
        .filter(|rev| !rev.is_empty())
        .ok_or_else(|| Error::new(format!("repo `{}` is missing rev", repo.repo)))
}

fn refuse_dependencies(repos: &[&Repo]) -> Result<()> {
    for repo in repos {
        for hook in &repo.hooks {
            if !hook.additional_dependencies.is_empty() {
                return Err(Error::new(format!(
                    "hook `{}` in `{}` sets additional_dependencies, which v0.2 cannot pin to a version and sha256",
                    hook.id, repo.repo
                )));
            }
        }
    }
    Ok(())
}

fn ensure_allowed(policy: &Policy, repos: &[&Repo]) -> Result<()> {
    for repo in repos {
        if !policy
            .repo_allow
            .iter()
            .any(|allowed| allowed == &repo.repo)
        {
            return Err(Error::new(format!(
                "repo `{}` is not in repo_allow",
                repo.repo
            )));
        }
    }
    Ok(())
}

fn validate_rev_token(rev: &str) -> Result<()> {
    if rev.is_empty()
        || rev.starts_with('-')
        || rev.contains("..")
        || rev.contains([' ', '\n', '\0', ':', '\\', '?', '*', '[', '~', '^'])
    {
        return Err(Error::new(format!(
            "rev `{rev}` is not an immutable tag or a full commit"
        )));
    }
    Ok(())
}

fn is_commit_pin(rev: &str) -> bool {
    rev.len() == 40
        && rev
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn open_mirror(cache: &Path, repo: &str) -> Result<PathBuf> {
    store::Store::open(cache)?;
    fs::create_dir_all(cache.join("mirrors"))
        .map_err(|err| Error::new(format!("failed to create cache: {err}")))?;
    fs::create_dir_all(cache.join("repos"))
        .map_err(|err| Error::new(format!("failed to create cache: {err}")))?;
    let dir = mirror_dir(cache, repo);
    if !dir.join("config").is_file() {
        fs::create_dir_all(&dir)
            .map_err(|err| Error::new(format!("failed to create mirror: {err}")))?;
        let template = empty_dir("ahi-empty-template")?;
        git(
            &dir,
            &[
                "init",
                "--bare",
                "--quiet",
                &format!("--template={}", template.display()),
            ],
        )?;
    }
    Ok(dir)
}

fn mirror_dir(cache: &Path, repo: &str) -> PathBuf {
    cache.join("mirrors").join(sha256_hex(repo.as_bytes()))
}

fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    let text = git_text(dir, &["rev-parse", "--verify", rev])?;
    let text = text.trim();
    if !is_commit_pin(text) {
        return Err(Error::new(format!("git rev-parse returned `{text}`")));
    }
    Ok(text.to_string())
}

fn object_type(dir: &Path, object: &str) -> Result<String> {
    let text = git_text(dir, &["cat-file", "-t", object])?;
    Ok(text.trim().to_string())
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    let bytes = git(dir, args)?;
    String::from_utf8(bytes).map_err(|_| Error::new("git returned output that is not UTF-8"))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_raw(dir, args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::new(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(output.stdout)
}

fn git_raw(dir: &Path, args: &[&str]) -> Result<Output> {
    let hooks = empty_dir("ahi-empty-hooks")?;
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .arg("-c")
        .arg("protocol.file.allow=always")
        .arg("-c")
        .arg("safe.directory=*")
        .arg("-c")
        .arg("advice.detachedHead=false")
        .arg("-c")
        .arg("credential.helper=")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0");
    command
        .output()
        .map_err(|err| Error::new(format!("failed to run git: {err}")))
}

fn empty_dir(name: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(name);
    fs::create_dir_all(&dir)
        .map_err(|err| Error::new(format!("failed to create {name}: {err}")))?;
    Ok(dir)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)
            .map_err(|err| Error::new(format!("failed to create {}: {err}", tmp.display())))?;
        file.write_all(bytes)
            .map_err(|err| Error::new(format!("failed to write {}: {err}", tmp.display())))?;
        file.sync_all()
            .map_err(|err| Error::new(format!("failed to fsync {}: {err}", tmp.display())))?;
    }
    fs::rename(&tmp, path)
        .map_err(|err| Error::new(format!("failed to replace {}: {err}", path.display())))?;
    Ok(())
}

struct Version {
    numbers: Vec<u64>,
    pre: Option<Vec<PreId>>,
}

enum PreId {
    Num(u64),
    Text(String),
}

fn parse_version(tag: &str) -> Option<Version> {
    let rest = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    if rest.is_empty() || !rest.chars().next()?.is_ascii_digit() {
        return None;
    }
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => {
            if core.is_empty() || pre.is_empty() {
                return None;
            }
            (core, Some(pre))
        }
        None => (rest, None),
    };
    let mut numbers = Vec::new();
    for part in core.split('.') {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        numbers.push(part.parse::<u64>().ok()?);
    }
    let pre = match pre {
        None => None,
        Some(pre) => {
            let mut ids = Vec::new();
            for part in pre.split('.') {
                if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                    return None;
                }
                if part.chars().all(|ch| ch.is_ascii_digit()) {
                    ids.push(PreId::Num(part.parse().ok()?));
                } else {
                    ids.push(PreId::Text(part.to_string()));
                }
            }
            Some(ids)
        }
    };
    Some(Version { numbers, pre })
}

fn cmp_version(left: &Version, right: &Version) -> Ordering {
    let width = left.numbers.len().max(right.numbers.len());
    for index in 0..width {
        let left_part = left.numbers.get(index).copied().unwrap_or(0);
        let right_part = right.numbers.get(index).copied().unwrap_or(0);
        match left_part.cmp(&right_part) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    match (&left.pre, &right.pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => cmp_pre(left, right),
    }
}

fn cmp_pre(left: &[PreId], right: &[PreId]) -> Ordering {
    let width = left.len().max(right.len());
    for index in 0..width {
        match (left.get(index), right.get(index)) {
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(PreId::Num(left)), Some(PreId::Num(right))) => match left.cmp(right) {
                Ordering::Equal => {}
                other => return other,
            },
            (Some(PreId::Num(_)), Some(PreId::Text(_))) => return Ordering::Less,
            (Some(PreId::Text(_)), Some(PreId::Num(_))) => return Ordering::Greater,
            (Some(PreId::Text(left)), Some(PreId::Text(right))) => match left.cmp(right) {
                Ordering::Equal => {}
                other => return other,
            },
            (None, None) => {}
        }
    }
    Ordering::Equal
}

fn select_latest(tags: &[String]) -> Option<String> {
    let mut best: Option<String> = None;
    for tag in tags {
        let Some(version) = parse_version(tag) else {
            continue;
        };
        match &best {
            None => best = Some(tag.clone()),
            Some(current) => {
                let current_version = parse_version(current)?;
                match cmp_version(&version, &current_version) {
                    Ordering::Greater => best = Some(tag.clone()),
                    Ordering::Less => {}
                    Ordering::Equal => {
                        if tag_rank(tag) > tag_rank(current) {
                            best = Some(tag.clone());
                        }
                    }
                }
            }
        }
    }
    best
}

fn tag_rank(tag: &str) -> (bool, std::cmp::Reverse<&str>) {
    let prefixed = tag.starts_with('v') || tag.starts_with('V');
    (prefixed, std::cmp::Reverse(tag))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => cmp_version(&latest, &current) == Ordering::Greater,
        _ => false,
    }
}

/// Replace the `rev` scalar for one exact `repo` value. The tag stays a tag.
pub fn replace_repo_rev(text: &str, repo: &str, new_rev: &str) -> Result<String> {
    if new_rev.is_empty() || new_rev.chars().any(|ch| ch.is_control()) {
        return Err(Error::new(format!(
            "rev `{new_rev}` is not a single-line tag"
        )));
    }
    let (mut lines, separator, trailing_newline) = split_lines(text);
    let mut matches = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some((key, value)) = key_value(line) {
            if key == "repo" && value == repo {
                matches.push(index);
            }
        }
    }
    if matches.len() != 1 {
        return Err(Error::new(format!(
            "rev for `{repo}` is not a single repo entry ahi update can rewrite"
        )));
    }
    let start = matches[0];
    let mut rev_index = None;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if let Some((key, _)) = key_value(line) {
            if key == "repo" {
                break;
            }
            if key == "rev" {
                rev_index = Some(index);
                break;
            }
        }
    }
    let Some(rev_index) = rev_index else {
        return Err(Error::new(format!(
            "rev for `{repo}` is not a single line; ahi update will not rewrite it"
        )));
    };
    lines[rev_index] = rewrite_rev_line(&lines[rev_index], new_rev)?;
    let mut out = lines.join(separator);
    if trailing_newline {
        out.push_str(separator);
    }
    Ok(out)
}

fn split_lines(text: &str) -> (Vec<String>, &'static str, bool) {
    let separator: &'static str = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = text.replace("\r\n", "\n");
    let trailing_newline = normalized.ends_with('\n');
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    if trailing_newline && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    (lines, separator, trailing_newline)
}

fn key_value(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let body = match trimmed.strip_prefix('-') {
        Some(rest) => rest.trim_start(),
        None => trimmed,
    };
    let (key, raw_value) = body.split_once(':')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return None;
    }
    let value = parse_scalar(raw_value.trim())?;
    Some((key.to_string(), value))
}

fn parse_scalar(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.starts_with('|')
        || raw.starts_with('>')
        || raw.starts_with('[')
        || raw.starts_with('{')
    {
        return None;
    }
    if let Some(rest) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => out.push(chars.next()?),
                '"' => return Some(out),
                ch => out.push(ch),
            }
        }
        return None;
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                if chars.as_str().starts_with('\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    return Some(out);
                }
            } else {
                out.push(ch);
            }
        }
        return None;
    }
    let bytes = raw.as_bytes();
    let mut end = raw.len();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            end = index;
            break;
        }
        index += 1;
    }
    let value = raw[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn rewrite_rev_line(line: &str, new_rev: &str) -> Result<String> {
    let colon = line
        .find(':')
        .ok_or_else(|| Error::new("rev line has no value"))?;
    let prefix = &line[..colon + 1];
    let rest = &line[colon + 1..];
    let lead_len = rest.len() - rest.trim_start().len();
    let lead = &rest[..lead_len];
    let trimmed = rest.trim_start();
    if let Some(quoted) = trimmed.strip_prefix('"') {
        let mut escaped = false;
        for (index, ch) in quoted.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                let after = &quoted[index + 1..];
                return Ok(format!(
                    "{prefix}{lead}\"{}\"{after}",
                    escape_double(new_rev)
                ));
            }
        }
        return Err(Error::new(
            "rev for the selected repo is not a closed quoted scalar",
        ));
    }
    if let Some(quoted) = trimmed.strip_prefix('\'') {
        let mut out_end = None;
        let chars: Vec<char> = quoted.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '\'' {
                if index + 1 < chars.len() && chars[index + 1] == '\'' {
                    index += 2;
                    continue;
                }
                out_end = Some(quoted.chars().take(index).collect::<String>().len());
                break;
            }
            index += 1;
        }
        let Some(end) = out_end else {
            return Err(Error::new(
                "rev for the selected repo is not a closed quoted scalar",
            ));
        };
        let after = &quoted[end + 1..];
        return Ok(format!("{prefix}{lead}'{new_rev}'{after}"));
    }
    let token_len = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let after = &trimmed[token_len..];
    if is_plain_token(new_rev) {
        Ok(format!("{prefix}{lead}{new_rev}{after}"))
    } else {
        Ok(format!("{prefix}{lead}\"{}{after}", escape_double(new_rev)))
    }
}

fn is_plain_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '/' | '-'))
}

fn escape_double(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order_is_numeric_and_prerelease_is_older() {
        let order = [
            "v1.0.0-alpha",
            "v1.0.0-alpha.1",
            "v1.0.0-beta",
            "v1.0.0-rc.1",
            "v1.0.0",
            "v1.2.0",
            "v1.10.0",
        ];
        for pair in order.windows(2) {
            assert!(
                is_newer(pair[1], pair[0]),
                "{} should be newer than {}",
                pair[1],
                pair[0]
            );
        }
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v1.2", "v1.2.0"));
        assert!(parse_version("nightly").is_none());
        assert_eq!(
            select_latest(&[
                "nightly".to_string(),
                "1.2.0".to_string(),
                "v1.2.0".to_string(),
                "v1.0.0".to_string(),
            ])
            .as_deref(),
            Some("v1.2.0")
        );
    }

    #[test]
    fn rev_rewrite_touches_one_repo() {
        let text = "\
repos:
- repo: https://example.com/hooks
  rev: v1.0.0
  hooks:
  - id: one
- repo: https://example.com/hooks.git
  rev: \"v2.0.0\"
";
        let updated = replace_repo_rev(text, "https://example.com/hooks", "v1.1.0").unwrap();
        assert!(updated.contains("rev: v1.1.0"));
        assert!(updated.contains("rev: \"v2.0.0\""));
        assert!(!updated.contains("rev: v1.0.0"));
    }

    #[test]
    fn signature_classification() {
        assert!(!commit_has_signature(
            "tree abc\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nsubject\n"
        ));
        assert!(commit_has_signature(
            "tree abc\ngpgsig -----BEGIN PGP SIGNATURE-----\n \ncommitter a <a@b> 1 +0000\n\nsubject\n"
        ));
        assert_eq!(
            signature_from_verify(true, "Good signature"),
            Signature::Verified
        );
        assert_eq!(
            signature_from_verify(false, "error: no signature found"),
            Signature::Absent
        );
        assert_eq!(signature_from_verify(false, ""), Signature::Failed);
        assert_eq!(
            signature_from_verify(false, "gpg: Can't check signature: No public key"),
            Signature::Unverified
        );
        assert_eq!(
            signature_from_verify(false, "gpg: BAD signature from example"),
            Signature::Failed
        );
    }
}
