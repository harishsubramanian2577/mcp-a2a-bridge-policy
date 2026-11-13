# Copyright 2026 Salesforce, Inc. All rights reserved.
"""Checks session continuity and isolation across two Flex replicas that share Redis.

Run by run.sh after `docker compose up`. Exits non-zero on the first failed check.
"""
import json
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

FLEX_1 = {"hr": "http://localhost:18081/hr/", "finance": "http://localhost:18082/finance/",
          "support": "http://localhost:18083/support/"}
FLEX_2 = {"hr": "http://localhost:28081/hr/", "finance": "http://localhost:28082/finance/",
          "support": "http://localhost:28083/support/"}
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


def initialize(url, client=None, capabilities=None):
    status, h, _ = http("POST", url, {"jsonrpc": "2.0", "id": 0, "method": "initialize",
                                      "params": {"protocolVersion": "2025-06-18", "capabilities": capabilities or {},
                                                 "clientInfo": {"name": "check", "version": "1"}}},
                          headers(client=client))
    sid = {k.lower(): v for k, v in h.items()}.get("mcp-session-id")
    return status, sid


def ask(url, message, session=None, client=None):
    """tools/call; returns (contextId the agent received, contextId in the MCP result)."""
    http("DELETE", AGENT)
    status, _, body = tool_call(url, message, session, client)
    if status != 200 or body is None or "structuredContent" not in body.get("result", {}):
        check(f"tools/call {message!r} answered", False, f"status {status}, body {body}")
    _, _, received = http("GET", AGENT)
    sent = received[-1]["message"].get("contextId") if received else None
    return sent, body["result"]["structuredContent"]["contextId"]


def raw(method, url, body=None, headers=None):
    """Like http(), but returns the body as text, for SSE responses."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={
        "content-type": "application/json", "accept": "application/json, text/event-stream", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.headers.get("content-type", ""), resp.read().decode()
    except urllib.error.HTTPError as err:
        return err.code, err.headers.get("content-type", ""), ""


def sse_events(text):
    """[(event id, JSON data)] of the events that carry data."""
    events = []
    for block in text.split("\n\n"):
        lines = block.splitlines()
        data = [l[6:] for l in lines if l.startswith("data: ")]
        if data:
            event_id = next((l[4:] for l in lines if l.startswith("id: ")), None)
            events.append((event_id, json.loads("".join(data))))
    return events


def mcp(session, client):
    return {**headers(session, client), "mcp-protocol-version": "2025-06-18"}


def elicitation_checks():
    url_1, url_2 = FLEX_1["support"], FLEX_2["support"]
    _, sid = initialize(url_1, "alice", {"elicitation": {}})
    http("DELETE", AGENT)
    status, ctype, text = raw("POST", url_1, {"jsonrpc": "2.0", "id": "call-1", "method": "tools/call",
                                             "params": {"name": "ask_agent", "arguments": {"message": "ask me"}}},
                              mcp(sid, "alice"))
    events = sse_events(text)
    question = events[0][1] if events else {}
    check("the question arrives as elicitation/create on SSE", status == 200 and ctype.startswith("text/event-stream")
          and question.get("method") == "elicitation/create" and "retry: " in text, f"{status} {ctype}")
    event_id = events[0][0]

    status, _, text = raw("GET", url_2, headers={**mcp(sid, "alice"), "last-event-id": event_id})
    check("flex-2 resumes the stream and waits for the answer", status == 200 and not sse_events(text) and "retry: " in text, str(status))
    status, _, _ = raw("POST", url_2, {"jsonrpc": "2.0", "id": question["id"], "result": {"action": "accept", "content": {"answer": "Rome"}}},
                       mcp(sid, "bob"))
    check("another client cannot answer", status == 404, str(status))

    # The same answer sent to both replicas at once reaches the agent once.
    answer = {"jsonrpc": "2.0", "id": question["id"], "result": {"action": "accept", "content": {"answer": "Paris"}}}
    results = []
    workers = [threading.Thread(target=lambda u=u: results.append(raw("POST", u, answer, mcp(sid, "alice"))[0])) for u in (url_1, url_2)]
    for w in workers:
        w.start()
    for w in workers:
        w.join()
    check("both replicas acknowledge the answer", results == [202, 202], str(results))
    _, _, received = http("GET", AGENT)
    answers = [r["message"] for r in received if r["message"].get("taskId")]
    check("the answer reaches the agent once, on the same task and context", len(answers) == 1
          and answers[0]["parts"][0]["text"] == "Paris" and answers[0].get("contextId", "").startswith("ctx-"),
          f"{len(answers)} answers")

    status, _, text = raw("GET", url_2, headers={**mcp(sid, "alice"), "last-event-id": event_id})
    events = sse_events(text)
    result = events[0][1] if events else {}
    check("the result arrives on flex-2 under the original call id", status == 200 and result.get("id") == "call-1"
          and result.get("result", {}).get("content", [{}])[0].get("text") == "echo: Paris", str(result)[:120])

    print("Elicitation decline")
    status, _, text = raw("POST", url_1, {"jsonrpc": "2.0", "id": "call-2", "method": "tools/call",
                                         "params": {"name": "ask_agent", "arguments": {"message": "ask again"}}},
                          mcp(sid, "alice"))
    event_id, question = sse_events(text)[0]
    status, _, _ = raw("POST", url_2, {"jsonrpc": "2.0", "id": question["id"], "result": {"action": "decline"}}, mcp(sid, "alice"))
    _, _, text = raw("GET", url_1, headers={**mcp(sid, "alice"), "last-event-id": event_id})
    ended = sse_events(text)[0][1]["result"]["structuredContent"]
    check("declining ends the conversation", status == 202 and ended == {"state": "ended", "reason": "declined"}, str(ended))
    http("DELETE", AGENT)
    raw("POST", url_2, {"jsonrpc": "2.0", "id": "call-3", "method": "tools/call",
                        "params": {"name": "ask_agent", "arguments": {"message": "fresh"}}}, mcp(sid, "alice"))
    _, _, received = http("GET", AGENT)
    sent = received[-1]["message"].get("contextId") if received else "no call"
    check("the next call starts a new context", sent is None, f"agent got {sent}")

    print("Client-managed fallback on the same API (ChatGPT-style client)")
    _, csid = initialize(url_1, "carol")
    status, _, body = tool_call(url_2, "ask me", csid, "carol")
    structured = (body or {}).get("result", {}).get("structuredContent", {})
    check("a client without the capability gets a JSON tool result with the ids", status == 200
          and structured.get("state") == "input-required" and structured.get("taskId"), str(structured))


def tool_call(url, message, session=None, client=None):
    global calls
    calls += 1
    return http("POST", url, {"jsonrpc": "2.0", "id": calls, "method": "tools/call",
                              "params": {"name": "ask_agent", "arguments": {"message": message}}},
                headers(session, client))


def not_found(url, message, session=None, client=None):
    """tools/call that must get 404 without reaching the agent; returns a detail string."""
    http("DELETE", AGENT)
    status, _, _ = tool_call(url, message, session, client)
    _, _, received = http("GET", AGENT)
    return status == 404 and not received, f"{status}, agent called {len(received)} times"


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
    check("another client with the same session id gets 404", *not_found(FLEX_2["hr"], "as bob", sid, "bob"))
    check("a request without x-client-id gets 404", *not_found(FLEX_2["hr"], "no identity", sid))

    print("Isolation between API instances on the same Redis")
    check("hr's session id is unknown to finance", *not_found(FLEX_1["finance"], "hr session on finance", sid))
    status, fsid = initialize(FLEX_2["finance"])
    check("finance issues its own session", status == 200 and fsid and fsid != sid, fsid)
    _, fctx = ask(FLEX_2["finance"], "finance first", fsid)
    sent, _ = ask(FLEX_1["finance"], "finance second", fsid)
    check("finance continues across replicas", sent == fctx, f"agent got {sent}")
    check("finance's session id is unknown to hr", *not_found(FLEX_1["hr"], "finance session on hr", fsid, "alice"))

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
    check("flex-1 returns 404 for the deleted session", *not_found(FLEX_1["hr"], "after delete", sid, "alice"))

    print("Late answer after delete")
    _, lsid = initialize(FLEX_1["hr"], "alice")
    in_flight = {}
    worker = threading.Thread(target=lambda: in_flight.update(
        zip(("status", "headers", "body"), tool_call(FLEX_1["hr"], "slow answer", lsid, "alice"))))
    worker.start()
    time.sleep(1.5)
    status, _, _ = http("DELETE", FLEX_2["hr"], headers=headers(lsid, "alice"))
    check("the owner deletes the session while a call is in flight", status == 204, str(status))
    worker.join()
    check("the in-flight call still gets its answer", in_flight.get("status") == 200, str(in_flight.get("status")))
    check("the late answer does not revive the session", *not_found(FLEX_2["hr"], "after late answer", lsid, "alice"))

    print("Elicitation across replicas (support, experimental)")
    elicitation_checks()

    print("All shared-storage checks passed.")


main()
