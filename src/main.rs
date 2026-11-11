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

const GLOBAL: &str = "Global options";

#[derive(Parser)]
#[command(
    name = "ahi",
    version,
    about = "Offline pre-commit runner, terminal chat, and development container launcher.",
    long_about = "Offline pre-commit runner, terminal chat, and development container launcher.\n\n\
        Hooks run only from this binary or from an approved local command; nothing is fetched or \
        installed by `ahi run`. Repository files: .pre-commit-config.yaml (hooks), ahi.hooks.yaml \
        (which local hooks may run), ahi.tools.yaml (chat tool calls), ahi.dev.yaml (dev image).",
    after_help = "Start with `ahi doctor`, then `ahi run`. Each command has `--help`."
)]
struct Cli {
    /// Pre-commit config to read instead of .pre-commit-config.yaml in the git root.
    #[arg(long, short = 'c', global = true, value_name = "PATH", help_heading = GLOBAL)]
    config: Option<PathBuf>,

    /// Cache directory for catalogs and objects. Outranks AHI_HOME and XDG_CACHE_HOME.
    #[arg(long, global = true, value_name = "DIR", help_heading = GLOBAL)]
    cache_dir: Option<PathBuf>,

    /// One directory for config, data, state, and cache instead of the XDG layout. Outranks AHI_STATE_DIR.
    #[arg(long, global = true, value_name = "DIR", help_heading = GLOBAL)]
    state_dir: Option<PathBuf>,

    /// When to color output. `auto` colors a terminal unless NO_COLOR is set. Only `ahi tools init` uses color so far; other output is plain text.
    #[arg(
        long,
        global = true,
        default_value = "auto",
        value_name = "WHEN",
        value_parser = ["auto", "always", "never"],
        help_heading = GLOBAL
    )]
    color: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the configured hooks on the staged files.
    ///
    /// Builtin checks run in this process. A `repo: local` system or script
    /// hook runs only when a rule in ahi.hooks.yaml approves its id and exact
    /// entry. Remote hooks are never fetched or executed.
    Run(RunArgs),
    /// List the builtin checks with their aliases.
    ListChecks {
        /// Output format.
        #[arg(long, default_value = "text", value_name = "FORMAT", value_parser = ["text", "json"])]
        format: String,
    },
    /// Describe one builtin check: what it looks at, its defaults, and the upstream hook it replaces.
    Explain {
        /// Builtin id or alias, as `ahi list-checks` prints it.
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Show the local environment: versions, config, directories, hook policy, runtime, sandbox.
    Doctor,
    /// Install or remove the git hook shim (not implemented yet).
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Replace this binary with the latest release, verified by its build attestation (Linux only).
    ///
    /// Downloads `ahi-<version>-sha256sums.txt` and the archive for this target,
    /// verifies both against their GitHub build-provenance attestations with the
    /// `gh` CLI, then atomically renames the new binary over the running one.
    /// Refuses a target it cannot verify. macOS and Windows use `cargo install
    /// ahi --locked` instead.
    Update,
    /// Suggest an ahi config from the pre-commit config and print it (writes only with --write).
    ///
    /// Prints a report, the ahi.hooks.yaml rules the local hooks need, and a
    /// suggested .pre-commit-config.yaml. Best effort: feature parity and file
    /// scope are not guaranteed, remote repositories are not fetched, and `ahi
    /// run` does not use the mapping. Not upstream `migrate-config`.
    Migrate {
        /// Write the suggestion over the config that was read. Refuses an existing file without --force.
        #[arg(long)]
        write: bool,
        /// With --write, replace an existing file.
        #[arg(long, requires = "write")]
        force: bool,
    },
    /// Pick a curated model and open the terminal chat.
    ///
    /// Only models curated in models.json are offered; startup makes no network
    /// requests. In a terminal, Enter saves the selection and opens the chat.
    /// In a pipe, the providers and catalog membership are printed instead.
    Tui {
        /// Refresh the reference catalogs first.
        #[arg(long)]
        refresh: bool,
        /// Tool policy file to use instead of ahi.tools.yaml in the working directory (see examples/tools).
        #[arg(long, value_name = "FILE")]
        tools: Option<PathBuf>,
    },
    /// Run one model turn without the chat screen and print the answer.
    ///
    /// The prompt comes from `-p <text>`, `--prompt-file <path>`, or stdin
    /// when neither is given and stdin is a pipe. `--model` names a curated
    /// `gateway/model-id` from models.json; nothing is fetched, no picker
    /// opens, and a missing credential points at `ahi login`. stdout carries
    /// only the final answer (`--format json`: the result object), written
    /// once after a validated success; diagnostics go to stderr.
    ///
    /// `--result-file <path>` writes one JSON object on every outcome
    /// (status, text, error, usage, tool counts, a ledger of every write and
    /// command with hashes and exit codes, and `effects`: none, some, or
    /// unknown). The file must not exist. `--headless` is the worker
    /// contract a coordinator drives: the prompt comes from `--prompt-file`
    /// only, `--result-file` is required, tools come only from `--tools`,
    /// and `ask` rules are answered as denials.
    ///
    /// Tools are confined to `--workdir` (default: the current directory)
    /// and governed by its ahi.tools.yaml or the `--tools` file, exactly as
    /// in `ahi tui`. When any tool is advertised, the prompt, result, and
    /// policy files must lie outside the working directory. Exit codes: 0
    /// success, 1 turn failure, 2 usage, 3 models.json or model, 4
    /// credential, 5 policy file, 6 timeout or signal (SIGINT, SIGTERM,
    /// SIGHUP write the result as `cancelled` first), 8 result file not
    /// written. See docs/HEADLESS_EXEC.md.
    Exec(ExecArgs),
    /// Show or write the chat tool policy (ahi.tools.yaml) for the working directory; show the shipped examples.
    ///
    /// Tools are off until the directory `ahi tui` starts in has an
    /// ahi.tools.yaml with an approve rule. `init` prints a shipped policy and
    /// writes it only with --write, like `ahi migrate`.
    ///
    /// Every builtin tool works inside that working directory. Rule paths
    /// are relative to it; absolute paths, `..`, and symbolic links are
    /// refused before any read, write, or command, and `run_command` starts
    /// only in a directory inside it. A rule's `path` narrows access within
    /// the directory and never widens it.
    ///
    /// This is not a sandbox. A program approved by a command rule runs on
    /// the host with the operator's access: it can follow symbolic links,
    /// read files above the working directory, and run other programs. The
    /// read, list, find, search, edit, and write tools cannot. Approve
    /// commands accordingly, or run `ahi tui` inside `ahi dev run` for a
    /// container boundary.
    Tools {
        #[command(subcommand)]
        command: ToolsCommand,
    },
    /// Sign in to OpenAI through the browser and store the credential.
    ///
    /// Prints the login URL, tries to open a browser, and waits for the
    /// redirect. The credential is written under the data directory.
    Login,
    /// Open a shell, or run a command, in the development image with the working directory at /work.
    ///
    /// Needs podman or docker on PATH. The image comes from iancleary/ahi-images.
    /// Without a subcommand the image is pulled when missing and a shell opens.
    /// Only the working directory is mounted; this is a reproducible shell, not
    /// a security boundary.
    Dev(DevArgs),
    /// Report the containment this host offers (detection only; nothing is confined yet).
    Sandbox {
        #[command(subcommand)]
        command: SandboxCommand,
    },
}

#[derive(Subcommand)]
enum SandboxCommand {
    /// Print the backend `ahi sandbox run` would use, whether an outer sandbox such as OpenShell is active, and the evidence.
    Check,
}

#[derive(clap::Args)]
struct DevArgs {
    #[command(subcommand)]
    command: Option<DevCommand>,
    /// Image name without a tag. Outranks AHI_DEV_IMAGE, `image` in ahi.dev.yaml, and ~/.config/ahi/dev-image [default: ghcr.io/iancleary/ahi-dev]
    #[arg(long, global = true, value_name = "REF")]
    image: Option<String>,
    /// Image tag: latest, edge, or a dated tag. Outranks `tag` in ahi.dev.yaml. Not allowed when the image already has a tag [default: latest]
    #[arg(long, global = true, value_name = "TAG")]
    tag: Option<String>,
    /// Shell to run in the container. Outranks `shell` in ahi.dev.yaml and the basename of SHELL [default: /bin/zsh]
    #[arg(long, global = true, value_name = "CMD")]
    shell: Option<String>,
    /// Container runtime, a name on PATH or a path. Outranks AHI_DEV_RUNTIME and `runtime` in ahi.dev.yaml [default: podman, then docker]
    #[arg(long, global = true, value_name = "CMD")]
    runtime: Option<String>,
    /// Pull or build again even when the image is present.
    #[arg(long, global = true)]
    rebuild: bool,
    /// Build images/dev from this local iancleary/ahi-images checkout instead of pulling.
    #[arg(long, global = true, value_name = "DIR")]
    from: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DevCommand {
    /// Pull the image, or build it from --from.
    Build,
    /// Run a command in the image and exit with its code; pulls first when the image is missing.
    ///
    /// Everything after `--` is the command, passed through untouched:
    /// `ahi dev run -- cargo test --all-features`.
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
    /// Show the runtime, the image and where each part of it came from, the mount, and the local image id.
    Status,
    /// Remove the local image.
    Clean,
}

#[derive(clap::Args)]
struct ExecArgs {
    /// Curated model as `gateway/model-id` (`ahi tui` saves the same form).
    #[arg(long, value_name = "GATEWAY/MODEL")]
    model: String,
    /// The prompt, inline. Not allowed with --headless.
    #[arg(
        short = 'p',
        long = "prompt",
        value_name = "TEXT",
        conflicts_with = "prompt_file",
        help_heading = "Input"
    )]
    prompt: Option<String>,
    /// Read the prompt from this UTF-8 file, verbatim (at most 1 MiB).
    #[arg(long, value_name = "PATH", help_heading = "Input")]
    prompt_file: Option<PathBuf>,
    /// Hex SHA-256 the prompt must hash to; a mismatch is a usage error before any request.
    #[arg(long, value_name = "HEX", help_heading = "Input")]
    prompt_sha256: Option<String>,
    /// Tool root, and where ahi.tools.yaml is looked up without --tools. Defaults to the current directory.
    #[arg(long, value_name = "DIR", help_heading = "Tools")]
    workdir: Option<PathBuf>,
    /// Policy file to read instead of the working directory's ahi.tools.yaml. Headless runs advertise no tool without it.
    #[arg(long, value_name = "PATH", help_heading = "Tools")]
    tools: Option<PathBuf>,
    /// Validate arguments, files, policy, model, and credential, then exit without calling the model.
    #[arg(long)]
    check: bool,
    /// Print one progress line per tool call on stderr (headless runs always do).
    #[arg(long, short = 'v', help_heading = "Output")]
    verbose: bool,
    /// Seconds for the whole run: the model, every tool call, every wait. Past it the turn is cancelled, the result says `timeout`, and the exit code is 6.
    #[arg(long, value_name = "SECS", default_value_t = ahi::chat::exec::DEFAULT_TIMEOUT_SECS, help_heading = "Limits")]
    timeout: u64,
    /// Reasoning level sent to the model.
    #[arg(long, default_value = "off", value_name = "LEVEL", value_parser = ["off", "minimal", "low", "medium", "high"])]
    thinking: String,
    /// What stdout carries after a success: the answer, or the result object.
    #[arg(long, default_value = "text", value_name = "FORMAT", value_parser = ["text", "json"], help_heading = "Output")]
    format: String,
    /// Write the result object here on every outcome. The file must not exist.
    #[arg(long, value_name = "PATH", help_heading = "Output")]
    result_file: Option<PathBuf>,
    /// Worker mode for a coordinator: prompt from --prompt-file only, --result-file required, nothing read from stdin.
    #[arg(long, requires = "prompt_file", requires = "result_file")]
    headless: bool,
    /// Caller-chosen identifier echoed into the result (1-64 letters, digits, '-', '_', '.'). Generated when omitted.
    #[arg(long, value_name = "ID")]
    run_id: Option<String>,
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Install the git hook shim (not implemented yet; exits 1).
    Install,
    /// Remove the git hook shim (not implemented yet; exits 1).
    Uninstall,
}

#[derive(Subcommand)]
enum ToolsCommand {
    /// Print a shipped policy for the working directory; write it with --write (see `ahi tools examples list`).
    ///
    /// In a terminal each rule is painted with the chat's colors for its
    /// outcome: approve green, ask gray, deny red (`--color never` for plain
    /// text; the file is always written plain). `ahi tools list` shows the
    /// policy in place the same way. Rule paths confine the builtin tools
    /// to the working directory; an approved command is not confined. Rules confine the model to
    /// the working directory; none can reach above it.
    Init {
        /// Which shipped policy; `ahi tools examples list` names them.
        #[arg(long, value_name = "NAME", default_value = ahi::tools::DEFAULT_EXAMPLE)]
        example: String,
        /// Write ahi.tools.yaml in the working directory. Refuses an existing file without --force.
        #[arg(long)]
        write: bool,
        /// With --write, replace an existing file.
        #[arg(long, requires = "write")]
        force: bool,
    },
    /// Show the working directory's ahi.tools.yaml, painted by outcome, with what it approves.
    List,
    /// The shipped policies: `list` names them, `show [<name>]` prints them painted by outcome.
    Examples {
        #[command(subcommand)]
        command: Option<ExamplesCommand>,
    },
}

#[derive(Subcommand)]
enum ExamplesCommand {
    /// Name each shipped policy with what it lets the model do (the default).
    List,
    /// Print every shipped policy painted by outcome, or one by name.
    Show {
        /// One example to print; omitted, all are printed.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Run only this hook id or alias.
    #[arg(value_name = "HOOK")]
    hook: Option<String>,

    /// Check every tracked file instead of the staged set.
    #[arg(long, short = 'a', help_heading = "File selection")]
    all_files: bool,
    /// Check these paths instead of the staged set.
    #[arg(long, num_args = 1.., value_name = "PATH", help_heading = "File selection")]
    files: Vec<String>,
    /// With --to-ref, check the files changed between the two refs.
    #[arg(
        long,
        value_name = "REF",
        requires = "to_ref",
        help_heading = "File selection"
    )]
    from_ref: Option<String>,
    /// With --from-ref, check the files changed between the two refs.
    #[arg(
        long,
        value_name = "REF",
        requires = "from_ref",
        help_heading = "File selection"
    )]
    to_ref: Option<String>,
    /// Git hook stage whose hooks run.
    #[arg(
        long,
        default_value = "pre-commit",
        value_name = "STAGE",
        help_heading = "File selection"
    )]
    hook_stage: String,
    /// Commit message file, for the commit-msg and prepare-commit-msg stages.
    #[arg(long, value_name = "PATH", help_heading = "File selection")]
    commit_msg_filename: Option<String>,

    /// Output format.
    #[arg(long, default_value = "text", value_name = "FORMAT", value_parser = ["text", "json", "sarif"], help_heading = "Output")]
    format: String,
    /// Show the output of passing local hooks too. Failures always show output.
    #[arg(long, short = 'v', help_heading = "Output")]
    verbose: bool,

    /// Stop after the first failing hook.
    #[arg(long, help_heading = "Behavior")]
    fail_fast: bool,
    /// Rewrite files for end-of-file, trailing-whitespace, and mixed-line-ending; other checks stay report-only.
    ///
    /// A hook argument `--fix` enables the same three checks; `--fix=no` on
    /// mixed-line-ending keeps it report-only. Writes are atomic, do not follow
    /// symlinks, and skip files outside the selected set. A file with unstaged
    /// changes is left unchanged.
    #[arg(long, help_heading = "Behavior")]
    fix: bool,
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
        Command::Exec(args) => exec(&cli, args),
        Command::Tools { command } => match command {
            ToolsCommand::Init {
                example,
                write,
                force,
            } => tools_init(example, *write, *force, use_color(&cli.color)),
            ToolsCommand::List => match env::current_dir() {
                Ok(cwd) => {
                    print!(
                        "{}",
                        ahi::tools::render_current(&cwd, use_color(&cli.color))
                    );
                    ExitCode::from(0)
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    ExitCode::from(1)
                }
            },
            ToolsCommand::Examples {
                command: None | Some(ExamplesCommand::List),
            } => {
                print!("{}", ahi::tools::render_index());
                ExitCode::from(0)
            }
            ToolsCommand::Examples {
                command: Some(ExamplesCommand::Show { name }),
            } => match ahi::tools::render_examples(name.as_deref(), use_color(&cli.color)) {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::from(0)
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    ExitCode::from(2)
                }
            },
        },
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
    // Tool policy is read from the directory `ahi tui` starts in, not the
    // repository root, so the working directory is the one to report.
    match env::current_dir() {
        Ok(cwd) => {
            for line in ahi::tools::doctor_lines(&cwd) {
                println!("{line}");
            }
        }
        Err(err) => println!("tools policy: {err}"),
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

/// `ahi exec`: one turn, no screen. The library decides the exit code and
/// writes diagnostics to stderr as it goes; this function only resolves the
/// directories, maps the arguments, and writes stdout once.
fn exec(cli: &Cli, args: &ExecArgs) -> ExitCode {
    use ahi::chat::exec::{self, Exit, Format, Options, PromptSource};
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(Exit::Config as u8);
        }
    };
    let prompt = match (&args.prompt, &args.prompt_file) {
        (Some(text), None) => PromptSource::Inline(text.clone()),
        (None, Some(path)) => PromptSource::File(path.clone()),
        _ => PromptSource::Stdin,
    };
    let options = Options {
        model: args.model.clone(),
        prompt,
        prompt_sha256: args.prompt_sha256.clone(),
        thinking: args.thinking.clone(),
        format: Format::parse(&args.format).unwrap_or(Format::Text),
        result_file: args.result_file.clone(),
        headless: args.headless,
        run_id: args.run_id.clone(),
        workdir: args.workdir.clone(),
        tools: args.tools.clone(),
        check: args.check,
        verbose: args.verbose,
        timeout: std::time::Duration::from_secs(args.timeout.max(1)),
    };
    let finished = exec::run(&state, &options);
    if let Some(text) = finished.stdout {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        // A closed pipe is the reader's choice, not a failure of the turn.
        let _ = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush());
    }
    ExitCode::from(finished.code)
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

/// `ahi tools init`: print a shipped policy; with --write create
/// ahi.tools.yaml in the working directory, the directory `ahi tui` reads it
/// from. The file is operator policy, so the chat screen never writes it.
/// `--color`: `always`, `never`, or `auto` (a terminal on stdout and no
/// NO_COLOR in the environment).
fn use_color(when: &str) -> bool {
    use std::io::IsTerminal;
    match when {
        "always" => true,
        "never" => false,
        _ => std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none(),
    }
}

fn tools_init(example: &str, write: bool, force: bool, color: bool) -> ExitCode {
    if force && !write {
        eprintln!("error: --force does not write a file; pass --write --force to replace one");
        return ExitCode::from(2);
    }
    let example = match ahi::tools::example(example) {
        Ok(example) => example,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let path = std::path::Path::new(ahi::tools::FILE_NAME);
    print!("{}", ahi::tools::render_policy(example.text, color));
    println!();
    if !write {
        println!(
            "Left {} unchanged. Rerun with --write to create it from `{}`; rule paths are relative to this directory.",
            path.display(),
            example.name
        );
        return ExitCode::from(0);
    }
    if path.exists() && !force {
        eprintln!(
            "error: refusing to overwrite {}; pass --write --force to replace it",
            path.display()
        );
        return ExitCode::from(1);
    }
    if let Err(err) = std::fs::write(path, example.text) {
        eprintln!("error: cannot write {}: {err}", path.display());
        return ExitCode::from(1);
    }
    println!(
        "Wrote {} from `{}`. `ahi tui` in this directory advertises the tools it approves; `ahi doctor` lists them.",
        path.display(),
        example.name
    );
    ExitCode::from(0)
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
    let shown = path
        .strip_prefix(&cwd)
        .unwrap_or(&path)
        .display()
        .to_string();
    let suggestion = match migrate::suggest(&original, &shown) {
        Ok(suggestion) => suggestion,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    print!("{}", suggestion.report);
    print!("{}", suggestion.suggested);
    println!();
    if !write {
        println!("Left {shown} unchanged. Rerun with --write --force to replace it with the suggestion above.");
        return ExitCode::from(0);
    }
    if path.exists() && !force {
        eprintln!("error: refusing to overwrite {shown}; pass --write --force to replace it");
        return ExitCode::from(1);
    }
    if let Err(err) = std::fs::write(&path, &suggestion.suggested) {
        eprintln!("error: {err}");
        return ExitCode::from(1);
    }
    println!("Wrote {shown}.");
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
