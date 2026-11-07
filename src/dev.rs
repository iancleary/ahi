//! `ahi dev`: pull or build the opinionated development image and open a
//! shell in it with the working directory mounted.
//!
//! The image is defined and published by `iancleary/ahi-images`; this module
//! only references, pulls, builds, inspects, removes, and runs it through a
//! container runtime (`podman` or `docker`) found on `PATH`. Nothing here
//! reads a Containerfile: `build` pulls the published image unless `--from`
//! names a local checkout of `ahi-images`, in which case it builds
//! `images/dev` there and tags the result with the image reference.
//!
//! Only the working directory is mounted, at `/work`. The ahi directories,
//! the home directory, and the container socket stay outside.
//!
//! A repository may pin its image in `ahi.dev.yaml` in the working directory.
//! The file is a preference, not an allowlist: it grants nothing, since the
//! user still runs `ahi dev` and the container still sees only the working
//! directory.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use serde::Deserialize;

use crate::directories::Directories;
use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

/// The image the `ahi-images` repository publishes.
pub const DEFAULT_IMAGE: &str = "ghcr.io/iancleary/ahi-dev:latest";
/// Environment variable that overrides the image reference.
pub const IMAGE_ENV: &str = "AHI_DEV_IMAGE";
/// Environment variable that names the container runtime.
pub const RUNTIME_ENV: &str = "AHI_DEV_RUNTIME";
/// File under the config directory with one image reference per line
/// (the first non-empty, non-`#` line wins).
pub const IMAGE_FILE: &str = "dev-image";
/// Repository-level preferences, read from the working directory.
pub const REPO_FILE_NAME: &str = "ahi.dev.yaml";
/// Where the working directory appears inside the container.
pub const MOUNT_POINT: &str = "/work";
/// Shell used when neither `--shell`, `ahi.dev.yaml`, nor `SHELL` says
/// otherwise. zsh is the login shell of the ahi-dev images.
pub const DEFAULT_SHELL: &str = "/bin/zsh";
/// Runtimes probed on `PATH`, in order.
pub const RUNTIMES: [&str; 2] = ["podman", "docker"];

/// What `ahi dev` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Ensure the image exists, then open a shell (the default).
    Enter,
    /// Ensure the image exists, then run `Options::command` in it.
    Run,
    /// Pull the image, or build it from a local `ahi-images` checkout.
    Build,
    /// Open a shell in an image that is already present.
    Shell,
    /// Print the runtime, the image reference and its source, and the image id
    /// and creation time when present.
    Status,
    /// Remove the local image.
    Clean,
}

/// Flags shared by every `ahi dev` action.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `--image`.
    pub image: Option<String>,
    /// `--shell`.
    pub shell: Option<String>,
    /// `--runtime`: a name looked up on `PATH` or a path.
    pub runtime: Option<String>,
    /// `--rebuild`: pull or build again even when the image is present.
    pub rebuild: bool,
    /// `--from`: a local `ahi-images` checkout to build from.
    pub from: Option<PathBuf>,
    /// `ahi dev run -- <command>`: argv passed through untouched.
    pub command: Vec<OsString>,
}

/// Where the image reference came from, for `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSource {
    Flag,
    Environment,
    RepoFile,
    ConfigFile,
    Default,
}

impl ImageSource {
    fn label(self) -> &'static str {
        match self {
            ImageSource::Flag => "--image",
            ImageSource::Environment => IMAGE_ENV,
            ImageSource::RepoFile => REPO_FILE_NAME,
            ImageSource::ConfigFile => IMAGE_FILE,
            ImageSource::Default => "default",
        }
    }
}

/// `ahi.dev.yaml`: the repository's preferences for `ahi dev`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoPreferences {
    pub version: u32,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}

/// Parse `ahi.dev.yaml`. Unknown keys are errors, like every ahi file.
pub fn parse_repo_preferences(src: &str, origin: &str) -> Result<RepoPreferences> {
    check_config_yaml(src, origin)?;
    let prefs: RepoPreferences =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    if prefs.version != 1 {
        return Err(Error::new(format!(
            "{origin}: {REPO_FILE_NAME} version {} is not supported (expected 1)",
            prefs.version
        )));
    }
    Ok(prefs)
}

/// Load `ahi.dev.yaml` from `directory`. Absence is the empty preference set;
/// a present file that does not parse is an error.
pub fn load_repo_preferences(directory: &Path) -> Result<RepoPreferences> {
    let path = directory.join(REPO_FILE_NAME);
    if !path.is_file() {
        return Ok(RepoPreferences::default());
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    parse_repo_preferences(&text, &path.display().to_string())
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Resolve the image reference: flag, then environment, then the
/// repository's `ahi.dev.yaml`, then the user's config file, then the
/// default. Empty values at any level are skipped.
pub fn resolve_image(
    flag: Option<&str>,
    environment: Option<&str>,
    repo: Option<&str>,
    config_file: Option<&str>,
) -> (String, ImageSource) {
    if let Some(image) = clean(flag) {
        return (image.to_string(), ImageSource::Flag);
    }
    if let Some(image) = clean(environment) {
        return (image.to_string(), ImageSource::Environment);
    }
    if let Some(image) = clean(repo) {
        return (image.to_string(), ImageSource::RepoFile);
    }
    if let Some(image) = config_file.and_then(first_reference) {
        return (image, ImageSource::ConfigFile);
    }
    (DEFAULT_IMAGE.to_string(), ImageSource::Default)
}

/// The first non-empty, non-comment line of the config file.
fn first_reference(contents: &str) -> Option<String> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}

/// Resolve the shell: `--shell`, else the repository's `ahi.dev.yaml`, else
/// the basename of `SHELL`, else `/bin/zsh`. The basename is used because
/// the host path (for example `/opt/homebrew/bin/zsh`) does not exist in the
/// image, while the image puts `bash` and `zsh` on `PATH`.
pub fn resolve_shell(flag: Option<&str>, repo: Option<&str>, shell_env: Option<&str>) -> String {
    if let Some(shell) = clean(flag).or(clean(repo)) {
        return shell.to_string();
    }
    shell_env
        .map(Path::new)
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_SHELL.to_string())
}

/// Pick the runtime: the flag, `AHI_DEV_RUNTIME`, or the repository's
/// `ahi.dev.yaml` when set (a name on `PATH` or a path), else the first of
/// `podman` and `docker` found on `PATH`.
pub fn resolve_runtime(
    flag: Option<&str>,
    environment: Option<&str>,
    repo: Option<&str>,
) -> Result<PathBuf> {
    let requested = clean(flag).or(clean(environment)).or(clean(repo));
    if let Some(name) = requested {
        return find_executable(name).ok_or_else(|| {
            Error::new(format!(
                "container runtime `{name}` was not found on PATH; install podman or docker, or pass --runtime <path>"
            ))
        });
    }
    RUNTIMES
        .iter()
        .find_map(|name| find_executable(name))
        .ok_or_else(|| {
            Error::new(
                "no container runtime found; ahi dev needs podman or docker on PATH (or --runtime <path>)",
            )
        })
}

/// Look a command up on `PATH`, or accept a path that names an executable.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .flat_map(|dir| executable_names(name).map(move |n| dir.join(n)))
        .find(|path| path.is_file())
}

#[cfg(windows)]
fn executable_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let bare = Path::new(name).extension().is_some();
    let mut names = vec![name.to_string()];
    if !bare {
        names.push(format!("{name}.exe"));
    }
    names.into_iter()
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> impl Iterator<Item = String> + '_ {
    std::iter::once(name.to_string())
}

/// Arguments for `<runtime> run ...` that run `command` in `image` with
/// `workdir` mounted at [`MOUNT_POINT`]. `-t` is added only when stdin is a
/// terminal so a pipe does not make the runtime refuse to start. On Unix the
/// host uid and gid are passed as `AHI_HOST_UID` and `AHI_HOST_GID` for an
/// entrypoint that remaps its user; an image that ignores them is unchanged.
pub fn run_arguments(
    image: &str,
    command: &[OsString],
    workdir: &Path,
    tty: bool,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["run".into(), "--rm".into(), "-i".into()];
    if tty {
        args.push("-t".into());
    }
    args.push("--volume".into());
    let mut volume = workdir.as_os_str().to_os_string();
    volume.push(format!(":{MOUNT_POINT}"));
    args.push(volume);
    args.push("--workdir".into());
    args.push(MOUNT_POINT.into());
    if let Ok(term) = env::var("TERM") {
        if !term.is_empty() {
            args.push("--env".into());
            args.push(format!("TERM={term}").into());
        }
    }
    for (name, value) in host_identity(workdir) {
        args.push("--env".into());
        args.push(format!("{name}={value}").into());
    }
    args.push(image.into());
    args.extend(command.iter().cloned());
    args
}

/// The owner of the mounted directory, which is the identity the mounted tree
/// expects. It is the process owner in every ordinary case and needs no libc call.
#[cfg(unix)]
fn host_identity(workdir: &Path) -> Vec<(&'static str, u32)> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(workdir) {
        Ok(meta) => vec![("AHI_HOST_UID", meta.uid()), ("AHI_HOST_GID", meta.gid())],
        Err(_) => Vec::new(),
    }
}

#[cfg(not(unix))]
fn host_identity(_workdir: &Path) -> Vec<(&'static str, u32)> {
    Vec::new()
}

/// Resolved settings for one invocation.
pub struct Session {
    pub runtime: PathBuf,
    pub image: String,
    pub source: ImageSource,
    pub shell: String,
    pub workdir: PathBuf,
}

impl Session {
    /// Resolve everything from the flags, the environment, and the config
    /// directory. Does not touch the runtime yet.
    pub fn resolve(options: &Options, dirs: &Directories) -> Result<Self> {
        let workdir = env::current_dir()
            .map_err(|err| Error::new(format!("failed to read the working directory: {err}")))?;
        let repo = load_repo_preferences(&workdir)?;
        let runtime = resolve_runtime(
            options.runtime.as_deref(),
            env::var(RUNTIME_ENV).ok().as_deref(),
            repo.runtime.as_deref(),
        )?;
        let config_file = fs::read_to_string(dirs.config.join(IMAGE_FILE)).ok();
        let (image, source) = resolve_image(
            options.image.as_deref(),
            env::var(IMAGE_ENV).ok().as_deref(),
            repo.image.as_deref(),
            config_file.as_deref(),
        );
        let shell = resolve_shell(
            options.shell.as_deref(),
            repo.shell.as_deref(),
            env::var("SHELL").ok().as_deref(),
        );
        Ok(Self {
            runtime,
            image,
            source,
            shell,
            workdir,
        })
    }

    fn command(&self) -> Command {
        Command::new(&self.runtime)
    }

    /// Whether the image is present locally.
    pub fn image_present(&self) -> Result<bool> {
        let status = self
            .command()
            .args(["image", "inspect", &self.image])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| self.spawn_error(err))?;
        Ok(status.success())
    }

    /// Pull the image, or build it from `from/images/dev` when given.
    pub fn build(&self, from: Option<&Path>, rebuild: bool) -> Result<()> {
        let status = match from {
            Some(checkout) => {
                let context = checkout.join("images").join("dev");
                let containerfile = context.join("Containerfile");
                if !containerfile.is_file() {
                    return Err(Error::new(format!(
                        "{} is not an ahi-images checkout: {} is missing",
                        checkout.display(),
                        containerfile.display()
                    )));
                }
                println!("Building {} from {}...", self.image, context.display());
                let mut command = self.command();
                command.args(["build", "--file"]).arg(&containerfile);
                if rebuild {
                    command.args(["--no-cache", "--pull"]);
                }
                command.args(["--tag", &self.image]).arg(&context);
                command.status()
            }
            None => {
                println!("Pulling {}...", self.image);
                self.command().args(["pull", &self.image]).status()
            }
        }
        .map_err(|err| self.spawn_error(err))?;
        check(status, "pull or build")
    }

    /// Ensure the image exists, pulling when it is missing or `rebuild` is set.
    pub fn ensure(&self, options: &Options) -> Result<()> {
        if options.rebuild || options.from.is_some() || !self.image_present()? {
            self.build(options.from.as_deref(), options.rebuild)?;
        }
        Ok(())
    }

    /// Run the shell interactively and return its exit status.
    pub fn shell(&self) -> Result<ExitStatus> {
        self.exec(&[OsString::from(&self.shell)])
    }

    /// Run `command` in the image with stdio inherited and return its exit
    /// status. The image must already be present.
    pub fn exec(&self, command: &[OsString]) -> Result<ExitStatus> {
        if !self.image_present()? {
            return Err(Error::new(format!(
                "image {} is not present; run `ahi dev build` or `ahi dev` first",
                self.image
            )));
        }
        let tty = std::io::stdin().is_terminal();
        let args = run_arguments(&self.image, command, &self.workdir, tty);
        self.command()
            .args(&args)
            .status()
            .map_err(|err| self.spawn_error(err))
    }

    /// Print the resolved settings and the image state.
    pub fn status(&self) -> Result<()> {
        println!("runtime: {}", self.runtime.display());
        println!("image: {} (from {})", self.image, self.source.label());
        println!("shell: {}", self.shell);
        println!("mount: {} -> {MOUNT_POINT}", self.workdir.display());
        let output = self
            .command()
            .args([
                "image",
                "inspect",
                "--format",
                "{{.Id}}\t{{.Created}}",
                &self.image,
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|err| self.spawn_error(err))?;
        if !output.status.success() {
            println!("local image: not present (run `ahi dev build`)");
            return Ok(());
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut fields = text.trim().splitn(2, '\t');
        let id = fields.next().unwrap_or("").trim();
        let created = fields.next().unwrap_or("").trim();
        println!("local image: {id}");
        if !created.is_empty() {
            println!("created: {created}");
        }
        Ok(())
    }

    /// Remove the local image.
    pub fn clean(&self) -> Result<()> {
        if !self.image_present()? {
            println!("image {} is not present", self.image);
            return Ok(());
        }
        let status = self
            .command()
            .args(["image", "rm", &self.image])
            .status()
            .map_err(|err| self.spawn_error(err))?;
        check(status, "image removal")
    }

    fn spawn_error(&self, err: std::io::Error) -> Error {
        Error::new(format!("failed to run {}: {err}", self.runtime.display()))
    }
}

fn check(status: ExitStatus, what: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!("{what} failed ({status})")))
    }
}

/// Run `ahi dev`. Returns the exit code the process should use: the child's
/// own code for `Enter`, `Shell`, and `Run`, 0 otherwise.
pub fn run(action: Action, options: &Options, dirs: &Directories) -> Result<i32> {
    let session = Session::resolve(options, dirs)?;
    match action {
        Action::Build => session.build(options.from.as_deref(), options.rebuild)?,
        Action::Status => session.status()?,
        Action::Clean => session.clean()?,
        Action::Enter | Action::Shell => {
            if action == Action::Enter {
                session.ensure(options)?;
            }
            let status = session.shell()?;
            return Ok(status.code().unwrap_or(1));
        }
        Action::Run => {
            if options.command.is_empty() {
                return Err(Error::new(
                    "ahi dev run needs a command after `--`, for example `ahi dev run -- cargo test`",
                ));
            }
            session.ensure(options)?;
            let status = session.exec(&options.command)?;
            return Ok(status.code().unwrap_or(1));
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_reference_follows_flag_env_file_default() {
        assert_eq!(
            resolve_image(Some("a:1"), Some("b:2"), Some("r:0"), Some("c:3")),
            ("a:1".to_string(), ImageSource::Flag)
        );
        assert_eq!(
            resolve_image(None, Some("b:2"), Some("r:0"), Some("c:3")),
            ("b:2".to_string(), ImageSource::Environment)
        );
        assert_eq!(
            resolve_image(None, None, Some("r:0"), Some("c:3")),
            ("r:0".to_string(), ImageSource::RepoFile)
        );
        assert_eq!(
            resolve_image(None, None, None, Some("# comment\n\n  c:3 \nd:4\n")),
            ("c:3".to_string(), ImageSource::ConfigFile)
        );
        assert_eq!(
            resolve_image(None, None, None, None),
            (DEFAULT_IMAGE.to_string(), ImageSource::Default)
        );
    }

    #[test]
    fn empty_overrides_fall_through() {
        assert_eq!(
            resolve_image(Some("  "), Some(""), Some(" "), Some("#only comments\n")),
            (DEFAULT_IMAGE.to_string(), ImageSource::Default)
        );
    }

    #[test]
    fn the_shell_uses_the_basename_of_shell_then_zsh() {
        assert_eq!(
            resolve_shell(Some("fish"), Some("bash"), Some("/bin/zsh")),
            "fish"
        );
        assert_eq!(resolve_shell(None, Some("bash"), Some("/bin/zsh")), "bash");
        assert_eq!(
            resolve_shell(None, None, Some("/opt/homebrew/bin/zsh")),
            "zsh"
        );
        assert_eq!(resolve_shell(None, None, Some("")), DEFAULT_SHELL);
        assert_eq!(resolve_shell(None, None, None), DEFAULT_SHELL);
    }

    #[test]
    fn run_arguments_mount_only_the_working_directory() {
        let command: Vec<OsString> = vec!["cargo".into(), "test".into(), "--all".into()];
        let args = run_arguments("img:tag", &command, Path::new("/repo"), false);
        assert_eq!(&args[..3], ["run", "--rm", "-i"]);
        assert!(!args.contains(&OsString::from("-t")));
        let volumes: Vec<&OsString> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(flag, _)| *flag == "--volume")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(volumes, [&OsString::from(format!("/repo:{MOUNT_POINT}"))]);
        assert_eq!(
            &args[args.len() - 4..],
            ["img:tag", "cargo", "test", "--all"]
        );
        let position = args.iter().position(|a| a == "--workdir").unwrap();
        assert_eq!(args[position + 1], MOUNT_POINT);
        let tty = run_arguments("img:tag", &command, Path::new("/repo"), true);
        assert_eq!(&tty[..4], ["run", "--rm", "-i", "-t"]);
        if cfg!(unix) {
            let real = run_arguments("img:tag", &command, &env::temp_dir(), false);
            assert!(real
                .iter()
                .any(|a| a.to_string_lossy().starts_with("AHI_HOST_UID=")));
            assert!(!args
                .iter()
                .any(|a| a.to_string_lossy().starts_with("AHI_HOST_UID=")));
        }
    }

    #[test]
    fn a_missing_requested_runtime_is_an_error_and_a_path_is_accepted() {
        let err = resolve_runtime(Some("ahi-no-such-runtime-xyz"), None, None).unwrap_err();
        assert!(err.to_string().contains("ahi-no-such-runtime-xyz"));
        let dir = crate::fixture_directory::create_unique_fixture_directory("dev-runtime");
        let fake = dir.join("fake-runtime");
        fs::write(&fake, b"").unwrap();
        assert_eq!(
            resolve_runtime(None, Some(fake.to_str().unwrap()), None).unwrap(),
            fake
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_repo_file_rejects_unknown_keys_and_other_versions() {
        let prefs = parse_repo_preferences(
            "version: 1\nimage: example.invalid/dev:1\nshell: zsh\n",
            "ahi.dev.yaml",
        )
        .unwrap();
        assert_eq!(prefs.image.as_deref(), Some("example.invalid/dev:1"));
        assert_eq!(prefs.shell.as_deref(), Some("zsh"));
        assert_eq!(prefs.runtime, None);
        let err = parse_repo_preferences("version: 1\nmount: /x\n", "ahi.dev.yaml")
            .unwrap_err()
            .to_string();
        assert!(err.contains("mount"), "{err}");
        let err = parse_repo_preferences("version: 2\n", "ahi.dev.yaml")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected 1"), "{err}");
    }
}
