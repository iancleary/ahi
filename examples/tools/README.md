# Tool policy examples

Each file is a complete `ahi.tools.yaml` for the chat screen's builtin,
read-only tools (`read_file`, `list_dir`, `find_files`, `search_files`). Copy one to a repository root as
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
| `deny-all.yaml` | Valid, but no approve rule: no tool is advertised and any requested call fails with `tool execution is disabled`. |
| `invalid-version.yaml` | Deliberately invalid (`version: 2`): `ahi tui --tools` exits with the parse error, and as `ahi.tools.yaml` every turn fails closed. |
