//! `ahi dev` through the real CLI without a container runtime: the runtime
//! lookup, the image resolution order, and the exact arguments handed to the
//! runtime, recorded by a stand-in script.

use std::fs;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "support/fixture_directory.rs"]
mod fixture_directory;

struct Fixture(PathBuf);

impl Fixture {
    fn new(purpose: &str) -> Self {
        Self(fixture_directory::create_unique_fixture_directory(purpose))
    }

    fn ahi(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ahi"));
        command
            .current_dir(&self.0)
            .env_remove("AHI_DEV_IMAGE")
            .env_remove("AHI_DEV_RUNTIME")
            .env("SHELL", "/usr/local/bin/fish")
            .env("AHI_STATE_DIR", self.0.join("state"));
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_missing_runtime_fails_closed_before_touching_anything() {
    let fixture = Fixture::new("dev-no-runtime");
    let output = fixture
        .ahi()
        .args(["dev", "status", "--runtime", "ahi-no-such-runtime-xyz"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = text(&output);
    assert!(text.contains("ahi-no-such-runtime-xyz"), "{text}");
    assert!(text.contains("podman or docker"), "{text}");
    assert!(!fixture.0.join("state").exists());
}

#[cfg(unix)]
fn fake_runtime(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("runtime.log");
    let script = dir.join("fake-runtime");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nprintf -- '--\\n' >> '{}'\nif [ \"$1\" = image ] && [ \"$2\" = inspect ]; then exit 0; fi\nexit 0\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    script
}

#[cfg(unix)]
#[test]
fn shell_mounts_only_the_working_directory_and_uses_the_resolved_image() {
    let fixture = Fixture::new("dev-shell");
    let runtime = fake_runtime(&fixture.0);
    fs::create_dir_all(fixture.0.join("state")).unwrap();
    fs::write(
        fixture.0.join("state").join("dev-image"),
        "# from the config directory\nexample.invalid/ahi-dev:file\n",
    )
    .unwrap();

    let output = fixture
        .ahi()
        .args(["dev", "shell", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let log = fs::read_to_string(fixture.0.join("runtime.log")).unwrap();
    let calls: Vec<Vec<&str>> = log
        .split("--\n")
        .filter(|call| !call.trim().is_empty())
        .map(|call| call.lines().collect())
        .collect();
    assert_eq!(calls.len(), 2, "{log}");
    assert_eq!(
        calls[0],
        ["image", "inspect", "example.invalid/ahi-dev:file"]
    );
    let run = &calls[1];
    assert_eq!(&run[..3], ["run", "--rm", "-i"]);
    assert!(!run.contains(&"-t"), "stdin is not a terminal: {run:?}");
    let mount = format!("{}:/work", fs::canonicalize(&fixture.0).unwrap().display());
    let volumes: Vec<&str> = run
        .windows(2)
        .filter(|pair| pair[0] == "--volume")
        .map(|pair| pair[1])
        .collect();
    assert_eq!(volumes, [mount.as_str()]);
    assert_eq!(
        &run[run.len() - 2..],
        ["example.invalid/ahi-dev:file", "fish"]
    );
}

#[cfg(unix)]
#[test]
fn the_image_flag_outranks_the_environment_which_outranks_the_file() {
    let fixture = Fixture::new("dev-image-order");
    let runtime = fake_runtime(&fixture.0);
    fs::create_dir_all(fixture.0.join("state")).unwrap();
    fs::write(
        fixture.0.join("state").join("dev-image"),
        "example.invalid/ahi-dev:file\n",
    )
    .unwrap();

    let from_env = fixture
        .ahi()
        .env("AHI_DEV_IMAGE", "example.invalid/ahi-dev:env")
        .args(["dev", "status", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    let from_flag = fixture
        .ahi()
        .env("AHI_DEV_IMAGE", "example.invalid/ahi-dev:env")
        .args([
            "dev",
            "status",
            "--image",
            "example.invalid/ahi-dev:flag",
            "--runtime",
        ])
        .arg(&runtime)
        .output()
        .unwrap();
    let from_file = fixture
        .ahi()
        .args(["dev", "status", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    assert!(
        text(&from_env).contains("image: example.invalid/ahi-dev:env (from AHI_DEV_IMAGE)"),
        "{}",
        text(&from_env)
    );
    assert!(
        text(&from_flag).contains("image: example.invalid/ahi-dev:flag (from --image)"),
        "{}",
        text(&from_flag)
    );
    assert!(
        text(&from_file).contains("image: example.invalid/ahi-dev:file (from dev-image)"),
        "{}",
        text(&from_file)
    );
}

#[cfg(unix)]
#[test]
fn the_repository_file_outranks_the_user_file_and_yields_to_the_environment() {
    let fixture = Fixture::new("dev-repo-file");
    let runtime = fake_runtime(&fixture.0);
    fs::create_dir_all(fixture.0.join("state")).unwrap();
    fs::write(
        fixture.0.join("state").join("dev-image"),
        "example.invalid/ahi-dev:file\n",
    )
    .unwrap();
    fs::write(
        fixture.0.join("ahi.dev.yaml"),
        "version: 1\nimage: example.invalid/ahi-dev:repo\nshell: bash\n",
    )
    .unwrap();

    let from_repo = fixture
        .ahi()
        .args(["dev", "status", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    let text_repo = text(&from_repo);
    assert!(
        text_repo.contains("image: example.invalid/ahi-dev:repo (from ahi.dev.yaml)"),
        "{text_repo}"
    );
    assert!(text_repo.contains("shell: bash"), "{text_repo}");

    let from_env = fixture
        .ahi()
        .env("AHI_DEV_IMAGE", "example.invalid/ahi-dev:env")
        .args(["dev", "status", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    assert!(
        text(&from_env).contains("image: example.invalid/ahi-dev:env (from AHI_DEV_IMAGE)"),
        "{}",
        text(&from_env)
    );

    fs::write(fixture.0.join("ahi.dev.yaml"), "version: 1\nmount: /x\n").unwrap();
    let invalid = fixture
        .ahi()
        .args(["dev", "status", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(1));
    assert!(text(&invalid).contains("mount"), "{}", text(&invalid));
}
