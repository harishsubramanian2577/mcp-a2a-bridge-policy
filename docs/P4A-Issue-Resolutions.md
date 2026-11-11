# P4A Review: Issue Resolutions

This document tracks the fixes for the P4A review findings on the MCP A2A Bridge Policy.

| | |
|---|---|
| Reviewed version | 1.0.3 (commit `3d61a41`) |
| Fixed in | 1.1.0, branch `fix/p4a-priority-issues` |
| Priority issues | [#1](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/1), [#4](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/4), [#12](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/12) |
| Test baseline | 32 unit tests on 1.0.3, 61 on 1.1.0, all passing |

## Status

| Issue | Title | Status |
|---|---|---|
| #1 | Failed response rewrite returns raw A2A payload instead of an MCP error | Fixed in 1.1.0 |
| #4 | Classify native A2A before MCP dispatch and preserve native version semantics | Fixed in 1.1.0 |
| #12 | Session isolation, identity binding and token generation | Fixed in 1.1.0. A live multi-instance Redis test and the private reporting channel are still open (see below). |

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
   - `tests/requests.rs` compiles (`cargo test --no-run`).
   - The Docker run (`make test`) was not executed in this environment because Docker was not available.

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
  - Entries store `touched_at`, and liveness is judged against the TTL configured now. After a config change, a lower TTL takes effect on existing entries immediately.
- [x] **Logs** carry only a short prefix of the hashed key, never the session ID or identity.
- [ ] **Not covered here:** a live test with two gateway instances on one Redis. `pdk-unit` gives each test its own storage, so cross-instance isolation is proven at the namespace and key level, not against a real Redis. A Docker playground test with shared Redis is the next step.
- [ ] **Private security reporting channel.** This is left for the repository owner: the repository settings are intentionally unchanged while the P4A team is still reviewing.

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
| `session::tests::expiry_follows_the_configured_ttl` | Expiry, including a lower TTL after a config change | n/a | Passes |
| `session::tests::session_ids_must_be_visible_ascii_and_bounded` | Session ID validation | n/a | Passes |

### Upgrade note
1.1.0 uses a new store namespace and key format. Sessions remembered by 1.0.x are not read after the upgrade, so clients start a new MCP session the next time they call `initialize`. Conversations that pass `context_id` explicitly are not affected.
