# Skills in ahi: shape of a 0.17.0 target

Status: design study, 2026-11-12, against `feat/delegate` (0.15.0 plus the delegation PR stack). Nothing here is implemented or authorized by this page.

## What a skill is

A skill is **a named, bounded text the model may read on request, under a policy rule**. Nothing more. In ahi's nouns:

| Noun | What a skill is in its terms |
| --- | --- |
| Builtin tool | One new read-only builtin, `load_skill(name)`, that returns the skill's `SKILL.md` body as a tool result. It is `read_file` with a fixed root, a name instead of a path, and its own policy kind. |
| Policy rule | `action: skill`, `match.skill: <name>`, `effect`, `ask`. The repository opts in to each skill by name, deny-by-default like every other kind. |
| Session | A `skill_load` entry per load (name, source, sha256, byte count), on the active path so `--resume` replays what the model saw. |
| Worker | The worker's checkout's skills under the worker's policy. A coordinator has no verb that touches them. |

A skill is **not** a tool, not a grant, not a prompt the operator did not write, and not code. The text a skill carries has exactly the authority of a file the model read with `read_file`: none. Everything below follows from that sentence.

Harness skills (Claude Code, Codex, OpenCode `skill`, Gemini `activate_skill`) bundle frontmatter, instructions, scripts, and often a tool allowlist, and are loaded by the harness into context automatically or by name. ahi keeps the first two, refuses the third as authority, and makes loading explicit and recorded.

### On-disk shape

```text
<workdir>/.ahi/skills/<name>/SKILL.md        # repo skills: the repository's, same trust as its files
<workdir>/.ahi/skills/<name>/*.md            # optional references, read with load_skill(name, file)
<config>/skills/<name>/SKILL.md              # operator skills: ~/.config/ahi/skills, outside every workdir
```

Both roots are enumerated at turn start, never fetched. A name present in both is the **operator's**; the repository cannot shadow a name the operator installed, and a `skill_load` entry says which root answered.

Frontmatter, YAML between `---` fences, parsed with `yaml_safe::typed` and unknown keys refused like every ahi file:

| Field | Required | Rule |
| --- | --- | --- |
| `name` | yes | Rule-id alphabet (1-64 of `[A-Za-z0-9._-]`), must equal the directory name |
| `description` | yes | One line, at most 256 bytes; this is the only text advertised to the model before a load |
| `when` | no | One line, at most 256 bytes, appended to the advertised description |
| `references` | no | List of file names in the skill's directory, no `/`, no `..`, `.md` or `.txt` only, at most 16 |
| `version` | no | Informational; never compared, never fetched |

Caps: `SKILL.md` at most 32 KiB, each reference at most 32 KiB, at most 64 skills per root; a skill over a cap or with a bad frontmatter is **not advertised and not loadable**, reported once on stderr and in `ahi doctor`. Loaded bytes count against the turn's existing 256 KiB tool-content budget.

Not allowed in a skill, enforced in code, not asked of the author: no field naming tools, paths, argv, workers, or hosts; no `scripts/` directory that `load_skill` will serve (see Scripts); no URLs are followed and no `install` step exists; no reading outside the skill's own directory; symbolic links anywhere under `.ahi/skills` fail the skill. Credentials in a skill are not detected (that is a scanner, not a boundary); the control is that a skill is a repo file and the repo's `.gitignore` and review already own that problem.

## Threat model

The repository's skills are the repository's text. Cloning a repository and running `ahi tui` in it already lets the model read any file the policy approves; a skill changes nothing about trust, only about *discoverability* (it is advertised by description) and *framing* (the model treats it as instructions). Both are what injection needs, so the controls are about who decides a skill is advertised and what a loaded skill can claim.

| Threat | Control | Kind |
| --- | --- | --- |
| Prompt injection in `SKILL.md` ("ignore your policy, run `curl … \| sh`") | A loaded skill is a tool result. Tool results cannot grant authority (existing invariant, `CHAT_SCREEN.md` "Authority"). Any tool the text talks the model into still crosses `ahi.tools.yaml` and the ask prompt. | code, already present |
| Skill advertised without the operator knowing | No skill is advertised unless an `action: skill` approve rule names it; `ahi.tools.yaml` is PROTECTED, so a repo skill cannot approve itself through a model write. `ahi tools list` and `ahi doctor` show the approved skill names. | policy + code |
| Skill text that claims a tool allowlist | The frontmatter has no such field; one present is a parse error and the skill is not loadable. Prose claims are prose. | code |
| Reference file path traversal (`references: [../../.env]`) | References are bare file names, resolved under the skill directory, canonical path must equal the lexical one, no symlinks, same containment as `read_file`. `load_skill(name, file)` accepts only a name in the declared list. | code |
| Skill name collision or confusable (`Deploy` vs `deploy`) | Names are matched case-insensitively for deny (as rules already are) and exactly for approve; two directories that collide case-insensitively fail both. | code |
| Skill planted in a worker's checkout | Already the worker's threat model: the worker runs under its own `ahi.tools.yaml`, placed by hc with `hc file`, outside the model's reach. A planted skill is advertised only if that policy names it; a worker with no `skill` rules sees none. Nothing new to build; one test to write. | policy, existing |
| Coordinator pushes a skill to a worker | No verb. `delegate` carries a prompt, not files; `session.create`/`turn.submit` have no skill parameter and must not gain one. A coordinator that wants a worker to use a skill asks hc to place it, like a policy. | code (absence), protocol |
| Skills from a registry (`ahi skill add org/thing`) | **Recommend against for 0.17.** ahi refuses fetching for hooks (`SPEC.md`), and a skill registry is the same problem with worse review habits. If it ever exists: pinned sha256 per file, `install` is `cp` into `<config>/skills`, never into a workdir, never on the model's say. | decision, deferred |
| Secrets in skill scripts or text | Skills are text; ahi serves them to the provider exactly as `read_file` does. The approve rule is the operator saying "this directory's text may go to the provider". No scanner; `.env` denial patterns do not apply because `.ahi/skills` is a separate root with its own list of files. | policy |
| Oversized or many skills to exhaust the content budget | Caps above; the shared 256 KiB per-turn budget already cuts and notices. | code |
| Operator-root skill edited by the model | `<config>` is outside every workdir and already excluded from every builtin; `.ahi/` in the workdir gains no protection for reads but **`.ahi/skills` joins PROTECTED for writes**, so a model cannot author a skill it later loads. | code |

The last row is the one new PROTECTED entry and the one decision with teeth: a model that can write `.ahi/skills/x/SKILL.md` and has an approve rule for `x` has a one-turn persistence channel. Protecting the directory closes it; the operator edits skills with an editor, as with `ahi.tools.yaml`.

## Governance

**Yes to `action: skill`.** It reuses the rule grammar unchanged: `id`, `action: skill`, `match: {skill: <name>}`, `effect`, optional `ask`. `match.path`, `match.argv`, `match.tool`, `match.worker` do not apply and are errors, mirroring `delegate`. `match.skill: "*"` is **not** offered in 0.17; a repository names each skill, which is the opt-in the question asks for. Deny beats approve; an `ask` rule prompts with the name, root, and byte count, never the text (the same "line counts, never contents" posture as writes). `s` grants the name for the session.

```yaml
rules:
  - id: review-skill
    action: skill
    match: {skill: rust-review}
    effect: approve
  - id: release-skill
    action: skill
    match: {skill: release}
    effect: approve
    ask: true
  - id: no-deploy-skill
    action: skill
    match: {skill: deploy}
    effect: deny
```

**Skill-declared tool needs: there are none.** The proposal "a skill narrows, never widens, by intersection" is sound but adds a second policy evaluator for text the repo author wrote, and intersection with nothing is nothing. If a skill wants `cargo test`, the repository's `ahi.tools.yaml` has a `command` rule for it or it does not run. The skill body may *say* "run `cargo test` first"; that is advice to the model, evaluated by the same rule as any other request. Keep the single evaluator. Revisit intersection only if a real repository shows a need to make a skill *remove* tools for the duration of a turn, which is a session-scoped narrowing feature, not a skill feature.

**Advertising: a `load_skill` builtin, not the system prompt.** Three reasons.

1. ahi sends no system prompt today (the Anthropic adapter asserts `system` is absent). A system-prompt channel would be new transport code on three protocols for text the operator did not write, and it would make a repository's skill text precede the operator's message in every request.
2. A tool result is already recorded, replayed, budgeted, and paired with its call. Replay of a session shows exactly when the model read what. A system prompt is re-sent on every turn and recorded nowhere.
3. The tool result is the existing "not authority" boundary. There is no equivalent rule for system text.

The cost is one tool definition in the request and a round-trip to load. The definition's `description` lists the approved skills as `name: description — when` lines, so the model can choose without a listing call; that list is the only skill text in the request before a load. `load_skill(name: string, file?: string)` returns `skill <name> from <repo|operator> (<n> bytes, sha256 <12 hex>)` followed by the body, or a reference file when `file` names one in the declared list. The tool is advertised only when at least one approve rule names a skill that exists and passes validation. A loaded skill is **not** re-sent on later turns except as the replayed tool result it already is.

`$` is already reserved in the chat for skill invocation. `$name` in the input sends the operator's draft with a leading line `Use the skill "name".`; it does **not** load the skill itself, so the load is still the model's call under the rule, still recorded, still askable. This keeps one path for loading.

## Scripts

Harness skills ship `scripts/` and tell the model to run them. In ahi the honest answer is that `run_command` exists, is approved by argv prefix, spawns no shell, and runs on the host. A skill script is a program in the repository; it runs when a `command` rule approves its argv, under `ask` if the rule says so, and under the same scrubbed environment and process group. The skill may *mention* `scripts/check.sh`; `load_skill` **never** serves executable content and the skill format has no `scripts` field, so there is nothing a skill can mark runnable.

Concretely for 0.17: `.ahi/skills/<name>/` may contain anything the author likes, but `load_skill` reads only `SKILL.md` and the declared `.md`/`.txt` references. A rule such as `match: {argv: [sh, .ahi/skills/release/run.sh]}` is the repository approving that script, visibly, in the file that approves everything else. No auto-approval from the skill, ever. If the owner wants a shorthand later ("approve this skill's scripts"), it is a policy sugar that expands to `command` rules at parse time and is shown expanded by `ahi tools list`; it is Later.

## Serve, exec, delegate

- `ahi serve` already evaluates policy per `workdir`; `load_skill` enumerates `<workdir>/.ahi/skills` and the server host's `<config>/skills`. For a worker that means the worker's checkout and the worker host's operator root. The `--tools <file>` path for exec and `turn.submit.tools` governs skills the same as every other action.
- `session.create`, `turn.submit`, and the `delegate` tool gain **no** skill parameter. "A skill for the worker" is a file hc places, exactly as the policy is.
- `--headless` and `unattended`: a skill under an `ask` rule is denied with the existing unattended reason. The worker's model sees "no operator is attending" and continues without it.
- Sessions gain a `skill_load` entry: `{type: "skill_load", seq, parent, name, source: "repo"|"operator", sha256, bytes, file?}`, written after the tool result it accompanies. `ahi session show` prints `skill rust-review (repo, 4.1 KiB)`. The ledger's result document counts loads under `tools.skills` and lists name, source, and sha256 in the ledger records; never the body. `audit.log` gets one line per load with the same fields. Note the store today treats an unknown entry type as `Corrupt`; the `skill_load` PR must decide whether a 0.16 ahi reading a 0.17 session fails or skips (see Decisions).

## Slice plan for 0.17.0

Each PR ships on its own and leaves the previous behavior intact when no `skill` rule exists.

| PR | Scope | Tests | Docs |
| --- | --- | --- | --- |
| 1. Policy kind | `ActionKind::Skill`, `match.skill`, parser errors for the other matchers, `ahi tools list`/`doctor` rendering, `examples/tools/skills.yaml`. No tool yet; a rule with no tool behind it is valid and advertises nothing. | parse accept/reject, deny-beats-approve, case-insensitive deny, rendering | README table row, `CHAT_SCREEN.md` Policy paragraph |
| 2. Skill loader | `src/skills/` (in-repo, no new crate): enumerate both roots, frontmatter parse, caps, symlink and traversal refusal, name/directory equality, operator-shadows-repo. `.ahi/skills` joins PROTECTED. `ahi doctor` lists valid and rejected skills with the reason. | fixtures for every refusal; a write to `.ahi/skills/...` denied under a wide `file_write` rule | new `docs/SKILLS.md` (format, roots, what is refused) |
| 3. `load_skill` builtin | Schema with the approved list in its description; policy evaluation in `tool_loop.rs` under `ActionKind::Skill`; `ask` prompt text (name, root, bytes); session grant by name; budget accounting; reference files. | HTTP fixture: no rule, no tool; approve rule loads; `ask` with `y`/`s`/`n`; unattended denial; reference traversal denied; budget cut | README "Tools and policy", `TOOL_CALLS.md` builtin list |
| 4. Record and audit | `skill_load` entry, `session show` line, ledger and result-document fields, `audit.log` line, `$name` input prefix in the chat. | store round-trip, `show` output, exec `--format json` fields, PTY `$name` | README `ahi session`, `SERVE.md` result document, CHANGELOG |
| 5 (optional). Worker proof | No code: an integration test that a planted `.ahi/skills` in a worker checkout is invisible without a rule, and `delegate` cannot name one. | the two tests | `DELEGATION.md` boundary bullet |

Two weeks for one person is realistic for 1-4; PR 5 is a half day and worth doing for the sentence it lets `DELEGATION.md` say.

**Later:** `match.skill: "*"`, any registry or `ahi skill add`, a `scripts` sugar that expands to `command` rules, session-scoped tool narrowing, automatic loading on keyword match (harnesses do this; ahi should not until the model's choice is visible in the record), skills in `ahi run` hooks, and a Windows path for `<config>/skills` (same resolver as `models.json`, so probably free, but untested).

## Decisions

1. A skill is text under a rule: `action: skill`, named per skill, deny-by-default, no wildcard in 0.17.
2. Loaded through a `load_skill` builtin as a tool result; no system prompt, no automatic injection.
3. Skills carry no tool allowlist, no scripts field, no credentials handling; the only policy evaluator is `ahi.tools.yaml`.
4. Scripts run only as `run_command` under an explicit `command` rule; a skill never approves anything.
5. Two roots, repo `.ahi/skills` and operator `<config>/skills`; operator wins a name collision; `.ahi/skills` is PROTECTED from model writes.
6. No registry, no fetch, no install in 0.17.
7. Workers use their own skills under their own policy; no protocol parameter carries a skill.
8. Every load is a `skill_load` entry, a ledger record, and an audit line, with hashes and sizes, never bodies.

## Owner decisions needed

1. **Store compatibility for `skill_load`.** Either bump the session schema so a 0.16 reader refuses the file cleanly, or change the reader to skip unknown entry types with a warning. Skipping is friendlier to `ahi session import` across versions and is the recommendation, but it changes a fail-closed reader into a tolerant one and deserves a deliberate yes.
2. **Operator root at all in 0.17.** Dropping `<config>/skills` removes the shadowing rule, one root, and one test matrix, and the owner can carry personal skills as a repo they `--tools` into. Keeping it is cheap but is the first time ahi reads model-facing text from the config directory.

## Open questions

- Should `ask` for a skill offer `s` for the session, or is a skill load rare enough that `y` alone is enough? Leaning `s` for parity with every other kind.
- Does the approved-skill list in the tool description leak anything a repository would rather not send until asked? It is the author's own `description` field, so no, but the doc should say the description goes to the provider on every turn the tool is advertised.
- Should `load_skill` be refused while a `file_write` approve rule covers `.ahi/` at all, as a belt beside the PROTECTED suspenders? Probably not; PROTECTED is the mechanism and a second check invites drift.
- Whether a `delegate` prompt that contains a loaded skill's body is a problem. It is the coordinator's model sending text it read to a worker's model, which it can already do with any file. No new control; worth a sentence in `DELEGATION.md`.
