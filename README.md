# MCP A2A Bridge Policy

A custom [Omni Gateway (Flex Gateway)](https://docs.mulesoft.com/gateway/latest/) policy, built with the [PDK](https://docs.mulesoft.com/pdk/latest/), that puts an MCP front on an A2A agent: MCP clients see it as a server with one tool. The agent can be a single A2A agent or a broker such as MuleSoft Agent Broker, and needs no changes for client-managed conversations. MCP-only clients such as ChatGPT, Claude and Cursor can then use it.

It is a **single-tool adapter**, not a full MCP↔A2A protocol translation: each `tools/call` becomes one A2A `SendMessage`. Polling, cancel, push notifications, streaming delivery and authentication flows are not mapped. MCP elicitation is an experimental opt-in, off by default. See [Limitations](docs/MCP-A2A-Bridge-Policy.md#8-limitations).

| | |
|---|---|
| Exchange assets | `mcp-a2a-bridge-policy` (definition), `mcp-atoa-bridge-policy-flex` (implementation) |
| Version | 1.4.0 |
| Category | Transformation |
| API instance types | MCP, A2A (v0.3), A2A (v1.0) |
| PDK | 1.10.0 |
| Flex Gateway | 1.11.0 or later (1.10.x can't load 1.1.0 or later) |

**Release 1.4.0, 2026-11-17 (issue #16):** adds optional Agent Card-derived discovery metadata (`metadataMode: agent-card`); `manual` remains the default and unchanged. Verified with 161 unit tests and one Docker integration test (`make test`), 46 shared-storage and official-SDK checks (`make test-shared-storage`), WASM build and definition validation, on a managed Omni Gateway (agent-card and manual instances) and on a Connected-mode Flex Gateway 1.11.0 (card change, malformed card, outage, stale bound and recovery). See [the verification](docs/P4A-Issue-Resolutions.md#issue-16-agent-card-derived-mcp-discovery).

**PDK upgrade verification, 2026-11-16:** PDK, pdk-unit, pdk-test and cargo-anypoint target stable 1.10.0. The upgraded worktree passes the WASM build, 146 unit tests, one Docker integration test, all 46 shared-storage/SDK checks, definition validation, formatting and host/WASM clippy. Policy version 1.3.3 and minimum Flex runtime 1.11.0 are unchanged; the upgrade is not a new published release.

**Integrated verification, 2026-11-16:** the PR15 tree combining commits `9226547` + `dc7e00d` passed `make test` with 146 unit tests and one Docker integration test, and `make test-shared-storage` with 46 checks (32 shared-storage, 12 official Python SDK, 2 official TypeScript SDK; log `pr15-integrated-shared-storage.log`). Definition validation, formatting, host clippy and WASM clippy passed. The fixtures default to Flex 1.11.0, Python MCP SDK 2.3.0 and TypeScript SDK 1.32.2. Managed-gateway testing and live configuration migration were not run. The commit pair identifies the verified integrated tree before the final merge commit; this is not a publication or server-side PR merge claim. The version above identifies the original release. See [the evidence and boundaries](docs/P4A-Issue-Resolutions.md#nov-16-follow-up-2026-11-16).

**PR14 history, verified 2026-11-16:** its Nov 16 worktree based on source HEAD `e1b5d0921c158922abc0d71e057ca46fbb7cc18c` passed `make build`, formatting, clippy, 70 unit tests and one Docker integration test on Flex 1.11.0. The prior-session shared-storage run passed all 18 checks on two Flex 1.11.0 replicas with Redis. These verify the separate PR14 worktree, not integrated PR15. Managed-gateway testing and live configuration migration were not run. See [verification details](docs/P4A-Issue-Resolutions.md#nov-16-follow-up-verification).

## What it does

| MCP client sends | Policy does |
|---|---|
| `initialize`, `ping`, `tools/list`, notifications | Answers them at the gateway (notifications get `202`); nothing reaches the broker. Invalid JSON-RPC, and unsupported `MCP-Protocol-Version` headers (`400`), are rejected before anything reaches the broker |
| `tools/call` for the bridge tool (default `ask_agent`) | Converts the call into an A2A `SendMessage` (v1.0) or `message/send` (v0.3), and converts the A2A task or message reply back into an MCP `CallToolResult`. A reply that isn't a valid answer to that request becomes a tool error |
| Native A2A traffic (only with `passThroughA2A: true`): JSON-RPC v1.0/v0.3, HTTP+JSON (`/message:send`, `/tasks/{id}:cancel`, ...), agent card, gRPC | Recognises it before any MCP handling and forwards it unchanged (same path, headers and body) |

**Multi-turn conversations.** Every tool result carries `structuredContent.state`, plus `contextId` and `taskId` when the broker sent them, and a text hint saying how to continue.
- To continue a conversation, the client passes `context_id`.
- To answer an `input-required` question from the broker, the client passes `task_id`, plus `context_id` when one was returned. By default the client collects the answer from the user, and the policy never sends elicitation requests ([example](docs/MCP-A2A-Bridge-Policy.md#31-client-managed-follow-up-questions)). This is how ChatGPT works. Experimental `enableElicitation` additionally requires an elicitation-capable client, SSE acceptance and a trusted upstream's boolean `Task.metadata["p4a.elicitationSafe"]: true` for **every** question. Missing or false markers keep the question client-managed; the marker is an upstream safety assertion, not secret detection ([details](docs/MCP-A2A-Bridge-Policy.md#32-experimental-mcp-elicitation-enableelicitation)).
- `auth-required` is completed by the user outside the conversation. Never send secrets in a message.
- An optional session store (`local`, or `remote` through the gateway's shared storage, which managed Omni Gateways have built in and self-managed ones get from Redis) can remember the last context and pending task for each `Mcp-Session-Id`. This helps clients that never send IDs back. Session IDs are random, issued at `initialize`, scoped to the API instance and policy, and bound to `sessionKeyHeader` when it is set. An unknown, expired, ended or other caller's session ID gets `404`, so the client starts again with `initialize`.

**Session upgrade and retention (PR #15).** Continuation, generation and pending elicitation now share one CAS-protected `mcp-a2a-sessions-v2` entry. Upgrades from the old namespace, or changes to `sessionTtlSeconds` or `enableElicitation` that change the retention namespace, require clients to `initialize` again. Logical idle expiry is `sessionTtlSeconds`; an open question retains state through its deadline plus that TTL for replay. With elicitation enabled, remote physical retention is TTL + 1800 seconds, not a longer idle lifetime. Completed streams are retained for one TTL; a new question during that window stays client-managed to preserve the old result. Once an answer is accepted, the input deadline no longer times it out; the upstream response timeout applies. `DELETE` returns `204` only after storing the termination tombstone, or `503` on a failed write (retry it).

**Persistence failures.** Session transitions use bounded CAS retries. An elicitation answer claim or result settlement that still needs persistence after retries are exhausted returns `503`, not a success acknowledgement. Do not blindly resend an accepted answer: it may already have reached the agent. `Answering` has a separate 1800-second processing/recovery deadline from acceptance; resuming an unresolved stream after that deadline attempts to store a tool error instructing the client to start a new call, not resend the accepted answer. Failure to persist that recovery also returns `503`.

**Size limit and encoding.** Converted answers always fit the gateway's 1 MB body limit. The policy drops duplicated data, truncates the text, or as a last resort returns a tool error, so the client never gets the raw A2A reply. Rewritten responses remove `Content-Encoding`; the bridge does not decompress compressed upstream bodies, so configure the upstream to return uncompressed JSON/SSE. Native pass-through is unchanged.

## Deployment modes

| Mode | Where the policy goes | Key settings | Client URL |
|---|---|---|---|
| **1. Dedicated MCP proxy (recommended)** | A separate MCP API instance next to the broker's A2A proxy, pointing at the same broker | `passThroughA2A: false`, `a2aPath: "/"` | `https://<gateway-host>/<mcp-base-path>/` |
| **2. Shared A2A proxy** | Directly on the broker's A2A API instance | `passThroughA2A: true`, `a2aPath: ""` | `https://<gateway-host>/<a2a-base-path>/` (MCP and A2A clients) |

On an MCP instance (Mode 1), apply MuleSoft's **MCP Support** policy **first**, as MuleSoft requires; it isn't available in Local Mode, so local tests omit it. Put the bridge **after** authentication policies (Client ID enforcement, or OIDC with OAuth 2.0 Protected Resource Metadata). The bridge answers `initialize` and `tools/list` itself, so policies placed after it never run for those requests. Set the response timeout to about 120 s, because multi-agent answers can take 30–60 s.

To report a security issue, see [SECURITY.md](SECURITY.md). Fixes in 1.1.0 and 1.3.0 for the P4A review findings are tracked in [docs/P4A-Issue-Resolutions.md](docs/P4A-Issue-Resolutions.md).

The full guide, with diagrams, every property, policy-order tables, limitations and a verification checklist, is in [docs/MCP-A2A-Bridge-Policy.md](docs/MCP-A2A-Bridge-Policy.md).

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
| `includeStructuredContent` | `true` | Add `state`, `contextId`, `taskId` (when present) and data parts to tool results |
| `sessionStoreMode` | `disabled` | `disabled`, `local` or `remote`. Use `remote` on managed gateways (always 2+ replicas) and with elicitation |
| `sessionTtlSeconds` | `1800` | Logical idle TTL and completed-stream replay window (60–86400); changing retention requires reinitialization |
| `sessionKeyHeader` | empty | Header hashed into the session key, for example `client_id`. Enforced: without it, a request gets no session |
| `enableElicitation` | `false` | Experimental: elicit only upstream-marked safe `input-required` questions. Needs a session store; leave off for ChatGPT |
| `elicitationTimeoutSeconds` | `300` | Time to answer each elicited question (30–1800, independent of idle TTL); not an upstream-response timeout |
| `maxElicitationRounds` | `3` | Elicited questions per call (1–10); more come back as a normal tool result |
| `metadataMode` | `manual` | `manual` or `agent-card`: derive the tool description and `serverInfo.title` from the upstream Agent Card ([details](docs/MCP-A2A-Bridge-Policy.md#54-discovery-metadata-from-the-agent-card-metadatamode)) |
| `agentCardUrl` | empty | Agent Card URL on the agent itself, used with `agent-card` |
| `agentCardAuthorization` | empty | Optional `Authorization` header for an authenticated card (sensitive) |
| `agentCardRefreshSeconds` | `300` | Card refresh period per worker (30–86400); failures retry after 30 s |
| `agentCardMaxStaleSeconds` | `3600` | Extra time a cached card may be used while refreshes fail (0–604800); then manual metadata |
| `agentCardTimeoutSeconds` | `5` | Timeout of one card fetch (1–30) |

Sessions share the v2 retention namespace only across replicas with the same API/policy scope
and effective retention configuration (`sessionTtlSeconds` plus the `enableElicitation` retention
allowance). A retention-changing update selects a new namespace and requires `initialize` again.
Old entries are not migrated; returning to an earlier retention namespace can expose old state
only while it remains logically live. This is not seamless session preservation.
Session `DELETE` returns `204` only after a conditional tombstone write succeeds, `404` for an
unknown/expired/terminated session or the wrong caller, and `503` if termination cannot be
confirmed (including a concurrent update). Retry `DELETE` after `503`.

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

This is a PDK 1.10 single-project (unified) layout: the definition and implementation are packaged together. Use the approved P4A workflow to publish or release the policy.

## Build and test

Prerequisites:
- Anypoint CLI v4 with `anypoint-pdk-plugin`
- Rust with the `wasm32-wasip1` target
- Python 3.11 or later, for the build's runtime-version check
- Docker, for the playground and integration tests

```bash
make setup        # once
make build        # builds the WASM binary; fails if metadata understates the Flex version needed
cargo test --lib  # unit tests, no Docker needed
make test         # integration tests (Docker); needs a local-mode tests/config/registration.yaml
make test-shared-storage  # two Flex replicas + Redis session test; REGISTRATION=<local-mode registration.yaml>
                          # then the official MCP Python and TypeScript SDKs as clients (needs PyPI and npm; SKIP_SDK=1 skips them)
make run          # local playground; needs playground/config/registration.yaml (gitignored)
make validate-definition  # Exchange metadata check; also run by make publish / make release
```

## Publish to Anypoint Exchange

1. In `Cargo.toml`, set `group_id` to your business group ID and set `version`. A released version can't be overwritten, so bump it for every change.
2. Authenticate with a connected app that has Exchange Contributor permission (`ANYPOINT_CLIENT_ID`, `ANYPOINT_CLIENT_SECRET`, `ANYPOINT_ORG`).
3. Release. `make release` and `make publish` first run `scripts/validate-definition.py`, which checks the `assetTypes` tokens against Exchange's allow-list (the A2A v1 token is `a2a_v1`) and the description length limit:

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
