# MCP A2A Bridge Policy

A custom [Omni Gateway (Flex Gateway)](https://docs.mulesoft.com/gateway/latest/) policy, built with the [PDK](https://docs.mulesoft.com/pdk/latest/), that exposes an A2A agent broker as an MCP server with a single tool. MCP-only clients such as ChatGPT, Claude and Cursor can then use a MuleSoft Agent Broker without any change to the broker.

| | |
|---|---|
| Exchange assets | `mcp-a2a-bridge-policy` (definition), `mcp-atoa-bridge-policy-flex` (implementation) |
| Version | 1.1.0 |
| Category | Transformation |
| API instance types | MCP, A2A (v0.3), A2A (v1.0) |
| PDK | 1.8.0 |

## What it does

| MCP client sends | Policy does |
|---|---|
| `initialize`, `ping`, `tools/list`, notifications | Answers them at the gateway; nothing reaches the broker |
| `tools/call` for the bridge tool (default `ask_agent`) | Converts the call into an A2A `SendMessage` (v1.0) or `message/send` (v0.3), and converts the A2A task or message reply back into an MCP `CallToolResult` |
| Native A2A traffic (only with `passThroughA2A: true`): JSON-RPC v1.0/v0.3, HTTP+JSON (`/message:send`, `/tasks/{id}:cancel`, ...), agent card, gRPC | Recognises it before any MCP handling and forwards it unchanged (same path, headers and body) |

**Multi-turn conversations.** Every tool result carries `structuredContent.{state, contextId, taskId}`.
- To continue a conversation, the client passes `context_id`.
- To answer an `input-required` question from the broker, the client passes `context_id` and `task_id`.
- An optional session store (`local`, or `remote` via Redis) can remember the last context for each `Mcp-Session-Id`. This helps clients that never send `context_id` back. Session IDs are random, issued at `initialize`, scoped to the API instance and policy, and bound to `sessionKeyHeader` when it is set.

**Size limit.** Converted answers always fit the gateway's 1 MB body limit. The policy drops duplicated data, truncates the text, or as a last resort returns a tool error, so the client never gets the raw A2A reply.

## Deployment modes

| Mode | Where the policy goes | Key settings | Client URL |
|---|---|---|---|
| **1. Dedicated MCP proxy (recommended)** | A separate MCP API instance next to the broker's A2A proxy, pointing at the same broker | `passThroughA2A: false`, `a2aPath: "/"` | `https://<gateway-host>/<mcp-base-path>/` |
| **2. Shared A2A proxy** | Directly on the broker's A2A API instance | `passThroughA2A: true`, `a2aPath: ""` | `https://<gateway-host>/<a2a-base-path>/` (MCP and A2A clients) |

Put the bridge **after** authentication policies (Client ID enforcement, or OIDC with OAuth 2.0 Protected Resource Metadata). The bridge answers `initialize` and `tools/list` itself, so policies placed after it never run for those requests. Set the response timeout to about 120 s, because multi-agent answers can take 30–60 s.

To report a security issue, see [SECURITY.md](SECURITY.md). Fixes in 1.1.0 for the P4A review findings are tracked in [docs/P4A-Issue-Resolutions.md](docs/P4A-Issue-Resolutions.md).

The full guide, with diagrams, every property and policy-order tables, is in [docs/MCP-A2A-Bridge-Policy.md](docs/MCP-A2A-Bridge-Policy.md).

## Configuration

| Property | Default | Purpose |
|---|---|---|
| `toolName` | `ask_agent` | Name of the MCP tool (required) |
| `toolDescription` | Generic text | What the client's model reads to decide when and how to call the tool |
| `serverName` | `mcp-a2a-bridge` | `serverInfo.name` in the `initialize` reply |
| `serverInstructions` | empty | Optional `instructions` in the `initialize` reply |
| `a2aProtocolVersion` | `1.0` | `1.0` or `0.3` (required) |
| `passThroughA2A` | `false` | Forward native A2A traffic (Mode 2) |
| `a2aPath` | empty | Path, relative to the upstream URL, that converted calls are sent to (native A2A traffic is never rewritten) |
| `includeStructuredContent` | `true` | Add `state`, `contextId`, `taskId` and data parts to tool results |
| `sessionStoreMode` | `disabled` | `disabled`, `local` or `remote` |
| `sessionTtlSeconds` | `1800` | Session store TTL (60–86400) |
| `sessionKeyHeader` | empty | Header hashed into the session key, for example `client_id`. Enforced: without it, a request gets no session |

## Repository layout

```
.project.yaml      PDK project descriptor (unified layout)
Makefile           setup, build, test, run, publish, release
Cargo.toml         Crate and Exchange coordinates ([package.metadata.anypoint])
definition/        Policy definition: gcl.yaml (configuration schema)
src/               Policy implementation (Rust/WASM) and unit tests
tests/             Integration tests (Docker)
playground/        Local Flex Gateway playground
mcp-schema/        MCP tool metadata, for registering the proxy as an MCP asset in Exchange
postman/           Postman collection for MCP calls with context_id/task_id follow-ups
docs/              Customer guide (Markdown, Word, PDF)
```

This is a PDK 1.8 single-project (unified) layout: one `make release` publishes both the definition and the implementation.

## Build and test

Prerequisites:
- Anypoint CLI v4 with `anypoint-pdk-plugin`
- Rust with the `wasm32-wasip1` target
- Docker, for the playground and integration tests

```bash
make setup        # once
make build        # builds the WASM binary
cargo test --lib  # unit tests, no Docker needed
make test         # integration tests (Docker); needs a local-mode tests/config/registration.yaml
make test-shared-storage  # two Flex replicas + Redis session test; REGISTRATION=<local-mode registration.yaml>
make run          # local playground; needs playground/config/registration.yaml (gitignored)
```

## Publish to Anypoint Exchange

1. In `Cargo.toml`, set `group_id` to your business group ID and set `version`. A released version can't be overwritten, so bump it for every change.
2. Authenticate with a connected app that has Exchange Contributor permission (`ANYPOINT_CLIENT_ID`, `ANYPOINT_CLIENT_SECRET`, `ANYPOINT_ORG`).
3. Release:

```bash
make release   # or: make publish  (-DEV version for testing)
```

> The implementation asset ID is `mcp-atoa-bridge-policy-flex`, not `mcp-a2a-…`. PDK's `build-asset-files` kebab-cases the implementation ID and rejects any that change, and `a2a` becomes `a-2-a`.

## Quick test

```bash
URL=https://<gateway-host>/<mcp-base-path>/
H=(-H 'content-type: application/json' -H 'accept: application/json, text/event-stream')

curl -s "$URL" "${H[@]}" -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
curl -s "$URL" "${H[@]}" -d '{"jsonrpc":"2.0","id":2,"method":"tools/call",
  "params":{"name":"ask_agent","arguments":{"message":"Who heads Human Resources?"}}}'
```

Add the authentication header your proxy expects (for example `client_id`/`client_secret`, or `Authorization: Bearer <token>`).
