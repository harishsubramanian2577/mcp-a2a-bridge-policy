# Copyright 2026 Salesforce, Inc. All rights reserved.
"""End-to-end checks with the official MCP Python SDK as the client.

check.py drives the policy with hand-built requests; this script lets a shipping MCP client do it,
so the SDK's own handshake, Streamable HTTP transport, elicitation handling and SSE resumption
(GET with Last-Event-ID after the question stream ends) meet the policy unchanged. Run by run.sh
in a container on the compose network, after check.py.
"""
import asyncio
import json
import sys
import urllib.request

from mcp import Client, types
from mcp.client.streamable_http import streamable_http_client
from mcp.shared._httpx_utils import create_mcp_http_client

SUPPORT = "http://flex-1:8083/support/"
AGENT = "http://agent:8080/requests"
TIMEOUT = 60


def check(name, condition, detail=""):
    print(f"  {'PASS' if condition else 'FAIL'}  {name}{f'  ({detail})' if detail else ''}")
    if not condition:
        sys.exit(1)


def agent(method="GET"):
    with urllib.request.urlopen(urllib.request.Request(AGENT, method=method), timeout=10) as resp:
        return json.loads(resp.read())


def client(identity, elicitation_callback=None):
    transport = streamable_http_client(
        SUPPORT, http_client=create_mcp_http_client(headers={"x-client-id": identity}))
    return Client(transport, mode="legacy", elicitation_callback=elicitation_callback,
                  read_timeout_seconds=TIMEOUT, cache=None)


def text(result):
    return result.content[0].text if result.content else ""


async def client_managed():
    print("SDK client without elicitation (ChatGPT-style)")
    async with client("sdk-plain") as c:
        check("the SDK negotiates a supported revision", c.protocol_version == "2025-06-18", c.protocol_version)
        tools = await c.list_tools()
        check("tools/list advertises the agent tool", [t.name for t in tools.tools] == ["ask_agent"])
        agent("DELETE")
        result = await c.call_tool("ask_agent", {"message": "ask me"})
        ids = result.structured_content or {}
        check("the question comes back as a tool result with the ids", not result.is_error
              and ids.get("state") == "input-required" and ids.get("taskId") and ids.get("contextId"), str(ids))
        result = await c.call_tool("ask_agent", {"message": "Rome", "context_id": ids["contextId"],
                                                 "task_id": ids["taskId"]})
        sent = agent()[-1]["message"]
        check("passing the ids back continues the task", text(result) == "echo: Rome"
              and sent.get("taskId") == ids["taskId"] and sent.get("contextId") == ids["contextId"], text(result))


async def elicitation_accept():
    print("SDK client with elicitation: accept")
    asked = []

    async def answer(_context, params):
        asked.append(params)
        return types.ElicitResult(action="accept", content={"answer": "Paris"})

    async with client("sdk-elicit", answer) as c:
        agent("DELETE")
        result = await c.call_tool("ask_agent", {"message": "ask me"})
        check("the SDK receives the question as elicitation/create", len(asked) == 1
              and asked[0].message == "Which city?", f"{len(asked)} questions")
        schema = getattr(asked[0], "requested_schema", None) or {}
        check("the form asks for one answer field", "answer" in schema.get("properties", {}), str(schema)[:120])
        check("the call resumes and returns the agent's final answer", not result.is_error
              and text(result) == "echo: Paris", text(result)[:120])
        answers = [r["message"] for r in agent() if r["message"].get("taskId")]
        check("the answer reaches the agent once, on the asking task", len(answers) == 1
              and answers[0]["parts"][0]["text"] == "Paris" and answers[0].get("contextId"), f"{len(answers)} answers")
        context = answers[0]["contextId"]
        agent("DELETE")
        result = await c.call_tool("ask_agent", {"message": "thanks"})
        sent = agent()[-1]["message"]
        check("the next call continues the same context", sent.get("contextId") == context
              and not sent.get("taskId"), f"agent got {sent.get('contextId')}")


async def elicitation_slow_answer():
    print("SDK client with elicitation: answer after the client has resumed")

    async def answer(_context, _params):
        # Longer than the retry hint, so the client resumes while the question is still open.
        await asyncio.sleep(5)
        return types.ElicitResult(action="accept", content={"answer": "Oslo"})

    async with client("sdk-slow", answer) as c:
        result = await c.call_tool("ask_agent", {"message": "ask me"})
        check("a resumed client that is still waiting gets the result", not result.is_error
              and text(result) == "echo: Oslo", text(result)[:120])


async def elicitation_decline():
    print("SDK client with elicitation: decline")

    async def decline(_context, _params):
        return types.ElicitResult(action="decline")

    async with client("sdk-decline", decline) as c:
        result = await c.call_tool("ask_agent", {"message": "ask me"})
        ended = result.structured_content or {}
        check("declining ends the conversation", ended == {"state": "ended", "reason": "declined"}, str(ended))
        agent("DELETE")
        await c.call_tool("ask_agent", {"message": "fresh"})
        sent = agent()[-1]["message"]
        check("the next call starts a new context", not sent.get("contextId") and not sent.get("taskId"),
              f"agent got {sent.get('contextId')}")


async def main():
    await client_managed()
    await elicitation_accept()
    await elicitation_slow_answer()
    await elicitation_decline()
    print("All MCP SDK checks passed.")


asyncio.run(asyncio.wait_for(main(), 4 * TIMEOUT))
