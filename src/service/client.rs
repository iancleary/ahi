//! A client of the service with one calling shape and three transports:
//! the service built in this process, the socket of a running `ahi serve`,
//! or the stdio of a command that carries the lines somewhere else
//! (`ssh host ahi connect`, `hc connect --worker label`). `ahi exec`,
//! `ahi tui`, and the `delegate` tool all speak through it.

// Without a Unix socket only the in-process transport exists on this platform.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::wire::{self, Request};
use super::{Event, Service, ServiceError};
use crate::directories::Directories;

pub(crate) struct Client {
    transport: Transport,
    /// The subscription's events. Shared and behind its own lock so a
    /// reader can wait on them while another thread calls.
    events: Events,
}

/// The subscription's receiver, once there is one.
pub(crate) type Events = Arc<Mutex<Option<Receiver<Event>>>>;

enum Transport {
    /// Constructed in this process: no server on the host.
    Local(Service),
    /// Over the socket of a running `ahi serve`.
    #[cfg(unix)]
    Remote(remote::Remote, PathBuf),
    /// Over the stdio of a command, herdr-core's `--via`.
    Via(remote::Remote, Bridge),
}

/// The connect command behind a `Via` transport and what it said on stderr.
struct Bridge {
    child: std::process::Child,
    /// The last `VIA_STDERR_CAP` bytes the command wrote to stderr.
    stderr: Arc<Mutex<Vec<u8>>>,
}

/// Environment the connect command gets besides the hook runner's base:
/// the agent an `ssh` in it authenticates with. Nothing else of the
/// coordinator's travels.
pub(crate) const VIA_ENV: &[&str] = &["SSH_AUTH_SOCK", "SSH_AGENT_PID"];
/// How much of the connect command's stderr is kept for the operator.
const VIA_STDERR_CAP: usize = 8 * 1024;

impl Client {
    /// The server on `socket` (or the default socket) when one answers,
    /// else the service in this process. `standalone` skips the socket;
    /// an explicit `socket` that nothing answers on is an error.
    pub(crate) fn open(
        state: &Directories,
        socket: Option<&Path>,
        standalone: bool,
    ) -> Result<Self, ServiceError> {
        #[cfg(unix)]
        {
            if !standalone {
                let path = crate::serve::socket_path(state, socket);
                if let Some(stream) = crate::serve::try_connect(&path) {
                    let reader = stream.try_clone().map_err(|err| ServiceError {
                        kind: "session",
                        message: format!("socket: {err}"),
                    })?;
                    let (remote, events) = remote::Remote::new(Box::new(reader), Box::new(stream));
                    return Ok(Self {
                        transport: Transport::Remote(remote, path),
                        events: Arc::new(Mutex::new(Some(events))),
                    });
                }
                if socket.is_some() {
                    return Err(usage(format!("no server answers on {}", path.display())));
                }
            } else if socket.is_some() {
                return Err(usage("--standalone and --socket contradict each other"));
            }
        }
        #[cfg(not(unix))]
        let _ = (socket, standalone);
        Ok(Self {
            transport: Transport::Local(Service::open(state)),
            events: Arc::new(Mutex::new(None)),
        })
    }

    /// A handle to the subscription's events, for a reader on another
    /// thread that must not hold the client while it waits.
    pub(crate) fn events(&self) -> Events {
        Arc::clone(&self.events)
    }

    /// Over the stdin and stdout of `argv`: a bridge such as
    /// `ssh host ahi connect`. The command runs with the hook runner's
    /// scrubbed environment plus `PATH` and the ssh agent variables, so a
    /// coordinator's secrets do not travel with it. Its stderr is captured
    /// for the operator (`via_stderr`), never written to the terminal,
    /// which may be a raw-mode screen, and never shown to the model.
    pub(crate) fn via(argv: &[String]) -> Result<Self, ServiceError> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| usage("empty connect command"))?;
        let extra: Vec<String> = VIA_ENV.iter().map(|name| name.to_string()).collect();
        let env = crate::hooks::local_hook::scrub_env(std::env::vars_os(), &extra);
        let mut child = std::process::Command::new(program)
            .args(args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| usage(format!("cannot start `{program}`: {err}")))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(usage("the connect command has no stdio"));
        };
        let kept = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&kept);
        std::thread::spawn(move || {
            use std::io::Read;
            let mut stderr = stderr;
            let mut chunk = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if let Ok(mut kept) = sink.lock() {
                    kept.extend_from_slice(&chunk[..n]);
                    if kept.len() > VIA_STDERR_CAP {
                        let excess = kept.len() - VIA_STDERR_CAP;
                        kept.drain(..excess);
                    }
                }
            }
        });
        let (remote, events) = remote::Remote::new(Box::new(stdout), Box::new(stdin));
        Ok(Self {
            transport: Transport::Via(
                remote,
                Bridge {
                    child,
                    stderr: kept,
                },
            ),
            events: Arc::new(Mutex::new(Some(events))),
        })
    }

    /// What the connect command has written to stderr so far, for the
    /// operator's eyes: `ssh` and `hc` diagnostics. Empty for the other
    /// transports.
    pub(crate) fn via_stderr(&self) -> String {
        match &self.transport {
            Transport::Via(_, bridge) => bridge
                .stderr
                .lock()
                .map(|kept| String::from_utf8_lossy(&kept).trim().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// The socket in use, or `None` for the in-process service and a bridge.
    pub(crate) fn socket(&self) -> Option<&Path> {
        match &self.transport {
            Transport::Local(_) => None,
            #[cfg(unix)]
            Transport::Remote(_, path) => Some(path),
            Transport::Via(..) => None,
        }
    }

    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, ServiceError> {
        let request = Request {
            id: json!(1),
            method: method.to_string(),
            params,
        };
        match &mut self.transport {
            Transport::Local(service) => {
                let events = &self.events;
                let mut subscribe = |receiver: Receiver<Event>| {
                    if let Ok(mut slot) = events.lock() {
                        *slot = Some(receiver);
                    }
                };
                wire::dispatch(service, &request, &mut subscribe)
            }
            #[cfg(unix)]
            Transport::Remote(remote, _) => remote.call(&request),
            Transport::Via(remote, _) => remote.call(&request),
        }
    }

    /// The next event of the subscription, if one arrives within `timeout`.
    pub(crate) fn next_event(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        next_event(&self.events, timeout)
    }
}

/// The next event on `events`, if one arrives within `timeout`.
pub(crate) fn next_event(events: &Events, timeout: Duration) -> Result<Event, RecvTimeoutError> {
    let events = events.lock().map_err(|_| RecvTimeoutError::Disconnected)?;
    match events.as_ref() {
        Some(receiver) => receiver.recv_timeout(timeout),
        None => Err(RecvTimeoutError::Disconnected),
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The bridge ends with its stdin; a command that lingers is killed.
        if let Transport::Via(remote, bridge) = &mut self.transport {
            remote.close();
            let _ = bridge.child.kill();
            let _ = bridge.child.wait();
        }
    }
}

fn usage(message: impl Into<String>) -> ServiceError {
    ServiceError {
        kind: "usage",
        message: message.into(),
    }
}

mod remote {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    use serde_json::Value;

    use crate::service::wire::{Request, MAX_ERROR_LEN, MAX_LINE};
    use crate::service::{Event, ServiceError};

    /// One byte stream each way, one request at a time, events on the side.
    pub(super) struct Remote {
        writer: Option<Box<dyn Write + Send>>,
        responses: Receiver<Value>,
    }

    impl Remote {
        pub(super) fn new(
            reader: Box<dyn Read + Send>,
            writer: Box<dyn Write + Send>,
        ) -> (Self, Receiver<Event>) {
            let (answers, responses) = mpsc::channel();
            let (notify, events) = mpsc::channel();
            thread::spawn(move || {
                // The peer may be another host's worker; a line without end
                // must not become memory here. Past the cap the connection is
                // dropped, which every waiter sees as closed.
                let mut reader = BufReader::new(reader);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match (&mut reader)
                        .take(MAX_LINE as u64 + 1)
                        .read_until(b'\n', &mut line)
                    {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if line.len() > MAX_LINE {
                        break;
                    }
                    let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                        continue;
                    };
                    if value.get("event").is_some() {
                        if let Some(event) = Event::from_json(&value) {
                            if notify.send(event).is_err() {
                                break;
                            }
                        }
                    } else if answers.send(value).is_err() {
                        break;
                    }
                }
            });
            (
                Self {
                    writer: Some(writer),
                    responses,
                },
                events,
            )
        }

        /// Drop the write side, which ends a bridge's stdin.
        pub(super) fn close(&mut self) {
            self.writer = None;
        }

        pub(super) fn call(&mut self, request: &Request) -> Result<Value, ServiceError> {
            let closed = || ServiceError {
                kind: "session",
                message: "the server closed the connection".to_string(),
            };
            let mut line = request.to_json().to_string();
            line.push('\n');
            let writer = self.writer.as_mut().ok_or_else(closed)?;
            writer
                .write_all(line.as_bytes())
                .and_then(|()| writer.flush())
                .map_err(|_| closed())?;
            let answer = self
                .responses
                .recv_timeout(Duration::from_secs(30))
                .map_err(|_| closed())?;
            if let Some(error) = answer.get("error") {
                return Err(ServiceError {
                    kind: match error["kind"].as_str() {
                        Some("usage") => "usage",
                        Some("config") => "config",
                        Some("auth") => "auth",
                        Some("policy") => "policy",
                        Some("conflict") => "conflict",
                        Some("not_found") => "not_found",
                        Some("limit") => "limit",
                        _ => "session",
                    },
                    message: capped(error["message"].as_str().unwrap_or("refused")),
                });
            }
            Ok(answer.get("result").cloned().unwrap_or(Value::Null))
        }
    }

    /// `text` cut to `MAX_ERROR_LEN` on a character boundary.
    pub(super) fn capped(text: &str) -> String {
        let mut cut = text.len().min(MAX_ERROR_LEN);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut out = text[..cut].to_string();
        if cut < text.len() {
            out.push_str(" [truncated]");
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Cursor;

        #[test]
        fn a_line_past_the_cap_ends_the_connection_and_a_refusal_is_capped() {
            let mut feed = Vec::new();
            feed.extend_from_slice(b"{\"id\":1,\"result\":{\"ok\":true}}\n");
            feed.extend_from_slice(
                format!(
                    "{{\"id\":2,\"error\":{{\"kind\":\"usage\",\"message\":\"{}\"}}}}\n",
                    "m".repeat(MAX_ERROR_LEN + 10)
                )
                .as_bytes(),
            );
            feed.resize(feed.len() + MAX_LINE + 1, b'x');
            feed.extend_from_slice(b"\n{\"id\":3,\"result\":{}}\n");
            let (mut remote, events) =
                Remote::new(Box::new(Cursor::new(feed)), Box::new(Vec::new()));
            let request = |id: u64| Request {
                id: serde_json::json!(id),
                method: "health".into(),
                params: Value::Null,
            };
            assert_eq!(remote.call(&request(1)).unwrap()["ok"], true);
            let refused = remote.call(&request(2)).unwrap_err();
            assert_eq!(refused.kind, "usage");
            assert!(
                refused.message.len() < MAX_ERROR_LEN + 20,
                "{}",
                refused.message.len()
            );
            assert!(refused.message.ends_with(" [truncated]"));
            // The oversized line ends the reader; nothing after it is read.
            let closed = remote.call(&request(3)).unwrap_err();
            assert_eq!(closed.kind, "session");
            assert!(matches!(
                events.recv_timeout(Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ));
        }
    }
}

#[cfg(test)]
mod via_tests {
    use super::*;

    #[test]
    fn the_connect_command_keeps_the_ssh_agent_and_nothing_secret() {
        let extra: Vec<String> = VIA_ENV.iter().map(|name| name.to_string()).collect();
        let env = crate::hooks::local_hook::scrub_env(
            [
                ("SSH_AUTH_SOCK", "/run/agent.sock"),
                ("SSH_AGENT_PID", "42"),
                ("OPENAI_API_KEY", "sk-no"),
                ("AWS_SECRET_ACCESS_KEY", "no"),
                ("GITHUB_TOKEN", "no"),
                ("HC_SESSION", "no"),
                ("PATH", "/usr/bin"),
            ],
            &extra,
        );
        let mut names: Vec<String> = env
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["PATH", "PRE_COMMIT", "SSH_AGENT_PID", "SSH_AUTH_SOCK"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bridge_that_fails_is_explained_by_its_stderr_and_not_by_the_terminal() {
        let argv: Vec<String> = ["sh", "-c", "echo 'ssh: connect refused' >&2; exit 255"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut client = Client::via(&argv).unwrap();
        let err = client.call("health", json!({})).unwrap_err();
        assert_eq!(err.kind, "session");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while client.via_stderr().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(client.via_stderr(), "ssh: connect refused");
    }
}
