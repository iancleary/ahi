# Tool policy examples

Each file is a complete `ahi.tools.yaml` for the chat screen's builtin,
tools: the read-only `read_file`, `list_dir`, `find_files`, and `search_files`
(`action: tool_call`), and the file-changing `edit_file` and `write_file`
(`action: file_write`). Copy one to a repository root as
`ahi.tools.yaml`, or point a session at it without copying:

```sh
ahi tui --tools examples/tools/ask-every-read.yaml
```

Rule paths and tool confinement are always relative to the directory where
`ahi tui` started, not to the policy file. The file must exist and parse;
`ahi tui` refuses to start otherwise.

| File | What the model gets |
|---|---|
| `read-all.yaml` | Read, list, find, and search anywhere in the tree except `.env`; no prompts. The repository root ships the same policy. |
| `ask-every-read.yaml` | Every read and listing asks in chat (`y` once, `s` for the session, `n` deny). |
| `sources-only.yaml` | Listing anywhere; reads only under `src` and `docs`, with `.env` and `target` denied. |
| `list-only.yaml` | Listing only. The model can see the tree but reads are denied. |
| `edit-sources.yaml` | Reads anywhere; `edit_file` under `src` and `tests` asks first; `write_file` may create files under `tests` without asking; `Cargo.lock` is never written. |
| `deny-all.yaml` | Valid, but no approve rule: no tool is advertised and any requested call fails with `tool execution is disabled`. |
| `invalid-version.yaml` | Deliberately invalid (`version: 2`): `ahi tui --tools` exits with the parse error, and as `ahi.tools.yaml` every turn fails closed. |
