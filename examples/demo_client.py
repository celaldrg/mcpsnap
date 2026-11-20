#!/usr/bin/env python3
"""Minimal MCP client for the examples: starts a server command, sends the
messages from client-session.jsonl one by one and waits for each response,
like a real client would.

    python3 examples/demo_client.py -- mcpsnap record -o s.jsonl -- python3 examples/notes_server.py
"""
import json
import os
import subprocess
import sys

here = os.path.dirname(os.path.abspath(__file__))
cmd = sys.argv[sys.argv.index("--") + 1:]
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
with open(os.path.join(here, "client-session.jsonl")) as f:
    for line in f:
        msg = json.loads(line)
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        if "id" not in msg:
            continue
        while True:
            out = proc.stdout.readline()
            if not out:
                sys.exit("server closed the connection")
            try:
                reply = json.loads(out)
            except ValueError:
                continue
            if reply.get("id") == msg["id"]:
                kind = "error" if "error" in reply else "ok"
                print(f"client: {msg['method']} -> {kind}", file=sys.stderr)
                break
proc.stdin.close()
proc.wait()
