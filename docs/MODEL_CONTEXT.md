# Model instructions and capabilities

ahi prepares one instruction snapshot for each turn. Standalone chat, service-backed chat, and `ahi exec` use the same backend. A queued prompt starts a new turn and receives a new snapshot.

The request contains the compiled core instructions, a summary of the effective capabilities, operator instructions, repository instructions, and conversation history. Tools remain structured provider definitions. Loaded skills remain tool results.

## Instruction sources and precedence

| Source | Location | Use |
| --- | --- | --- |
| Core instructions | Compiled in `src/system_prompt.rs` | Define the assistant's general behavior. |
| Effective capabilities | Generated from the resolved mode policy | Explain available tools and the limits enforced by the runtime. |
| Operator instructions | `~/.agents/AGENTS.md` | Supply personal defaults across workspaces. The home directory belongs to the process executing the turn. |
| Repository instructions | `AGENTS.md` at the Git worktree root | Supply project guidance. Outside Git, ahi uses `AGENTS.md` in the requested working directory. |
| Operator skills | `~/.agents/skills/<name>/SKILL.md` | Supply shared skills that are loaded on demand. |
| Repository skills | `.agents/skills/<name>/SKILL.md` | Supply project skills that are loaded on demand. |

These locations use shared harness conventions. Repository-root `AGENTS.md` follows the [open AGENTS.md format](https://agents.md/). The `.agents/skills` directories follow the [cross-client skill discovery convention](https://agentskills.io/client-implementation/adding-skills-support#where-to-scan).

`~/.agents/AGENTS.md` is ahi's provider-neutral operator instruction location. It is a project convention, not a global path mandated by the AGENTS.md format. Instruction and skill discovery do not depend on the selected model provider. Company-specific harness directories are not searched as fallbacks.

Direct operator requests take precedence over file-based guidance within the enforced limits. Repository guidance takes precedence over operator defaults for project matters. Instruction files cannot approve a tool call, change execution mode, or add a policy grant.

For example, operator instructions can request concise reports. A repository can require a detailed verification section for changes in that project. Both apply until a direct request changes the reporting requirement. A repository instruction to execute a command still requires an approving mode policy.

Starting in a Git subdirectory still loads the worktree-root `AGENTS.md`. This version does not walk nested `AGENTS.md` files or switch instruction scope for individual tool paths. Linked Git worktrees use their own worktree root. Bare repositories use the requested working directory.

Both instruction files are optional. Each present file must be a regular UTF-8 file of at most 64 KiB. Invalid, unreadable, or oversized files fail the turn before the first model request. ahi does not silently truncate them.

The operator file can be a symbolic link, including a dotfiles-managed link. ahi resolves its target before opening it. Repository `AGENTS.md` symbolic links are refused. Opened files are checked again for type and size. macOS and Linux use the existing descriptor-relative, nonblocking, no-follow reader. Windows opens repository files without following the final reparse point.

Built-in file-write tools refuse changes to files named `AGENTS.md`, including case variants and Full Access paths. Commands have their own policy rules and are not a filesystem sandbox.

## Mode and policy remain the authority

Allowed commands come from the effective policy. There is no second command allowlist in the prompt.

- Plan supplies built-in read, Git inspection, skill-loading, and private scratch capabilities. It blocks workspace writes, program execution, and delegation.
- Trusted supplies the built-in workspace policy, including program execution.
- Full Access adds absolute host paths for the six file tools. Command working directories remain inside the workspace.
- Policy and named workspace modes use the applicable rules from `ahi.modes.yaml`, the legacy `ahi.tools.yaml`, or the explicit `--tools` file. Selecting Trusted bypasses workspace rules as described in [Workspace trust](WORKSPACE_TRUST.md).

The capability summary is generated from that resolved policy and the tools actually advertised. It reports:

- The selected mode, canonical workspace path, and advertised tool names.
- Workspace and host-path boundaries, protected file names, and runtime exclusions.
- Command argument prefixes and working-directory prefixes.
- File path prefixes, skill and script selectors, and delegation selectors.
- Approve, ask, and deny effects, plus the matching and deny-precedence rules.

For example, a command rule can appear as:

```json
{"id":"mode-review-command-1","effect":"approve","action":"command","tools":["run_command"],"argv_prefix":["cargo","test"],"working_directory_prefix":"src"}
```

This describes a token prefix. The model can append arguments after `cargo test`, and the command must use a matching working directory. A matching deny rule still wins. A rule marked `ask` requires operator approval unless an existing scoped grant satisfies it.

Every call still passes argument validation and runtime policy evaluation. Advertising a tool does not authorize every possible argument. The summary does not turn a workspace working directory into containment for a spawned process.

Detailed rules have a 32 KiB summary budget. If the complete rule representation does not fit, ahi omits the entire detailed rule block and says so. It retains the high-level capability description and enforces the complete runtime policy. This avoids displaying an incomplete allowlist that hides a later deny rule.

## Turn snapshots and provider requests

Instruction text and the effective policy summary stay fixed through all model and tool rounds in one turn. Changes to either `AGENTS.md` file take effect on the next turn. Mode and policy changes follow their existing turn-admission rules.

Chat Completions receives the combined text as its first system message. Responses receives it in `instructions`. Anthropic receives it in `system`. The core instructions occur once in each request. The provider request is rebuilt for each tool continuation with the same snapshot and the accumulated tool results.

The instruction snapshot is not appended to durable conversation history. Resuming a session loads instructions for the current workspace and execution process. Existing skill loads remain recorded tool exchanges. See [Shared skill runtime](SKILLS.md).

For service-backed turns, the daemon's binary and home directory supply the instructions. Upgrading only the client does not replace a running daemon's compiled core instructions.

## Verification and troubleshooting

Build the checkout binary and run the maintained local fixture:

```sh
cargo build --locked --bin ahi
uv run --no-project --offline scripts/smoke-model-picker.py \
  --binary target/debug/ahi \
  --model-instructions-only \
  --capture-directory target/model-instruction-captures
```

The fixture uses disposable home, state, and repository directories with loopback HTTP. It checks both chat engines, all three provider encodings, instruction refresh, skill-independent context, real permitted effects, runtime denials, and exclusion from durable history. It does not require an external model service or personal credentials. The host must permit pseudo-terminals, local HTTP listeners, and Unix sockets.

If a turn fails while loading instructions, inspect the path and reason in the error. Replace an invalid repository link with a regular file. Correct the encoding or reduce a file to 64 KiB. A missing optional file needs no replacement. In service-backed chat, check the daemon's home and requested working directory when a file is absent from the request.

The implementation lives in [system_prompt](../src/system_prompt.rs), [instruction loading](../src/system_prompt/instructions.rs), [capability rendering](../src/tool_policy/capabilities.rs), and the [shared turn loop](../src/chat/backend/http.rs).
