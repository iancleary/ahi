# Terminal commands and gateway onboarding

Subsequent backend work is documented in [Gateway model configuration](GATEWAY_MODEL_CONFIGURATION.md), [OpenAI inference](OPENAI_INFERENCE.md), and [Tool calls](TOOL_CALLS.md). The audit below records the original UX problem; current discovery accepts both supported catalog shapes and honors a validated returned inference path, never a guessed `/v1`.

## Product audit

The task is not to make three slash-command parsers accept the same spelling. The task is to make the available actions visible and make adding a gateway lead naturally to a usable model.

Observed before this refresh:

| Finding | User consequence | Owning boundary |
| --- | --- | --- |
| Chat has an input-anchored Settings popup; models and catalog have only an append-only command string | Learning one screen does not teach the others; `/gateway` appears absent | Former `src/chat/settings.rs` (replaced by `src/commands.rs`), `src/tui.rs` |
| Gateway/login help appears late in long wrapped footer text | Important onboarding controls disappear on narrow terminals | Picker layout and help |
| “Settings” mixes navigation, registration, authentication, and exit | The menu name does not predict its contents | Command presentation |
| The initial title foregrounds `models.json` and curation terminology | A new user sees implementation details before an action | Models view and empty state |
| URL entry, discovery, review, registration, curation, and active selection have no visible progression | A successful connection still leaves the user asking what to do next | Gateway form and catalog handoff |
| Returning from catalog resets the picker to the first provider | The new gateway is lost among existing providers | Picker navigation state |
| Enter does nothing in catalog, including on a checked model | Starting a chat requires an unexplained extra navigation step | Catalog selection |
| A registration warning from chat is written behind the subsequently opened catalog | Partial success is not visible when the user needs to recover | Structured handoff |

The existing persistence rules are valuable and must survive the UI change: discovery is read-only, registration is explicit, catalog membership is not model selection, and only an explicit use-model action changes the active chat model.

## Competing interaction sketches

### A. Permanent application sidebar

```text
┌ Actions ───────┬ Models ─────────────────────────┐
│ Add gateway   │ Provider / model list            │
│ Sign in       │                                   │
│ Models        │                                   │
│ Catalog       │                                   │
│ Quit          │                                   │
└───────────────┴───────────────────────────────────┘
```

This makes all actions visible, but permanently reduces transcript and model-list width. It changes the established chat layout and is particularly costly at 80 columns. Rejected for this refresh.

### B. Shared command palette and guided onboarding — selected

```text
Models                         Saved in models.json
  OpenAI
    No models selected

  Add your first gateway: press / and choose Add gateway.

┌ Commands · ↑/↓ select · Enter open · Esc dismiss ┐
│ › /gateway   Add gateway                         │
│   /models    Choose model                        │
│   /catalog   Browse provider catalog             │
│   /login     Sign in with OpenAI                 │
│   /quit      Quit ahi                           │
└─────────────────────────────────────────────────┘
/                         / commands · Enter use model
```

```text
Add gateway
1 Connect  →  2 Review  →  3 Choose models

Gateway URL
┌─────────────────────────────────────────────────┐
│ https://gateway.example/v1                       │
└─────────────────────────────────────────────────┘
Use the base URL; a validated catalog may supply /v1.
Discovery: https://gateway.example/agent/models.json

Enter discover                         Esc cancel
```

```text
Catalog · lab                     3 Choose models
Gateway registered. No chat model has changed.

  [ ] vendor/model-a
› [x] vendor/model-b

Space add/remove · Enter use checked model · Esc models
/ commands
```

This extends the interaction already learned in chat, keeps the transcript wide, makes actions available in every context, and removes the unnecessary return-to-models step before starting chat. The palette is shared, not three visually similar implementations. Gateway confirmation and curation remain deliberate operations.

### C. Startup-only setup wizard

```text
Welcome to ahi
  1. Add a gateway
  2. Select models
  3. Start chat
```

A dedicated first-run wizard can be welcoming, but creates a second interaction model for repeat onboarding and does not fix existing chat/catalog/model inconsistency. Rejected as a separate mode. The selected design borrows its progress and next-step guidance without adding a startup-only workflow.

## Chosen interaction contract

### One command grammar, one menu

- `/` opens **Commands** in chat, Models, and Catalog. Typing filters; Up/Down selects; Enter applies; Esc dismisses without executing.
- Common actions use one registry for canonical commands, labels, descriptions, and applicable contexts. Shared actions include Add gateway, Choose model, Browse catalog, Sign in, and Quit. Chat-only thinking/mouse controls stay contextual.
- The command input and popup are anchored consistently near the bottom. Hints for opening commands remain visible rather than being buried inside a wrapped paragraph.
- Exact commands and established aliases remain available. Unknown commands never become model prompts.
- Back/cancel and application exit are separate outcomes. A command called Quit must not merely dismiss a nested picker and return to chat.
- Chat preserves the existing response-in-progress rule: only quit interrupts; other action drafts remain until the response finishes.

### A gateway is not yet a usable chat model

The visible sequence is **Connect → Review → Choose models**:

1. Connect: editable base URL with an example and an explanation of inference path versus discovery origin. Discovery is explicit and sends no credentials.
2. Review: identify create versus rediscover, show the effective destination and authentication marker/reference, summarize available models, and make confirmation versus back clear. Do not imply an existing gateway's authentication will be replaced by catalog defaults.
3. Choose models: immediately display the provider catalog, registration result, warnings, and next action. Space saves model availability. Enter on a checked model explicitly uses it and starts/resumes chat. Enter on an unchecked model explains that it must be added first.

No automatic first-model selection, inferred `/v1`, background startup discovery, remote authentication setup, or paid verification is part of this refresh. Empty catalogs and cache failures must leave a visible recovery path. A successful registration with a cache failure is partial success, not cancellation.

### Preserve place and work

Cancelling a form returns to the originating screen. Navigating back from catalog keeps focus on that provider, preferably the relevant curated model. Starting or changing a model preserves chat history. A palette must not consume list navigation after dismissal or send its draft to inference. External-edit conflicts remain protected by the existing atomic configuration writer.

## Architecture and verification

The organizing shapes are a context-aware command registry and command-menu state, a typed picker outcome (selected model, back, application exit), and typed gateway completion containing provider identity plus registration/cache status. The screen loops still own terminal lifecycle and business-action execution; shared presentation must not become a second application controller.

Verification must exercise actual terminal behavior, not just parser functions. Extend `scripts/smoke-model-picker.py` using disposable state and the existing local gateway. Required evidence includes palette access/filter/navigation/dismissal in all three screens; an empty-installation path through connection, confirmation, explicit curation, and streamed chat; correct focus and exit semantics; existing state/conflict/cancellation guarantees; and usable controls in narrower terminal dimensions.

The sketches above are design alternatives, not pixel-accurate screenshots or claims about a released binary. The following implementation is verified locally; it has not been published.

### Implemented structure

- [`src/commands.rs`](../src/commands.rs): `CommandDefinition` supplies canonical spellings, aliases, labels, descriptions, common actions, and applicable `CommandContext`s. `CommandMenu` owns matching, navigation, selection/dismissal state, and popup rendering. All three screens consume it. The former chat-only settings module is removed.
- [`src/tui.rs`](../src/tui.rs): Models/Catalog presentation, bounded single-line command entry with bracketed-paste lifecycle, state-aware empty guidance, context-preserving Back, explicit checked-model use, and visible registration outcomes. `PickerOutcome::{Selected, Back, Exit}` prevents Quit from being confused with cancelling a nested picker.
- [`src/chat/mod.rs`](../src/chat/mod.rs): the existing chat editor and response lifecycle consume the shared menu. Handoffs retain transcript and mouse preference; registration is carried directly to Catalog. The input rule keeps a `/ commands` hint.
- [`src/gateway.rs`](../src/gateway.rs): focused Connect URL entry, retained input/error retry, loading/cancel state, Review summary, and fixed-position controls. Review shows new/existing/conflicting registration and existing effective authentication when rediscovering.
- [`src/harness/gateway.rs`](../src/harness/gateway.rs): `CatalogPersistence::{Saved, Failed(String)}` travels with `GatewayRegistration`. No presentation logic infers partial success by parsing a notice string. Existing snapshot/atomic-save and credential restrictions remain in force.

Onboarding-specific Catalog step labels end when you leave that view. Returning from a multi-model Catalog retains the highlighted checked model without making a selection write. An existing gateway with no selected models prompts catalog choice, not first-gateway setup. Raw `models.json` empty-reason diagnostics remain unchanged for the non-terminal interface but are not the primary terminal copy.

### Reproduce the observable proof

```sh
cargo fmt --all -- --check
cargo clippy --target-dir target/terminal-experience-verification --all-targets --all-features -- -D warnings
cargo test --target-dir target/terminal-experience-verification --all-features
cargo build --target-dir target/terminal-experience-verification
uv run scripts/smoke-model-picker.py \
  --binary target/terminal-experience-verification/debug/ahi \
  --capture-directory target/terminal-experience-captures
```

The maintained runner uses real PTYs, disposable state, and a loopback HTTP gateway. All existing Rust checks and this workflow passed during implementation and lead verification. No new unit tests or external model calls were added. The runner now exercises common palettes and aliases, keyboard and paste interaction, empty-installation onboarding, explicit Space/Enter decisions, a real streamed response, retained conversation context and active-response drafts, relevant-model Back, nested `/quit`, validation/conflicts/cancellation, and immediate partial-cache failure/recovery. The caller may omit `--capture-directory` when captures are unnecessary.

Representative generated `.txt` screens and `.ansi` terminal streams appear beneath the selected capture directory:

| Artifact | What to inspect |
| --- | --- |
| `models-empty-80x24.txt` | New-installation action and discoverable Commands hint |
| `models-commands-80x24.txt`, `catalog-commands-80x24.txt`, `chat-commands-80x24.txt` | The same common command ordering, popup, and controls |
| `gateway-new-review-80x24.txt`, `gateway-review-60x24.txt` | Review details with confirmation/cancellation controls still visible |
| `catalog-next-steps-80x24.txt` | Immediate registration result and explicit Space/Enter next steps |
| `models-catalog-return-second-80x24.txt` | Return to the second curated model without rewriting curation |
| `models-registered-empty-80x24.txt`, `models-registered-uncached-80x24.txt` | Registered-but-unconfigured and unavailable-catalog recovery paths |
| `catalog-cache-warning-80x24.txt`, `catalog-cache-recovered-80x24.txt` | Partial success visible at the point of recovery, then usable models after refresh |

These are generated local artifacts, not committed screenshots or a complete terminal emulator. Reconstructed text omits styling; the ANSI stream preserves emitted styles. Tests cover 80×24 and selected 60×24 layouts, not every terminal/font/protocol. Live gateway reachability, successful live OpenAI authentication, and native Windows terminal behavior remain unverified. Discovery cancellation prevents later persistence but does not promise immediate termination of an in-flight HTTP request.

## Delivery checklist

- [x] Audit the current interaction and persistence boundaries.
- [x] Compare alternative layouts and choose a shared palette with guided onboarding.
- [x] Implement the shared registry/menu and guided gateway/catalog transitions.
- [x] Verify the user-facing flows and preserve existing checks.
- [x] Update operational documentation and the maintained verification skill.

Throughput checkpoint:

- [x] **Blocking first steps:** inspect existing uncommitted work, audit the current screens, and choose the interaction contract before implementation.
- [x] **Independent workstreams:** architecture audit and design documentation are separate artifacts; the code-coupled implementation has one owner.
- [x] **Shared mutable state:** only the implementation owner changes source/tests; lead-owned design documentation is separate. Verification processes use disposable state and isolated Cargo output directories. Git writes are not authorized.
- [x] **Smallest safe decomposition:** one implementation worker owns the registry, all consumers, gateway handoffs, and terminal proof because splitting those interfaces would create integration churn.
