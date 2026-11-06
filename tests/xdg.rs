//! The XDG layout through the real CLI and the library: every category lands
//! in its own root, and nothing is read from an old single-root location.
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use ahi::directories::Directories;

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

struct Fixture(PathBuf);

impl Fixture {
    fn new(purpose: &str) -> Self {
        Self(fixture_directory::create_unique_fixture_directory(purpose))
    }

    fn directories(&self) -> Directories {
        Directories {
            config: self.0.join("config"),
            data: self.0.join("data"),
            state: self.0.join("state"),
            cache: self.0.join("cache"),
        }
    }

    /// The CLI with only XDG variables set, each pointing into the fixture.
    fn ahi(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
        command
            .env_remove("AHI_STATE_DIR")
            .env_remove("AHI_HOME")
            .env_remove("LOCALAPPDATA")
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("XDG_CACHE_HOME", self.0.join("cache"));
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn the_cli_reads_config_credentials_and_catalogs_from_their_own_roots() {
    let fixture = Fixture::new("xdg-cli");
    let root = &fixture.0;
    let config = br#"{"providers":{"lab":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","models":[{"id":"curated"}]}}}"#;
    write(&root.join("config/ahi/models.json"), config);
    write(
        &root.join("data/ahi/credentials/openai.json"),
        b"presence is all this test needs",
    );
    write(&root.join("cache/ahi/catalogs/lab.json"), br#"["curated"]"#);
    // A models.json left in the state root is never consulted.
    write(&root.join("state/ahi/models.json"), b"not json");
    let output = fixture.ahi().arg("tui").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}{err}");
    assert!(text.contains("signed-in"), "{text}");
    assert!(text.contains("lab"), "{text}");
    assert!(text.contains("curated"), "{text}");
    assert_eq!(
        fs::read(root.join("config/ahi/models.json")).unwrap(),
        config
    );
    assert!(!root.join("state/ahi/catalogs").exists());
}

#[test]
fn doctor_prints_every_location() {
    let fixture = Fixture::new("xdg-doctor");
    let root = &fixture.0;
    let output = fixture.ahi().arg("doctor").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let expect = |label: &str, dir: &str| {
        let line = format!("{label}: {}", root.join(dir).join("ahi").display());
        assert!(text.contains(&line), "{line}\n{text}");
    };
    expect("models.json", "config");
    expect("data", "data");
    expect("state", "state");
    expect("cache", "cache");
}

#[test]
fn a_curated_save_writes_only_the_config_root() {
    let fixture = Fixture::new("xdg-save");
    let dirs = fixture.directories();
    let registry = ahi::harness::load_registry(&dirs).unwrap();
    ahi::harness::save_curated_models(&dirs, &registry, "openai", &["test-model".into()]).unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(dirs.config.join("models.json")).unwrap()).unwrap();
    assert_eq!(
        document["providers"]["openai"]["models"][0]["id"],
        "test-model"
    );
    assert!(!dirs.data.exists());
    assert!(!dirs.state.exists());
    assert!(!dirs.cache.exists());
}

#[test]
fn a_catalog_refresh_writes_the_cache_and_leaves_config_alone() {
    let fixture = Fixture::new("xdg-refresh");
    let dirs = fixture.directories();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        let length = stream.read(&mut request).unwrap();
        assert!(request[..length].starts_with(b"GET /agent/models.json HTTP/1.1\r\n"));
        let body = r#"{"id":"lab","api":"openai-completions","apiKey":"gateway","models":[{"id":"available"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let config = format!(
        r#"{{"providers":{{"lab":{{"baseUrl":"http://{address}/v1","api":"openai-completions","models":[{{"id":"curated"}}]}}}}}}"#
    );
    write(&dirs.config.join("models.json"), config.as_bytes());
    let result = ahi::harness::refresh_provider(&dirs, "lab");
    server.join().unwrap();
    result.unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(dirs.cache.join("catalogs/lab.json")).unwrap()).unwrap();
    assert_eq!(snapshot["models"], serde_json::json!(["available"]));
    assert_eq!(
        fs::read_to_string(dirs.config.join("models.json")).unwrap(),
        config
    );
    assert!(!dirs.data.exists());
    assert!(!dirs.state.exists());
}

#[test]
fn the_hook_audit_log_is_state_not_cache() {
    let fixture = Fixture::new("xdg-audit");
    let root = &fixture.0;
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q", "--template="]);
    write(
        &repo.join(".pre-commit-config.yaml"),
        b"repos:\n- repo: builtin\n  hooks:\n  - id: ahi/trailing-whitespace\n",
    );
    write(&repo.join("ok.txt"), b"ok\n");
    git(&["add", "-A"]);
    let output = fixture
        .ahi()
        .current_dir(&repo)
        .args(["run", "--all-files"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let audit = fs::read_to_string(root.join("state/ahi/audit.log")).unwrap();
    let record: serde_json::Value = serde_json::from_str(audit.trim()).unwrap();
    assert_eq!(record["command"], "run");
    assert_eq!(record["outcome"], "passed");
    assert!(!root.join("cache").exists());
}
