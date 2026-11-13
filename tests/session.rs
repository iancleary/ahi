//! `ahi session` over a disposable state directory: import a session file,
//! list and show it, export it again, and the refusals.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

fn ahi(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ahi"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .expect("ahi")
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn errors(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const ID: &str = "0f1e2d3c-4b5a-4978-8765-43210fedcba9";

fn session_file(dir: &Path) -> PathBuf {
    let path = dir.join("from-elsewhere.jsonl");
    fs::write(
        &path,
        [
            format!(r##"{{"type":"session","schema":1,"id":"{ID}","started":"2026-11-12T10:00:00Z","host":"worker","cwd":"/work/repo","gateway":"local","model":"m1","thinking":"off","ahi_version":"0.15.0"}}"##),
            r##"{"seq":1,"parent":null,"time":"2026-11-12T10:00:01Z","type":"message","role":"operator","text":"summarize the readme"}"##.to_string(),
            r##"{"seq":2,"parent":1,"time":"2026-11-12T10:00:02Z","type":"message","role":"assistant","text":"reading","calls":[{"id":"c1","name":"read_file","arguments":{"path":"README.md"}}]}"##.to_string(),
            r##"{"seq":3,"parent":2,"time":"2026-11-12T10:00:03Z","type":"message","role":"tool","exit_code":0,"text":"read_file README.md\n# ahi","result":{"call_id":"c1","name":"read_file","content":"# ahi\n"}}"##.to_string(),
            r##"{"seq":4,"parent":3,"time":"2026-11-12T10:00:04Z","type":"message","role":"assistant","text":"It is a pre-commit runner."}"##.to_string(),
            r##"{"seq":5,"parent":4,"time":"2026-11-12T10:00:05Z","type":"turn","run_id":"00112233aabbccdd","status":"ok","prompt_sha256":"","result":{"origin":"exec"}}"##.to_string(),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    path
}

#[test]
fn sessions_are_listed_shown_exported_and_imported() {
    let dir = fixture_directory::create_unique_fixture_directory("session-cli");
    let state = dir.join("state");
    fs::create_dir_all(&state).unwrap();

    let empty = ahi(&state, &["session", "list"]);
    assert!(empty.status.success());
    assert_eq!(text(&empty), "no sessions yet\n");
    assert_eq!(text(&ahi(&state, &["session", "list", "--json"])), "");

    let file = session_file(&dir);
    let imported = ahi(&state, &["session", "import", file.to_str().unwrap()]);
    assert!(imported.status.success(), "{}", errors(&imported));
    assert_eq!(
        text(&imported),
        format!(
            "imported session {ID} from {}: 5 new entries\n",
            file.display()
        )
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let stored = state.join("sessions").join(format!("{ID}.jsonl"));
        assert_eq!(
            fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(state.join("sessions"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    let listed = ahi(&state, &["session", "list"]);
    assert_eq!(
        text(&listed),
        format!("{ID}  2026-11-12T10:00:00Z  local/m1  /work/repo  1 turn  last ok (00112233aabbccdd)\n")
    );
    let json: serde_json::Value =
        serde_json::from_str(text(&ahi(&state, &["session", "list", "--json"])).trim()).unwrap();
    assert_eq!(json["id"], ID);
    assert_eq!(json["entries"], 5);
    assert_eq!(json["last_status"], "ok");

    let shown = ahi(&state, &["session", "show", ID]);
    assert!(shown.status.success(), "{}", errors(&shown));
    assert_eq!(
        text(&shown),
        format!(
            "session {ID}  local/m1  /work/repo  started 2026-11-12T10:00:00Z  host worker\n\
             \n> summarize the readme\n\
             \nreading\n  [call read_file README.md]\n\
             \x20 [tool exit 0] read_file README.md\n\
             \nIt is a pre-commit runner.\n\
             — turn 00112233aabbccdd ok  2026-11-12T10:00:05Z\n"
        )
    );
    assert!(
        !text(&shown).contains("# ahi\n"),
        "tool content is not printed, only the screen preview's first line"
    );
    // The file itself can be shown by path, and --json gives the lines back.
    let by_path = ahi(
        &state,
        &["session", "show", file.to_str().unwrap(), "--json"],
    );
    assert_eq!(by_path.stdout, fs::read(&file).unwrap());
    let branch = ahi(&state, &["session", "show", ID, "--branch", "2"]);
    assert!(text(&branch).contains("[call read_file README.md]"));
    assert!(!text(&branch).contains("pre-commit runner"));
    let missing = ahi(&state, &["session", "show", ID, "--branch", "9"]);
    assert!(!missing.status.success());
    assert!(errors(&missing).contains("no entry 9"));

    let out = dir.join("copy.jsonl");
    let exported = ahi(
        &state,
        &["session", "export", ID, "--to", out.to_str().unwrap()],
    );
    assert!(exported.status.success(), "{}", errors(&exported));
    assert_eq!(fs::read(&out).unwrap(), fs::read(&file).unwrap());
    let again = ahi(
        &state,
        &["session", "export", ID, "--to", out.to_str().unwrap()],
    );
    assert!(!again.status.success());
    assert!(errors(&again).contains("already exists"));

    let unknown = ahi(&state, &["session", "show", "nope"]);
    assert!(!unknown.status.success());
    assert!(
        errors(&unknown).contains("not a session id"),
        "{}",
        errors(&unknown)
    );
    let traversal = ahi(&state, &["session", "export", "../x", "--to", "/dev/null"]);
    assert!(!traversal.status.success());
    assert!(errors(&traversal).contains("not a session id"));

    fs::remove_dir_all(dir).unwrap();
}
