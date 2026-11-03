# ahi agent runtime and coordinator

Status: proposal for review. This document is not the design of record. [docs/SPEC.md](SPEC.md) remains the compatibility, security, and architecture contract for v0.1 through v0.4. Nothing here changes code, the command-line surface, version numbers, release scripts, or published artifacts.

The package, the library target, and the binary stay named `ahi`. Callers continue to write `use ahi::...`. The expansion "agent hook interface" is still not confirmed. This proposal uses `ahi` as the name of a public agent runtime and coordinator that keeps the existing hook runner.

Research date: 2026-11-03. Upstream facts are taken from the pages, commits, and source files listed in [Sources](#sources). Anything this author did not re-check is marked **Unverified**.

## How to read this document

The proposal has five layers:

1. A core runtime and coordinator.
2. A durable conversation store.
3. A policy and hook engine around agent actions.
4. Sandbox execution, including the existing OpenShell plan.
5. Distribution of ahi itself.

The hook runner specified for v0.1 through v0.4 stays available. The differentiator is to generalize that hook posture, so the same kind of declared and pinned rule approves, denies, or rewrites an agent action, and so sandbox escalation is one of those rules.

Two implementation choices are presented for the conversation store. The leading option is a Rust-native store that follows the published Pi Durable data model. The documented alternative is a Node.js sidecar that embeds Pi Durable, which is the shape [jessfraz/agent](https://github.com/jessfraz/agent) uses. The choice is open. See [Open decisions](#open-decisions).

## Inferred context

The following is **inferred** from the review brief for this proposal, not from a statement recorded in this repository. Treat it as design input.

- ahi becomes the public agent runtime and coordinator. The role is comparable to the product described at [pi.dev](https://pi.dev/): a harness an operator adapts, distributed as a program the operator installs. The fetched pi.dev page calls Pi "a minimal agent harness" and says "We've shipped Pi 1.0 with Pi Durable."
- Distribution targets named in the brief: crates.io (`cargo install ahi`), the domain ahi.dev (install instructions and documentation), and the attested static release binaries this repository already defines.
- ahi installs and updates only itself and its first-party components. It does not become a general package manager. It stays useful for repositories that are not Rust projects.
- The coordinator, task and session registry, worker model, event intake, and the principle that authorization carries forward are to be molded from [jessfraz/agent](https://github.com/jessfraz/agent) and from Ian's private copy `https://github.com/iancleary/agent`. They are not to be copied. The private repository was not readable in this environment. What this document says about it comes from the owner's comparison on the review, cited below, and is marked as such.
- The conversation and provider layer follows Pi Durable at `https://github.com/earendil-works/pi`. The brief states that Ian is leaning toward a Rust-native store that follows Pi's data model, with a Node.js sidecar as the alternative.
- The differentiator is ahi's existing declared, pinned, configuration-driven hooks, generalized to agent actions (tool calls, commands, file writes, pushes, and outbound messages), combined with sandbox-by-default execution and escalation governed by that same engine.
- The existing pre-commit runner remains available, for example as an `ahi hooks` subcommand, so 0.1.x users keep working.

Observed, not inferred: the repository is `https://github.com/iancleary/ahi`. The root `Cargo.toml` at the base of this proposal is version `0.1.3`, edition 2021, license `MIT`, MSRV `1.83`, `unsafe_code = "forbid"`. [docs/SPEC.md](SPEC.md) already records an MIT `LICENSE` with `Copyright (c) 2026 Ian Cleary`.

## Research limits

`GET https://api.github.com/repos/iancleary/agent` returned HTTP 404 on 2026-11-03. This environment cannot tell a missing repository from a private repository. The private tree was not read, and this document does not repeat hostnames, addresses, gateway origins, machine names, state-file contents, or conversation text from it.

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
6. Distribute one `ahi` binary through crates.io, through documented verification of the existing attested archives, and through ahi.dev as documentation of those paths.
7. Stay useful in repositories whose languages are shell, Docker, GitHub Actions, TypeScript, Go, Python, or Rust. The runtime does not require a Rust project.

## Non-goals

- Replacing [docs/SPEC.md](SPEC.md), renumbering v0.2, v0.3, or v0.4, or pulling agent execution into those phases.
- A general package manager. ahi does not install Node, Python, Go, Rust toolchains, npm packages, Pi packages, hook repositories, or third-party agents. `ahi fetch` remains the pinned hook-cache bake specified for v0.2. It is not an installer for arbitrary tools.
- A drop-in copy of jessfraz/agent, of Rex, of Tailscale, of Nix, or of the Pi coding-agent terminal interface.
- Byte-for-byte compatibility with a Pi session file, unless the open store decision explicitly selects that. See [Conversation store](#conversation-store).
- An iOS client in the first milestones. Whether to build one later is an open decision. Per the owner's comparison, the private copy changed `ios/README.md` only, with no Swift source changes relative to its baseline. Those documentation edits are not an iOS scope for ahi.
- Ingesting private state from `iancleary/agent`, from an operator's agent state directory, or from a personal instructions file. ahi does not read or copy credentials, gateway origins, hostnames, addresses, machine names, conversation transcripts, embedding indexes, or the contents of state files. A design fact in this document is not permission to import that state. Personal deployment notes and skills from the private copy are not part of ahi.
- Hosting a multi-tenant service. The coordinator is a process the operator runs, in the same spirit as the current single-user hook runner. **Inferred** from the brief and from the existing specification's lack of a third-party hook service.
- Publishing, tagging, or cutting a release as part of accepting this document.

## Current baseline

v0.1.3, as implemented and as specified:

- `ahi run` reads `.pre-commit-config.yaml`, filters files, and runs builtin checks. `language: system` and `language: script` run only when `ahi.policy.yaml` pins the hook id and the exact `entry`. Remote hooks fail with an explicit error.
- The command set in `src/main.rs` is `run`, `list-checks`, `explain`, `doctor`, `install`, `uninstall`, `lock`, `verify`, `fetch`, `autoupdate`, and `migrate`. `install`, `uninstall`, `lock`, `verify`, `fetch`, and `autoupdate` exit 1 and name the phase that will implement them.
- `ahi.policy.yaml` version 1 is deny-by-default. Unknown keys are errors. The file is parsed. v0.1 consults `trusted_system_hooks` for local system and script hooks.
- `ahi::store` is the content-addressed hook cache. It is not a conversation database.
- `audit.log` is append-only JSON lines: time, command, repository, hook ids, outcome, finding count, local hook ids, and environment names. It does not record file contents or environment values.
- Release assets, once the workflow in [docs/release.md](release.md) has published them, are a static `x86_64-unknown-linux-musl` binary, a static `aarch64-unknown-linux-musl` binary, and `ahi-<version>-sha256sums.txt`, each attested. macOS and Windows are CI targets. They are not release-archive targets.
- The crate forbids `unsafe_code`. v0.4's Landlock and seccomp plan will have to confront that lint. This proposal does not change the lint.

Roadmap already committed in the specification:

| Phase | Contract |
| --- | --- |
| v0.2 | `ahi.lock`, `ahi verify`, `ahi fetch`, `ahi autoupdate`. Tag resolution only there. No hook subprocesses beyond what v0.1 already runs for pinned local hooks. |
| v0.3 | Execute `fail`, `pygrep`, `python`, `rust`, `golang`, and `docker_image` under the runtime rules. No Node. No runtime downloads. |
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

The hook runner does not go through the conversation store. `ahi run` remains a local, offline check. Putting hook runs into the agent ledger would couple a pre-commit invocation to a coordinator the v0.1 operator does not run.

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
| Coordinator | One selected machine owns conversations, settings, intake, and the main agent. Clients and workers use that store. | Same ownership split. The first listener is a local socket. Tailscale is not required. |
| Tasks | `TaskRecord` in `src/protocol.rs` has a stable id, conversation id, title, outcome, summary, model selection, state (`open`, `waiting` with actor and next check, or `resolved` with evidence), worker state, source links, pending inputs, and child agent records. | A task keeps a stable id, one owning conversation, an intended outcome, source links, and an explicit next actor or resolution evidence. Child work is a child conversation or task, not a terminal tab. |
| Workers | A worker announces a Rex host id, executable, and launch paths, and keeps a reverse connection. `WorkerRef` is host, session, window, and block. Offline workers keep their tasks. The MVP does not transfer live work. | A worker is an ahi process that accepts admitted effects and runs them under the sandbox. It does not require Rex. Offline workers keep their assignments. Live work is not transferred. |
| Sessions | Each task gets one named Rex session. Closing the session allows reopen without losing the conversation. Rex owns terminal lifetime. | The conversation is the durable session. A terminal multiplexer is not part of the runtime. Reopening a worker reattaches to the task; it does not create a second conversation. |
| Event intake | `src/sources/mod.rs` defines a `Source` trait. Adapters exist for mail, calendars, GitHub, Slack, and macOS Messages, using existing authenticated command-line tools. Events are admitted before a checkpoint advances. Coverage can be complete, partial, or unavailable. | The same admission rule: durable admit, then cursor. The trait can exist before any adapter is enabled. Adapters are off by default. ahi does not install those command-line tools. macOS Messages is not a first milestone. |
| Authorization carries forward | README: "Authorization carries forward; an approved action should not trigger another approval loop." Architecture: a yes to a concrete batch covers completing and verifying that batch; another tool call does not require another approval; source content and imported history cannot grant authority; requested or standing authority carries through the task and its workers. The same document says typed tool contracts are not a process sandbox. | A grant is a stored rule with a subject, an action constraint, and a bound. A later action that matches it is approved without a new prompt. An action outside the grant is denied. Model text and source events cannot write a grant. The sandbox, not the type of the tool, contains the process. |
| Recovery | Persist allocation intent before creating a session. A missing receipt is uncertain and is not replayed. History sync must never create an execution. | Same for effects: record the intent in the commit that approves the action; an unknown external result stays unknown; a restart does not repeat it. |
| Install | `nix profile add github:jessfraz/agent`. Private settings stay outside the repository. | `cargo install ahi`, or an attested archive. Nix is not required. Private conversations stay outside this repository. |
| Phone client | `ios/README.md` describes a SwiftUI client for the main conversation over the coordinator's authenticated connection, with a private TestFlight distribution. | No phone client until the coordinator protocol is stable, and only if that open decision says to build one. |

This research did not find a grant record beside `TaskRecord` in `src/protocol.rs`. The carry-forward principle is stated in the README and the architecture document. A search of the Rust tree found Tailscale peer checks and source-helper authorization, not a separate approval ledger. That search does not prove the TypeScript runtime lacks every check. **Unverified:** whether jessfraz/agent enforces the principle in code or only in model instructions.

### From the owner's comparison of the private copy

Per the owner's comparison, the private copy keeps the public split: Rust owns types, the wire protocol, and the command-line interface; the TypeScript runtime owns provider construction and execution. New Rust types are mirrored by generated bindings. ahi's leading option removes that split. The designs below are the ones the comparison marks to carry, adapted into the Rust runtime. Carry-over is in scope only where the license permits it. See [Licensing and attribution](#licensing-and-attribution).

| Comparison verdict | Design, per the owner's comparison | What ahi adopts |
| --- | --- | --- |
| Carry | Coordinator settings send model preferences and the gateway registration to workers with bootstrap. | The coordinator is the source of those settings. A worker does not invent a model or a gateway. |
| Carry | `ModelPreferences`: a map of preset label to model selection, plus roles `chat`, `delegation`, and `background`. Labels are trimmed and non-empty. A role label must name an existing preset. Resolution takes a label and a fallback. | The same three roles and the same validation. Example preset labels in the comparison are `plan`, `build`, `simple`, and `feature`. Those labels are examples, not a required set. |
| Carry | Scheduled-job model is optional. A job with no model uses the background role. Setting a provider, a reasoning effort, or a fast mode without a model is a validation error. An explicit job model still wins. | The same inheritance rule, once scheduled jobs exist. They are not a first milestone. |
| Carry (adapt) | `ModelProvider` gains `Custom(ProviderId)`. The identifier is a validated ASCII string on the wire, not a fixed enum. | Provider identifiers are data. The validation rules are the ones to reuse. Built-in names and gateway names share that string space and must not collide. |
| Carry | Gateway fetch is native HTTP. HTTPS only, except a literal loopback origin. The request carries no credential. Redirects are not followed. Timeout 15 seconds. Body cap 1 MiB. The catalog schema is strict: OpenAI-compatible completions, an API-key field that is a gateway marker rather than a secret, zero costs, and capability flags. | The same origin contract. The catalog path in the comparison is `<origin>/pi/agent/model.json`. |
| Carry (adapt) | Each gateway keeps its last accepted catalog when a refresh fails. Registrations persist. A gateway's local name and the provider identifier in the catalog are different fields. Legacy single-URL cache files are migrated in place. | Retain the last-good catalog per registration. Reimplement validation in Rust. Do not take a JavaScript schema library. The provider identifier in the catalog is authoritative. The local name is only a label. The comparison marks some aliasing cases as uncertain; ahi does not guess them. |
| Carry (adapt) | Worktree allocation is recorded. A refused cleanup stays held, and the refusal is recorded. | An allocation and a refused cleanup are action receipts. Cleanup does not run unless the engine approves it. The held state remains until a later receipt says otherwise. |
| Carry | Login always prints the authorization URL. It may try a browser, then the default browser. Failure to open a browser does not abort the pending login. | `ahi login` prints the URL on standard error and waits. Headless and SSH sessions stay able to finish sign-in. The comparison says this path was verified live on the private copy. This environment did not verify it. |
| Carry | A managed-settings apply overlays private machine configuration on declarative settings. | Declarative settings can live with the operator's shared configuration. Private machine values, including model preferences and gateway registrations, apply on top and are not written back into a repository. Proposed command: `ahi settings apply`. |
| Drop | Provider construction through `pi-ai`, JavaScript schema validation, and generated TypeScript bindings. | Replaced by the Rust provider interface and Rust types. |
| Drop | Node bridge dispatch of gateway commands. | Absent in the Rust-native store. Present only if the sidecar alternative is chosen, and then only behind that process boundary. |
| Drop | The terminal selection-picker implementation. | No terminal-interface phase. Listing presets or conversations does not submit a turn. |
| Drop | iOS edits in the private copy, and its skill and handoff files. | Out of scope. See the non-goal on private state. |

The comparison also says slash commands work in the plain line interface, not only in the terminal interface. ahi's first coordinator commands are line commands. A picker is not required for them to run.

### From Pi Durable

The published package `@earendil-works/pi-durable` describes a harness that commits conversations, model turns, tool calls, and application state before showing them. If the process dies mid-turn, reopening the storage continues from the last commit. A model request the process died during counts as a failed attempt and is retried under the package's retry settings. The README marks the API experimental and says it changes without notice.

Borrowed invariants, if the Rust-native option is chosen:

- One open storage, one writer, atomic commits. Nothing is shown before its commit is stored.
- A conversation is a transcript of immutable entries. A handle is an id.
- A task is a durable state machine with a checkpoint. Every task has an owner: its conversation, or another task.
- A submission has an optional `requestId`. Repeating that id returns the existing submission.
- Unsafe external effects are not replayed blindly. Pi Durable's tool hooks and jessfraz/agent's architecture both treat a lost receipt as uncertain. ahi keeps that rule for pushes, outbound messages, and other mutations.
- Forks branch from an entry. Compaction and reset change what the model sees next. They do not delete the earlier entries. The README states that the model sees entries from the newest reset onward, and that compaction summarizes older context.

Deliberately different:

- ahi does not adopt Pi's package installer. The fetched [pi.dev](https://pi.dev/) page shows `pi install npm:@foo/pi-tools` and `pi install git:github.com/badlogic/pi-doom`, and an install line `curl -fsSL https://pi.dev/install.sh | sh`. ahi does not grow an installer for third-party packages, and it does not use a pipe-to-shell install. The existing specification already refuses `curl | sh` for hook downloads.
- ahi does not vendor Pi's provider list. The same page names Anthropic, OpenAI, Google, Azure, Bedrock, Mistral, Groq, Cerebras, xAI, Hugging Face, Kimi For Coding, MiniMax, NVIDIA, OpenRouter, Ollama, "and more." The provider set for ahi is an open decision.
- ahi's hook engine is the policy boundary. Pi Durable hooks can block or rewrite a tool (`beforeTool`) and replace a result (`afterTool`). That is a useful shape. ahi's rules are configuration the operator declares, audited as grants, and tied to the sandbox. They are not only extension code loaded into the process.
- The hook cache (`ahi::store`) stays a content-addressed blob store. The conversation store is a different module and a different file.

### Shapes that are not the Pi Durable model

Three other Pi formats showed up in research. They are not the model this proposal follows, unless Ian selects one of them in the store decision.

| Shape | Where it was read | Why it is separate |
| --- | --- | --- |
| Coding-agent session JSONL | [pi.dev session format](https://pi.dev/docs/latest/session-format) and `packages/coding-agent/docs/session-format.md` | A JSONL tree of session entries for the coding-agent CLI. Parent links, compaction, and system messages that patch prompt sections. Not the SQLite schema below. |
| AgentHarness specification | `packages/agent/docs/harness.md` on `main` | Specifies entries, registers, and a usage ledger, with a SQLite sketch (`entries`, `registers`, `usage_ledger`, branch tables, `writer_lease`). The document says: "The incompatible format-4 code currently in the source tree is unfinished and is replaced in place." |
| Earlier harness-v2 / session-backend tables | A non-`main` document and a test at an older commit, found by search | Tables such as `lanes`, `records`, and `facts`. Not re-read as the current `main` schema. **Unverified** as a shipped format. |

jessfraz/agent's bridge imports `SqliteStorage` from `@earendil-works/pi-durable/storage/sqlite`. That is the Pi Durable package, not the AgentHarness SQL.

## Conversation store

### Leading option: Rust-native store

Implement the store in Rust, in this crate, or in a module that a later split moves. Follow the Pi Durable data model as published in the package and as stored by its SQLite backend on `main`. The coordinator does not start a Node.js process.

This is the leading option because the brief says Ian is leaning that way, and because it matches constraints this repository already has:

- Release assets are one static musl binary with no companion runtime. A Node sidecar is a second required program. `runtime/package.json` in jessfraz/agent requires Node `>=26.12.1`.
- [docs/SPEC.md](SPEC.md) open decision 5 recommends that Node is not a v1 language and that ahi must not grow a nodeenv. The inferred container images omit Node. A sidecar reverses that recommendation for the coordinator even if `language: node` hooks stay unsupported.
- `unsafe_code = "forbid"` and the supply-chain allow list in `deny.toml` cover the Rust graph. An npm graph (`pi-durable`, `pi-ai`, `chord`, `openid-client`, Playwright, and the rest of that `package.json`) is a second review surface. The sidecar option accepts that surface. The Rust-native option does not.
- The hook runner must keep working on machines that do not have Node. A single binary keeps `ahi run` and `ahi serve` on the same artifact.

The cost is implementation effort and compatibility. A Rust port opens a Pi Durable database only if a later phase writes an importer against a pinned schema. Pi Durable's README says the API changes without notice. `main` has already added the task status `sleeping` (schema version 2 in `migrations.ts`). A Rust store owns its migrations. The sidecar imports the package and follows that package's releases, including incompatible ones.

**Recommendation:** Rust-native store. Treat byte compatibility with Pi files as a separate, later importer, not as a requirement of the first store. **Decision: open.**

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

SQLite durability, as the README states it: WAL mode with `synchronous = NORMAL`. Commits survive process crashes. The newest may be lost on power or host failure. One process owns a storage. There is no cross-process locking in the package. jessfraz/agent adds the inherited `writer.lock` around the Node child.

ahi additions, stored as documents or as entries in that same commit stream, not as a second writer:

| Record | Proposed contents | Why it is not a Pi built-in |
| --- | --- | --- |
| Grant | Rule id, subject (task or conversation), action constraint, bound (one batch, or until a time), evidence entry id | Carries an approval forward as data. Pi's `beforeTool` hook is code, not a stored grant. |
| Action receipt | Action id, verdict, rule id, argument hash, sandbox backend, outcome `denied`, `completed`, or `uncertain` | The audit line for an effect. Unknown outcomes are not replayed. |
| Worker assignment | Task id, worker generation, host id, last presence | Replaces Rex's session, window, and block ids. |
| Source admission | Provider, account, event id, version, link or dismissal reason | The cursor advances only after this commit. |

Argument hashes, rule ids, and paths that are not secret belong in the receipt. Secret values, raw credentials, and full file contents do not. That continues the `audit.log` rule already implemented in `src/audit.rs`. The hook-runner `audit.log` stays the log for `ahi run`. It is not the conversation store.

**Recommendation for durability, stricter than Pi Durable's documented `synchronous = NORMAL`:** fsync the commit that records a grant or an external effect. A lost grant or a lost "push already sent" bit is the failure mode this runtime is trying to avoid. The later public jessfraz/agent tree does the same class of thing for its Pi database: `runtime/src/conversation-database.ts` at `e671f61` sets `PRAGMA synchronous = FULL` and `PRAGMA fullfsync = ON`, and its comment says canonical Pi commits request full synchronization and macOS `F_FULLFSYNC`. That file was read here. Per the owner's comparison, the private copy does not contain it. The recommendation follows the later public opener and the security-log requirement, not the private copy's older tree.

### Upstream drift

Per the owner's comparison, public jessfraz/agent moved after the baseline `d4ef2fb` that the private copy imported. This proposal's public reading is `e671f61`. The comparison names these additions as files the private copy never had. They are not changes made in the copy.

| Public file at `e671f61` | What this environment read | Why it matters to the store decision |
| --- | --- | --- |
| `runtime/src/conversation-database.ts` | Opens the Node SQLite database and sets `synchronous = FULL` and `fullfsync = ON`. It does not declare a new table schema in the portion read (the whole file is the opener). | The private copy is not evidence about this durability setting. Pi Durable's README still documents `synchronous = NORMAL`. The public agent tree that wraps Pi has already asked for full synchronization. A Rust store should choose its own mode with that conflict visible. |
| `runtime/src/startup-diagnostics.ts` | Appends a private bounded log of fixed stages and numeric failure codes. The file comment says diagnostics never own recovery. | Useful as a separate log. It is not the conversation store, and it does not decide resume. |

The comparison also names iOS source-status views and tests that the private copy lacks. Those are product drift, not store schema. A re-port of the private copy onto later public jessfraz/agent would meet the dynamic provider-identifier change, which the comparison expects to conflict. That is a reason for ahi to own provider identifiers as data in Rust, rather than to track the TypeScript provider enum.

The private copy's use of `@earendil-works/pi-durable` 1.4.0, per the comparison, is the sidecar the leading option declines. It predates the public opener above. Choosing the sidecar to "match the private copy" would freeze an older Node store. Choosing it to "match current public jessfraz/agent" would track a moving bridge, including the full-synchronization opener. Neither is the Rust-native store.

### Alternative: Node.js sidecar

Run Pi Durable as jessfraz/agent does.

Observed mechanism:

- Rust is the supervisor. `Runtime::start_with_notifications` in `src/runtime.rs` locks `writer.lock`, clears the close-on-exec bit so the child keeps the lock, and spawns `node --no-warnings <bridge.js> <state-dir>`.
- `runtime/src/bridge.ts` imports `Harness` from `@earendil-works/pi-durable` and `SqliteStorage` from `@earendil-works/pi-durable/storage/sqlite`.
- The Node process speaks a generated request and event protocol to Rust. Rust owns host effects. TypeScript owns the harness.

| | Rust-native store (leading) | Node sidecar (alternative) |
| --- | --- | --- |
| Artifact | The existing static binary can remain the whole program. | The binary requires Node `>=26.12.1` and a built `bridge.js` plus `node_modules`. |
| Security review | One Rust graph, under `cargo deny` and the current license allow list. | That graph plus the npm packages pinned in jessfraz/agent's `runtime/package.json`, including Playwright. |
| Node decision | Leaves open decision 5 in the specification on its current recommendation: no Node. | Reverses that recommendation for the coordinator. `language: node` hooks can still stay unsupported. |
| Pi format | Same logical model. Reading a Pi database is extra work and must pin a schema. The coding-agent JSONL file and the AgentHarness SQL are different formats and are not read by default. | Opens Pi Durable SQLite as that package defines it, for the pinned version. Still does not read coding-agent JSONL unless Pi does. |
| API churn | ahi owns migrations. Pi's "changes without notice" warning does not rewrite ahi's file. | Upgrading `@earendil-works/pi-durable` can change storage and behavior between releases. The README states that. |
| Effort | A store, a scheduler, and a provider client in Rust. Large. | A narrow adapter, which jessfraz/agent already found sufficient to connect tools to Pi commits. Smaller, and coupled to Node. |
| Hook runner | `ahi run` does not start Node. | `ahi run` must not start Node either. The sidecar starts only for coordinator commands. |

If the sidecar is chosen, the coordinator commands document a Node requirement. The hook runner does not. The release archive would have to say that the static binary is not sufficient for `ahi serve`. That is a real change to the asset contract in [docs/release.md](release.md), and it waits for the decision.

## Policy and hook engine

v0.1 pins a local hook by id and exact `entry` in `ahi.policy.yaml`. Unknown keys error. Absence of the file is deny-by-default. The agent engine keeps that file's version 1 schema unchanged, so a repository that only runs hooks does not gain a new required document.

Agent rules live in a new file, proposed name `ahi.actions.yaml`. The version 1 parser of `ahi.policy.yaml` does not read it. A missing actions file denies every agent effect. It does not prompt.

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
  - id: push-main
    action: vcs_push
    match:
      remote: "https://github.com/iancleary/ahi"
    effect: deny
```

This YAML is a sketch of the contract, not a parser implemented in this change. Names of files and fields are proposals.

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

Asking the operator is not a fourth verdict. A rule may set `ask: true`. On a match, the coordinator shows the concrete action and, if the operator confirms, appends a grant for that bound, then re-evaluates. The effect runs only after the re-evaluation returns `approve`. A rule without `ask` denies. The default file does not ask on every tool call. A configuration that asks for every action invites confirmation without reading the action.

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

The verdict object:

| Field | Meaning |
| --- | --- |
| `effect` | `approve`, `deny`, or `rewrite` |
| `rule_id` | The rule that fired. Required. |
| `reason` | Short text safe to show and to store. |
| `action` | Present only for `rewrite`. The replacement action. |
| `sandbox` | Present only for `approve`. `default` or an escalation the schema allows. |

In-process callers use the same structure the external program reads and writes, so a builtin rule and a pinned program cannot drift into two contracts.

`ahi hooks run` is the existing `ahi run`. It does not emit these action records. Pre-commit hooks stay on the v0.1 path. A later phase may optionally record a hook run as an action. That is not required for compatibility, and it is not part of v0.2 through v0.4.

## Sandbox execution

The specification's v0.4 section is the execution plan this layer uses. Summary, without replacing that section:

- If ahi is already inside an OpenShell sandbox, do not apply a second Landlock or seccomp filter. The OpenShell runtime profile denies `seccomp(SECCOMP_SET_MODE_FILTER)` with `EPERM`. Network control there is the policy proxy.
- Otherwise the host backends in the specification apply to effect subprocesses: Landlock and seccomp on Linux, `bwrap` when Landlock is unavailable, `sandbox-exec` on macOS with the limits written there, and a Windows job object that contains the process tree and does not restrict the filesystem. Builtin in-process checks stay in process.
- If the selected backend cannot enforce "no network" and the action was not approved with network, the action does not run.

Escalation is a field on an `approve` rule: extra read paths, extra write paths, or network. The engine attaches that profile to the child. A rewrite cannot add a path or a network grant that the matching approve rule does not list. Inside OpenShell, an ahi rule cannot widen the outer filesystem or the proxy. The most ahi can do there is deny. The specification already treats an in-sandbox hook run as advisory and CI as the backstop. The same sentence applies to an agent effect inside that sandbox: the coordinator, if it is the sandboxed process, cannot be the root of trust for grants the sandbox user can edit. A grant file inside the writable workspace is operator policy for a local run, and it is not a substitute for a trusted CI or a coordinator whose policy file the effect cannot write.

The `unsafe_code = "forbid"` lint is in the root `Cargo.toml`. Landlock and seccomp need privileged system calls. When v0.4 or this layer implements them, the unsafe block belongs in one reviewed module or in a dependency, with the lint exception written down. This proposal does not add that exception.

## Coordinator, tasks, and workers

Proposed commands, additive, and absent until a later phase implements them:

| Command | Role |
| --- | --- |
| `ahi serve` | Run the coordinator. One writer. Local socket. Refuses to start if the store lock is held. |
| `ahi worker` | Connect to a coordinator and execute admitted effects. Does not open the store for write. |
| `ahi task list` and `ahi task show` | Read the task registry through the coordinator. |
| `ahi actions check` | Evaluate one action against `ahi.actions.yaml` and print the verdict. Does not execute it. |
| `ahi self-update` | Replace this ahi binary, or another first-party artifact from the same release, after verification. |

`ahi serve` with no configuration starts a coordinator that has no provider and no sources. That process can still answer `ahi actions check` once the actions file exists. It does not open a network connection by merely starting.

Model selection, per the owner's comparison and adapted as above: presets name concrete models, and the roles `chat`, `delegation`, and `background` point at presets. Bootstrap sends the resolved settings to a worker. A scheduled job, when that feature exists, uses the background role unless the job names its own model.

Gateway catalogs use the origin contract in the comparison table. Refresh failure keeps the last accepted catalog. The fetch does not send credentials and does not follow redirects.

`ahi login` prints the authorization URL and leaves the login pending if no browser opens. `ahi settings apply` overlays private machine settings, including preferences and gateway registrations, onto declarative settings and does not write those private values into the repository.

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

Routing uses an exact source link, an exact task id, or an explicit operator choice. Semantic search of old conversations is a later capability. jessfraz/agent documents keyword and semantic search over a rebuildable index. The first ahi coordinator continues a task only on an exact id or an exact source link. A similarity match does not select a repository for a push.

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
| Credentials | Not copied into the transcript, the receipt, or `audit.log`. Provider credentials stay in the operator's existing store. The exact store is part of the provider decision. |
| Hook programs | Pinned entry, no shell, fail closed, scrubbed environment using the name rules already in the specification. |
| Distribution | ahi replaces only its own release artifacts, and only after checksum and attestation checks. It does not execute a downloaded install script as the trust root. |

Out of scope, as in the specification: a kernel exploit, and a person who already controls the account that runs ahi. The sandbox contains effects. It does not contain that person.

## Crate and workspace layout

The repository is one package, `ahi`, with a library and a binary. `scripts/cut-release.sh` and the publish workflow assume that root manifest. `fuzz/` is already a detached workspace.

Option A, **recommendation** for the first agent phases: stay one crate. Add modules only when a phase implements them.

| Module | When | Responsibility |
| --- | --- | --- |
| `ahi::policy` | exists | Version 1 hook policy. Unchanged. |
| `ahi::store` | exists | Hook cache. Unchanged role. |
| `ahi::actions` | first agent phase | Parse `ahi.actions.yaml`, evaluate a verdict. |
| `ahi::sandbox` | with v0.4, then reused | Detect an outer sandbox. Apply a host backend to a child. |
| `ahi::conversation` | store phase | The Rust-native store, if that option wins. Not the hook cache. |
| `ahi::coordinator` | coordinator phase | Lock, socket, tasks, worker admission. |

Option B: a Cargo workspace of unpublished internal crates (`ahi-actions`, `ahi-conversation`, `ahi-coordinator`) with the published package still named `ahi`. The binary crate re-exports the library surface. Release scripts must grow a workspace story. They do not have one today.

Option C: publish several crates. Callers could depend on the store without the binary. That is a public API commitment, and it multiplies the release this repository has deliberately not automated past a single package.

**Recommendation:** option A until a module's compile time or a real library consumer forces a split. **Decision: open.** This document does not split the crate.

## Distribution

Three paths, all for ahi itself:

1. **crates.io.** `cargo install ahi --locked`, once a release is published. The name check in the specification (HTTP 404 from crates.io on 2026-10-25) was not repeated successfully. On 2026-11-03 a request to `https://crates.io/api/v1/crates/ahi` returned HTTP 403 with an empty body. **Unverified** whether the name is still free.
2. **Attested archives.** The layout and the `gh attestation verify --repo iancleary/ahi` commands in [docs/release.md](release.md) stay the binary distribution for Linux `x86_64` and `aarch64`. This proposal does not add macOS or Windows archives. Operators on those systems use `cargo install` until a later release decision adds archives. The specification's inferred context includes macOS and Windows development machines. Those machines are not covered by the current archive set.
3. **ahi.dev.** The brief names this domain for an install document and for project documentation. In this environment `getent hosts ahi.dev` returned no address, and a request to `https://ahi.dev` produced no HTTP response. **Unverified** whether the domain is registered or serving. The document that should live there, when the domain is ready, is the same install and verify procedure as the README. It is not a second installer with a different trust model.

`ahi self-update`, when it exists, downloads the archive for the detected target, checks `ahi-<version>-sha256sums.txt`, and verifies the attestation against `iancleary/ahi`. It then replaces the ahi executable. It refuses a target it cannot verify. It does not run a remote shell script. It does not accept a package name argument. First-party components are artifacts this repository's release workflow publishes. A hook repository, a language runtime, and a Pi package are not first-party components.

The pipe-to-shell line on [pi.dev](https://pi.dev/) is the pattern this section does not use.

## Compatibility and semantic versioning

The root package is `0.1.3`. The specification's open decision on SemVer versus calendar versioning is unchanged. The script in the tree implements explicit SemVer. This proposal assumes that script until that decision goes the other way.

0.x permits breaking changes. 0.1.x users still need a compatibility story, because `cargo install ahi` tracks the latest release and the attested binary is how operators upgrade.

| Surface | Compatibility rule |
| --- | --- |
| `ahi run` and its flags | Remain. Behavior stays the specification's behavior. |
| `list-checks`, `explain`, `doctor`, `migrate` | Remain top-level. |
| `install`, `uninstall`, `lock`, `verify`, `fetch`, `autoupdate` | Remain the phase stubs they are until v0.2 or the shim phase implements them. Agent work does not take those names. |
| `ahi hooks run` | An alias of `ahi run`, added when the grouped surface is added. The same alias pattern can cover the other hook commands. Aliases are additive. |
| `ahi.policy.yaml` version 1 | Unchanged. Unknown keys remain errors. Agent keys do not go in this file. |
| `ahi.actions.yaml` | New. Ignored by `ahi run`. |
| Default `ahi` with no subcommand | Keep today's help text. Do not make a bare `ahi` start a coordinator. |
| Store files | The hook cache layout stays as specified. The conversation database is a new file under the coordinator's state directory, not inside the repository being checked. |
| SemVer | Adding commands and a new file is a minor bump once the project is 1.0. Before 1.0, the same additions still must not change `ahi run` exit codes or the version 1 policy schema. A major version is the point at which the conversation schema and the actions schema freeze. It is not a point at which `ahi run` is removed. |

**Recommendation:** keep `ahi run` through 1.x. The grouped `ahi hooks` command is the name new documentation uses. Both work. Removing the top-level hook commands would be a break this proposal does not ask for.

## Milestones

These phases sit after the existing roadmap, or beside it only where the work is documentation and schema design. They do not replace v0.2, v0.3, or v0.4. Version numbers below v0.5 are not assigned. The labels are phase names.

| Phase | Relation to the roadmap | Delivered behavior |
| --- | --- | --- |
| Specification phases v0.2, v0.3, v0.4 | Unchanged, and first | Lockfile and fetch, limited hook languages, sandbox detection and optional host backends. |
| Actions | After the v0.3 subprocess runner exists, so external hook programs have one process builder. Can be designed beside v0.2. | `ahi.actions.yaml`, `ahi actions check`, deny-by-default, no model and no network. `ahi hooks run` alias. `ahi run` unchanged. |
| Sandbox for effects | After v0.4, reusing its detection and backends. | An approved `command` or `file_write` runs in the default sandbox. Escalation only by rule. Tests follow the specification's Linux, macOS, and Windows honesty table. |
| Conversation store | After the actions phase. Blocked on the store decision. | Rust-native: commits, resume, one writer, grants and receipts. Sidecar: Node documented as a coordinator requirement, hook runner still free of Node. No provider required to test resume. |
| Coordinator | After the store. | `ahi serve`, `ahi worker`, task registry, local socket, uncertain-outcome rule. No source adapters. |
| Intake | After the coordinator, one adapter at a time, each off by default. | Admit-before-cursor. A dismissal reason is data. The adapter does not install a tool. |
| Distribution | When Ian chooses to ship. Not part of implementing the phases above. | crates.io, ahi.dev documentation of the attested archives, `ahi self-update` for the binary only. |

v0.3 is the right predecessor for external action hooks because it is the phase that executes pinned programs. v0.4 is the right predecessor for effect sandboxes because nesting policy is already specified there. Starting a second sandbox design beside v0.4 would fork the threat model.

## Licensing and attribution

The owner conditioned carry-over from jessfraz/agent and from the private copy on the license permitting it. This section states the license texts that were read and the attribution those texts require. It is not a license opinion, and it does not relicense anyone's code.

ahi today: root `LICENSE` and `Cargo.toml` say MIT. The copyright line is `Copyright (c) 2026 Ian Cleary`. The MIT condition in that file is that the copyright notice and the permission notice appear in all copies or substantial portions of the software.

jessfraz/agent: `LICENSE` at public baseline `d4ef2fb5af0b44531e771a6c3ba4bb7959caf966` and at `e671f61d54ffe3b1361db79e7cc79a68ac252b23` is the MIT license. Both files begin `Copyright (c) 2026 Agent contributors`. The condition read in the file is the MIT condition: the copyright notice and the permission notice must be included in all copies or substantial portions of the software. <https://github.com/jessfraz/agent/blob/d4ef2fb5af0b44531e771a6c3ba4bb7959caf966/LICENSE>

Pi: `LICENSE` on `main` is the MIT license. The copyright line read from that file is `Copyright (c) 2025 Mario Zechner`. The same MIT condition follows: include the copyright notice and the permission notice in all copies or substantial portions. <https://github.com/earendil-works/pi/blob/main/LICENSE>. The `license` field of `packages/durable/package.json` on `main` is `MIT`. That field was read on 2026-11-03. The package version in that file was `1.5.0`, which is newer than the `1.4.0` pin in jessfraz/agent. The field is not a claim that every Pi file header was checked.

This proposal copies neither repository's source. Describing a data model or an origin contract in this file does not attach those copyright lines to this file. A later change that copies or substantially reproduces source from either project must keep that project's copyright notice and permission notice with the copied portion. The private copy is not a license source. It was not read, and its texts are not available to attribute.

The open choice for ahi remains MIT, as the file stands, or MIT OR Apache-2.0, as [docs/SPEC.md](SPEC.md) recommends and does not apply. That choice is about ahi's own license. It does not remove the MIT notice from a portion that came from jessfraz/agent or from Pi.

## Open decisions

Each item is open. The recommendation is the proposal, not a change already made in the tree.

### 1. Conversation store: Rust-native or Node.js sidecar

**Leading option:** a Rust-native store that follows the Pi Durable logical model and the SQLite record types cited above. Byte-level import of a Pi database is later work against a pinned schema. Coding-agent JSONL and the AgentHarness register schema are not that model. Durability follows the later public jessfraz opener (`synchronous = FULL`), not Pi Durable's documented `NORMAL`, and not the private copy: per the owner's comparison that copy predates `conversation-database.ts`.

**Alternative:** a Node.js sidecar with the same supervisor shape as jessfraz/agent (`node` plus `@earendil-works/pi-durable`). Matching the private copy would mean the `1.4.0` packages the comparison names. Matching public `e671f61` would mean a newer bridge. Both stay the alternative.

Tradeoffs are in [Conversation store](#conversation-store) and [Upstream drift](#upstream-drift). The brief states that Ian is leaning toward the Rust-native store. **Decision: open.**

### 2. Crate split or a single crate

**Recommendation:** one crate, with new modules, until compile time or a library consumer requires a split. The release script publishes one package.

**Alternative:** a workspace of internal crates, or several published crates.

**Decision: open.**

### 3. License: MIT, or MIT OR Apache-2.0

Unchanged from the specification. The texts and the attribution they require are in [Licensing and attribution](#licensing-and-attribution). The specification's recommendation is dual MIT OR Apache-2.0, and its provisional choice is MIT only, because the file is already in the tree. Carry-over from jessfraz/agent or Pi stays under those projects' MIT notices.

**Decision: open.** This proposal does not add `LICENSE-APACHE`.

### 4. Relationship to `iancleary/agent`, including privacy

The private repository was not readable here (HTTP 404). Per the owner's comparison it is a copy of jessfraz/agent at `d4ef2fb`, described there as version 0.1.0 at `0b47b63`, and it is not a git fork. This document uses that comparison for design facts only.

**Recommendation:** public ahi does not depend on that repository. It does not ingest the private state listed in the comparison's privacy section: runbooks, tailnet names and addresses, gateway origins, session material, conversation text, state-directory contents, embedding data, or personal instruction files. One line in the private repository names a machine, per the comparison; this document does not repeat it. If the private tree is a personal deployment, it can depend on public ahi later. Whether to keep it private, archive it, or publish parts of it is a separate choice. Publishing it is not required for this proposal, and this proposal does not request access.

**Decision: open.**

### 5. iOS client scope

jessfraz/agent includes a SwiftUI client (`ios/README.md`) that attaches to the coordinator's main conversation. That shows a phone client is a possible product. It is not a requirement for the runtime.

**Recommendation:** no iOS target in this repository during the actions, sandbox, store, and coordinator phases. The owner's comparison reaches the same drop: the private copy's iOS delta is documentation, and it is not carried. Revisit after `ahi serve` has a stable local protocol. A client, if built, talks to the coordinator. It does not hold the store.

**Decision: open.**

### 6. Provider set

pi.dev lists many hosted providers. jessfraz/agent's setup document says ChatGPT is the default, that `agent login` opens ChatGPT sign-in, and that other Pi providers can use environment or ambient credentials. Agent does not provide an interactive login for every provider. Those are facts about those projects. They are not a selection for ahi.

Per the owner's comparison, the private copy makes provider identifiers data (`Custom(ProviderId)`) and adds a gateway catalog under the origin contract above. That sharpens this decision. The interface should accept a provider identifier string, not a closed enum, so a gateway can be registered without a release. The gateway is one provider behind that interface. Its fetch rules are part of the security posture, not a list of vendors to bundle. Login prints a URL and does not abort when a browser is absent. The comparison's TypeScript provider construction is a drop for the Rust-native option.

**Recommendation:** define that identifier-based interface in the store phase, and ship no network provider until the store decision is made. The first concrete providers are one the operator already has credentials for, and a gateway or loopback catalog that satisfies the origin contract. Do not port Pi's provider matrix. Do not depend on `pi-ai` unless the sidecar option is chosen. Do not embed a gateway origin in the binary or in this repository.

**Decision: open.** Which hosted vendor, if any, is still open. The shape of an identifier plus a credential-free gateway catalog is the part the comparison settles as the design to carry.

### Further open items

These are narrower than the six decisions above. They still need an owner before the phase that implements them.

| Item | Recommendation |
| --- | --- |
| Remote worker transport | Local socket first. An authenticated remote listener later. Do not require Tailscale in the first coordinator. |
| Interactive `ask` | Only rules that set `ask: true` prompt. Default is deny with no prompt. |
| SQLite synchronous mode | `FULL` for the conversation database, including grants and external-effect commits. Pi Durable's README documents `NORMAL`. Public jessfraz/agent at `e671f61` sets `FULL` and `fullfsync`. The private copy, per the owner's comparison, has neither that file nor a vote on this row. |
| Release archives for macOS and Windows | Not in this proposal. `cargo install` covers those systems until a release change adds archives. |
| Phase version numbers after v0.4 | Do not assign them until the preceding specification phases land. |
| Semantic search of past tasks | Not in the first coordinator. Exact id and exact source link only. |

## Sources

Fetched or cloned on 2026-11-03 unless noted.

- This repository's contract: [docs/SPEC.md](SPEC.md), [docs/release.md](release.md), root `Cargo.toml` version `0.1.3`, `src/main.rs`, `src/policy.rs`, `src/audit.rs`.
- jessfraz/agent commit `e671f61d54ffe3b1361db79e7cc79a68ac252b23`: <https://github.com/jessfraz/agent>. Files used: `README.md`, `docs/architecture.md`, `docs/setup.md`, `docs/roadmap.md`, `ios/README.md`, `LICENSE`, `runtime/package.json`, `runtime/src/bridge.ts`, `runtime/src/conversation-database.ts`, `runtime/src/startup-diagnostics.ts`, `src/runtime.rs`, `src/protocol.rs` (`TaskRecord`, `WorkerRef`, `SourceRef`), `src/sources/mod.rs`.
- jessfraz/agent `LICENSE` at baseline `d4ef2fb5af0b44531e771a6c3ba4bb7959caf966`: <https://github.com/jessfraz/agent/blob/d4ef2fb5af0b44531e771a6c3ba4bb7959caf966/LICENSE>. MIT, `Copyright (c) 2026 Agent contributors`.
- Owner's comparison of the private copy, comment `6594868422`: <https://github.com/iancleary/ahi/pull/6#issuecomment-6594868422>. Design facts about `iancleary/agent` are from this comment only.
- Pi `LICENSE`: <https://github.com/earendil-works/pi/blob/main/LICENSE>. MIT, `Copyright (c) 2025 Mario Zechner`. `packages/durable/package.json` `license` field `MIT`, read 2026-11-03.
- Pi repository `main` at `9c9ce8547e5a88cbe834a53e9a6d83a789d08e47`: <https://github.com/earendil-works/pi>. Package README: <https://github.com/earendil-works/pi/blob/main/packages/durable/README.md>. Schema: `packages/durable/src/storage/sqlite/schema.ts`, `packages/durable/src/storage/sqlite/migrations.ts`, `packages/durable/src/storage/store/tables.ts`. Records: `packages/durable/src/types.ts`.
- AgentHarness specification, a different model: <https://github.com/earendil-works/pi/blob/main/packages/agent/docs/harness.md>.
- Coding-agent session format: <https://pi.dev/docs/latest/session-format> and <https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md>.
- Pi product page, including the install line and the provider list: <https://pi.dev/>.
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
