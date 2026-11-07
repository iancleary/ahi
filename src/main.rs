use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use ahi::directories::Directories;
use ahi::harness;
use ahi::hooks::audit::AuditEvent;
use ahi::hooks::check::{all_checks, builtin_by_id};
use ahi::hooks::config::discover_config;
use ahi::hooks::git::repo_root;
use ahi::hooks::output::render;
use ahi::hooks::runner::{self, Selection};
use ahi::hooks::store::{self, Store};
use ahi::hooks::{config, migrate, policy};
use ahi::VERSION;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ahi",
    version,
    about = "Fail-closed pre-commit runner with a terminal chat for OpenAI and compatible gateways."
)]
struct Cli {
    /// Path to a pre-commit config file.
    #[arg(long, short = 'c', global = true)]
    config: Option<PathBuf>,

    /// Cache directory for catalogs and objects. Overrides AHI_HOME and
    /// XDG_CACHE_HOME.
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,

    /// Keep config, data, state, and cache under one directory instead of the
    /// XDG layout. Overrides AHI_STATE_DIR.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,

    /// Color: auto, always, or never. v0.1 prints plain text either way.
    #[arg(long, global = true, default_value = "auto")]
    color: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run configured hooks.
    Run(RunArgs),
    /// List builtin checks.
    ListChecks {
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Explain one builtin check.
    Explain { id: String },
    /// Show the local environment. Sandboxes are not implemented in v0.1.
    Doctor,
    /// Git hook shim commands.
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Replace this ahi binary from the attested Linux archive of the latest release.
    ///
    /// Downloads and verifies `ahi-<version>-sha256sums.txt` and the archive for
    /// the target against their GitHub build-provenance attestations with the
    /// `gh` CLI, then atomically renames the new binary over the running one.
    /// Linux only; macOS and Windows stay on `cargo install`. Refuses a target
    /// it cannot verify. Does not accept a package name. The command is not
    /// named `self-update`.
    Update,
    /// Best-effort config suggestion. Does not guarantee feature parity or the same file scope.
    ///
    /// Prints a report and a suggested config. Does not fetch remotes and does not
    /// change `ahi run`. Writes only with `--write`, and refuses to replace an
    /// existing file unless `--force` is also set.
    Migrate {
        /// Write the suggested config over the file that was read.
        /// Refuses when that file exists unless `--force` is set.
        #[arg(long)]
        write: bool,
        /// Replace an existing file. Does nothing unless `--write` is also set.
        #[arg(long)]
        force: bool,
    },
    /// Pick only models curated in models.json. Startup performs no network requests.
    ///
    /// A terminal opens a picker; Enter saves a selection and opens the local
    /// echo-backed chat screen. A pipe prints providers and model catalog membership.
    Tui {
        /// Explicitly refresh reference catalogs before opening the picker or printing.
        #[arg(long)]
        refresh: bool,
        /// Tool policy file to use instead of ahi.tools.yaml in the working
        /// directory. It must exist and parse; rule paths and tool confinement
        /// stay relative to the working directory. See examples/tools.
        #[arg(long, value_name = "FILE")]
        tools: Option<PathBuf>,
    },
    /// Print the OpenAI browser login URL and wait for the redirect.
    ///
    /// Opening a browser is attempted and is not required. The credential is
    /// written under the state directory.
    Login,
    /// Open a shell in the opinionated development image with the working
    /// directory mounted at /work.
    ///
    /// Needs podman or docker on PATH. The image is published by
    /// iancleary/ahi-images; without a subcommand the image is pulled when it
    /// is missing and a shell opens. Only the working directory is mounted.
    Dev(DevArgs),
    /// Containment for hooks and tool calls. This version only detects what
    /// the host offers; nothing is confined yet.
    Sandbox {
        #[command(subcommand)]
        command: SandboxCommand,
    },
}

#[derive(Subcommand)]
enum SandboxCommand {
    /// Print the backend `ahi sandbox run` would use, whether an outer
    /// sandbox such as OpenShell is already active, and the evidence.
    Check,
}

#[derive(clap::Args)]
struct DevArgs {
    #[command(subcommand)]
    command: Option<DevCommand>,
    /// Pull or build the image again even when it is already present.
    #[arg(long, global = true)]
    rebuild: bool,
    /// Image reference. Overrides AHI_DEV_IMAGE, the repository's ahi.dev.yaml,
    /// and the `dev-image` file in the config directory.
    /// Default: ghcr.io/iancleary/ahi-dev.
    #[arg(long, global = true, value_name = "REF")]
    image: Option<String>,
    /// Image tag, such as latest, edge, or a dated tag. Overrides `tag` in the
    /// repository's ahi.dev.yaml. Default: latest. An image reference that
    /// already names a tag cannot be combined with this flag.
    #[arg(long, global = true, value_name = "TAG")]
    tag: Option<String>,
    /// Shell to run inside the container. Default: the repository's `shell` in ahi.dev.yaml, else the basename of SHELL, else /bin/zsh.
    #[arg(long, global = true, value_name = "CMD")]
    shell: Option<String>,
    /// Container runtime: a name on PATH or a path. Overrides AHI_DEV_RUNTIME.
    /// Default: podman, then docker.
    #[arg(long, global = true, value_name = "CMD")]
    runtime: Option<String>,
    /// Build from this local checkout of iancleary/ahi-images (its images/dev
    /// directory) instead of pulling, and tag the result with the image reference.
    #[arg(long, global = true, value_name = "DIR")]
    from: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DevCommand {
    /// Pull the image, or build it from --from.
    Build,
    /// Run a command in the image, pulling first when it is missing, and exit
    /// with its code. Everything after `--` is the command.
    Run {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "COMMAND"
        )]
        command: Vec<std::ffi::OsString>,
    },
    /// Open a shell in an image that is already present; never pulls.
    Shell,
    /// Show the runtime, the image reference and where it came from, and the local image id.
    Status,
    /// Remove the local image.
    Clean,
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Install the git hook shim. Not implemented.
    Install,
    /// Remove the git hook shim. Not implemented.
    Uninstall,
}

#[derive(clap::Args)]
struct RunArgs {
    /// Run only this hook id or alias.
    hook: Option<String>,
    /// Check every tracked file.
    #[arg(long, short = 'a')]
    all_files: bool,
    /// Check these paths instead of the staged set.
    #[arg(long, num_args = 1..)]
    files: Vec<String>,
    /// Together with --to-ref, check files changed between the two refs.
    #[arg(long)]
    from_ref: Option<String>,
    /// Together with --from-ref, check files changed between the two refs.
    #[arg(long)]
    to_ref: Option<String>,
    /// Git hook stage to select. Default: pre-commit.
    #[arg(long, default_value = "pre-commit")]
    hook_stage: String,
    /// text, json, or sarif.
    #[arg(long, default_value = "text")]
    format: String,
    /// Stop after the first failing hook.
    #[arg(long)]
    fail_fast: bool,
    /// Show output from a passing local hook. Failures always show output.
    #[arg(long, short = 'v')]
    verbose: bool,
    /// Rewrite end-of-file, trailing-whitespace, and mixed-line-ending.
    ///
    /// Other checks stay report-only. A hook argument `--fix` enables the same
    /// three checks. `--fix=no` on mixed-line-ending stays report-only.
    /// Writes are atomic, do not follow symlinks, and skip files outside the
    /// selected set. A file with unstaged changes is left unchanged.
    #[arg(long)]
    fix: bool,
    /// Commit-message path for commit-msg and prepare-commit-msg.
    #[arg(long)]
    commit_msg_filename: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if !matches!(cli.color.as_str(), "auto" | "always" | "never") {
        eprintln!("error: --color must be auto, always, or never");
        return ExitCode::from(2);
    }
    match &cli.command {
        Command::Run(args) => run(&cli, args),
        Command::ListChecks { format } => list_checks(format),
        Command::Explain { id } => explain(id),
        Command::Doctor => doctor(
            cli.config.as_deref(),
            cli.cache_dir.as_deref(),
            cli.state_dir.as_deref(),
        ),
        Command::Hooks { command } => match command {
            HooksCommand::Install | HooksCommand::Uninstall => {
                unimplemented_until("the hook shim phase (after hook execution exists)")
            }
        },
        Command::Update => match ahi::update::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::from(1)
            }
        },
        Command::Migrate { write, force } => migrate_config(cli.config.as_deref(), *write, *force),
        Command::Tui { refresh, tools } => tui(&cli, *refresh, tools.as_deref()),
        Command::Login => login(&cli),
        Command::Dev(args) => dev(&cli, args),
        Command::Sandbox {
            command: SandboxCommand::Check,
        } => {
            let report = ahi::sandbox::report(&ahi::sandbox::probe());
            for line in &report.lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
    }
}

fn dev(cli: &Cli, args: &DevArgs) -> ExitCode {
    use ahi::dev::{Action, Options};
    let action = match args.command {
        None => Action::Enter,
        Some(DevCommand::Build) => Action::Build,
        Some(DevCommand::Run { .. }) => Action::Run,
        Some(DevCommand::Shell) => Action::Shell,
        Some(DevCommand::Status) => Action::Status,
        Some(DevCommand::Clean) => Action::Clean,
    };
    let options = Options {
        image: args.image.clone(),
        tag: args.tag.clone(),
        shell: args.shell.clone(),
        runtime: args.runtime.clone(),
        rebuild: args.rebuild,
        from: args.from.clone(),
        command: match &args.command {
            Some(DevCommand::Run { command }) => command.clone(),
            _ => Vec::new(),
        },
    };
    let dirs = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    match ahi::dev::run(action, &options, &dirs) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn unimplemented_until(phase: &str) -> ExitCode {
    eprintln!("error: not implemented until {phase}");
    ExitCode::from(1)
}

fn run(cli: &Cli, args: &RunArgs) -> ExitCode {
    let modes = [
        args.all_files,
        !args.files.is_empty(),
        args.from_ref.is_some() || args.to_ref.is_some(),
    ]
    .into_iter()
    .filter(|on| *on)
    .count();
    if modes > 1 {
        eprintln!("error: use only one of --all-files, --files, or --from-ref/--to-ref");
        return ExitCode::from(2);
    }
    if args.from_ref.is_some() != args.to_ref.is_some() {
        eprintln!("error: Specify both --from-ref and --to-ref.");
        return ExitCode::from(1);
    }
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let root = match repo_root(&cwd) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let config_path = match discover_config(&root, cli.config.as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let config = match config::load_config(&config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let selection = if args.all_files {
        Selection::All
    } else if !args.files.is_empty() {
        match relativize_files(&root, &cwd, &args.files) {
            Ok(files) => Selection::Files(files),
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::from(1);
            }
        }
    } else if let (Some(from), Some(to)) = (&args.from_ref, &args.to_ref) {
        Selection::Range {
            from: from.clone(),
            to: to.clone(),
        }
    } else {
        Selection::Staged
    };
    let stage_fixes = matches!(selection, Selection::Staged);
    let mut skip = BTreeSet::new();
    if let Ok(raw) = env::var("SKIP") {
        for part in raw.split(',') {
            let part = part.trim();
            if !part.is_empty() {
                skip.insert(part.to_string());
            }
        }
    }
    let report = match runner::run(&runner::RunRequest {
        root: &root,
        config: &config,
        selection,
        stage: &args.hook_stage,
        only_hook: args.hook.as_deref(),
        fail_fast: args.fail_fast,
        skip,
        commit_msg_filename: args.commit_msg_filename.as_deref(),
        fix: args.fix,
        stage_fixes,
        verbose: args.verbose,
    }) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let rendered = match render(&report, &args.format) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    print!("{rendered}");
    {
        let ids: Vec<String> = report.hooks.iter().map(|hook| hook.id.clone()).collect();
        let findings = report.hooks.iter().map(|hook| hook.findings.len()).sum();
        let outcome = match report.result {
            runner::Outcome::Passed => "passed",
            runner::Outcome::Failed => "failed",
            runner::Outcome::Skipped => "skipped",
        };
        let local_env: Vec<ahi::hooks::audit::HookEnv<'_>> = report
            .local_env
            .iter()
            .map(|item| ahi::hooks::audit::HookEnv {
                hook: &item.hook,
                names: &item.names,
            })
            .collect();
        // The log is persistent state, not cache; a missing location only warns.
        let appended = directories(cli).and_then(|dirs| {
            ahi::hooks::audit::append(
                &dirs.state,
                &AuditEvent {
                    command: "run",
                    repo: &root,
                    hook_ids: &ids,
                    outcome,
                    finding_count: findings,
                    local_hooks: &report.local_hooks,
                    local_env: &local_env,
                },
            )
        });
        if let Err(err) = appended {
            eprintln!("warning: audit log not written: {err}");
        }
    }
    if report.result == runner::Outcome::Failed {
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}

fn list_checks(format: &str) -> ExitCode {
    match format {
        "text" => {
            for check in all_checks() {
                println!("{:<32} {}", check.id(), check.name());
                for alias in check.aliases() {
                    println!("  alias: {alias}");
                }
            }
            ExitCode::from(0)
        }
        "json" => {
            let checks: Vec<_> = all_checks()
                .iter()
                .map(|check| {
                    serde_json::json!({
                        "id": check.id(),
                        "name": check.name(),
                        "aliases": check.aliases(),
                        "description": check.description(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&checks).unwrap_or_else(|_| "[]".into())
            );
            ExitCode::from(0)
        }
        other => {
            eprintln!("error: unknown format `{other}`");
            ExitCode::from(2)
        }
    }
}

fn explain(id: &str) -> ExitCode {
    let Some(check) = builtin_by_id(id) else {
        eprintln!("error: no builtin check `{id}`");
        return ExitCode::from(1);
    };
    println!("{}", check.id());
    println!("{}", check.name());
    println!();
    println!("{}", check.description());
    println!();
    println!(
        "default types: {}",
        if check.default_types().is_empty() {
            "(none)".to_string()
        } else {
            check.default_types().join(", ")
        }
    );
    if !check.default_files().is_empty() {
        println!("default files: {}", check.default_files());
    }
    if !check.aliases().is_empty() {
        println!("aliases: {}", check.aliases().join(", "));
    }
    if let Some(upstream) = check.upstream() {
        println!("upstream hook: {upstream}");
    }
    if check.fixes() {
        println!(
            "fixer: report-only unless `ahi run --fix` or a hook argument `--fix` is set. The rewrite is atomic, does not follow symlinks, and only touches files in the selected set. ahi refuses the rewrite when that file has unstaged changes. There is no stash."
        );
    }
    if check.default_always_run() {
        println!("always_run: true when the config omits always_run");
    }
    if !check.default_pass_filenames() {
        println!("pass_filenames: false when the config omits pass_filenames");
    }
    println!("version: ahi {VERSION}");
    println!();
    let loaded = env::current_dir().ok().and_then(|cwd| {
        repo_root(&cwd)
            .ok()
            .and_then(|root| policy::load_policy(&root).ok())
    });
    print_local_env(loaded.as_ref());
    ExitCode::from(0)
}

fn print_local_env(policy: Option<&ahi::hooks::policy::Policy>) {
    println!(
        "local hook environment (names only, if set): {}",
        ahi::base_env_names().join(", ")
    );
    println!(
        "toolchain pass-through (default, if set): {}",
        ahi::default_toolchain_names().join(", ")
    );
    println!(
        "toolchain pass-through adds names a tool needs to find an install that is already there. ahi does not install, update, or cache a toolchain. Credential names (TOKEN, SECRET, PASSWORD, KEY, AWS_*, AZURE_*, GITHUB_*) are refused."
    );
    let Some(policy) = policy else {
        return;
    };
    if policy.toolchain_env.is_empty() {
        println!("toolchain pass-through (policy): (none)");
    } else {
        println!(
            "toolchain pass-through (policy): {}",
            policy.toolchain_env.join(", ")
        );
    }
    for rule in &policy.rules {
        if rule.toolchain_env.is_empty() {
            continue;
        }
        println!(
            "toolchain pass-through (rule {}): {}",
            rule.id,
            rule.toolchain_env.join(", ")
        );
    }
}

fn doctor(
    config: Option<&std::path::Path>,
    cache: Option<&std::path::Path>,
    state: Option<&std::path::Path>,
) -> ExitCode {
    println!("ahi {VERSION}");
    match ahi::hooks::git::git_version() {
        Ok(version) => println!("git: {version}"),
        Err(err) => println!("git: {err}"),
    }
    match env::current_dir()
        .and_then(|cwd| repo_root(&cwd).map_err(|err| std::io::Error::other(err.to_string())))
    {
        Ok(root) => {
            println!("root: {}", root.display());
            match discover_config(&root, config) {
                Ok(path) => println!("config: {}", path.display()),
                Err(err) => println!("config: {err}"),
            }
        }
        Err(err) => println!("root: {err}"),
    }
    match Directories::resolve(state, cache) {
        Ok(dirs) => {
            println!("models.json: {}", dirs.config.display());
            println!("data: {}", dirs.data.display());
            println!("state: {}", dirs.state.display());
            match Store::open(&dirs.cache) {
                Ok(store) => {
                    let mode = if store.is_writable() {
                        "writable"
                    } else {
                        "read-only"
                    };
                    println!("cache: {} ({mode})", store.root().display());
                }
                Err(err) => println!("cache: {} ({err})", dirs.cache.display()),
            }
            for path in ahi::directories::exposed_paths(&dirs) {
                println!(
                    "warning: {} is readable by other users; ahi expects 0700 directories and 0600 files",
                    path.display()
                );
            }
        }
        Err(err) => println!("directories: {err}"),
    }
    println!("scratch: {}", store::scratch_dir().display());
    match ahi::dev::resolve_runtime(None, env::var(ahi::dev::RUNTIME_ENV).ok().as_deref(), None) {
        Ok(runtime) => println!("container runtime: {}", runtime.display()),
        Err(_) => println!("container runtime: none (ahi dev needs podman or docker)"),
    }
    println!(
        "{}",
        ahi::sandbox::summary(&ahi::sandbox::report(&ahi::sandbox::probe()))
    );
    println!(
        "network: not enforced in this version (no sandbox); local hook environments are scrubbed"
    );
    match env::current_dir()
        .and_then(|cwd| repo_root(&cwd).map_err(|err| std::io::Error::other(err.to_string())))
    {
        Ok(root) => match policy::load_policy(&root) {
            Ok(loaded) => {
                let path = root.join(policy::FILE_NAME);
                if loaded.path_present {
                    println!("hooks policy: {}", path.display());
                } else {
                    println!("hooks policy: absent (deny by default)");
                }
                let approved = loaded.approved_hooks();
                let denied = loaded.rules.len() - approved;
                println!("hook rules: {approved} approve, {denied} deny");
                println!(
                    "local execution: system and script when an approve rule matches the hook id and the exact entry; other languages are refused"
                );
                println!(
                    "remote hooks: not executed and not fetched. Checkers are repo: builtin. ahi migrate can suggest a builtin id"
                );
                println!(
                    "fix: `ahi run --fix` rewrites end-of-file, trailing whitespace, and mixed line endings; other checks stay report-only"
                );
                print_local_env(Some(&loaded));
            }
            Err(err) => println!("hooks policy: {err}"),
        },
        Err(err) => println!("hooks policy: {err}"),
    }
    ExitCode::from(0)
}

fn directories(cli: &Cli) -> ahi::Result<Directories> {
    Directories::resolve(cli.state_dir.as_deref(), cli.cache_dir.as_deref())
}

fn tui(cli: &Cli, refresh: bool, actions: Option<&std::path::Path>) -> ExitCode {
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    match ahi::tui::run(&state, refresh, actions) {
        Ok(output) => {
            if !output.text.is_empty() {
                println!("{}", output.text);
            }
            ExitCode::from(u8::from(output.refresh_failed))
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn login(cli: &Cli) -> ExitCode {
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    match harness::login(&state) {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn migrate_config(config: Option<&std::path::Path>, write: bool, force: bool) -> ExitCode {
    if force && !write {
        eprintln!("error: --force does not write a file; pass --write --force to replace one");
        return ExitCode::from(2);
    }
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let root = match repo_root(&cwd) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let path = match discover_config(&root, config) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let original = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let suggestion = match migrate::suggest(&original, &path.display().to_string()) {
        Ok(suggestion) => suggestion,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    print!("{}", suggestion.report);
    print!("{}", suggestion.suggested);
    if !write {
        eprintln!("left {} unchanged", path.display());
        return ExitCode::from(0);
    }
    if path.exists() && !force {
        eprintln!(
            "error: refusing to overwrite {}; pass --write --force to replace it",
            path.display()
        );
        return ExitCode::from(1);
    }
    if let Err(err) = std::fs::write(&path, &suggestion.suggested) {
        eprintln!("error: {err}");
        return ExitCode::from(1);
    }
    eprintln!("wrote {}", path.display());
    ExitCode::from(0)
}

fn relativize_files(
    root: &std::path::Path,
    cwd: &std::path::Path,
    files: &[String],
) -> ahi::Result<Vec<String>> {
    let mut out = Vec::new();
    for file in files {
        let path = std::path::Path::new(file);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let absolute = absolute.canonicalize().unwrap_or(absolute);
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        match absolute.strip_prefix(&root) {
            Ok(rel) => out.push(rel.to_string_lossy().replace('\\', "/")),
            Err(_) => {
                return Err(ahi::Error::new(format!(
                    "path `{file}` is outside the repository"
                )));
            }
        }
    }
    Ok(out)
}
