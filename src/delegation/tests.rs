//! Contract tests with a fake Rex CLI and a loopback gateway. No live Rex
//! server, no live provider, no `ahi serve`.
use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

const SESSION: &str = "session:11111111-1111-4111-8111-111111111111";
const BLOCK: &str = "block:22222222-2222-4222-8222-222222222222";
#[cfg(not(windows))]
const WORKER: &str = "/opt/ahi/bin/ahi";
#[cfg(windows)]
const WORKER: &str = r"C:\ahi\ahi.exe";
const PROMPT: &str = "Summarize the sentinel phrase quartz-lantern in one line.";

struct Fixture {
    root: PathBuf,
    dirs: Directories,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn fixture(purpose: &str) -> Fixture {
    let root = crate::fixture_directory::create_unique_fixture_directory(purpose);
    let dirs = Directories::single_root(root.clone());
    Fixture { root, dirs }
}
fn target() -> Target {
    Target::Rex {
        server: "127.0.0.1:4400".into(),
        session: SESSION.into(),
    }
}
fn spec(prompt: &str) -> Spec {
    Spec {
        gateway: "local".into(),
        model: "vendor/model".into(),
        thinking: "off".into(),
        prompt: prompt.into(),
        timeout_secs: 60,
        target: target(),
    }
}
fn run_worker(dir: &Path, attempt: &str) -> u8 {
    let cwd = load_dir(dir).map(|r| r.scratch).unwrap_or_default();
    worker::run_at(
        dir,
        attempt,
        &cwd,
        Observed {
            rex_session: Some(SESSION.into()),
            rex_block: Some(BLOCK.into()),
        },
    )
}
fn dir_of(f: &Fixture, receipt: &Receipt) -> PathBuf {
    assignment_dir(&f.dirs, &receipt.id).unwrap()
}

/// The fake native CLI from the terminal_host contract test, in its own
/// directory: it reads `reply` and appends to `calls` beside itself.
fn fake_rex(f: &Fixture) -> (PathBuf, PathBuf) {
    let host = f.root.join("host");
    fs::create_dir(&host).unwrap();
    let executable = host.join(format!("rex{}", std::env::consts::EXE_SUFFIX));
    let compile = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "tests/support/terminal_host_backend.rs",
            "-o",
        ])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    (executable, host)
}
fn calls(host: &Path) -> usize {
    fs::read_to_string(host.join("calls"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// Put a reservation in the state a worker may claim: launch intent for
/// `attempt`, expiring `ttl` seconds from now. Stands in for Rex's reply.
fn intend_launch(f: &Fixture, receipt: &mut Receipt, attempt: &str, ttl: i64) {
    let dir = dir_of(f, receipt);
    receipt.expires_at = Some(crate::util::rfc3339::unix_now() + ttl);
    advance(
        &dir,
        receipt,
        Phase::Launching {
            attempt: attempt.into(),
        },
    )
    .unwrap();
}

fn launching(attempt: &str) -> Phase {
    Phase::Launching {
        attempt: attempt.into(),
    }
}
fn launched(attempt: &str) -> Phase {
    Phase::Launched {
        attempt: attempt.into(),
        terminal: Handle {
            target: target(),
            id: BLOCK.into(),
        },
    }
}
fn unknown(attempt: &str) -> Phase {
    Phase::LaunchUnknown {
        attempt: attempt.into(),
        reason: "r".into(),
    }
}
fn failed(attempt: &str) -> Phase {
    Phase::LaunchFailed {
        attempt: attempt.into(),
        reason: "r".into(),
    }
}
#[test]
fn phases_follow_the_legal_transitions_only() {
    assert!(Phase::Reserved.can_become(&launching("a")));
    assert!(!Phase::Reserved.can_become(&launched("a")));
    assert!(launching("a").can_become(&launched("a")));
    assert!(launching("a").can_become(&unknown("a")));
    assert!(launching("a").can_become(&failed("a")));
    assert!(
        !launching("a").can_become(&launched("b")),
        "another attempt"
    );
    assert!(
        !launching("a").can_become(&launching("b")),
        "no second start"
    );
    assert!(!launched("a").can_become(&launching("b")));
    assert!(
        !unknown("a").can_become(&launching("b")),
        "ambiguity is not retried"
    );
    assert!(!failed("a").can_become(&launching("b")));
    assert!(launching("a").admits_claim("a"));
    assert!(launched("a").admits_claim("a"));
    assert!(
        unknown("a").admits_claim("a"),
        "a claim may precede Rex's reply"
    );
    assert!(!launching("a").admits_claim("b"));
    assert!(!Phase::Reserved.admits_claim("a"), "no launch was intended");
    assert!(!failed("a").admits_claim("a"));
}

#[test]
fn reserve_keeps_prompt_and_receipt_apart_from_scratch_and_never_adopts_a_path() {
    let f = fixture("delegation-reserve");
    let receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    assert_eq!(receipt.phase, Phase::Reserved);
    assert_eq!(receipt.expires_at, None);
    assert_eq!(
        receipt.prompt_sha256,
        crate::session::sha256_hex(PROMPT.as_bytes())
    );
    assert_eq!(receipt.prompt_bytes, PROMPT.len() as u64);
    assert!(receipt.scratch.is_dir());
    assert_eq!(
        fs::read_dir(&receipt.scratch).unwrap().count(),
        0,
        "scratch is empty"
    );
    assert!(!receipt.scratch.starts_with(&dir));
    assert!(!dir.starts_with(&receipt.scratch));
    assert_eq!(fs::read_to_string(dir.join(PROMPT_FILE)).unwrap(), PROMPT);
    let receipt_text = fs::read_to_string(dir.join(RECEIPT_FILE)).unwrap();
    assert!(
        !receipt_text.contains("quartz-lantern"),
        "the receipt never holds the prompt"
    );
    assert_eq!(load(&f.dirs, &receipt.id).unwrap(), receipt);
    assert_eq!(read_prompt(&dir, &receipt).unwrap(), PROMPT);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (path, mask) in [
            (dir.clone(), 0o777),
            (receipt.scratch.clone(), 0o777),
            (dir.join(PROMPT_FILE), 0o777),
            (dir.join(RECEIPT_FILE), 0o777),
        ] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & mask;
            assert_eq!(mode & 0o077, 0, "{} is {mode:o}", path.display());
        }
    }

    // The same id twice, or a scratch path that already exists, is a conflict.
    let err = reserve_with_id(&f.dirs, spec(PROMPT), receipt.id.clone()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    let other = crate::session::uuid();
    fs::create_dir_all(root(&f.dirs).join("scratch").join(&other)).unwrap();
    let err = reserve_with_id(&f.dirs, spec(PROMPT), other.clone()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(
        load(&f.dirs, &other).is_err()
            || !assignment_dir(&f.dirs, &other)
                .unwrap()
                .join(PROMPT_FILE)
                .exists()
    );

    // Specs that cannot be bound are refused before anything is created.
    let before = fs::read_dir(root(&f.dirs).join("assignments"))
        .unwrap()
        .count();
    let mut bad = spec("   ");
    assert_eq!(
        reserve(&f.dirs, bad.clone()).unwrap_err().kind,
        ErrorKind::Input
    );
    bad = spec(PROMPT);
    bad.thinking = "ultra".into();
    assert_eq!(
        reserve(&f.dirs, bad.clone()).unwrap_err().kind,
        ErrorKind::Input
    );
    bad = spec(PROMPT);
    bad.timeout_secs = 0;
    assert_eq!(
        reserve(&f.dirs, bad.clone()).unwrap_err().kind,
        ErrorKind::Input
    );
    bad = spec(PROMPT);
    bad.target = Target::Rex {
        server: "127.0.0.1:4400".into(),
        session: "label".into(),
    };
    assert_eq!(
        reserve(&f.dirs, bad.clone()).unwrap_err().kind,
        ErrorKind::Input
    );
    bad = spec(PROMPT);
    bad.target = Target::Hc {
        session: "core".into(),
        machine: None,
    };
    assert_eq!(
        reserve(&f.dirs, bad.clone()).unwrap_err().kind,
        ErrorKind::Input
    );
    bad = spec(PROMPT);
    bad.gateway = "a/b".into();
    assert_eq!(reserve(&f.dirs, bad).unwrap_err().kind, ErrorKind::Input);
    assert_eq!(
        reserve(&Directories::single_root("relative"), spec(PROMPT))
            .unwrap_err()
            .kind,
        ErrorKind::Input,
        "the worker runs elsewhere; relative ahi directories cannot be handed over"
    );
    assert_eq!(
        fs::read_dir(root(&f.dirs).join("assignments"))
            .unwrap()
            .count(),
        before
    );
    assert!(preview(&f.dirs, &spec(PROMPT)).is_ok());
    assert!(!Path::new("relative").exists());
}

#[test]
fn launch_is_issued_once_and_records_the_full_handle() {
    let f = fixture("delegation-launch");
    let (rex, host) = fake_rex(&f);
    fs::write(
        host.join("reply"),
        json!({"block_ids": [BLOCK]}).to_string(),
    )
    .unwrap();

    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    let err = launch(&f.dirs, &mut receipt, Path::new("relative/ahi"), Some(&rex)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Input);
    assert_eq!(
        receipt.phase,
        Phase::Reserved,
        "nothing persisted for a refused launch"
    );
    assert_eq!(calls(&host), 0);

    launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex)).unwrap();
    let Phase::Launched { attempt, terminal } = receipt.phase.clone() else {
        panic!("{:?}", receipt.phase);
    };
    assert_eq!(terminal.target, target());
    assert_eq!(terminal.id, BLOCK);
    assert!(receipt.expires_at.is_some());
    assert_eq!(
        load(&f.dirs, &receipt.id).unwrap(),
        receipt,
        "the handle is durable"
    );
    assert_eq!(calls(&host), 1);
    let input: Value = serde_json::from_slice(&fs::read(host.join("input")).unwrap()).unwrap();
    assert_eq!(input["session_id"], SESSION);
    assert_eq!(input["focus"], false);
    let options = &input["layout"]["block"]["options"];
    assert_eq!(options["shell"], "none");
    assert_eq!(options["cwd"], receipt.scratch.display().to_string());
    assert_eq!(
        options["command"],
        json!(worker_argv(Path::new(WORKER), &dir, &attempt))
    );
    let argv = options["command"].to_string();
    assert!(argv.contains(WORKER_SUBCOMMAND));
    assert!(!argv.contains("quartz-lantern"), "no prompt in argv");
    assert!(!argv.contains("vendor/model"), "no route in argv");
    assert!(
        !argv.contains(&receipt.scratch.display().to_string()),
        "cwd is not an argument"
    );
    assert_eq!(
        fs::read_dir(&receipt.scratch).unwrap().count(),
        0,
        "launch writes nothing to scratch"
    );

    // No second Start on a launched assignment, from this or a reloaded receipt.
    let err = launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    let mut reloaded = load(&f.dirs, &receipt.id).unwrap();
    let err = launch(&f.dirs, &mut reloaded, Path::new(WORKER), Some(&rex)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(calls(&host), 1);

    let status = status(&f.dirs, &receipt.id).unwrap();
    assert_eq!(status["state"], "launched");
    assert_eq!(status["settled"], false);
    assert_eq!(status["assignment"]["phase"]["terminal"]["id"], BLOCK);
    assert!(status["claim"].is_null());
    assert!(status["result"].is_null());

    cancel(&f.dirs, &mut receipt).unwrap();
    assert_eq!(
        super::status(&f.dirs, &receipt.id).unwrap()["assignment"]["phase"]["terminal"]["id"],
        BLOCK
    );
    assert!(cancel_requested(&dir));
    cancel(&f.dirs, &mut receipt).unwrap(); // idempotent while unsettled
    assert_eq!(
        launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex))
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
    assert_eq!(calls(&host), 1, "cancel touches no terminal");
}

#[test]
fn neither_ambiguous_nor_failed_launches_are_reissued() {
    let f = fixture("delegation-launch-ambiguous");
    let (rex, host) = fake_rex(&f);

    // Rex answered, but with no usable block: a block may exist.
    fs::write(host.join("reply"), json!({"block_ids": []}).to_string()).unwrap();
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let err = launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Launch);
    assert!(
        matches!(receipt.phase, Phase::LaunchUnknown { .. }),
        "{:?}",
        receipt.phase
    );
    assert_eq!(load(&f.dirs, &receipt.id).unwrap().phase, receipt.phase);
    let err = launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(
        calls(&host),
        1,
        "an ambiguous delivery is never followed by another Start"
    );
    assert_eq!(
        status(&f.dirs, &receipt.id).unwrap()["state"],
        "launch_unknown"
    );
    // The worker may still have been started and may claim this attempt.
    let attempt = receipt.phase.attempt().unwrap().to_string();
    assert!(claim(&dir_of(&f, &receipt), &attempt, &Observed::default()).is_ok());

    // Even a confirmed failed start needs a new assignment.
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let err = launch(
        &f.dirs,
        &mut receipt,
        Path::new(WORKER),
        Some(&f.root.join("not-installed")),
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Launch);
    let Phase::LaunchFailed { attempt: first, .. } = receipt.phase.clone() else {
        panic!("{:?}", receipt.phase);
    };
    assert!(claim(&dir_of(&f, &receipt), &first, &Observed::default()).is_err());
    assert!(launch(&f.dirs, &mut receipt, Path::new(WORKER), Some(&rex)).is_err());
    assert_eq!(calls(&host), 1);
}

#[test]
fn late_start_reply_cannot_erase_cancel_or_allow_inference() {
    let f = fixture("late-start");
    let mut launcher = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &launcher);
    intend_launch(&f, &mut launcher, "a1", 60);
    let mut operator = load_dir(&dir).unwrap();
    cancel(&f.dirs, &mut operator).unwrap();
    advance(&dir, &mut launcher, launched("a1")).unwrap();
    assert_eq!(status(&f.dirs, &launcher.id).unwrap()["state"], "cancelled");
    assert!(claim(&dir, "a1", &Observed::default()).is_err());
    assert!(!dir.join(CLAIM_FILE).exists());
}

#[test]
fn claims_are_single_and_bound_to_the_intended_attempt_and_results_publish_once() {
    let f = fixture("delegation-claim");
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    let err = claim(&dir, "a1", &Observed::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "nothing was launched");
    assert!(read_claim(&dir).unwrap().is_none());

    intend_launch(&f, &mut receipt, "a1", 60);
    assert_eq!(
        claim(&dir, "zz", &Observed::default()).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert_eq!(
        claim(&dir, "not valid!", &Observed::default())
            .unwrap_err()
            .kind,
        ErrorKind::Input
    );
    assert!(read_claim(&dir).unwrap().is_none());
    let observed = Observed {
        rex_session: Some("session:33333333-3333-4333-8333-333333333333".into()),
        rex_block: Some(BLOCK.into()),
    };
    let (seen, claimed) = claim(&dir, "a1", &observed).unwrap();
    assert_eq!(seen.id, receipt.id);
    assert_eq!(claimed.attempt, "a1");
    assert!(
        claimed.session_mismatch,
        "observed IDs are diagnostic, and disagreement is recorded"
    );
    assert_eq!(read_claim(&dir).unwrap(), Some(claimed.clone()));
    let err = claim(&dir, "a1", &Observed::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "second claimant");
    assert_eq!(
        read_claim(&dir).unwrap(),
        Some(claimed),
        "the first claim stands"
    );
    assert_eq!(status(&f.dirs, &receipt.id).unwrap()["state"], "claimed");
    assert!(remaining_secs(&receipt) > 0);

    cancel(&f.dirs, &mut receipt).unwrap();
    assert!(cancel_requested(&dir));
    assert_eq!(status(&f.dirs, &receipt.id).unwrap()["state"], "cancelling");

    let outcome = Outcome {
        turns_completed: 1,
        assignment: receipt.id.clone(),
        attempt: "a1".into(),
        status: Status::Cancelled,
        gateway: receipt.gateway.clone(),
        model: receipt.model.clone(),
        text: String::new(),
        text_bytes: 0,
        truncated: false,
        usage: None,
        error: None,
        finished: crate::util::rfc3339::now(),
    };
    finish(&dir, &outcome).unwrap();
    let mut second = outcome.clone();
    second.status = Status::Ok;
    second.text = "late".into();
    assert_eq!(finish(&dir, &second).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(read_outcome(&dir).unwrap(), Some(outcome));
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        5,
        "receipt, prompt, claim, cancel, result; no temporaries"
    );
    let status = status(&f.dirs, &receipt.id).unwrap();
    assert_eq!(status["state"], "finished");
    assert_eq!(status["settled"], true);
    assert_eq!(status["result"]["status"], "cancelled");
    assert_eq!(
        cancel(&f.dirs, &mut receipt).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert_eq!(
        launch(&f.dirs, &mut receipt, Path::new(WORKER), None)
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );

    // An expired attempt is refused before any provider contact.
    let mut stale = reserve(&f.dirs, spec(PROMPT)).unwrap();
    intend_launch(&f, &mut stale, "a2", -1);
    let stale_dir = dir_of(&f, &stale);
    assert_eq!(
        claim(&stale_dir, "a2", &Observed::default())
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
    assert!(read_claim(&stale_dir).unwrap().is_none());

    // A receipt copied under another directory name does not describe it.
    let borrowed = f.root.join(crate::session::uuid());
    fs::create_dir(&borrowed).unwrap();
    fs::copy(dir.join(RECEIPT_FILE), borrowed.join(RECEIPT_FILE)).unwrap();
    assert_eq!(load_dir(&borrowed).unwrap_err().kind, ErrorKind::Input);
    assert_eq!(
        load(&f.dirs, "not-an-id").unwrap_err().kind,
        ErrorKind::Input
    );
}

#[test]
fn service_claim_requires_a_nonce_and_cannot_be_rebound_to_another_connection() {
    use super::broker::Connection;
    let f = fixture("claim-connection");
    let service = crate::service::Service::open(&f.dirs);
    let sock = f.root.join("socket");
    let mut r = reserve(&f.dirs, spec(PROMPT)).unwrap();
    intend_launch(&f, &mut r, "one", 60);
    let dir = dir_of(&f, &r);
    write_private_new(
        &dir.join("broker.json"),
        json!({"socket":sock,"nonce":"secret"})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    write_private_new(
        &dir.join("admission.json"),
        br#"{"mode":"plan","policy":{"rules":[],"named":{},"default_mode":null},"follow_ups":[]}"#,
    )
    .unwrap();
    let mut first = Connection::default();
    let mut other = Connection::default();
    let mut p = json!({"id":r.id,"attempt":"one","nonce":"wrong","pid":42,"cwd":r.scratch,"observed":{"rex_session":SESSION,"rex_block":BLOCK}});
    assert!(first
        .dispatch(&service, &sock, "child.claim", p.clone())
        .is_err());
    assert!(read_claim(&dir).unwrap().is_none());
    p["nonce"] = json!("secret");
    let claim = first
        .dispatch(&service, &sock, "child.claim", p.clone())
        .unwrap();
    assert_eq!(claim["claim"]["pid"], 42);
    assert!(other.dispatch(&service, &sock, "child.claim", p).is_err());
    let finish = json!({"assignment":r.id,"attempt":"one","gateway":"local","model":"vendor/model","status":"ok","text":"answer","text_bytes":6,"turns_completed":1,"truncated":false,"finished":""});
    assert!(other
        .dispatch(&service, &sock, "child.finish", finish.clone())
        .is_err());
    assert!(read_outcome(&dir).unwrap().is_none());
    assert_eq!(
        first
            .dispatch(&service, &sock, "child.finish", finish.clone())
            .unwrap()["status"],
        "ok"
    );
    assert!(first
        .dispatch(&service, &sock, "child.finish", finish)
        .is_err());
}

#[test]
fn bounded_text_cuts_on_character_boundaries() {
    let mut text = String::new();
    assert!(!push_bounded(&mut text, "ab", 8));
    assert!(push_bounded(&mut text, "cd世界", 8));
    assert_eq!(text, "abcd世");
    assert!(!push_bounded(&mut text, "x", 8));
    assert_eq!(text, "abcd世x");
    let mut text = String::new();
    assert!(
        push_bounded(&mut text, "世", 1),
        "no boundary within the limit drops the whole delta"
    );
    assert_eq!(text, "");
}

// ---- loopback gateway: the worker's actual provider request ----

fn write_models(f: &Fixture, url: &str) {
    fs::write(
        f.dirs.config.join("models.json"),
        json!({"providers": {"local": {
            "baseUrl": url, "api": "openai-completions", "apiKey": "gateway",
            "headers": {"X-Gateway": "gateway"}, "models": [{"id": "vendor/model"}]
        }}})
        .to_string(),
    )
    .unwrap();
}

fn event_stream(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Serve exactly one request, then stop listening. Returns the request.
fn serve_one(response: String) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no model request received");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        drop(listener);
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        request.push_str(std::str::from_utf8(&body).unwrap());
        let _ = stream.write_all(response.as_bytes());
        request
    });
    (url, handle)
}

fn request_body(request: &str) -> Value {
    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
}

#[test]
fn worker_runs_one_toolless_turn_on_the_bound_route_and_publishes_a_structured_result() {
    let f = fixture("delegation-worker");
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":12}}}\n\n",
        "data: [DONE]\n\n"
    );
    let (url, server) = serve_one(event_stream(body));
    write_models(&f, &url);
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    // A policy planted in the worker's cwd must not give it tools.
    fs::write(
        receipt.scratch.join("ahi.tools.yaml"),
        crate::tool_policy::examples::EXAMPLES[1].text,
    )
    .unwrap();
    intend_launch(&f, &mut receipt, "a1", 60);

    assert_eq!(run_worker(&dir, "a1"), 2, "non-empty scratch refused");
    fs::remove_file(receipt.scratch.join("ahi.tools.yaml")).unwrap();
    assert_eq!(
        worker::run_at(&dir, "a1", &receipt.scratch, Observed::default()),
        2,
        "wrong Rex session refused"
    );
    assert!(read_claim(&dir).unwrap().is_none());
    assert_eq!(run_worker(&dir, "a1"), 0);
    let request = server.join().unwrap();
    assert!(
        request.starts_with("POST /v1/chat/completions "),
        "{request}"
    );
    let body = request_body(&request);
    assert_eq!(
        body["model"], "vendor/model",
        "the exact bound model, not a saved selection"
    );
    assert!(
        body.get("tools").is_none(),
        "no tool schema is offered: {body}"
    );
    assert!(body.get("tool_choice").is_none());
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["content"], PROMPT);

    let outcome = read_outcome(&dir).unwrap().expect("result published");
    assert_eq!(outcome.status, Status::Ok);
    assert_eq!(outcome.text, "Hello 世界");
    assert_eq!(outcome.text_bytes, "Hello 世界".len() as u64);
    assert!(!outcome.truncated);
    assert_eq!(outcome.assignment, receipt.id);
    assert_eq!(outcome.attempt, "a1");
    assert_eq!(
        (outcome.gateway.as_str(), outcome.model.as_str()),
        ("local", "vendor/model")
    );
    assert_eq!(
        outcome.usage,
        Some(Usage {
            input_tokens: 42,
            output_tokens: 7,
            cached_input_tokens: Some(12)
        })
    );
    assert_eq!(outcome.error, None);
    let claimed = read_claim(&dir).unwrap().unwrap();
    assert_eq!(claimed.pid, std::process::id());
    let status = status(&f.dirs, &receipt.id).unwrap();
    assert_eq!(status["state"], "finished");
    assert_eq!(status["result"]["text"], "Hello 世界");
    assert_eq!(
        fs::read_dir(&receipt.scratch).unwrap().count(),
        0,
        "the worker writes nothing to scratch"
    );
    assert!(
        !fs::read_to_string(dir.join(RECEIPT_FILE))
            .unwrap()
            .contains("Hello"),
        "the worker never edits the receipt"
    );

    // A second delivery of the same attempt: no claim, no request, same result.
    assert_eq!(run_worker(&dir, "a1"), 3);
    assert_eq!(read_outcome(&dir).unwrap(), Some(outcome));
}

#[test]
fn worker_fails_closed_when_the_model_asks_for_a_tool() {
    let f = fixture("delegation-worker-tool");
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call_a", "type": "function", "function": {"name": "run_command", "arguments": "{\"command\":\"id\"}"}}
        ]}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
    );
    let (url, server) = serve_one(event_stream(&body));
    write_models(&f, &url);
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    intend_launch(&f, &mut receipt, "a1", 60);
    assert_eq!(run_worker(&dir, "a1"), 1);
    server.join().unwrap();
    let outcome = read_outcome(&dir)
        .unwrap()
        .expect("a failed result is still a result");
    assert_eq!(outcome.status, Status::Failed);
    let error = outcome.error.expect("the failure is named");
    assert_ne!(
        error.kind, "transport",
        "one request only; the listener closed after it: {error:?}"
    );
    assert!(
        !error.message.contains("\"command\""),
        "tool arguments do not leak into the result"
    );
}

#[test]
fn corrupted_prompt_settles_the_claim_without_inference() {
    let f = fixture("worker-corrupt");
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    intend_launch(&f, &mut receipt, "a1", 60);
    fs::write(dir.join(PROMPT_FILE), "different prompt").unwrap();
    assert_eq!(run_worker(&dir, "a1"), 1);
    let status = status(&f.dirs, &receipt.id).unwrap();
    assert_eq!(status["settled"], true);
    assert_eq!(status["result"]["status"], "failed");
}

#[test]
fn worker_refuses_without_a_provider_request_when_cancelled_stale_or_duplicate() {
    let f = fixture("delegation-worker-refuse");
    // An unreachable gateway: any provider contact would fail as transport.
    write_models(&f, "http://127.0.0.1:1/v1");
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    assert_eq!(run_worker(&dir, "a1"), 3, "no launch intent");
    intend_launch(&f, &mut receipt, "a1", 60);
    assert_eq!(run_worker(&dir, "zz"), 3, "another attempt");
    cancel(&f.dirs, &mut receipt).unwrap();
    assert_eq!(run_worker(&dir, "a1"), 3, "cancelled before the claim");
    assert!(read_claim(&dir).unwrap().is_none());
    assert!(read_outcome(&dir).unwrap().is_none());
    assert_eq!(run_worker(Path::new("relative"), "a1"), 2);
    assert_eq!(run_worker(&f.root.join("missing"), "a1"), 2);

    // Cancelled after the claim, before the provider: a cancelled result, no request.
    let mut receipt = reserve(&f.dirs, spec(PROMPT)).unwrap();
    let dir = dir_of(&f, &receipt);
    intend_launch(&f, &mut receipt, "a1", 60);
    let (_, claimed) = claim(&dir, "a1", &Observed::default()).unwrap();
    assert_eq!(
        run_worker(&dir, "a1"),
        3,
        "the claim is held by {}",
        claimed.pid
    );
    assert!(
        read_outcome(&dir).unwrap().is_none(),
        "a refused worker publishes nothing"
    );
}
