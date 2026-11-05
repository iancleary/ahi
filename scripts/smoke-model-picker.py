#!/usr/bin/env python3
"""Exercise the real terminal picker with disposable local state (POSIX only).

Run after cargo build: uv run scripts/smoke-model-picker.py
Only loopback HTTP and disposable credential fixtures are used.
No external requests, real credentials, clipboard, or editor configuration is used.
"""
import argparse
import errno
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlencode, urlsplit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ahi"))
    parser.add_argument("--capture-directory", type=Path)
    args = parser.parse_args()
    if args.capture_directory:
        args.capture_directory.mkdir(parents=True, exist_ok=True)
    if os.name != "posix":
        parser.error("this PTY smoke check requires POSIX")
    import fcntl
    import pty
    import termios

    binary = args.binary.resolve(strict=True)

    class Session:
        def __init__(self, state: Path, editor: Path | None = None,
                     cwd: Path | None = None, executable_dir: Path | None = None,
                     columns: int = 160, rows: int = 30,
                     environment: dict[str, str] | None = None) -> None:
            self.columns = columns
            self.rows = rows
            self.pid, self.fd = pty.fork()
            if self.pid == 0:
                env = os.environ.copy()
                # Hide installed clipboard programs, and do not inherit an editor.
                env.update(TERM="xterm-256color", PATH=str(executable_dir) if executable_dir else "/nonexistent")
                env.update(environment or {})
                if cwd:
                    os.chdir(cwd)
                env.pop("VISUAL", None)
                env.pop("EDITOR", None)
                if editor:
                    env["VISUAL"] = f"/bin/sh '{editor}'"
                os.execve(binary, [str(binary), "--state-dir", str(state), "tui"], env)
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
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
            # Ratatui emits cell differences; replay cursor movement and erasure
            # instead of treating escape-stripped output as a complete screen.
            cells = [[" "] * self.columns for _ in range(self.rows)]
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
                        cells = [[" "] * self.columns for _ in range(self.rows)]
                    elif part.endswith("K") and 0 <= row < self.rows:
                        mode = part[2:-1] or "0"
                        start = col if mode == "0" else 0
                        end = col + 1 if mode == "1" else self.columns
                        for position in range(max(0, start), min(self.columns, end)):
                            cells[row][position] = " "
                    elif part[-1:] in ("A", "B", "C", "D", "G", "d"):
                        amount = int(part[2:-1] or "1")
                        if part.endswith("A"):
                            row = max(0, row - amount)
                        elif part.endswith("B"):
                            row = min(self.rows - 1, row + amount)
                        elif part.endswith("C"):
                            col = min(self.columns - 1, col + amount)
                        elif part.endswith("D"):
                            col = max(0, col - amount)
                        elif part.endswith("G"):
                            col = amount - 1
                        else:
                            row = amount - 1
                    continue
                for char in part:
                    if char == "\r":
                        col = 0
                    elif char == "\n":
                        row += 1
                    elif char >= " ":
                        if 0 <= row < self.rows and 0 <= col < self.columns:
                            cells[row][col] = char
                        col += 1
            return "\n".join("".join(line) for line in cells)

        def expect(self, text: str) -> None:
            end = time.monotonic() + 10
            while text not in self.screen() and time.monotonic() < end:
                self.read()
            assert text in self.screen(), (text, self.screen())

        def resize(self, columns: int, rows: int) -> None:
            self.columns = columns
            self.rows = rows
            self.output = b""
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))

        def capture(self, name: str) -> None:
            if args.capture_directory:
                deadline = time.monotonic() + 0.25
                while time.monotonic() < deadline:
                    self.read()
                (args.capture_directory / f"{name}.txt").write_text(self.screen() + "\n")
                (args.capture_directory / f"{name}.ansi").write_bytes(self.output)

        def absent(self, text: str) -> None:
            deadline = time.monotonic() + 10
            while text in self.screen() and time.monotonic() < deadline:
                self.read()
            assert text not in self.screen(), (text, self.screen())

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
            # Closing the PTY first avoids macOS terminal-teardown waits on failure.
            os.close(self.fd)
            if not self.dead:
                os.kill(self.pid, 9)
                os.waitpid(self.pid, 0)

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
            session.expect("Models · Enter use model")
            session.send("r")
            session.expect("OpenAI credential unavailable")
            session.send("c")
            session.expect("Catalog · openai")
            session.send("jjy")
            session.expect("No clipboard available")
            session.send("q")
            output = session.finish()
            assert "catalog-only" in output
            assert not (state / "selection").exists()
            assert (state / "models.json").read_text() == body
        finally:
            session.close()

        session = Session(state, columns=80, rows=24)
        try:
            session.expect("Models · Enter use model")
            session.send("\x1b[200~qjc /quit\r\n\x1b[201~")
            session.send("/")
            session.expect("Commands ·")
            session.send("\x1b[200~" + "x" * 129 + "\x1b[201~")
            session.expect("/gateway")
            session.send("\x1b[200~catalog\x1b[201~")
            session.expect("/catalog")
            session.capture("models-command-paste-80x24")
            session.send("\r")
            session.expect("Catalog · openai · 2 checked")
            session.absent("3 Choose models")
            session.send("/\x1b[200~quit\r\n\x1b[201~")
            session.expect("/gateway")
            session.send("\x1b")
            session.absent("Commands ·")
            session.send("/\x1b[200~models\x1b[201~")
            session.expect("/models")
            session.capture("catalog-command-paste-80x24")
            session.send("\x1b")
            session.absent("Commands ·")
            session.send("j")
            session.read()
            session.send("\x1b")
            session.expect("Models · Enter use model")
            session.capture("models-catalog-return-second-80x24")
            session.send("\r")
            session.expect("(openai) first")
            assert (state / "selection").read_text() == "openai/first\n"
            assert (state / "models.json").read_text() == body
            session.send("/quit\r")
            session.finish()
            assert b"\x1b[?2004h" in session.output
            assert b"\x1b[?2004l" in session.output
        finally:
            session.close()
            (state / "selection").unlink(missing_ok=True)

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
            session.expect("Models · Enter use model")
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

        # Disposable adapters capture copy payloads without touching the real clipboard.
        clipboard_bin = state / "clipboard-bin"
        clipboard_bin.mkdir()
        copied_text = state / "copied-selection.txt"
        for name in ("pbcopy", "wl-copy", "xclip", "xsel"):
            helper = clipboard_bin / name
            helper.write_text("#!/bin/sh\n/bin/cat > " + shlex.quote(str(copied_text)) + "\n")
            helper.chmod(0o700)
        session = Session(state, executable_dir=clipboard_bin)
        try:
            session.expect("vendor/model/variant")
            session.send("j\r")
            session.expect("(openai) vendor/model/variant • medium")
            mouse_enable = rb"\x1b\[\?(?:1000|1002|1003|1006|1015)h"
            assert re.search(mouse_enable, session.output), "chat must enable mouse controls by default"
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
            session.expect("OpenAI credential unavailable: run ahi login")
            assert "128k" not in session.screen()  # Never invent model context/usage.
            rows = session.screen().splitlines()
            assert rows[-1].endswith("(openai) vendor/model/variant • medium")
            assert rows[-2].startswith("/ commands ")
            assert all(character == "─" for character in rows[-2][len("/ commands "):])
            assert "↑" not in rows[-1]  # No permanent token-accounting row.

            # SGR coordinates are one-based: drag-select "merge" on row 1.
            session.send("\x1b[<0;1;1M\x1b[<32;5;1M\x1b[<0;5;1m")
            session.expect("Selection copied")
            assert copied_text.read_text() == "merge"
            copied_text.write_text("retry-marker")
            session.send("\x19")  # Ctrl+Y copies the retained selection again.
            end = time.monotonic() + 10
            while copied_text.read_text() != "merge" and time.monotonic() < end:
                session.read()
            assert copied_text.read_text() == "merge"
            session.send("\x1b")  # Escape clears the highlight, not the transcript.
            end = time.monotonic() + 10
            while "Selection copied" in session.screen() and time.monotonic() < end:
                session.read()
            assert "Selection copied" not in session.screen()

            # Mouse capture is the default. Turning it off must emit the actual
            # terminal reset sequences so native text selection can take over.
            session.send("/mouse\r")
            session.expect("mouse on: wheel and jump controls enabled")
            assert b"\x1b[?1000h" in session.output
            session.send("/mouse off\r")
            end = time.monotonic() + 10
            while session.output.rfind(b"\x1b[?1000l") < session.output.rfind(b"\x1b[?1000h") and time.monotonic() < end:
                session.read()
            assert session.output.rfind(b"\x1b[?1000l") > session.output.rfind(b"\x1b[?1000h")
            mouse_enables = len(re.findall(mouse_enable, session.output))

            session.send("/")
            session.expect("Commands · ↑/↓ select · Enter apply · Esc dismiss")
            session.expect("Choose model")
            session.send("mouse \x1b[B\x1b[A")
            session.expect("Native text selection")
            session.send("\x1b")
            end = time.monotonic() + 10
            while "Commands ·" in session.screen() and time.monotonic() < end:
                session.read()
            assert "Commands ·" not in session.screen()
            assert "/mouse " in session.screen()
            session.send("\x03")  # Clear the preserved draft, not exit chat.
            session.send("/\x1b[B\x1b[B\r")
            session.expect("Catalog · openai")
            session.expect("catalog-only")
            session.send("q")
            session.expect("(openai) vendor/model/variant • medium")

            # /models reloads local curation instead of capturing the startup
            # registry. Reference-only IDs must not become picker defaults.
            original = (state / "models.json").read_bytes()
            updated = json.loads(original)
            updated["providers"]["openai"]["models"] = [
                {"id": "vendor/model/variant"}, {"id": "fresh/from-disk"}
            ]
            (state / "models.json").write_text(json.dumps(updated))
            session.send("/models\r")
            session.expect("Models · Enter use model")
            session.expect("fresh/from-disk")
            assert "catalog-only" not in session.screen()
            session.send("j\r")
            session.expect("(openai) fresh/from-disk • medium")
            session.expect("merge smoke")  # Transcript survived switching.
            assert len(re.findall(mouse_enable, session.output)) == mouse_enables
            assert (state / "selection").read_text() == "openai/fresh/from-disk\n"
            session.send("/model\r")
            session.expect("Models · Enter use model")
            session.send("q")
            session.expect("(openai) fresh/from-disk • medium")
            assert (state / "selection").read_text() == "openai/fresh/from-disk\n"
            session.send("/models\r")
            session.expect("Models · Enter use model")
            session.send("/quit\r")
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
            session.expect("Models · Enter use model")
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
            session.expect("No models selected · c choose models")
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
            session.expect("No models selected · c choose models")
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
        session = Session(state, cwd=repo, executable_dir=git_bin_dir)
        try:
            session.expect("Models · Enter use model")
            session.send("j\r")
            session.expect(" main +1 !1")
            git("symbolic-ref", "HEAD", "refs/heads/tool-branch")
            git("add", "tracked")
            session.send("status recheck\r")
            session.expect(" tool-branch +1")
            session.expect("OpenAI credential unavailable: run ahi login")
            assert "!1" not in session.screen()
            session.send("\x04")
            session.finish()
        finally:
            session.close()
        login_state = state / "login-state"
        login_state.mkdir()
        (login_state / "models.json").write_text(body)
        browser_directory = state / "browser-bin"
        browser_directory.mkdir()
        authorization_file = state / "authorization-url"
        for executable in ("open", "xdg-open"):
            helper = browser_directory / executable
            helper.write_text(
                "#!/bin/sh\nprintf '%s' \"$1\" > "
                + shlex.quote(str(authorization_file)) + "\n"
            )
            helper.chmod(0o700)

        def authorization_parameters(session: Session) -> dict[str, list[str]]:
            session.expect("OpenAI login")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                session.read()
                if authorization_file.exists() and authorization_file.read_text():
                    break
            url = urlsplit(authorization_file.read_text())
            assert (url.scheme, url.netloc, url.path) == (
                "https", "auth.openai.com", "/api/accounts/authorize"
            )
            parameters = parse_qs(url.query)
            assert parameters["client_id"] == ["dynamic_agent_client"]
            assert parameters["resource"] == ["https://api.openai.com/v1"]
            assert parameters["redirect_uri"] == ["http://127.0.0.1:1455/auth/callback"]
            assert "chatgpt.tokens.use.direct" in parameters["scope"][0].split()
            assert parameters["code_challenge_method"] == ["S256"]
            assert parameters["nonce"][0]
            authorization_file.unlink()
            return parameters

        credential_path = login_state / "credentials/openai.json"
        session = Session(login_state, executable_dir=browser_directory)
        try:
            session.expect("Models · Enter use model")
            with socket.socket() as occupied_port:
                occupied_port.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                occupied_port.bind(("127.0.0.1", 1455))
                occupied_port.listen()
                session.send("/login\r")
                session.expect("OpenAI callback port 1455 unavailable")
                assert not authorization_file.exists()
            session.send("/login\r")
            first_parameters = authorization_parameters(session)
            session.send("\x1b[200~bare-code\x1b[201~\r")
            session.expect("paste the full OpenAI callback URL, not a bare code")
            mismatched_callback = (
                "http://127.0.0.1:1455/auth/callback?"
                + urlencode({"code": "fixture-code", "state": "mismatch", "client_id": "fixture-client"})
            )
            session.send("\x1b[200~" + mismatched_callback + "\x1b[201~\r")
            session.expect("OpenAI callback state missing or mismatched")
            assert "fixture-code" not in session.screen()
            session.send("\x1b")
            session.expect("OpenAI login cancelled")
            assert not credential_path.exists()
            assert not (login_state / "selection").exists()

            credential_path.parent.mkdir()
            credential_path.write_text('{"flow":"legacy-fixture","access_token":"preserved-fixture"}')
            original_credential = credential_path.read_bytes()
            session.send("j\r")
            session.expect("(openai) second • medium")
            session.send("retained login transcript\r")
            session.expect("run ahi login to migrate to the current OpenAI flow")
            session.send("/log")
            session.expect("OpenAI sign-in")
            session.send("\r")
            second_parameters = authorization_parameters(session)
            assert first_parameters["state"] != second_parameters["state"]
            assert first_parameters["ext_agent_host_id"] == second_parameters["ext_agent_host_id"]
            session.send("\x03")
            session.expect("OpenAI login cancelled")
            session.expect("retained login transcript")
            assert credential_path.read_bytes() == original_credential
            assert (login_state / "selection").read_text() == "openai/second\n"

            session.send("/catalog\r")
            session.expect("Catalog · openai")
            session.send("/login\r")
            authorization_parameters(session)
            session.send("\x1b")
            session.expect("OpenAI login cancelled")
            session.expect("Catalog · openai")
            session.send("q")
            session.expect("(openai) second • medium")
            session.send("/login\r")
            final_parameters = authorization_parameters(session)
            connection = http.client.HTTPConnection("127.0.0.1", 1455, timeout=5)
            try:
                connection.request("GET", "/auth/callback?" + urlencode({
                    "state": final_parameters["state"][0], "error": "access_denied"
                }))
                response = connection.getresponse()
                assert response.status == 400
                response.read()
            finally:
                connection.close()
            session.expect("OpenAI authorization denied in browser")
            session.expect("retained login transcript")
            assert credential_path.read_bytes() == original_credential
            assert (login_state / "models.json").read_text() == body
            assert not (login_state / "catalogs").exists()
            session.send("/quit\r")
            session.finish()
        finally:
            session.close()
        environment = os.environ.copy()
        environment["PATH"] = "/nonexistent"
        result = subprocess.run(
            [str(binary), "--state-dir", str(login_state), "login"],
            input="bare-code\n", text=True, capture_output=True,
            env=environment, timeout=10,
        )
        assert result.returncode == 1
        assert "https://auth.openai.com/api/accounts/authorize?" in result.stderr
        assert "paste the full OpenAI callback URL, not a bare code" in result.stderr
        assert credential_path.read_bytes() == original_credential
        with socket.socket() as connection:
            connection.settimeout(1)
            assert connection.connect_ex(("127.0.0.1", 1455)) != 0
        gateway_state = state / "gateway-state"
        gateway_state.mkdir()
        gateway_catalog = {
            "id": "discovered", "api": "openai-completions", "apiKey": "gateway",
            "models": [{"id": "served/model", "input": ["text"],
                        "cost": {"input": 0, "output": 0}}],
        }
        gateway_response = {"status": 200, "catalog": gateway_catalog, "defer": False}
        gateway_requests = []
        release_discovery = threading.Event()
        discovery_received = threading.Event()
        discovery_finished = threading.Event()
        response_received = threading.Event()
        release_response = threading.Event()

        class GatewayHandler(BaseHTTPRequestHandler):
            def log_message(self, *_arguments: object) -> None:
                pass

            def do_GET(self) -> None:
                gateway_requests.append((self.path, dict(self.headers), None))
                discovery_received.set()
                if gateway_response["defer"]:
                    release_discovery.wait(timeout=10)
                payload = json.dumps(gateway_response["catalog"]).encode()
                self.send_response(gateway_response["status"] if self.path == "/agent/models.json" else 404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Location", "/unexpected-redirect")
                self.end_headers()
                self.wfile.write(payload)
                discovery_finished.set()

            def do_POST(self) -> None:
                payload = self.rfile.read(int(self.headers["Content-Length"]))
                request = json.loads(payload)
                gateway_requests.append((self.path, dict(self.headers), request))
                if request["messages"][-1]["content"] == "verify retained context":
                    response_received.set()
                    release_response.wait(timeout=10)
                queued = gateway_response.get("streams")
                response = (queued.pop(0) if queued else gateway_response.get("stream", (
                    'data: {"choices":[{"index":0,"delta":{"content":"gateway workflow confirmed"},"finish_reason":"stop"}]}\n\n'
                    'data: [DONE]\n\n'
                ))).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

        server = ThreadingHTTPServer(("127.0.0.1", 0), GatewayHandler)
        server_thread = threading.Thread(target=server.serve_forever)
        base_url = f"http://127.0.0.1:{server.server_port}/v1"

        def enter_gateway_url(session: Session, url: str) -> None:
            session.send("\x15\x1b[200~" + url + "\x1b[201~\r")

        def gateway_preview(session: Session, url: str = base_url) -> None:
            enter_gateway_url(session, url)
            session.expect("Review the gateway before registering")
            session.expect("Gateway: discovered")
            session.expect("served/model")
            session.expect("Enter register → Catalog")
            session.expect("Esc/Ctrl+C cancel without saving")
            session.capture("gateway-review-80x24")

        def inspect_commands(session: Session, capture_name: str) -> None:
            session.send("/")
            session.expect("Commands · ↑/↓ select · Enter apply · Esc dismiss")
            for label in ("Add gateway", "Choose model", "Browse catalog", "OpenAI sign-in", "Quit ahi"):
                session.expect(label)
            session.capture(capture_name)
            session.send("\x1b[B\x1b[A")
            session.send("gate")
            session.expect("Add gateway")
            session.absent("/models")
            session.send("\x1b")
            session.absent("Commands ·")

        session = Session(gateway_state, cwd=gateway_state, columns=80, rows=24, environment={
            "AHI_GATEWAY_FIXTURE_KEY": "preserved-key-marker",
            "AHI_GATEWAY_FIXTURE_HEADER": "preserved-header-marker",
        })
        server_thread.start()
        try:
            session.expect("Add your first gateway")
            session.expect("/ commands")
            session.expect("No models selected")
            session.absent("no models in models.json")
            session.absent("Enter saves the highlighted model")
            session.capture("models-empty-80x24")
            inspect_commands(session, "models-commands-80x24")
            session.send("/unregistered\r")
            session.expect("Unknown command")
            session.send("/\x1b[B\x1b[B\r")
            session.expect("Catalog · openai")
            inspect_commands(session, "catalog-commands-80x24")
            session.send("/\x1b[B\r")
            session.expect("Models · Enter use model")
            session.send("/gate\r")
            session.expect("Gateway URL")
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            assert not (gateway_state / "models.json").exists()
            session.send("/gateway\r")
            session.expect("Gateway URL")
            for url, diagnostic in [
                ("http://gateway.invalid/v1", "http outside loopback"),
                (base_url + "?credential=fixture", "must not contain a query or fragment"),
                (base_url.replace("127.0.0.1", "fixture:secret@127.0.0.1"), "must not contain userinfo"),
            ]:
                enter_gateway_url(session, url)
                session.expect(diagnostic)
                assert not gateway_requests
            session.send("\x15\x1b[200~" + base_url + "x\x1b[201~\x7f\r")
            session.expect("Review the gateway before registering")
            assert len(gateway_requests) == 1
            assert gateway_requests[0][0] == "/agent/models.json"
            assert not {"authorization", "cookie"} & {name.lower() for name in gateway_requests[0][1]}
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            assert not (gateway_state / "models.json").exists()
            assert not (gateway_state / "catalogs").exists()
            session.send("/gateway\r")
            session.expect("Gateway URL")
            gateway_preview(session)
            session.capture("gateway-new-review-80x24")
            session.resize(60, 24)
            session.expect("Enter register → Catalog")
            session.expect("Esc/Ctrl+C cancel without saving")
            session.capture("gateway-review-60x24")
            session.resize(80, 24)
            session.expect("Gateway: discovered")
            session.send("\r")
            session.expect("Catalog · discovered")
            session.expect("[ ] served/model")
            session.expect("Gateway discovered registered")
            session.expect("Space add/remove")
            session.expect("Enter use checked model")
            session.expect("3 Choose models")
            session.capture("catalog-next-steps-80x24")
            session.send("\x1b")
            session.expect("Press c to choose models")
            session.absent("Add your first gateway")
            session.capture("models-registered-empty-80x24")
            session.send("c")
            session.expect("Catalog · discovered")
            session.absent("3 Choose models")
            session.absent("Gateway discovered registered")
            session.send("\r")
            session.expect("Press Space to add this model")
            assert not (gateway_state / "selection").exists()
            session.send("/gateway\r")
            session.expect("Gateway URL")
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            session.expect("Catalog · discovered")
            registered = json.loads((gateway_state / "models.json").read_text())
            assert registered == {"providers": {"discovered": {
                "baseUrl": base_url, "api": "openai-completions", "apiKey": "gateway", "models": []
            }}}
            snapshot = json.loads((gateway_state / "catalogs/discovered.json").read_text())
            assert snapshot["models"] == ["served/model"]
            assert snapshot["source"] == base_url.removesuffix("/v1") + "/agent/models.json"
            assert not (gateway_state / "selection").exists()
            session.send("j ")
            session.expect("[x] served/model")
            session.expect("1 checked")
            session.send("\r")
            session.expect("(discovered) served/model • medium")
            session.send("/unknown-command\r")
            session.expect("unknown command /unknown-command")
            assert not any(path.endswith("/chat/completions") for path, _, _ in gateway_requests)
            session.send("verify discovered gateway\r")
            session.expect("gateway workflow confirmed")
            path, headers, request = gateway_requests[-1]
            assert path == "/v1/chat/completions"
            assert {name.lower(): value for name, value in headers.items()}["authorization"] == "Bearer gateway"
            assert request["model"] == "served/model"
            assert request["messages"] == [{"role": "user", "content": "verify discovered gateway"}]
            configured = json.loads((gateway_state / "models.json").read_text())
            configured["providers"]["discovered"]["apiKey"] = "$AHI_GATEWAY_FIXTURE_KEY"
            configured["providers"]["discovered"]["headers"] = {"X-Fixture": "$AHI_GATEWAY_FIXTURE_HEADER"}
            (gateway_state / "models.json").write_text(json.dumps(configured, indent=2) + "\n")
            original_models = (gateway_state / "models.json").read_bytes()
            original_catalog = (gateway_state / "catalogs/discovered.json").read_bytes()
            session.send("/gate")
            session.expect("Add gateway")
            session.send("\r")
            session.expect("Gateway URL")
            for catalog, status, diagnostic in [
                ({**gateway_catalog, "apiKey": "literal-secret"}, 200, "catalog apiKey must be gateway"),
                ({**gateway_catalog, "id": "openai"}, 200, "openai is reserved for browser login"),
                ({**gateway_catalog, "id": "../invalid"}, 200, "invalid catalog provider identifier"),
                ({**gateway_catalog, "provider": "conflicting"}, 200, "catalog id does not match provider"),
                ({**gateway_catalog, "models": [{"id": "served/model", "cost": {"input": -1, "output": 0}}]}, 200, "invalid gateway catalog schema or model metadata"),
                (gateway_catalog, 302, "redirects are not followed"),
            ]:
                gateway_response.update(catalog=catalog, status=status)
                enter_gateway_url(session, base_url)
                session.expect(diagnostic)
                assert (gateway_state / "models.json").read_bytes() == original_models
                assert (gateway_state / "catalogs/discovered.json").read_bytes() == original_catalog
            assert all(path != "/unexpected-redirect" for path, _, _ in gateway_requests)
            gateway_response.update(catalog=gateway_catalog, status=200)
            gateway_preview(session, base_url.removesuffix("/v1") + "/different")
            session.send("\r")
            session.expect("already registered at a different URL")
            assert (gateway_state / "models.json").read_bytes() == original_models
            session.send("e")
            gateway_preview(session)
            session.expect("Rediscover existing gateway; settings retained")
            session.expect("Authentication: $AHI_GATEWAY_FIXTURE_KEY")
            session.send("\r")
            session.expect("Catalog · discovered")
            session.expect("[x] served/model")
            session.expect("Gateway discovered registered")
            assert (gateway_state / "models.json").read_bytes() == original_models
            session.send("\x1b")
            session.expect("Models · Enter use model")
            session.send("\r")
            session.expect("gateway workflow confirmed")
            session.expect("(discovered) served/model • medium")
            inspect_commands(session, "chat-commands-80x24")
            session.send("\x03verify retained context\r")
            assert response_received.wait(timeout=5)
            session.send("/gateway\r")
            session.expect("/gateway")
            session.expect("Working")
            assert "Gateway URL" not in session.screen()
            release_response.set()
            session.absent("Working")
            assert "/gateway" in session.screen()
            assert gateway_requests[-1][2]["messages"] == [
                {"role": "user", "content": "verify discovered gateway"},
                {"role": "assistant", "content": "gateway workflow confirmed"},
                {"role": "user", "content": "verify retained context"},
            ]
            retained_headers = {name.lower(): value for name, value in gateway_requests[-1][1].items()}
            assert retained_headers["authorization"] == "Bearer preserved-key-marker"
            assert retained_headers["x-fixture"] == "preserved-header-marker"
            session.send("\r")
            session.expect("Gateway URL")
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            assert session.output.rfind(b"\x1b[?1000h") > session.output.rfind(b"\x1b[?1000l")
            session.send("/gateway\r")
            session.expect("Gateway URL")
            gateway_preview(session)
            (gateway_state / "models.json").write_bytes(original_models + b"\n")
            session.send("\r")
            session.expect("models.json changed outside this view")
            assert (gateway_state / "models.json").read_bytes() == original_models + b"\n"
            session.send("\x03")
            session.expect("Gateway discovery cancelled")
            gateway_response["defer"] = True
            discovery_received.clear()
            discovery_finished.clear()
            session.send("/gateway\r")
            session.expect("Gateway URL")
            enter_gateway_url(session, base_url)
            session.expect("Discovering gateway…")
            assert discovery_received.wait(timeout=5)
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            release_discovery.set()
            assert discovery_finished.wait(timeout=5)
            assert (gateway_state / "models.json").read_bytes() == original_models + b"\n"
            gateway_response.update(defer=False, catalog={**gateway_catalog, "id": "empty", "models": []})
            session.send("/gateway\r")
            session.expect("Gateway URL")
            enter_gateway_url(session, base_url)
            session.expect("Gateway: empty")
            session.send("\r")
            session.expect("Catalog · empty")
            session.expect("catalog contains no models")
            session.send("\r")
            session.expect("No models available; r refresh or /gateway retry discovery")
            assert json.loads((gateway_state / "models.json").read_text())["providers"]["empty"]["models"] == []
            assert (gateway_state / "selection").read_text() == "discovered/served/model\n"
            session.capture("catalog-empty-80x24")
            session.send("q")
            session.expect("gateway workflow confirmed")
            gateway_response.update(catalog={**gateway_catalog, "id": "uncached"})
            catalog_conflict = gateway_state / "catalogs/uncached.json.tmp"
            catalog_conflict.write_text("foreign catalog writer")
            session.send("/gateway\r")
            session.expect("Gateway URL")
            enter_gateway_url(session, base_url)
            session.expect("Gateway: uncached")
            session.expect("Review the gateway before registering")
            session.send("\r")
            session.expect("Catalog · uncached")
            session.expect("registered, but catalog save failed")
            session.expect("r refresh or /gateway retry")
            session.capture("catalog-cache-warning-80x24")
            saved = json.loads((gateway_state / "models.json").read_text())
            assert saved["providers"]["uncached"]["models"] == []
            assert saved["providers"]["discovered"] == json.loads(original_models)["providers"]["discovered"]
            assert catalog_conflict.read_text() == "foreign catalog writer"
            assert not (gateway_state / "catalogs/uncached.json").exists()
            session.send("\x1b")
            session.expect("Models · Enter use model")
            session.expect("Catalog unavailable: r discovers models")
            session.absent("Add your first gateway")
            session.capture("models-registered-uncached-80x24")
            session.send("/catalog\r")
            session.expect("Catalog · uncached")
            session.expect("catalog unavailable: press r to refresh")
            session.absent("3 Choose models")
            session.absent("registered, but catalog save failed")
            session.capture("catalog-dismissed-registration-80x24")
            catalog_conflict.unlink()
            session.send("r")
            session.expect("[ ] served/model")
            session.expect("Catalog · uncached")
            assert (gateway_state / "catalogs/uncached.json").exists()
            assert not (gateway_state / "selection").read_text().startswith("uncached/")
            session.capture("catalog-cache-recovered-80x24")
            session.send("/quit\r")
            session.finish()
        finally:
            release_discovery.set()
            release_response.set()
            session.close()
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=5)

        metadata_state = state / "model-metadata"
        metadata_state.mkdir()
        server = ThreadingHTTPServer(("127.0.0.1", 0), GatewayHandler)
        origin = f"http://127.0.0.1:{server.server_port}"
        inference_url = origin + "/v1"
        compatibility = {
            "supportsDeveloperRole": False, "supportsReasoningEffort": False,
            "supportsStore": False, "supportsUsageInStreaming": True,
            "supportsFinishReason": True, "maxTokensField": "max_tokens",
            "requiresToolResultName": False, "requiresAssistantAfterToolResult": False,
            "requiresThinkingAsText": False,
            "requiresReasoningContentOnAssistantMessages": False,
        }
        model_entries = [
            {"id": "GLM-5.3-Flash-EXL3",
             "name": "GLM-5.3 Flash (Mia EXL3, two Sparks, 1M context, vision)",
             "contextWindow": 1048576, "maxTokens": 32768},
            {"id": "nemotron-3.5-bolt",
             "name": "Nemotron 3.5 Bolt 74B-A9B (RTX 5090 vLLM, 262K context, vision)",
             "contextWindow": 262144, "maxTokens": 262144},
        ]
        for model in model_entries:
            model.update(input=["text", "image"], reasoning=False,
                         inputLimits={"maxRequestBytes": 33554432},
                         cost={"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                         compat=compatibility.copy())
        provider_catalog = {"api": "openai-completions", "apiKey": "gateway",
                            "baseUrl": inference_url, "models": model_entries}
        supplied_catalog = {"providers": {"home": provider_catalog}}
        gateway_response.update(status=200, catalog=supplied_catalog, defer=False)
        gateway_requests.clear()
        usage_stream = (
            'data: {"choices":[{"index":0,"delta":{"content":"metadata confirmed"},"finish_reason":"stop"}]}\n\n'
            'data: {"usage":{"prompt_tokens":1000,"completion_tokens":500,"prompt_tokens_details":{"cached_tokens":200}},"choices":[]}\n\n'
            'data: [DONE]\n\n'
        )
        gateway_response["stream"] = usage_stream
        session = Session(metadata_state, cwd=metadata_state)
        server_thread = threading.Thread(target=server.serve_forever)
        server_thread.start()
        try:
            session.expect("Add your first gateway")
            session.send("/gateway\r")
            session.expect("Gateway URL")
            enter_gateway_url(session, origin)
            session.expect("Gateway: home")
            session.expect("Inference base URL: " + inference_url)
            session.expect("GLM-5.3-Flash-EXL3")
            session.capture("metadata-supplied-review")
            assert not (metadata_state / "models.json").exists()
            session.send("\r")
            session.expect("Catalog · home")
            session.expect("context 1048576")
            session.expect("output 32768")
            session.expect("33554432 bytes")
            session.capture("metadata-catalog-details")
            session.send(" \r")
            session.expect("(home) GLM-5.3-Flash-EXL3")
            session.send("verify supplied metadata\r")
            session.expect("metadata confirmed")
            session.expect("0.1% / 1M")
            path, headers, request = gateway_requests[-1]
            assert path == "/v1/chat/completions"
            assert request["max_tokens"] == 4096
            assert request["stream_options"] == {"include_usage": True}
            assert not {"store", "reasoning_effort", "tools"} & request.keys()
            cache_path = metadata_state / "catalogs/home.json"
            snapshot = json.loads(cache_path.read_text())
            assert snapshot["models"] == [model["id"] for model in model_entries]
            for model in model_entries:
                stored = snapshot["metadata"][model["id"]]
                assert stored == {field: value for field, value in model.items() if field != "id"}
            assert json.loads((metadata_state / "models.json").read_text())["providers"]["home"]["baseUrl"] == inference_url
            session.capture("metadata-context-meter")
            session.send("/quit\r")
            session.finish()
        finally:
            session.close()
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=5)

        server = ThreadingHTTPServer(("127.0.0.1", 0), GatewayHandler)
        origin = f"http://127.0.0.1:{server.server_port}"
        inference_url = origin + "/v1"
        provider_catalog["baseUrl"] = inference_url
        local_path = metadata_state / "models.json"
        local_document = json.loads(local_path.read_text())
        local_provider = local_document["providers"]["home"]
        local_provider["baseUrl"] = inference_url
        local_provider["compat"] = {
            "supportsUsageInStreaming": False, "supportsReasoningEffort": True,
            "supportsFinishReason": False, "supportsStore": True,
            "requiresReasoningContentOnAssistantMessages": True,
            "maxTokensField": "max_completion_tokens",
        }
        local_provider["models"][0].update(
            name="Curated GLM", reasoning=True, maxTokens=128,
            cost={"input": 1.5, "output": 3.25, "cacheRead": 0.25},
            compat={"supportsUsageInStreaming": True},
        )
        local_path.write_text(json.dumps(local_document))
        no_finish_stream = usage_stream.replace(',"finish_reason":"stop"', '')
        gateway_response.update(catalog=supplied_catalog, stream=no_finish_stream)
        gateway_requests.clear()
        session = Session(metadata_state, cwd=metadata_state)
        server_thread = threading.Thread(target=server.serve_forever)
        server_thread.start()
        try:
            session.expect("Models · Enter use model")
            session.send("jjj")
            session.expect("Curated GLM")
            session.expect("context 1048576")
            session.send("\r")
            session.expect("(home) GLM-5.3-Flash-EXL3")
            session.send("verify persisted overrides\r")
            session.expect("metadata confirmed")
            session.absent("Working")
            request = gateway_requests[-1][2]
            assert request["max_completion_tokens"] == 128
            assert "max_tokens" not in request
            assert request["reasoning_effort"] == "medium"
            assert request["store"] is False
            assert request["stream_options"] == {"include_usage": True}
            session.send("/")
            session.expect("usage $0.002875")
            session.capture("metadata-paid-usage")
            session.send("\x1b")
            session.absent("Commands ·")
            session.send("\x03verify assistant replay\r")
            session.absent("Working")
            deadline = time.monotonic() + 5
            while len(gateway_requests) < 2 and time.monotonic() < deadline:
                session.read()
            assert gateway_requests[-1][2]["messages"][1]["reasoning_content"] == ""

            def reload_metadata() -> None:
                local_path.write_text(json.dumps(local_document))
                session.send("/model\r")
                session.expect("Models · Enter use model")
                session.send("\r")
                session.expect("(home) GLM-5.3-Flash-EXL3")

            local_provider["models"][0]["compat"]["supportsUsageInStreaming"] = False
            (metadata_state / "thinking").write_text("off\n")
            reload_metadata()
            gateway_response["stream"] = no_finish_stream.replace('data: [DONE]\n\n', '')
            session.send("verify missing terminator\r")
            session.expect("model response ended before [DONE]")
            assert "stream_options" not in gateway_requests[-1][2]
            gateway_response["stream"] = usage_stream.replace('"stop"', '"length"')
            session.send("verify explicit truncation\r")
            session.expect("model response truncated by its output limit")
            gateway_response["stream"] = (
                'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0}]}}]}\n\n'
                'data: [DONE]\n\n'
            )
            session.send("verify malformed tool completion\r")
            session.expect("model tool call has no id")
            gateway_response["stream"] = (
                'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"local_operation","arguments":"{}"}}]}}]}\n\n'
                'data: [DONE]\n\n'
            )
            session.send("verify tool execution remains disabled\r")
            session.expect("tool execution is disabled")
            gateway_response["stream"] = "data: " + "x" * (1024 * 1024) + "\n\ndata: [DONE]\n\n"
            session.send("verify stream size limit\r")
            session.expect("model response exceeded stream size limit")
            gateway_response["stream"] = no_finish_stream
            before = len(gateway_requests)
            local_provider["models"][0]["inputLimits"] = {"maxRequestBytes": 1}
            reload_metadata()
            session.send("verify byte rejection\r")
            session.expect("model request exceeds inputLimits.maxRequestBytes")
            assert len(gateway_requests) == before
            session.capture("metadata-byte-rejection")
            local_provider["models"][0].pop("inputLimits")
            reload_metadata()
            session.send("/model\r")
            session.expect("Models · Enter use model")
            session.send("c")
            session.expect("Catalog · home")
            session.send("j ")
            session.expect("2 checked")
            session.send(" ")
            session.expect("1 checked")
            assert json.loads(local_path.read_text())["providers"]["home"]["models"] == local_provider["models"]
            session.send("k\r")
            session.expect("(home) GLM-5.3-Flash-EXL3")
            saved_local = local_path.read_bytes()
            saved_cache = cache_path.read_bytes()
            session.send("/gateway\r")
            session.expect("Gateway URL")
            for catalog, url, diagnostic in [
                ({"providers": {"home": provider_catalog, "other": {**provider_catalog, "baseUrl": origin + "/other"}}}, origin, "provider selection is ambiguous"),
                ({"providers": {"home": {**provider_catalog, "baseUrl": "https://different.invalid/v1"}}}, origin, "same origin"),
                ({"providers": {"home": {**provider_catalog, "models": [{"id": "invalid", "contextWindow": 0}]}}}, origin, "invalid gateway catalog schema or model metadata"),
                ({"providers": {"home": {**provider_catalog, "models": [{"id": "invalid", "inputLimits": {"maxRequestBytes": -1}}]}}}, origin, "invalid gateway catalog schema or model metadata"),
            ]:
                gateway_response["catalog"] = catalog
                previous_requests = len(gateway_requests)
                enter_gateway_url(session, url)
                deadline = time.monotonic() + 5
                while len(gateway_requests) == previous_requests and time.monotonic() < deadline:
                    session.read()
                assert len(gateway_requests) > previous_requests
                deadline = time.monotonic() + 0.25
                while time.monotonic() < deadline:
                    session.read()
                session.expect(diagnostic)
                assert local_path.read_bytes() == saved_local
                assert cache_path.read_bytes() == saved_cache
            gateway_response["catalog"] = {"providers": {"home": provider_catalog, "other": {**provider_catalog, "baseUrl": origin + "/other"}}}
            enter_gateway_url(session, inference_url)
            session.expect("Gateway: home")
            session.capture("metadata-exact-provider-selection")
            session.send("\x1b")
            session.expect("Gateway discovery cancelled")
            provider_catalog["baseUrl"] = origin + "/changed"
            model_entries[0]["cost"] = {"input": 1.75, "output": 4, "cacheRead": 0.5, "cacheWrite": 0.25}
            session.send("/model\r")
            session.expect("Models · Enter use model")
            session.send("r")
            session.expect("Refresh finished with 1 error(s)")
            refreshed_snapshot = json.loads(cache_path.read_text())
            assert refreshed_snapshot["provider"] == "home"
            assert refreshed_snapshot["metadata"][model_entries[0]["id"]]["cost"] == model_entries[0]["cost"]
            assert local_path.read_bytes() == saved_local
            session.send("/quit\r")
            session.finish()
        finally:
            session.close()
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=5)

        server = ThreadingHTTPServer(("127.0.0.1", 0), GatewayHandler)
        local_provider["baseUrl"] = f"http://127.0.0.1:{server.server_port}/v1"
        local_path.write_text(json.dumps(local_document))
        gateway_requests.clear()
        session = Session(metadata_state, cwd=metadata_state)
        server_thread = threading.Thread(target=server.serve_forever)
        server_thread.start()
        try:
            session.expect("Models · Enter use model")
            session.send("jjj\r")
            session.expect("• off")
            session.send("verify reasoning off\r")
            session.expect("metadata confirmed")
            session.absent("Working")
            assert "reasoning_effort" not in gateway_requests[-1][2]
            assert "stream_options" not in gateway_requests[-1][2]
            session.capture("metadata-restart-off")
            expected_request = {
                "model": "GLM-5.3-Flash-EXL3", "stream": True,
                "messages": [{"role": "user", "content": "容量"}],
                "max_completion_tokens": 128, "store": False,
            }
            exact_bytes = len(json.dumps(expected_request, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            session.send("/clear\r")
            local_provider["models"][0]["inputLimits"] = {"maxRequestBytes": exact_bytes - 1}
            local_path.write_text(json.dumps(local_document))
            before = len(gateway_requests)
            session.send("容量\r")
            session.expect("model request exceeds inputLimits.maxRequestBytes")
            assert len(gateway_requests) == before
            session.send("/clear\r")
            local_provider["models"][0]["inputLimits"]["maxRequestBytes"] = exact_bytes
            local_path.write_text(json.dumps(local_document))
            session.send("容量\r")
            session.expect("metadata confirmed")
            session.absent("Working")
            assert gateway_requests[-1][2] == expected_request
            session.send("/")
            session.expect("usage $0.002875")
            session.capture("metadata-exact-utf8-limit")
            session.send("\x1b")
            session.absent("Commands ·")
            session.send("\x03/clear\r")
            gateway_response["stream"] = no_finish_stream.replace(',"prompt_tokens_details":{"cached_tokens":200}', '')
            session.send("容量\r")
            session.expect("metadata confirmed")
            session.absent("Working")
            session.send("/")
            session.expect("Commands ·")
            session.absent("usage $")
            session.capture("metadata-unknown-cached-usage")
            session.send("\x1b")
            session.absent("Commands ·")
            session.send("\x03/quit\r")
            session.finish()
        finally:
            session.close()
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=5)

        # Tool calls: enabled only by ahi.actions.yaml in the working directory
        # (never the state directory), approved or denied by the operator.
        tool_workspace = state / "tool-workspace"
        (tool_workspace / "src").mkdir(parents=True)
        (tool_workspace / "src" / "lib.rs").write_text("pub fn tool_fixture() {}\n")
        (tool_workspace / "ahi.actions.yaml").write_text(
            "version: 1\ndefault: deny\nrules:\n"
            "  - id: read-sources\n    action: tool_call\n"
            "    match: {tool: read_file, path: src}\n"
            "    effect: approve\n    ask: true\n    sandbox: default\n"
        )

        def tool_call_stream(call_id: str, path: str) -> str:
            call = {"index": 0, "id": call_id, "type": "function",
                    "function": {"name": "read_file", "arguments": json.dumps({"path": path})}}
            chunk = {"choices": [{"index": 0, "delta": {"tool_calls": [call]}, "finish_reason": "tool_calls"}]}
            return "data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n"

        def text_stream(text: str) -> str:
            chunk = {"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]}
            return "data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n"

        server = ThreadingHTTPServer(("127.0.0.1", 0), GatewayHandler)
        local_provider["baseUrl"] = f"http://127.0.0.1:{server.server_port}/v1"
        local_provider["models"][0].pop("inputLimits", None)
        local_path.write_text(json.dumps(local_document))
        gateway_requests.clear()
        gateway_response["streams"] = [
            tool_call_stream("call_1", "src/lib.rs"), text_stream("tool result received"),
            tool_call_stream("call_2", "src/lib.rs"), text_stream("tool denial received"),
        ]
        session = Session(metadata_state, cwd=tool_workspace)
        server_thread = threading.Thread(target=server.serve_forever)
        server_thread.start()
        try:
            session.expect("Models · Enter use model")
            session.send("jjj\r")
            session.expect("(home) GLM-5.3-Flash-EXL3")
            session.send("verify tool approval\r")
            session.expect("Tool approval · 1 call")
            session.expect("› 1. read_file src/lib.rs")
            session.expect("y once · s session under src/")
            session.capture("tool-approval-prompt")
            assert [tool["function"]["name"] for tool in gateway_requests[-1][2]["tools"]] == ["read_file"]
            assert len(gateway_requests) == 1
            session.send("y")
            session.expect("approved once")
            session.expect("pub fn tool_fixture() {}")
            session.expect("tool result received")
            session.absent("Working")
            followup = gateway_requests[-1][2]["messages"]
            assert followup[-2]["tool_calls"][0]["id"] == "call_1", followup
            assert followup[-1] == {"role": "tool", "tool_call_id": "call_1",
                                    "content": "pub fn tool_fixture() {}\n"}, followup
            session.capture("tool-approved-result")
            session.send("verify tool denial\r")
            session.expect("› 1. read_file src/lib.rs")
            session.send("n")
            session.expect("denied by the operator")
            session.expect("tool denial received")
            session.absent("Working")
            replay = gateway_requests[-1][2]["messages"]
            # The earlier exchange is replayed on the later turn.
            assert any(message.get("tool_call_id") == "call_1" for message in replay), replay
            assert replay[-1] == {"role": "tool", "tool_call_id": "call_2",
                                  "content": "denied: denied by the operator"}, replay
            assert len(gateway_requests) == 4
            session.capture("tool-denied-result")
            session.send("/quit\r")
            session.finish()
        finally:
            session.close()
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=5)
    print(
        "PASS: Pi-shaped catalog onboarding, authoritative /v1 Review and inference, metadata cache/restart/curation, "
        "provider/model overrides, capped output fields, reported paid usage without cached-input double counting, "
        "usage omission, reasoning Off and assistant replay, finish-reason fallback with truncation/tool safety, "
        "ahi.actions.yaml-gated read_file with operator approve/deny, tool resubmission and cross-turn replay, "
        "exact UTF-8 byte boundaries without rejected HTTP requests, model names/limits/context, "
        "ambiguous/exact/registered provider selection, cross-origin/invalid metadata retention, legacy caches/flat catalogs; "
        "/gateway URL entry/edit/validation, preview/cancel/register, catalog curation, real gateway streaming, "
        "shared Commands popup/filter/navigation/dismissal in Models, Catalog and Chat; "
        "80x24 and 60x24 onboarding controls, immediate cache warning/recovery, "
        "checked-model Enter, second-curated-model catalog return without curation writes, "
        "single-line bounded command paste and inert out-of-command paste, dismissed onboarding context, "
        "registered empty/uncached guidance, nested /quit, streamed context and active-response draft retention; "
        "catalog rejection, conflict/stale-save retention, rediscovery, and pending-fetch cancellation; "
        "standalone login without browser, invalid-paste retention and listener cleanup, "
        "/login from models, catalog and chat, command completion, OAuth request parameters, "
        "port-conflict recovery, callback validation, cancellation/reentry, browser denial, credential and transcript preservation, "
        "explicit refresh error, catalog browse/copy fallback, Space check/uncheck, "
        "Shift+arrows and Shift+j/k reorder, quit/restart persistence, embedded-slash IDs, "
        "missing-catalog ID retention/reorder/removal, save conflict/failure retention, "
        "editor resume/reload and byte preservation, "
        "invalid-edit retention, missing editor, explicit config creation, Enter-to-chat, "
        "OpenAI missing/legacy credential errors, /model local reload/switch/cancel, transcript retention, clean chat exit, "
        "real Git counts and branch/status refresh on message submission, "
        "default mouse controls, managed drag-selection and exact clipboard payload, native text-selection mode via /mouse off, slash commands/filter/dismiss, /catalog, /models, /quit, "
        "single-row footer, context in the input rule, and pre-submit draft wrapping/growth"
    )


if __name__ == "__main__":
    main()
