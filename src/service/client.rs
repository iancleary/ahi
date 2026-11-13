//! A client of the service with one calling shape and two transports:
//! the service built in this process, or the socket of a running
//! `ahi serve`. `ahi exec` and `ahi tui` both speak through it.

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
}

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
                    let (remote, events) = remote::Remote::new(stream);
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

    /// The socket in use, or `None` for the in-process service.
    pub(crate) fn socket(&self) -> Option<&Path> {
        match &self.transport {
            Transport::Local(_) => None,
            #[cfg(unix)]
            Transport::Remote(_, path) => Some(path),
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

fn usage(message: impl Into<String>) -> ServiceError {
    ServiceError {
        kind: "usage",
        message: message.into(),
    }
}

#[cfg(unix)]
mod remote {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    use serde_json::Value;

    use crate::service::wire::Request;
    use crate::service::{Event, ServiceError};

    /// One socket, one request at a time, events on the side.
    pub(super) struct Remote {
        stream: UnixStream,
        responses: Receiver<Value>,
    }

    impl Remote {
        pub(super) fn new(stream: UnixStream) -> (Self, Receiver<Event>) {
            let (answers, responses) = mpsc::channel();
            let (notify, events) = mpsc::channel();
            if let Ok(reader) = stream.try_clone() {
                thread::spawn(move || {
                    for line in BufReader::new(reader).lines() {
                        let Ok(line) = line else { break };
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
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
            }
            (Self { stream, responses }, events)
        }

        pub(super) fn call(&mut self, request: &Request) -> Result<Value, ServiceError> {
            let closed = || ServiceError {
                kind: "session",
                message: "the server closed the connection".to_string(),
            };
            let mut line = request.to_json().to_string();
            line.push('\n');
            self.stream
                .write_all(line.as_bytes())
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
                        _ => "session",
                    },
                    message: error["message"].as_str().unwrap_or("refused").to_string(),
                });
            }
            Ok(answer.get("result").cloned().unwrap_or(Value::Null))
        }
    }
}
