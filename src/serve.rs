//! `ahi serve`: the session service on a Unix socket, one process per user
//! per host; `ahi connect`: a stdio bridge to that socket, so another
//! machine reaches the service over `ssh host ahi connect` exactly as
//! herdr-core reaches its own cores.
//!
//! There is no network listener, no token, and no TLS. Whoever can open
//! the socket (mode 600, in a directory of mode 700) is the user who owns
//! it; across machines, ssh carries the bytes and does the authenticating.
//! The server never idles out: it ends on `ahi serve stop`, SIGTERM or
//! SIGINT (open turns are cancelled and given `GRACE` to close), or the host
//! going down.

use std::path::{Path, PathBuf};

use crate::directories::Directories;
#[cfg(not(unix))]
use crate::error::{Error, Result};

/// Where the socket lives: `--socket`, else `$XDG_RUNTIME_DIR/ahi/serve.sock`
/// when that variable names an absolute path, else `<state>/run/serve.sock`.
pub fn socket_path(state: &Directories, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = PathBuf::from(runtime);
        if runtime.is_absolute() {
            return runtime.join("ahi").join("serve.sock");
        }
    }
    state.state.join("run").join("serve.sock")
}

#[cfg(unix)]
pub use unix::{connect, ensure_running, run, stop, try_connect};

#[cfg(not(unix))]
pub fn ensure_running(_state: &Directories, _socket: Option<&Path>) -> Result<PathBuf> {
    Err(Error::new(
        "ahi serve needs a Unix socket; on this platform run `ahi tui --standalone`",
    ))
}

#[cfg(not(unix))]
pub fn run(_state: &Directories, _socket: Option<&Path>, _verbose: bool) -> Result<u8> {
    Err(Error::new(
        "ahi serve needs a Unix socket; on this platform run `ahi exec` and `ahi tui` directly",
    ))
}

#[cfg(not(unix))]
pub fn stop(_state: &Directories, _socket: Option<&Path>) -> Result<u8> {
    Err(Error::new("ahi serve is not available on this platform"))
}

#[cfg(not(unix))]
pub fn connect(_state: &Directories, _socket: Option<&Path>) -> Result<u8> {
    Err(Error::new("ahi connect is not available on this platform"))
}

#[cfg(unix)]
mod unix {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use super::socket_path;
    use crate::directories::Directories;
    use crate::error::{Error, Result};
    use crate::service::wire::{self, Request, MAX_LINE};
    use crate::service::{Event, Service, GRACE};

    /// A subscribed connection hears from the server at least this often.
    const KEEPALIVE: Duration = Duration::from_secs(15);
    /// How often idle sessions (no turn, no subscriber) are let go of.
    const RELEASE_EVERY: Duration = Duration::from_secs(15);

    /// A stream to a live server at `path`, or `None` when nothing answers.
    /// A socket file nothing listens on is left for the server to clean up.
    pub fn try_connect(path: &Path) -> Option<UnixStream> {
        UnixStream::connect(path).ok()
    }

    /// `ahi serve`: bind, accept until told to stop, drain, exit.
    pub fn run(state: &Directories, socket: Option<&Path>, verbose: bool) -> Result<u8> {
        let path = socket_path(state, socket);
        if try_connect(&path).is_some() {
            return Err(Error::new(format!(
                "a server already answers on {}; `ahi serve stop` ends it",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            crate::directories::create_private_dir(parent)
                .map_err(|err| Error::new(format!("cannot create {}: {err}", parent.display())))?;
        }
        // A socket file nothing answers on is a crash's leftover.
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_file(&path).map_err(|err| {
                Error::new(format!("cannot remove stale {}: {err}", path.display()))
            })?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|err| Error::new(format!("cannot bind {}: {err}", path.display())))?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let service = Service::open(state);
        eprintln!(
            "ahi serve {} listening on {}; sessions in {}",
            crate::VERSION,
            path.display(),
            service.store_dir().display()
        );
        let watched = ahi_unix::watch(&[ahi_unix::Signal::Terminate, ahi_unix::Signal::Interrupt]);
        if !watched {
            eprintln!("warning: signals are not watched; stop the server with `ahi serve stop`");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let service = service.clone();
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let service = service.clone();
                    let stop = Arc::clone(&stop);
                    thread::spawn(move || serve_connection(service, stream, stop, verbose));
                }
            })
        };
        let mut next_release = Instant::now() + RELEASE_EVERY;
        while !stop.load(Ordering::Relaxed) && ahi_unix::received().is_none() {
            thread::sleep(Duration::from_millis(50));
            if Instant::now() >= next_release {
                let released = service.release_idle();
                if verbose && released > 0 {
                    eprintln!("released {released} idle session(s)");
                }
                next_release = Instant::now() + RELEASE_EVERY;
            }
        }
        let reason = match ahi_unix::received() {
            Some(signal) => signal.name().to_string(),
            None => "shutdown request".to_string(),
        };
        eprintln!(
            "stopping on {reason}: {} open turn(s) cancelled",
            service.open_turns()
        );
        service.shutdown(GRACE);
        let _ = std::fs::remove_file(&path);
        drop(accept);
        Ok(0)
    }

    /// The socket of a live server, starting one detached when none
    /// answers: stdio to `<state>/serve.log`, its own process group, so it
    /// outlives the chat that started it. Two starters race on the bind; the
    /// loser finds the winner answering and uses it.
    pub fn ensure_running(state: &Directories, socket: Option<&Path>) -> Result<PathBuf> {
        let path = socket_path(state, socket);
        if try_connect(&path).is_some() {
            return Ok(path);
        }
        let exe = std::env::current_exe()
            .map_err(|err| Error::new(format!("cannot find the ahi binary: {err}")))?;
        crate::directories::create_private_dir(&state.state)
            .map_err(|err| Error::new(format!("cannot create {}: {err}", state.state.display())))?;
        let log_path = state.state.join("serve.log");
        let log = crate::directories::private_file()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|err| Error::new(format!("cannot open {}: {err}", log_path.display())))?;
        let errors = log
            .try_clone()
            .map_err(|err| Error::new(format!("cannot open {}: {err}", log_path.display())))?;
        let mut command = std::process::Command::new(exe);
        // A single-root layout is passed on; the XDG layout resolves the same
        // way in the child from the same environment.
        if state.config == state.state && state.data == state.state {
            command.arg("--state-dir").arg(&state.state);
            if state.cache != state.state {
                command.arg("--cache-dir").arg(&state.cache);
            }
        }
        command
            .arg("serve")
            .arg("--socket")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(errors);
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command
            .spawn()
            .map_err(|err| Error::new(format!("cannot start ahi serve: {err}")))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if try_connect(&path).is_some() {
                return Ok(path);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Err(Error::new(format!(
            "ahi serve did not answer on {} within 2 s; see {}",
            path.display(),
            log_path.display()
        )))
    }

    /// `ahi serve stop`: ask the server on the socket to exit.
    pub fn stop(state: &Directories, socket: Option<&Path>) -> Result<u8> {
        let path = socket_path(state, socket);
        let Some(stream) = try_connect(&path) else {
            eprintln!("no server answers on {}", path.display());
            return Ok(1);
        };
        let mut writer = stream.try_clone().map_err(io_error)?;
        let line = json!({"id": 1, "method": "shutdown", "params": {}}).to_string();
        writer.write_all(line.as_bytes()).map_err(io_error)?;
        writer.write_all(b"\n").map_err(io_error)?;
        let mut answer = String::new();
        BufReader::new(stream)
            .read_line(&mut answer)
            .map_err(io_error)?;
        eprintln!("server on {} is stopping", path.display());
        Ok(0)
    }

    /// `ahi connect`: copy stdin to the socket and the socket to stdout
    /// until either side closes. What `ssh host ahi connect` runs. Exits 0
    /// when stdin ended (the client went away), 3 when the server closed
    /// first (it stopped or died mid-turn), 1 when nothing answered: a
    /// coordinator reads which side ended.
    pub fn connect(state: &Directories, socket: Option<&Path>) -> Result<u8> {
        let path = socket_path(state, socket);
        let Some(stream) = try_connect(&path) else {
            eprintln!("no server answers on {}", path.display());
            return Ok(1);
        };
        let mut to_socket = stream.try_clone().map_err(io_error)?;
        let stdin_ended = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&stdin_ended);
        let inbound = thread::spawn(move || {
            let _ = io::copy(&mut io::stdin().lock(), &mut to_socket);
            ended.store(true, Ordering::Relaxed);
            let _ = to_socket.shutdown(std::net::Shutdown::Write);
        });
        // Not `io::copy`: its buffered-writer path fills stdout's buffer before
        // flushing, so a one-line answer would wait for more bytes or EOF. A
        // bridge forwards each read at once.
        let mut from_socket = stream;
        let mut stdout = io::stdout().lock();
        let mut buffer = [0u8; 8192];
        loop {
            match from_socket.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout
                        .write_all(&buffer[..n])
                        .and_then(|()| stdout.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        // The server closed; a reader still blocked on stdin ends with the process.
        drop(inbound);
        if stdin_ended.load(Ordering::Relaxed) {
            Ok(0)
        } else {
            eprintln!("the server on {} closed the connection", path.display());
            Ok(3)
        }
    }

    fn io_error(err: io::Error) -> Error {
        Error::new(format!("socket: {err}"))
    }

    /// One client connection: requests in, answers and events out. The
    /// connection ends when every clone of the stream is dropped, so each
    /// forwarder watches `closed` and leaves; a `shutdown` call would not do,
    /// since on some platforms it fails once the peer has half-closed.
    fn serve_connection(
        service: Service,
        stream: UnixStream,
        stop: Arc<AtomicBool>,
        verbose: bool,
    ) {
        let writer = Arc::new(Mutex::new(match stream.try_clone() {
            Ok(writer) => writer,
            Err(_) => return,
        }));
        let closed = Arc::new(AtomicBool::new(false));
        let mut reader = BufReader::new(stream);
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
                let _ = write_line(
                    &writer,
                    &wire::response(
                        &Value::Null,
                        Err(crate::service::ServiceError {
                            kind: "usage",
                            message: format!("request longer than {MAX_LINE} bytes"),
                        }),
                    ),
                );
                break;
            }
            let text = String::from_utf8_lossy(&line);
            if text.trim().is_empty() {
                continue;
            }
            let request = match Request::parse(text.trim_end()) {
                Ok(request) => request,
                Err(message) => {
                    let _ = write_line(
                        &writer,
                        &wire::response(
                            &Value::Null,
                            Err(crate::service::ServiceError {
                                kind: "usage",
                                message,
                            }),
                        ),
                    );
                    continue;
                }
            };
            if verbose {
                eprintln!("request {}", request.method);
            }
            if request.method == "shutdown" {
                let _ = write_line(&writer, &wire::response(&request.id, Ok(json!({}))));
                stop.store(true, Ordering::Relaxed);
                break;
            }
            let forward_to = Arc::clone(&writer);
            let closed_flag = Arc::clone(&closed);
            let mut subscribe = |session: &str, receiver: Receiver<Event>| {
                let writer = Arc::clone(&forward_to);
                let closed = Arc::clone(&closed_flag);
                let session = session.to_string();
                thread::spawn(move || forward(session, receiver, writer, closed));
            };
            let answer = wire::dispatch(&service, &request, &mut subscribe);
            if write_line(&writer, &wire::response(&request.id, answer)).is_err() {
                break;
            }
        }
        closed.store(true, Ordering::Relaxed);
        drop(writer);
    }

    /// Events of one subscription to one connection, with a keepalive when
    /// nothing happens. Ends when the session drops the subscriber (said
    /// with one `lagged` event, so the client knows to resubscribe), a write
    /// fails, or the connection's reader has ended.
    fn forward(
        session: String,
        receiver: Receiver<Event>,
        writer: Arc<Mutex<UnixStream>>,
        closed: Arc<AtomicBool>,
    ) {
        let mut idle = Duration::ZERO;
        loop {
            if closed.load(Ordering::Relaxed) {
                return;
            }
            let event = match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(event) => {
                    idle = Duration::ZERO;
                    event
                }
                Err(RecvTimeoutError::Timeout) => {
                    idle += Duration::from_millis(250);
                    if idle < KEEPALIVE {
                        continue;
                    }
                    idle = Duration::ZERO;
                    Event::Keepalive
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = write_line(&writer, &Event::Lagged { session }.to_json());
                    return;
                }
            };
            if write_line(&writer, &event.to_json()).is_err() {
                return;
            }
        }
    }

    fn write_line(writer: &Arc<Mutex<UnixStream>>, value: &Value) -> io::Result<()> {
        let mut text = value.to_string();
        text.push('\n');
        let mut stream = writer
            .lock()
            .map_err(|_| io::Error::other("writer poisoned"))?;
        stream.write_all(text.as_bytes())
    }
}
