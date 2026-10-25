use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use ahi::audit::AuditEvent;
use ahi::check::{all_checks, builtin_by_id};
use ahi::config::discover_config;
use ahi::git::repo_root;
use ahi::output::render;
use ahi::runner::{self, Selection};
use ahi::store::{self, Store};
use ahi::{config, migrate, policy, VERSION};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ahi", version, about = "Fail-closed pre-commit runner.")]
struct Cli {
    /// Path to a pre-commit config file.
    #[arg(long, short = 'c', global = true)]
    config: Option<PathBuf>,

    /// Cache directory. Overrides AHI_HOME. There is no home-directory fallback.
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,

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
    /// Install the git hook shim.
    Install,
    /// Remove the git hook shim.
    Uninstall,
    /// Resolve tags to full commits and write ahi.lock.
    Lock,
    /// Check ahi.lock against the cache.
    Verify,
    /// Populate the cache. The only command that uses the network.
    Fetch,
    /// Move locked repos to newer commits.
    Autoupdate,
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
        Command::Doctor => doctor(cli.config.as_deref(), cli.cache_dir.as_deref()),
        Command::Install | Command::Uninstall => {
            unimplemented_until("the hook shim phase (after hook execution exists)")
        }
        Command::Lock | Command::Verify | Command::Fetch | Command::Autoupdate => {
            unimplemented_until("v0.2")
        }
        Command::Migrate { write, force } => migrate_config(cli.config.as_deref(), *write, *force),
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
    let cache = store::cache_dir(cli.cache_dir.as_deref());
    if let Ok(store) = Store::open(&cache) {
        let ids: Vec<String> = report.hooks.iter().map(|hook| hook.id.clone()).collect();
        let findings = report.hooks.iter().map(|hook| hook.findings.len()).sum();
        let outcome = match report.result {
            runner::Outcome::Passed => "passed",
            runner::Outcome::Failed => "failed",
            runner::Outcome::Skipped => "skipped",
        };
        if let Err(err) = ahi::audit::append(
            store.root(),
            &AuditEvent {
                command: "run",
                repo: &root,
                hook_ids: &ids,
                outcome,
                finding_count: findings,
                local_hooks: &report.local_hooks,
            },
        ) {
            eprintln!("warning: {err}");
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
    ExitCode::from(0)
}

fn doctor(config: Option<&std::path::Path>, cache: Option<&std::path::Path>) -> ExitCode {
    println!("ahi {VERSION}");
    match ahi::git::git_version() {
        Ok(version) => println!("git: {version}"),
        Err(err) => println!("git: {err}"),
    }
    match env::current_dir().and_then(|cwd| {
        repo_root(&cwd)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err.to_string()))
    }) {
        Ok(root) => {
            println!("root: {}", root.display());
            match discover_config(&root, config) {
                Ok(path) => println!("config: {}", path.display()),
                Err(err) => println!("config: {err}"),
            }
        }
        Err(err) => println!("root: {err}"),
    }
    let cache = store::cache_dir(cache);
    match Store::open(&cache) {
        Ok(store) => {
            let mode = if store.is_writable() {
                "writable"
            } else {
                "read-only"
            };
            println!("cache: {} ({mode})", store.root().display());
        }
        Err(err) => println!("cache: {} ({err})", cache.display()),
    }
    println!("scratch: {}", store::scratch_dir().display());
    println!("sandbox: not probed in this version");
    println!(
        "network: not enforced in this version (no sandbox); local hook environments are scrubbed"
    );
    match env::current_dir().and_then(|cwd| {
        repo_root(&cwd)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err.to_string()))
    }) {
        Ok(root) => match policy::load_policy(&root) {
            Ok(loaded) => {
                let path = root.join("ahi.policy.yaml");
                if loaded.path_present {
                    println!("policy: {}", path.display());
                } else {
                    println!("policy: absent (deny by default)");
                }
                let pinned = loaded
                    .trusted_system_hooks
                    .iter()
                    .filter(|hook| hook.entry.is_some())
                    .count();
                let bare = loaded.trusted_system_hooks.len() - pinned;
                println!(
                    "trusted system hooks: {pinned} entry-pinned, {bare} id-only (id-only does not authorize)"
                );
                println!(
                    "local execution: system and script when the policy pins the hook id and the exact entry; other languages are refused"
                );
                println!(
                    "remote hooks: not executed and not substituted; use repo: builtin or `ahi migrate` for a best-effort suggestion"
                );
                println!(
                    "fix: `ahi run --fix` rewrites end-of-file, trailing whitespace, and mixed line endings; other checks stay report-only"
                );
            }
            Err(err) => println!("policy: {err}"),
        },
        Err(err) => println!("policy: {err}"),
    }
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
