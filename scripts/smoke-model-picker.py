#!/usr/bin/env python3
"""Exercise the real terminal picker with disposable local state (POSIX only).

Run after cargo build: uv run scripts/smoke-model-picker.py
Only loopback HTTP and disposable credential fixtures are used.
No external requests, real credentials, clipboard, or editor configuration is used.
"""
import argparse
import base64
import errno
import hashlib
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
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlencode, urlsplit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ahi"))
    parser.add_argument("--capture-directory", type=Path)
    focused = parser.add_mutually_exclusive_group()
    focused.add_argument("--startup-version-only", action="store_true",
                         help="verify the picker accepts matching service protocols and refuses incompatible or legacy daemons")
    focused.add_argument("--appearance-only", action="store_true",
                         help="stop after local appearance, selection, and model-switch checks")
    focused.add_argument("--skill-picker-only", action="store_true",
                         help="stop after the scoped skill picker and bracketed paste checks")
    focused.add_argument("--workspace-modes-only", action="store_true",
                         help="verify workspace trust, mode cycling, policy bypass and revocation")
    focused.add_argument("--login-copy-only", action="store_true",
                         help="verify raw OAuth URL clipboard copy and cancellation with disposable adapters")
    focused.add_argument("--post-tool-failure-only", action="store_true",
                         help="verify a failed continuation never resends an already executed write")
    focused.add_argument("--stream-recovery-only", action="store_true",
                         help="verify live checkpoints and Ctrl+C recovery in both engines")
    focused.add_argument("--session-details-only", action="store_true",
                         help="verify rich tool details in resumed standalone and service-backed chats")
    focused.add_argument("--queue-manager-only", action="store_true",
                         help="verify queued prompt editing, ordering, navigation and compact layouts")
    focused.add_argument("--model-instructions-only", action="store_true",
                         help="verify operator/repository instructions and effective capability context")
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
                     environment: dict[str, str] | None = None,
                     command: tuple[str, ...] = ("tui",)) -> None:
            self.columns = columns
            self.rows = rows
            self.pid, self.fd = pty.fork()
            if self.pid == 0:
                env = os.environ.copy()
                instruction_home = state / "home"
                instruction_home.mkdir(parents=True, exist_ok=True)
                env["HOME"] = str(instruction_home)
                # Hide real clipboard programs and make copy routing deterministic.
                # A dummy display prevents headless OSC 52 from masking adapter failures.
                env.update(TERM="xterm-256color", PATH=str(executable_dir) if executable_dir else "/nonexistent",
                           DISPLAY=":ahi-test", WAYLAND_DISPLAY="", TERMUX_VERSION="",
                           WT_SESSION="", SSH_CONNECTION="", SSH_CLIENT="",
                           MOSH_CONNECTION="", AHI_CLIPBOARD_OSC52="")
                env.pop("OSC7577_TOKEN", None)
                env.update(environment or {})
                if cwd:
                    os.chdir(cwd)
                env.pop("VISUAL", None)
                env.pop("EDITOR", None)
                # A developer's Anthropic key would add an implicit provider.
                env.pop("ANTHROPIC_API_KEY", None)
                if editor:
                    env["VISUAL"] = f"/bin/sh '{editor}'"
                root_args = [] if '--state-dir' in command or '--state-root' in command else ['--state-dir', str(state)]
                os.execve(binary, [str(binary), *root_args, *command], env)
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
            self.output = b""
            self.recent = b""
            self.dead = False
            self.onboarded_read_only = False

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
            # OSC title, clipboard and program-status payloads are terminal
            # metadata, not cells. Ignore incomplete tails until the next read.
            visible = re.sub(r"\x1b\].*?(?:\x07|\x1b\\|\Z)", "", self.output.decode(errors="replace"), flags=re.S)
            parts = re.split(r"(\x1b\[[0-?]*[ -/]*[@-~])", visible)
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
                # Non-onboarding fixtures explicitly choose the safe temporary
                # mode. The modes-only scenario exercises the prompt itself.
                if not args.workspace_modes_only and not self.onboarded_read_only and "Trust this workspace?" in self.screen():
                    self.send("r")
                    self.onboarded_read_only = True
            assert text in self.screen(), (text, self.screen())

        def expect_borderless(self) -> None:
            assert not any(char in self.screen() for char in "┌┐└┘│─"), self.screen()

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

        def finish(self, expected_code: int = 0) -> str:
            end = time.monotonic() + 10
            while time.monotonic() < end:
                self.read()
                pid, status = os.waitpid(self.pid, os.WNOHANG)
                if pid:
                    self.dead = True
                    self.read()
                    assert os.waitstatus_to_exitcode(status) == expected_code, self.output
                    return self.output.decode(errors="replace")
            raise AssertionError("picker did not exit")

        def close(self) -> None:
            # Closing the PTY first avoids macOS terminal-teardown waits on failure.
            os.close(self.fd)
            if not self.dead:
                os.kill(self.pid, 9)
                os.waitpid(self.pid, 0)

    full_suite = not any(value for key, value in vars(args).items() if key.endswith('_only'))
    if full_suite or args.startup_version_only:
        with tempfile.TemporaryDirectory(prefix="ahi-version-", dir="/tmp") as temp, socket.socket(socket.AF_UNIX) as listener:
            state = Path(temp)
            path = state / "serve.sock"
            listener.bind(str(path))
            listener.listen()
            listener.settimeout(5)
            for health, compatible in [
                ({"version": "0.27.2"}, False),
                ({"version": "other-release", "protocol_version": 2}, False),
                ({"version": "other-release", "protocol_version": 1}, True),
            ]:
                session = Session(state, command=("tui", "--socket", str(path)))
                try:
                    # Ignore the empty liveness probe; answer the real handshake.
                    while True:
                        conn, _ = listener.accept()
                        with conn, conn.makefile("rb") as reader:
                            conn.settimeout(5)
                            line = reader.readline()
                            if line:
                                req = json.loads(line)
                                assert req["method"] == "health", req
                                conn.sendall((json.dumps({"id": req["id"], "result": health}) + "\n").encode())
                                break
                    if compatible:
                        session.expect("Models · Enter use model")
                        session.send("q")
                        session.finish()
                    else:
                        output = session.finish(expected_code=1)
                        assert f"the server on {path} is ahi {health['version']}" in output and "ahi serve stop" in output, output
                        assert b"\x1b[?1049h" not in session.output, "entered alternate screen before refusing service"
                finally:
                    session.close()
        if args.startup_version_only:
            print("PASS: picker accepts a different release with matching protocol; legacy and incompatible services refused before raw mode")
            return

    if args.workspace_modes_only:
        queue_release = threading.Event()
        redraw_release = threading.Event()
        advertised_skills = {}

        class ModeGateway(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                prompt = body["messages"][-1].get("content")
                if prompt in ("redraw-probe", "now write"):
                    advertised_skills[prompt] = next((t["function"]["description"] for t in body.get("tools", []) if t["function"]["name"] == "load_skill"), "")
                if prompt == "redraw-probe":
                    for fragment in ["Cleaned up", " the README’s introduction", " and organization:\n\n- Summarized the changes."]:
                        item = {"choices": [{"index": 0, "delta": {"content": fragment}}]}
                        self.wfile.write(f"data: {json.dumps(item)}\n\n".encode())
                        self.wfile.flush()
                        if fragment == "Cleaned up":
                            redraw_release.wait(10)
                    self.wfile.write(b'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')
                    self.wfile.flush()
                    return
                if body["messages"][-1].get("content") == "queue-turn":
                    prefix = {"choices": [{"index": 0, "delta": {"content": "mode-stream-waiting"}}]}
                    self.wfile.write(f"data: {json.dumps(prefix)}\n\n".encode())
                    self.wfile.flush()
                    queue_release.wait(10)
                if prompt == "write outside":
                    destination = str((work.parent / "external-marker").resolve())
                    args = json.dumps({"path": destination, "content": "confirmed outside write"})
                    delta = {"tool_calls": [{"index": 0, "id": "external", "type": "function", "function": {"name": "write_file", "arguments": args}}]}
                    finish = "tool_calls"
                elif prompt == "live-output":
                    argv = ["/bin/sh", "-c", "printf 'live-%s\\n' first; while [ ! -f release-output ]; do sleep 0.05; done; printf 'live-%s\\n' last"]
                    delta = {"tool_calls": [{"index": 0, "id": "live", "type": "function", "function": {"name": "run_command", "arguments": json.dumps({"argv": argv, "timeout": 12})}}]}
                    finish = "tool_calls"
                elif body["messages"][-1]["role"] == "tool":
                    last_prompt = next(m['content'] for m in reversed(body['messages']) if m['role'] == 'user')
                    delta = {"content": "<tool_call>not another call" if last_prompt == "draft-preserved" else "mode-fixture-complete"}
                    finish = "stop"
                else:
                    delta = {"tool_calls": [{"index": 0, "id": "write-mode", "type": "function",
                             "function": {"name": "write_file", "arguments": json.dumps({"path": "marker", "content": "written"})}},
                             {"index": 1, "id": "command-mode", "type": "function",
                              "function": {"name": "run_command", "arguments": json.dumps({"argv": ["/bin/sh", "-c", "printf command-ok > command-marker; printf 'captured-%s\\nsecond-%s\\n' stdout line"], "skill": "", "script": "", "args": []})}}]}
                    finish = "tool_calls"
                for item in [{"choices": [{"index": 0, "delta": delta}]},
                             {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}]:
                    self.wfile.write(f"data: {json.dumps(item)}\n\n".encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            def log_message(self, *args):
                pass

        gateway = ThreadingHTTPServer(("127.0.0.1", 0), ModeGateway)
        worker = threading.Thread(target=gateway.serve_forever, daemon=True)
        worker.start()
        try:
            for engine in ("standalone", "service"):
                with tempfile.TemporaryDirectory(prefix="ahi-modes-", dir="/tmp") as temp:
                    state = Path(temp) / "state"
                    work = Path(temp) / "work"
                    state.mkdir()
                    work.mkdir()
                    sid = "0f1e2d3c-4b5a-4978-8765-43210fedcba9"
                    (state / "sessions").mkdir()
                    (state / "sessions" / f"{sid}.jsonl").write_text(json.dumps(dict(
                        type="session", schema=1, id=sid, host="fixture", cwd=str(work),
                        ahi_version="0.22.0", gateway="local", model="fixture", thinking="off",
                        started="2026-01-01T00:00:00Z")) + "\n")
                    (state / "models.json").write_text(json.dumps({"providers": {"local": {
                        "baseUrl": f"http://127.0.0.1:{gateway.server_port}/v1", "api": "openai-completions",
                        "models": [{"id": "fixture"}],
                    }}}))
                    sock = str(state / "run/serve.sock")
                    options = ("--standalone",) if engine == "standalone" else ("--socket", sock)
                    home = Path(temp) / "home"
                    operator = home / ".agents/skills"
                    operator.mkdir(parents=True)
                    installed = Path(temp) / "dotfiles" / "local-probe"
                    installed.mkdir(parents=True)
                    (installed / "SKILL.md").write_text("---\nname: local-probe\ndescription: Probe metadata\n---\nHidden body\n")
                    (operator / "local-probe").symlink_to(installed, target_is_directory=True)
                    repo = work / ".agents/skills/repo-probe"
                    repo.mkdir(parents=True)
                    (repo / "SKILL.md").write_text("---\nname: repo-probe\ndescription: Repository fixture\n---\nHidden body\n")
                    advertised_skills.clear()
                    command = ("tui", "--session", sid, *options)
                    chat = Session(state, cwd=work, columns=120, rows=70, command=command, environment={"HOME": str(home)})
                    try:
                        chat.expect("Plan · Alt+M to review trust")
                        chat.absent("Trust this workspace?")
                        chat.send("\x1b[Z")  # Shift+Tab: effort, never authority
                        chat.expect("(local) fixture • minimal")
                        chat.expect("Plan · Alt+M to review trust")
                        redraw_release.clear()
                        chat.send("redraw-probe\r")
                        chat.expect("Thinking...")
                        chat.send("\x0f")  # Inspect, then collapse the same in-flight text.
                        chat.expect("Cleaned up")
                        chat.send("\x0f")
                        chat.absent("Cleaned up")
                        redraw_release.set()
                        chat.expect("Cleaned up the README’s introduction and organization:")
                        chat.expect("• Summarized the changes.")
                        assert chat.screen().count("Cleaned up") == 1
                        # Reuse the same connection and model. Each subsequent
                        # turn must add one reply, not another copy per subscriber.
                        for turns in (2, 3):
                            chat.send("redraw-probe\r")
                            deadline = time.monotonic() + 8
                            while chat.screen().count("• Summarized the changes.") < turns and time.monotonic() < deadline:
                                chat.read()
                            assert chat.screen().count("Cleaned up the README’s introduction and organization:") == turns, (engine, chat.screen())
                        chat.send("draft-preserved")
                        chat.send("\x1bm")
                        chat.expect("Trust this workspace?")
                        chat.send("t")
                        chat.expect("Trusted · Alt+M for Plan")
                        chat.expect("draft-preserved")
                        assert len(list((state / "workspace-trust").iterdir())) == 1
                        assert not (work / "ahi.tools.yaml").exists()
                        chat.send("\x1bm")
                        chat.expect("Plan · Alt+M for Trusted")
                        chat.send("\r")
                        chat.expect("<tool_call>not another call")
                        chat.expect("write_file marker · Plan blocks workspace writes, commands and delegation")
                        chat.absent("Tool-call markup was returned as text")  # This turn did receive structured calls.
                        assert not (work / "marker").exists()
                        assert not (work / "command-marker").exists()
                        chat.send("\x1bm")
                        chat.expect("Trusted · Alt+M for Plan")
                        chat.send("now write\r")
                        chat.expect("write_file marker")
                        deadline = time.monotonic() + 5
                        while not (work / "marker").exists():
                            chat.read()
                            assert time.monotonic() < deadline
                        assert (work / "marker").read_text() == "written"
                        chat.expect("exit 0")
                        for prompt in ("redraw-probe", "now write"):
                            catalog = advertised_skills[prompt]
                            assert "operator/local-probe" in catalog and "repo/repo-probe" in catalog, catalog
                            assert "Hidden body" not in catalog
                        chat.absent("captured-stdout")
                        chat.send("draft-toggle")
                        chat.send("\x0f")
                        chat.expect("captured-stdout")
                        chat.expect("second-line")
                        assert chat.screen().count("captured-stdout") == 1
                        chat.send("\x0f")
                        chat.absent("captured-stdout")
                        chat.expect("draft-toggle")
                        chat.send("\x03")  # clear draft before issuing /mode
                        chat.send("live-output\r")
                        chat.expect("Running command · stdout")
                        chat.expect("live-first")
                        chat.absent("live-last")
                        (work / "release-output").touch()
                        chat.absent("Running command · stdout")
                        chat.send("\x0f")
                        chat.expect("live-last")
                        assert chat.screen().count("live-first") == 1
                        chat.send("\x0f")
                        chat.send("/mode\r")
                        chat.expect("Enter select · Esc dismiss")
                        chat.send("full\r")
                        chat.expect("Full Access to this host?")
                        chat.send("\x1b")
                        chat.expect("Trusted · Alt+M for Plan")
                        chat.send("/mode\r")
                        chat.expect("Enter select · Esc dismiss")
                        chat.send("full\r")
                        chat.expect("Full Access to this host?")
                        chat.send("f")
                        chat.expect("Full Access · Alt+M for Plan")
                        chat.send("write outside\r")
                        external = work.parent / "external-marker"
                        deadline = time.monotonic() + 8
                        while not external.exists() and time.monotonic() < deadline:
                            chat.read()
                            time.sleep(0.025)
                        assert external.read_text() == "confirmed outside write"
                        chat.send("\x1bm")
                        chat.expect("Plan · Alt+M for Trusted")
                        chat.send("\x1bm")
                        chat.expect("Trusted · Alt+M for Plan")
                        # Start another Trusted turn, queue Plan while
                        # streaming, and prove the already-admitted turn still writes.
                        chat.send("/mode\r")
                        chat.expect("Enter select · Esc dismiss")
                        chat.send("\x1b")
                        chat.absent("Enter select · Esc dismiss")
                        (work / "marker").unlink()
                        queue_release.clear()
                        chat.send("queue-turn\r")
                        chat.expect("Thinking...")
                        chat.send("\x1bm")
                        chat.expect("Trusted → Plan next turn")
                        queue_release.set()
                        deadline = time.monotonic() + 5
                        while not (work / "marker").exists():
                            chat.read()
                            assert time.monotonic() < deadline
                        chat.expect("Plan · Alt+M for Trusted")
                        assert (work / "marker").read_text() == "written"
                        assert (work / "command-marker").read_text() == "command-ok"
                        chat.send("/mode revoke\r")
                        chat.expect("Workspace trust revoked")
                        assert not list((state / "workspace-trust").iterdir())
                        chat.send("/quit\r")
                        restored = chat.finish().rsplit("\x1b[?1049l", 1)[-1]
                        assert "You: now write" in restored and "Assistant: mode-fixture-complete" in restored
                        assert f"Resume: ahi --session {sid}" in restored
                    finally:
                        chat.close()
                    policy = "version: 2\ndefault_mode: review\nmodes:\n  review: {allow: {tools: [read_file]}}\n  editing: {allow: {write_files: true}}\n"
                    (work / "ahi.modes.yaml").write_text(policy)
                    chat = Session(state, cwd=work, columns=120, rows=35, command=command)
                    try:
                        chat.expect("review · Alt+M for Plan")
                        chat.absent("Trust this workspace?")
                        chat.send("\x1bm")
                        chat.expect("Plan · Alt+M to review trust")
                        (work / "marker").unlink()
                        (work / "command-marker").unlink()
                        chat.send("\x1bm")
                        chat.expect("Trust this workspace?")
                        chat.send("\x1b")
                        chat.absent("Trust this workspace?")
                        chat.expect("Plan · Alt+M to review trust")
                        chat.send("\x1bm")
                        chat.expect("Trust this workspace?")
                        chat.send("t")
                        chat.expect("Trusted · Alt+M for editing")
                        chat.absent("Bypass tool policy")
                        chat.send("permissive policy bypass\r")
                        deadline = time.monotonic() + 5
                        while not (work / "command-marker").exists():
                            chat.read()
                            assert time.monotonic() < deadline
                        assert (work / "marker").read_text() == "written"
                        assert (work / "command-marker").read_text() == "command-ok"
                        chat.send("\x1bm")
                        chat.expect("editing · Alt+M for review")
                        chat.send("/mode\r")
                        chat.expect("Enter select · Esc dismiss")
                        chat.send("review\r")
                        chat.expect("review · Alt+M for Plan")
                        chat.send("/mode\r")
                        chat.expect("Enter select · Esc dismiss")
                        chat.send("edit\r")
                        chat.expect("editing · Alt+M for review")
                        (work / "marker").unlink()
                        (work / "command-marker").unlink()
                        chat.send("named permission check\r")
                        chat.expect("no policy rule allows this call")
                        assert (work / "marker").read_text() == "written"
                        assert not (work / "command-marker").exists()
                        assert (work / "ahi.modes.yaml").read_text() == policy
                        chat.send("/quit\r")
                        chat.finish()
                    finally:
                        chat.close()
                        if engine == "service":
                            subprocess.run([binary, "--state-dir", state, "serve", "--socket", sock, "stop"],
                                           capture_output=True, timeout=10, check=True)
        finally:
            gateway.shutdown()
            gateway.server_close()
            worker.join()
        print("PASS: trust, draft-preserving mode cycle, read-only enforcement, read-write effect, direct permissive policy cycling, placeholder-filled command execution and revocation in both engines")
        return

    if args.login_copy_only:
        with tempfile.TemporaryDirectory(prefix="ahi-login-copy-", dir="/tmp") as temp:
            state = Path(temp)
            helpers = state / "bin"
            helpers.mkdir()
            model_id = "fixture/" + "long-model-name-" * 10
            gateway_url = "https://fixture.invalid/" + "long-path/" * 20 + "?value=raw&other=unchanged"
            (state / "models.json").write_text(json.dumps({"providers": {"local": {
                "baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions",
                "models": [{"id": model_id}],
            }}}))
            authorization = state / "authorization"
            copied = state / "copied"
            for name in ("open", "xdg-open"):
                helper = helpers / name
                helper.write_text("#!/bin/sh\nprintf '%s' \"$1\" > " + shlex.quote(str(authorization)) + "\n")
                helper.chmod(0o700)
            for name in ("pbcopy", "wl-copy", "xclip", "xsel"):
                helper = helpers / name
                helper.write_text("#!/bin/sh\n/bin/cat > " + shlex.quote(str(copied)) + "\n")
                helper.chmod(0o700)
            for mode in ("native", "unavailable", "osc52"):
                authorization.unlink(missing_ok=True)
                copied.unlink(missing_ok=True)
                if mode == "unavailable":
                    for name in ("pbcopy", "wl-copy", "xclip", "xsel"):
                        (helpers / name).write_text("#!/bin/sh\nexit 1\n")
                env = {"AHI_CLIPBOARD_OSC52": "1" if mode == "osc52" else "",
                       "WSL_DISTRO_NAME": "", "WSL_INTEROP": "", "WSLENV": ""}
                chat = Session(state, executable_dir=helpers, columns=80, rows=24,
                               command=("tui", "--standalone"), environment=env)
                try:
                    chat.expect("Models · Enter use model")
                    chat.expect_borderless()
                    chat.send("jjjy")
                    chat.expect("No clipboard available" if mode == "unavailable" else "Copied")
                    if mode == "native":
                        assert copied.read_text() == model_id
                    chat.send("/gateway\r")
                    chat.expect("Gateway URL · editing")
                    chat.expect_borderless()
                    chat.send("\x1b[200~" + gateway_url + "\x1b[201~")
                    chat.send("\x19")
                    chat.expect("Clipboard unavailable" if mode == "unavailable" else "URL sent to clipboard")
                    if mode == "native":
                        assert copied.read_text() == gateway_url
                    elif mode == "osc52":
                        assert b"\x1b]52;c;" + base64.b64encode(gateway_url.encode()) + b"\x07" in chat.output
                    chat.expect_borderless()
                    chat.send("\x1b")
                    chat.expect("Models · Enter use model")
                    chat.send("/login\r")
                    chat.expect("Ctrl+Y copy URL")
                    chat.expect_borderless()
                    deadline = time.monotonic() + 5
                    while not authorization.exists():
                        assert time.monotonic() < deadline
                        chat.read()
                    raw = authorization.read_text()
                    assert raw.startswith("https://auth.openai.com/")
                    chat.send("\x1b[200~callback-draft\x1b[201~")
                    chat.expect("Callback: 14 characters (hidden)")
                    chat.send("\x19")
                    if mode == "unavailable":
                        chat.expect("Clipboard unavailable")
                    else:
                        chat.expect("URL sent to clipboard")
                        if mode == "native":
                            assert copied.read_text() == raw  # no borders, line breaks, or extra newline
                        else:
                            payload = base64.b64encode(raw.encode())
                            assert b"\x1b]52;c;" + payload + b"\x07" in chat.output
                    chat.expect("Callback: 14 characters (hidden)")
                    chat.send("\x1b")
                    chat.expect("OpenAI login cancelled")
                    chat.send("/quit\r")
                    chat.finish()
                finally:
                    chat.close()
            assert not (state / "credentials/openai.json").exists()
        print("PASS: borderless model/gateway/login screens; exact raw model ID and URL copies, unavailable clipboard, OSC52 payload, callback preservation and cancel")
        return

    if full_suite or args.post_tool_failure_only:
        for engine in ('standalone', 'service'):
            requests = []

            class WriteThenDisconnect(BaseHTTPRequestHandler):
                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    requests.append(body)
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    if len(requests) != 1:
                        # Break the stream *after* the write result was submitted.
                        self.send_header('Content-Length', '500')
                        self.end_headers()
                        self.wfile.write(b': interrupted\n\n')
                        return
                    self.end_headers()
                    call = dict(index=0, id='once', type='function', function=dict(name='write_file',
                                arguments=json.dumps({'path': 'marker', 'content': 'written once'})))
                    event = {'choices': [{'index': 0, 'delta': {'tool_calls': [call]}, 'finish_reason': 'tool_calls'}]}
                    self.wfile.write(f'data: {json.dumps(event)}\n\ndata: [DONE]\n\n'.encode())

                def log_message(self, *args):
                    pass

            with tempfile.TemporaryDirectory(prefix='ahi-post-tool-', dir='/tmp') as temp, ThreadingHTTPServer(('127.0.0.1', 0), WriteThenDisconnect) as gateway:
                root = Path(temp)
                state, work = root / 'state', root / 'work'
                state.mkdir()
                work.mkdir()
                policy = root / 'policy.yaml'
                policy.write_text('version: 1\nrules:\n- {id: write, action: file_write, match: {tool: write_file}, effect: approve}\n')
                (state / 'models.json').write_text(json.dumps({'providers': {'local': {
                    'baseUrl': f'http://127.0.0.1:{gateway.server_port}/v1', 'api': 'openai-completions',
                    'models': [{'id': 'fixture'}]}}}))
                sock = str(root / 'service.sock')
                options = ('--standalone',) if engine == 'standalone' else ('--socket', sock)
                command = ('tui', '--tools', str(policy), *options)
                token = 'synthetic-fixture-token-7577'
                chat = Session(state, cwd=work, command=command, environment={'OSC7577_TOKEN': token})
                worker = threading.Thread(target=gateway.serve_forever, daemon=True)
                worker.start()
                try:
                    chat.expect('Models · Enter use model')
                    chat.send('jjj\r')
                    chat.expect('(local) fixture')
                    chat.send('write the marker\r')
                    chat.expect('check their effects')
                    assert 'Enter resends' not in chat.screen()
                    chat.send('\r/quit\r')
                    chat.finish()
                    path, = (state / 'sessions').glob('*.jsonl')
                    entries = [json.loads(line) for line in path.read_text().splitlines()]
                    assert [e['text'] for e in entries if e.get('role') == 'operator'] == ['write the marker']
                    assert [e['result']['call_id'] for e in entries if e.get('role') == 'tool'] == ['once']
                    assert (work / 'marker').read_text() == 'written once'
                    assert len(requests) == 2 and requests[1]['messages'][-1]['tool_call_id'] == 'once'
                    gateway.shutdown()
                    worker.join(5)
                    frames = re.findall(rb'\x1b\]7577;([^\x1b]+)\x1b\\', chat.output)
                    assert len(frames) == 2, frames  # register once, then clear on quit
                    records = []
                    for frame in frames:
                        body, mac = frame.rsplit(b':mac=', 1)
                        assert mac == hashlib.sha256(body + b':' + token.encode()).hexdigest().encode()
                        fields = dict(pair.split(b'=', 1) for pair in body.split(b':'))
                        assert int(fields[b'pid']) == chat.pid
                        records.append(fields)
                    assert records[0][b'need'] == b'cwd'
                    assert all(abs(time.time() - int(rec[b'time'])) < 60 for rec in records)
                    assert records[1][b'clear'] == b'1'
                    assert token not in path.read_text() and token not in json.dumps(requests)
                    assert token.encode() not in chat.output
                    argv = [base64.b64decode(arg, validate=True).decode() for arg in records[0][b'argv'].split(b',')]
                    assert argv == ['ahi', 'tui', '--config-dir', str(state), '--data-dir', str(state), '--state-root', str(state), '--cache-dir', str(state), *options, '--tools', str(policy.resolve()), '--session', path.stem], argv
                    cwd = Path(base64.b64decode(records[0][b'cwd']).decode())
                    assert cwd.samefile(work)
                    # Exercise registration immediately on reopening with a valid
                    # token, and silence with an invalid one on the other engine.
                    valid = engine == 'standalone'
                    reopened = Session(state, cwd=cwd, command=tuple(argv[1:]),
                                       environment={'OSC7577_TOKEN': token if valid else 'invalid',
                                                    'AHI_STATE_DIR': str(root / 'wrong-state'),
                                                    'AHI_HOME': str(root / 'wrong-cache'),
                                                    'XDG_CONFIG_HOME': str(root / 'wrong-config'),
                                                    'XDG_DATA_HOME': str(root / 'wrong-data'),
                                                    'XDG_STATE_HOME': str(root / 'wrong-xdg-state')})
                    worker = threading.Thread(target=gateway.serve_forever, daemon=True)
                    worker.start()
                    try:
                        reopened.expect('resumed session')
                        reopened.send('\r/quit\r')
                        reopened.finish()
                        assert reopened.output.count(b'\x1b]7577;') == (2 if valid else 0)
                        assert len(requests) == 2
                    finally:
                        reopened.close()
                finally:
                    chat.close()
                    gateway.shutdown()
                    worker.join(5)
                    if engine == 'service':
                        subprocess.run([binary, '--state-dir', state, 'serve', '--socket', sock, 'stop'], capture_output=True, timeout=10)
        print('PASS: one write, one correlated result, no blank-Enter replay before or after resume (both engines)')
        if args.post_tool_failure_only:
            return

    if args.model_instructions_only:
        for engine in ("standalone", "service"):
            with tempfile.TemporaryDirectory(prefix="ahi-instructions-", dir="/tmp") as temporary:
                root = Path(temporary)
                state = root / "state"
                work = root / "work"
                home = root / "home"
                state.mkdir()
                (work / "src").mkdir(parents=True)
                (home / ".agents").mkdir(parents=True)
                git = shutil.which("git")
                assert git, "git is required for repository instruction discovery"
                repository_environment = {**os.environ, "HOME": str(home),
                                          "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
                subprocess.run([git, "init", "--quiet", str(work)], env=repository_environment, check=True)
                operator_file = home / ".agents/AGENTS.md"
                repository_file = work / "AGENTS.md"
                operator_file.write_text("Operator instruction marker: operator-initial.\n")
                repository_file.write_text("Repository instruction marker: repository-initial.\n")
                (work / "src/evidence").write_text("instruction-evidence-marker\n")
                policy = (
                    "version: 2\ndefault_mode: review\nmodes:\n  review:\n"
                    "    allow:\n      files:\n"
                    "        - {tools: [read_file], paths: [src]}\n"
                    "      commands:\n"
                    "        - {argv_prefixes: [[/bin/echo, allowed]], working_dirs: [src]}\n"
                    "    ask:\n      commands:\n"
                    "        - {argv_prefixes: [[/bin/echo, approval]], working_dirs: [src]}\n"
                    "    deny:\n      files:\n"
                    "        - {tools: [read_file], paths: [src/private]}\n"
                    "      commands:\n"
                    "        - {argv_prefixes: [[/bin/sh]], working_dirs: ['.']}\n"
                    "  editing:\n    allow: {write_files: true}\n"
                )
                (work / "ahi.modes.yaml").write_text(policy)
                requests = []

                class InstructionGateway(BaseHTTPRequestHandler):
                    def do_POST(self):
                        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                        requests.append(request)
                        if "instructions" in request:
                            item = {"type": "message", "id": "message_fixture", "role": "assistant",
                                    "status": "completed", "content": [{"type": "output_text",
                                                                         "text": "instruction-exec-complete"}]}
                            pending = {**item, "status": "in_progress", "content": []}
                            events = [
                                {"type": "response.created", "response": {"id": "response_fixture"}},
                                {"type": "response.output_item.added", "output_index": 0, "item": pending},
                                {"type": "response.output_item.done", "output_index": 0, "item": item},
                                {"type": "response.completed", "response": {
                                    "id": "response_fixture", "status": "completed", "output": [item]}},
                            ]
                        elif "system" in request:
                            events = [
                                {"type": "message_start", "message": {"id": "message_fixture",
                                 "role": "assistant", "usage": {"input_tokens": 1}}},
                                {"type": "content_block_start", "index": 0,
                                 "content_block": {"type": "text", "text": ""}},
                                {"type": "content_block_delta", "index": 0,
                                 "delta": {"type": "text_delta", "text": "instruction-exec-complete"}},
                                {"type": "content_block_stop", "index": 0},
                                {"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                                 "usage": {"output_tokens": 1}},
                                {"type": "message_stop"},
                            ]
                        else:
                            last = request["messages"][-1]
                            if last["role"] == "user" and last["content"] == "instruction-first":
                                operator_file.write_text("Operator instruction marker: operator-updated.\n")
                                repository_file.write_text("Repository instruction marker: repository-updated.\n")
                                calls = [
                                    {"index": 0, "id": "read_evidence", "type": "function", "function": {
                                        "name": "read_file", "arguments": json.dumps({"path": "src/evidence"})}},
                                    {"index": 1, "id": "allowed_command", "type": "function", "function": {
                                        "name": "run_command", "arguments": json.dumps({
                                            "argv": ["/bin/echo", "allowed", "capability-command-evidence"],
                                            "path": "src"})}},
                                    {"index": 2, "id": "forbidden_command", "type": "function", "function": {
                                        "name": "run_command", "arguments": json.dumps({
                                            "argv": ["/bin/sh", "-c", "printf unexpected > forbidden-command"],
                                            "path": "src"})}},
                                ]
                                delta = {"tool_calls": calls}
                                finish = "tool_calls"
                            elif last["role"] == "user" and last["content"] == "instruction-protected-write":
                                call = {"index": 0, "id": "protect_instructions", "type": "function", "function": {
                                    "name": "write_file", "arguments": json.dumps({
                                        "path": "AGENTS.md", "content": "unexpected-replacement", "overwrite": True})}}
                                delta = {"tool_calls": [call]}
                                finish = "tool_calls"
                            else:
                                last_prompt = next(message["content"] for message in reversed(request["messages"])
                                                   if message["role"] == "user")
                                if last_prompt == "instruction-protected-write":
                                    answer = "instruction-protected-write-complete"
                                elif last["role"] == "tool":
                                    answer = "instruction-first-complete"
                                else:
                                    answer = ("instruction-second-complete" if last_prompt == "instruction-second"
                                              else "instruction-exec-complete")
                                delta = {"content": answer}
                                finish = "stop"
                            events = [{"choices": [{"index": 0, "delta": delta,
                                                    "finish_reason": finish}]}]
                        payload = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
                        if "choices" in events[0]:
                            payload += b"data: [DONE]\n\n"
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream")
                        self.send_header("Content-Length", str(len(payload)))
                        self.end_headers()
                        self.wfile.write(payload)

                    def log_message(self, *_arguments):
                        pass

                gateway = ThreadingHTTPServer(("127.0.0.1", 0), InstructionGateway)
                server_thread = threading.Thread(target=gateway.serve_forever, daemon=True)
                server_thread.start()
                providers = {}
                for provider, api in (("local", "openai-completions"),
                                      ("responses", "openai-responses"),
                                      ("anthropic", "anthropic-messages")):
                    providers[provider] = {"baseUrl": f"http://127.0.0.1:{gateway.server_port}/v1",
                                           "api": api, "models": [{"id": "fixture", "maxTokens": 4096}]}
                (state / "models.json").write_text(json.dumps({"providers": providers}))
                session_id = "0f1e2d3c-4b5a-4978-8765-43210fedcba9"
                store = state / "sessions"
                store.mkdir()
                transcript = store / f"{session_id}.jsonl"
                transcript.write_text(json.dumps(dict(
                    type="session", schema=1, id=session_id, host="fixture", cwd=str(work),
                    ahi_version="0.33.0", gateway="local", model="fixture", thinking="off",
                    started="2026-01-01T00:00:00Z")) + "\n")
                service_socket = str(state / "service.sock")
                options = ("--standalone",) if engine == "standalone" else ("--socket", service_socket)
                environment = os.environ.copy()
                environment["HOME"] = str(home)
                environment.pop("ANTHROPIC_API_KEY", None)
                chat = Session(state, cwd=work, columns=120, rows=35,
                               command=("tui", "--session", session_id, *options),
                               environment={"HOME": str(home)})

                def instructions(request: dict) -> str:
                    return request.get("instructions", request.get("system", "")) or next(
                        message["content"] for message in request["messages"] if message["role"] == "system")

                def assert_capabilities(context: str) -> None:
                    for value in ("/bin/echo", "allowed", "approval", "/bin/sh", "src/private"):
                        assert value in context, (value, context)
                    assert "deny" in context.lower() or "denied" in context.lower(), context

                def execute_case(name: str, provider: str = "local", directory: Path = work,
                                 timeout: int = 20):
                    result = root / f"result-{name}.json"
                    completed = subprocess.run(
                        [binary, "--state-dir", state, "exec", "--standalone", "--model",
                         f"{provider}/fixture", "--workdir", directory, "--prompt", f"instruction-{name}",
                         "--result-file", result, "--format", "json"],
                        cwd=directory, env=environment, capture_output=True, text=True, timeout=timeout)
                    return completed, json.loads(result.read_text()) if result.exists() else None

                try:
                    chat.expect("resumed session")
                    chat.send("instruction-first\r")
                    chat.expect("instruction-first-complete")
                    assert len(requests) == 2, requests
                    initial, continuation = map(instructions, requests)
                    assert initial == continuation
                    assert initial.count("operator-initial") == 1
                    assert initial.count("repository-initial") == 1
                    assert initial.index("operator-initial") < initial.index("repository-initial")
                    assert "operator-updated" not in initial and "repository-updated" not in initial
                    assert "review" in initial
                    assert_capabilities(initial)
                    results = [message for message in requests[1]["messages"] if message["role"] == "tool"]
                    assert any("instruction-evidence-marker" in message["content"] for message in results), results
                    assert any("capability-command-evidence" in message["content"] for message in results), results
                    assert any("denied" in message["content"].lower() for message in results), results
                    assert not (work / "src/forbidden-command").exists()
                    chat.send("instruction-second\r")
                    chat.expect("instruction-second-complete")
                    assert len(requests) == 3
                    updated = instructions(requests[-1])
                    assert "operator-updated" in updated and "repository-updated" in updated
                    assert "operator-initial" not in updated and "repository-initial" not in updated
                    saved_instructions = repository_file.read_text()
                    chat.send("/mode\r")
                    chat.expect("Modes · filter:")
                    chat.send("editing\r")
                    chat.absent("Modes · filter:")
                    chat.send("instruction-protected-write\r")
                    chat.expect("instruction-protected-write-complete")
                    assert len(requests) == 5, requests
                    assert "write_file" in [tool["function"]["name"] for tool in requests[3]["tools"]]
                    rejected = next(message for message in requests[4]["messages"]
                                    if message.get("tool_call_id") == "protect_instructions")
                    assert "protected" in rejected["content"].lower(), rejected
                    assert repository_file.read_text() == saved_instructions
                    chat.capture(f"model-instructions-{engine}")
                    chat.send("/quit\r")
                    chat.finish()
                    recorded = transcript.read_text()
                    for marker in ("operator-initial", "repository-initial", "operator-updated", "repository-updated"):
                        assert marker not in recorded, recorded
                    if engine == "service":
                        continue
                    operator_file.unlink()
                    operator_target = root / "operator-instructions.md"
                    operator_target.write_text("Operator instruction marker: operator-linked.\n")
                    operator_file.symlink_to(operator_target)
                    (work / "src/ahi.modes.yaml").write_text(policy)
                    for provider in ("responses", "anthropic"):
                        before = len(requests)
                        completed, result = execute_case(provider, provider, work / "src")
                        assert completed.returncode == 0, (completed.stdout, completed.stderr, result)
                        assert len(requests) == before + 1
                        context = instructions(requests[-1])
                        assert "operator-linked" in context and "repository-updated" in context
                        assert_capabilities(context)
                        assert result["status"] == "ok", result
                    invalid_files = (
                        ("symbolic-link", lambda: repository_file.symlink_to(operator_target)),
                        ("invalid-utf8", lambda: repository_file.write_bytes(b"\xff")),
                        ("oversized", lambda: repository_file.write_bytes(b"x" * (64 * 1024 + 1))),
                        ("directory", repository_file.mkdir),
                        ("named-pipe", lambda: os.mkfifo(repository_file)),
                    )
                    for name, create in invalid_files:
                        if repository_file.is_dir() and not repository_file.is_symlink():
                            repository_file.rmdir()
                        else:
                            repository_file.unlink(missing_ok=True)
                        create()
                        before = len(requests)
                        started = time.monotonic()
                        completed, result = execute_case(name, timeout=5)
                        assert time.monotonic() - started < 5, name
                        assert completed.returncode != 0, (name, completed.stdout, completed.stderr, result)
                        assert len(requests) == before, (name, requests[before:])
                    repository_file.unlink()
                    repository_file.write_text(saved_instructions)
                    operator_file.unlink()
                    operator_pipe = root / "operator-instructions.pipe"
                    os.mkfifo(operator_pipe)
                    operator_file.symlink_to(operator_pipe)
                    before = len(requests)
                    started = time.monotonic()
                    completed, result = execute_case("operator-named-pipe", timeout=5)
                    assert time.monotonic() - started < 5
                    assert completed.returncode != 0, (completed.stdout, completed.stderr, result)
                    assert len(requests) == before, requests[before:]
                    repository_file.unlink()
                    operator_file.unlink()
                    before = len(requests)
                    completed, result = execute_case("missing-files")
                    assert completed.returncode == 0, (completed.stdout, completed.stderr, result)
                    assert len(requests) == before + 1
                    context = instructions(requests[-1])
                    assert "operator-linked" not in context and "repository-updated" not in context
                finally:
                    chat.close()
                    if engine == "service":
                        subprocess.run([binary, "--state-dir", state, "serve", "--socket", service_socket, "stop"],
                                       env=environment, capture_output=True, timeout=10)
                    gateway.shutdown()
                    gateway.server_close()
                    server_thread.join(5)
        print("PASS: instruction loading, capability restrictions, turn snapshots, provider encodings and durable-history separation")
        return

    if args.queue_manager_only:
        for engine in ("standalone", "service"):
            requests = []
            releases = [threading.Event() for _ in range(6)]

            class QueueGateway(BaseHTTPRequestHandler):
                def do_POST(self):
                    request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    number = len(requests)
                    requests.append(request)
                    if number >= len(releases) or not releases[number].wait(30):
                        self.send_error(500)
                        return
                    if number == 2:
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream")
                        self.send_header("Content-Length", "500")
                        self.end_headers()
                        self.wfile.write(b": interrupted\n\n")
                        return
                    event = {"choices": [{"index": 0,
                              "delta": {"content": f"queue-response-{number + 1}"},
                              "finish_reason": "stop"}]}
                    payload = f"data: {json.dumps(event)}\n\ndata: [DONE]\n\n".encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)

                def log_message(self, *_arguments):
                    pass

            with tempfile.TemporaryDirectory(prefix="ahi-queue-", dir="/tmp") as temporary:
                state = Path(temporary)
                session_id = "0f1e2d3c-4b5a-4978-8765-43210fedcba9"
                store = state / "sessions"
                store.mkdir()
                transcript = store / f"{session_id}.jsonl"
                transcript.write_text(json.dumps(dict(
                    type="session", schema=1, id=session_id, host="fixture", cwd=temporary,
                    ahi_version="0.32.0", gateway="local", model="fixture", thinking="off",
                    started="2026-01-01T00:00:00Z")) + "\n")
                gateway = ThreadingHTTPServer(("127.0.0.1", 0), QueueGateway)
                server_thread = threading.Thread(target=gateway.serve_forever, daemon=True)
                server_thread.start()
                (state / "models.json").write_text(json.dumps({"providers": {"local": {
                    "baseUrl": f"http://127.0.0.1:{gateway.server_port}/v1",
                    "api": "openai-completions", "models": [{"id": "fixture"}],
                }}}))
                service_socket = str(state / "service.sock")
                options = ("--standalone",) if engine == "standalone" else ("--socket", service_socket)
                chat = Session(state, cwd=state, columns=80, rows=24,
                               command=("tui", "--session", session_id, *options))

                def wait_requests(count: int) -> None:
                    deadline = time.monotonic() + 10
                    while len(requests) < count and time.monotonic() < deadline:
                        chat.read()
                    assert len(requests) == count, [r["messages"][-1] for r in requests]

                def expect_selected(text: str) -> None:
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        if any("›" in row and text in row for row in chat.screen().splitlines()):
                            return
                        chat.read()
                    raise AssertionError((text, chat.screen()))

                try:
                    chat.expect("resumed session")
                    chat.send("first-prompt\r")
                    wait_requests(1)
                    queued = ["second-prompt", "third-prompt", "fourth-prompt"]
                    queued.extend(f"pending-{number:02}" for number in range(1, 10))
                    for prompt in queued:
                        chat.send(prompt + "\r")
                    chat.expect("12 pending")
                    chat.send("preserved-draft\x1bq")
                    expect_selected("second-prompt")
                    chat.expect("p Pause")
                    chat.expect("r Resume + close")
                    chat.send("\x1b[B\x1b[1;3A")
                    expect_selected("third-prompt")
                    chat.send("\r-edited\r")
                    expect_selected("third-prompt-edited")
                    chat.send("\x1b[F")
                    expect_selected("pending-09")
                    chat.send("\x1b[5~")
                    chat.absent("› 12")
                    chat.send("\x1b[6~\x1b[H")
                    expect_selected("third-prompt-edited")
                    chat.capture(f"queue-manager-{engine}-80x24")
                    chat.resize(40, 12)
                    expect_selected("third-prompt-edited")
                    chat.expect("p Pause")
                    chat.expect("r Resume + close")
                    chat.expect("preserved-draft")
                    chat.send("\r")
                    chat.expect("Edit queued prompt")
                    chat.send("\x1b[200~" + " wrapped-content" * 5 + "\ncompact-end\x1b[201~")
                    chat.expect("compact-end")
                    compact_rows = chat.screen().splitlines()
                    header_row = next(index for index, row in enumerate(compact_rows)
                                      if "Edit queued prompt" in row)
                    content_row = next(index for index, row in enumerate(compact_rows)
                                       if "compact-end" in row)
                    assert content_row >= header_row + 2, chat.screen()
                    chat.capture(f"queue-manager-{engine}-editor-40x12")
                    chat.send("\x1b")
                    expect_selected("third-prompt-edited")
                    chat.send("\x1b[F")
                    expect_selected("pending-09")
                    chat.capture(f"queue-manager-{engine}-40x12")
                    chat.resize(80, 24)
                    expect_selected("pending-09")
                    chat.send("\x1b[H?")
                    chat.expect("Queue controls")
                    chat.expect("p: Ready → Paused")
                    chat.expect("r: Paused → Ready")
                    chat.capture(f"queue-manager-{engine}-help")
                    chat.send("p?")
                    chat.expect("Queue paused")
                    expect_selected("third-prompt-edited")
                    chat.send("?r")
                    chat.absent("Queue controls")
                    chat.expect("Queue ready")
                    chat.send("\x1bq")
                    expect_selected("third-prompt-edited")
                    releases[0].set()
                    deadline = time.monotonic() + .5
                    while time.monotonic() < deadline:
                        chat.read()
                    assert len(requests) == 1, "opening the queue must suspend dispatch"
                    chat.send("r")
                    wait_requests(2)
                    assert requests[1]["messages"][-1]["content"] == "third-prompt-edited"
                    chat.expect("preserved-draft")
                    chat.send("\x1bqp")
                    chat.expect("Queue paused")
                    releases[1].set()
                    deadline = time.monotonic() + .5
                    while time.monotonic() < deadline:
                        chat.read()
                    assert len(requests) == 2
                    expect_selected("second-prompt")
                    chat.send("\r-discarded")
                    chat.expect("second-prompt-discarded")
                    chat.send("\x1b")
                    expect_selected("second-prompt")
                    chat.send("\x1b[13;3u/quit\r")
                    chat.expect("Slash commands cannot be queued.")
                    chat.capture(f"queue-manager-{engine}-invalid-edit")
                    chat.send("\x1b")
                    expect_selected("second-prompt")
                    chat.send("\r")
                    chat.expect("Edit queued prompt")
                    chat.expect("second-prompt")
                    chat.absent("second-prompt-discarded")
                    chat.send("\x1b")
                    expect_selected("second-prompt")
                    chat.send("\x1b[3~")
                    expect_selected("fourth-prompt")
                    chat.resize(80, 7)
                    expect_selected("fourth-prompt")
                    chat.expect("p Pause")
                    chat.expect("r Resume + close")
                    chat.send("c")
                    chat.expect("y clear all")
                    chat.capture(f"queue-manager-{engine}-clear-80x7")
                    chat.send("\x1b")
                    chat.absent("y clear all")
                    chat.resize(80, 24)
                    expect_selected("fourth-prompt")
                    chat.send("c\x1b")
                    expect_selected("fourth-prompt")
                    chat.send("cy")
                    chat.expect("0 pending")
                    chat.capture(f"queue-manager-{engine}-empty")
                    chat.send("\x1b")
                    chat.absent("No pending prompts")
                    chat.expect("preserved-draft")
                    chat.send("\x03")
                    chat.absent("preserved-draft")
                    assert len(requests) == 2
                    chat.send("retry-current\r")
                    wait_requests(3)
                    chat.send("after-retry\r")
                    chat.expect("1 pending")
                    releases[2].set()
                    chat.expect("Enter resends the last message")
                    chat.send("\r")
                    wait_requests(4)
                    assert requests[3]["messages"][-1]["content"] == "retry-current"
                    assert sum(message.get("content") == "retry-current"
                               for message in requests[3]["messages"]) == 1
                    releases[3].set()
                    chat.expect("queue-response-4")
                    assert len(requests) == 4
                    chat.expect("Queue paused")
                    chat.send("\x1bqr")
                    wait_requests(5)
                    assert requests[4]["messages"][-1]["content"] == "after-retry"
                    chat.send("after-mode-dialog\r/mode\r")
                    chat.expect("Modes · filter:")
                    releases[4].set()
                    deadline = time.monotonic() + .5
                    while time.monotonic() < deadline:
                        chat.read()
                    assert len(requests) == 5, "the mode dialog must suspend dispatch"
                    chat.capture(f"queue-manager-{engine}-mode-dialog")
                    chat.send("\x1b")
                    wait_requests(6)
                    assert requests[5]["messages"][-1]["content"] == "after-mode-dialog"
                    releases[5].set()
                    chat.expect("queue-response-6")
                    chat.send("/quit\r")
                    chat.finish()
                    entries = [json.loads(line) for line in transcript.read_text().splitlines()]
                    operator_prompts = [entry["text"] for entry in entries if entry.get("role") == "operator"]
                    assert operator_prompts == [
                        "first-prompt", "third-prompt-edited", "retry-current",
                        "retry-current", "after-retry", "after-mode-dialog"], operator_prompts
                finally:
                    for release in releases:
                        release.set()
                    chat.close()
                    if engine == "service":
                        subprocess.run([binary, "--state-dir", state, "serve", "--socket", service_socket, "stop"],
                                       capture_output=True, timeout=10, check=True)
                    gateway.shutdown()
                    gateway.server_close()
                    server_thread.join(5)
        print("PASS: standalone and service queue editing, request order, draft preservation and 80×24/40×12 layouts")
        return

    if args.stream_recovery_only:
        marker = "visible-stream-marker"
        prose = "Fixture prose. " * 150 + marker
        release = threading.Event()

        class SlowStream(BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                event = {"choices": [{"index": 0, "delta": {"content": prose}}]}
                self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
                self.wfile.flush()
                release.wait(30)  # quit while the gateway is still streaming

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), SlowStream)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            for mode in ("standalone", "service", "crash"):
                with tempfile.TemporaryDirectory(prefix="ahi-stream-", dir="/tmp") as temp:
                    state = Path(temp)
                    sid = "0f1e2d3c-4b5a-4978-8765-43210fedcba9"
                    store = state / "sessions"
                    store.mkdir()
                    path = store / f"{sid}.jsonl"
                    header = dict(type="session", schema=1, id=sid, host="fixture", cwd=temp,
                                  ahi_version="0.21.0", gateway="local", model="fixture", thinking="off",
                                  started="2026-01-01T00:00:00Z")
                    path.write_text(json.dumps(header) + "\n")
                    (state / "models.json").write_text(json.dumps({"providers": {"local": {
                        "baseUrl": f"http://127.0.0.1:{server.server_port}/v1", "api": "openai-completions",
                        "models": [{"id": "fixture"}],
                    }}}))
                    sock = str(state / "run/serve.sock")
                    options = ("--socket", sock) if mode == "service" else ("--standalone",)
                    command = ("tui", "--session", sid, *options)
                    chat = Session(state, cwd=state, columns=120, rows=60, command=command)
                    try:
                        chat.expect("resumed session")
                        chat.send("produce some prose\r")
                        chat.expect("Thinking...")
                        deadline = time.monotonic() + 5
                        while True:
                            entries = [json.loads(line) for line in path.read_text().splitlines()]
                            if any(marker in entry.get("delta", "") for entry in entries):
                                break
                            assert time.monotonic() < deadline, "visible prefix was not checkpointed"
                            time.sleep(.05)
                        assert not any(entry["type"] == "turn" for entry in entries)
                        chat.absent(marker)  # Rendering is quiet; the checkpoint is not.
                        if mode != "crash":
                            chat.send("\x03")
                            chat.finish()
                            entries = [json.loads(line) for line in path.read_text().splitlines()]
                            assert entries[-1].get("status") == "cancelled", entries[-1]
                            assert [e["text"] for e in entries if e.get("role") == "assistant"] == [prose]
                    finally:
                        chat.close()
                        if mode == "service":
                            subprocess.run([binary, "--state-dir", state, "serve", "--socket", sock, "stop"],
                                           capture_output=True, timeout=10, check=True)
                    shown = subprocess.check_output([binary, "--state-dir", state, "session", "show", sid], text=True)
                    assert shown.count(marker) == 1 and prose in shown, shown
                    reopened = Session(state, cwd=state, columns=120, rows=60,
                                       command=("tui", "--session", sid, "--standalone"))
                    try:
                        reopened.expect(marker)
                        assert reopened.screen().count(marker) == 1
                        reopened.send("/quit\r")
                        reopened.finish()
                    finally:
                        reopened.close()
        finally:
            release.set()
            server.shutdown()
            server.server_close()
            worker.join()
        print("PASS: visible checkpoints, standalone and service Ctrl+C, and interrupted standalone recovery")
        return

    if args.session_details_only:
        with tempfile.TemporaryDirectory(prefix="ahi-rich-session-") as temp:
            state = Path(temp)
            session_id = "0f1e2d3c-4b5a-4978-8765-43210fedcba9"
            (state / "models.json").write_text(json.dumps({"providers": {"local": {
                "baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions",
                "models": [{"id": "fixture"}],
            }}}))
            records = [{"type": "session", "schema": 1, "id": session_id,
                        "started": "2026-01-01T00:00:00Z", "cwd": str(state),
                        "host": "fixture", "ahi_version": "0.17.0",
                        "gateway": "local", "model": "fixture", "thinking": "off"}]
            entries = [
                {"type": "message", "role": "operator", "text": "change and test"},
                {"type": "message", "role": "assistant", "text": "", "calls": [
                    {"id": "edit", "name": "edit_file", "arguments": {"path": "src/a.rs", "old": "old", "new": "new"}},
                    {"id": "test", "name": "run_command", "arguments": {"argv": ["cargo", "test"]}},
                    {"id": "oversize", "name": "read_file", "arguments": {"path": "large.txt"}},
                    {"id": "legacy", "name": "read_file", "arguments": {"path": "legacy.txt"}},
                ]},
                {"type": "message", "role": "tool", "exit_code": 0, "text": "edit_file src/a.rs",
                 "result": {"call_id": "edit", "name": "edit_file", "content": "written"},
                 "presentation": {"detail": {"kind": "write", "before": "old\n", "after": "new\n"},
                                  "elapsed": {"secs": 0, "nanos": 400000000}}},
                {"type": "message", "role": "tool", "exit_code": 1, "text": "run_command .",
                 "result": {"call_id": "test", "name": "run_command", "content": "tests failed"},
                 "presentation": {"detail": {"kind": "command", "argv": ["cargo", "test"], "exit_code": 1,
                                              "stdout": "one test", "stderr": "assertion failed",
                                              "stdout_cut": False, "stderr_cut": False},
                                  "elapsed": {"secs": 3, "nanos": 800000000}}},
                {"type": "message", "role": "tool", "exit_code": 0, "text": "read_file large.txt",
                 "result": {"call_id": "oversize", "name": "read_file", "content": "bounded result"},
                 "presentation": {"detail": {"kind": "omitted", "reason": {"kind": "detail_too_large"}},
                                  "elapsed": {"secs": 1, "nanos": 0}}},
                {"type": "message", "role": "tool", "exit_code": 0, "text": "read_file legacy.txt",
                 "result": {"call_id": "legacy", "name": "read_file", "content": "legacy result"},
                 "presentation": {"detail": {"kind": "omitted", "reason": "Historical preview unavailable"},
                                  "elapsed": {"secs": 2, "nanos": 0}}},
                {"type": "message", "role": "assistant", "text": "The tests failed."},
                {"type": "turn", "run_id": "00112233aabbccdd", "status": "ok", "prompt_sha256": "", "result": {}},
            ]
            for seq, entry in enumerate(entries, 1):
                records.append(dict(entry, seq=seq, parent=seq - 1 or None, time="2026-01-01T00:00:00Z"))
            source = state / "fixture.jsonl"
            source.write_text("".join(json.dumps(record) + "\n" for record in records))
            subprocess.run([str(binary), "--state-dir", str(state), "session", "import", str(source)],
                           capture_output=True, timeout=10, check=True)
            exported = state / "exported.jsonl"
            subprocess.run([str(binary), "--state-dir", str(state), "session", "export", session_id, "--to", str(exported)],
                           capture_output=True, timeout=10, check=True)
            assert [json.loads(line) for line in exported.read_text().splitlines()] == records
            try:
                for label, command in [
                    ("standalone", ("tui", "--session", session_id, "--standalone")),
                    ("service", ("--session", session_id)),
                ]:
                    session = Session(state, columns=100, rows=60, command=command)
                    try:
                        session.expect("edit_file src/a.rs · +1 -1 · 0.4s")
                        session.absent("assertion failed")
                        session.send("\x0f")
                        session.expect("1   -old")
                        session.expect("1 +new")
                        session.expect("cargo test · exit 1 · 3.8s")
                        session.expect("assertion failed")
                        session.expect("512 KiB session-detail limit")
                        session.expect("Historical preview unavailable")
                        session.capture(f"resumed-tool-details-{label}")
                        session.send("/quit\r")
                        output = session.finish()
                        assert f"Resume: ahi --session {session_id}" in output
                    finally:
                        session.close()
            finally:
                subprocess.run([str(binary), "--state-dir", str(state), "serve", "stop"],
                               capture_output=True, timeout=10)
        print("PASS: import/export and resumed edit diffs, failed command streams, typed/legacy omissions, exit code, duration, and quit in both engines")
        return

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

        # Real cursor-local skill completion; never submit to the configured provider.
        skill_state = state / "skill-picker"
        skill_state.mkdir()
        (skill_state / "models.json").write_text(body)
        (skill_state / "catalogs").mkdir()
        (skill_state / "catalogs/openai.json").write_text(json.dumps(["first", "second"]))
        home = skill_state / "home"
        home.mkdir()
        for root, description in [(skill_state, "repo"), (home, "operator")]:
            for name in ["review-one", "review-two"]:
                directory = root / ".agents/skills" / name
                directory.mkdir(parents=True)
                (directory / "SKILL.md").write_text(
                    f"---\nname: {name}\ndescription: {description}\n---\n{description}\n")
        session = Session(skill_state, cwd=skill_state, columns=100, rows=30,
                          environment={"HOME": str(home)})
        try:
            session.expect("Models · Enter use model")
            session.send("j\r")
            session.expect("Skills: 4 available · 4 discovered")
            session.send("\x1b[200~before $operator/review-two after\x1b[201~")
            session.expect("before $operator/review-two after")
            session.absent("Skills [")
            session.absent("Skills: operator/review-two")
            session.send("\x03")
            session.send("before  after")
            session.send("\x1b[D" * 6)
            session.send("$rvw")
            session.expect("Skills [Both]")
            session.expect("$repo/review-one")
            session.expect("$operator/review-one")
            session.send("\t")
            session.expect("Skills [Repo]")
            session.absent("$operator/review-one")
            session.send("\t")
            session.expect("Skills [Operator]")
            session.absent("$repo/review-one")
            session.send("\x1b[B\r")
            session.expect("before $operator/review-two after")
            session.expect("Skills: operator/review-two")
            session.absent("Skills [")
            session.capture("inline-skill-completion")
            session.send("\x15")  # Ctrl+U stashes without submitting the selected skill.
            session.absent("before $operator/review-two after")
            session.send("later draft\x1b[A")
            session.expect("before $operator/review-two after")
            session.expect("Skills: operator/review-two")
            session.send("\x1b[B")
            session.expect("later draft")
            session.absent("Skills: operator/review-two")
            session.send("\x12missing")  # Ctrl+R, no match, cancel without touching draft.
            session.expect("[no match]")
            session.send("\x1b")
            session.expect("later draft")
            session.send("\x15new review-two note\x15")  # stash two more inputs
            session.send("\x12review-two")
            session.expect("new review-two note")
            session.send("\x12")  # previous matching input, skipping 'later draft'
            session.expect("before $operator/review-two after")
            session.send("\r")  # accept for editing, not submission
            session.expect("Skills: operator/review-two")
            session.expect("before $operator/review-two after")
            session.send("\x1b[B")
            session.expect("later draft")
            session.send("\x03$rev")
            session.expect("Skills [Both]")
            session.send("\x1b")
            session.absent("Skills [")
            session.send("iew")
            session.expect("$review")
            session.absent("Skills [")
            session.send("\x03/quit\r")
            session.finish()
        finally:
            session.close()

        if args.skill_picker_only:
            print("scoped skill picker and bracketed paste passed")
            return

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
        # Match Pi's Linux -> Windows Terminal OSC 52 -> UTF-8-file interop routing.
        if sys.platform.startswith("linux"):
            wsl_state = state / "wsl-copy"
            (wsl_state / "catalogs").mkdir(parents=True)
            private_tmp = wsl_state / "private'tmp"
            private_tmp.mkdir()
            wsl_id = "vendor/界e\u0301🙂"
            (wsl_state / "models.json").write_text(json.dumps({"providers": {"openai": {
                "baseUrl": "https://api.openai.com/v1", "api": "openai-completions",
                "models": [{"id": wsl_id}],
            }}}))
            (wsl_state / "catalogs/openai.json").write_text(json.dumps([wsl_id]))
            windows_copy = wsl_state / "windows-copy.txt"
            powershell = clipboard_bin / "powershell.exe"
            powershell.write_text(
                f"#!{sys.executable}\nimport pathlib, re, sys\n"
                "script = sys.argv[-1]\n"
                "path = re.search(r\"ReadAllText\\('((?:[^']|'')*)'\", script)[1].replace(\"''\", \"'\")\n"
                "source = pathlib.Path(path)\n"
                "assert source.stat().st_mode & 0o777 == 0o600\n"
                f"pathlib.Path({str(windows_copy)!r}).write_bytes(source.read_bytes())\n"
            )
            powershell.chmod(0o700)
            wslpath = clipboard_bin / "wslpath"
            wslpath.write_text('#!/bin/sh\n/bin/printf "%s\\n" "$2"\n')
            wslpath.chmod(0o700)
            try:
                for mode in ("linux", "powershell", "terminal", "remote", "forced", "failure"):
                    environment = {"WSL_DISTRO_NAME": "ahi-test", "TMPDIR": str(private_tmp)}
                    if mode != "linux":
                        environment["DISPLAY"] = ""
                    if mode == "terminal":
                        environment["WT_SESSION"] = "ahi-test"
                    if mode == "remote":
                        environment.update(DISPLAY=":ahi-test", SSH_CONNECTION="ahi-test")
                    if mode == "forced":
                        environment.update(DISPLAY=":ahi-test", AHI_CLIPBOARD_OSC52="1")
                    if mode == "failure":
                        # Disable interop and fail X11 helpers; a desktop must
                        # report failure rather than assume OSC 52 was accepted.
                        environment["DISPLAY"] = ":ahi-test"
                        powershell.write_text("#!/bin/sh\nexit 1\n")
                        for name in ("xclip", "xsel"):
                            (clipboard_bin / name).write_text("#!/bin/sh\nexit 1\n")
                    session = Session(wsl_state, executable_dir=clipboard_bin, environment=environment)
                    try:
                        session.expect("Models · Enter use model")
                        session.send("cy")
                        session.expect("No clipboard available" if mode == "failure" else "Copied")
                        sequence = b"\x1b]52;c;" + base64.b64encode(wsl_id.encode()) + b"\x07"
                        assert (sequence in session.output) == (mode in ("terminal", "remote", "forced"))
                        assert copied_text.exists() == (mode in ("linux", "remote", "forced"))
                        assert windows_copy.exists() == (mode == "powershell")
                        if copied_text.exists():
                            assert copied_text.read_text() == wsl_id
                        if windows_copy.exists():
                            assert windows_copy.read_text() == wsl_id
                        assert not list(private_tmp.iterdir()), "temporary clipboard file leaked"
                        session.send("q")
                        session.finish()
                    finally:
                        session.close()
                        copied_text.unlink(missing_ok=True)
                        windows_copy.unlink(missing_ok=True)
            finally:
                powershell.unlink()
                wslpath.unlink()
                for name in ("xclip", "xsel"):
                    (clipboard_bin / name).write_text("#!/bin/sh\n/bin/cat > " + shlex.quote(str(copied_text)) + "\n")

        # Explicit files exercise loading; omitted vertical padding uses one row.
        (state / "padding.toml").write_text("horizontal = 2\n")
        (state / "theme.toml").write_text("[backgrounds]\noperator = '#112233'\n")
        session = Session(state, cwd=state, executable_dir=clipboard_bin,
                          environment={"AHI_CLIPBOARD_OSC52": "1"})
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
            assert rows[-2].startswith("─ / commands ")
            assert "Plan · Alt+M to review trust · /mode" in rows[-2]
            assert "↑" not in rows[-1]  # No permanent token-accounting row.

            # The configured inset is two cells; default vertical padding is one row.
            message_row = next(i for i, row in enumerate(rows) if row.startswith("  merge smoke"))
            assert b"48;2;17;34;51" in session.output, "configured operator background missing"
            session.capture("chat-message-padding")
            assert message_row >= 1, session.screen()
            assert all(not rows[message_row + offset].strip() for offset in (-1, 1))
            # SGR coordinates are one-based. Select the text, not its padding.
            y = message_row + 1
            session.send(f"\x1b[<0;3;{y}M\x1b[<32;7;{y}M\x1b[<0;7;{y}m")
            session.expect("Selection copied")
            assert copied_text.read_text() == "merge"
            assert b"\x1b]52;c;bWVyZ2U=\x07" in session.output
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
            restored = output.rsplit("\x1b[?1049l", 1)[-1]
            assert "Session: Switched model to openai/fresh/from-disk" in restored
            sid = re.search(r"Resume: ahi --session ([0-9a-f-]+)", restored).group(1)
            shown = subprocess.check_output([binary, "--state-dir", state, "session", "show", sid], text=True)
            assert "switched model to openai/fresh/from-disk" in shown  # quit without submitting another turn
            assert "provider openai origin" not in output
            (state / "models.json").write_bytes(original)
        finally:
            session.close()
            (state / "padding.toml").unlink()
            (state / "theme.toml").unlink()
            if args.appearance_only:
                subprocess.run([str(binary), "--state-dir", str(state), "serve", "stop"],
                               capture_output=True, timeout=10, check=True)
        if args.appearance_only:
            print("PASS: configured padding and RGB background, exact clipboard text, model-switch retention, and terminal quit")
            return

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

            credential_path.parent.mkdir(mode=0o700)
            credential_path.write_text('{"flow":"legacy-fixture","access_token":"preserved-fixture"}')
            credential_path.chmod(0o600)
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
        environment.pop("ANTHROPIC_API_KEY", None)
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
            core, question = request["messages"]
            assert core["role"] == "system" and core["content"]
            assert question == {"role": "user", "content": "verify discovered gateway"}
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
            session.expect("Thinking...")
            assert "Gateway URL" not in session.screen()
            release_response.set()
            session.absent("Thinking...")
            assert "/gateway" in session.screen()
            assert gateway_requests[-1][2]["messages"] == [core,
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
        # These cases verify exact request-compatibility bytes without tools.
        # Keep them in explicit Policy mode rather than implicitly selecting
        # built-in Read-only tools at the new trust prompt.
        (metadata_state / "ahi.tools.yaml").write_text("version: 1\nrules: []\n")
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
            assert request["max_tokens"] == 32768
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
            session.absent("Thinking...")
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
            session.absent("Thinking...")
            deadline = time.monotonic() + 5
            while len(gateway_requests) < 2 and time.monotonic() < deadline:
                session.read()
            assert gateway_requests[-1][2]["messages"][2]["reasoning_content"] == ""

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
            # A textual tool marker is still partial prose, never a structured call.
            gateway_response["stream"] = usage_stream.replace('"stop"', '"length"').replace(
                'metadata confirmed', '<tool_call>unfinished example')
            session.send("verify explicit truncation\r")
            session.expect("<tool_call>unfinished example")
            session.expect("request limit: 128 tokens")
            session.expect("Ask for a shorter answer")
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
            session.expect("over inputLimits.maxRequestBytes (")
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
            session.absent("Thinking...")
            assert "reasoning_effort" not in gateway_requests[-1][2]
            assert "stream_options" not in gateway_requests[-1][2]
            session.capture("metadata-restart-off")
            expected_request = {
                "model": "GLM-5.3-Flash-EXL3", "stream": True,
                "messages": [core, {"role": "user", "content": "容量"}],
                "max_completion_tokens": 128, "store": False,
            }
            exact_bytes = len(json.dumps(expected_request, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            session.send("/clear\r")
            local_provider["models"][0]["inputLimits"] = {"maxRequestBytes": exact_bytes - 1}
            local_path.write_text(json.dumps(local_document))
            before = len(gateway_requests)
            session.send("容量\r")
            session.expect("over inputLimits.maxRequestBytes (")
            assert len(gateway_requests) == before
            session.send("/clear\r")
            local_provider["models"][0]["inputLimits"]["maxRequestBytes"] = exact_bytes
            local_path.write_text(json.dumps(local_document))
            session.send("容量\r")
            session.expect("metadata confirmed")
            session.absent("Thinking...")
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
            session.absent("Thinking...")
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

        # Tool calls: enabled only by ahi.tools.yaml in the working directory
        # (never the state directory), approved or denied by the operator.
        tool_workspace = state / "tool-workspace"
        (tool_workspace / "src").mkdir(parents=True)
        (tool_workspace / "src" / "lib.rs").write_text("pub fn tool_fixture() {}\n")
        (tool_workspace / "ahi.tools.yaml").write_text(
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
            session.expect("y once · n deny")
            session.capture("tool-approval-prompt")
            assert [tool["function"]["name"] for tool in gateway_requests[-1][2]["tools"]] == ["read_file"]
            assert len(gateway_requests) == 1
            session.send("y")
            session.expect("approved once")
            session.absent("pub fn tool_fixture() {}")
            session.send("\x0f")
            session.expect("pub fn tool_fixture() {}")
            session.expect("tool result received")
            session.absent("Thinking...")
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
            session.absent("Thinking...")
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
        "provider/model overrides, declared output limits without clamping, reported paid usage without cached-input double counting, "
        "usage omission, reasoning Off and assistant replay, finish-reason fallback with truncation/tool safety, "
        "ahi.tools.yaml-gated read_file with operator approve/deny, tool resubmission and cross-turn replay, "
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
        "default mouse controls, managed drag-selection and exact clipboard payload, WSL Linux/OSC52/UTF-8-file routing and cleanup, remote/forced OSC52, native text-selection mode via /mouse off, slash commands/filter/dismiss, /catalog, /models, /quit, "
        "single-row footer, context in the input rule, and pre-submit draft wrapping/growth"
    )


if __name__ == "__main__":
    main()
