// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Filter-level tests: drive the real entrypoint through pdk-unit and assert what reaches the
//! upstream Agent Broker and what the MCP client gets back.

use std::rc::Rc;

use pdk_unit::{TraceBackend, UnitHttpMessage, UnitHttpRequest, UnitHttpResponse, UnitTestBuilder};
use serde_json::{json, Value};

const SAMPLE_RESPONSE: &str = include_str!("../tests/fixtures/a2a_v1_send_message_response.json");

fn config(store: &str) -> String {
    json!({
        "toolName": "ask_agent",
        "toolDescription": "Ask the agent",
        "a2aProtocolVersion": "1.0",
        "passThroughA2A": true,
        "includeStructuredContent": true,
        "sessionStoreMode": store,
        "sessionTtlSeconds": 1800,
    })
    .to_string()
}

type Agent = Rc<TraceBackend<Box<dyn Fn(UnitHttpRequest) -> UnitHttpResponse>>>;

/// A mock agent that answers with `body`, relabelled with the JSON-RPC id of the request it
/// receives, as a real agent would. The bridge checks that the answer matches its request.
fn agent(body: &str) -> Agent {
    agent_with("application/json", body.as_bytes().to_vec())
}

fn agent_with(content_type: &str, body: Vec<u8>) -> Agent {
    let content_type = content_type.to_string();
    Rc::new(TraceBackend::new(Box::new(move |request: UnitHttpRequest| {
        let id = serde_json::from_slice::<Value>(request.body()).ok().and_then(|r| r.get("id").cloned());
        let body = match id {
            Some(id) if content_type.contains("event-stream") => relabel_sse(&body, &id),
            Some(id) => relabel(&body, &id).unwrap_or_else(|| body.clone()),
            None => body.clone(),
        };
        UnitHttpResponse::new(200).with_header("content-type", content_type.as_str()).with_body(body)
    })))
}

/// A mock agent that always answers with exactly `body`.
fn fixed_agent(body: &str) -> Agent {
    let body = body.as_bytes().to_vec();
    Rc::new(TraceBackend::new(Box::new(move |_| {
        UnitHttpResponse::new(200).with_header("content-type", "application/json").with_body(body.clone())
    })))
}

fn relabel(json: &[u8], id: &Value) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(json).ok()?;
    value.as_object_mut()?.get_mut("id").map(|old| *old = id.clone())?;
    serde_json::to_vec(&value).ok()
}

fn relabel_sse(sse: &[u8], id: &Value) -> Vec<u8> {
    let text = String::from_utf8_lossy(sse);
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| match line.strip_prefix("data: ").and_then(|data| relabel(data.as_bytes(), id)) {
            Some(json) => format!("data: {}", String::from_utf8(json).unwrap()),
            None => line.to_string(),
        })
        .collect();
    lines.join("\n").into_bytes()
}

fn initialize(id: i64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } })
}

fn mcp_post(body: Value) -> UnitHttpRequest {
    mcp_post_at("/mcp", body)
}

fn mcp_post_at(path: &str, body: Value) -> UnitHttpRequest {
    UnitHttpRequest::post()
        .with_path(path)
        .with_header("content-type", "application/json")
        .with_header("accept", "application/json, text/event-stream")
        .with_body(body.to_string().into_bytes())
}

fn json_body<M: UnitHttpMessage>(msg: &M) -> Value {
    serde_json::from_slice(msg.body()).expect("body is JSON")
}

fn tools_call(id: i64, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "ask_agent", "arguments": args } })
}

#[test]
fn initialize_is_answered_locally() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }
    })));

    assert_eq!(response.status_code(), 200);
    let body = json_body(&response);
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    assert!(response.header("mcp-session-id").is_none());
    assert!(backend.next().is_none(), "initialize must not reach the agent");
}

#[test]
fn tools_list_advertises_the_bridge_tool() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" })));

    let body = json_body(&response);
    assert_eq!(body["result"]["tools"][0]["name"], "ask_agent");
    assert!(body["result"]["tools"][0]["inputSchema"]["properties"]["context_id"].is_object());
    assert!(backend.next().is_none());
}

#[test]
fn notifications_get_202_and_sse_get_gets_405() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let notified = tester.request(mcp_post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })));
    assert_eq!(notified.status_code(), 202);

    let sse = tester.request(
        UnitHttpRequest::get()
            .with_path("/mcp")
            .with_header("accept", "text/event-stream"),
    );
    assert_eq!(sse.status_code(), 405);
    assert!(backend.next().is_none());
}

#[test]
fn unknown_tool_is_invalid_params() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "other", "arguments": {} }
    })));

    assert_eq!(json_body(&response)["error"]["code"], -32602);
    assert!(backend.next().is_none());
}

#[test]
fn tools_call_is_transcoded_both_ways() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(tools_call(
        7,
        json!({ "message": "Find the page", "context_id": "ctx-given" }),
    )));

    let upstream = backend.next().expect("the agent was called");
    assert_eq!(upstream.header("a2a-version"), Some("1.0"));
    let a2a = json_body(&upstream);
    assert_eq!(a2a["method"], "SendMessage");
    assert_eq!(a2a["id"], 7);
    assert_eq!(a2a["params"]["message"]["role"], "ROLE_USER");
    assert_eq!(a2a["params"]["message"]["parts"][0]["text"], "Find the page");
    assert_eq!(a2a["params"]["message"]["contextId"], "ctx-given");

    assert_eq!(response.status_code(), 200);
    assert!(response.header("content-type").unwrap().contains("application/json"));
    let mcp = json_body(&response);
    assert_eq!(mcp["id"], 7);
    assert_eq!(mcp["result"]["isError"], false);
    assert_eq!(mcp["result"]["structuredContent"]["state"], "completed");
    assert_eq!(
        mcp["result"]["structuredContent"]["contextId"],
        "59ec80cd-988c-47fd-8917-1f84dd3bd409"
    );
    let hint = mcp["result"]["content"][1]["text"].as_str().unwrap();
    assert!(hint.contains("context_id=\"59ec80cd-988c-47fd-8917-1f84dd3bd409\""));
}

#[test]
fn agent_json_rpc_error_becomes_tool_error() {
    let backend = agent(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"Task not found"}}"#);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(tools_call(1, json!({ "message": "hi", "task_id": "t-x" }))));

    let mcp = json_body(&response);
    assert_eq!(mcp["result"]["isError"], true);
    assert!(mcp["result"]["content"][0]["text"].as_str().unwrap().contains("Task not found"));
}

#[test]
fn a2a_requests_pass_through_untouched() {
    let backend = fixed_agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "SendMessage",
        "params": { "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "x" }] } } });
    let response = tester.request(mcp_post(request.clone()));

    assert_eq!(json_body(&backend.next().unwrap()), request);
    assert_eq!(json_body(&response), serde_json::from_str::<Value>(SAMPLE_RESPONSE).unwrap());
}

#[test]
fn local_store_remembers_context_per_session() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let init = tester.request(mcp_post(initialize(1)));
    let session = init.header("mcp-session-id").expect("session issued").to_string();

    let call = |id| {
        mcp_post(tools_call(id, json!({ "message": "next" }))).with_header("mcp-session-id", session.as_str())
    };
    tester.request(call(2));
    let first = json_body(&backend.next().unwrap());
    assert!(first["params"]["message"].get("contextId").is_none());

    tester.request(call(3));
    let second = json_body(&backend.next().unwrap());
    assert_eq!(
        second["params"]["message"]["contextId"],
        "59ec80cd-988c-47fd-8917-1f84dd3bd409"
    );
}

fn bridge_only_config(extra: Value) -> String {
    let mut config = json!({ "toolName": "ask_agent", "a2aProtocolVersion": "1.0" });
    config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    config.to_string()
}

#[test]
fn passthrough_is_off_by_default() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bridge_only_config(json!({})))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let a2a = tester.request(mcp_post(json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {} })));
    assert_eq!(json_body(&a2a)["error"]["code"], -32601);

    let card = tester.request(UnitHttpRequest::get().with_path("/.well-known/agent-card.json"));
    assert_eq!(card.status_code(), 405);

    let text = tester.request(
        UnitHttpRequest::post()
            .with_path("/mcp")
            .with_header("content-type", "text/plain")
            .with_body(b"hi".to_vec()),
    );
    assert_eq!(text.status_code(), 415);
    assert!(backend.next().is_none());
}

#[test]
fn a2a_path_rewrites_the_forwarded_path_under_the_base_path() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agents-mcp".to_string()))
        .with_config(bridge_only_config(json!({ "a2aPath": "/a2a/my-agent" })))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    tester.request(
        mcp_post_at("/agents-mcp/mcp?tenant=x", tools_call(1, json!({ "message": "hi" }))),
    );

    let upstream = backend.next().unwrap();
    assert_eq!(upstream.header(":path"), Some("/agents-mcp/a2a/my-agent?tenant=x"));
}

#[test]
fn root_a2a_path_keeps_the_trailing_slash() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agents-mcp/".to_string()))
        .with_config(bridge_only_config(json!({ "a2aPath": "/" })))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    tester.request(mcp_post_at("/agents-mcp/mcp", tools_call(1, json!({ "message": "hi" }))));

    assert_eq!(backend.next().unwrap().header(":path"), Some("/agents-mcp/"));
}

#[test]
fn path_is_untouched_without_a2a_path() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agents-mcp".to_string()))
        .with_config(bridge_only_config(json!({})))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    tester.request(mcp_post_at("/agents-mcp/mcp", tools_call(1, json!({ "message": "hi" }))));

    assert_eq!(backend.next().unwrap().header(":path"), Some("/agents-mcp/mcp"));
}

#[test]
fn forward_path_joins_base_and_a2a_path() {
    use crate::resolve_forward_path as fp;
    assert_eq!(fp(Some("/agents-mcp"), Some("/a2a/x")), Some("/agents-mcp/a2a/x".into()));
    assert_eq!(fp(Some("/agents-mcp/*"), Some("a2a/x/")), Some("/agents-mcp/a2a/x".into()));
    assert_eq!(fp(Some("/agents-mcp/"), Some("/")), Some("/agents-mcp/".into()));
    assert_eq!(fp(Some("/agents-mcp"), Some("/")), Some("/agents-mcp/".into()));
    assert_eq!(fp(Some("/"), Some("/")), Some("/".into()));
    assert_eq!(fp(Some("/"), Some("/a2a")), Some("/a2a".into()));
    assert_eq!(fp(Some("/agents-mcp"), Some("  ")), None);
    assert_eq!(fp(None, Some("/a2a")), None);
}

// ---- Issue #1: the converted answer always fits, and is the only thing the client receives.

fn task_reply(context_id: &str, parts: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": "broker", "result": { "task": {
        "id": "t-1", "contextId": context_id,
        "status": { "state": "TASK_STATE_COMPLETED",
                    "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": parts } } } } })
}

fn assert_bounded_mcp_answer(response: &pdk_unit::UnitHttpResponse, id: i64) -> Value {
    assert_eq!(response.status_code(), 200);
    assert!(response.header("content-type").unwrap().contains("application/json"));
    assert!(response.body().len() < crate::mcp::MAX_BODY_BYTES, "{} bytes", response.body().len());
    let mcp = json_body(response);
    assert_eq!(mcp["jsonrpc"], "2.0");
    assert_eq!(mcp["id"], id);
    assert!(mcp["result"]["content"][0]["text"].is_string(), "MCP result.content is present");
    assert!(mcp["result"].get("task").is_none(), "the raw A2A task must not leak");
    mcp
}

#[test]
fn large_data_part_json_reply_becomes_a_bounded_mcp_result() {
    // Reproduction from issue #1: 600,000-character data part → 1,200,306 bytes once converted.
    let reply = task_reply("ctx-big", json!([{ "data": "x".repeat(600_000) }]));
    let backend = agent_with("application/json", reply.to_string().into_bytes());
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(tools_call(11, json!({ "message": "big" }))));

    let mcp = assert_bounded_mcp_answer(&response, 11);
    assert_eq!(mcp["result"]["isError"], false);
    assert_eq!(mcp["result"]["structuredContent"]["contextId"], "ctx-big");
    assert!(mcp["result"]["structuredContent"].get("data").is_none());
}

#[test]
fn large_sse_reply_is_never_released_as_raw_sse() {
    let reply = task_reply("ctx-sse", json!([{ "text": "\"".repeat(600_000) }]));
    let sse = format!("event: message\ndata: {}\n\n", reply);
    let backend = agent_with("text/event-stream", sse.into_bytes());
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(tools_call(12, json!({ "message": "big" }))));

    assert!(!response.body().starts_with(b"event:") && !response.body().starts_with(b"data:"));
    let mcp = assert_bounded_mcp_answer(&response, 12);
    assert!(mcp["result"]["content"][0]["text"].as_str().unwrap().ends_with("Ask for a shorter answer.]"));
}

#[test]
fn undeliverable_answer_does_not_update_the_session() {
    // A context id this large cannot fit (it is repeated in the hint and structuredContent), so
    // the answer is replaced by a tool error and must not become the remembered context.
    let reply = task_reply(&"c".repeat(600_000), json!([{ "text": "ok" }]));
    let backend = agent_with("application/json", reply.to_string().into_bytes());
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);
    let session = start_session(&mut tester, &[]);

    let first = tester.request(with_headers(mcp_post(tools_call(2, json!({ "message": "a" }))), &[("mcp-session-id", &session)]));
    let mcp = assert_bounded_mcp_answer(&first, 2);
    assert_eq!(mcp["result"]["isError"], true);
    backend.next();

    tester.request(with_headers(mcp_post(tools_call(3, json!({ "message": "b" }))), &[("mcp-session-id", &session)]));
    let second = json_body(&backend.next().unwrap());
    assert!(second["params"]["message"].get("contextId").is_none(), "session was not updated");
}

#[test]
fn oversized_outbound_message_is_rejected_before_rewriting() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    // 350,000 control characters are 6 bytes each once JSON-escaped (~2.1 MB outbound).
    let message = "\u{1}".repeat(350_000);
    let response = tester.request(mcp_post(tools_call(4, json!({ "message": message }))));

    assert_eq!(json_body(&response)["error"]["code"], -32602);
    assert!(backend.next().is_none());
}

// ---- Issue #4: native A2A is classified first and forwarded without any change.

fn with_headers(mut request: UnitHttpRequest, headers: &[(&str, &str)]) -> UnitHttpRequest {
    for (name, value) in headers {
        request = request.with_header(*name, *value);
    }
    request
}

fn shared_config(version: &str, extra: Value) -> String {
    let mut config = json!({ "toolName": "ask_agent", "a2aProtocolVersion": version, "passThroughA2A": true });
    config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    config.to_string()
}

fn shared_tester(version: &str, extra: Value, backend: &Agent) -> pdk_unit::UnitTest {
    UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agent".to_string()))
        .with_config(shared_config(version, extra))
        .with_backend(Rc::clone(backend))
        .with_entrypoint(crate::configure)
}

#[test]
fn native_http_json_send_and_cancel_pass_through_untouched() {
    let agent_answer = r#"{"task":{"id":"t-1","status":{"state":"TASK_STATE_WORKING"}}}"#;
    let backend = agent(agent_answer);
    let mut tester = shared_tester("1.0", json!({ "a2aPath": "/a2a" }), &backend);

    let send = json!({ "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "hello" }] } });
    for path in ["/agent/message:send", "/agent/tenant-1/message:send", "/agent/tasks/t-1:cancel"] {
        let response = tester.request(mcp_post_at(path, send.clone()));
        let upstream = backend.next().expect("reached the agent");
        assert_eq!(upstream.header(":path"), Some(path), "path is not rewritten");
        assert_eq!(upstream.header("a2a-version"), None, "no version header is added");
        assert_eq!(upstream.body(), send.to_string().as_bytes());
        assert_eq!(response.body(), agent_answer.as_bytes(), "native response is unchanged");
    }

    let get = tester.request(UnitHttpRequest::get().with_path("/agent/tasks/t-1"));
    assert_eq!(backend.next().unwrap().header(":path"), Some("/agent/tasks/t-1"));
    assert_eq!(get.body(), agent_answer.as_bytes());
}

#[test]
fn bare_http_json_body_on_another_route_passes_through() {
    let backend = agent("{}");
    let mut tester = shared_tester("1.0", json!({}), &backend);
    let body = json!({ "message": { "messageId": "m", "role": "ROLE_USER", "parts": [] } });

    tester.request(mcp_post_at("/agent/custom", body.clone()));

    assert_eq!(backend.next().unwrap().body(), body.to_string().as_bytes());
}

#[test]
fn native_http_json_is_rejected_when_pass_through_is_off() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(bridge_only_config(json!({})))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let send = tester.request(mcp_post_at("/message:send", json!({ "message": { "parts": [] } })));
    assert_eq!(json_body(&send)["error"]["code"], -32600);
    let get = tester.request(UnitHttpRequest::get().with_path("/tasks/t-1"));
    assert_eq!(get.status_code(), 405);
    assert!(backend.next().is_none());
}

#[test]
fn native_json_rpc_keeps_its_version_semantics() {
    let backend = fixed_agent(SAMPLE_RESPONSE);
    // Bridge configured for v1 with an a2aPath; native calls must ignore both.
    let mut tester = shared_tester("1.0", json!({ "a2aPath": "/a2a" }), &backend);

    let legacy = json!({ "jsonrpc": "2.0", "id": 1, "method": "message/send",
        "params": { "message": { "messageId": "m", "role": "user", "parts": [{ "kind": "text", "text": "x" }] } } });
    let response = tester.request(mcp_post_at("/agent/", legacy.clone()));
    let upstream = backend.next().unwrap();
    assert_eq!(upstream.header("a2a-version"), None, "v0.3 call stays v0.3");
    assert_eq!(upstream.header(":path"), Some("/agent/"));
    assert_eq!(json_body(&upstream), legacy);
    assert_eq!(response.body(), SAMPLE_RESPONSE.as_bytes());

    let v1 = json!({ "jsonrpc": "2.0", "id": 2, "method": "SendMessage", "params": {} });
    tester.request(with_headers(mcp_post_at("/agent/?A2A-Version=1.0", v1.clone()), &[("a2a-version", "1.0")]));
    let upstream = backend.next().unwrap();
    assert_eq!(upstream.header("a2a-version"), Some("1.0"));
    assert_eq!(upstream.header(":path"), Some("/agent/?A2A-Version=1.0"));
    assert_eq!(json_body(&upstream), v1);
}

#[test]
fn translated_v1_call_overrides_a_conflicting_client_version() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = shared_tester("1.0", json!({}), &backend);

    tester.request(with_headers(
        mcp_post_at("/agent/mcp?A2A-Version=0.3&x=1", tools_call(1, json!({ "message": "hi" }))),
        &[("a2a-version", "0.3")],
    ));

    let upstream = backend.next().unwrap();
    assert_eq!(upstream.header("a2a-version"), Some("1.0"));
    assert_eq!(upstream.header(":path"), Some("/agent/mcp?x=1"));
    assert_eq!(json_body(&upstream)["method"], "SendMessage");
}

#[test]
fn translated_v03_call_drops_a_conflicting_client_version() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = shared_tester("0.3", json!({ "a2aPath": "/a2a" }), &backend);

    tester.request(with_headers(
        mcp_post_at("/agent/mcp?a2a-version=1.0", tools_call(1, json!({ "message": "hi" }))),
        &[("a2a-version", "1.0")],
    ));

    let upstream = backend.next().unwrap();
    assert_eq!(upstream.header("a2a-version"), None);
    assert_eq!(upstream.header(":path"), Some("/agent/a2a"));
    let body = json_body(&upstream);
    assert_eq!(body["method"], "message/send");
    assert_eq!(body["params"]["message"]["role"], "user");
}

// ---- Issue #12: sessions are random, registered, identity-bound and scoped.

fn start_session(tester: &mut pdk_unit::UnitTest, headers: &[(&str, &str)]) -> String {
    let init = tester.request(with_headers(
        mcp_post(initialize(1)),
        headers,
    ));
    init.header("mcp-session-id").expect("session issued").to_string()
}

fn bound_config() -> String {
    let mut config: Value = serde_json::from_str(&config("local")).unwrap();
    config["sessionKeyHeader"] = json!("client_id");
    config.to_string()
}

fn call_context(
    tester: &mut pdk_unit::UnitTest,
    backend: &Agent,
    headers: &[(&str, &str)],
) -> Value {
    tester.request(with_headers(mcp_post(tools_call(5, json!({ "message": "next" }))), headers));
    json_body(&backend.next().unwrap())["params"]["message"]["contextId"].clone()
}

/// Asserts that a `tools/call` with these headers gets `404 Session not found` and never reaches
/// the agent.
fn assert_session_not_found(tester: &mut pdk_unit::UnitTest, backend: &Agent, headers: &[(&str, &str)], why: &str) {
    let response = tester.request(with_headers(mcp_post(tools_call(5, json!({ "message": "next" }))), headers));
    assert_eq!(response.status_code(), 404, "{}", why);
    let body = json_body(&response);
    assert_eq!(body["id"], 5, "{}", why);
    assert_eq!(body["error"]["code"], -32001, "{}", why);
    assert!(backend.next().is_none(), "{}: the agent must not be called", why);
}

#[test]
fn identity_binding_is_enforced_not_relaxed() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bound_config())
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let anonymous = tester.request(mcp_post(initialize(1)));
    assert!(anonymous.header("mcp-session-id").is_none(), "no session without the identity header");

    let session = start_session(&mut tester, &[("client_id", "client-a")]);
    let owner = [("mcp-session-id", session.as_str()), ("client_id", "client-a")];
    assert_eq!(call_context(&mut tester, &backend, &owner), Value::Null);
    assert_eq!(call_context(&mut tester, &backend, &owner), "59ec80cd-988c-47fd-8917-1f84dd3bd409");

    let omitted = [("mcp-session-id", session.as_str())];
    assert_session_not_found(&mut tester, &backend, &omitted, "identity omitted");
    let changed = [("mcp-session-id", session.as_str()), ("client_id", "client-b")];
    assert_session_not_found(&mut tester, &backend, &changed, "identity changed");
    assert_eq!(call_context(&mut tester, &backend, &owner), "59ec80cd-988c-47fd-8917-1f84dd3bd409", "owner unaffected");
}

#[test]
fn unissued_session_ids_are_not_found() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let planted = [("mcp-session-id", "chosen-by-the-caller")];
    assert_session_not_found(&mut tester, &backend, &planted, "never issued");
    assert_session_not_found(&mut tester, &backend, &planted, "nothing was stored for it");
    assert_eq!(call_context(&mut tester, &backend, &[]), Value::Null, "calls without a session still work");
}

#[test]
fn delete_ends_a_session_only_for_its_owner() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bound_config())
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);
    let session = start_session(&mut tester, &[("client_id", "client-a")]);
    let owner = [("mcp-session-id", session.as_str()), ("client_id", "client-a")];
    call_context(&mut tester, &backend, &owner);

    let delete = |headers: &[(&str, &str)]| with_headers(UnitHttpRequest::delete().with_path("/mcp"), headers);
    let other = [("mcp-session-id", session.as_str()), ("client_id", "client-b")];
    assert_eq!(tester.request(delete(&other)).status_code(), 404);
    assert_eq!(tester.request(delete(&[("mcp-session-id", session.as_str())])).status_code(), 404);
    assert_eq!(call_context(&mut tester, &backend, &owner), "59ec80cd-988c-47fd-8917-1f84dd3bd409", "still alive");

    assert_eq!(tester.request(delete(&owner)).status_code(), 204);
    assert_session_not_found(&mut tester, &backend, &owner, "ended");
    assert_eq!(tester.request(delete(&owner)).status_code(), 404);
}

#[test]
fn session_ids_are_random_and_ignore_tracing_headers() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let fixed = [("x-request-id", "same-request-id")];
    let ids: std::collections::HashSet<String> = (0..50).map(|_| start_session(&mut tester, &fixed)).collect();
    assert_eq!(ids.len(), 50);
    for id in &ids {
        assert!(!id.contains("same-request-id"));
        assert_eq!(id.len(), 36);
    }

    tester.request(with_headers(mcp_post(tools_call(1, json!({ "message": "hi" }))), &fixed));
    let message_id = json_body(&backend.next().unwrap())["params"]["message"]["messageId"].clone();
    assert!(!message_id.as_str().unwrap().contains("same-request-id"));
}

fn config_with(store: &str, extra: Value) -> String {
    let mut config: Value = serde_json::from_str(&config(store)).unwrap();
    config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    config.to_string()
}

fn tester_for(config: String, backend: &Agent) -> pdk_unit::UnitTest {
    UnitTestBuilder::default()
        .with_config(config)
        .with_backend(Rc::clone(backend))
        .with_entrypoint(crate::configure)
}

// ---- Issue #2: envelopes, ids, params and initialize are validated before anything runs.

#[test]
fn invalid_envelopes_get_errors_and_never_reach_the_agent() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = tester_for(config("local"), &backend);
    let call = |extra: Value| {
        let mut message = tools_call(1, json!({ "message": "hi" }));
        message.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        message
    };

    // (message, HTTP status, JSON-RPC code, id in the error)
    let cases = [
        (call(json!({ "jsonrpc": "1.0" })), 200, -32600, json!(1)),
        (call(json!({ "id": { "a": 1 } })), 400, -32600, Value::Null),
        (call(json!({ "id": null })), 400, -32600, Value::Null),
        (call(json!({ "id": 1.5 })), 400, -32600, Value::Null),
        (call(json!({ "params": [1] })), 200, -32602, json!(1)),
        (json!({ "jsonrpc": "2.0", "id": 2, "method": "ping", "params": 5 }), 200, -32602, json!(2)),
        (json!({ "jsonrpc": "2.0", "id": 3, "method": 7 }), 200, -32600, json!(3)),
        (json!({ "jsonrpc": "2.0", "method": "ping" }), 400, -32600, Value::Null),
        (json!({ "jsonrpc": "2.0", "method": "tools/call", "params": { "name": "ask_agent" } }), 400, -32600, Value::Null),
        (json!({ "jsonrpc": "2.0", "id": 4 }), 200, -32600, json!(4)),
        (json!({ "jsonrpc": "2.0", "id": 5, "result": {}, "error": { "code": 1, "message": "x" } }), 400, -32600, Value::Null),
    ];
    for (message, status, code, id) in cases {
        let response = tester.request(mcp_post(message.clone()));
        assert_eq!(response.status_code(), status, "{}", message);
        let body = json_body(&response);
        assert_eq!(body["jsonrpc"], "2.0", "{}", message);
        assert_eq!(body["error"]["code"], code, "{}", message);
        assert_eq!(body.get("id").cloned().unwrap_or(Value::Null), id, "{}", message);
        if id.is_null() {
            assert!(body.get("id").is_none(), "an unknown id is omitted: {}", message);
        }
    }
    assert!(backend.next().is_none(), "invalid input has no upstream side effects");
}

#[test]
fn notifications_and_responses_are_told_apart_by_structure() {
    let backend = agent("{}");
    let mut tester = tester_for(config("disabled"), &backend);

    // A notification-method message with an id is a request: it gets a reply.
    let with_id = tester.request(mcp_post(json!({ "jsonrpc": "2.0", "id": 4, "method": "notifications/initialized" })));
    assert_eq!(with_id.status_code(), 200);
    assert_eq!(json_body(&with_id)["id"], 4);
    assert_eq!(json_body(&with_id)["error"]["code"], -32601);

    // Notifications and client responses get a bodyless 202.
    for message in [
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } }),
        json!({ "jsonrpc": "2.0", "id": "srv-1", "result": {} }),
        json!({ "jsonrpc": "2.0", "id": "srv-2", "error": { "code": -1, "message": "declined" } }),
    ] {
        let response = tester.request(mcp_post(message.clone()));
        assert_eq!(response.status_code(), 202, "{}", message);
        assert!(response.body().is_empty(), "{}", message);
    }
    assert!(backend.next().is_none());
}

#[test]
fn initialize_requires_well_formed_params_but_negotiates_unknown_versions() {
    let backend = agent("{}");
    let mut tester = tester_for(config("local"), &backend);
    let init = |params: Value| mcp_post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params }));
    let client = json!({ "name": "t", "version": "1" });

    for params in [
        json!({}),
        json!({ "protocolVersion": 20250618, "capabilities": {}, "clientInfo": client }),
        json!({ "protocolVersion": "", "capabilities": {}, "clientInfo": client }),
        json!({ "protocolVersion": "2025-06-18", "clientInfo": client }),
        json!({ "protocolVersion": "2025-06-18", "capabilities": [], "clientInfo": client }),
        json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t" } }),
    ] {
        let response = tester.request(init(params.clone()));
        assert_eq!(json_body(&response)["error"]["code"], -32602, "{}", params);
        assert!(response.header("mcp-session-id").is_none(), "no session for {}", params);
    }

    // A well-formed but unsupported version is negotiated, not rejected.
    let response = tester.request(init(json!({ "protocolVersion": "1999-01-01", "capabilities": {}, "clientInfo": client })));
    assert_eq!(json_body(&response)["result"]["protocolVersion"], "2025-06-18");
    assert!(response.header("mcp-session-id").is_some());
    assert!(backend.next().is_none());
}

// ---- Issue #3: an unsupported MCP-Protocol-Version header is rejected with 400.

#[test]
fn unsupported_protocol_version_header_is_rejected_and_the_client_can_recover() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = tester_for(config("disabled"), &backend);
    let list = json!({ "jsonrpc": "2.0", "id": "l", "method": "tools/list" });
    let version = |message: &Value, v: &str| with_headers(mcp_post(message.clone()), &[("mcp-protocol-version", v)]);

    for bad in ["1999-01-01", "2025-06-18, 2025-03-26", "latest", ""] {
        let response = tester.request(version(&list, bad));
        assert_eq!(response.status_code(), 400, "{:?}", bad);
        assert_eq!(json_body(&response)["error"]["code"], -32600);
        assert_eq!(json_body(&response)["id"], "l");
    }
    let call = tester.request(version(&tools_call(1, json!({ "message": "hi" })), "1999-01-01"));
    assert_eq!(call.status_code(), 400);
    assert!(backend.next().is_none(), "the tool call did not run");

    // Supported or absent headers are accepted, so the client recovers by retrying.
    for good in ["2025-06-18", "2025-03-26", "2024-11-05"] {
        assert_eq!(tester.request(version(&list, good)).status_code(), 200, "{}", good);
    }
    assert_eq!(tester.request(mcp_post(list.clone())).status_code(), 200);
    let retried = tester.request(version(&tools_call(2, json!({ "message": "hi" })), "2025-06-18"));
    assert_eq!(json_body(&retried)["result"]["isError"], false);
    assert!(backend.next().is_some());

    // initialize negotiates in its body, and native A2A has no MCP header semantics.
    let init = tester.request(version(&initialize(3), "1999-01-01"));
    assert_eq!(json_body(&init)["result"]["protocolVersion"], "2025-06-18");
    let native = json!({ "jsonrpc": "2.0", "id": 9, "method": "SendMessage", "params": {} });
    tester.request(version(&native, "1999-01-01"));
    assert_eq!(json_body(&backend.next().expect("native A2A passes through")), native);
}

#[test]
fn delete_checks_the_protocol_version_header() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = tester_for(config("local"), &backend);
    let session = start_session(&mut tester, &[]);
    let delete = |version: &str| {
        with_headers(UnitHttpRequest::delete().with_path("/mcp"), &[("mcp-session-id", session.as_str()), ("mcp-protocol-version", version)])
    };

    assert_eq!(tester.request(delete("1999-01-01")).status_code(), 400);
    assert_eq!(tester.request(delete("2025-06-18")).status_code(), 204, "the session was still live");
}

// ---- Issue #5: a terminated session is gone: 404, and it is not revived.

#[test]
fn terminated_session_gets_404_and_initialize_starts_a_new_one() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = tester_for(config("local"), &backend);
    let session = start_session(&mut tester, &[]);
    let headers = [("mcp-session-id", session.as_str())];

    assert_eq!(call_context(&mut tester, &backend, &headers), Value::Null);
    let delete = with_headers(UnitHttpRequest::delete().with_path("/mcp"), &headers);
    assert_eq!(tester.request(delete).status_code(), 204);

    assert_session_not_found(&mut tester, &backend, &headers, "after DELETE");
    for method in ["ping", "tools/list"] {
        let response = tester.request(with_headers(mcp_post(json!({ "jsonrpc": "2.0", "id": 8, "method": method })), &headers));
        assert_eq!(response.status_code(), 404, "{}", method);
    }
    let notified = tester.request(with_headers(
        mcp_post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
        &headers,
    ));
    assert_eq!(notified.status_code(), 404);
    assert_eq!(json_body(&notified).get("id"), None);

    let fresh = start_session(&mut tester, &headers);
    assert_ne!(fresh, session);
    assert_eq!(call_context(&mut tester, &backend, &[("mcp-session-id", fresh.as_str())]), Value::Null);
    assert_session_not_found(&mut tester, &backend, &headers, "the old id stays ended");
}

#[test]
fn session_ids_are_ignored_without_a_session_store() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = tester_for(config("disabled"), &backend);

    let response = tester.request(with_headers(
        mcp_post(tools_call(1, json!({ "message": "hi" }))),
        &[("mcp-session-id", "from-another-server")],
    ));
    assert_eq!(response.status_code(), 200);
    assert!(backend.next().is_some());
}

// ---- Issue #6: an answer that is not a valid, correlated A2A result is a tool error.

#[test]
fn malformed_or_uncorrelated_agent_answers_are_tool_errors() {
    let task = |id: Value| json!({ "jsonrpc": "2.0", "id": id, "result": { "task": {
        "id": "t-1", "contextId": "ctx-wrong", "status": { "state": "TASK_STATE_COMPLETED" } } } });
    let answers = [
        task(json!("another-request")).to_string(),
        task(json!(2)).to_string(),
        r#"{"jsonrpc":"2.0","id":1,"result":null}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":1,"result":42}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":1,"result":{"message":null}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":1,"result":{"task":null}}"#.to_string(),
        r#"{"jsonrpc":"1.0","id":1,"result":{"message":{"messageId":"m","role":"ROLE_AGENT","parts":[]}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"x"}}"#.to_string(),
    ];
    for answer in answers {
        let backend = fixed_agent(&answer);
        let mut tester = tester_for(config("local"), &backend);
        let session = start_session(&mut tester, &[]);
        let headers = [("mcp-session-id", session.as_str())];

        let response = tester.request(with_headers(mcp_post(tools_call(1, json!({ "message": "hi" }))), &headers));
        let mcp = json_body(&response);
        assert_eq!(mcp["id"], 1, "{}", answer);
        assert_eq!(mcp["result"]["isError"], true, "{}", answer);
        assert!(mcp["result"]["content"][0]["text"].as_str().unwrap().contains("invalid response"), "{}", answer);
        backend.next();

        // Nothing from the invalid answer is remembered.
        tester.request(with_headers(mcp_post(tools_call(1, json!({ "message": "again" }))), &headers));
        assert!(json_body(&backend.next().unwrap())["params"]["message"].get("contextId").is_none(), "{}", answer);
    }
}

// ---- Issue #9: a task without a context id can still be continued.

#[test]
fn input_required_task_without_context_keeps_its_continuation() {
    let reply = json!({ "jsonrpc": "2.0", "id": 0, "result": { "task": { "id": "t9",
        "status": { "state": "TASK_STATE_INPUT_REQUIRED",
                    "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": [{ "text": "Which account?" }] } } } } });
    let backend = agent(&reply.to_string());
    let mut tester = tester_for(config_with("local", json!({ "includeStructuredContent": false })), &backend);
    let session = start_session(&mut tester, &[]);
    let headers = [("mcp-session-id", session.as_str())];

    let response = tester.request(with_headers(mcp_post(tools_call(1, json!({ "message": "pay the bill" }))), &headers));
    let mcp = json_body(&response);
    assert!(mcp["result"].get("structuredContent").is_none());
    let text: Vec<&str> = mcp["result"]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
    assert_eq!(text[0], "Which account?");
    assert!(text[1].contains("input-required") && text[1].contains("task_id=\"t9\""), "{}", text[1]);
    assert!(!text[1].contains("context_id"), "{}", text[1]);
    backend.next();

    // The client answers with the task id alone.
    tester.request(mcp_post(tools_call(2, json!({ "message": "Checking", "task_id": "t9" }))));
    let outbound = json_body(&backend.next().unwrap());
    assert_eq!(outbound["params"]["message"]["taskId"], "t9");
    assert!(outbound["params"]["message"].get("contextId").is_none());

    // Or relies on the session, which remembered the pending task.
    tester.request(with_headers(mcp_post(tools_call(3, json!({ "message": "Checking" }))), &headers));
    let outbound = json_body(&backend.next().unwrap());
    assert_eq!(outbound["params"]["message"]["taskId"], "t9");
}

#[test]
fn auth_required_task_without_context_is_not_answered_with_secrets() {
    let reply = json!({ "jsonrpc": "2.0", "id": 0, "result": { "task": { "id": "t-auth",
        "status": { "state": "TASK_STATE_AUTH_REQUIRED",
                    "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": [{ "text": "Sign in to the CRM." }] } } } } });
    let backend = agent(&reply.to_string());
    let mut tester = tester_for(config("disabled"), &backend);

    let mcp = json_body(&tester.request(mcp_post(tools_call(1, json!({ "message": "show my deals" })))));
    assert_eq!(mcp["result"]["structuredContent"]["state"], "auth-required");
    assert_eq!(mcp["result"]["structuredContent"]["taskId"], "t-auth");
    let hint = mcp["result"]["content"][1]["text"].as_str().unwrap();
    assert!(hint.contains("task_id=\"t-auth\""), "{}", hint);
    assert!(hint.contains("never send passwords, tokens or other secrets"), "{}", hint);
    assert!(hint.contains("replying does not grant authorization"), "{}", hint);
}
