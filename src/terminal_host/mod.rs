//! Interface to external terminal multiplexers, not ahi's TUI or session protocol.
//!
//! Rex is the default choice at the operator boundary; hc is an explicit alternative.
//! A client targets one server/core for its entire lifetime. Calls are never retried or
//! failed over. A terminal handle is not an ahi conversation/session identifier.

mod hc;
mod rex;
mod transport;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// The execution namespace. Native Windows and WSL must use separate namespaces.
/// Local hc namespaces are meaningful only on the machine where the client runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    /// New, dedicated session on this server; not an existing terminal namespace.
    RexServer {
        server: String,
    },
    Rex {
        server: String,
        session: String,
    },
    Hc {
        session: String,
        machine: Option<String>,
    },
}

/// Backend- and target-qualified native identity; never a label or focused terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handle {
    pub target: Target,
    pub id: String,
}

#[derive(Debug, Clone)]
pub enum Operation {
    /// Exact argv, with an absolute executable path to avoid hc template expansion.
    /// The server retains the terminal after exit; disconnecting ahi does not stop it.
    Start {
        argv: Vec<String>,
        cwd: String,
    },
    Inspect {
        terminal: Handle,
    },
    Capture {
        terminal: Handle,
    },
    /// Raw text without Enter. Not supported by hc's public send API.
    Text {
        terminal: Handle,
        text: String,
    },
    /// Submit text. Rex performs write, then Enter; this is not an atomic transaction.
    Submit {
        terminal: Handle,
        text: String,
    },
    Key {
        terminal: Handle,
        key: Key,
    },
    Interrupt {
        terminal: Handle,
    },
    /// Rex closes one block; hc ends its program but retains the terminal record.
    Terminate {
        terminal: Handle,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Enter,
    Escape,
    ControlC,
}
impl std::str::FromStr for Key {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "enter" => Ok(Self::Enter),
            "escape" => Ok(Self::Escape),
            "control-c" => Ok(Self::ControlC),
            _ => Err(Error::input("key must be enter, escape, or control-c")),
        }
    }
}
impl Key {
    fn native(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Escape => "esc",
            Self::ControlC => "ctrl+c",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Pending,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// Loss of contact or an unfamiliar native state is not evidence of exit.
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Started {
        terminal: Handle,
    },
    Inspected {
        terminal: Handle,
        process: ProcessState,
    },
    /// A bounded snapshot, not a byte stream, delta, or structured agent response.
    Captured {
        terminal: Handle,
        text: String,
        extent: String,
        complete: Option<bool>,
    },
    InputSent {
        terminal: Handle,
    },
    Interrupted {
        terminal: Handle,
        key: String,
        observation: Option<String>,
    },
    StopRequested {
        terminal: Handle,
        record_retained: bool,
        process: Option<ProcessState>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Input,
    Unsupported,
    Unavailable,
    Timeout,
    Limit,
    Protocol,
    Backend,
}

/// `effects_uncertain` forbids interpreting a failed mutation as permission to retry.
#[derive(Debug, Serialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub effects_uncertain: bool,
    pub message: String,
}
impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            effects_uncertain: false,
            message: message.into(),
        }
    }
    fn input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Input, message)
    }
    fn protocol() -> Self {
        Self::new(ErrorKind::Protocol, "unsupported terminal-host response")
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if self.effects_uncertain {
            write!(
                f,
                "; effects are uncertain: inspect the original target before retrying"
            )?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// A concrete, fixed backend. Construction and preview do not contact a service.
#[derive(Debug)]
pub struct Client {
    target: Target,
    executable: PathBuf,
    timeout: Duration,
}
impl Client {
    pub fn new(target: Target) -> Result<Self> {
        target.validate()?;
        let executable = match target {
            Target::Rex { .. } | Target::RexServer { .. } => "rex",
            Target::Hc { .. } => "hc",
        }
        .into();
        Ok(Self {
            target,
            executable,
            timeout: Duration::from_secs(15),
        })
    }

    /// Explicit executable selection, also used by disposable contract fixtures.
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    /// Native calls remain bounded, including when a backend stops reading stdin.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(120) {
            return Err(Error::input(
                "terminal-host timeout must be greater than zero and at most 120 seconds",
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Validate and describe the actual native requests without launching anything.
    pub fn preview(&self, operation: &Operation) -> Result<Value> {
        let calls = self.prepare(operation)?;
        Ok(
            serde_json::json!({"target": self.target, "executable": self.executable, "calls": calls}),
        )
    }

    pub fn execute(&self, operation: &Operation) -> Result<Reply> {
        let calls = self.prepare(operation)?;
        let deadline = std::time::Instant::now() + self.timeout;
        let mut last = Value::Null;
        let mut previous_effect = false;
        for call in calls {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let result = transport::call(&self.executable, &call, remaining);
            match result {
                Ok(reply) => {
                    last = reply;
                    previous_effect |= operation.mutates();
                }
                Err(mut error) => {
                    // A spawn failure is known not to have sent this call. Earlier calls
                    // (Rex submit's write) may already have taken effect, however.
                    error.effects_uncertain = previous_effect
                        || (operation.mutates() && error.kind != ErrorKind::Unavailable);
                    return Err(error);
                }
            }
        }
        let decoded = match &self.target {
            Target::RexServer { server } => rex::created_session(server, last),
            Target::Rex { .. } => rex::decode(&self.target, operation, last),
            Target::Hc { .. } => hc::decode(&self.target, operation, last),
        };
        decoded.map_err(|mut error| {
            error.effects_uncertain = operation.mutates();
            error
        })
    }

    fn prepare(&self, operation: &Operation) -> Result<Vec<Call>> {
        operation.validate(&self.target)?;
        match &self.target {
            Target::RexServer { server } => rex::new_session(server, operation, self.timeout),
            Target::Rex { server, session } => {
                rex::prepare(server, session, operation, self.timeout)
            }
            Target::Hc { session, machine } => hc::prepare(session, machine.as_deref(), operation),
        }
    }
}

#[derive(Debug, Serialize)]
struct Call {
    argv: Vec<String>,
    stdin: Option<Input>,
    response: Response,
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Input {
    Json(Value),
    Text(String),
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Response {
    Json,
    Text,
    HcRpc,
}

impl Target {
    fn validate(&self) -> Result<()> {
        match self {
            Self::RexServer { server } => name(server)?,
            Self::Rex { server, session } => {
                name(server)?;
                if !rex_id(session, "session:") {
                    return Err(Error::input(
                        "Rex requires a full session:UUID, not a label or ahi session ID",
                    ));
                }
            }
            Self::Hc { session, machine } => {
                name(session)?;
                if let Some(machine) = machine {
                    name(machine)?;
                    if machine.contains('/') {
                        return Err(Error::input("hc machine must not contain /"));
                    }
                }
            }
        }
        Ok(())
    }
}
impl Handle {
    fn validate(&self) -> Result<()> {
        self.target.validate()?;
        let valid = match &self.target {
            Target::RexServer { .. } => false,
            Target::Rex { .. } => rex_id(&self.id, "block:"),
            Target::Hc { machine, .. } => {
                let local = match machine {
                    Some(machine) => self.id.strip_prefix(&format!("{machine}/")),
                    None => Some(self.id.as_str()),
                };
                local.and_then(|id| id.strip_prefix('t')).is_some_and(|id| {
                    !id.is_empty()
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
                })
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::input(
                "terminal ID must be a full native ID in the selected target namespace",
            ))
        }
    }
}
impl Operation {
    fn terminal(&self) -> Option<&Handle> {
        match self {
            Self::Start { .. } => None,
            Self::Inspect { terminal }
            | Self::Capture { terminal }
            | Self::Text { terminal, .. }
            | Self::Submit { terminal, .. }
            | Self::Key { terminal, .. }
            | Self::Interrupt { terminal }
            | Self::Terminate { terminal } => Some(terminal),
        }
    }
    fn mutates(&self) -> bool {
        !matches!(self, Self::Inspect { .. } | Self::Capture { .. })
    }
    fn validate(&self, target: &Target) -> Result<()> {
        if let Some(terminal) = self.terminal() {
            terminal.validate()?;
            if &terminal.target != target {
                return Err(Error::input(
                    "terminal belongs to a different backend or execution target",
                ));
            }
        }
        match self {
            Self::Start { argv, cwd } => {
                if argv.is_empty() || !absolute(&argv[0]) || !absolute(cwd) {
                    return Err(Error::input("start requires an absolute executable path and absolute cwd in the target environment"));
                }
                if argv.len() > 256
                    || argv.iter().map(String::len).sum::<usize>() + cwd.len() > 16 * 1024
                    || argv.iter().any(|s| s.contains('\0'))
                    || cwd.contains('\0')
                {
                    return Err(Error::input("start arguments exceed limits or contain NUL"));
                }
            }
            Self::Text { text, .. } | Self::Submit { text, .. }
                if text.len() > 16 * 1024 || text.contains('\0') =>
            {
                return Err(Error::input("input exceeds 16 KiB or contains NUL"));
            }
            _ => {}
        }
        Ok(())
    }
}
/// Close a positively identified, dedicated Rex session. No focus or label resolution.
pub(crate) fn close_rex_session(handle: &Handle) -> Result<()> {
    handle.validate()?;
    let Target::Rex { server, session } = &handle.target else {
        return Err(Error::input("not a Rex session"));
    };
    transport::call(
        std::path::Path::new("rex"),
        &Call {
            argv: vec![
                "--autostart=false".into(),
                "--server".into(),
                server.clone(),
                "--timeout".into(),
                "10s".into(),
                "kill".into(),
                session.clone(),
            ],
            stdin: None,
            response: Response::Text,
        },
        Duration::from_secs(12),
    )
    .map(|_| ())
}
fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        Err(Error::input("terminal-host target names must be nonempty, bounded, and contain no controls or leading dash"))
    } else {
        Ok(())
    }
}
fn absolute(value: &str) -> bool {
    let b = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
}
fn rex_id(value: &str, prefix: &str) -> bool {
    let Some(id) = value.strip_prefix(prefix) else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && id.bytes().any(|b| b != b'0' && b != b'-')
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Error::protocol())
}

#[cfg(test)]
mod tests;
