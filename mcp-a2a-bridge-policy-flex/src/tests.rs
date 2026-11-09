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
    let backend = agent(SAMPLE_RESPONSE);
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

    let init = tester.request(mcp_post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })));
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
