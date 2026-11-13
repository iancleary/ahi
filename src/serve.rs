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

/// The server's version must be this binary's: the wire is one process
/// talking to itself across time, and an upgraded binary must not keep
/// using the old server's protocol. `Ok` when they match.
pub fn check_version(found: &str, socket: &Path) -> crate::error::Result<()> {
    if found == crate::VERSION {
        return Ok(());
    }
    Err(crate::error::Error::new(format!(
        "the server on {} is ahi {found} and this is ahi {}; run `ahi serve stop` and start again",
        socket.display(),
        crate::VERSION
    )))
}

#[cfg(unix)]
mod unix {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// Connections the server holds at once; the next is refused and closed.
    const MAX_CONNECTIONS: usize = 64;
    /// Subscriptions one connection may hold.
    const MAX_SUBSCRIPTIONS: usize = 16;

    /// A stream to a live server at `path`, or `None` when nothing answers.
    /// A socket file nothing listens on is left for the server to clean up.
    pub fn try_connect(path: &Path) -> Option<UnixStream> {
        UnixStream::connect(path).ok()
    }

    /// `ahi serve`: take the lock, bind, accept until told to stop, drain,
    /// exit. The lock (`serve.lock` beside the socket) is held for the
    /// server's life, so two starters cannot both bind, and the socket file
    /// is only ever removed by the process that holds it: a loser never
    /// unlinks the winner's socket.
    pub fn run(state: &Directories, socket: Option<&Path>, verbose: bool) -> Result<u8> {
        let path = socket_path(state, socket);
        if let Some(parent) = path.parent() {
            crate::directories::create_private_dir(parent)
                .map_err(|err| Error::new(format!("cannot create {}: {err}", parent.display())))?;
        }
        let lock_path = lock_path(&path);
        let lock = crate::directories::private_file()
            .create(true)
            .open(&lock_path)
            .map_err(|err| Error::new(format!("cannot open {}: {err}", lock_path.display())))?;
        if !ahi_unix::try_lock_exclusive(&lock)
            .map_err(|err| Error::new(format!("cannot lock {}: {err}", lock_path.display())))?
        {
            return Err(Error::new(format!(
                "a server already runs: {} is held; `ahi serve stop` ends it",
                lock_path.display()
            )));
        }
        // Bind under a private name and rename into place: the socket is
        // never visible with umask permissions, and a stale file left by a
        // crash is replaced in the same step.
        // `.tmp` for `.sock`: no longer than the socket path, which is bounded.
        let staging = path.with_extension("tmp");
        let _ = std::fs::remove_file(&staging);
        let listener = UnixListener::bind(&staging)
            .map_err(|err| Error::new(format!("cannot bind {}: {err}", staging.display())))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))
                .map_err(|err| Error::new(format!("cannot chmod {}: {err}", staging.display())))?;
        }
        std::fs::rename(&staging, &path).map_err(|err| {
            let _ = std::fs::remove_file(&staging);
            Error::new(format!("cannot place {}: {err}", path.display()))
        })?;
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
        let connections = Arc::new(AtomicUsize::new(0));
        let accept = {
            let service = service.clone();
            let stop = Arc::clone(&stop);
            let connections = Arc::clone(&connections);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let Some(seat) = Seat::take(&connections) else {
                        refuse(
                            &stream,
                            "limit",
                            "the server holds its maximum of connections",
                        );
                        continue;
                    };
                    let service = service.clone();
                    let stop = Arc::clone(&stop);
                    thread::spawn(move || {
                        let _seat = seat;
                        serve_connection(service, stream, stop, verbose)
                    });
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
        // Removed while the lock is still held, so only this server's
        // socket ever goes; the lock itself is released by exit.
        let _ = std::fs::remove_file(&path);
        drop(accept);
        drop(lock);
        Ok(0)
    }

    /// `serve.lock` beside the socket.
    fn lock_path(socket: &Path) -> PathBuf {
        socket.with_extension("lock")
    }

    /// One of `MAX_CONNECTIONS`; given back when the connection ends.
    struct Seat(Arc<AtomicUsize>);

    impl Seat {
        fn take(connections: &Arc<AtomicUsize>) -> Option<Self> {
            let mut held = connections.load(Ordering::Relaxed);
            loop {
                if held >= MAX_CONNECTIONS {
                    return None;
                }
                match connections.compare_exchange_weak(
                    held,
                    held + 1,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(Self(Arc::clone(connections))),
                    Err(now) => held = now,
                }
            }
        }
    }

    impl Drop for Seat {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// One refusal line to a connection that gets nothing else.
    fn refuse(stream: &UnixStream, kind: &'static str, message: &str) {
        let mut stream = stream;
        let mut text = wire::response(
            &Value::Null,
            Err(crate::service::ServiceError {
                kind,
                message: message.to_string(),
            }),
        )
        .to_string();
        text.push('\n');
        let _ = stream.write_all(text.as_bytes());
    }

    /// The socket of a live server, starting one detached when none
    /// answers: stdio to `<state>/serve.log`, its own process group, so it
    /// outlives the chat that started it. Two starters race on the server's
    /// lock; the loser's server exits at once and the loser finds the winner
    /// answering within the wait.
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
        // The socket's mode already limits who connects; the kernel's word
        // on the peer is checked too, before a byte is read, so a socket
        // reached through a looser directory or a copied descriptor is still
        // one user talking to itself.
        match ahi_unix::peer_uid(&stream) {
            Ok(uid) if uid == ahi_unix::current_uid() => {}
            Ok(_) => {
                refuse(&stream, "auth", "the connecting user is not the server's");
                return;
            }
            Err(_) => {
                refuse(&stream, "auth", "the peer's credentials cannot be read");
                return;
            }
        }
        let writer = Arc::new(Mutex::new(match stream.try_clone() {
            Ok(writer) => writer,
            Err(_) => return,
        }));
        let closed = Arc::new(AtomicBool::new(false));
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        let mut subscriptions = 0usize;
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
            // During the drain nothing new is taken; open turns finish or
            // are cancelled, and this connection is told why and closed.
            if stop.load(Ordering::Relaxed) {
                let _ = write_line(
                    &writer,
                    &wire::response(
                        &request.id,
                        Err(crate::service::ServiceError {
                            kind: "conflict",
                            message: "the server is shutting down".into(),
                        }),
                    ),
                );
                break;
            }
            if request.method == "session.subscribe" {
                if subscriptions >= MAX_SUBSCRIPTIONS {
                    let _ = write_line(
                        &writer,
                        &wire::response(
                            &request.id,
                            Err(crate::service::ServiceError {
                                kind: "limit",
                                message: format!(
                                    "this connection holds its maximum of {MAX_SUBSCRIPTIONS} subscriptions"
                                ),
                            }),
                        ),
                    );
                    continue;
                }
                subscriptions += 1;
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_check_names_both_sides_and_the_remedy() {
        let socket = std::path::Path::new("/run/ahi/serve.sock");
        assert!(super::check_version(crate::VERSION, socket).is_ok());
        let err = super::check_version("0.1.0", socket)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is ahi 0.1.0"), "{err}");
        assert!(err.contains(crate::VERSION), "{err}");
        assert!(err.contains("ahi serve stop"), "{err}");
    }
}
