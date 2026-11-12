# P4A Review: Issue Resolutions

This document tracks the fixes for the P4A review findings on the MCP A2A Bridge Policy.

| | |
|---|---|
| Reviewed version | 1.0.3 (commit `3d61a41`) |
| Fixed in | 1.1.0, branch `fix/p4a-priority-issues` |
| Priority issues | [#1](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/1), [#4](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/4), [#12](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/12) |
| Test baseline | 32 unit tests on 1.0.3, 61 on 1.1.0, all passing |
| Minimum gateway | Flex Gateway 1.11.0 (1.1.0 uses PDK stop-iteration, which 1.10.x doesn't support) |

## Status

| Issue | Title | Status |
|---|---|---|
| #1 | Failed response rewrite returns raw A2A payload instead of an MCP error | Fixed in 1.1.0 |
| #4 | Classify native A2A before MCP dispatch and preserve native version semantics | Fixed in 1.1.0 |
| #12 | Session isolation, identity binding and token generation | Fixed in 1.1.0. Verified live on two Flex replicas sharing Redis. Private reporting is enabled. |

## How the fixes were verified

1. **Unit and filter tests.**
   - The tests drive the real policy entrypoint through the PDK 1.8.0 `pdk-unit` harness.
   - Command: `cargo test --lib`.
   - Result: 61 passed, 0 failed.
2. **Baseline check.**
   - The new filter-level regression tests were copied unchanged onto the 1.0.3 code (`main` at `3d61a41`) and run there.
   - 13 of the 14 failed, each with the bug the issue reports.
   - The 14th, `native_http_json_is_rejected_when_pass_through_is_off`, is a guard test. It is expected to pass on both versions.
   - This shows that the tests reproduce the findings and that 1.1.0 fixes them.
3. **WASM build.**
   - Command: `make build`.
   - Result: succeeded. The only new host import is WASI `random_get`, which Flex Gateway/Envoy provides.
4. **Integration tests.**
   - `make test` passes on Flex 1.11.0: 61 unit tests and the `tools_call_round_trip` integration test.
   - The first live run found a bug in the test itself. pdk-test puts the API path in the upstream `destinationPath`, so the gateway has no base path in local mode and `a2aPath` can't be applied. The test now calls the agent's path with `a2aPath` empty. The `a2aPath` rewrite is covered by unit tests.
   - `make test` needs a Flex registration: put a local-mode `registration.yaml` in `tests/config/` (gitignored), which is mounted into the gateway.
5. **Shared-storage test.** `make test-shared-storage` passes on Flex 1.11.0, 1.12.0 and 1.13.0 (see issue #12 below).
6. **Minimum gateway version.** The same harness on Flex 1.10.6 fails at load time with `FlexAbiHandler: command 'flex_enable_stop_iteration' unknown`. The request filter needs PDK's stop-iteration feature (the #4 fix), so 1.1.0 requires Flex Gateway 1.11.0 or later. The README and customer guide say so, and both integration tests and the playground are pinned to 1.11.0.
7. **Published runtime metadata.**
   - PDK's `build-asset-files` always writes `minRuntimeVersion: 1.6.1` into `target/implementation/metadata.yaml` and `target/implementation-dev/metadata.yaml`, and `publish`/`release` upload those files as they are.
   - The Makefile pins `MIN_RUNTIME_VERSION := 1.11.0`. Right after `build-asset-files`, `scripts/check-min-runtime.py --write` sets that version in both files.
   - The same script then fails the build if the pin, either metadata file, or the playground image is below the version the enabled `pdk` features need. It reads the features from `Cargo.toml`; `enable_stop_iteration` needs 1.11.0. Because `publish` and `release` depend on `build`, they can't upload understated metadata.
   - Verified from a clean build (`rm -rf target && make build`): both files say `minRuntimeVersion: 1.11.0`. Lowering the pin (`make build MIN_RUNTIME_VERSION=1.6.1`), putting 1.6.1 back in a metadata file, or setting the playground to 1.10.0 each makes the check fail.

---

## Issue #1: Failed response rewrite returns raw A2A payload

### Problem
A 600,000-character data part is under the input limit. Conversion copies it into both `content[].text` and `structuredContent.data`, so the converted body grows to about 1.2 MB. PDK 1.8 rejects any body of 1 MiB or more (`BodyError::ExceededBodySize`).

The policy logged the error, but it had already changed the headers to `200` / `application/json`. As a result:
- The client received the raw A2A `result.task`, or raw SSE, as if it were an MCP response.
- The session store had already been updated before delivery.

### Root cause
- `response_filter` called `set_body` with no size check and ignored the result apart from logging it.
- `remember()` ran before the write.

### Resolution steps
- [x] Added `mcp::bounded_result_envelope`. It serialises the result under `MAX_BODY_BYTES` (1,000,000), shrinking in this order:
  1. Drop `structuredContent.data`, which is a duplicate of the text.
  2. Truncate the answer text by the overflow plus a margin, cut on a UTF-8 character boundary, and add a truncation note. Removing k raw bytes removes at least k serialised bytes, so one pass is enough.
  3. Replace the result with a small tool error.
- [x] Added `mcp::deliver_result`. If the write still fails, it writes a minimal tool-error envelope, so the original A2A body is never released.
- [x] Moved the session update so it runs only after a successful delivery of the real answer (full or reduced), never after a replacement or a failure.
- [x] On the request side, a generated A2A request that would exceed the limit now gets `-32602` before any rewrite. Previously it got a `-32603` after a failed write.

### Files
`src/mcp.rs`, `src/lib.rs` (`response_filter`, `transcode_tool_call`).

### Tests
| Test | What it proves | 1.0.3 | 1.1.0 |
|---|---|---|---|
| `tests::large_data_part_json_reply_becomes_a_bounded_mcp_result` | Reproduction from the issue: 600,000-character data part over JSON | Fails: the client got raw A2A (`id: "broker"`) | Passes |
| `tests::large_sse_reply_is_never_released_as_raw_sse` | Same over SSE, with escaped-text expansion | Fails: raw SSE was released | Passes |
| `tests::undeliverable_answer_does_not_update_the_session` | No session update after an unsuccessful delivery | Fails | Passes |
| `tests::oversized_outbound_message_is_rejected_before_rewriting` | Request-side bound | Fails (`-32603`) | Passes (`-32602`) |
| `mcp::tests::duplicated_data_part_is_dropped_from_structured_content_first` | Shrink step 1 | n/a | Passes |
| `mcp::tests::escaped_text_expansion_is_truncated_under_the_limit` | Shrink step 2 | n/a | Passes |
| `mcp::tests::multibyte_text_is_cut_on_a_char_boundary` | UTF-8 safety | n/a | Passes |
| `mcp::tests::oversized_non_text_parts_replace_the_result_with_a_tool_error` | Shrink step 3 | n/a | Passes |
| `mcp::tests::failed_write_falls_back_to_a_tool_error_and_reports_not_delivered` | Failed write: tool error is written and the session is not updated | n/a | Passes |
| `mcp::tests::successful_write_reports_delivered_unless_replaced` | Delivery result | n/a | Passes |

---

## Issue #4: Classify native A2A before MCP dispatch

### Problem
With `passThroughA2A: true`:
- A native HTTP+JSON `POST /message:send` got an MCP `-32600` instead of reaching the broker.
- A native v0.3 `message/send` without an `A2A-Version` header had `1.0` added.
- A translated call kept a conflicting `A2A-Version` header or query parameter from the client.

### Root cause
`request_filter` used the split headers-then-body flow. It therefore had to set `A2A-Version`, `:path` and `content-length` before it could read the body and know what the request was. Every JSON POST was changed, native or not. It also classified traffic only by JSON-RPC `method`, so HTTP+JSON requests, which have no envelope, were rejected.

### Resolution steps
- [x] **Support native HTTP+JSON pass-through** (the shared-mode contract is not narrowed). `a2a::is_http_json_route` recognises every v1.0 HTTP+JSON route by verb and trailing path segments, under any base path or tenant prefix. The routes are `/message:send`, `/message:stream`, `/tasks`, `/tasks/{id}`, `/tasks/{id}:cancel`, `/tasks/{id}:subscribe`, `/tasks/{id}/pushNotificationConfigs[/{id}]`, `/extendedAgentCard` and `/.well-known/agent-card.json`. These requests are forwarded before any MCP handling.
- [x] Turned on the PDK `enable_stop_iteration` feature and switched the request filter to `into_headers_body_state()`. The body is now classified while headers are still writable, so headers are changed **only for translated `tools/call`s**.
- [x] Native A2A JSON-RPC (v1.0 or v0.3 vocabulary) and bare JSON bodies without a `jsonrpc: "2.0"` envelope are forwarded with no change to path, headers or body. The response filter only acts on translated calls, so native responses are unchanged too.
- [x] Translated calls always match the configured and generated protocol:
  - v1.0 sets `A2A-Version: 1.0`, overwriting any client value.
  - v0.3 removes the header.
  - Any `A2A-Version` query parameter is removed.
- [x] Documented that `a2aPath` applies only to translated calls (README, customer guide, `gcl.yaml`).
- [x] With pass-through off, native HTTP+JSON is still rejected (`-32600` / `405`).

### Files
`Cargo.toml` (feature), `src/a2a.rs`, `src/lib.rs` (`request_filter`, `transcode_tool_call`), `definition/gcl.yaml`, docs.

### Tests
| Test | What it proves | 1.0.3 | 1.1.0 |
|---|---|---|---|
| `tests::native_http_json_send_and_cancel_pass_through_untouched` | `POST /message:send` (with and without tenant), `POST /tasks/{id}:cancel` and `GET /tasks/{id}` keep path, headers and body, and the response is unchanged | Fails (never reached the agent) | Passes |
| `tests::bare_http_json_body_on_another_route_passes_through` | HTTP+JSON body on a non-standard route | Fails | Passes |
| `tests::native_json_rpc_keeps_its_version_semantics` | Native v0.3 without a header stays v0.3; native v1.0 path and query are not rewritten even with `a2aPath` set | Fails (`1.0` was added) | Passes |
| `tests::translated_v1_call_overrides_a_conflicting_client_version` | Client sends `0.3` header and query; the broker gets `1.0` and a clean query | Fails (`0.3` kept) | Passes |
| `tests::translated_v03_call_drops_a_conflicting_client_version` | Client sends `1.0` header and query; the broker gets no header and a clean path | Fails (`1.0` kept) | Passes |
| `tests::native_http_json_is_rejected_when_pass_through_is_off` | Mode 1 contract is unchanged | Passes | Passes |
| `a2a::tests::http_json_routes_are_recognised_under_any_prefix` | Route table, including negatives such as `/mcp` and `PUT` | n/a | Passes |
| `a2a::tests::version_query_is_stripped_case_insensitively` | Query cleanup | n/a | Passes |

---

## Issue #12: Session isolation, identity binding and token generation

The detailed reproductions in the issue were withheld pending a private channel. This section covers only the published acceptance criteria.

### Resolution steps
- [x] **Identity binding is enforced, not relaxed.** When `sessionKeyHeader` is set and the header is missing or empty, the request bypasses the store entirely. `initialize` issues no session, `tools/call` ignores `Mcp-Session-Id`, and `DELETE` returns `404`. The request itself still works, and context can still be passed in the tool arguments.
- [x] **Unambiguous session keys.** The key is a hex SHA-256 over a versioned, length-prefixed encoding of (scope, session ID, identity-or-none). This replaces `"<sid>:<DefaultHasher(identity)>"`, which could collide and used a non-cryptographic hash.
- [x] **Unpredictable, globally unique session IDs.** They are random v4 UUIDs from the host's secure random source (`getrandom`, which is WASI `random_get`) and do not depend on `x-request-id` or any other caller-controlled data. If randomness is unavailable, no session is issued. A2A `messageId` values are random too.
- [x] **Only issued sessions are honoured.** `initialize` registers the new session (`StoreMode::Absent`). `tools/call` and `DELETE` act only on a live, registered entry for the same caller, so a caller can't plant a chosen session ID.
- [x] **Delete-safe updates.** The response side updates the entry with compare-and-swap (`StoreMode::Cas`). A session deleted while a call was in flight stays deleted. This also covers part of the concurrency finding in #5.
- [x] **Scoped shared storage.** The namespace is `mcp-a2a-sessions/<api-id>/<policy-name>.<policy-namespace>`, falling back to the base path when the API ID is unknown in local mode. The same scope is hashed into each key. Two API instances that share Redis are isolated, while replicas of one instance share the same namespace, so sessions continue across replicas.
- [x] **Configured expiry.**
  - The TTL is clamped to 60–86400 seconds. The same value sets the Redis TTL (`remote_ttl_millis`) and the liveness check.
  - Entries store `touched_at`, and the liveness check compares it with the TTL in the current configuration rather than the TTL used when the entry was written. A unit test covers this: an entry written under a long TTL is treated as expired once the configured TTL is lowered. Changing the TTL on a running gateway has not been tested live.
- [x] **Logs** carry only a short prefix of the hashed key, never the session ID or identity.
- [x] **Two-replica shared Redis test added** (`make test-shared-storage`, in `tests/shared-storage/`). `pdk-test` 1.8.0 has no Redis service and its service traits are private, so this is a Docker Compose harness: Redis, two Flex Gateway replicas, two API instances (`hr`, bound to `x-client-id`, and `finance`) and a mock A2A agent that records what it receives. `check.py` verifies:
  - a session issued on flex-1 continues on flex-2;
  - another client, or a request without the identity header, gets no context from it on either replica;
  - `hr` and `finance` can't use each other's session IDs, and their keys sit in different namespaces;
  - Redis keys contain neither the session ID nor the identity, and their TTL matches `sessionTtlSeconds`;
  - the session survives a restart of flex-1;
  - only the owner can `DELETE` it, and once deleted on flex-2 it is gone on flex-1 too.
- [x] **Live run of that test: passed.** It ran on 2026-11-11 against Flex Gateway 1.11.0, 1.12.0 and 1.13.0 in local mode, with a local-mode registration mounted read-only (`REGISTRATION=<path>`). The harness refuses to run if Flex starts in connected mode. All 18 checks pass:

  ```
  Replica continuity (hr, bound to x-client-id)
    PASS  initialize on flex-1 issues a session  (da1bd333-f5a7-46fe-8f80-1a2eae41b3b7)
    PASS  first call starts a new context  (ctx-1)
    PASS  flex-2 continues the context stored by flex-1  (agent got ctx-1)
  Identity binding across replicas
    PASS  another client with the same session id gets no context  (agent got None)
    PASS  a request without x-client-id gets no context  (agent got None)
  Isolation between API instances on the same Redis
    PASS  hr's session id is unknown to finance  (agent got None)
    PASS  finance issues its own session  (0e6627f1-aea5-4453-9eef-5213c6fc32e6)
    PASS  finance continues across replicas  (agent got ctx-5)
    PASS  finance's session id is unknown to hr  (agent got None)
  What is stored in Redis
    PASS  session entries are in Redis  (6 entries, 2 index keys)
           mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:9758a32df64439d312e3b15155e5061ce4482c993e38dcb08799325f7ab0a5e8
           mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:992b971e14afbd5b52f84246dc96be00de847c755efc424b97ca618de63fd6f1
           mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default
           mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:1dcf3854d2ddd015f2114a6973b22ab04e3d3819b646079ed287ad3bf49f5c72
           mcp-a2a-sessions%2F-%2Fhr-agent-p1.default
           mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:mcp-a2a-sessions%2F-%2Fhr-agent-p1.default:99ea7c739f41997a807e2879df5134937fe629344c523acf169f7f3f9d07f745
           mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:8f201b699471a9d7857e19d5bf4b36c4a1ef37b3993c8b9e6ba6dfbd1a175185
           mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default:f80e9c33e878615e839aee72d945f2ce27d13f46ee5e39f40a0678178e6c731e
    PASS  no key contains a session id
    PASS  no key contains the client identity
    PASS  every session entry expires within sessionTtlSeconds  (599075-599704 ms)
           index mcp-a2a-sessions%2F-%2Ffinance-agent-p1.default: type hash, TTL -1
           index mcp-a2a-sessions%2F-%2Fhr-agent-p1.default: type hash, TTL -1
    PASS  hr and finance use different namespaces  (2 namespaces)
  Restart survival
    PASS  the session survives a flex-1 restart  (agent got ctx-1)
  Delete across replicas
    PASS  another client cannot delete the session  (404)
    PASS  the owner deletes the session on flex-2  (204)
    PASS  flex-1 no longer uses the deleted session  (agent got None)
  All shared-storage checks passed.
  ```

  Each session entry is stored under `<namespace>:<SHA-256 key>` with the Redis TTL set to `sessionTtlSeconds`. The two `index` keys are Flex's own per-namespace key index, which the gateway manages and which has no TTL. In local mode the API ID and base path are not known, so the namespace is told apart by the policy instance name (`hr-agent-p1.default` and `finance-agent-p1.default`).
- [x] **Private security reporting channel.** GitHub private vulnerability reporting is enabled, and [`SECURITY.md`](../SECURITY.md) describes how to report. The repository visibility is unchanged.

### Files
`src/session.rs` (rewritten), `src/lib.rs` (`Identity`, `issue_session`, `transcode_tool_call`, `DELETE` handling), `Cargo.toml` (`sha2`, `getrandom`), `definition/gcl.yaml`, docs.

### Tests
| Test | What it proves | 1.0.3 | 1.1.0 |
|---|---|---|---|
| `tests::identity_binding_is_enforced_not_relaxed` | No session without identity. Omitted or changed identity can't read the session; the owner can. | Fails | Passes |
| `tests::unissued_session_ids_are_ignored` | A caller-chosen `Mcp-Session-Id` is not stored or used | Fails | Passes |
| `tests::delete_ends_a_session_only_for_its_owner` | `DELETE` by another or anonymous caller gets `404` and the session survives; the owner gets `204` | Fails | Passes |
| `tests::session_ids_are_random_and_ignore_tracing_headers` | 50 IDs with a fixed `x-request-id` are all different and don't contain it; same for `messageId` | Fails | Passes |
| `tests::local_store_remembers_context_per_session` | Continuity still works for a valid session | Passes | Passes |
| `session::tests::session_key_binds_scope_session_and_identity` | Key depends on scope, session and identity, and doesn't reveal the session ID | n/a | Passes |
| `session::tests::session_key_encoding_is_unambiguous` | Former `:` collisions and `""` vs none are distinct | n/a | Passes |
| `session::tests::random_uuids_are_unique_v4` | 1,000 unique RFC 4122 v4 IDs (covers replica and restart uniqueness) | n/a | Passes |
| `session::tests::namespace_is_per_api_instance_and_policy` | Two API instances or policies get different namespaces | n/a | Passes |
| `session::tests::ttl_is_clamped_and_matches_the_remote_backend` | Redis TTL in milliseconds matches the configured TTL | n/a | Passes |
| `session::tests::expiry_follows_the_configured_ttl` | Expiry is judged against the configured TTL, including a lower TTL than the one the entry was written with (unit level only) | n/a | Passes |
| `session::tests::session_ids_must_be_visible_ascii_and_bounded` | Session ID validation | n/a | Passes |

### Upgrade note
1.1.0 uses a new store namespace and key format. Sessions remembered by 1.0.x are not read after the upgrade, so clients start a new MCP session the next time they call `initialize`. Conversations that pass `context_id` explicitly are not affected.
