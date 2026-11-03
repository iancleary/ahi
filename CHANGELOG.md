# Changelog

## 0.3.0 (unreleased)

### Breaking changes

- `ahi tui` loads only local configuration and catalogs. It no longer fetches gateway catalogs at startup.
- The main picker offers exactly `providers.<id>.models` from the state directory's `models.json`, in saved order. Catalogs never supply picker defaults. A gateway with omitted or empty `models` no longer inherits catalog IDs, even after refresh. Curate desired IDs with the catalog checkboxes or editor to migrate from 0.2.0. The `kept-config` fallback is removed because configured IDs always remain the picker list.
- `LoadedProvider.models` carries an ID and catalog-membership status per entry. Catalog metadata/errors are separate fields; `Registry` retains original config bytes for stale-save detection. Non-terminal output adds membership, empty-reason, and error lines after provider summaries.
- Interactive selection exits with a save confirmation instead of the provider dump. It still writes `selection` and does not start a conversation.
- OpenAI login uses the current ChatGPT direct-token flow, not the old Codex public-client flow. Pasted input must be a full callback URL with state and issued client ID. Conflicting OpenAI proxy origins are refused rather than receiving browser tokens; register proxies as separate gateway IDs.

### Added

- Explicit catalog refresh with `r` or `ahi tui --refresh`. In a pipe, failed refreshes retain/print the local load and return exit status 1.
- `c` opens catalog curation: Space toggles inclusion, Shift+Up/Down or K/J reorders checked models, and each successful change atomically saves `models.json`. Checked IDs appear first in saved order; configured IDs missing from the catalog remain visible, checked and removable. `y` copies an ID using an available clipboard tool or prints it on exit.
- `e` opens `models.json` in VISUAL/EDITOR (notepad fallback on Windows); editor-only reload preserves exact bytes. Explicit checkbox/reorder saves may create the file and pretty-print JSON while preserving other providers, settings and object-key order. Stale external edits and failed saves preserve the previous loaded registry and file bytes.
- OpenAI entries may curate models. All curated IDs remain visible, marked listed, absent from catalog or catalog unavailable. Empty lists explain how to edit or curate.
- Catalog objects record provider, source URL, RFC 3339 retrieval time and reported IDs. Writes use exclusive `.json.tmp`, sync and rename; failed refreshes retain previous bytes. Old arrays still load and show `retrieval time unknown (old catalog format)`, referring only to a missing cache timestamp, not OAuth.
- Current OpenAI direct-token OAuth includes dynamic issued-client registration, per-installation public UUID, PKCE/state/nonce, listener readiness before browser launch, strict callback parsing/cleanup, issued-client persistence, expiry and bounded refresh rotation. Credential replacement is atomic and mode 0600 on Unix. Catalog refresh uses only the fixed Platform resource with new marked credentials; tokens/response bodies are never printed or stored in catalogs.
- JSON/SARIF report object keys remain sorted despite order-preserving config serialization; existing report goldens are unchanged.

### OpenAI migration and discovery limitation

The previous fixed-client flow issued Codex subscription credentials. Existing credentials remain untouched until a validated new `ahi login` succeeds; old credentials are not silently sent to Platform or Codex. The earlier manual Platform `/models` HTTP 403 used an old-flow credential and is not evidence about new direct-token grants.

A subsequent live `/v1/models` request with a new direct-token credential returned HTTP 200 and `models[].slug` metadata. The decoder now accepts that format and standard `data[].id`, preserving all reported IDs in order, with local HTTP regression coverage. Automated checks have not exercised the interactive browser flow. Discovery remains best-effort; a permission failure preserves cached catalogs and curated selections. Pi uses this grant for Responses, not Decisions, and uses bundled model definitions; ahi copies no default model list and adds no inference/conversation runner. No Codex fallback or real completion request was added.
