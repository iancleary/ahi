#!/usr/bin/env python3
"""Exercise the real terminal picker with disposable local state (POSIX only).

Run after cargo build: python3 scripts/smoke-model-picker.py
No network, credentials, clipboard, or real editor configuration is used.
"""
import argparse
import errno
import json
import os
from pathlib import Path
import re
import select
import shutil
import struct
import subprocess
import tempfile
import time


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ahi"))
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("this PTY smoke check requires POSIX")
    import fcntl
    import pty
    import termios

    binary = args.binary.resolve(strict=True)

    class Session:
        def __init__(self, state: Path, editor: Path | None = None,
                     cwd: Path | None = None, git_bin_dir: Path | None = None) -> None:
            self.pid, self.fd = pty.fork()
            if self.pid == 0:
                env = os.environ.copy()
                # Hide installed clipboard programs, and do not inherit an editor.
                env.update(TERM="xterm-256color", PATH=str(git_bin_dir) if git_bin_dir else "/nonexistent")
                if cwd:
                    os.chdir(cwd)
                env.pop("VISUAL", None)
                env.pop("EDITOR", None)
                if editor:
                    env["VISUAL"] = f"/bin/sh '{editor}'"
                os.execve(binary, [str(binary), "--state-dir", str(state), "tui"], env)
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 160, 0, 0))
            self.output = b""
            self.recent = b""
            self.dead = False

        def read(self) -> None:
            if select.select([self.fd], [], [], 0.05)[0]:
                try:
                    data = os.read(self.fd, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        return
                    raise
                self.output += data
                self.recent += data
                if b"\x1b[6n" in data:
                    os.write(self.fd, b"\x1b[1;1R")

        def screen(self) -> str:
            # Ratatui emits cell diffs, not complete lines: unchanged letters
            # are absent from the stream. Replay absolute cursor moves instead
            # of treating escape-stripped output as a screenshot.
            cells = [[" "] * 160 for _ in range(30)]
            row = col = 0
            parts = re.split(
                r"(\x1b\[[0-?]*[ -/]*[@-~])", self.output.decode(errors="replace")
            )
            for part in parts:
                if part.startswith("\x1b["):
                    if part.endswith(("H", "f")):
                        values = part[2:-1].split(";")
                        row = int(values[0] or "1") - 1
                        col = int(values[1] or "1") - 1 if len(values) > 1 else 0
                    elif part == "\x1b[2J" or part == "\x1b[?1049h":
                        cells = [[" "] * 160 for _ in range(30)]
                    continue
                for char in part:
                    if char == "\r":
                        col = 0
                    elif char == "\n":
                        row += 1
                    elif char >= " ":
                        if 0 <= row < 30 and 0 <= col < 160:
                            cells[row][col] = char
                        col += 1
            return "\n".join("".join(line) for line in cells)

        def expect(self, text: str) -> None:
            end = time.monotonic() + 10
            while text not in self.screen() and time.monotonic() < end:
                self.read()
            assert text in self.screen(), (text, self.screen())

        def send(self, keys: str) -> None:
            self.recent = b""
            os.write(self.fd, keys.encode())

        def finish(self) -> str:
            end = time.monotonic() + 10
            while time.monotonic() < end:
                self.read()
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                if pid:
                    self.dead = True
                    self.read()
                    assert os.waitstatus_to_exitcode(status) == 0, self.output
                    return self.output.decode(errors="replace")
            raise AssertionError("picker did not exit")

        def close(self) -> None:
            if not self.dead:
                os.kill(self.pid, 9)
                os.waitpid(self.pid, 0)
            os.close(self.fd)

    with tempfile.TemporaryDirectory(prefix="ahi-picker-") as temp:
        state = Path(temp)
        (state / "catalogs").mkdir()
        (state / "catalogs/openai.json").write_text(
            json.dumps(["first", "second", "catalog-only", "vendor/model/variant"])
        )
        config = {"providers": {"openai": {
            "baseUrl": "https://api.openai.com/v1", "api": "openai-completions",
            "models": [{"id": "second"}, {"id": "first"}],
        }}}
        body = json.dumps(config, indent=2) + "\n"
        (state / "models.json").write_text(body)
        session = Session(state)
        try:
            session.expect("curated models (Enter opens chat)")
            session.send("r")
            session.expect("OpenAI credential unavailable")
            session.send("c")
            session.expect("catalog — curate models.json")
            session.send("jjy")
            session.expect("No clipboard available")
            session.send("q")
            output = session.finish()
            assert "catalog-only" in output
            assert not (state / "selection").exists()
            assert (state / "models.json").read_text() == body
        finally:
            session.close()

        def expect_ids(session: Session, expected: list[str]) -> None:
            end = time.monotonic() + 10
            while time.monotonic() < end:
                session.read()
                if not (state / "models.json").exists():
                    continue
                saved = json.loads((state / "models.json").read_text())
                actual = [model["id"] for model in saved["providers"]["openai"]["models"]]
                if actual == expected:
                    return
            raise AssertionError((actual, expected, session.screen()))

        # Actual Space, both reorder bindings, persistence, and a new process.
        catalog_body = (state / "catalogs/openai.json").read_bytes()
        session = Session(state)
        try:
            session.expect("curated models (Enter opens chat)")
            session.send("c")
            session.expect("[x] second")
            session.expect("[ ] catalog-only")
            session.expect("retrieval time unknown (old catalog format)")
            session.send("jj ")
            expect_ids(session, ["second", "first", "catalog-only"])
            session.send("K")
            expect_ids(session, ["second", "catalog-only", "first"])
            session.send("\x1b[1;2A")  # Shift+Up
            expect_ids(session, ["catalog-only", "second", "first"])
            session.send("J")
            expect_ids(session, ["second", "catalog-only", "first"])
            session.send("\x1b[1;2B")  # Shift+Down
            expect_ids(session, ["second", "first", "catalog-only"])
            session.send(" ")
            expect_ids(session, ["second", "first"])
            session.send(" ")
            expect_ids(session, ["second", "first", "catalog-only"])
            session.send("K")
            expect_ids(session, ["second", "catalog-only", "first"])
            session.send("jj ")  # The unchecked ID contains embedded slashes.
            expect_ids(session, ["second", "catalog-only", "first", "vendor/model/variant"])
            session.send("KKK")
            expect_ids(session, ["vendor/model/variant", "second", "catalog-only", "first"])
            session.send("q")
            session.finish()
            assert not (state / "selection").exists()
            assert (state / "catalogs/openai.json").read_bytes() == catalog_body
        finally:
            session.close()

        session = Session(state)
        try:
            session.expect("vendor/model/variant")
            session.send("j\r")
            session.expect("(openai) vendor/model/variant • medium")
            assert "128k" not in session.screen()  # Context is initially unknown.
            assert (state / "selection").read_text() == "openai/vendor/model/variant\n"
            # Typing alone wraps the draft into three visual rows and moves
            # the upper rule upward; it must not submit or invent context.
            session.send("0123456789" * 32 + " tail")
            session.expect(" tail")
            draft_rows = session.screen().splitlines()
            assert draft_rows[25] == "0123456789" * 16
            assert draft_rows[26] == "0123456789" * 16
            assert draft_rows[27].startswith(" tail")
            assert all(character == "─" for character in draft_rows[24])
            assert "[stand-in]" not in session.screen()
            assert "128k" not in session.screen()
            session.send("\x03merge smoke\r")  # Clear draft, then submit a message.
            session.expect("[stand-in] merge smoke")
            session.expect("replies and token counts are not from a model")
            session.expect("0% / 128k ──")
            rows = session.screen().splitlines()
            assert rows[-1].endswith("(openai) vendor/model/variant • medium")
            assert all(character == "─" for character in rows[-2])
            assert "↑" not in rows[-1]  # No permanent token-accounting row.

            # /model reloads local curation instead of capturing the startup
            # registry. Reference-only IDs must not become picker defaults.
            original = (state / "models.json").read_bytes()
            updated = json.loads(original)
            updated["providers"]["openai"]["models"] = [
                {"id": "vendor/model/variant"}, {"id": "fresh/from-disk"}
            ]
            (state / "models.json").write_text(json.dumps(updated))
            session.send("/model\r")
            session.expect("curated models (Enter opens chat)")
            session.expect("fresh/from-disk")
            assert "catalog-only" not in session.screen()
            session.send("jj\r")
            session.expect("(openai) fresh/from-disk • medium")
            session.expect("[stand-in] merge smoke")  # Transcript survived switching.
            assert (state / "selection").read_text() == "openai/fresh/from-disk\n"
            session.send("/model\r")
            session.expect("curated models (Enter opens chat)")
            session.send("q")
            session.expect("(openai) fresh/from-disk • medium")
            assert (state / "selection").read_text() == "openai/fresh/from-disk\n"
            session.send("\x04")  # Ctrl+D on empty input exits chat.
            output = session.finish()
            assert "Selected openai/vendor/model/variant; saved to" in output
            assert "provider openai origin" not in output
            (state / "models.json").write_bytes(original)
        finally:
            session.close()

        # A configured ID absent from the reference remains checked, can be
        # reordered, survives another process, and is removed only on Space.
        saved = json.loads((state / "models.json").read_text())
        saved["providers"]["openai"]["models"].append({"id": "missing/from-catalog"})
        (state / "models.json").write_text(json.dumps(saved))
        session = Session(state)
        try:
            session.expect("missing/from-catalog")
            session.send("c")
            session.expect("[x] missing/from-catalog  [absent from catalog]")
            session.send("jjjjK")
            expect_ids(session, ["vendor/model/variant", "second", "catalog-only", "missing/from-catalog", "first"])
            session.send("q")
            session.finish()
        finally:
            session.close()
        session = Session(state)
        try:
            session.expect("missing/from-catalog")
            session.send("c")
            session.expect("[x] missing/from-catalog")
            session.send("jjj ")
            expect_ids(session, ["vendor/model/variant", "second", "catalog-only", "first"])
            session.send("q")
            session.finish()
        finally:
            session.close()

        # Conflicting external edits and exclusive-temp failures remain visible
        # and leave both disk bytes and the loaded checkbox selection unchanged.
        session = Session(state)
        try:
            session.expect("vendor/model/variant")
            session.send("c")
            session.expect("[x] vendor/model/variant")
            saved = (state / "models.json").read_bytes()
            (state / "models.json.tmp").write_text("busy writer")
            session.send(" ")
            session.expect("Save failed: cannot create")
            session.expect("[x] vendor/model/variant")
            assert (state / "models.json").read_bytes() == saved
            assert (state / "models.json.tmp").read_text() == "busy writer"
            (state / "models.json.tmp").unlink()
            external = '{"providers":{}}\n'
            (state / "models.json").write_text(external)
            session.send(" ")
            session.expect("changed outside this view; press e")
            session.expect("[x] vendor/model/variant")
            session.send("q")
            session.finish()
            assert (state / "models.json").read_text() == external
        finally:
            session.close()

        # Editor handoff, local reload, ordered selection, and chat entry.
        editor = state / "fixture editor.sh"
        config["providers"]["openai"]["models"] = [{"id": "chosen/from-editor"}]
        edited = json.dumps(config, indent=2) + "\n"
        editor.write_text("printf '%s' '" + edited + "' > \"$1\"\nexit 7\n")
        session = Session(state, editor)
        try:
            session.expect("curated models (Enter opens chat)")
            session.send("e")
            session.expect("Editor exited")
            session.expect("chosen/from-editor")
            session.send("j\r")
            session.expect("(openai) chosen/from-editor • medium")
            session.send("\x04")
            output = session.finish()
            assert "Selected openai/chosen/from-editor; saved to" in output
            assert "provider openai origin" not in output
            assert (state / "selection").read_text() == "openai/chosen/from-editor\n"
            assert (state / "models.json").read_text() == edited
        finally:
            session.close()

        # An invalid edit retains the old registry while leaving the file broken.
        editor.write_text("printf '{\\ninvalid\\n}' > \"$1\"\n")
        session = Session(state, editor)
        try:
            session.expect("chosen/from-editor")
            session.send("e")
            session.expect("line 2")
            session.send("j\r")
            session.expect("(openai) chosen/from-editor • medium")
            session.send("/model\r")
            session.expect("line 2")
            session.expect("chosen/from-editor")
            session.send("q")
            session.expect("(openai) chosen/from-editor • medium")
            session.send("\x04")
            session.finish()
            assert (state / "selection").read_text() == "openai/chosen/from-editor\n"
            assert (state / "models.json").read_text() == "{\ninvalid\n}"
        finally:
            session.close()

        (state / "models.json").unlink()
        session = Session(state)
        try:
            session.expect("no models in models.json")
            session.send("e")
            session.expect("set VISUAL or EDITOR")
            session.send("q")
            session.finish()
            assert not (state / "models.json").exists()
        finally:
            session.close()
        # Browsing and missing-editor actions did not create config; only the
        # following explicit checkbox does. It produces a valid OpenAI entry.
        session = Session(state)
        try:
            session.expect("no models in models.json")
            session.send("c ")
            expect_ids(session, ["first"])
            session.expect("[x] first")
            session.send("q")
            session.finish()
            saved = json.loads((state / "models.json").read_text())
            assert saved["providers"]["openai"]["baseUrl"] == "https://api.openai.com/v1"
            assert saved["providers"]["openai"]["api"] == "openai-completions"
        finally:
            session.close()
        # Real read-only Git status in the displayed workspace. Only git is
        # exposed on PATH, so clipboard/editor isolation stays intact.
        git_executable = shutil.which("git")
        assert git_executable, "git is required for the worktree-status smoke check"
        git_bin_dir = state / "git-bin"
        git_bin_dir.mkdir()
        (git_bin_dir / "git").symlink_to(git_executable)
        repo = state / "git-workspace"
        repo.mkdir()

        def git(*arguments: str) -> None:
            subprocess.run([git_executable, *arguments], cwd=repo, check=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)

        git("init", "-b", "main")
        (repo / "tracked").write_text("staged\n")
        git("add", "tracked")
        (repo / "tracked").write_text("unstaged\n")
        session = Session(state, cwd=repo, git_bin_dir=git_bin_dir)
        try:
            session.expect("curated models (Enter opens chat)")
            session.send("j\r")
            session.expect(" main +1 !1")
            git("symbolic-ref", "HEAD", "refs/heads/tool-branch")
            git("add", "tracked")
            session.send("status recheck\r")
            session.expect(" tool-branch +1")
            session.expect("[stand-in] status recheck")
            assert "!1" not in session.screen()
            session.send("\x04")
            session.finish()
        finally:
            session.close()
    print(
        "PASS: explicit refresh error, catalog browse/copy fallback, Space check/uncheck, "
        "Shift+arrows and Shift+j/k reorder, quit/restart persistence, embedded-slash IDs, "
        "missing-catalog ID retention/reorder/removal, save conflict/failure retention, "
        "editor resume/reload and byte preservation, "
        "invalid-edit retention, missing editor, explicit config creation, Enter-to-chat, "
        "echo reply, /model local reload/switch/cancel, transcript retention, clean chat exit, "
        "real Git counts and branch/status refresh on message submission, "
        "single-row footer, context in the input rule, and pre-submit draft wrapping/growth"
    )


if __name__ == "__main__":
    main()
