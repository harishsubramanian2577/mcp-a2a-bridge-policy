# P4A Review: Issue Resolutions

This document tracks the fixes for the P4A review findings on the MCP A2A Bridge Policy.

| | |
|---|---|
| Reviewed version | 1.0.3 (commit `3d61a41`) |
| Fixed in | 1.1.0, branch `fix/p4a-priority-issues` (priority issues)<br>1.3.0, branch `fix/p4a-remaining-issues` (remaining issues; the interim 1.2.0 was never published) |
| Priority issues | [#1](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/1), [#4](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/4), [#12](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/12) |
| Remaining issues | [#2](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/2), [#3](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/3), [#5](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/5), [#6](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/6), [#7](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/7), [#8](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/8), [#9](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/9), [#10](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/10), [#11](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/11), [#13](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/13) |
| Test baseline | 32 unit tests on 1.0.3, 61 on 1.1.0, 105 on 1.3.0, 112 after the PR #15 review follow-up, all passing |
| Minimum gateway | Flex Gateway 1.11.0 (1.1.0 and later use PDK stop-iteration, which 1.10.x doesn't support) |

## Status

| Issue | Title | Status |
|---|---|---|
| #1 | Failed response rewrite returns raw A2A payload instead of an MCP error | Fixed in 1.1.0 |
| #4 | Classify native A2A before MCP dispatch and preserve native version semantics | Fixed in 1.1.0 |
| #12 | Session isolation, identity binding and token generation | Fixed in 1.1.0. Verified live on two Flex replicas sharing Redis. Private reporting is enabled. |
| #2 | Validate MCP envelopes and initialization before dispatching agent calls | Fixed in 1.3.0 |
| #3 | Reject unsupported MCP-Protocol-Version headers on subsequent requests | Fixed in 1.3.0 |
| #5 | Terminated MCP sessions remain usable and can be restored by late responses | Fixed in 1.3.0. Late answer after `DELETE` verified live on two replicas. |
| #6 | Malformed upstream results are reported as successful completed tool calls | Fixed in 1.3.0 |
| #7 | SSE artifact folding ignores replacement versus append semantics | Fixed in 1.3.0 |
| #8 | SSE parser hides corrupt events and mishandles valid framing | Fixed in 1.3.0 |
| #9 | Input-required tasks without contextId lose task continuation hints | Fixed in 1.3.0 |
| #10 | Correct MCP policy ordering and state the adapter limitations | Fixed in 1.3.0 (documentation) |
| #11 | Validate and correct the A2A v1 Exchange asset-type token | Resolved in 1.3.0. The token stays `a2a_v1`, which Exchange accepts; `a2av1` is rejected. |
| #13 | Configurable MCP elicitation for A2A input-required | Fixed in 1.3.0. Experimental `enableElicitation`, **off by default** (the customer's client, ChatGPT, doesn't support elicitation). Client-managed mode is unchanged. Verified live on two replicas. |

## How the fixes were verified

This section covers 1.1.0. For 1.3.0, see [Verification of 1.3.0](#verification-of-130).

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
| `tests::unissued_session_ids_are_ignored` | A caller-chosen `Mcp-Session-Id` is not stored or used. Renamed to `unissued_session_ids_are_not_found` in 1.3.0, where such an ID gets `404` (#5). | Fails | Passes |
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

---

## Verification of 1.3.0

1. **Unit and filter tests.**
   - Command: `cargo test --lib`.
   - Result: 105 passed, 0 failed. The run is stable both in parallel and with `--test-threads=1`. That is 86 for #2–#11 and 19 for #13 (8 module tests and 11 filter tests).
2. **Baseline check.**
   - The 13 new or changed filter-level tests in `src/tests.rs` were run unchanged on the 1.1.0 code (`3196f89`).
   - 12 of the 13 failed, each with the bug its issue reports.
   - The 13th, `session_ids_are_ignored_without_a_session_store`, is a guard test. It is expected to pass on both versions.
3. **Integration tests.**
   - `make test` passes on Flex 1.11.0: 105 unit tests and the `tools_call_round_trip` integration test.
   - The policy reference name includes the minor version (`mcp-atoa-bridge-policy-flex-v1-3`), so `tests/common/mod.rs` was updated for 1.3.0. Before that update, Flex reported `Extension ... not found` and the request reached the mock agent untranslated.
   - The integration test now uses the fixture's JSON-RPC id (`broker-1`), because 1.3.0 rejects an agent answer whose id doesn't match the request (#6). It also sends `MCP-Protocol-Version: 2025-06-18`.
4. **Shared-storage test.**
   - `make test-shared-storage` passes on Flex 1.11.0 with two replicas sharing Redis. That covers the 1.1.0 checks, updated for `404` (#5), plus a new "Late answer after delete" section and the elicitation sections shown under #13:

   ```
   Identity binding across replicas
     PASS  another client with the same session id gets 404  (404, agent called 0 times)
     PASS  a request without x-client-id gets 404  (404, agent called 0 times)
   Isolation between API instances on the same Redis
     PASS  hr's session id is unknown to finance  (404, agent called 0 times)
     PASS  finance's session id is unknown to hr  (404, agent called 0 times)
   Delete across replicas
     PASS  another client cannot delete the session  (404)
     PASS  the owner deletes the session on flex-2  (204)
     PASS  flex-1 returns 404 for the deleted session  (404, agent called 0 times)
   Late answer after delete
     PASS  the owner deletes the session while a call is in flight  (204)
     PASS  the in-flight call still gets its answer  (200)
     PASS  the late answer does not revive the session  (404, agent called 0 times)
   All shared-storage checks passed.
   ```

5. **Definition check.** `make validate-definition` passes on `a2a_v1` and fails on `a2av1`, which Exchange rejects.
6. **Exchange release.** 1.3.0 (definition and implementation) was released to Exchange on 2026-11-11 and is published.

---

## Issue #2: Validate MCP envelopes and initialization

### Problem
- A `tools/call` with `jsonrpc: "1.0"` still reached the agent.
- A `ping` without an id got a reply with `id: null`.
- `initialize` with `params: {}` succeeded.
- Object ids and scalar params were accepted.
- Notifications were recognised by the `notifications/` prefix alone, so a `notifications/initialized` that carried an id was acknowledged without a JSON-RPC reply.

### Resolution steps
- [x] Added `mcp::classify`, which tells messages apart by structure:
  - a request has a method and an id;
  - a notification has a `notifications/*` method and no id;
  - a response has an id and exactly one of `result` or `error`.

  Any other method sent without an id is treated as a request with a missing id.
- [x] Every MCP message is validated before dispatch: `jsonrpc` must be `"2.0"`, the id must be a string or an integer (not `null`, an object or a fraction), and params, when present, must be an object.
- [x] `initialize` requires `protocolVersion` (a string), `capabilities` (an object) and `clientInfo` (an object with `name` and `version`). A well-formed but unknown version is still negotiated down to a supported one. Missing or mistyped fields get `-32602`.
- [x] Accepted notifications and client responses get a bodyless `202`.
- [x] Invalid input never reaches the agent:
  - when the id can be read, the error is in-band (`200`) and carries that id;
  - when it can't, the reply is HTTP `400` with an error body that has no id.

### Files
`src/mcp.rs` (`classify`, `classify_json`, `initialize_protocol_version`, `error_body`), `src/lib.rs` (`request_filter`, `http_error`, `invalid_message`).

### Tests
| Test | What it proves | 1.1.0 | 1.3.0 |
|---|---|---|---|
| `tests::invalid_envelopes_get_errors_and_never_reach_the_agent` | Table of bad versions, ids, params and missing ids. Each gets the right error and status, and the agent is never called. | Fails | Passes |
| `tests::notifications_and_responses_are_told_apart_by_structure` | Notifications and client responses get a bodyless `202`. A `notifications/*` method that carries an id gets a reply. | Fails | Passes |
| `tests::initialize_requires_well_formed_params_but_negotiates_unknown_versions` | Malformed `initialize` is rejected; an unknown version is negotiated | Fails | Passes |
| `mcp::tests::classifies_requests_notifications_and_responses_structurally` | Classification rules | n/a | Passes |
| `mcp::tests::rejects_invalid_envelopes_ids_and_params` | Envelope, id and params validation | n/a | Passes |
| `mcp::tests::initialize_params_are_validated` | `initialize` field checks | n/a | Passes |
| `mcp::tests::error_body_omits_a_missing_id` | The `400` body has no id | n/a | Passes |

---

## Issue #3: Reject unsupported MCP-Protocol-Version headers

### Problem
`tools/list` with `MCP-Protocol-Version: 1999-01-01` got `200`. The header was never checked.

### Resolution steps
- [x] On MCP traffic (POST and `DELETE`), an `MCP-Protocol-Version` header that isn't `2025-06-18`, `2025-03-26` or `2024-11-05` gets HTTP `400`. A missing header is accepted, as Streamable HTTP allows. `initialize` keeps its own negotiation, which is based on the params.
- [x] Native A2A pass-through traffic is not checked.
- [x] The rejection has no side effects, so the client can retry with a supported version.
- [x] Documented that negotiating `2024-11-05` doesn't add that revision's legacy HTTP+SSE transport.

### Files
`src/mcp.rs` (`check_protocol_version_header`), `src/lib.rs`, docs.

### Tests
| Test | What it proves | 1.1.0 | 1.3.0 |
|---|---|---|---|
| `tests::unsupported_protocol_version_header_is_rejected_and_the_client_can_recover` | Unsupported and malformed headers get `400` without calling the agent. Supported and absent headers work. A retry succeeds. | Fails | Passes |
| `tests::delete_checks_the_protocol_version_header` | `DELETE` with a bad header gets `400` and the session survives | Fails | Passes |
| `mcp::tests::protocol_version_header_must_be_supported_when_present` | Header rules | n/a | Passes |

---

## Issue #5: Terminated sessions remain usable

### Problem
After `DELETE` (`204`), a call with the same session ID still reached the agent and got `200`. A late answer to a call in flight could also store the context again.

### Resolution steps
- [x] **Session lifecycle is separate from context fallback.** When a session store is configured, an `Mcp-Session-Id` that is unknown, expired, deleted or bound to another identity gets HTTP `404` with JSON-RPC `-32001` "Session not found", and the agent is not called. The client starts a new session with `initialize`, as the MCP session rules require.
- [x] **Late answers can't revive a session.** The response side updates the session only with compare-and-swap against the entry read when the call started (added in 1.1.0 for #12). A session deleted while the call was in flight stays deleted. The in-flight call still gets its answer.
- [x] Without a session store, session IDs are ignored as before. The bridge issues none, so none can end.

### Files
`src/lib.rs` (`session_not_found`, `request_filter`, `transcode_tool_call`), `src/session.rs`, `tests/shared-storage/`.

### Tests
| Test | What it proves | 1.1.0 | 1.3.0 |
|---|---|---|---|
| `tests::terminated_session_gets_404_and_initialize_starts_a_new_one` | initialize, call, `DELETE`, call: the last call gets `404` without calling the agent, and a new `initialize` works | Fails | Passes |
| `tests::unissued_session_ids_are_not_found` | A caller-chosen session ID gets `404` | Fails | Passes |
| `tests::identity_binding_is_enforced_not_relaxed` (updated) | Omitted or changed identity gets `404`; the owner is unaffected | Fails | Passes |
| `tests::session_ids_are_ignored_without_a_session_store` | Guard: without a store, session IDs don't cause `404` | Passes | Passes |
| `session::tests::expiry_follows_the_configured_ttl` | Expired entries are not live | Passes | Passes |
| `make test-shared-storage`, "Late answer after delete" | Live, two replicas: `DELETE` on flex-2 while a call is in flight on flex-1; the call gets `200`, and the session then gets `404` | n/a | Passes |

The pdk-unit harness runs a call to completion in one step, so it can't run a `DELETE` in the middle of one. The late-answer case is therefore tested live.

---

## Issue #6: Malformed upstream results reported as success

### Problem
`result: null`, `result: 42`, `{"message": null}` and `{"task": null}` all became successful MCP results. The envelope version and the response id weren't checked, so another request's answer could be relabelled with the current MCP id.

### Resolution steps
- [x] `a2a::rpc_result` checks the agent's envelope. `jsonrpc` must be `"2.0"`, the response must have exactly one of `result` or `error`, and the id must match the one the bridge sent. An error may carry a `null` id, as JSON-RPC allows.
- [x] The result must be a valid Task or Message, wrapped (`{task}`, `{message}`) or bare, in v1.0 or v0.3 form. Required members are type-checked: status, state, parts, artifacts and roles.
- [x] Anything invalid becomes an MCP tool error (`isError: true`), "The A2A agent returned an invalid response: ...", and the session is not updated.
- [x] Test fixtures and mocks now answer with the request's id instead of a fixed one.

### Files
`src/a2a.rs` (`rpc_result`, `result_snapshot`, `task_snapshot`, `message_snapshot`, `parts`), `src/tests.rs`, `tests/requests.rs`.

### Tests
| Test | What it proves | 1.1.0 | 1.3.0 |
|---|---|---|---|
| `tests::malformed_or_uncorrelated_agent_answers_are_tool_errors` | The cases from the issue, plus a wrong id, through the filter. Each is a tool error and nothing is remembered. | Fails | Passes |
| `a2a::tests::malformed_envelopes_are_invalid_responses` | Wrong version, wrong id, both or neither of result and error | n/a | Passes |
| `a2a::tests::malformed_results_are_invalid_responses` | Null, scalar and empty results, null wrappers, missing or mistyped status and parts | n/a | Passes |
| `a2a::tests::task_state_accepts_both_spellings_and_rejects_others` | Unknown states, and v1.0 `TASK_STATE_UNSPECIFIED`, are invalid | n/a | Passes |

---

## Issue #7: SSE artifact folding ignores replace versus append

### Problem
Two artifact updates for the same `artifactId` with `append: false` both stayed in the final answer.

### Resolution steps
- [x] Artifacts are folded by `artifactId`, starting from the artifacts in the initial Task:
  - an update replaces the artifact, unless `append` is `true`, in which case it extends it;
  - an artifact not seen before is added;
  - artifacts keep the order in which they were first seen.
- [x] A status update replaces the status, including its message.
- [x] The final answer is flattened only after folding, so superseded output never appears in it.
- [x] Both v1.0 (`artifactUpdate`, `statusUpdate`) and v0.3 (`artifact-update`, `status-update`) events are handled.

### Files
`src/a2a.rs` (`apply_artifact`, `apply_status`, `parse_sse_body`).

### Tests
| Test | What it proves |
|---|---|
| `a2a::tests::artifact_updates_replace_unless_appending` | Replacement of the initial snapshot, repeated replacements, true append and interleaved ids, in v1.0 and v0.3 |
| `a2a::tests::status_update_replaces_the_status_message` | The latest status message wins |

---

## Issue #8: SSE parser hides corrupt events and mishandles framing

### Problem
- A malformed event after a valid one was dropped, and the earlier content was returned as a success.
- A leading BOM hid a valid event.
- CR-only line ends weren't split correctly.
- An unterminated last event was still processed.

### Resolution steps
- [x] `a2a::sse_event_data` follows the WHATWG event-stream parsing rules:
  - LF, CRLF and CR line ends;
  - one leading BOM is stripped;
  - `:` comment lines are skipped;
  - multi-line `data` is joined;
  - an event is dispatched only at a blank line.
- [x] A stream that ends in the middle of an event is an error.
- [x] Empty events and comments are ignored. Any other corrupt event fails the whole stream: malformed JSON, a non-JSON-RPC envelope, an event without a result or error, another request's id, or a malformed update. The result is a tool error, and nothing from the stream is remembered.

### Files
`src/a2a.rs` (`sse_event_data`, `parse_sse_body`).

### Tests
| Test | What it proves |
|---|---|
| `a2a::tests::sse_accepts_every_line_ending_bom_comments_and_multiline_data` | All three line-end forms with several events, the BOM case from the issue, comments and multi-line data |
| `a2a::tests::corrupt_or_truncated_streams_are_errors` | A valid snapshot followed by each kind of corrupt event, an unterminated EOF, an event without its blank line, and an agent error |

---

## Issue #9: Input-required tasks without contextId lose the continuation

### Problem
A Task with `id: t9`, state `input-required` and no `contextId` produced only the question. The state and `t9` were missing, because the hint returned early when there was no context.

### Resolution steps
- [x] A continuation hint is produced whenever an interrupted task has a task ID. `context_id` is added only when the agent returned one.
- [x] On the next call, the bridge sends `taskId` alone, and the agent infers the context, as A2A v1.0 allows.
- [x] The `auth-required` hint asks the user to complete authorization outside the conversation. It says that replying doesn't grant authorization and that passwords, tokens and other secrets must never be sent.
- [x] Documented task-only continuation and the `auth-required` wording.

### Files
`src/a2a.rs` (`continuation_hint`, `into_reply`), docs.

### Tests
| Test | What it proves | 1.1.0 | 1.3.0 |
|---|---|---|---|
| `tests::input_required_task_without_context_keeps_its_continuation` | With structured content on and off, the result carries the state and `t9`, and the next call sends `taskId: t9` with no `contextId` | Fails | Passes |
| `tests::auth_required_task_without_context_is_not_answered_with_secrets` | `auth-required` wording, without structured content | Fails | Passes |
| `a2a::tests::input_required_without_context_keeps_the_task_hint` | Hint with task ID only | n/a | Passes |
| `a2a::tests::builds_task_only_continuation` | Outbound message with `taskId` only | n/a | Passes |
| `a2a::tests::auth_required_hint_never_implies_the_reply_authorizes` | `auth-required` wording | n/a | Passes |
| `a2a::tests::hints_for_running_and_finished_tasks` | No hint for finished tasks | n/a | Passes |

---

## Issue #10: Policy ordering and adapter limitations

### Resolution steps
- [x] **Deployment table corrected.** In Mode 1 (MCP instance), MCP Support comes first, then authentication, then the bridge. The table notes that MCP Support isn't available in Local Mode, so the local test setup doesn't use it. The Mode 1 diagram and the README ordering paragraph match.
- [x] **Purpose.** The purpose statement is agent-first, with MuleSoft Agent Broker as one example. It calls the policy a single-tool adapter, not full protocol transcoding, and links to Limitations.
- [x] **New "8. Limitations" section** in the customer guide. It covers:
  - polling, cancel and push not supported;
  - buffered rather than streamed MCP delivery;
  - no elicitation or auth orchestration;
  - lossy artifact and file projection;
  - supported bindings and versions;
  - session fallback;
  - retry risk for calls with side effects;
  - client responsibilities for interrupted tasks.
- [x] **New "9. Verification checklist"**, including auth-protected discovery and a real client continuation.

### Files
`docs/MCP-A2A-Bridge-Policy.md`, `README.md`.

---

## Issue #11: A2A v1 Exchange asset-type token

### Resolution steps
- [x] `definition/gcl.yaml`: `metadata/capabilities/assetTypes` stays `mcp,a2a,a2a_v1`. The issue proposed `a2av1`, but Exchange rejects that token (see the evidence below), so the original token was correct.
- [x] Added `scripts/validate-definition.py` and `make validate-definition`. The script checks:
  - every token is on the Exchange allow-list, as Exchange reports it: `http`, `mcp`, `a2a`, `a2a_v1`, `rest`, `wsdl`, `llm`, `grpc`, `websocket`, `sse`, `graphql`;
  - there are no duplicates;
  - the list is exactly `mcp,a2a,a2a_v1`;
  - a title is present;
  - the descriptions in `gcl.yaml` and `Cargo.toml` are 256 characters or fewer.
- [x] `make publish` and `make release` run the check first, so a bad token stops before upload.

**Evidence (2026-11-11):**
- A release with `mcp,a2a,a2av1` was rejected by Exchange with HTTP `400`: "/capabilities/assetTypes/2 must be equal to one of the allowed values (http, mcp, a2a, a2a_v1, rest, wsdl, llm, grpc, websocket, sse, graphql)". Nothing was published.
- The 1.3.0 release with `mcp,a2a,a2a_v1` was accepted. The published definition's `metadata.yaml` lists `mcp`, `a2a` and `a2a_v1`.
- The validator passes on `a2a_v1` and fails on `a2av1`.

- In API Manager (2026-11-11), 1.3.0 is offered on MCP and A2A v1 instances, and the elicitation fields show their defaults (off, 300 seconds, 3 rounds).

**Verification boundary:** no A2A v0.3 instance was available, so the `a2a` token was not checked in API Manager.

### Files
`definition/gcl.yaml`, `scripts/validate-definition.py`, `Makefile`.

---

## Issue #13: Elicitation for input-required, with client-managed fallback

### Decisions
| Question | Decision |
|---|---|
| Target clients | The customer's primary MCP client is ChatGPT, which doesn't support MCP elicitation. So `enableElicitation` is **off by default**, and the client-managed mode (`structuredContent` with `contextId`/`taskId`, guide 3.1) stays the default and is unchanged. Elicitation is experimental, for clients that declare it. |
| Form and answers | One required free-text field, `answer`, with the agent's question as the message. Nothing is inferred from the agent's text, because A2A doesn't say what shape of answer it wants. The answer is sent to the agent as a plain text part of the same task (`taskId`, plus `contextId` when known), which is exactly what a client-managed follow-up sends. The field description tells the user never to enter passwords, tokens or other secrets, and `auth-required` is never turned into a form. |
| Timeout | `elicitationTimeoutSeconds`, default **300**, range 30–1800, capped by `sessionTtlSeconds`. Five minutes covers a person reading and typing without leaving a stale task waiting at the agent. |
| Repeated questions | `maxElicitationRounds`, default **3**, range 1–10, per `tools/call`. A further question comes back as an ordinary `input-required` result with the IDs, so a looping agent can't keep the user in forms. |
| Decline, cancel, disconnect | The conversation ends: the result says why (`structuredContent: {state: "ended", reason}`), and the session's context and task are cleared, so the next call starts fresh. A disconnect is not a cancellation in MCP, so a client that never comes back is treated as a timeout. `notifications/cancelled` for the call ends it at once. A JSON-RPC error from the client (for example it can't show the form) is not a refusal: the question falls back to the client-managed result and the conversation goes on. |

### Resolution steps
- [x] **Opt-in and gating.** `enableElicitation` (default `false`) is honoured only with a session store, a client that negotiated `2025-06-18` or later and declared `capabilities.elicitation`, and a `tools/call` that accepts `text/event-stream`. Otherwise behaviour is unchanged.
- [x] **Resumable stream instead of a held connection.** A policy can't keep a response open while waiting for the user, so the bridge uses Streamable HTTP resumption:
  - the `tools/call` gets an SSE stream with one `elicitation/create` (event `id: <stream>-<n>`) and a `retry` hint, and the stream ends;
  - the client POSTs its response and gets `202`;
  - an accepted answer goes to the agent;
  - the client resumes with `GET` and `Last-Event-ID`, and gets the next question or the final result under the original request ID.
- [x] **State in the session store.** The open question is stored next to the session, under a separate namespace, so any replica can take the answer or resume the stream. It is bound to the session's identity. Sessions stored by earlier builds stay readable.
- [x] **Exactly-once answers.** The question is claimed with compare-and-swap before the answer is forwarded. A duplicate or concurrent answer, an answer to another or an old question ID, and a late answer after timeout or decline are acknowledged with `202` and never reach the agent. A malformed answer gets `400` and the question stays open.
- [x] **Open questions are protected.** A `tools/call` that would continue the waiting conversation gets a tool error asking the user to answer or cancel first, and the question and its stream stay intact. An independent call (`new_conversation`, or other explicit IDs) runs without eliciting. `notifications/cancelled` with the call's `requestId` ends the question and clears the context, if it still belongs to that conversation. (Changed in the PR #15 review follow-up; 1.3.0 dropped the open question on any new call.)
- [x] **Configuration and docs.** New `gcl.yaml` properties: `enableElicitation`, `elicitationTimeoutSeconds` and `maxElicitationRounds`. Updated the customer guide (new "3.2 Experimental: MCP elicitation", configuration table, Limitations, checklist item 9) and the README.

### Files
`src/elicitation.rs` (new), `src/lib.rs` (`resume_stream`, `answer_elicitation`, `cancel_elicitation`, `start_elicitation`, `settle_answer`), `src/session.rs` (elicitation namespace, `update_elicitation`), `definition/gcl.yaml`, `tests/shared-storage/`, docs.

### Tests
| Test | What it proves |
|---|---|
| `tests::elicitation_is_off_by_default_and_questions_stay_tool_results` | ChatGPT path: with the default config, even a client that declares elicitation gets the JSON result with `taskId` and `contextId` |
| `tests::elicitation_needs_the_capability_a_session_store_and_an_sse_capable_call` | No capability, a pre-2025-06-18 revision, a JSON-only `Accept`, or no session store: no elicitation |
| `tests::accepted_answer_resumes_the_task_and_the_result_arrives_on_the_resumed_stream` | Full round trip: the question on SSE, waiting on resume, the answer is `202` and goes to the agent with `taskId`/`contextId`, the result arrives under the original ID, a repeat answer isn't forwarded, and the context continues |
| `tests::repeated_questions_stop_at_the_round_limit` | New request and event IDs per round, and the extra question comes back client-managed |
| `tests::decline_cancel_and_timeout_end_the_conversation` | Each ends with its reason, nothing is sent to the agent, a late answer is ignored, and the next call has no `contextId`/`taskId` |
| `tests::a_client_that_cannot_show_the_form_gets_the_question_as_a_tool_result` | A JSON-RPC error answer falls back to the client-managed result |
| `tests::answers_and_streams_are_bound_to_the_session_and_the_question` | Another identity gets `404`, a wrong question ID is ignored, a malformed answer gets `400`, and an unknown stream gets `400` |
| `tests::auth_required_and_task_less_questions_are_never_elicited` | `auth-required` and plain messages are answered directly |
| `tests::a_question_without_context_resumes_with_the_task_only` | Works with #9's task-only continuation |
| `tests::an_agent_failure_after_the_answer_is_a_tool_error_on_the_resumed_stream` | Agent errors after an answer become a tool error |
| `tests::cancelling_the_call_ends_the_question_but_another_call_does_not` | `notifications/cancelled` ends the question; a new `tools/call` does not |
| `elicitation::tests::*` (8) | Settings bounds, capability and version gating, form schema, strict answer parsing, event IDs, stream framing, ended results |
| `make test-shared-storage`, elicitation sections | Live, two replicas on Redis (below) |

```
Elicitation across replicas (support, experimental)
  PASS  the question arrives as elicitation/create on SSE  (200 text/event-stream)
  PASS  flex-2 resumes the stream and waits for the answer  (200)
  PASS  another client cannot answer  (404)
  PASS  both replicas acknowledge the answer  ([202, 202])
  PASS  the answer reaches the agent once, on the same task and context  (1 answers)
  PASS  the result arrives on flex-2 under the original call id
Elicitation decline
  PASS  declining ends the conversation  ({'reason': 'declined', 'state': 'ended'})
  PASS  the next call starts a new context  (agent got None)
Client-managed fallback on the same API (ChatGPT-style client)
  PASS  a client without the capability gets a JSON tool result with the ids
```

**Verification boundary:** the live checks use a scripted MCP client. No shipping MCP client that supports elicitation and `Last-Event-ID` resumption has been tried yet, which is why the feature is marked experimental. ChatGPT is unaffected, because it never declares the capability and the setting is off by default.

## PR #15 review follow-up

The second review of PR #15 (at `9dff842`) found seven more problems. It also flagged lint failures and the inherited `minRuntimeVersion` blocker. The branch now contains `fix/p4a-priority-issues` (PR #14). That merge brings in the `minRuntimeVersion` 1.11.0 fix and its build-time check (`scripts/check-min-runtime.py`).

**Merge order:** PR #15 targets `main`, and it includes all of PR #14. Merge #14 first, then #15. Once #14 is in `main`, the #15 diff contains only the remaining-issue fixes.

| # | Finding | Fix | Regression test |
|---|---|---|---|
| 1 | A second tool call destroys the outstanding elicitation | A call that continues the waiting conversation is refused with a tool error, and the question and its stream are kept. An independent call runs without eliciting. `start_elicitation` never replaces a question that is still open. | `tests::review_second_call_must_preserve_first_stream` |
| 2 | A completed task-only response keeps a stale continuation | When a reply has no context and no awaiting task, the stored task is cleared. | `tests::review_completed_task_only_must_clear_continuation` (completed, failed, rejected, canceled) |
| 3 | An explicit `task_id` inherits an unrelated stored context | If the call passes `context_id` or `task_id`, only the IDs it passes are used. | `tests::review_explicit_task_must_not_inherit_unrelated_context` |
| 4 | A new call reuses an expired elicitation conversation | A question past its deadline is ended as timed out, and its conversation is forgotten before the call is built. | `tests::review_expired_question_must_not_resume_on_new_call` |
| 5 | Mixed request/response envelopes still execute | A request or notification that carries `result` or `error` is rejected as `-32600`. Notifications get `400`. Nothing reaches the agent. | `tests::review_mixed_envelope_must_not_execute` |
| 6 | Malformed A2A Message/Part bodies become successful results | Messages must have a known `role` and a non-empty `messageId`. Each part must carry well-typed `text`, `data`, `file`, `url` or `raw` content. Anything else is a tool error. | `tests::review_invalid_message_must_be_tool_error` |
| 7 | SSE folding combines different tasks | A stream whose events name a different task or context than the earlier events is rejected. | `tests::review_sse_task_ids_must_match` |
| 8 | Inherited runtime metadata blocker | Merged from PR #14. `make build` writes `minRuntimeVersion: 1.11.0` and checks it. | `scripts/check-min-runtime.py` (runs in `make build`) |

**How the fixes were verified:**

- All eight regression tests were copied unchanged onto the pre-fix code and run there. The rewritten cancel test is one of the eight. All of them fail there, and all pass after the fixes.
- `cargo test --lib`: 112 passed.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean. So is `cargo clippy --lib --target wasm32-wasip1 -- -D warnings`.
- `make build` passes its runtime-version check.

**Known limitation:** a question that has been answered but whose result hasn't been fetched yet can still be replaced by a new call's question.

**Still open:** interop with an MCP SDK client has not been tested end to end. The elicitation checks use a scripted client (see the verification boundary under #13).
