#!/usr/bin/env python3
"""A tiny stdio MCP server used by the mcpsnap examples and tests.

Run with --v2 to get "the next release": a reworded tool description, a new
required argument, a removed tool, a version bump, a changed error message and
a stray print() that writes to stdout. Exactly the kind of drift that slips
through code review.
"""
import json
import sys

V2 = "--v2" in sys.argv
NOTES = []

TOOLS = [
    {
        "name": "add_note",
        "description": "Save a short note.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "search_notes",
        "description": "Search notes by keyword." if V2 else "Full-text search over saved notes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
            },
            "required": ["query", "limit"] if V2 else ["query"],
        },
    },
]
if not V2:
    TOOLS.append({
        "name": "delete_note",
        "description": "Delete a note by its index.",
        "inputSchema": {
            "type": "object",
            "properties": {"index": {"type": "integer"}},
            "required": ["index"],
        },
    })


def text(t, error=False):
    out = {"content": [{"type": "text", "text": t}]}
    if error:
        out["isError"] = True
    return out


def call(name, args):
    if name == "add_note":
        NOTES.append(args["text"])
        return text(f"saved note #{len(NOTES) - 1}")
    if name == "search_notes":
        hits = [n for n in NOTES if args.get("query", "").lower() in n.lower()]
        return text("\n".join(hits) if hits else "no matches")
    if name == "delete_note" and not V2:
        i = args.get("index", -1)
        if not 0 <= i < len(NOTES):
            return text(f"no note at index {i}", error=True)
        NOTES.pop(i)
        return text("deleted")
    raise KeyError(name)


def handle(msg):
    method, params = msg.get("method"), msg.get("params") or {}
    if method == "initialize":
        return {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "notes-server", "version": "0.4.0" if V2 else "0.3.0"},
        }
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        return call(params.get("name"), params.get("arguments") or {})
    if method == "resources/list":
        return {"resources": [{"uri": "notes://all", "name": "All notes", "mimeType": "text/plain"}]}
    raise LookupError(method)


def main():
    print("notes-server ready", file=sys.stderr, flush=True)
    if V2:
        print("notes-server: loaded 0 notes", flush=True)  # the classic stdout bug
    for line in sys.stdin:
        if not line.strip():
            continue
        msg = json.loads(line)
        if "id" not in msg:
            continue  # notifications need no reply
        reply = {"jsonrpc": "2.0", "id": msg["id"]}
        try:
            reply["result"] = handle(msg)
        except KeyError as e:
            reply["error"] = {"code": -32602, "message": ("Unknown tool " if V2 else "Unknown tool: ") + str(e.args[0])}
        except LookupError as e:
            reply["error"] = {"code": -32601, "message": f"Method not found: {e.args[0]}"}
        sys.stdout.write(json.dumps(reply) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
