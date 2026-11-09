# MCP A2A Bridge Policy

A custom [Omni Gateway (Flex Gateway)](https://docs.mulesoft.com/gateway/latest/) policy, built with the [PDK](https://docs.mulesoft.com/pdk/latest/), that exposes an A2A agent broker as an MCP server with a single tool. MCP-only clients such as ChatGPT, Claude and Cursor can then use a MuleSoft Agent Broker without any change to the broker.

| | |
|---|---|
| Exchange assets | `mcp-a2a-bridge-policy` (definition), `mcp-a2a-bridge-policy-flex` (implementation) |
| Version | 1.0.3 |
| Category | Transformation |
| API instance types | MCP, A2A (v0.3), A2A (v1.0) |
| PDK | 1.8.0 |

## What it does

| MCP client sends | Policy does |
|---|---|
| `initialize`, `ping`, `tools/list`, notifications | Answers them at the gateway; nothing reaches the broker |
| `tools/call` for the bridge tool (default `ask_agent`) | Converts the call into an A2A `SendMessage` (v1.0) or `message/send` (v0.3), and converts the A2A task or message reply back into an MCP `CallToolResult` |
| Native A2A traffic (only with `passThroughA2A: true`) | Forwards it to the broker unchanged |

**Multi-turn conversations.** Every tool result carries `structuredContent.{state, contextId, taskId}`.
- To continue a conversation, the client passes `context_id`.
- To answer an `input-required` question from the broker, the client passes `context_id` and `task_id`.
- An optional session store (`local`, or `remote` via Redis) can remember the last context for each `Mcp-Session-Id`. This helps clients that never send `context_id` back.

## Deployment modes

| Mode | Where the policy goes | Key settings | Client URL |
|---|---|---|---|
| **1. Dedicated MCP proxy (recommended)** | A separate MCP API instance next to the broker's A2A proxy, pointing at the same broker | `passThroughA2A: false`, `a2aPath: "/"` | `https://<gateway-host>/<mcp-base-path>/` |
| **2. Shared A2A proxy** | Directly on the broker's A2A API instance | `passThroughA2A: true`, `a2aPath: ""` | `https://<gateway-host>/<a2a-base-path>/` (MCP and A2A clients) |

Put the bridge **after** authentication policies (Client ID enforcement, or OIDC with OAuth 2.0 Protected Resource Metadata). The bridge answers `initialize` and `tools/list` itself, so policies placed after it never run for those requests. Set the response timeout to about 120 s, because multi-agent answers can take 30–60 s.

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
| `a2aPath` | empty | Path, relative to the upstream URL, that converted calls are sent to |
| `includeStructuredContent` | `true` | Add `state`, `contextId`, `taskId` and data parts to tool results |
| `sessionStoreMode` | `disabled` | `disabled`, `local` or `remote` |
| `sessionTtlSeconds` | `1800` | Session store TTL (60–86400) |
| `sessionKeyHeader` | empty | Header hashed into the session key, for example `client_id` |

## Repository layout

```
mcp-a2a-bridge-policy-definition/   Policy definition: gcl.yaml (schema) and exchange.json
mcp-a2a-bridge-policy-flex/         Policy implementation (Rust/WASM): src/, unit and integration tests, playground
mcp-schema/                         MCP tool metadata, for registering the proxy as an MCP asset in Exchange
postman/                            Postman collection for MCP calls with context_id/task_id follow-ups
docs/                               Customer guide (Markdown, Word, PDF)
```

## Build and test

Prerequisites:
- Anypoint CLI v4 with `anypoint-pdk-plugin`
- Rust with the `wasm32-wasip1` target
- Docker, for the playground and integration tests

```bash
cd mcp-a2a-bridge-policy-flex
make setup        # once
make build        # builds the WASM binary
cargo test --lib  # unit tests, no Docker needed
make test         # integration tests (Docker)
make run          # local playground; needs playground/config/registration.yaml (gitignored)
```

## Publish to Anypoint Exchange

1. Set your business group ID and version. The version must match in all three places, and a released version can't be overwritten.
   - `mcp-a2a-bridge-policy-definition/exchange.json`: `groupId`, `version`.
   - `mcp-a2a-bridge-policy-flex/Cargo.toml`: `version`, `group_id`, `definition_asset_id.version`.
2. Authenticate with a connected app that has Exchange Contributor permission (`ANYPOINT_CLIENT_ID`, `ANYPOINT_CLIENT_SECRET`, `ANYPOINT_ORG`).
3. Release the definition first, then the implementation:

```bash
cd mcp-a2a-bridge-policy-definition && make release   # or: make publish  (-DEV version)
cd ../mcp-a2a-bridge-policy-flex   && make release   # or: make publish
```

## Quick test

```bash
URL=https://<gateway-host>/<mcp-base-path>/
H=(-H 'content-type: application/json' -H 'accept: application/json, text/event-stream')

curl -s "$URL" "${H[@]}" -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
curl -s "$URL" "${H[@]}" -d '{"jsonrpc":"2.0","id":2,"method":"tools/call",
  "params":{"name":"ask_agent","arguments":{"message":"Who heads Human Resources?"}}}'
```

Add the authentication header your proxy expects (for example `client_id`/`client_secret`, or `Authorization: Bearer <token>`).
