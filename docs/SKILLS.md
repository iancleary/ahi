# Shared skill runtime

Skills are bounded instructions and supporting text, not permission grants.
The TUI, `ahi exec` (including `--headless`), and the session service use the
same discovery and loading implementation.

## Discovery and metadata

ahi discovers `<workspace>/.agents/skills/<name>/SKILL.md` and
`<user home>/.agents/skills/<name>/SKILL.md`. The OS resolves the home directory;
ahi configuration/state overrides do not relocate it. Ahi keeps both definitions
when names collide: `repo/review` and `operator/review` are distinct IDs. The
picker puts repository skills first on an equal match. `ahi doctor` lists
both roots. Discovery does not scan ancestors or recurse for skills. An
operator-installed skill directory entry may link to a dotfiles checkout; ahi
resolves the target before reading its files. Repository skill entries, skill
files, and referenced paths cannot be symlinks.

Discovery reads each bounded file locally to validate and extract frontmatter.
It retains metadata, not the instruction body. The model does not receive the
full file at startup.

```yaml
---
name: review
description: >-
  Review changes for correctness and maintainability. Use before submitting a PR.
license: MIT
compatibility: Requires Rust and Cargo
metadata:
  author: example
  version: "1"
disable-model-invocation: false
---

Read `references/checklist.md` before reviewing the changes.
```

- `name` and a nonempty `description` are required. The name must match the
  directory and satisfy ahi's existing 1–64-character name rules (ASCII letters,
  digits, hyphens, underscores, and dots).
- Descriptions may contain up to 1024 Unicode characters, including multiline
  YAML strings. They are normalized to one line in the model-facing catalog.
- `disable-model-invocation: true` marks a skill manual-only.
- `license`, `compatibility`, `metadata`, `allowed-tools`, and unknown extension
  keys are accepted and retained as informational YAML values. `ahi doctor`
  lists their keys, not their values. **`allowed-tools` never grants permission.**
- Legacy ahi fields `when`, `references`, and `version` remain accepted.
  `when` and optional declared references augment the catalog. `version` is
  informational. References are optional; supporting paths need not be listed
  in frontmatter. Listed references are validated at discovery.
- Malformed YAML, duplicate keys, unsafe YAML constructs, missing required
  fields, invalid names, symlinks, and oversized files are still rejected.

The `load_skill` tool description advertises approved automatic skills with
name, description, source, and `SKILL.md` location (plus legacy routing hints
when present). Other metadata values and instruction bodies are not advertised.
Manual-only skills are visible to the operator but omitted from this automatic
catalog. A guessed automatic call cannot load a manual-only skill.

## Explicit invocation

In the TUI, a typed `$` opens the picker. Search by name or qualified ID;
**Tab** cycles **Both → Repo → Operator**, **↑/↓** selects a row, and **Enter**
inserts a selected reference without sending. The draft status lists selected
IDs. Editing through a selected reference removes the selection. **Esc** leaves
the query as ordinary text. Bracketed paste inserts text without opening the
picker; neither pasted nor manually typed `$` text requests a skill on submission.

Use repeatable `--skill` flags in `ahi exec`, including in headless mode:

```sh
ahi exec --model local/model --skill repo/review -p 'Review this use of $PATH'
```

The session service accepts the equivalent `"skills": ["repo/review"]` field
in `turn.submit`, separate from `prompt`. Each surface submits qualified IDs,
not instructions to reinterpret the prompt. `exec` and the service-backed TUI
refuse explicit selection if the connected service does not support it. Reusing
`--run-id` requires the same prompt and the same ordered selection list; the
prompt digest still hashes only the prompt.

Before the first model request, the shared backend:

1. Deduplicates the turn's explicit IDs in order. It does not scan the prompt,
   history, assistant text, or tool output for selections.
2. Runs actual `load_skill` calls through the existing policy, approval channel,
   cancellation checks, and output budget.
3. Records the calls and results as a normal, replayable tool exchange. The
   original user prompt remains unchanged, including its digest in headless runs.
4. Sends the resulting instructions to the model as tool results. Each result
   identifies the skill source and base directory for supporting paths.

This is deterministic loading, not an instruction asking the model to decide
whether to load the skill. Duplicate references load the body only once per
turn. Explicitly selected manual-only skills become available for subsequent
supporting-file calls during that turn. They are not automatically re-enabled
by their presence in history on later turns.

A failed, denied, or cancelled explicit load prevents inference. Instructions
that exceed the turn output budget are not sent partially. Headless execution
denies an `ask: true` load when no operator is attending; explicit syntax does
not bypass approvals. Automatic loads retain normal tool-loop error behavior.

## Supporting files and permissions

After loading a skill, the model can read a supporting file with:

```json
{"name": "repo/review", "file": "references/checklist.md"}
```

Paths are relative to that skill's directory, not the workspace. `references/`,
`scripts/`, and `assets/` text files can be read without frontmatter declarations.
Reading a script never executes it; executing it still requires a separate
command approval. Omit `file` to read the instruction body without frontmatter.

Reads require regular UTF-8 files and enforce a 32 KiB limit, including a bounded
read if the file grows. Absolute paths, parent traversal, backslashes, drive/ADS
syntax, and symbolic links are refused. macOS/Linux also enforce no-follow
component opens through directory descriptors; other platforms use checked
pathname opens. These checks are not a sandbox.

An `action: skill` rule must match the scoped ID, for example
`match: {skill: repo/review}`. A bare name is a configuration error; an approval
for `repo/review` cannot authorize `operator/review`, or vice versa. `repo` is
relative to the turn's workspace, not a pin to a particular repository or revision.
Approval authorizes reading the instructions and bounded supporting files in that
skill's directory, not other tools. To opt in to every current and future
operator skill, choose the `operator-skills` preset in `ahi tools init`, or add
this rule to a policy you control:

```yaml
- id: operator-skills
  action: skill
  match: {skill: "operator/*"}
  effect: approve
```

That opt-in applies to a custom policy. Built-in Read-only and Read-write
already make both skill scopes available. The rule matches the turn runner's OS-resolved home, not
repository skills. Exact deny rules still win; add `ask: true` to require
approval before loading. Reading `scripts/` files does not authorize execution. The audit log records the scoped ID, source,
file, size, and content hash, not the instruction text. Session tool results retain
the content for replay.

## Executing bundled scripts (Unix)

To allow commands independently of skill reads, opt into `action: skill_command`.
For example, this checkout's `ahi.tools.yaml` approves both scopes:

```yaml
- id: operator-skill-scripts
  action: skill_command
  match: {skill: "operator/*", script: "scripts/**"}
  effect: approve
- id: repo-skill-scripts
  action: skill_command
  match: {skill: "repo/*", script: "scripts/**"}
  effect: approve
```

The first selector matches valid installed skill IDs in that scope. The second
matches executable scripts recursively under each skill's `scripts/` directory,
not arbitrary programs elsewhere. For narrower authorization, use an exact
scoped ID and script path, add an argument prefix (`match.argv`), or set
`ask: true`. The tool call supplies an exact target, never a wildcard:

```json
{"skill": "repo/review", "script": "scripts/check.sh", "args": ["--staged"]}
```

`run_command` resolves the script relative to the selected skill; the command
runs in the workspace (or its `path` subdirectory), not the skill directory.
Scripts need an executable bit and a shebang. Symlinks and traversal are
refused. These are pathname checks, not a sandbox or a content pin: changing a
script changes what will run. Output caps, timeouts, cancellation, environment
scrubbing and command auditing use the ordinary command runner. No shell is
inferred from a filename. These opt-ins do not change the default policy or
the read-only `operator-skills` preset.

Skills remain separate from automatic [operator and repository `AGENTS.md` instruction loading](MODEL_CONTEXT.md). Neither instruction source grants tool permissions. Policy-mode approval remains required.
