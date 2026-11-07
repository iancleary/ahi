# Development container and sandbox: how `ahi dev` and `ahi sandbox` relate

Status: `ahi dev` shipped in 0.7.0 as a launcher. `ahi dev run` and `ahi sandbox` are proposals here, not implemented. Image work is tracked in [iancleary/ahi-images](https://github.com/iancleary/ahi-images).

## Two commands, two questions

| | `ahi dev` | `ahi sandbox` (proposed) |
| --- | --- | --- |
| Question it answers | What tools are in the shell? | What may this process touch? |
| Mechanism | A container image with a toolchain, run by `podman` or `docker` | Filesystem, network, and syscall policy around a child process |
| Who it serves | The developer | An agent tool call, a local hook, or a command ahi itself runs |
| Direction | Opens things up: sudo, network, the mounted tree | Closes things down: deny by default |
| Source of truth | `ahi.dev.yaml`, a preference | `ahi.hooks.yaml` and `ahi.tools.yaml` rules, plus an outer OpenShell policy |
| Security claim | None. Its properties are the runtime's | The strongest backend the host offers, stated honestly by `ahi sandbox check` |

They stay separate commands. A dev image with sudo and an open network is the opposite of a sandbox, and the specification's threat model already warns against containment implied by a container. The two compose instead: `ahi dev run -- ahi sandbox run -- cargo test` uses the image's toolchain while OpenShell or Landlock holds the boundary.

## `ahi dev` today

`ahi dev` pulls `ghcr.io/iancleary/ahi-dev:latest` when it is missing and opens a shell. The runtime line is:

```text
run --rm -i [-t] --volume <cwd>:/work --workdir /work [--env TERM=...] <image> <shell>
```

- **One mount.** The current working directory, not the git root, bind-mounted read-write at `/work`. Nothing else: no home directory, no ssh agent, no git credentials, no ahi directories, no container socket. Opt-ins, if any arrive, are explicit flags, never defaults.
- **Image resolution.** `--image`, then `AHI_DEV_IMAGE`, then `image` in the working directory's `ahi.dev.yaml`, then `dev-image` in the config directory, then the default.
- **Not a boundary.** The container runs as the image's `dev` user with passwordless sudo and an open network, through a daemon that is root-equivalent on the host. `ahi dev` is a reproducible shell.

### Consequences of the single mount

- **Subdirectories.** Starting in a subdirectory mounts only that subdirectory; `.git` is then outside. Start from the repository root for the whole repo. A `--root` flag that mounts the git root and sets `--workdir` to the matching subpath is a possible addition.
- **Shared `target/`.** `cargo build` inside writes `target/` into the repo, sharing one directory between the host toolchain and the image toolchain. The image should set `CARGO_TARGET_DIR=/work/target/dev` so the two caches do not fight. Image-side change.
- **Ownership.** Docker Desktop on macOS and Windows, and rootless podman, map the image's `dev` user to the host user. Rootful Docker on Linux does not: files written inside come out owned by uid 1000. ahi does not pass `--user`, because that breaks the image's home directory and sudo. The fix is an entrypoint that remaps `dev` to a host uid and gid passed in as environment variables. Image-side change, with ahi passing `AHI_HOST_UID` and `AHI_HOST_GID`.
- **Worktrees.** A `git worktree` checkout has a `.git` file pointing at the main repository, which is outside the mount. Git inside the container does not work in a worktree. An ahi-side flag that also mounts the main repository at its host path is the likely fix; it is not there now.
- **Symlinks** that leave the tree resolve on the host and are dead inside.

## `ahi dev run -- <command>` (proposed)

The non-interactive twin of `ahi dev shell`: the same image, mount, and runtime, with the arguments after `--` replacing the shell and the command's exit code returned.

```text
ahi dev run [--rebuild] [--image REF] [--runtime CMD] -- <command> [args...]
```

- Pulls when the image is missing, like bare `ahi dev`, because a scripted caller cannot be asked.
- Passes argv through untouched; `--shell` is ignored by `run`.
- `-t` only when stdin is a terminal, so pipes work. Stdout and stderr pass through unmodified.
- Stays a launcher. It is not `ahi run --in-container`: the hook runner never depends on a daemon. A repository that wants hooks in the image writes a hook whose entry is `ahi dev run -- ...` and approves it in `ahi.hooks.yaml` like any other command.
- Deferred until a case needs them: `--env NAME` pass-through (names only, never values, matching `toolchain_env`) and `--workdir <relative>` for monorepos.

Implementation: one `DevCommand::Run { command: Vec<OsString> }` with `trailing_var_arg`, `run_arguments` taking a slice instead of one shell, and a CLI test through the stand-in runtime script in `tests/dev.rs`.

## `ahi sandbox` (proposed)

The specification's v0.4 sandbox plan, exposed to a human so the engine's containment is testable before an agent uses it.

1. `ahi sandbox check` prints the backend that would be used and why, and whether the process is already inside OpenShell. Build this first: detection and backend selection are what everything else depends on, they run on CI runners, and they settle the specification's OpenShell questions with measurements.
2. `ahi sandbox run -- <command>` runs a child under that backend with the default profile: read the working directory and the ahi cache, write `/tmp` and, when asked, the working directory, no network. Linux first (Landlock plus seccomp, `bwrap` fallback), Windows as a job object labelled "process-tree only", macOS to be decided given `sandbox-exec`'s deprecated status.
3. **Inside OpenShell, do not nest.** The runtime profile denies `seccomp(SECCOMP_SET_MODE_FILTER)` with `EPERM`; ahi detects that and runs the child directly, trusting the outer policy. OpenShell is the sandbox on the machine when present; ahi's backend is the fallback when it is not.
4. **Profiles come from rules.** The `sandbox:` field already parsed on tool rules, currently restricted to `default`, names the profile. Widening is a field on an approve rule: extra read paths, write paths, network. The same profile later wraps `language: system` hooks and agent tool calls; `ahi sandbox run` is the direct way to try the containment a rule would get.

The sandbox is not a container and has no toolchain inside it. It runs the host's binaries under the host's kernel with less access.

## Order of work

1. ahi-images: layered images with pinned toolchains, `CARGO_TARGET_DIR`, uid remapping in the entrypoint. See the proposal filed there.
2. ahi: `ahi dev run`, then pass `AHI_HOST_UID`/`AHI_HOST_GID` once the entrypoint reads them.
3. ahi: `ahi sandbox check`, then `ahi sandbox run` on Linux, then rule-driven profiles for hooks and tools.
