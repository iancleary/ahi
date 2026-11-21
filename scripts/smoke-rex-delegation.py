#!/usr/bin/env python3
"""Exercise real, headless Rex sessions against a disposable ahi service and loopback gateway."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def until(fn, timeout=10):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for {fn}")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    ahi = str(p.parse_args().binary.resolve(strict=True))
    rex = shutil.which("rex")
    assert rex
    received = []
    entered = threading.Event()
    release = threading.Event()

    class Gateway(BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            received.append(request)
            last = request["messages"][-1]["content"]

            def tool(name, args):
                return {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_fixture",
                            "type": "function",
                            "function": {"name": name, "arguments": json.dumps(args)},
                        }
                    ]
                }

            if last == "Revoke trust before admission":
                remembered.unlink()
                delta = tool(
                    "delegate",
                    {"local": {"profile": "rex-scratch"}, "prompt": "Must not start"},
                )
            elif last == "Delegate":
                delta = tool(
                    "delegate",
                    {
                        "local": {"profile": "rex-scratch", "mode": "plan"},
                        "prompt": "Hold until released",
                    },
                )
            elif last.startswith("Follow child:"):
                delta = tool(
                    "delegate",
                    {
                        "local": {
                            "profile": "rex-scratch",
                            "run": last.split(":", 1)[1],
                            "action": "follow",
                        },
                        "prompt": "Parent follow-up",
                    },
                )
            elif last == "Hold until released":
                entered.set()
                assert release.wait(20)
                delta = tool("list_dir", {})
            elif last == "Write allowed":
                delta = tool(
                    "write_file", {"path": "allowed.txt", "content": "fixture"}
                )
            elif last == "Write denied":
                delta = tool(
                    "write_file", {"path": "denied.txt", "content": "forbidden"}
                )
            else:
                delta = {"content": "fixture answer"}
            body = (
                "".join(
                    "data: " + json.dumps({"choices": [dict(index=0, **v)]}) + "\n\n"
                    for v in [
                        {"delta": delta},
                        {
                            "delta": {},
                            "finish_reason": "tool_calls"
                            if "tool_calls" in delta
                            else "stop",
                        },
                    ]
                )
                + "data: [DONE]\n\n"
            )
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body.encode())

    with (
        tempfile.TemporaryDirectory(dir="/tmp", prefix="ahi-child-") as tmp,
        ThreadingHTTPServer(("127.0.0.1", 0), Gateway) as gateway,
    ):
        root = Path(tmp).resolve()
        state = root / "ahi"
        state.mkdir()
        endpoint = "unix://" + str(root / "rex.sock")
        config = root / "empty.lua"
        config.touch()
        env = {
            "HOME": str(root),
            "TMPDIR": str(root),
            "PATH": str(Path(rex).parent) + os.pathsep + "/usr/bin:/bin",
            "AHI_DELEGATE_REX_SERVER": endpoint,
        }
        (state / "models.json").write_text(
            json.dumps(
                {
                    "providers": {
                        "fixture": {
                            "baseUrl": f"http://127.0.0.1:{gateway.server_port}/v1",
                            "api": "openai-completions",
                            "apiKey": "gateway",
                            "models": [{"id": "plain"}],
                        }
                    }
                }
            )
        )

        def command(*args, check=True):
            return subprocess.run(
                args,
                env=env,
                cwd=root,
                check=check,
                timeout=20,
                capture_output=True,
                text=True,
            )

        def call(*args):
            proc = command(*args, check=False)
            assert proc.returncode == 0, (args, proc.stderr)
            return json.loads(proc.stdout)

        rx = [rex, "--autostart=false", "--server", endpoint]
        prefix = [ahi, "--state-dir", str(state), "delegate"]
        prompt = root / "task"

        def rpc(method, **params):
            with socket.socket(socket.AF_UNIX) as s:
                s.settimeout(8)
                s.connect(str(state / "run/serve.sock"))
                s.sendall(
                    (
                        json.dumps({"id": 1, "method": method, "params": params}) + "\n"
                    ).encode()
                )
                return json.loads(s.makefile("rb").readline())["result"]

        def turn(s, prompt):
            rid = rpc(
                "turn.submit",
                session=s,
                mode="trusted",
                prompt=prompt,
                timeout=30,
                unattended=True,
            )["run_id"]
            return until(
                lambda: rpc("turn.get", session=s, run_id=rid)["status"] == "ok"
            )

        def submit(text, mode=None):
            prompt.write_text(text)
            return call(
                *prefix,
                "launch",
                "--server",
                endpoint,
                "--model",
                "fixture/plain",
                "--prompt-file",
                str(prompt),
                *(["--mode", mode] if mode else []),
            )

        def settled(run):
            return call(*prefix, "status", run, "--wait", "10")

        def rex_state(h):
            data = call(
                *rx,
                "api",
                "call",
                "com.superlogical.terminal.program_status",
                json.dumps(
                    {
                        "session_id": h["target"]["session"],
                        "block_id": h["id"],
                        "args": {},
                    }
                ),
            )
            return data

        def has_state(h, state):
            return f'"state": "{state}"' in json.dumps(rex_state(h))

        t = threading.Thread(target=gateway.serve_forever, daemon=True)
        t.start()
        with (root / "rex.log").open("w") as log:
            server = subprocess.Popen(
                [
                    rex,
                    "server",
                    "run",
                    "--listen",
                    endpoint,
                    "--restore=false",
                    "--state-dir",
                    str(root / "rex"),
                    "--config",
                    str(config),
                ],
                cwd=root,
                env=env,
                stdout=log,
                stderr=log,
            )
            try:
                until((root / "rex.sock").is_socket)
                anchor = call(
                    *rx, "new", "--json", "--shell", "none", "--", "/bin/sleep", "90"
                )["session_id"]
                first = submit("Initial prompt")
                rid = first["child"]
                s = settled(rid)
                h = s["assignment"]["phase"]["terminal"]
                assert (
                    h["target"]["session"] != anchor and s["result"]["status"] == "ok"
                )
                until(lambda: has_state(h, "idle"))
                prompt.write_text("Recall prior")
                n = call(*prefix, "follow", rid, "--prompt-file", str(prompt))
                nid = n["assignment"]["id"]
                assert nid != rid and n["child"] == rid
                follow = settled(nid)
                assert (
                    follow["claim"]["pid"] == s["claim"]["pid"]
                    and follow["assignment"]["phase"]["terminal"] == h
                )
                assert [m["content"] for m in received[-1]["messages"]][-3:] == [
                    "Initial prompt",
                    "fixture answer",
                    "Recall prior",
                ]
                assert (
                    command(
                        *prefix,
                        "follow",
                        rid,
                        "--prompt-file",
                        str(prompt),
                        check=False,
                    ).returncode
                    != 0
                )
                call(*prefix, "close", rid)
                until(
                    lambda: (
                        h["target"]["session"]
                        not in [
                            x["session_id"]
                            for x in call(*rx, "ls", "--json")["sessions"]
                        ]
                    )
                )
                transcript = (
                    state
                    / "delegation"
                    / "assignments"
                    / rid
                    / "sessions"
                    / (rid + ".jsonl")
                )
                assert "Recall prior" in transcript.read_text()
                # This workspace explicitly trusts the parent. Revocation below
                # changes its ceiling while that parent's turn is still open.
                remembered = (
                    state
                    / "workspace-trust"
                    / hashlib.sha256(str(root).encode()).hexdigest()
                )
                remembered.parent.mkdir()
                remembered.write_text(str(root))
                parent = rpc(
                    "session.create", cwd=str(root), gateway="fixture", model="plain"
                )["id"]
                turn(parent, "Delegate")
                assert entered.wait(5)
                ref = next(
                    json.loads(m["content"])
                    for req in received
                    for m in req["messages"]
                    if m["role"] == "tool" and "submitted" in m["content"]
                )
                child = call(*prefix, "status", ref["assignment"])
                assert child["result"] is None and child["authority"]["mode"] == "plan"
                h = child["assignment"]["phase"]["terminal"]
                until(lambda: has_state(h, "working"))
                turn(parent, "Independent parent turn")
                assert not release.is_set()
                release.set()
                assert settled(ref["assignment"])["result"]["status"] == "ok"
                until(lambda: has_state(h, "idle"))
                # Another parent cannot adopt the child's ID. The original parent can.
                stranger = rpc(
                    "session.create", cwd=str(root), gateway="fixture", model="plain"
                )["id"]
                turn(stranger, "Follow child:" + ref["assignment"])
                assert call(*prefix, "status", ref["assignment"])["transition"] is None
                turn(parent, "Follow child:" + ref["assignment"])
                next_run = call(*prefix, "status", ref["assignment"])["transition"][
                    "run"
                ]
                assert settled(next_run)["claim"]["pid"] == child["claim"]["pid"]
                # Simulate a human at the child terminal. Do not scrape the answer.
                command(
                    *rx,
                    "send",
                    "--session",
                    h["target"]["session"],
                    "--block",
                    h["id"],
                    "/close",
                )
                command(
                    *rx,
                    "send-key",
                    "--session",
                    h["target"]["session"],
                    "--block",
                    h["id"],
                    "enter",
                )
                until(
                    lambda: (
                        h["target"]["session"]
                        not in [
                            x["session_id"]
                            for x in call(*rx, "ls", "--json")["sessions"]
                        ]
                    )
                )
                assert anchor in [
                    x["session_id"] for x in call(*rx, "ls", "--json")["sessions"]
                ]
                before = set((state / "delegation/assignments").iterdir())
                turn(parent, "Revoke trust before admission")
                assert set((state / "delegation/assignments").iterdir()) == before
                (root / "ahi.modes.yaml").write_text(
                    "version: 2\ndefault_mode: editor\nmodes:\n  editor:\n    allow:\n      files:\n        - tools: [write_file]\n          paths: [allowed.txt]\n"
                )
                edit = submit("Write allowed")
                scratch = Path(edit["assignment"]["scratch"])
                assert (
                    settled(edit["child"])["result"]["status"] == "ok"
                    and (scratch / "allowed.txt").read_text() == "fixture"
                )
                prompt.write_text("Write denied")
                attempt = call(
                    *prefix, "follow", edit["child"], "--prompt-file", str(prompt)
                )
                assert (
                    settled(attempt["assignment"]["id"])["result"]["status"] == "ok"
                    and not (scratch / "denied.txt").exists()
                )
                (root / "ahi.modes.yaml").unlink()
                assert (
                    command(
                        *prefix,
                        "follow",
                        attempt["assignment"]["id"],
                        "--mode",
                        "trusted",
                        "--prompt-file",
                        str(prompt),
                        check=False,
                    ).returncode
                    != 0
                )
                call(*prefix, "close", edit["child"])
                entered.clear()
                release.clear()
                busy = submit("Hold until released")
                assert entered.wait(5)
                assert (
                    call(*prefix, "close", busy["child"])["state"] == "close_requested"
                )
                release.set()
                stopped = settled(busy["child"])
                assert stopped["result"]["status"] == "cancelled"
                until(
                    lambda: (
                        call(*prefix, "status", busy["child"])["lifecycle"]["state"]
                        == "closed"
                    )
                )
                print(
                    "PASS: dedicated Rex sessions; detached parent; real working/idle OSC; same PID and durable history on follow-up; mode ceiling; child and orchestrator close"
                )
            finally:
                release.set()
                if (state / "run/serve.sock").is_socket():
                    command(
                        ahi, "--state-dir", str(state), "serve", "stop", check=False
                    )
                server.terminate()
                server.wait(timeout=10)
                gateway.shutdown()


if __name__ == "__main__":
    main()
