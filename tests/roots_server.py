# Test server that asks the client for roots (a server -> client request)
# before answering tools/call.
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "tools/call":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": "s1", "method": "roots/list"}) + "\n")
        sys.stdout.flush()
        reply = json.loads(sys.stdin.readline())
        roots = [r["uri"] for r in reply.get("result", {}).get("roots", [])]
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"content": [{"type": "text", "text": ",".join(roots)}]}}) + "\n")
        sys.stdout.flush()
    elif "id" in msg:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}) + "\n")
        sys.stdout.flush()
