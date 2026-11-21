mod delegate;
mod precommit;
mod terminal_host;
mod tool_policy;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::directories::Directories;
use crate::harness;
use crate::precommit::config::discover_config;
use crate::precommit::git::repo_root;
use crate::precommit::policy;
use crate::precommit::store::{self, Store};
use crate::VERSION;
use clap::{Parser, Subcommand};

const GLOBAL: &str = "Global options";

#[derive(Parser)]
#[command(
    name = "ahi",
    version,
    about = "Offline pre-commit runner, terminal chat, and development container launcher.",
    long_about = "Offline pre-commit runner, terminal chat, and development container launcher.\n\n\
        Hooks run only from this binary or from an approved local command; nothing is fetched or \
        installed by `ahi precommit run`. Repository files: .pre-commit-config.yaml (hooks), ahi.hooks.yaml \
        (which local hooks may run), ahi.tools.yaml (chat tool calls), ahi.dev.yaml (dev image).",
    after_help = "Continue a chat with `ahi --session <ID>`. List saved chats with `ahi session list`. Each command has `--help`.",
    subcommand_negates_reqs = true,
    arg_required_else_help = true
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

    /// Continue a recorded session in the terminal chat (see `ahi session list`).
    #[arg(long, required = true, value_name = "ID")]
    session: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Control an external Rex or hc terminal host (not ahi's TUI or session protocol).
    TerminalHost(terminal_host::Options),
    /// Operator-only: run one bounded, tool-less child turn as a worker process in a Rex session (experimental).
    ///
    /// `launch` reserves a private receipt and an empty scratch directory,
    /// then asks the configured Rex server to start `ahi` as the worker in
    /// the given session; no Rex client needs to be attached. The worker
    /// claims the assignment once, runs the turn on the exact model, and
    /// publishes a structured result that `status` reads back. Nothing here
    /// is offered to a model.
    Delegate(delegate::Options),
    /// The worker side of `ahi delegate`; started by the terminal host, not by hand.
    #[command(hide = true, name = "internal-worker")]
    InternalWorker(delegate::WorkerArgs),
    /// Run precommit checks, inspect configuration, and manage Git hook integration.
    Precommit {
        #[command(subcommand)]
        command: precommit::Command,
    },
    /// Deprecated: use `ahi precommit run` with the same arguments.
    ///
    /// Builtin checks run in this process. A `repo: local` system or script
    /// hook runs only when a rule in ahi.hooks.yaml approves its id and exact
    /// entry. Remote hooks are never fetched or executed.
    #[command(hide = true)]
    Run(precommit::RunArgs),
    /// Deprecated: use `ahi precommit list-checks`.
    #[command(hide = true)]
    ListChecks(precommit::ListChecksArgs),
    /// Deprecated: use `ahi precommit explain`.
    #[command(hide = true)]
    Explain(precommit::ExplainArgs),
    /// Show the local environment: versions, config, directories, hook policy, runtime, sandbox.
    Doctor,
    /// Deprecated: use `ahi precommit install` or `ahi precommit uninstall` (not implemented yet).
    #[command(hide = true)]
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
    /// Deprecated: use `ahi precommit migrate` with the same arguments.
    ///
    /// Prints a report, the ahi.hooks.yaml rules the local hooks need, and a
    /// suggested .pre-commit-config.yaml. Best effort: feature parity and file
    /// scope are not guaranteed, remote repositories are not fetched, and
    /// `ahi precommit run` does not use the mapping. Not upstream `migrate-config`.
    #[command(hide = true)]
    Migrate(precommit::MigrateArgs),
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
        /// Continue a recorded session by id (see `ahi session list`) instead of opening the picker.
        #[arg(long = "session", visible_alias = "resume", value_name = "ID")]
        resume: Option<String>,
        /// Do not record this chat as a session (standalone only; the server always records).
        #[arg(long)]
        no_history: bool,
        /// Run turns in this process instead of through `ahi serve`.
        #[arg(long)]
        standalone: bool,
        /// The server's socket instead of the default; started there when none answers.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Attach to a server elsewhere through this command's stdio, e.g. "ssh host ahi connect".
        #[arg(long, value_name = "COMMAND")]
        via: Option<String>,
    },
    /// Run one model turn from the command line and exit; the worker side of a coordinator.
    ///
    /// The prompt comes from -p, --prompt-file, or stdin. Tools are confined to
    /// --workdir (default: the current directory) and governed by its
    /// ahi.tools.yaml or an explicit --tools file; --headless answers every
    /// `ask` as denied, takes the prompt only from a file, and writes a
    /// result document to --result-file. On a host where `ahi serve` is
    /// running the turn goes through it; otherwise it runs in this process.
    /// The answer is on stdout, progress on stderr, and the exit code says
    /// how the turn ended: 0 ok, 1 turn failed, 2 usage, 3 configuration,
    /// 4 credential, 5 policy file, 6 cancelled or timed out, 7 session, 8
    /// result file not written.
    Exec(Box<ExecArgs>),
    /// Serve ahi's session protocol on a Unix socket for front ends and explicit connectors; `stop` ends it.
    ///
    /// One process per user per host owns the session store and runs every
    /// turn. The socket is $XDG_RUNTIME_DIR/ahi/serve.sock (or
    /// <state>/run/serve.sock), mode 600. There is no network listener and no
    /// token: the filesystem authenticates locally and ssh does across
    /// machines (`ssh host ahi connect`). The server never idles out; it
    /// stops on `ahi serve stop`, SIGTERM, or SIGINT.
    Serve {
        /// Socket path instead of the default.
        #[arg(long, global = true, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Log each request's method on stderr.
        #[arg(short, long)]
        verbose: bool,
        #[command(subcommand)]
        command: Option<ServeCommand>,
    },
    /// Bridge stdin and stdout to the local `ahi serve` socket; what `ssh host ahi connect` runs.
    Connect {
        /// Socket path instead of the default.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },
    /// List, show, export, and import recorded chat sessions.
    ///
    /// Every chat is recorded as one file of JSON lines under the state
    /// directory (`sessions/<id>.jsonl`, owner-only): the operator's
    /// messages, the model's replies and tool calls, each tool's result, and
    /// one `turn` line per exchange. `ahi tui --resume <id>` continues one.
    /// `export` copies a session file out and `import` brings one in, so a
    /// session written on another machine can be listed and resumed here.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Show or write the chat tool policy (ahi.tools.yaml) for the working directory; show the shipped examples.
    ///
    /// Tools are off until the directory `ahi tui` starts in has an
    /// ahi.tools.yaml with an approve rule. `init` prints a shipped policy and
    /// writes it only with --write, like `ahi precommit migrate`.
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
enum ServeCommand {
    /// Start the server detached when none answers, then print the socket path.
    Start,
    /// Ask the running server to cancel open turns and exit.
    Stop,
}

#[derive(clap::Args)]
struct ExecArgs {
    /// Gateway and model as gateway/model-id, curated in models.json.
    #[arg(long, value_name = "GATEWAY/MODEL")]
    model: String,
    /// The prompt text.
    #[arg(short = 'p', long = "prompt", value_name = "TEXT")]
    prompt: Option<String>,
    /// Read the prompt from this file (UTF-8, at most 1 MiB).
    #[arg(long, value_name = "PATH")]
    prompt_file: Option<PathBuf>,
    /// Load repo/NAME or operator/NAME before inference (repeatable).
    #[arg(long = "skill", value_name = "SCOPE/NAME", value_parser = crate::skills::parse_id)]
    skills: Vec<String>,
    /// No human: the prompt from --prompt-file only, every `ask` denied, a result file required.
    #[arg(long)]
    headless: bool,
    /// Working directory for the tools [default: the current directory].
    #[arg(long, value_name = "DIR")]
    workdir: Option<PathBuf>,
    /// Tool policy file instead of the working directory's ahi.tools.yaml; must lie outside it.
    #[arg(long, value_name = "FILE")]
    tools: Option<PathBuf>,
    /// Seconds for the whole turn; a running command is clamped to what remains [default: 600].
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,
    /// Caller-chosen id; a repeat with the same prompt is answered from the record.
    #[arg(long, value_name = "ID")]
    run_id: Option<String>,
    /// Refuse the run unless the prompt's SHA-256 is this hex digest.
    #[arg(long, value_name = "HEX")]
    prompt_sha256: Option<String>,
    /// Continue this recorded session instead of starting one.
    #[arg(long, value_name = "SESSION")]
    session: Option<String>,
    /// Write the result document here; the file must not exist.
    #[arg(long, value_name = "PATH")]
    result_file: Option<PathBuf>,
    /// What stdout carries: the answer, or the whole result document.
    #[arg(long, default_value = "text", value_name = "FORMAT", value_parser = ["text", "json"])]
    format: String,
    /// Reasoning effort for this turn.
    #[arg(long, value_name = "LEVEL", value_parser = ["off", "minimal", "low", "medium", "high"])]
    thinking: Option<String>,
    /// Validate the arguments, files, model, and credential, then exit without a turn.
    #[arg(long)]
    check: bool,
    /// Use the server on this socket; fail if none answers.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Run in this process even when a server is live.
    #[arg(long)]
    standalone: bool,
    /// Reach a server through this command's stdio, e.g. "ssh host ahi connect".
    #[arg(long, value_name = "COMMAND")]
    via: Option<String>,
    /// Progress lines on stderr (always on with --headless).
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum SessionCommand {
    /// One line per session, newest first.
    List {
        /// One JSON object per line instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print a session's conversation: by id from the store, or a session file by path.
    Show {
        /// Session id, or a path to a session file.
        target: String,
        /// The header and entries as JSON lines instead of text.
        #[arg(long)]
        json: bool,
        /// Show the branch ending at this entry instead of the active path.
        #[arg(long, value_name = "SEQ")]
        branch: Option<u64>,
    },
    /// Copy a session file out of the store. The destination must not exist.
    Export {
        /// Session id.
        id: String,
        /// Destination file.
        #[arg(long, value_name = "FILE")]
        to: PathBuf,
    },
    /// Bring a session file into the store, or extend the stored copy with the file's new entries.
    Import {
        /// A session file written by ahi.
        path: PathBuf,
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

#[derive(Subcommand)]
enum HooksCommand {
    /// Deprecated: use `ahi precommit install` (not implemented yet; exits 1).
    Install,
    /// Deprecated: use `ahi precommit uninstall` (not implemented yet; exits 1).
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
    /// to the working directory; an approved command is not confined.
    Init {
        /// Which shipped policy; `ahi tools examples list` names them.
        #[arg(long, value_name = "NAME", default_value = crate::tool_policy::examples::DEFAULT_EXAMPLE)]
        example: String,
        /// Walk through shipped modes in a terminal before previewing or writing.
        #[arg(long, conflicts_with = "example")]
        interactive: bool,
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

pub fn run() -> ExitCode {
    let mut cli = Cli::parse();
    if cli.session.is_some() && cli.command.is_some() {
        eprintln!("error: --session cannot be combined with a subcommand; use `ahi --session <ID>` or `ahi tui --session <ID>`");
        return ExitCode::from(2);
    }
    if !matches!(cli.color.as_str(), "auto" | "always" | "never") {
        eprintln!("error: --color must be auto, always, or never");
        return ExitCode::from(2);
    }
    let command = cli.command.take().unwrap_or_else(|| Command::Tui {
        refresh: false,
        tools: None,
        resume: cli.session.clone(),
        no_history: false,
        standalone: false,
        socket: None,
        via: None,
    });
    precommit::warn_deprecated(&command);
    match &command {
        Command::TerminalHost(options) => ExitCode::from(terminal_host::run(options)),
        Command::Delegate(options) => match directories(&cli) {
            Ok(dirs) => ExitCode::from(delegate::run(&dirs, options)),
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::from(3)
            }
        },
        Command::InternalWorker(args) => ExitCode::from(delegate::worker(args)),
        Command::Precommit { command } => precommit::dispatch(&cli, command),
        Command::Run(args) => precommit::run_precommit(&cli, args),
        Command::ListChecks(args) => precommit::list_checks(&args.format),
        Command::Explain(args) => precommit::explain(&args.id),
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
        Command::Update => match crate::update::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::from(1)
            }
        },
        Command::Migrate(args) => {
            precommit::migrate_config(cli.config.as_deref(), args.write, args.force)
        }
        Command::Tui {
            refresh,
            tools,
            resume,
            no_history,
            standalone,
            socket,
            via,
        } => tui(
            &cli,
            *refresh,
            tools.as_deref(),
            resume.as_deref(),
            !no_history,
            *standalone,
            socket.as_deref(),
            via.as_deref(),
        ),
        Command::Session { command } => session(&cli, command),
        Command::Exec(args) => exec(&cli, args),
        Command::Serve {
            socket,
            verbose,
            command,
        } => with_directories(&cli, |state| match command {
            Some(ServeCommand::Stop) => crate::serve::stop(&state, socket.as_deref()),
            Some(ServeCommand::Start) => {
                let path = crate::serve::ensure_running(&state, socket.as_deref())?;
                println!("{}", path.display());
                Ok(0)
            }
            None => crate::serve::run(&state, socket.as_deref(), *verbose),
        }),
        Command::Connect { socket } => with_directories(&cli, |state| {
            crate::serve::connect(&state, socket.as_deref())
        }),
        Command::Tools { command } => match command {
            ToolsCommand::Init {
                example,
                interactive,
                write,
                force,
            } => tool_policy::init(example, *interactive, *write, *force, use_color(&cli.color)),
            ToolsCommand::List => match env::current_dir() {
                Ok(cwd) => {
                    print!(
                        "{}",
                        crate::cli::tool_policy::render_current(&cwd, use_color(&cli.color))
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
                print!("{}", crate::cli::tool_policy::render_index());
                ExitCode::from(0)
            }
            ToolsCommand::Examples {
                command: Some(ExamplesCommand::Show { name }),
            } => match crate::cli::tool_policy::render_examples(
                name.as_deref(),
                use_color(&cli.color),
            ) {
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
            let report = crate::sandbox::report(&crate::sandbox::probe());
            for line in &report.lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
    }
}

fn dev(cli: &Cli, args: &DevArgs) -> ExitCode {
    use crate::dev::{Action, Options};
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
    match crate::dev::run(action, &options, &dirs) {
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

fn doctor(
    config: Option<&std::path::Path>,
    cache: Option<&std::path::Path>,
    state: Option<&std::path::Path>,
) -> ExitCode {
    println!("ahi {VERSION}");
    match crate::precommit::git::git_version() {
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
            for path in crate::directories::exposed_paths(&dirs) {
                println!(
                    "warning: {} is readable by other users; ahi expects 0700 directories and 0600 files",
                    path.display()
                );
            }
        }
        Err(err) => println!("directories: {err}"),
    }
    println!("scratch: {}", store::scratch_dir().display());
    match crate::dev::resolve_runtime(
        None,
        env::var(crate::dev::RUNTIME_ENV).ok().as_deref(),
        None,
    ) {
        Ok(runtime) => println!("container runtime: {}", runtime.display()),
        Err(_) => println!("container runtime: none (ahi dev needs podman or docker)"),
    }
    println!(
        "{}",
        crate::sandbox::summary(&crate::sandbox::report(&crate::sandbox::probe()))
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
                    "remote hooks: not executed and not fetched. Checkers are repo: builtin. ahi precommit migrate can suggest a builtin id"
                );
                println!(
                    "fix: `ahi precommit run --fix` rewrites end-of-file, trailing whitespace, and mixed line endings; other checks stay report-only"
                );
                precommit::print_local_env(Some(&loaded));
            }
            Err(err) => println!("hooks policy: {err}"),
        },
        Err(err) => println!("hooks policy: {err}"),
    }
    // Tool policy is read from the directory `ahi tui` starts in, not the
    // repository root, so the working directory is the one to report.
    match env::current_dir() {
        Ok(cwd) => {
            for line in crate::cli::tool_policy::doctor_lines(&cwd) {
                println!("{line}");
            }
            // Shared skills are independent of ahi's configuration/state overrides.
            for line in crate::skills::doctor_lines(&cwd, std::env::home_dir().as_deref()) {
                println!("{line}");
            }
        }
        Err(err) => println!("tools policy: {err}"),
    }
    ExitCode::from(0)
}

fn directories(cli: &Cli) -> crate::Result<Directories> {
    Directories::resolve(cli.state_dir.as_deref(), cli.cache_dir.as_deref())
}

#[allow(clippy::too_many_arguments)]
fn tui(
    cli: &Cli,
    refresh: bool,
    actions: Option<&std::path::Path>,
    resume: Option<&str>,
    record: bool,
    standalone: bool,
    socket: Option<&std::path::Path>,
    via: Option<&str>,
) -> ExitCode {
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    match crate::tui::run(
        &state, refresh, actions, resume, record, standalone, socket, via,
    ) {
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

/// Resolve the directories, run `body`, and turn its result into an exit code.
fn with_directories(cli: &Cli, body: impl FnOnce(Directories) -> crate::Result<u8>) -> ExitCode {
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    match body(state) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn exec(cli: &Cli, args: &ExecArgs) -> ExitCode {
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(crate::exec::Exit::Config.code());
        }
    };
    let options = crate::exec::Options {
        model: args.model.clone(),
        prompt: args.prompt.clone(),
        prompt_file: args.prompt_file.clone(),
        skills: args.skills.clone(),
        headless: args.headless,
        workdir: args.workdir.clone(),
        tools: args.tools.clone(),
        timeout_secs: args.timeout,
        run_id: args.run_id.clone(),
        prompt_sha256: args.prompt_sha256.clone(),
        session: args.session.clone(),
        result_file: args.result_file.clone(),
        format: if args.format == "json" {
            crate::exec::Format::Json
        } else {
            crate::exec::Format::Text
        },
        thinking: args.thinking.clone(),
        check: args.check,
        socket: args.socket.clone(),
        standalone: args.standalone,
        via: args.via.clone(),
        verbose: args.verbose,
    };
    ExitCode::from(crate::exec::run(&state, &options))
}

fn session(cli: &Cli, command: &SessionCommand) -> ExitCode {
    use crate::session::commands;
    let state = match directories(cli) {
        Ok(dirs) => dirs,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    let result = match command {
        SessionCommand::List { json } => commands::list(&state, *json).map(|(text, skipped)| {
            for problem in skipped {
                eprintln!("skipped: {problem}");
            }
            text
        }),
        SessionCommand::Show {
            target,
            json,
            branch,
        } => commands::show(&state, target, *json, *branch),
        SessionCommand::Export { id, to } => commands::export(&state, id, to),
        SessionCommand::Import { path } => commands::import(&state, path),
    };
    match result {
        Ok(text) => {
            print!("{text}");
            ExitCode::from(0)
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

/// Resolve `--color` without changing the process environment.
fn use_color(when: &str) -> bool {
    use std::io::IsTerminal;
    match when {
        "always" => true,
        "never" => false,
        _ => std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none(),
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    const ID: &str = "01a1db77-ba18-74c2-b4cd-f05cf247eb76";

    #[test]
    fn session_entry_points_preserve_the_selected_id() {
        let cli = Cli::try_parse_from(["ahi", "--session", ID]).unwrap();
        assert_eq!(cli.session.as_deref(), Some(ID));
        assert!(cli.command.is_none());
        for flag in ["--session", "--resume"] {
            let cli = Cli::try_parse_from(["ahi", "tui", flag, ID]).unwrap();
            let Some(Command::Tui { resume, .. }) = cli.command else {
                panic!("expected the TUI command");
            };
            assert_eq!(resume.as_deref(), Some(ID));
        }
    }

    #[test]
    fn session_selection_rejects_missing_and_duplicate_arguments() {
        for args in [
            vec!["ahi", "--session"],
            vec!["ahi", "tui", "--session"],
            vec!["ahi", "tui", "--resume"],
            vec!["ahi", "tui", "--session", ID, "--resume", ID],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
        assert!(matches!(
            Cli::try_parse_from(["ahi"]).err().unwrap().kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        ));
    }
}
