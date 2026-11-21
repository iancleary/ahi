//! Same-account Rex scratch delegation. A peer-checked local service admits
//! the task, freezes authority, and publishes the durable receipt, claim and
//! result. The worker in the block runs inference and confined tools. The
//! private scratch cwd is outside ahi state; nonces never enter argv, tool
//! roots or status. Missing results are unknown, never a reason to relaunch.

pub(crate) mod broker;
pub(crate) mod local;
pub(crate) mod resident;
mod worker;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::directories::{self, Directories};
use crate::terminal_host::{Client, Handle, Operation, Reply, Target};

pub(crate) use worker::run as run_worker;

/// The largest prompt accepted, matching `ahi exec`.
pub(crate) const MAX_PROMPT_BYTES: usize = 1024 * 1024;
/// Child text retained in the result document; the rest is dropped and flagged.
pub(crate) const MAX_RESULT_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 600;
pub(crate) const MAX_TIMEOUT_SECS: u64 = 6 * 3600;

const RECEIPT_FILE: &str = "assignment.json";
const PROMPT_FILE: &str = "prompt.txt";
const CLAIM_FILE: &str = "claim.json";
const RESULT_FILE: &str = "result.json";
const SCHEMA: u32 = 1;

/// The hidden entry point the Rex block runs. Not a public command.
pub(crate) const WORKER_SUBCOMMAND: &str = "internal-worker";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorKind {
    /// Bad operator input or a malformed stored record.
    Input,
    /// An attempt to reuse, relaunch, or doubly claim an assignment.
    Conflict,
    /// The terminal host did not start the worker, or its outcome is unknown.
    Launch,
    Io,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Error {
    pub(crate) kind: ErrorKind,
    pub(crate) message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    fn input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Input, message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }
    fn io(what: &str, path: &Path, err: std::io::Error) -> Self {
        Self::new(
            ErrorKind::Io,
            format!("cannot {what} {}: {err}", path.display()),
        )
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Where an assignment stands, as the operator side last recorded it. The
/// worker's progress (`claim.json`, `result.json`) is observed separately, so
/// a lost worker can never be mistaken for a finished one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Phase {
    /// Receipt, prompt, and scratch exist; no launch has been asked for.
    Reserved,
    /// Launch intent persisted; the Rex call may or may not have happened.
    Launching { attempt: String },
    /// Rex acknowledged exactly one block for this attempt.
    Launched { attempt: String, terminal: Handle },
    /// Rex may have created a block, but the reply did not establish it.
    /// Never relaunched automatically: the operator reconciles.
    LaunchUnknown { attempt: String, reason: String },
    /// Rex was never reached. A new assignment is required for another launch.
    LaunchFailed { attempt: String, reason: String },
}

impl Phase {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Launching { .. } => "launching",
            Self::Launched { .. } => "launched",
            Self::LaunchUnknown { .. } => "launch_unknown",
            Self::LaunchFailed { .. } => "launch_failed",
        }
    }

    pub(crate) fn attempt(&self) -> Option<&str> {
        match self {
            Self::Reserved => None,
            Self::Launching { attempt }
            | Self::Launched { attempt, .. }
            | Self::LaunchUnknown { attempt, .. }
            | Self::LaunchFailed { attempt, .. } => Some(attempt),
        }
    }

    /// The legal transitions. Anything else is a conflict, never a retry.
    pub(crate) fn can_become(&self, next: &Phase) -> bool {
        match (self, next) {
            (Self::Reserved, Self::Launching { .. }) => true,
            (
                Self::Launching { attempt },
                Self::Launched { attempt: next, .. }
                | Self::LaunchUnknown { attempt: next, .. }
                | Self::LaunchFailed { attempt: next, .. },
            ) => attempt == next,
            _ => false,
        }
    }

    /// Whether a worker presenting `attempt` may claim: a launch for that
    /// attempt was intended, and nothing since has withdrawn it. A claim may
    /// legitimately arrive before Rex's reply, so `Launching` qualifies, as
    /// does `LaunchUnknown` (the block may well exist).
    fn admits_claim(&self, attempt: &str) -> bool {
        match self {
            Self::Launching { attempt: own }
            | Self::Launched { attempt: own, .. }
            | Self::LaunchUnknown { attempt: own, .. } => own == attempt,
            Self::Reserved | Self::LaunchFailed { .. } => false,
        }
    }
}

/// The four ahi directories the worker needs to resolve the route and its
/// credential on the execution host. Absolute, since the worker's working
/// directory is the scratch directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Paths {
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) cache: PathBuf,
}

impl Paths {
    fn from_directories(dirs: &Directories) -> Result<Self> {
        let paths = Self {
            config: dirs.config.clone(),
            data: dirs.data.clone(),
            state: dirs.state.clone(),
            cache: dirs.cache.clone(),
        };
        for path in [&paths.config, &paths.data, &paths.state, &paths.cache] {
            if !path.is_absolute() {
                return Err(Error::input(format!(
                    "ahi directory {} is not absolute; the worker runs from another working directory",
                    path.display()
                )));
            }
        }
        Ok(paths)
    }

    pub(crate) fn to_directories(&self) -> Directories {
        Directories {
            config: self.config.clone(),
            data: self.data.clone(),
            state: self.state.clone(),
            cache: self.cache.clone(),
        }
    }
}

/// What the operator asked for, before anything durable exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Spec {
    pub(crate) gateway: String,
    pub(crate) model: String,
    /// A `ThinkingLevel` name; validated at reservation.
    pub(crate) thinking: String,
    pub(crate) prompt: String,
    pub(crate) timeout_secs: u64,
    pub(crate) target: Target,
}

/// The durable receipt. Never contains the prompt, a credential, or child text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub(crate) schema: u32,
    pub(crate) id: String,
    pub(crate) created: String,
    pub(crate) gateway: String,
    pub(crate) model: String,
    pub(crate) thinking: String,
    pub(crate) prompt_sha256: String,
    pub(crate) prompt_bytes: u64,
    pub(crate) timeout_secs: u64,
    /// Unix seconds after which a worker must not claim or call a provider.
    /// Set when the launch intent is persisted.
    pub(crate) expires_at: Option<i64>,
    pub(crate) directories: Paths,
    pub(crate) scratch: PathBuf,
    pub(crate) target: Target,
    pub(crate) phase: Phase,
}

/// What the worker observed about its own terminal. Diagnostic: the handle of
/// record is the one Rex returned to the operator side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Observed {
    pub(crate) rex_session: Option<String>,
    pub(crate) rex_block: Option<String>,
}

impl Observed {
    pub(crate) fn from_env() -> Self {
        let read = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        Self {
            rex_session: read("REX_SESSION"),
            rex_block: read("REX_BLOCK"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Claim {
    pub(crate) assignment: String,
    pub(crate) attempt: String,
    pub(crate) pid: u32,
    pub(crate) claimed: String,
    pub(crate) observed: Observed,
    /// The observed Rex session differs from the configured target.
    pub(crate) session_mismatch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Ok,
    Failed,
    Cancelled,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Usage {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cached_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Failure {
    pub(crate) kind: String,
    pub(crate) message: String,
}

/// The structured result, the only thing that settles an assignment.
/// `text` is untrusted model output, bounded by `MAX_RESULT_TEXT_BYTES`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Outcome {
    #[serde(default)]
    pub(crate) turns_completed: usize,
    pub(crate) assignment: String,
    pub(crate) attempt: String,
    pub(crate) status: Status,
    pub(crate) gateway: String,
    pub(crate) model: String,
    pub(crate) text: String,
    pub(crate) text_bytes: u64,
    pub(crate) truncated: bool,
    pub(crate) usage: Option<Usage>,
    pub(crate) error: Option<Failure>,
    pub(crate) finished: String,
}

fn root(dirs: &Directories) -> PathBuf {
    dirs.state.join("delegation")
}

pub(crate) fn valid_id(id: &str) -> bool {
    crate::session::valid_id(id)
}

/// The assignment directory for `id`, by name only; it need not exist.
pub(crate) fn assignment_dir(dirs: &Directories, id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        return Err(Error::input(
            "an assignment id is a lower-case hyphenated UUID",
        ));
    }
    Ok(root(dirs).join("assignments").join(id))
}

fn validate(spec: &Spec) -> Result<()> {
    if spec.gateway.is_empty() || spec.model.is_empty() {
        return Err(Error::input("the model must be given as gateway/model-id"));
    }
    if spec.gateway.contains('/') || spec.gateway.chars().any(char::is_control) {
        return Err(Error::input(
            "the gateway id must not contain `/` or controls",
        ));
    }
    if crate::chat::thinking::ThinkingLevel::parse(&spec.thinking).is_none() {
        return Err(Error::input(format!(
            "unknown thinking level `{}`",
            spec.thinking
        )));
    }
    if spec.prompt.trim().is_empty() {
        return Err(Error::input("the prompt is empty"));
    }
    if spec.prompt.len() > MAX_PROMPT_BYTES {
        return Err(Error::input(format!(
            "the prompt is larger than the {MAX_PROMPT_BYTES} byte cap"
        )));
    }
    if spec.timeout_secs == 0 || spec.timeout_secs > MAX_TIMEOUT_SECS {
        return Err(Error::input(format!(
            "the timeout must be 1 to {MAX_TIMEOUT_SECS} seconds"
        )));
    }
    if !matches!(spec.target, Target::Rex { .. } | Target::RexServer { .. }) {
        return Err(Error::input(
            "only a Rex execution target hosts a delegated worker in this version",
        ));
    }
    if let Target::Rex { server, .. } | Target::RexServer { server } = &spec.target {
        if !server
            .strip_prefix("unix://")
            .is_some_and(|s| s.starts_with('/'))
            && !server
                .parse::<std::net::SocketAddr>()
                .is_ok_and(|s| s.ip().is_loopback() && s.port() != 0)
        {
            return Err(Error::input(
                "use an explicit local unix:///path or loopback IP:port for the Rex server",
            ));
        }
    }
    // The client's own validation: full session:UUID, bounded server name.
    Client::new(spec.target.clone()).map_err(|err| Error::input(err.message))?;
    Ok(())
}

/// Create a directory that must not already exist, owner-only on Unix.
fn create_exclusive(path: &Path) -> Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::conflict(
            format!("{} already exists; a path is never adopted", path.display()),
        )),
        Err(err) => Err(Error::io("create", path, err)),
    }
}

/// Validate a spec and the directories it would be reserved under, creating
/// nothing. What `reserve` would reject, this rejects.
pub(crate) fn preview(dirs: &Directories, spec: &Spec) -> Result<()> {
    validate(spec)?;
    Paths::from_directories(dirs).map(|_| ())
}

/// Reserve a fresh assignment: receipt and private prompt in one tree, an
/// empty scratch directory in another. No provider request, no Rex call.
#[cfg(test)]
pub(crate) fn reserve(dirs: &Directories, spec: Spec) -> Result<Receipt> {
    reserve_with_id(dirs, spec, crate::session::uuid())
}

#[cfg(test)]
fn reserve_with_id(dirs: &Directories, spec: Spec, id: String) -> Result<Receipt> {
    preview(dirs, &spec)?;
    let scratch = root(dirs).join("scratch");
    directories::create_private_dir(&scratch).map_err(|e| Error::io("create", &scratch, e))?;
    reserve_at(dirs, spec, id.clone(), scratch.join(id))
}
fn reserve_at(dirs: &Directories, spec: Spec, id: String, scratch: PathBuf) -> Result<Receipt> {
    reserve_run(dirs, spec, id, scratch, true)
}
fn reserve_run(
    dirs: &Directories,
    spec: Spec,
    id: String,
    scratch: PathBuf,
    fresh: bool,
) -> Result<Receipt> {
    validate(&spec)?;
    let paths = Paths::from_directories(dirs)?;
    let dir = assignment_dir(dirs, &id)?;
    for parent in [root(dirs), root(dirs).join("assignments")] {
        directories::create_private_dir(&parent)
            .map_err(|err| Error::io("create", &parent, err))?;
    }
    create_exclusive(&dir)?;
    if fresh {
        create_exclusive(&scratch)?;
    }
    let prompt_path = dir.join(PROMPT_FILE);
    write_private_new(&prompt_path, spec.prompt.as_bytes())?;
    let receipt = Receipt {
        schema: SCHEMA,
        id,
        created: crate::util::rfc3339::now(),
        gateway: spec.gateway,
        model: spec.model,
        thinking: spec.thinking,
        prompt_sha256: crate::session::sha256_hex(spec.prompt.as_bytes()),
        prompt_bytes: spec.prompt.len() as u64,
        timeout_secs: spec.timeout_secs,
        expires_at: None,
        directories: paths,
        scratch,
        target: spec.target,
        phase: Phase::Reserved,
    };
    write_receipt(&dir, &receipt)?;
    Ok(receipt)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = directories::private_file()
        .create_new(true)
        .open(path)
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                Error::conflict(format!("{} already exists", path.display()))
            } else {
                Error::io("create", path, err)
            }
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| Error::io("write", path, err))
}

/// Replace the receipt: a private temporary beside it, then a rename, so a
/// worker reading it never sees a partial document.
fn write_receipt(dir: &Path, receipt: &Receipt) -> Result<()> {
    let path = dir.join(RECEIPT_FILE);
    let tmp = dir.join(format!(".{RECEIPT_FILE}.tmp-{}", std::process::id()));
    let text = serde_json::to_string_pretty(receipt)
        .map_err(|err| Error::new(ErrorKind::Io, format!("cannot encode the receipt: {err}")))?;
    let _ = fs::remove_file(&tmp);
    write_private_new(&tmp, text.as_bytes())?;
    fs::rename(&tmp, &path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        Error::io("replace", &path, err)
    })
}

pub(crate) fn load(dirs: &Directories, id: &str) -> Result<Receipt> {
    load_dir(&assignment_dir(dirs, id)?)
}

/// Read a receipt from its directory and check that it describes that
/// directory: an edited `--assignment-dir` cannot borrow another receipt.
pub(crate) fn load_dir(dir: &Path) -> Result<Receipt> {
    let path = dir.join(RECEIPT_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for (p, directory) in [(dir, true), (path.as_path(), false)] {
            let m = fs::symlink_metadata(p)
                .map_err(|_| Error::input(format!("no assignment at {}", dir.display())))?;
            if (if directory { !m.is_dir() } else { !m.is_file() })
                || m.uid() != ahi_unix::current_uid()
                || m.mode() & 0o077 != 0
            {
                return Err(Error::input(
                    "assignment must be private to the worker account",
                ));
            }
        }
    }
    let bytes = fs::read(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::input(format!("no assignment at {}", dir.display()))
        } else {
            Error::io("read", &path, err)
        }
    })?;
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .map_err(|err| Error::input(format!("{} is not a receipt: {err}", path.display())))?;
    if receipt.schema != SCHEMA {
        return Err(Error::input(format!(
            "{} has receipt schema {}, this version reads {SCHEMA}",
            path.display(),
            receipt.schema
        )));
    }
    let name = dir.file_name().and_then(|name| name.to_str());
    if !valid_id(&receipt.id) || name != Some(receipt.id.as_str()) {
        return Err(Error::input(format!(
            "{} names assignment {:?}, not its directory",
            path.display(),
            receipt.id
        )));
    }
    if assignment_dir(&receipt.directories.to_directories(), &receipt.id)?
        .canonicalize()
        .ok()
        != dir.canonicalize().ok()
    {
        return Err(Error::input(
            "assignment directory differs from its reserved location",
        ));
    }
    Ok(receipt)
}

fn advance(dir: &Path, receipt: &mut Receipt, next: Phase) -> Result<()> {
    if !receipt.phase.can_become(&next) {
        return Err(Error::conflict(format!(
            "assignment {} is {}; it cannot become {}",
            receipt.id,
            receipt.phase.name(),
            next.name()
        )));
    }
    receipt.phase = next;
    write_receipt(dir, receipt)
}

/// The exact argv the Rex block runs: the trusted executable, the hidden
/// worker entry point, and identifiers. No prompt, cwd, model, or secret.
pub(crate) fn worker_argv(executable: &Path, dir: &Path, attempt: &str) -> Vec<String> {
    vec![
        executable.display().to_string(),
        WORKER_SUBCOMMAND.to_string(),
        "--assignment-dir".to_string(),
        dir.display().to_string(),
        "--attempt".to_string(),
        attempt.to_string(),
    ]
}

/// Ask the configured Rex session to start the worker, once. The launch
/// intent is persisted before Rex is called; the reply, or its absence, is
/// persisted after. A second call on a launched or ambiguous assignment is
/// refused: reconciliation is the operator's, never a blind retry.
///
/// `host_executable` replaces the `rex` executable on PATH (contract fixtures).
pub(crate) fn launch(
    dirs: &Directories,
    receipt: &mut Receipt,
    worker_executable: &Path,
    host_executable: Option<&Path>,
) -> Result<()> {
    let dir = assignment_dir(dirs, &receipt.id)?;
    if !worker_executable.is_absolute() {
        return Err(Error::input(
            "the worker executable must be an absolute path on the execution host",
        ));
    }
    if result_exists(&dir) || claim_exists(&dir) {
        return Err(Error::conflict(format!(
            "assignment {} already has a worker; nothing is relaunched",
            receipt.id
        )));
    }
    let mut client =
        Client::new(receipt.target.clone()).map_err(|err| Error::input(err.message))?;
    if let Some(path) = host_executable {
        client = client.with_executable(path.to_path_buf());
    }
    let attempt = crate::session::run_id();
    let next = Phase::Launching {
        attempt: attempt.clone(),
    };
    if !receipt.phase.can_become(&next) {
        return Err(Error::conflict(format!(
            "assignment {} is {}; a launch was already issued and is not retried (reconcile with `ahi delegate status`)",
            receipt.id,
            receipt.phase.name()
        )));
    }
    if cancel_requested(&dir) {
        return Err(Error::conflict("assignment cancelled before launch"));
    }
    write_private_new(&dir.join("launch.intent"), attempt.as_bytes())?;
    receipt.expires_at = Some(crate::util::rfc3339::unix_now() + receipt.timeout_secs as i64);
    advance(&dir, receipt, next)?;
    let operation = Operation::Start {
        argv: worker_argv(worker_executable, &dir, &attempt),
        cwd: receipt.scratch.display().to_string(),
    };
    let outcome = client.execute(&operation);
    let (phase, error) = match outcome {
        Ok(Reply::Started { terminal }) => (
            Phase::Launched {
                attempt: attempt.clone(),
                terminal,
            },
            None,
        ),
        Ok(_) => {
            let reason = "the terminal host answered with an unexpected reply".to_string();
            (
                Phase::LaunchUnknown {
                    attempt: attempt.clone(),
                    reason: reason.clone(),
                },
                Some(reason),
            )
        }
        Err(err) if err.effects_uncertain => (
            Phase::LaunchUnknown {
                attempt: attempt.clone(),
                reason: err.message.clone(),
            },
            Some(format!(
                "{}; a block may exist: inspect the Rex session before anything else",
                err.message
            )),
        ),
        Err(err) => (
            Phase::LaunchFailed {
                attempt: attempt.clone(),
                reason: err.message.clone(),
            },
            Some(err.message),
        ),
    };
    advance(&dir, receipt, phase)?;
    match error {
        None => Ok(()),
        Some(message) => Err(Error::new(ErrorKind::Launch, message)),
    }
}

/// Persist the operator's intent to stop. The worker checks the receipt
/// before claiming, before calling the provider, and while streaming; the
/// terminal is not touched here.
pub(crate) fn cancel(dirs: &Directories, receipt: &mut Receipt) -> Result<()> {
    let dir = assignment_dir(dirs, &receipt.id)?;
    if result_exists(&dir) {
        return Err(Error::conflict(format!(
            "assignment {} already finished; its result stands",
            receipt.id
        )));
    }
    // Independent writer: a late Start reply must never roll this back.
    match write_private_new(&dir.join("cancel.request"), b"") {
        Err(err) if err.kind == ErrorKind::Conflict => Ok(()), // already requested
        result => result,
    }
}

fn claim_exists(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(CLAIM_FILE)).is_ok()
}

fn result_exists(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(RESULT_FILE)).is_ok()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| Error::input(format!("{} is malformed: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io("read", path, err)),
    }
}

pub(crate) fn read_claim(dir: &Path) -> Result<Option<Claim>> {
    read_json(&dir.join(CLAIM_FILE))
}

pub(crate) fn read_outcome(dir: &Path) -> Result<Option<Outcome>> {
    read_json(&dir.join(RESULT_FILE))
}

/// Everything known about an assignment, for the operator: the receipt, the
/// worker's claim and result if present, and one derived `state`. Nothing is
/// inferred from the terminal; a missing result is `unsettled`, not failure.
pub(crate) fn status(dirs: &Directories, id: &str) -> Result<Value> {
    let dir = assignment_dir(dirs, id)?;
    let receipt = load_dir(&dir)?;
    let claim = read_claim(&dir)?;
    let outcome = read_outcome(&dir)?;
    let state = match (&outcome, &claim) {
        (Some(_), _) => "finished",
        (None, Some(_)) if cancel_requested(&dir) => "cancelling",
        (None, None) if cancel_requested(&dir) => "cancelled",
        (None, Some(_)) => "claimed",
        (None, None)
            if matches!(receipt.phase, Phase::Reserved) && dir.join("launch.intent").exists() =>
        {
            "launch_unknown"
        }
        (None, None) => receipt.phase.name(),
    };
    Ok(json!({
        "child":resident::child_id(&dir,&receipt)?,
        "lifecycle":resident::lifecycle(dirs,&resident::child_id(&dir,&receipt)?)?,
        "transition":read_json::<resident::Transition>(&dir.join("transition.json"))?,
        "assignment": receipt,
        "authority":read_json::<Value>(&dir.join("authority.json"))?,
        "claim": claim,
        "result": outcome,
        "state": state,
        "settled": outcome.is_some(),
    }))
}

// ---- worker-side primitives: claim once, finish once ----

/// One atomic claim by the worker. Checks the receipt admits this attempt,
/// the attempt has not expired, and nothing has finished; then creates
/// `claim.json` exclusively. A second claimant fails with `Conflict` and must
/// not call a provider.
#[cfg(test)]
pub(crate) fn claim(dir: &Path, attempt: &str, observed: &Observed) -> Result<(Receipt, Claim)> {
    claim_pid(dir, attempt, observed, std::process::id())
}
fn claim_pid(dir: &Path, attempt: &str, observed: &Observed, pid: u32) -> Result<(Receipt, Claim)> {
    let receipt = load_dir(dir)?;
    if !crate::session::valid_run_id(attempt) {
        return Err(Error::input("the attempt id is malformed"));
    }
    if result_exists(dir) {
        return Err(Error::conflict(format!(
            "assignment {} already has a result",
            receipt.id
        )));
    }
    // A verified resident may claim cancelled work solely to publish the
    // cancellation and return to idle; the worker checks before inference.
    if cancel_requested(dir)
        && !matches!(
            (&receipt.target, &receipt.phase),
            (Target::RexServer { .. }, Phase::Launched { .. })
        )
    {
        return Err(Error::conflict("assignment cancelled before claim"));
    }
    if !receipt.phase.admits_claim(attempt) {
        return Err(Error::conflict(format!(
            "assignment {} is {} (attempt {}); attempt {attempt} may not claim it",
            receipt.id,
            receipt.phase.name(),
            receipt.phase.attempt().unwrap_or("none")
        )));
    }
    let now = crate::util::rfc3339::unix_now();
    match receipt.expires_at {
        Some(expires) if now < expires => {}
        _ => {
            return Err(Error::conflict(format!(
                "assignment {} attempt {attempt} has expired",
                receipt.id
            )))
        }
    }
    let session_mismatch = match (&receipt.target, &observed.rex_session) {
        (Target::Rex { session, .. }, Some(seen)) => seen != session,
        _ => false,
    };
    let claim = Claim {
        assignment: receipt.id.clone(),
        attempt: attempt.to_string(),
        pid,
        claimed: crate::util::rfc3339::now(),
        observed: observed.clone(),
        session_mismatch,
    };
    let text = serde_json::to_string_pretty(&claim)
        .map_err(|err| Error::new(ErrorKind::Io, format!("cannot encode the claim: {err}")))?;
    publish_once(dir, CLAIM_FILE, text.as_bytes()).map_err(|err| {
        if err.kind == ErrorKind::Conflict {
            Error::conflict(format!(
                "assignment {} is already claimed; this worker must not run",
                receipt.id
            ))
        } else {
            err
        }
    })?;
    Ok((receipt, claim))
}

/// Whether the operator has since recorded a cancel.
pub(crate) fn cancel_requested(dir: &Path) -> bool {
    matches!(
        read_json::<resident::Transition>(&dir.join("transition.json")),
        Ok(Some(resident::Transition::Close))
    ) || !matches!(fs::symlink_metadata(dir.join("cancel.request")), Err(ref e) if e.kind()==std::io::ErrorKind::NotFound)
        || load_dir(dir).is_err()
}

/// The seconds a claimed worker still has, from the receipt's expiry.
pub(crate) fn remaining_secs(receipt: &Receipt) -> i64 {
    receipt
        .expires_at
        .map(|expires| expires - crate::util::rfc3339::unix_now())
        .unwrap_or(0)
}

pub(crate) fn read_prompt(dir: &Path, receipt: &Receipt) -> Result<String> {
    let path = dir.join(PROMPT_FILE);
    let bytes = fs::read(&path).map_err(|err| Error::io("read", &path, err))?;
    if bytes.len() > MAX_PROMPT_BYTES || crate::session::sha256_hex(&bytes) != receipt.prompt_sha256
    {
        return Err(Error::input(format!(
            "{} does not match the receipt's prompt digest",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|_| Error::input("the stored prompt is not UTF-8"))
}

/// Publish the result exactly once: write a private temporary, then link it
/// to `result.json`, which fails if a result already exists. No partial
/// document is ever visible and no second result can replace the first.
pub(crate) fn finish(dir: &Path, outcome: &Outcome) -> Result<()> {
    let text = serde_json::to_string_pretty(outcome)
        .map_err(|err| Error::new(ErrorKind::Io, format!("cannot encode the result: {err}")))?;
    publish_once(dir, RESULT_FILE, text.as_bytes())
}

/// Make a complete record visible to status readers and pick exactly one winner.
fn publish_once(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp-{}", crate::session::uuid()));
    write_private_new(&tmp, bytes)?;
    let linked = fs::hard_link(&tmp, &path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::conflict(
            format!("{} already exists; the first result stands", path.display()),
        )),
        Err(err) => Err(Error::io("publish", &path, err)),
    }
}

/// Append `delta` to `text` within `limit` bytes, cutting on a character
/// boundary; returns whether anything was dropped.
pub(crate) fn push_bounded(text: &mut String, delta: &str, limit: usize) -> bool {
    let room = limit.saturating_sub(text.len());
    if delta.len() <= room {
        text.push_str(delta);
        return false;
    }
    let mut cut = room;
    while cut > 0 && !delta.is_char_boundary(cut) {
        cut -= 1;
    }
    text.push_str(&delta[..cut]);
    true
}
