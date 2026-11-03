# ahi agent runtime and coordinator

Status: proposal for review, with the owner's decisions recorded in [Recorded decisions](#recorded-decisions). This document is not the design of record. [docs/SPEC.md](SPEC.md) remains the compatibility, security, and architecture contract for v0.1 through v0.4. Nothing here changes code, the command-line surface, version numbers, release scripts, or published artifacts.

The package, the library target, and the binary stay named `ahi`. Callers continue to write `use ahi::...`. The expansion "agent hook interface" is still not confirmed. This proposal uses `ahi` as the name of a public agent runtime and coordinator that keeps the existing hook runner.

Research date: 2026-11-03. Upstream facts are taken from the pages, commits, and source files listed in [Sources](#sources). Anything this author did not re-check is marked **Unverified**.

## How to read this document

The proposal has five layers:

1. A core runtime and coordinator.
2. A durable conversation store.
3. A policy and hook engine around agent actions.
4. Sandbox execution, including the existing OpenShell plan.
5. Distribution of ahi itself.

The hook runner specified for v0.1 through v0.4 stays available. The differentiator is to generalize that hook posture, so the same kind of declared and pinned rule approves, denies, or rewrites an agent action. A pre-commit run is one of those actions, beside a tool call, a command, a file write, a push, and an outbound message. Sandbox escalation is one of those rules.

The conversation store is a Rust-native store that follows the published Pi Durable logical model. There is no Node.js sidecar. See [Recorded decisions](#recorded-decisions).

## Inferred context

The following is **inferred** from the review brief for this proposal, not from a statement recorded in this repository. Treat it as design input.

- ahi becomes the public agent runtime and coordinator. The role is comparable to the product described at [pi.dev](https://pi.dev/): a harness an operator adapts, distributed as a program the operator installs. The fetched pi.dev page calls Pi "a minimal agent harness" and says "We've shipped Pi 1.0 with Pi Durable."
- Distribution targets named in the brief: crates.io (`cargo install ahi`), the domain ahi.dev (install instructions and documentation), and the attested static release binaries this repository already defines.
- ahi installs and updates only itself and its first-party components. It does not become a general package manager. It stays useful for repositories that are not Rust projects.
- The coordinator, task and session registry, worker model, event intake, and the principle that authorization carries forward are to be molded from [jessfraz/agent](https://github.com/jessfraz/agent) and from Ian's private copy `https://github.com/iancleary/agent`. They are not to be copied. The private repository was not readable in this environment. What this document says about it comes from the owner's comparison on the review, cited below, and is marked as such.
- The conversation and provider layer follows Pi Durable at `https://github.com/earendil-works/pi`. The review brief treated a Node.js sidecar as an alternative. The owner has chosen the Rust-native store and rejected the sidecar. See [Recorded decisions](#recorded-decisions).
- The differentiator is ahi's existing declared, pinned, configuration-driven hooks, generalized to agent actions (tool calls, commands, file writes, pushes, and outbound messages), combined with sandbox-by-default execution and escalation governed by that same engine.
- The existing pre-commit runner remains available, for example as an `ahi hooks` subcommand, so 0.1.x users keep working.

Observed, not inferred: the repository is `https://github.com/iancleary/ahi`. The root `Cargo.toml` is edition 2021, license `MIT`, MSRV `1.88`, `unsafe_code = "forbid"`. [docs/SPEC.md](SPEC.md) already records an MIT `LICENSE` with `Copyright (c) 2026 Ian Cleary`.

## Research limits

`GET https://api.github.com/repos/iancleary/agent` returned HTTP 404 on 2026-11-03. This environment cannot tell a missing repository from a private repository. The private tree was not read, and this document does not repeat hostnames, addresses, gateway origins, machine names, state-file contents, or conversation text from it. The owner will retire that repository. It was exploratory. This proposal does not depend on it.

The owner compared that tree with public jessfraz/agent in a review comment: <https://github.com/iancleary/ahi/pull/6#issuecomment-6594868422> (comment `6594868422`, 2026-11-03). Per that comparison, and not from an independent reading:

- The baseline is jessfraz/agent at `d4ef2fb5af0b44531e771a6c3ba4bb7959caf966`, imported verbatim on 2026-10-31 as commit `f4adc30` per the comparison. The committer date of the public baseline is 2026-10-31T15:32:22Z. This environment confirmed that public commit. It did not confirm `f4adc30`.
- The compared tree is `iancleary/agent` version 0.1.0 at `0b47b63`. The histories are disjoint. The repository was seeded from a copy, not from a git fork. The comment reports a direct tree-to-tree diff of 46 files, +2,636/−201.
- That copy predates public `main` at `e671f61`, which is the commit this proposal read. Files that exist only on the later public tree are upstream drift, not changes made in the copy. See [Upstream drift](#upstream-drift).

Statements taken from the comment are attributed as **per the owner's comparison**. Public jessfraz/agent facts below still come from the shallow clone at `e671f61d54ffe3b1361db79e7cc79a68ac252b23` (committer date 2026-11-02T18:14:05-08:00), unless a line names the comparison.

Pi Durable was read from [earendil-works/pi](https://github.com/earendil-works/pi) `main` at commit `9c9ce8547e5a88cbe834a53e9a6d83a789d08e47` (committer date 2026-11-02T23:53:07Z). jessfraz/agent pins the npm packages `@earendil-works/pi-durable`, `@earendil-works/pi-ai`, `@earendil-works/chord`, `@earendil-works/pi-mcp`, and `@earendil-works/pi-telemetry` at version `1.4.0`. This research did not diff that tarball against `main`. Schema details below are from `main`, not from a claim that `1.4.0` has the same tables.

The normative Pi Durable document `packages/durable/docs/spec.md` is about 362 KiB. It was not read end to end. The data-model sketch uses the package README, `packages/durable/src/types.ts`, and the SQLite schema sources cited below. The separate specification `packages/agent/docs/harness.md` describes a different storage shape. It is cited only to keep those shapes apart.

OpenShell statements are not re-fetched here. They are used as already written in [docs/SPEC.md](SPEC.md).

## Goals

1. Keep the v0.1 through v0.4 hook runner working under the names operators already use, and offer a grouped `ahi hooks` surface that calls the same behavior.
2. Make ahi the process that coordinates agent work: one writer for conversation state, a task registry, and workers that execute effects without holding that store.
3. Persist conversations so a restarted process resumes from the last committed point and does not repeat an effect whose outcome is unknown.
4. Decide every agent effect by a declared rule: approve, deny, or rewrite. Record the decision. Let a prior grant apply to a later matching action without a new prompt, and refuse to treat model output or inbound mail as a grant.
5. Run effects in a sandbox by default. Widen that sandbox only when a rule says so. When an outer OpenShell sandbox is already present, detect it and do not nest, as v0.4 already specifies.
6. Distribute one `ahi` binary with `cargo install ahi` on Linux, macOS, and Windows, and document verification of the existing attested Linux archives. ahi.dev, when it serves, documents those paths.
7. Stay useful in repositories whose languages are shell, Docker, GitHub Actions, TypeScript, Go, Python, or Rust. The runtime does not require a Rust project.

## Non-goals

- Replacing [docs/SPEC.md](SPEC.md), renumbering v0.2, v0.3, or v0.4, or pulling agent execution into those phases.
- A package manager, including a hook-repository installer. ahi does not install Node, Python, Go, Rust toolchains, npm packages, Pi packages, hook repositories, or third-party agents. Checkers are compiled into the binary and selected with `repo: builtin`. There is no `ahi fetch`, `ahi lock`, or `ahi verify`. `ahi update` replaces the ahi binary from an attested archive. It does not install hook repositories.
- A drop-in copy of jessfraz/agent, of Rex, of Tailscale, of Nix, or of the Pi coding-agent terminal interface.
- Byte-for-byte compatibility with a Pi session file. The store follows Pi Durable's logical model. Reading a Pi database file would be a later importer against a pinned schema, and it is not part of the first store.
- An iOS client in this repository. The owner deferred that client. It is not part of the actions, sandbox, store, or coordinator work. Per the owner's comparison, the private copy changed `ios/README.md` only, with no Swift source changes relative to its baseline. Those documentation edits are not an iOS scope for ahi.
- Depending on `iancleary/agent`, or ingesting private state from it. The owner will retire that repository. It was exploratory. ahi does not read or copy credentials, gateway origins, hostnames, addresses, machine names, conversation transcripts, embedding indexes, or the contents of state files. A design fact in this document is not permission to import that state. Personal deployment notes and skills from the private copy are not part of ahi.
- A Node.js sidecar, a Node bridge, or a dependency on `@earendil-works/pi-durable` at runtime.
- An `ahi settings` subcommand. The owner has not defined one. This proposal does not add one.
- Hosting a multi-tenant service. The coordinator is a process the operator runs, in the same spirit as the current single-user hook runner. **Inferred** from the brief and from the existing specification's lack of a third-party hook service.
- Publishing, tagging, or cutting a release as part of accepting this document.

## Current baseline

v0.1.3, as implemented and as specified:

- `ahi run` reads `.pre-commit-config.yaml`, filters files, and runs builtin checks. `language: system` and `language: script` run only when `ahi.policy.yaml` pins the hook id and the exact `entry`. Remote hooks fail with an explicit error.
- The command set in `src/main.rs` is `run`, `list-checks`, `explain`, `doctor`, `hooks`, `update`, `migrate`, `tui`, and `login`. `ahi hooks install` and `ahi hooks uninstall` exit 1 and name the hook shim phase. Top-level `install` and `uninstall` are not commands. `update` exits 1 and names the attested archive replacement of this binary. `tui` loads OpenAI and gateways from `models.json`. `login` is the OpenAI browser sign-in. `lock`, `verify`, `fetch`, `autoupdate`, and `self-update` are not commands.
- `ahi.policy.yaml` version 1 is deny-by-default. Unknown keys are errors. The file is parsed. v0.1 consults `trusted_system_hooks` for local system and script hooks.
- `ahi::store` is the content-addressed hook cache. It is not a conversation database.
- `audit.log` is append-only JSON lines: time, command, repository, hook ids, outcome, finding count, local hook ids, and environment names. It does not record file contents or environment values.
- Release assets, once the workflow in [docs/release.md](release.md) has published them, are a static `x86_64-unknown-linux-musl` binary, a static `aarch64-unknown-linux-musl` binary, and `ahi-<version>-sha256sums.txt`, each attested. macOS and Windows are CI targets. They are not release-archive targets.
- The crate forbids `unsafe_code`. v0.4's Landlock and seccomp plan will have to confront that lint. This proposal does not change the lint.

Roadmap already committed in the specification:

| Phase | Contract |
| --- | --- |
| v0.2 | Withdrawn as a hook-repository installer. No `ahi.lock`. No new builtins. `ahi update` remains the later replacement of the ahi binary. |
| v0.3 | Single-machine harness. `ahi tui` loads OpenAI and a custom gateway. `ahi login` is the browser sign-in. Not more builtins, and not a language installer. |
| v0.4 | Detect an outer sandbox and do not nest. Optional host backends, with the limits in the specification. |

## Architecture

```text
operator, CI, and local clients
            |
            v
+------------------------------------------------------+
| ahi coordinator (one writer)                         |
|   conversation store                                 |
|   task registry                                      |
|   grant and action ledger                            |
|   source intake (off unless configured)              |
+------------------------------------------------------+
            |
            | every effect is an action record
            v
+------------------------------------------------------+
| policy and hook engine                               |
|   approve, deny, or rewrite                          |
|   default deny                                       |
|   a grant applies only to a later match             |
+------------------------------------------------------+
            |
            v
+------------------------------------------------------+
| sandbox execution                                    |
|   outer OpenShell: detect, do not nest               |
|   otherwise: host backend from the v0.4 table        |
|   a wider sandbox exists only by rule                |
+------------------------------------------------------+
            |
            v
workers on execution hosts
    effects only; they do not own the conversation store

Beside that path, unchanged in role:

ahi run
ahi hooks run
    .pre-commit-config.yaml
    ahi.policy.yaml version 1
    builtin checks in process
    pinned local hooks
```

The coordinator is the only writer of the conversation store. Workers receive effects the coordinator has already admitted. A disconnected worker does not move its effect onto another machine. That matches the documented jessfraz/agent rule that disconnects never silently move an effect, and it matches the specification's fail-closed default.

The direct hook runner does not require the conversation store. `ahi run` remains a local, offline check. A `pre_commit` action, when the actions engine evaluates one, calls that same runner. The v0.1 invocation does not require a coordinator.

### Layer responsibilities

| Layer | Owns | Does not own |
| --- | --- | --- |
| Coordinator | Process lifetime, the single writer, task routing, client admission, source polling when enabled, model presets and role defaults, gateway catalog refresh | Model HTTP inside a worker, sandbox filters, hook-repository downloads |
| Conversation store | Atomic commits of entries, documents, tasks, and submissions | Tool execution |
| Policy and hook engine | The verdict for one action, including rewrite | The decision to hide a denial from the audit |
| Sandbox | The process boundary for an effect that was approved | A second Landlock or seccomp filter inside OpenShell |
| Distribution | Installing and replacing the ahi binary and other first-party release artifacts | Toolchains and third-party packages |

## What is borrowed, and what ahi does instead

### From jessfraz/agent

Read at commit `e671f61d54ffe3b1361db79e7cc79a68ac252b23`. The README describes a Rust command-line program around Pi Durable. The architecture document states that Pi Durable owns conversations, model turns, durable requests, and local SQLite, and that Rust owns configuration, process lifetime, source adapters, and filesystem effects. A small TypeScript adapter joins them. `src/runtime.rs` starts that adapter with `Command::new("node")` on `runtime/dist/src/bridge.js`, and it holds an exclusive `writer.lock` that the child inherits so a dead supervisor does not admit a second writer.

| Topic | What the public repository documents or implements | What this proposal does |
| --- | --- | --- |
| Coordinator | One selected machine owns conversations, settings, intake, and the main agent. Clients and workers use that store. | Same ownership split. Worker attachment is a catalog of transports. The local socket and Tailscale are both entries. Tailscale is not required, and ahi does not install it. |
| Tasks | `TaskRecord` in `src/protocol.rs` has a stable id, conversation id, title, outcome, summary, model selection, state (`open`, `waiting` with actor and next check, or `resolved` with evidence), worker state, source links, pending inputs, and child agent records. | A task keeps a stable id, one owning conversation, an intended outcome, source links, and an explicit next actor or resolution evidence. Child work is a child conversation or task, not a terminal tab. |
| Workers | A worker announces a Rex host id, executable, and launch paths, and keeps a reverse connection. `WorkerRef` is host, session, window, and block. Offline workers keep their tasks. The MVP does not transfer live work. | A worker is an ahi process that accepts admitted effects and runs them under the sandbox. It does not require Rex. Offline workers keep their assignments. Live work is not transferred. |
| Sessions | Each task gets one named Rex session. Closing the session allows reopen without losing the conversation. Rex owns terminal lifetime. | The conversation is the durable session. A terminal multiplexer is not part of the runtime. Reopening a worker reattaches to the task; it does not create a second conversation. |
| Event intake | `src/sources/mod.rs` defines a `Source` trait. Adapters exist for mail, calendars, GitHub, Slack, and macOS Messages, using existing authenticated command-line tools. Events are admitted before a checkpoint advances. Coverage can be complete, partial, or unavailable. | The same admission rule: durable admit, then cursor. The trait can exist before any adapter is enabled. Adapters are off by default. ahi does not install those command-line tools. macOS Messages is not a first milestone. |
| Authorization carries forward | README: "Authorization carries forward; an approved action should not trigger another approval loop." Architecture: a yes to a concrete batch covers completing and verifying that batch; another tool call does not require another approval; source content and imported history cannot grant authority; requested or standing authority carries through the task and its workers. The same document says typed tool contracts are not a process sandbox. | A grant is a stored rule with a subject, an action constraint, and a bound. A later action that matches it is approved without a new prompt. An action outside the grant is denied. Model text and source events cannot write a grant. The sandbox, not the type of the tool, contains the process. |
| Recovery | Persist allocation intent before creating a session. A missing receipt is uncertain and is not replayed. History sync must never create an execution. | Same for effects: record the intent in the commit that approves the action; an unknown external result stays unknown; a restart does not repeat it. |
| Install | `nix profile add github:jessfraz/agent`. Private settings stay outside the repository. | `cargo install ahi`, or an attested archive. Nix is not required. Private conversations stay outside this repository. |
| Phone client | `ios/README.md` describes a SwiftUI client for the main conversation over the coordinator's authenticated connection, with a private TestFlight distribution. | No phone client in this repository during the actions, sandbox, store, and coordinator work. The owner deferred it. |

This research did not find a grant record beside `TaskRecord` in `src/protocol.rs`. The carry-forward principle is stated in the README and the architecture document. A search of the Rust tree found Tailscale peer checks and source-helper authorization, not a separate approval ledger. That search does not prove the TypeScript runtime lacks every check. **Unverified:** whether jessfraz/agent enforces the principle in code or only in model instructions.

### From the owner's comparison of the private copy

Per the owner's comparison, the private copy keeps the public split: Rust owns types, the wire protocol, and the command-line interface; the TypeScript runtime owns provider construction and execution. New Rust types are mirrored by generated bindings. ahi removes that split. The designs below are the ones the comparison marks to carry, adapted into the Rust runtime, except where a later owner decision drops the command or the alias. Carry-over is in scope only where the license permits it. See [Licensing and attribution](#licensing-and-attribution).

| Comparison verdict | Design, per the owner's comparison | What ahi adopts |
| --- | --- | --- |
| Carry | Coordinator settings send model preferences and the gateway registration to workers with bootstrap. | The coordinator is the source of those settings. A worker does not invent a model or a gateway. |
| Carry | `ModelPreferences`: a map of preset label to model selection, plus roles `chat`, `delegation`, and `background`. Labels are trimmed and non-empty. A role label must name an existing preset. Resolution takes a label and a fallback. | The same three roles and the same validation. Example preset labels in the comparison are `plan`, `build`, `simple`, and `feature`. Those labels are examples, not a required set. |
| Carry | Scheduled-job model is optional. A job with no model uses the background role. Setting a provider, a reasoning effort, or a fast mode without a model is a validation error. An explicit job model still wins. | The same inheritance rule, once scheduled jobs exist. They are not a first milestone. |
| Carry (adapt) | `ModelProvider` gains `Custom(ProviderId)`. The identifier is a validated ASCII string on the wire, not a fixed enum. | A model selection is `gateway/model`. The first segment is the provider id. The second is the model. There is no second name for either segment. See [Gateway and model names](#gateway-and-model-names). |
| Carry | Gateway fetch is native HTTP. HTTPS only, except a literal loopback origin. The request carries no credential. Redirects are not followed. Timeout 15 seconds. Body cap 1 MiB. The catalog schema is strict: OpenAI-compatible completions, an API-key field that is a gateway marker rather than a secret, zero costs, and capability flags. | The same origin contract. The catalog path in the comparison is `<origin>/pi/agent/model.json`. |
| Carry (adapt) | Each gateway keeps its last accepted catalog when a refresh fails. Registrations persist. A gateway's local name and the provider identifier in the catalog are different fields. Legacy single-URL cache files are migrated in place. | Retain the last-good catalog per registration. Reimplement validation in Rust. Do not take a JavaScript schema library. ahi does not keep a local name beside the provider id. The catalog's provider id must equal the gateway id used to fetch it. A mismatch is a rejected catalog, not an alias. |
| Carry (adapt) | Worktree allocation is recorded. A refused cleanup stays held, and the refusal is recorded. | An allocation and a refused cleanup are action receipts. Cleanup does not run unless the engine approves it. The held state remains until a later receipt says otherwise. |
| Carry | Login always prints the authorization URL. It may try a browser, then the default browser. Failure to open a browser does not abort the pending login. | This is the OpenAI browser login. `ahi login` prints the URL on standard error, binds the numeric-loopback callback before opening the browser, and accepts a pasted full callback URL with matching state and issued client ID at the same time. Headless and SSH sessions stay able to finish sign-in. The comparison says this path was verified live on the private copy. This environment did not verify it. |
| Drop for ahi | A managed-settings apply overlays private machine configuration on declarative settings. | The owner has no settings subcommand in mind. This proposal does not add `ahi settings` or `ahi settings apply`, and it does not define a settings file format. Credentials and gateway origins still do not belong in a git repository. |
| Drop | Provider construction through `pi-ai`, JavaScript schema validation, and generated TypeScript bindings. | Replaced by the Rust provider interface and Rust types. |
| Drop | Node bridge dispatch of gateway commands. | No Node bridge and no sidecar. Gateway fetch and provider calls are Rust. |
| Drop | The terminal selection-picker implementation copied from the private TUI. | The single-machine harness has its own provider and model picker, `ahi tui`. It is not a port of the private terminal, and it does not start a conversation. |
| Drop | iOS edits in the private copy, and its skill and handoff files. | Out of scope. See the non-goal on private state. |

The comparison also says slash commands work in the plain line interface, not only in the terminal interface. ahi's first coordinator commands are line commands. A picker is not required for them to run.

### From Pi Durable

The published package `@earendil-works/pi-durable` describes a harness that commits conversations, model turns, tool calls, and application state before showing them. If the process dies mid-turn, reopening the storage continues from the last commit. A model request the process died during counts as a failed attempt and is retried under the package's retry settings. The README marks the API experimental and says it changes without notice.

Borrowed invariants for the Rust-native store:

- One open storage, one writer, atomic commits. Nothing is shown before its commit is stored.
- A conversation is a transcript of immutable entries. A handle is an id.
- A task is a durable state machine with a checkpoint. Every task has an owner: its conversation, or another task.
- A submission has an optional `requestId`. Repeating that id returns the existing submission.
- Unsafe external effects are not replayed blindly. Pi Durable's tool hooks and jessfraz/agent's architecture both treat a lost receipt as uncertain. ahi keeps that rule for pushes, outbound messages, and other mutations.
- Forks branch from an entry. Compaction and reset change what the model sees next. They do not delete the earlier entries. The README states that the model sees entries from the newest reset onward, and that compaction summarizes older context.

Deliberately different:

- ahi does not adopt Pi's package installer. The fetched [pi.dev](https://pi.dev/) page shows `pi install npm:@foo/pi-tools` and `pi install git:github.com/badlogic/pi-doom`, and an install line `curl -fsSL https://pi.dev/install.sh | sh`. ahi does not grow an installer for third-party packages, and it does not use a pipe-to-shell install. The existing specification already refuses `curl | sh` for hook downloads.
- ahi does not vendor Pi's provider list or Pi's default model list. The same page names Anthropic, OpenAI, Google, Azure, Bedrock, Mistral, Groq, Cerebras, xAI, Hugging Face, Kimi For Coding, MiniMax, NVIDIA, OpenRouter, Ollama, "and more." The first provider ahi implements is OpenAI. Another gateway is a `gateway/model` id whose origin comes from `models.json`, not an entry copied from that list.
- ahi's hook engine is the policy boundary. Pi Durable hooks can block or rewrite a tool (`beforeTool`) and replace a result (`afterTool`). That is a useful shape. ahi's rules are configuration the operator declares, audited as grants, and tied to the sandbox. They are not only extension code loaded into the process.
- The hook cache (`ahi::store`) stays a content-addressed blob store. The conversation store is a different module and a different file.

### Shapes that are not the Pi Durable model

Three other Pi formats showed up in research. They are not the model this proposal follows. The owner chose the Pi Durable logical model. These stay out.

| Shape | Where it was read | Why it is separate |
| --- | --- | --- |
| Coding-agent session JSONL | [pi.dev session format](https://pi.dev/docs/latest/session-format) and `packages/coding-agent/docs/session-format.md` | A JSONL tree of session entries for the coding-agent CLI. Parent links, compaction, and system messages that patch prompt sections. Not the SQLite schema below. |
| AgentHarness specification | `packages/agent/docs/harness.md` on `main` | Specifies entries, registers, and a usage ledger, with a SQLite sketch (`entries`, `registers`, `usage_ledger`, branch tables, `writer_lease`). The document says: "The incompatible format-4 code currently in the source tree is unfinished and is replaced in place." |
| Earlier harness-v2 / session-backend tables | A non-`main` document and a test at an older commit, found by search | Tables such as `lanes`, `records`, and `facts`. Not re-read as the current `main` schema. **Unverified** as a shipped format. |

jessfraz/agent's bridge imports `SqliteStorage` from `@earendil-works/pi-durable/storage/sqlite`. That is the Pi Durable package, not the AgentHarness SQL.

## Conversation store

### Rust-native store

**Decision:** implement the store in Rust, in this crate. Follow the Pi Durable logical model as published in the package and as stored by its SQLite backend on `main`. The coordinator does not start a Node.js process. There is no Node bridge and no sidecar.

This matches constraints this repository already has:

- Release assets are one static musl binary with no companion runtime. A Node sidecar is a second required program. `runtime/package.json` in jessfraz/agent requires Node `>=26.12.1`.
- [docs/SPEC.md](SPEC.md) open decision 5 recommends that Node is not a v1 language and that ahi must not grow a nodeenv. The inferred container images omit Node. A sidecar would reverse that recommendation for the coordinator even if `language: node` hooks stay unsupported.
- `unsafe_code = "forbid"` and the supply-chain allow list in `deny.toml` cover the Rust graph. An npm graph (`pi-durable`, `pi-ai`, `chord`, `openid-client`, Playwright, and the rest of that `package.json`) would be a second review surface. ahi does not take that surface.
- The hook runner must keep working on machines that do not have Node. A single binary keeps `ahi run` and `ahi serve` on the same artifact.

The cost is implementation effort. A Rust store opens a Pi Durable database only if a later phase writes an importer against a pinned schema. That importer is not a requirement of the first store. Pi Durable's README says the API changes without notice. `main` has already added the task status `sleeping` (schema version 2 in `migrations.ts`). ahi owns its migrations.

### Data model sketch

This sketch follows Pi Durable on `main` at `9c9ce8547e5a88cbe834a53e9a6d83a789d08e47`. Table and column names are from:

- `packages/durable/src/storage/sqlite/schema.ts`
- `packages/durable/src/storage/sqlite/migrations.ts`
- `packages/durable/src/storage/store/tables.ts`
- record fields from `packages/durable/src/types.ts`

SQL identifiers may take a prefix (`createSqliteSchema`). The names below are the unprefixed ones. Integer primary keys are the storage ids. The JSON `record` column holds the typed record. Indexed strings that must stay lossless are stored with Pi's JSON encoding of the string. This research did not re-state every telemetry, memo, and abort-reason field. Those fields are in `types.ts`.

Catalog and identity:

| Table | Columns read from `migrations.ts` | Role |
| --- | --- | --- |
| `durable_metadata` | `singleton` (only row 1), `next_id` (text), `next_seq` | Id and commit-sequence allocation. |
| `record_ids` | `id`, `record_type` in `conversation`, `entry`, `task`, `submission`, `document` | One id namespace across record types. |
| `durable_schema` | Name only, in `schema.ts`. Column list is in Chord's migration helper, which was not read. | Applied migration versions. |

Conversations and entries:

| Table | Columns | Record, from `types.ts` |
| --- | --- | --- |
| `conversations` | `id`, `owner_conversation_id`, `owner_task_id`, `record` | `ConversationRecord`: `id`, optional `parent` (`conversationId`, `at` entry), optional `owner` (`conversationId`, `taskId`). |
| `entries` | `id`, `conversation_id`, `head`, `commit_seq`, `record` | `EntryRecord`: `id`, `conversationId`, `kind` (string), optional `model` messages, optional `data`, optional `head`, optional context `edits`, optional `byTaskId`. Entries are immutable. The README's built-in kinds include `pi.user`, `pi.assistant`, `pi.tool-result`, `pi.system`, and `pi.reset`. `kind` is a string, not a SQL enum. |

Tasks, submissions, and documents:

| Table | Columns | Record |
| --- | --- | --- |
| `tasks` | `id`, `conversation_id`, `kind`, `status`, `abort_requested`, `background`, `record` | `TaskRecord`: `kind`, `version`, `input`, optional owner task, `background`, `abortRequested`, and `state`. Schema version 2 allows status `pending`, `running`, `waiting`, `sleeping`, `completing`, `terminal`. `waiting` names task ids and a join policy. `sleeping` names a time. `terminal` carries an outcome (`completed`, `failed`, `aborted`, `orphaned`, or `faulted`). |
| `submissions` | `id`, `conversation_id`, `request_id`, `status`, `record` | Status `queued`, `placed`, `done`, or `unanswered`. Type `input`, `write`, or `continue`. `requestId` is the idempotency key, scoped to the conversation. |
| `documents` | `id`, `kind`, `family`, `key_value`, `scope_kind` (`session`, `conversation`, `task`), `owner_id`, `created_at`, `retired_at`, `record` | Mutable typed JSON. A retirement is a new fact, not an in-place deletion of history the document promised to keep. |
| `document_revisions` | `document_id`, `seq`, `kind` (`base` or `delta`), `version`, `content` | Revision log for document content. |

Built-in document kinds named in the package README, not re-derived from SQL: `pi.agent`, `pi.provider`, `pi.live`, `pi.inbox`, and `pi.usage`. `pi.provider` holds a provider-facing session id. The README says that id is a UUIDv7 and is forwarded to pi-ai as `sessionId`. It is not the integer primary key of `conversations`.

Pi Durable's README states WAL mode with `synchronous = NORMAL`. In that mode a commit that SQLite has returned can still be lost on power loss or a kernel panic, because the write may sit in the operating system's cache. A process crash still recovers it from the WAL. The database is not expected to tear. One process owns a storage. There is no cross-process locking in the package. jessfraz/agent adds the inherited `writer.lock` around its Node child. ahi keeps one writer with a lock in the Rust process and does not start that child.

ahi additions, stored as documents or as entries in that same commit stream, not as a second writer:

| Record | Proposed contents | Why it is not a Pi built-in |
| --- | --- | --- |
| Grant | Rule id, subject (task or conversation), action constraint, bound (one batch, or until a time), evidence entry id | Carries an approval forward as data. Pi's `beforeTool` hook is code, not a stored grant. |
| Action receipt | Action id, verdict, rule id, argument hash, sandbox backend, outcome `denied`, `completed`, or `uncertain` | The audit line for an effect. Unknown outcomes are not replayed. |
| Worker assignment | Task id, worker generation, host id, last presence | Replaces Rex's session, window, and block ids. |
| Source admission | Provider, account, event id, version, link or dismissal reason | The cursor advances only after this commit. |

Argument hashes, rule ids, and paths that are not secret belong in the receipt. Secret values, raw credentials, and full file contents do not. That continues the `audit.log` rule already implemented in `src/audit.rs`. The hook-runner `audit.log` stays the log for `ahi run`. It is not the conversation store.

**Decision:** maximum durability for the conversation database, including grants and external-effect commits. For this WAL database that setting is `PRAGMA synchronous = FULL` and `PRAGMA fullfsync = ON`.

`synchronous` chooses when SQLite waits for bytes to reach stable storage. `NORMAL`, in WAL mode, returns from a commit once the frames are handed to the operating system. A killed process still recovers those commits. A power loss or kernel panic can drop the newest ones. `FULL` does not return from the commit until those frames have been synced. In WAL mode, SQLite documents `FULL` as the durable setting: the extra sync is what makes a commit survive power loss. A commit that succeeded is still there after that failure, if the drive honors the flush.

SQLite also has `synchronous = EXTRA`. In WAL mode, SQLite documents `EXTRA` as the same as `FULL`. The directory sync that `EXTRA` adds applies when a rollback journal is deleted in DELETE journal mode. This store uses WAL, so `EXTRA` is not a stronger setting here. `PRAGMA checkpoint_fullfsync` is also not an additional step once `fullfsync` is on. SQLite documents that `fullfsync` makes every sync, including a checkpoint, use `F_FULLFSYNC` on macOS, and that the checkpoint flag then does not matter.

`fullfsync` is the macOS half of the same promise. On macOS, `fsync` does not always push data through the drive's own cache. SQLite's `fullfsync` flag uses `F_FULLFSYNC`, which asks for that stronger flush. On Linux the flag has no effect. `FULL` is the control that matters there. The cost is one wait per commit. This store commits grants and effect receipts, not a row per token. That wait is what keeps "this grant exists" and "this push was sent" from disappearing after a power loss. The later public jessfraz/agent tree sets both pragmas in `runtime/src/conversation-database.ts` at `e671f61`. Pi Durable's README still documents `NORMAL`. Per the owner's comparison, the private copy has neither that file nor this setting. The decision is the WAL maximum, which is those two pragmas. A drive that reports a flush it did not perform is outside them.

### Upstream drift

Per the owner's comparison, public jessfraz/agent moved after the baseline `d4ef2fb` that the private copy imported. This proposal's public reading is `e671f61`. The comparison names these additions as files the private copy never had. They are not changes made in the copy.

| Public file at `e671f61` | What this environment read | Why it still matters |
| --- | --- | --- |
| `runtime/src/conversation-database.ts` | Opens the Node SQLite database and sets `synchronous = FULL` and `fullfsync = ON`. It does not declare a new table schema in the portion read (the whole file is the opener). | The private copy is not evidence about this durability setting. Pi Durable's README still documents `synchronous = NORMAL`. The public agent tree that wraps Pi asked for full synchronization. ahi adopts `FULL` and `fullfsync` in its own store and does not adopt the Node opener. |
| `runtime/src/startup-diagnostics.ts` | Appends a private bounded log of fixed stages and numeric failure codes. The file comment says diagnostics never own recovery. | Useful as a separate log. It is not the conversation store, and it does not decide resume. |

The comparison also names iOS source-status views and tests that the private copy lacks. Those are product drift, not store schema. A re-port of the private copy onto later public jessfraz/agent would meet the dynamic provider-identifier change, which the comparison expects to conflict. That is a reason for ahi to own provider identifiers as data in Rust, rather than to track the TypeScript provider enum.

The private copy's use of `@earendil-works/pi-durable` 1.4.0, per the comparison, is the sidecar the owner rejected. It predates the public opener above. Matching that copy would freeze an older Node store. Matching public jessfraz/agent at `e671f61` would track a moving Node bridge. ahi does neither.

### Rejected: Node.js sidecar

jessfraz/agent runs Pi Durable in a Node child. `Runtime::start_with_notifications` in `src/runtime.rs` locks `writer.lock`, clears the close-on-exec bit so the child keeps the lock, and spawns `node --no-warnings <bridge.js> <state-dir>`. `runtime/src/bridge.ts` imports `Harness` from `@earendil-works/pi-durable` and `SqliteStorage` from `@earendil-works/pi-durable/storage/sqlite`. The Node process speaks a generated protocol to Rust.

The owner rejected that shape. ahi does not spawn `node`, does not ship `bridge.js`, and does not depend on `@earendil-works/pi-durable` at runtime. The static binary stays sufficient for `ahi serve`. The hook runner does not gain a Node requirement either.

## Policy and hook engine

v0.1 pins a local hook by id and exact `entry` in `ahi.policy.yaml`. Unknown keys error. Absence of the file is deny-by-default. The agent engine keeps that file's version 1 schema unchanged, so a repository that only runs hooks does not gain a new required document.

Agent rules live in a new file, proposed name `ahi.actions.yaml`. The version 1 parser of `ahi.policy.yaml` does not read it. A missing actions file denies every agent effect. It does not prompt.

The same file covers every agent action. A pre-commit run is one kind, beside a tool call, a command, a file write, a push, and an outbound message. The v0.1 hook policy file stays the pin for which local hook programs may run. It is not replaced by this file.

```yaml
version: 1
default: deny
rules:
  - id: format-check
    action: command
    match:
      argv: ["cargo", "fmt", "--check"]
    effect: approve
    sandbox: default
  - id: repo-pre-commit
    action: pre_commit
    match:
      config: ".pre-commit-config.yaml"
    effect: approve
    sandbox: default
  - id: read-file-tool
    action: tool_call
    match:
      tool: "read_file"
    effect: approve
    sandbox: default
  - id: push-main
    action: vcs_push
    match:
      remote: "https://github.com/iancleary/ahi"
    effect: deny
```

This YAML is a sketch of the contract, not a parser implemented in this change. Names of files and fields are proposals. The examples will be adapted with the contract as the engine is built. The pre-commit rule calls the existing runner. It does not invent a second pre-commit implementation.

Evaluation:

1. Build an action record. The model may request it. The model does not sign it.
2. Match rules. A `deny` match wins over an `approve` match. **Recommendation.** A rewrite is not an approval.
3. If the verdict is `rewrite`, substitute the action and evaluate the substitute once. A second rewrite denies. The depth limit stops a hook from looping.
4. If the verdict is `approve`, the sandbox profile on that rule is the profile that is applied. `sandbox: default` is the narrow profile. A wider profile is an explicit escalation.
5. If no rule matches, deny.
6. Commit the receipt in the same transaction that records the intent, before the effect runs. If the commit fails, the effect does not run.

An external hook program is allowed only under the same pin v0.1 uses for system hooks: an id and an exact `entry`, split without a shell, binary already on `PATH` or named by path. It receives the action as JSON on standard input and writes a verdict on standard output. A non-zero exit, a timeout, or JSON that does not match the verdict schema is a deny. That is the opposite of a fail-open hook. The specification already fails closed on supply-chain and sandbox boundaries. Agent hooks do the same.

Builtin rules do not need a program. They are the path that keeps the coordinator useful with no extra tools installed.

### Authorization carries forward

A grant is an `approve` rule that names a bound:

- `bound: once`, for a single action id, or
- `bound: batch`, for a set of action ids recorded when the operator approved that batch, or
- `bound: until`, a time, and only together with a match that is still specific (argv prefix, path prefix, remote, recipient).

The coordinator stores the grant when the operator records it. A later action that matches the stored constraint and is still inside the bound is approved without asking again. A different argv, a different path, a different remote, or a different recipient does not match. Source events, imported history, and assistant text do not create grants. A worker cannot widen its own grant.

Asking the operator is not a fourth verdict. A rule may set `ask: true`. On a match, the coordinator shows the concrete action and, if the operator confirms, appends a grant for that bound, then re-evaluates. The effect runs only after the re-evaluation returns `approve`. A rule without `ask` denies. The default file does not ask on every tool call.

**Decision:** `ask: true` is the only prompt, and it is enough for now. The direction is a sandbox plus the declared policy, so a later run needs less confirmation than a prompt on each effect. A configuration that asks for every action invites confirmation without reading the action.

Standing authority is a rule in `ahi.actions.yaml`, reviewed with the repository. It is not a sentence in the transcript.

## Hook contract

Action kinds in the first contract:

| Kind | When it is evaluated | Match fields (proposal) |
| --- | --- | --- |
| `tool_call` | Before a named tool runs | Tool name, argument hash, and any fields the tool declares as matchable |
| `command` | Before a process is spawned | `argv` as a list, working directory, environment names (not values) |
| `file_write` | Before bytes are published over a path | Path prefix, repository root, whether the path is a symlink |
| `vcs_push` | Before a push or an equivalent publication of git objects | Remote, ref |
| `outbound_message` | Before a message leaves the machine | Channel, recipient, and a hash of the body |
| `pre_commit` | Before the existing pre-commit runner starts | Config path. The runner, the config schema, and `ahi.policy.yaml` version 1 stay as specified |

The verdict object:

| Field | Meaning |
| --- | --- |
| `effect` | `approve`, `deny`, or `rewrite` |
| `rule_id` | The rule that fired. Required. |
| `reason` | Short text safe to show and to store. |
| `action` | Present only for `rewrite`. The replacement action. |
| `sandbox` | Present only for `approve`. `default` or an escalation the schema allows. |

In-process callers use the same structure the external program reads and writes, so a builtin rule and a pinned program cannot drift into two contracts.

`ahi hooks run` remains the existing `ahi run`: a local offline check against `.pre-commit-config.yaml`. It does not require the coordinator, and v0.2 through v0.4 do not have to emit action records. The actions contract still names `pre_commit` as one kind among the others. When the actions engine is asked for that kind, it calls this same runner. An agent does not get a separate pre-commit implementation.

## Sandbox execution

The specification's v0.4 section is the execution plan this layer uses. Summary, without replacing that section:

- If ahi is already inside an OpenShell sandbox, do not apply a second Landlock or seccomp filter. The OpenShell runtime profile denies `seccomp(SECCOMP_SET_MODE_FILTER)` with `EPERM`. Network control there is the policy proxy.
- Otherwise the host backends in the specification apply to effect subprocesses: Landlock and seccomp on Linux, `bwrap` when Landlock is unavailable, `sandbox-exec` on macOS with the limits written there, and a Windows job object that contains the process tree and does not restrict the filesystem. Builtin in-process checks stay in process.
- If the selected backend cannot enforce "no network" and the action was not approved with network, the action does not run.

Escalation is a field on an `approve` rule: extra read paths, extra write paths, or network. The engine attaches that profile to the child. A rewrite cannot add a path or a network grant that the matching approve rule does not list. Inside OpenShell, an ahi rule cannot widen the outer filesystem or the proxy. The most ahi can do there is deny. The specification already treats an in-sandbox hook run as advisory and CI as the backstop. The same sentence applies to an agent effect inside that sandbox: the coordinator, if it is the sandboxed process, cannot be the root of trust for grants the sandbox user can edit. A grant file inside the writable workspace is operator policy for a local run, and it is not a substitute for a trusted CI or a coordinator whose policy file the effect cannot write.

The `unsafe_code = "forbid"` lint is in the root `Cargo.toml`. Landlock and seccomp need privileged system calls. When v0.4 or this layer implements them, the unsafe block belongs in one reviewed module or in a dependency, with the lint exception written down. This proposal does not add that exception.

## Gateway and model names

**Decision:** a model selection is two layers, written `gateway/model` and split on the first slash. The gateway has no alias. `qwen/qwen-3.5` names gateway `qwen` and model `qwen-3.5`. The remainder after the first slash is the model id, so a model id may itself contain a slash: `lab/qwen/qwen-3.5` is gateway `lab` and model `qwen/qwen-3.5`. There is no third name and no local nickname.

The first provider is OpenAI, selected as `openai/<model>`. ahi does not ship Pi's default model list and does not embed a table of other vendors' models. The string sent to the OpenAI API is the model segment, such as `gpt-4.1` for a selection `openai/gpt-4.1`. OpenAI's API does not take the `openai/` prefix.

**Decision:** OpenAI signs in through the browser login. `ahi login` prints the authorization URL, may open a browser, and leaves the login pending when no browser opens. Headless and SSH sessions finish by using that URL. The resulting credential is stored in the coordinator's state directory. It is not written into the repository, the transcript, or an action receipt. There is still no `ahi settings` subcommand. An environment API key is not the OpenAI path this decision selects.

The name `qwen/qwen-3.5` means Qwen's public gateway and the model `qwen-3.5`. That meaning does not include the URL. The first built-in origin is OpenAI's. A later provider adds its own public origin under the same rule. Until that origin exists, a well-known public id names the gateway and cannot be called.

### Registering a gateway

The two-layer name selects a gateway and a model. It does not contain a URL. In 0.3.0 the single-machine harness separates the operator's curated selection (`models.json`) from each provider's reference catalog (`catalogs/<provider>.json`). Loading either is local-only and never requests the network. Browser login does not require either list. The operator may use the editor or explicit catalog checkbox/reorder actions to update `models.json`; browsing and refresh never write it.

1. A `models.json` file in the state directory supplies the only model ids the picker offers, in file order.
2. Explicit catalog refresh supplies reference material for choosing those IDs. It never fills the picker; only explicit checkbox/reorder actions or editor changes update the curated file.

ahi does not read `~/.pi`. Pi's directory can hold another product's credentials. ahi keeps its own `models.json`. The shape of that file is the provider map Pi documents, at the researched commit, as `~/.pi/agent/models.json`. There is no settings subcommand. The operator edits the file directly or explicitly checks/reorders models in the catalog view. No discovery or refresh step writes or rewrites that file.

```json
{
  "providers": {
    "lab": {
      "baseUrl": "http://127.0.0.1:11434/v1",
      "api": "openai-completions",
      "models": [{ "id": "qwen-3.5" }]
    }
  }
}
```

The object key `lab` is the gateway id. The selection for that model is `lab/qwen-3.5`. One entry can list many model ids. Two on-prem servers are two gateway ids, each with its own `baseUrl`. A third path segment is not required.

The first registered `api` is `openai-completions`, matching the catalog contract already carried: an OpenAI-compatible completions endpoint. A local or on-prem server that speaks that API is registered by this file. A server with a different wire protocol needs a later provider implementation. The file does not add that protocol.

A literal loopback `baseUrl` may be `http`. Any other origin is `https`. Redirects are not followed. The timeout is 15 seconds and the body cap is 1 MiB, the same bounds as a fetched catalog. The example above is loopback. This repository does not gain a private origin.

`apiKey` in this file is a non-secret marker for a server that ignores it, or an environment reference `$NAME`. The file does not run a shell command. A long-term secret is not written into it. Optional headers, when a server requires them, use the same environment reference.

An entry whose key is `openai` may curate models, but browser-token refresh requires `baseUrl: https://api.openai.com/v1`; conflicting origins fail explicitly. Browser credentials are never forwarded to a proxy. Register proxies under a separate gateway id. An OpenAI entry still supplies `baseUrl`, `api`, and optional `models`; its legacy `api: openai-completions` configuration label does not implement inference or prove that the browser grant authorizes completions.

### Discovering models from a gateway

The operator's curated list can drift when a provider adds or removes models. Explicit refresh keeps a separate reference catalog so the operator can compare ids without replacing their selection. The same rule applies to OpenAI and gateways: the picker is exactly `providers.<id>.models` in `models.json`, not a default catalog plus a second scoped list.

The fetch is the origin contract from the comparison. The request carries no credential. Redirects are not followed. The timeout is 15 seconds and the body cap is 1 MiB. The catalog schema is strict: OpenAI-compatible completions, an API-key field that is a gateway marker rather than a secret, zero costs, and capability flags. The comparison names the catalog path `<origin>/pi/agent/model.json`. The catalog's provider id must equal the gateway id. A mismatch is rejected. It is not an alias. Refresh failure keeps the last accepted catalog. The model id in the catalog is the string sent upstream. The selection syntax stays the two-layer form.

**Breaking change in 0.3.0:** a `models.json` entry can omit `models`, but then offers no models, even after a successful refresh. Its picker row explains `no models in models.json: press e to edit, c to browse the catalog`. Configured ids are never replaced or silently dropped. Each is marked `listed`, `absent from catalog`, or `catalog unavailable`. The old `kept-config` fallback is removed because configured ids are always the picker list.

Refresh happens only on `r` (all providers) or `ahi tui --refresh`. In a non-terminal run, `--refresh` refreshes then prints the local load; any refresh failure makes that invocation exit 1. A failed refresh leaves the previous catalog file byte-for-byte intact and displays an error for the current invocation. Successful refresh writes `<provider>.json.tmp`, syncs it, and renames it over `<provider>.json`. Exclusive temp creation prevents simultaneous refreshes from writing the same temp file; a stale `.tmp` is reported, not silently overwritten. A catalog object records `provider`, `source`, `retrieved_at` (RFC 3339 UTC), and `models`. The old bare-array format remains readable with unknown source and retrieval time. The UI's `retrieval time unknown (old catalog format)` label refers only to a missing cache timestamp, not the OAuth flow.

**OpenAI is the credentialed refresh exception to the gateway rule above.** Explicit refresh uses a new direct-token credential for best-effort `GET https://api.openai.com/v1/models`. Requests follow no redirects, time out after 15 seconds and cap bodies at 1 MiB; token values and response bodies are never printed or written to catalogs. Expiry is checked before requests and one authentication renewal/retry is allowed per invocation, persisting rotation first. There is no startup request, Codex fallback or copied Pi model list.

**New versus old OpenAI sign-in:** Pi's current `openai` uses the direct-token ChatGPT grant for the Platform Responses API, not Decisions; its distinct `openai-codex` provider uses the older fixed public client and ChatGPT backend. ahi now implements the current flow: `/api/accounts/authorize` with `dynamic_agent_client`, stable public installation UUID (`openai-device-id`), PKCE/state/nonce, `resource: https://api.openai.com/v1`, and `chatgpt.tokens.use.direct`. It binds numeric-loopback `http://127.0.0.1:1455/auth/callback` before presenting the URL, and requires code, matching state and the issued client ID in either automatic callback or pasted full URL. Code exchange/refresh uses `/api/accounts/oauth/token` and the issued client ID/resource. Bare-code paste is no longer accepted. Credentials retain access/refresh/TTL fields and add flow, issued client, scopes, resource and absolute expiry with a three-minute margin; ID token/profile/account data are not persisted. Old credentials remain intact until a validated new login succeeds, and explicit refresh tells their owner to run `ahi login` to migrate. A failed login never overwrites them.

The earlier manual Platform model-list probe returned HTTP 403 **with an old Codex-flow credential**. It says nothing about the new flow's model-list permissions. A subsequent live `/v1/models` request with the new direct-token credential returned HTTP 200 and `models[].slug` metadata. ahi accepts that schema and standard `data[].id`, keeping all reported IDs in order. The interactive browser flow itself has not been exercised by the automated checks. A catalog 403 reports a grant/discovery limitation and preserves all cached and curated IDs. Pi's provider bundles model definitions; ahi does not copy them. No inference or conversation runner was added. Do not concurrently rotate one credential from multiple ahi processes; atomic replacement prevents partial files but is not a refresh-token transaction lock.

Primary sources: Pi commit `1708b47889664ec9838f6734ff57df58b079a50a`, [current ChatGPT OAuth](https://github.com/earendil-works/pi/blob/1708b47889664ec9838f6734ff57df58b079a50a/packages/ai/src/auth/oauth/openai-chatgpt.ts), [OpenAI provider](https://github.com/earendil-works/pi/blob/1708b47889664ec9838f6734ff57df58b079a50a/packages/ai/src/providers/openai.ts), and [separate legacy Codex provider](https://github.com/earendil-works/pi/blob/1708b47889664ec9838f6734ff57df58b079a50a/packages/ai/src/providers/openai-codex.ts). Verified against source and local fixture tests, not a real new-flow login.

`c` opens the selected provider's catalog curation view. Checked models lead in saved picker order, followed by unchecked reference IDs. Space toggles inclusion and saves; Shift+Up/Down (or K/J) reorders checked IDs and saves. Missing-catalog IDs remain checked, visible, reorderable and removable. `y` copies an ID with an available clipboard tool or prints it on exit. No clipboard package is installed. Escape returns to the main picker.

`e` leaves the alternate screen and disables raw mode, then invokes `$VISUAL`, falling back to `$EDITOR`, using ahi's existing shlex splitter (so `code --wait` works). Windows defaults to `notepad`; other systems ask for `VISUAL` or `EDITOR` if neither is set. Editor-only reload never postformats or rewrites the editor's bytes. Explicit checkbox/reorder actions may create `models.json` and pretty-print JSON, preserving provider settings and object-key order. A stale external edit or exclusive-temp conflict leaves the file and loaded registry unchanged and reports recovery steps. Saves compare the original bytes before preparing and again before atomic replacement; a noncooperating external editor can still race the final check and rename. Browsing or pressing `e` with no editor never creates the file. After the editor exits, ahi restores the terminal and reloads local files without fetching. A parse or validation failure retains the previous registry and displays the path and line; `e` can reopen the broken file, which remains untouched.

The single-machine harness implements this load. It does not implement a coordinator, a worker, or a conversation store. OpenAI login does not require `models.json`. A gateway does.

## Coordinator, tasks, and workers

Proposed commands, additive, and absent until a later phase implements them. There is no `ahi settings` command in this list.

| Command | Role |
| --- | --- |
| `ahi serve` | Run the coordinator. One writer. Listens on the transports configured from the catalog below. Refuses to start if the store lock is held. |
| `ahi worker` | Connect to a coordinator through one catalogued transport and execute admitted effects. Does not open the store for write. |
| `ahi task list` and `ahi task show` | Read the task registry through the coordinator. |
| `ahi actions check` | Evaluate one action against `ahi.actions.yaml` and print the verdict. Does not execute it. |
| `ahi update` | Replace this ahi binary from an attested archive this repository publishes, after verification. Not a hook-repository command. |
| `ahi tui [--refresh]` | Load curated models and reference catalogs locally. Explicit `--refresh` or `r` refreshes catalogs. Enter saves `gateway/model` to `selection` and exits with a confirmation; it does not start a conversation. A pipe prints the load and membership diagnostics. |
| `ahi login` | OpenAI browser sign-in. Prints the URL, may open a browser, and stores the credential in the state directory. |
| `ahi hooks install` / `ahi hooks uninstall` | Git hook shim. Stubs until that phase. |

`ahi serve` with no configuration starts a coordinator that has no provider and no sources, and listens on the local socket. That process can still answer `ahi actions check` once the actions file exists. It does not open a network connection by merely starting.

Worker attachment is a catalog of transports. Each entry is a way a worker reaches the coordinator. The entries are the same kind of choice:

| Transport | What it is |
| --- | --- |
| Local socket | A socket on the coordinator machine. No extra software. This is the entry an unconfigured `ahi serve` listens on. |
| Tailscale | An optional transport that uses a Tailscale network the operator already has. ahi does not install Tailscale and does not require it. |

Model selection, per the owner's comparison and adapted as above: presets name concrete `gateway/model` selections, and the roles `chat`, `delegation`, and `background` point at presets. Bootstrap sends the resolved selection to a worker. A worker does not invent a gateway. A scheduled job, when that feature exists, uses the background role unless the job names its own model. The first concrete provider behind a preset is OpenAI.

`ahi login` is the OpenAI browser sign-in. It prints the authorization URL and leaves the login pending if no browser opens. The stored credential stays in the state directory. A registered local or on-prem gateway uses the `models.json` marker or `$NAME` reference, not this login.

Worktree allocation is an admitted effect. The receipt records the owner. A cleanup that the engine refuses stays recorded as held.

Task record, molded from the fields actually present on jessfraz/agent's `TaskRecord`, without Rex:

| Field | Purpose |
| --- | --- |
| `id` | Stable. Never reused. |
| `conversation_id` | The transcript for this task. |
| `title`, `outcome` | What the operator asked for, and the current statement of done. |
| `state` | `open`, `waiting` (actor and optional next check), or `resolved` (evidence). |
| `sources` | Links that were admitted. Linking is not execution. |
| `worker` | Unassigned, or assigned to a host generation. Uncertain if a receipt was lost. |

**Decision:** routing uses an exact task id or an exact source link, or an explicit operator choice. jessfraz/agent documents keyword and semantic search over a rebuildable index. The first ahi coordinator does not. A similarity match does not select a task and does not select a repository for a push.

Workers: the coordinator persists the assignment before it sends the effect. The worker returns a receipt. A missing receipt leaves the action `uncertain`. The coordinator does not send it again. The operator, or a rule that only reads, may inspect the outside system and record what happened. That inspection is itself an action and goes through the engine.

Event intake, when enabled, follows the public `Source` shape: a key of provider and account, a poll that returns events plus a checkpoint candidate plus coverage, and a coordinator that commits the events before it stores the checkpoint. A tool must link each event to a task or dismiss it with a reason. Prose does not clear the queue. Failed polls stay visible as coverage gaps and do not look like an empty inbox.

No source is enabled by an empty configuration. GitHub, mail, and Slack are candidates because jessfraz/agent implements those adapters with `gh` and other existing tools. They are not part of the first milestone. Each adapter, when it exists, uses credentials the operator already has. ahi does not install the tool. Inbound content is data. It is not a grant.

## Threat model

The specification's threat model still applies to the hook runner: untrusted hook repositories, a config change, a hostile file in the tree, and a local race on the cache. This section adds the agent runtime. It does not weaken the earlier model.

Additional assets: the conversation store, grants, action receipts, provider credentials, outbound message bodies, and the worker socket.

Additional adversaries:

1. Model output. It can request a tool call. It cannot approve one, rewrite a rule file, or mark an effect `completed`.
2. Tool results and file contents fed back into the transcript. They are untrusted data on the next turn.
3. Inbound source events (mail, Slack, GitHub). They cannot create a grant. They can at most become a task the operator's rules already allow the coordinator to open. Opening a task is not permission to push or to send mail.
4. A worker that has been instructed, by a prompt, to perform an effect the coordinator did not admit. The worker runs only admitted effects. Anything else is a defect.
5. A sandbox user who can edit `ahi.actions.yaml` inside an OpenShell workspace. Same backstop as the specification: that run is advisory. Enforcement for shared repositories is a runner whose policy the workspace cannot rewrite.

Boundaries:

| Boundary | Rule |
| --- | --- |
| Conversation store | One writer. Commits are atomic. A crash resumes from the last commit. External mutations with outcome `uncertain` stay uncertain. |
| Grants | Written by an operator action or by a file the operator placed. Not written by the model or by a source event. |
| Network | Denied in the default sandbox. `ahi serve` does not open a provider connection until a provider is configured. Source polls run only for configured accounts. |
| Credentials | Not copied into the transcript, the receipt, or `audit.log`. OpenAI's credential comes from `ahi login` and stays in the state directory. A registered gateway's secret is an environment reference in `models.json`, not a literal in that file or in the repository. A gateway catalog fetch carries no credential. |
| Hook programs | Pinned entry, no shell, fail closed, scrubbed environment using the name rules already in the specification. |
| Distribution | ahi replaces only its own release artifacts, and only after checksum and attestation checks. It does not execute a downloaded install script as the trust root. |

Out of scope, as in the specification: a kernel exploit, and a person who already controls the account that runs ahi. The sandbox contains effects. It does not contain that person.

## Crate and workspace layout

The repository is one package, `ahi`, with a library and a binary. `scripts/cut-release.sh` and the publish workflow assume that root manifest. `fuzz/` is already a detached workspace.

**Decision:** stay one crate. Add modules only when a phase implements them. A workspace, or several published crates, is not the plan. The release script publishes one package. Split later only if compile time or a real library consumer requires it.

| Module | When | Responsibility |
| --- | --- | --- |
| `ahi::policy` | exists | Version 1 hook policy. Unchanged. |
| `ahi::store` | exists | Hook cache. Unchanged role. |
| `ahi::actions` | first agent phase | Parse `ahi.actions.yaml`, evaluate a verdict. |
| `ahi::sandbox` | with v0.4, then reused | Detect an outer sandbox. Apply a host backend to a child. |
| `ahi::conversation` | store phase | The Rust-native store. Not the hook cache. Not a Node child. |
| `ahi::coordinator` | coordinator phase | Lock, transport catalog, tasks, worker admission. |

This document does not split the crate. A workspace of internal crates, or several published crates, would change the release script. That change is not part of this decision.

## Distribution

Three paths, all for ahi itself:

1. **crates.io.** `cargo install ahi --locked`, once a release is published. The name check in the specification (HTTP 404 from crates.io on 2026-10-25) was not repeated successfully. On 2026-11-03 a request to `https://crates.io/api/v1/crates/ahi` returned HTTP 403 with an empty body. **Unverified** whether the name is still free.
2. **Attested archives.** The layout and the `gh attestation verify --repo iancleary/ahi` commands in [docs/release.md](release.md) stay the Linux `x86_64` and `aarch64` archives. **Decision:** Linux, macOS, and Windows all use `cargo install ahi`. This proposal does not add macOS or Windows archives. The Linux archives remain the extra path they already are.
3. **ahi.dev.** The brief names this domain for an install document and for project documentation. In this environment `getent hosts ahi.dev` returned no address, and a request to `https://ahi.dev` produced no HTTP response. **Unverified** whether the domain is registered or serving. The document that should live there, when the domain is ready, is the same install and verify procedure as the README. It is not a second installer with a different trust model.

`ahi update` downloads an attested archive for a target the release workflow publishes, checks `ahi-<version>-sha256sums.txt`, and verifies the attestation against `iancleary/ahi`. It then replaces the ahi executable. It refuses a target it cannot verify. macOS and Windows stay on `cargo install`. It does not run a remote shell script. It does not accept a package name argument. The command is specified and not implemented. First-party components are artifacts this repository's release workflow publishes. A hook repository, a language runtime, and a Pi package are not first-party components.

The pipe-to-shell line on [pi.dev](https://pi.dev/) is the pattern this section does not use.

## Compatibility and semantic versioning

The root package is `0.1.3`. **Decision:** agent work continues that same version line. There is no second version for the runtime, and this document does not pick the next number or cut a release. The script in the tree implements explicit SemVer. [docs/SPEC.md](SPEC.md) still has its own open question of SemVer versus calendar versioning. This proposal follows the script already in the tree.

0.x permits breaking changes. 0.1.x users still need a compatibility story, because `cargo install ahi` tracks the latest release and the attested binary is how operators upgrade.

| Surface | Compatibility rule |
| --- | --- |
| `ahi run` and its flags | Remain. Behavior stays the specification's behavior. |
| `list-checks`, `explain`, `doctor`, `migrate` | Remain top-level. |
| `install`, `uninstall` | Not top-level commands. The hook shim, when it exists, is `ahi hooks install` and `ahi hooks uninstall`. Those stubs exit 1. |
| `update` | Replaces the ahi binary from an attested archive. This version exits 1. Agent work does not use this name for hook pins. `self-update` is not an alias. |
| `lock`, `verify`, `fetch`, `autoupdate` | These names are not commands. ahi does not install or move hook repositories. |
| `ahi hooks run` | An alias of `ahi run`, added when the grouped surface is added. The same alias pattern can cover the other hook commands. Aliases are additive. |
| `ahi.policy.yaml` version 1 | Unchanged. Unknown keys remain errors. Agent keys do not go in this file. |
| `ahi.actions.yaml` | New. Ignored by `ahi run`. |
| Default `ahi` with no subcommand | Keep today's help text. Do not make a bare `ahi` start a coordinator. |
| Store files | The hook cache layout stays as specified. The conversation database is a new file under the coordinator's state directory, not inside the repository being checked. |
| SemVer | Adding commands and a new file is a minor bump once the project is 1.0. Before 1.0, the same additions still must not change `ahi run` exit codes or the version 1 policy schema. A major version is the point at which the conversation schema and the actions schema freeze. It is not a point at which `ahi run` is removed. |

**Recommendation:** keep `ahi run` through 1.x. The grouped `ahi hooks` command is the name new documentation uses. Both work. Removing the top-level hook commands would be a break this proposal does not ask for.

## Milestones

These phases sit after the hook-runner work. They ship as later ahi releases on the version line the crate already uses. The labels below are phase names. This document does not assign the next version number.

| Phase | Relation to the roadmap | Delivered behavior |
| --- | --- | --- |
| Specification phases v0.2, v0.3, v0.4 | v0.2 is withdrawn. v0.3 is the single-machine harness. v0.4 is the sandbox. | OpenAI and a custom gateway in `ahi tui`, then sandbox detection. No new builtins in v0.3. No lockfile and no language installer. |
| Actions | Uses the local system and script process builder that already exists. It does not wait on a language installer. | `ahi.actions.yaml`, including `pre_commit` beside tool calls and the other kinds, `ahi actions check`, deny-by-default, `ask: true` only where a rule sets it. `ahi hooks run` alias. `ahi run` unchanged. |
| Sandbox for effects | After v0.4, reusing its detection and backends. | An approved `command`, `file_write`, or `pre_commit` runs in the default sandbox. Escalation only by rule. The direction is less prompting as the policy and the sandbox cover the effect. Tests follow the specification's Linux, macOS, and Windows honesty table. |
| Conversation store | After the actions phase. | Rust-native commits, resume, one writer, grants and receipts, `synchronous = FULL` and `fullfsync = ON`. No Node child. No provider required to test resume. |
| Coordinator | After the store. | `ahi serve`, `ahi worker`, task registry, the transport catalog with the local socket as the unconfigured listener, exact task id or exact source link, uncertain-outcome rule. No source adapters. OpenAI is the first provider, with browser login. `models.json` and gateway discovery are specified above and are not required for that first provider. Neither runs merely because the process started. |
| Intake | After the coordinator, one adapter at a time, each off by default. | Admit-before-cursor. A dismissal reason is data. The adapter does not install a tool. |
| Distribution | When a release is cut. Not part of implementing the phases above. | `cargo install ahi` on Linux, macOS, and Windows. ahi.dev documents that path and the existing attested Linux archives. `ahi update` only for an archive the release workflow publishes. |

Local `language: system` and `language: script` hooks are the external programs the runner already executes. v0.4 is the right predecessor for effect sandboxes because nesting policy is already specified there. Starting a second sandbox design beside v0.4 would fork the threat model.

## Licensing and attribution

The owner conditioned carry-over from jessfraz/agent and from the private copy on the license permitting it. This section states the license texts that were read and the attribution those texts require. It is not a license opinion, and it does not relicense anyone's code.

ahi today: root `LICENSE` and `Cargo.toml` say MIT. The copyright line is `Copyright (c) 2026 Ian Cleary`. The MIT condition in that file is that the copyright notice and the permission notice appear in all copies or substantial portions of the software.

jessfraz/agent: `LICENSE` at public baseline `d4ef2fb5af0b44531e771a6c3ba4bb7959caf966` and at `e671f61d54ffe3b1361db79e7cc79a68ac252b23` is the MIT license. Both files begin `Copyright (c) 2026 Agent contributors`. The condition read in the file is the MIT condition: the copyright notice and the permission notice must be included in all copies or substantial portions of the software. <https://github.com/jessfraz/agent/blob/d4ef2fb5af0b44531e771a6c3ba4bb7959caf966/LICENSE>

Pi: `LICENSE` on `main` is the MIT license. The copyright line read from that file is `Copyright (c) 2025 Mario Zechner`. The same MIT condition follows: include the copyright notice and the permission notice in all copies or substantial portions. <https://github.com/earendil-works/pi/blob/main/LICENSE>. The `license` field of `packages/durable/package.json` on `main` is `MIT`. That field was read on 2026-11-03. The package version in that file was `1.5.0`, which is newer than the `1.4.0` pin in jessfraz/agent. The field is not a claim that every Pi file header was checked.

This proposal copies neither repository's source. Describing a data model or an origin contract in this file does not attach those copyright lines to this file. A later change that copies or substantially reproduces source from either project must keep that project's copyright notice and permission notice with the copied portion. The private copy is not a license source. It was not read, and its texts are not available to attribute.

**Decision:** ahi stays MIT, as the `LICENSE` file and `Cargo.toml` already say. This proposal does not add `LICENSE-APACHE`. [docs/SPEC.md](SPEC.md) still writes dual MIT OR Apache-2.0 as an open specification question. This change does not edit that file. The choice is about ahi's own license. It does not remove the MIT notice from a portion that came from jessfraz/agent or from Pi.

## Recorded decisions

The owner recorded these on review of this document. They are decisions for the proposal. The single-machine harness in [docs/SPEC.md](SPEC.md) implements OpenAI login, `models.json`, and catalog fetch. The store, the sandbox, and the coordinator are not in the tree yet.

### 1. Conversation store

Rust-native, following Pi Durable's logical model. No Node.js sidecar and no Node bridge. Byte-level import of a Pi database is later work, not a requirement of the first store. Coding-agent JSONL and the AgentHarness register schema are not this model. Durability is the WAL maximum: `synchronous = FULL` and `fullfsync = ON`. `EXTRA` matches `FULL` in WAL mode. The nuance is in [Conversation store](#conversation-store).

### 2. Crate

One `ahi` crate, with new modules when a phase implements them. The release script keeps publishing one package.

### 3. License

MIT, as the file in the tree already says. This proposal does not add `LICENSE-APACHE`. Upstream portions, if a later change copies them, keep the MIT notices in [Licensing and attribution](#licensing-and-attribution).

### 4. `iancleary/agent`

The owner will retire it. It was exploratory. Public ahi does not depend on it and does not ingest the private state listed in the comparison's privacy section: runbooks, tailnet names and addresses, gateway origins, session material, conversation text, state-directory contents, embedding data, or personal instruction files. One line in the private repository names a machine, per the comparison; this document does not repeat it. This proposal does not request access and does not publish that tree.

### 5. iOS client

Deferred. No iOS target in this repository during the actions, sandbox, store, and coordinator work. The comparison's iOS delta is documentation, and it is not carried. A later client, if one is built, talks to the coordinator and does not hold the store.

### 6. Providers

Do not use Pi's default model list. The first provider is OpenAI, selected as `openai/<model>`. Sign-in is the browser login: `ahi login` prints the URL and does not abort when no browser opens. The credential stays in the state directory. There is no settings subcommand.

A model selection is two layers, split on the first slash, with no alias. The state directory's `models.json` maps a gateway id to an OpenAI-compatible origin and model ids. `ahi tui` reads that file and catalogs locally; only `r` or `--refresh` refreshes catalogs. Omitted `models` means an empty picker with an explanation, not automatic catalog fill. ahi does not read `~/.pi`. The catalog's provider id must equal the gateway id. Only OpenAI has a built-in public origin in the first provider. Model-preference roles are not implemented. See [Gateway and model names](#gateway-and-model-names).

### Further decisions

| Item | Decision |
| --- | --- |
| Worker transport | A catalog. The local socket and Tailscale are both entries. An unconfigured coordinator listens on the local socket. ahi does not install or require Tailscale. |
| Interactive `ask` | Only a rule that sets `ask: true` prompts. The default is deny with no prompt. The direction is a sandbox and declared policy, with less confirmation later. |
| SQLite synchronous mode | Maximum durability. For WAL that is `FULL` and `fullfsync`. `EXTRA` is the same as `FULL` in WAL mode. |
| Provider registration | State-directory `models.json`, not `~/.pi/`. The picker offers exactly that file's ids. Catalogs are separate reference material; local load never fetches. `r` or `--refresh` explicitly refreshes them. `e` hands the file to the operator's editor; Space and Shift+Up/Down (or K/J) explicitly save checked IDs/order. Browsing and refresh never write it. |
| OpenAI sign-in | Browser login through `ahi login`. The URL is printed. A missing browser does not abort the login. |
| Where ahi runs | Linux, macOS, and Windows, through `cargo install ahi`. Existing attested archives stay Linux-only. |
| Versioning | Continue ahi's existing version line. No separate runtime version. This document does not assign the next number. |
| Task routing | Exact task id or exact source link to start. No semantic search in the first coordinator. |
| Gateway names | Two layers, split on the first slash. No alias. A catalog that disagrees with the gateway id is rejected. |
| Node | No bridge and no sidecar. |
| Settings command | None in this proposal. |
| Hook contract | One actions contract. `pre_commit` is one kind, beside tool calls, commands, file writes, pushes, and outbound messages. `ahi run` stays the local runner. |

## Sources

Fetched or cloned on 2026-11-03 unless noted.

- This repository's contract: [docs/SPEC.md](SPEC.md), [docs/release.md](release.md), root `Cargo.toml` version `0.1.4`, `src/main.rs`, `src/policy.rs`, `src/audit.rs`, `src/harness.rs`.
- jessfraz/agent commit `e671f61d54ffe3b1361db79e7cc79a68ac252b23`: <https://github.com/jessfraz/agent>. Files used: `README.md`, `docs/architecture.md`, `docs/setup.md`, `docs/roadmap.md`, `ios/README.md`, `LICENSE`, `runtime/package.json`, `runtime/src/bridge.ts`, `runtime/src/conversation-database.ts`, `runtime/src/startup-diagnostics.ts`, `src/runtime.rs`, `src/protocol.rs` (`TaskRecord`, `WorkerRef`, `SourceRef`), `src/sources/mod.rs`.
- jessfraz/agent `LICENSE` at baseline `d4ef2fb5af0b44531e771a6c3ba4bb7959caf966`: <https://github.com/jessfraz/agent/blob/d4ef2fb5af0b44531e771a6c3ba4bb7959caf966/LICENSE>. MIT, `Copyright (c) 2026 Agent contributors`.
- Owner's comparison of the private copy, comment `6594868422`: <https://github.com/iancleary/ahi/pull/6#issuecomment-6594868422>. Design facts about `iancleary/agent` are from this comment only.
- Pi `LICENSE`: <https://github.com/earendil-works/pi/blob/main/LICENSE>. MIT, `Copyright (c) 2025 Mario Zechner`. `packages/durable/package.json` `license` field `MIT`, read 2026-11-03.
- Pi repository `main` at `9c9ce8547e5a88cbe834a53e9a6d83a789d08e47`: <https://github.com/earendil-works/pi>. Package README: <https://github.com/earendil-works/pi/blob/main/packages/durable/README.md>. Schema: `packages/durable/src/storage/sqlite/schema.ts`, `packages/durable/src/storage/sqlite/migrations.ts`, `packages/durable/src/storage/store/tables.ts`. Records: `packages/durable/src/types.ts`.
- AgentHarness specification, a different model: <https://github.com/earendil-works/pi/blob/main/packages/agent/docs/harness.md>.
- Coding-agent session format: <https://pi.dev/docs/latest/session-format> and <https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md>.
- Pi product page, including the install line and the provider list: <https://pi.dev/>.
- Pi coding-agent model configuration at the same `9c9ce854` commit: `packages/coding-agent/docs/models.md`. The user file documented there is `~/.pi/agent/models.json`, with a `providers` map of `baseUrl`, `api`, `apiKey`, and `models`. Read 2026-11-03. ahi adapts the map into its own state-directory `models.json` and does not read `~/.pi`.
- How Pi works, for the coding-agent loop rather than the Pi Durable tables: <https://pi.dev/docs/latest/how-pi-works>.
- crates.io name check recorded in the specification on 2026-10-25. Not reconfirmed on 2026-11-03 (HTTP 403 from this environment).

## Unverified

- The tree of `https://github.com/iancleary/agent`. The API returned 404. Facts about it are per the owner's comparison and were not checked against the repository.
- The import commit the comparison names, and the live login check the comparison reports. The public baseline commit `d4ef2fb` was confirmed. The private commit `0b47b63` was not.
- Whether `@earendil-works/pi-durable` `1.4.0` has the same SQLite tables as `main` at `9c9ce854`.
- `packages/durable/docs/spec.md` was not read through. Chord's `CREATE TABLE` for `durable_schema` was not read.
- Whether jessfraz/agent enforces "authorization carries forward" in code, or only states it and instructs the model.
- ahi.dev did not resolve here. No site content was read.
- The crates.io name `ahi` was not rechecked. The specification's 2026-10-25 result stands as a point-in-time fact only.
- AgentHarness `harness.md` describes unfinished format-4 code in tree. This proposal does not treat that SQL as Pi Durable's format.
- Older session-backend table lists found by search were not re-read on `main`.
- OpenShell behavior is taken from the specification and was not re-fetched.
- No coordinator, store, or sandbox in this proposal was implemented or executed.
