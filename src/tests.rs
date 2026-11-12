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

fn agent(body: &str) -> Rc<TraceBackend<UnitHttpResponse>> {
    Rc::new(TraceBackend::new(
        UnitHttpResponse::new(200)
            .with_header("content-type", "application/json")
            .with_body(body.as_bytes().to_vec()),
    ))
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
    assert!(
        backend.next().is_none(),
        "initialize must not reach the agent"
    );
}

#[test]
fn tools_list_advertises_the_bridge_tool() {
    let backend = agent("{}");
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(
        json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" }),
    ));

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

    let notified = tester.request(mcp_post(
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ));
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
    assert_eq!(
        a2a["params"]["message"]["parts"][0]["text"],
        "Find the page"
    );
    assert_eq!(a2a["params"]["message"]["contextId"], "ctx-given");

    assert_eq!(response.status_code(), 200);
    assert!(response
        .header("content-type")
        .unwrap()
        .contains("application/json"));
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
    let backend =
        agent(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"Task not found"}}"#);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let response = tester.request(mcp_post(tools_call(
        1,
        json!({ "message": "hi", "task_id": "t-x" }),
    )));

    let mcp = json_body(&response);
    assert_eq!(mcp["result"]["isError"], true);
    assert!(mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Task not found"));
}

#[test]
fn a2a_requests_pass_through_untouched() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("disabled"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let request = json!({ "jsonrpc": "2.0", "id": 9, "method": "SendMessage",
        "params": { "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "x" }] } } });
    let response = tester.request(mcp_post(request.clone()));

    assert_eq!(json_body(&backend.next().unwrap()), request);
    assert_eq!(
        json_body(&response),
        serde_json::from_str::<Value>(SAMPLE_RESPONSE).unwrap()
    );
}

#[test]
fn local_store_remembers_context_per_session() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let init = tester.request(mcp_post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    ));
    let session = init
        .header("mcp-session-id")
        .expect("session issued")
        .to_string();

    let call = |id| {
        mcp_post(tools_call(id, json!({ "message": "next" })))
            .with_header("mcp-session-id", session.as_str())
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
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    config.to_string()
}

#[test]
fn passthrough_is_off_by_default() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bridge_only_config(json!({})))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let a2a = tester.request(mcp_post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {} }),
    ));
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

    tester.request(mcp_post_at(
        "/agents-mcp/mcp?tenant=x",
        tools_call(1, json!({ "message": "hi" })),
    ));

    let upstream = backend.next().unwrap();
    assert_eq!(
        upstream.header(":path"),
        Some("/agents-mcp/a2a/my-agent?tenant=x")
    );
}

#[test]
fn root_a2a_path_keeps_the_trailing_slash() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agents-mcp/".to_string()))
        .with_config(bridge_only_config(json!({ "a2aPath": "/" })))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    tester.request(mcp_post_at(
        "/agents-mcp/mcp",
        tools_call(1, json!({ "message": "hi" })),
    ));

    assert_eq!(
        backend.next().unwrap().header(":path"),
        Some("/agents-mcp/")
    );
}

#[test]
fn path_is_untouched_without_a2a_path() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .metadata(|m| m.api_metadata.base_path = Some("/agents-mcp".to_string()))
        .with_config(bridge_only_config(json!({})))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    tester.request(mcp_post_at(
        "/agents-mcp/mcp",
        tools_call(1, json!({ "message": "hi" })),
    ));

    assert_eq!(
        backend.next().unwrap().header(":path"),
        Some("/agents-mcp/mcp")
    );
}

#[test]
fn forward_path_joins_base_and_a2a_path() {
    use crate::resolve_forward_path as fp;
    assert_eq!(
        fp(Some("/agents-mcp"), Some("/a2a/x")),
        Some("/agents-mcp/a2a/x".into())
    );
    assert_eq!(
        fp(Some("/agents-mcp/*"), Some("a2a/x/")),
        Some("/agents-mcp/a2a/x".into())
    );
    assert_eq!(
        fp(Some("/agents-mcp/"), Some("/")),
        Some("/agents-mcp/".into())
    );
    assert_eq!(
        fp(Some("/agents-mcp"), Some("/")),
        Some("/agents-mcp/".into())
    );
    assert_eq!(fp(Some("/"), Some("/")), Some("/".into()));
    assert_eq!(fp(Some("/"), Some("/a2a")), Some("/a2a".into()));
    assert_eq!(fp(Some("/agents-mcp"), Some("  ")), None);
    assert_eq!(fp(None, Some("/a2a")), None);
}

// ---- Issue #1: the converted answer always fits, and is the only thing the client receives.

fn agent_with(content_type: &str, body: Vec<u8>) -> Rc<TraceBackend<UnitHttpResponse>> {
    Rc::new(TraceBackend::new(
        UnitHttpResponse::new(200)
            .with_header("content-type", content_type)
            .with_body(body),
    ))
}

fn task_reply(context_id: &str, parts: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": "broker", "result": { "task": {
        "id": "t-1", "contextId": context_id,
        "status": { "state": "TASK_STATE_COMPLETED",
                    "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": parts } } } } })
}

fn assert_bounded_mcp_answer(response: &pdk_unit::UnitHttpResponse, id: i64) -> Value {
    assert_eq!(response.status_code(), 200);
    assert!(response
        .header("content-type")
        .unwrap()
        .contains("application/json"));
    assert!(
        response.body().len() < crate::mcp::MAX_BODY_BYTES,
        "{} bytes",
        response.body().len()
    );
    let mcp = json_body(response);
    assert_eq!(mcp["jsonrpc"], "2.0");
    assert_eq!(mcp["id"], id);
    assert!(
        mcp["result"]["content"][0]["text"].is_string(),
        "MCP result.content is present"
    );
    assert!(
        mcp["result"].get("task").is_none(),
        "the raw A2A task must not leak"
    );
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
    assert!(mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with("Ask for a shorter answer.]"));
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

    let first = tester.request(with_headers(
        mcp_post(tools_call(2, json!({ "message": "a" }))),
        &[("mcp-session-id", &session)],
    ));
    let mcp = assert_bounded_mcp_answer(&first, 2);
    assert_eq!(mcp["result"]["isError"], true);
    backend.next();

    tester.request(with_headers(
        mcp_post(tools_call(3, json!({ "message": "b" }))),
        &[("mcp-session-id", &session)],
    ));
    let second = json_body(&backend.next().unwrap());
    assert!(
        second["params"]["message"].get("contextId").is_none(),
        "session was not updated"
    );
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
    let mut config =
        json!({ "toolName": "ask_agent", "a2aProtocolVersion": version, "passThroughA2A": true });
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    config.to_string()
}

fn shared_tester(
    version: &str,
    extra: Value,
    backend: &Rc<TraceBackend<UnitHttpResponse>>,
) -> pdk_unit::UnitTest {
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
    for path in [
        "/agent/message:send",
        "/agent/tenant-1/message:send",
        "/agent/tasks/t-1:cancel",
    ] {
        let response = tester.request(mcp_post_at(path, send.clone()));
        let upstream = backend.next().expect("reached the agent");
        assert_eq!(
            upstream.header(":path"),
            Some(path),
            "path is not rewritten"
        );
        assert_eq!(
            upstream.header("a2a-version"),
            None,
            "no version header is added"
        );
        assert_eq!(upstream.body(), send.to_string().as_bytes());
        assert_eq!(
            response.body(),
            agent_answer.as_bytes(),
            "native response is unchanged"
        );
    }

    let get = tester.request(UnitHttpRequest::get().with_path("/agent/tasks/t-1"));
    assert_eq!(
        backend.next().unwrap().header(":path"),
        Some("/agent/tasks/t-1")
    );
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

    let send = tester.request(mcp_post_at(
        "/message:send",
        json!({ "message": { "parts": [] } }),
    ));
    assert_eq!(json_body(&send)["error"]["code"], -32600);
    let get = tester.request(UnitHttpRequest::get().with_path("/tasks/t-1"));
    assert_eq!(get.status_code(), 405);
    assert!(backend.next().is_none());
}

#[test]
fn native_json_rpc_keeps_its_version_semantics() {
    let backend = agent(SAMPLE_RESPONSE);
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
    tester.request(with_headers(
        mcp_post_at("/agent/?A2A-Version=1.0", v1.clone()),
        &[("a2a-version", "1.0")],
    ));
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
        mcp_post_at(
            "/agent/mcp?A2A-Version=0.3&x=1",
            tools_call(1, json!({ "message": "hi" })),
        ),
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
        mcp_post_at(
            "/agent/mcp?a2a-version=1.0",
            tools_call(1, json!({ "message": "hi" })),
        ),
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
        mcp_post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })),
        headers,
    ));
    init.header("mcp-session-id")
        .expect("session issued")
        .to_string()
}

fn bound_config() -> String {
    let mut config: Value = serde_json::from_str(&config("local")).unwrap();
    config["sessionKeyHeader"] = json!("client_id");
    config.to_string()
}

fn call_context(
    tester: &mut pdk_unit::UnitTest,
    backend: &Rc<TraceBackend<UnitHttpResponse>>,
    headers: &[(&str, &str)],
) -> Value {
    tester.request(with_headers(
        mcp_post(tools_call(5, json!({ "message": "next" }))),
        headers,
    ));
    json_body(&backend.next().unwrap())["params"]["message"]["contextId"].clone()
}

#[test]
fn identity_binding_is_enforced_not_relaxed() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bound_config())
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let anonymous = tester.request(mcp_post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    ));
    assert!(
        anonymous.header("mcp-session-id").is_none(),
        "no session without the identity header"
    );

    let session = start_session(&mut tester, &[("client_id", "client-a")]);
    let owner = [
        ("mcp-session-id", session.as_str()),
        ("client_id", "client-a"),
    ];
    assert_eq!(call_context(&mut tester, &backend, &owner), Value::Null);
    assert_eq!(
        call_context(&mut tester, &backend, &owner),
        "59ec80cd-988c-47fd-8917-1f84dd3bd409"
    );

    let omitted = [("mcp-session-id", session.as_str())];
    assert_eq!(
        call_context(&mut tester, &backend, &omitted),
        Value::Null,
        "identity omitted"
    );
    let changed = [
        ("mcp-session-id", session.as_str()),
        ("client_id", "client-b"),
    ];
    assert_eq!(
        call_context(&mut tester, &backend, &changed),
        Value::Null,
        "identity changed"
    );
}

#[test]
fn unissued_session_ids_are_ignored() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(config("local"))
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);

    let planted = [("mcp-session-id", "chosen-by-the-caller")];
    assert_eq!(call_context(&mut tester, &backend, &planted), Value::Null);
    assert_eq!(
        call_context(&mut tester, &backend, &planted),
        Value::Null,
        "nothing was stored for it"
    );
}

#[test]
fn delete_ends_a_session_only_for_its_owner() {
    let backend = agent(SAMPLE_RESPONSE);
    let mut tester = UnitTestBuilder::default()
        .with_config(bound_config())
        .with_backend(Rc::clone(&backend))
        .with_entrypoint(crate::configure);
    let session = start_session(&mut tester, &[("client_id", "client-a")]);
    let owner = [
        ("mcp-session-id", session.as_str()),
        ("client_id", "client-a"),
    ];
    call_context(&mut tester, &backend, &owner);

    let delete = |headers: &[(&str, &str)]| {
        with_headers(UnitHttpRequest::delete().with_path("/mcp"), headers)
    };
    let other = [
        ("mcp-session-id", session.as_str()),
        ("client_id", "client-b"),
    ];
    assert_eq!(tester.request(delete(&other)).status_code(), 404);
    assert_eq!(
        tester
            .request(delete(&[("mcp-session-id", session.as_str())]))
            .status_code(),
        404
    );
    assert_eq!(
        call_context(&mut tester, &backend, &owner),
        "59ec80cd-988c-47fd-8917-1f84dd3bd409",
        "still alive"
    );

    assert_eq!(tester.request(delete(&owner)).status_code(), 204);
    assert_eq!(
        call_context(&mut tester, &backend, &owner),
        Value::Null,
        "ended"
    );
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
    let ids: std::collections::HashSet<String> = (0..50)
        .map(|_| start_session(&mut tester, &fixed))
        .collect();
    assert_eq!(ids.len(), 50);
    for id in &ids {
        assert!(!id.contains("same-request-id"));
        assert_eq!(id.len(), 36);
    }

    tester.request(with_headers(
        mcp_post(tools_call(1, json!({ "message": "hi" }))),
        &fixed,
    ));
    let message_id = json_body(&backend.next().unwrap())["params"]["message"]["messageId"].clone();
    assert!(!message_id.as_str().unwrap().contains("same-request-id"));
}
