// Copyright 2026 Salesforce, Inc. All rights reserved.

mod common;

use httpmock::Method::POST;
use httpmock::MockServer;
use pdk_test::port::Port;
use pdk_test::services::flex::{ApiConfig, Flex, FlexConfig, PolicyConfig};
use pdk_test::services::httpmock::{HttpMock, HttpMockConfig};
use pdk_test::{pdk_test, TestComposite};
use serde_json::{json, Value};

use common::*;

// Flex port for the internal test network
const FLEX_PORT: Port = 8081;

const SAMPLE_RESPONSE: &str = include_str!("fixtures/a2a_v1_send_message_response.json");

// A real Flex instance in front of a mocked A2A agent: the MCP tools/call must reach the agent
// as an A2A SendMessage and come back as an MCP tool result. Local mode has no API base path, so
// a2aPath cannot be applied here; the client calls the agent's path and the unit tests cover the
// rewrite.
#[pdk_test]
async fn tools_call_round_trip() -> anyhow::Result<()> {
    let httpmock_config = HttpMockConfig::builder()
        .port(80)
        .version("latest")
        .hostname("backend")
        .build();

    let policy_config = PolicyConfig::builder()
        .name(POLICY_NAME)
        .configuration(json!({
            "toolName": "ask_agent",
            "a2aProtocolVersion": "1.0",
            "passThroughA2A": false,
            "a2aPath": "",
        }))
        .build();

    let api_config = ApiConfig::builder()
        .name("mcpBridge")
        .upstream(&httpmock_config)
        .path("/")
        .port(FLEX_PORT)
        .policies([policy_config])
        .build();

    let flex_config = FlexConfig::builder()
        .version("1.11.0")
        .hostname("local-flex")
        .with_api(api_config)
        .config_mounts([(POLICY_DIR, "policy"), (COMMON_CONFIG_DIR, "common")])
        .build();

    let composite = TestComposite::builder()
        .with_service(flex_config)
        .with_service(httpmock_config)
        .build()
        .await?;

    let flex: Flex = composite.service()?;
    let flex_url = flex.external_url(FLEX_PORT).unwrap();
    let httpmock: HttpMock = composite.service()?;
    let mock_server = MockServer::connect_async(httpmock.socket()).await;

    let agent = mock_server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/a2a/my-agent")
                .header("a2a-version", "1.0")
                .body_contains("\"SendMessage\"");
            then.status(200)
                .header("content-type", "application/json")
                .body(SAMPLE_RESPONSE);
        })
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{flex_url}/a2a/my-agent"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "ask_agent", "arguments": { "message": "Find the page" } }
            })
            .to_string(),
        )
        .send()
        .await?;

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await?;
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(
        body["result"]["structuredContent"]["contextId"],
        "59ec80cd-988c-47fd-8917-1f84dd3bd409"
    );
    agent.assert_async().await;

    Ok(())
}
