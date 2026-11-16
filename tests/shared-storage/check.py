# Copyright 2026 Salesforce, Inc. All rights reserved.
"""Checks session continuity and isolation across two Flex replicas that share Redis.

Run by run.sh after `docker compose up`. Exits non-zero on the first failed check.
"""
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

FLEX_1 = {"hr": "http://localhost:18081/hr/", "finance": "http://localhost:18082/finance/"}
FLEX_2 = {"hr": "http://localhost:28081/hr/", "finance": "http://localhost:28082/finance/"}
AGENT = "http://localhost:18090/requests"
TTL_MILLIS = 600_000
calls = 0


def http(method, url, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={
        "content-type": "application/json",
        "accept": "application/json, text/event-stream",
        **(headers or {}),
    })
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read()
            return resp.status, dict(resp.headers), json.loads(raw) if raw else None
    except urllib.error.HTTPError as err:
        return err.code, dict(err.headers), None


def check(name, condition, detail=""):
    print(f"  {'PASS' if condition else 'FAIL'}  {name}{f'  ({detail})' if detail else ''}")
    if not condition:
        sys.exit(1)


def headers(session=None, client=None):
    h = {}
    if session:
        h["mcp-session-id"] = session
    if client:
        h["x-client-id"] = client
    return h


def initialize(url, client=None):
    status, h, _ = http("POST", url, {"jsonrpc": "2.0", "id": 0, "method": "initialize",
                                      "params": {"protocolVersion": "2025-06-18"}}, headers(client=client))
    sid = {k.lower(): v for k, v in h.items()}.get("mcp-session-id")
    return status, sid


def ask(url, message, session=None, client=None):
    """tools/call; returns (contextId the agent received, contextId in the MCP result)."""
    global calls
    calls += 1
    http("DELETE", AGENT)
    status, _, body = http("POST", url, {"jsonrpc": "2.0", "id": calls, "method": "tools/call",
                                         "params": {"name": "ask_agent", "arguments": {"message": message}}},
                           headers(session, client))
    if status != 200 or body is None or "structuredContent" not in body.get("result", {}):
        check(f"tools/call {message!r} answered", False, f"status {status}, body {body}")
    _, _, received = http("GET", AGENT)
    sent = received[-1]["message"].get("contextId") if received else None
    return sent, body["result"]["structuredContent"]["contextId"]


def wait_ready(urls, timeout=180):
    deadline = time.time() + timeout
    for url in urls:
        while True:
            try:
                if initialize(url, "probe")[0] == 200:
                    break
            except (urllib.error.URLError, ConnectionError, OSError):
                pass
            if time.time() > deadline:
                check(f"{url} ready", False, "timed out")
            time.sleep(2)


def redis(*args):
    return subprocess.run(["docker", "compose", "exec", "-T", "redis", "redis-cli", *args],
                          capture_output=True, text=True, check=True).stdout.split()


def main():
    wait_ready([*FLEX_1.values(), *FLEX_2.values()])

    print("Replica continuity (hr, bound to x-client-id)")
    status, sid = initialize(FLEX_1["hr"], "alice")
    check("initialize on flex-1 issues a session", status == 200 and sid, sid)
    sent, ctx = ask(FLEX_1["hr"], "first", sid, "alice")
    check("first call starts a new context", sent is None and ctx, ctx)
    sent, _ = ask(FLEX_2["hr"], "second", sid, "alice")
    check("flex-2 continues the context stored by flex-1", sent == ctx, f"agent got {sent}")

    print("Identity binding across replicas")
    sent, _ = ask(FLEX_2["hr"], "as bob", sid, "bob")
    check("another client with the same session id gets no context", sent is None, f"agent got {sent}")
    sent, _ = ask(FLEX_2["hr"], "no identity", sid)
    check("a request without x-client-id gets no context", sent is None, f"agent got {sent}")

    print("Isolation between API instances on the same Redis")
    sent, _ = ask(FLEX_1["finance"], "hr session on finance", sid)
    check("hr's session id is unknown to finance", sent is None, f"agent got {sent}")
    status, fsid = initialize(FLEX_2["finance"])
    check("finance issues its own session", status == 200 and fsid and fsid != sid, fsid)
    _, fctx = ask(FLEX_2["finance"], "finance first", fsid)
    sent, _ = ask(FLEX_1["finance"], "finance second", fsid)
    check("finance continues across replicas", sent == fctx, f"agent got {sent}")
    sent, _ = ask(FLEX_1["hr"], "finance session on hr", fsid, "alice")
    check("finance's session id is unknown to hr", sent is None, f"agent got {sent}")

    print("What is stored in Redis")
    keys = [k for k in redis("--scan") if "mcp-a2a-sessions" in k]
    # Flex keeps one index key per namespace (no TTL, managed by the gateway); every other key
    # is a session entry whose last segment is the policy's SHA-256 session key.
    entries = [k for k in keys if len(k.rsplit(":", 1)[-1]) == 64]
    indexes = [k for k in keys if k not in entries]
    check("session entries are in Redis", len(entries) >= 2, f"{len(entries)} entries, {len(indexes)} index keys")
    for k in keys:
        print(f"         {k}")
    check("no key contains a session id", not any(s in k for k in keys for s in (sid, fsid)))
    check("no key contains the client identity", not any("alice" in k for k in keys))
    ttls = [int(redis("PTTL", k)[0]) for k in entries]
    check("every session entry expires within sessionTtlSeconds", all(0 < t <= TTL_MILLIS for t in ttls), f"{min(ttls)}-{max(ttls)} ms")
    for k in indexes:
        print(f"         index {k.rsplit(':', 1)[-1]}: type {redis('TYPE', k)[0]}, TTL {redis('PTTL', k)[0]}")
    namespaces = {k.rsplit(":", 1)[0] for k in entries}
    check("hr and finance use different namespaces", len(namespaces) >= 2, f"{len(namespaces)} namespaces")

    print("Restart survival")
    subprocess.run(["docker", "compose", "restart", "flex-1"], check=True, capture_output=True)
    wait_ready(FLEX_1.values())
    sent, _ = ask(FLEX_1["hr"], "after restart", sid, "alice")
    check("the session survives a flex-1 restart", sent == ctx, f"agent got {sent}")

    print("Delete across replicas")
    status, _, _ = http("DELETE", FLEX_2["hr"], headers=headers(sid, "bob"))
    check("another client cannot delete the session", status == 404, str(status))
    status, _, _ = http("DELETE", FLEX_2["hr"], headers=headers(sid, "alice"))
    check("the owner deletes the session on flex-2", status == 204, str(status))
    sent, _ = ask(FLEX_1["hr"], "after delete", sid, "alice")
    check("flex-1 no longer uses the deleted session", sent is None, f"agent got {sent}")

    print("All shared-storage checks passed.")


main()
