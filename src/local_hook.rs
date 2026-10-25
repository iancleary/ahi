//! Run `repo: local` hooks whose language is `system` or `script`.
//!
//! The `entry` is split without a shell. Batches follow the xargs partition.
//! `require_serial` and `PRE_COMMIT_NO_CONCURRENCY` force one worker. Batches
//! themselves run one after another so output stays ordered and hooks that
//! share a build directory (cargo) do not overlap.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};
use crate::filter::{self, command_budget};
use crate::paths::join_inside;
use crate::shlex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOutput {
    pub failed: bool,
    pub exit_code: i32,
    pub output: String,
    pub invocations: usize,
}

pub struct LocalRequest<'a> {
    pub root: &'a Path,
    pub entry: &'a str,
    pub args: &'a [String],
    pub files: &'a [String],
    pub pass_filenames: bool,
    pub require_serial: bool,
    pub script: bool,
    pub log_file: &'a str,
}

pub fn execute(request: &LocalRequest<'_>) -> Result<LocalOutput> {
    let planned = plan(request, None)?;
    run_planned(request.root, &planned, request.log_file)
}

pub(crate) fn plan(
    request: &LocalRequest<'_>,
    max_length: Option<usize>,
) -> Result<Vec<Vec<String>>> {
    let mut argv = shlex::split(request.entry)?;
    if request.script {
        argv[0] = resolve_script(request.root, &argv[0])?;
    }
    argv.extend(request.args.iter().cloned());
    let concurrency =
        if request.require_serial || std::env::var_os("PRE_COMMIT_NO_CONCURRENCY").is_some() {
            1
        } else {
            std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1)
        };
    let budget = max_length.unwrap_or_else(|| command_budget(256 * 1024, scrubbed_environ_size()));
    if !request.pass_filenames || request.files.is_empty() {
        return Ok(vec![argv]);
    }
    filter::partition(&argv, request.files, concurrency, budget).map_err(|err| {
        Error::new(format!(
            "argument `{}` does not fit in one command line",
            err.argument
        ))
    })
}

fn resolve_script(root: &Path, token: &str) -> Result<String> {
    if token.contains('\0') {
        return Err(Error::new("script entry contains a NUL"));
    }
    let path = if Path::new(token).is_absolute() {
        return Err(Error::new(format!(
            "script entry `{token}` must stay inside the repository"
        )));
    } else {
        join_inside(root, token)?
    };
    if !path.is_file() {
        return Err(Error::new(format!(
            "script `{token}` is not a file inside the repository"
        )));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn run_planned(root: &Path, batches: &[Vec<String>], log_file: &str) -> Result<LocalOutput> {
    let env = scrub_env(std::env::vars_os());
    let mut combined = String::new();
    let mut failed = false;
    let mut exit_code = 0;
    for batch in batches {
        let Some((program, args)) = batch.split_first() else {
            continue;
        };
        let output = command(program, args, root, &env);
        match output {
            Ok(output) => {
                let text = format_output(&output.stdout, &output.stderr);
                if !text.is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&text);
                }
                if !output.status.success() {
                    failed = true;
                    exit_code = output.status.code().unwrap_or(1);
                }
            }
            Err(err) => {
                failed = true;
                exit_code = 1;
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&err.to_string());
                combined.push('\n');
            }
        }
    }
    if !log_file.is_empty() {
        append_log(root, log_file, &combined)?;
    }
    Ok(LocalOutput {
        failed,
        exit_code,
        output: combined,
        invocations: batches.len(),
    })
}

fn command(
    program: &str,
    args: &[String],
    root: &Path,
    env: &[(OsString, OsString)],
) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        .env_clear()
        .stdin(std::process::Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    command.output()
}

fn format_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::new();
    if !stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(stdout));
    }
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    text
}

fn append_log(root: &Path, log_file: &str, text: &str) -> Result<()> {
    let path = join_inside(root, log_file)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::new(format!("failed to create log directory: {err}")))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| Error::new(format!("failed to open log file `{}`: {err}", log_file)))?;
    file.write_all(text.as_bytes())
        .map_err(|err| Error::new(format!("failed to write log file `{log_file}`: {err}")))?;
    Ok(())
}

const KEPT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TERM",
];

pub fn scrub_env<I, K, V>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut out = Vec::new();
    for (key, value) in vars {
        let key_os = key.as_ref();
        let Some(name) = key_os.to_str() else {
            continue;
        };
        if !kept_name(name) {
            continue;
        }
        out.push((key_os.to_os_string(), value.as_ref().to_os_string()));
    }
    out.push((OsString::from("PRE_COMMIT"), OsString::from("1")));
    out
}

fn kept_name(name: &str) -> bool {
    if !KEPT_ENV.contains(&name) {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("AWS_") || upper.starts_with("AZURE_") {
        return false;
    }
    if upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("KEY")
    {
        return false;
    }
    true
}

fn scrubbed_environ_size() -> usize {
    scrub_env(std::env::vars_os())
        .iter()
        .map(|(key, value)| key.len() + value.len() + 2)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_drops_secrets_and_sets_pre_commit() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/tmp/home"),
            ("CARGO_REGISTRY_TOKEN", "secret"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("AWS_PROFILE", "dev"),
            ("GITHUB_TOKEN", "secret"),
            ("MY_KEY", "secret"),
            ("AZURE_CLIENT_SECRET", "secret"),
            ("LANG", "C"),
        ];
        let scrubbed = scrub_env(vars);
        let names: Vec<String> = scrubbed
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"PATH".to_string()));
        assert!(names.contains(&"HOME".to_string()));
        assert!(names.contains(&"LANG".to_string()));
        assert!(names.contains(&"PRE_COMMIT".to_string()));
        assert!(!names.iter().any(|name| name.contains("TOKEN")
            || name.contains("SECRET")
            || name.contains("KEY")
            || name.starts_with("AWS_")
            || name.starts_with("AZURE_")));
        let pre_commit = scrubbed
            .iter()
            .find(|(key, _)| key == "PRE_COMMIT")
            .unwrap();
        assert_eq!(pre_commit.1, "1");
    }

    #[test]
    fn plan_appends_args_and_does_not_expand_dollars() {
        let root = std::env::temp_dir();
        let args = vec!["--check".to_string()];
        let files = vec!["src/lib.rs".to_string()];
        let request = LocalRequest {
            root: &root,
            entry: "echo $HOME",
            args: &args,
            files: &files,
            pass_filenames: true,
            require_serial: true,
            script: false,
            log_file: "",
        };
        let planned = plan(&request, Some(4096)).unwrap();
        assert_eq!(
            planned,
            vec![vec![
                "echo".to_string(),
                "$HOME".to_string(),
                "--check".to_string(),
                "src/lib.rs".to_string()
            ]]
        );
    }

    #[test]
    fn require_serial_uses_one_batch_when_the_line_fits() {
        let root = std::env::temp_dir();
        let files: Vec<String> = (0..9).map(|index| format!("f{index}.rs")).collect();
        let request = |serial: bool| LocalRequest {
            root: &root,
            entry: "tool",
            args: &[],
            files: &files,
            pass_filenames: true,
            require_serial: serial,
            script: false,
            log_file: "",
        };
        let serial = plan(&request(true), Some(4096)).unwrap();
        assert_eq!(serial.len(), 1);
        assert_eq!(serial[0].len(), 1 + files.len());
        let parallel = plan(&request(false), Some(30)).unwrap();
        assert!(parallel.len() > 1, "{parallel:?}");
    }

    #[test]
    fn pass_filenames_false_omits_files() {
        let root = std::env::temp_dir();
        let files = vec!["a.rs".to_string()];
        let request = LocalRequest {
            root: &root,
            entry: "cargo clippy --all-targets --all-features -- -Dclippy::all",
            args: &[],
            files: &files,
            pass_filenames: false,
            require_serial: false,
            script: false,
            log_file: "",
        };
        let planned = plan(&request, Some(4096)).unwrap();
        assert_eq!(planned.len(), 1);
        assert!(!planned[0].iter().any(|arg| arg.ends_with(".rs")));
        assert_eq!(planned[0][0], "cargo");
    }
}
