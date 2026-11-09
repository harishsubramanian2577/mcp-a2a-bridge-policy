// Copyright 2026 Salesforce, Inc. All rights reserved.
//! MCP A2A Bridge Policy.
//!
//! Makes an A2A-only upstream (MuleSoft Agent Broker) reachable by MCP clients:
//! - answers `initialize`, `ping`, `tools/list` and notifications locally;
//! - rewrites `tools/call` of the bridge tool into an A2A `SendMessage` (v1.0) / `message/send` (v0.3);
//! - rewrites the A2A Task/Message answer into an MCP `CallToolResult`.
//!
//! Multi-turn state travels in the tool arguments (`context_id`, `task_id`). An optional
//! session store remembers the last context per `Mcp-Session-Id` for clients that do not echo it.

mod a2a;
mod generated;
mod mcp;
mod session;
#[cfg(test)]
mod tests;

use std::cell::Cell;

use anyhow::{anyhow, Result};
use pdk::data_storage::DataStorageBuilder;
use pdk::hl::*;
use pdk::logger;
use pdk::metadata::Metadata;
use serde_json::{json, Value};

use crate::a2a::A2aVersion;
use crate::generated::config::Config;
use crate::mcp::ToolCall;
use crate::session::SessionStore;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const JSON_CONTENT_TYPE: &str = "application/json";

struct Bridge {
    tool_name: String,
    tool_description: String,
    server_name: String,
    server_instructions: Option<String>,
    a2a_version: A2aVersion,
    pass_through_a2a: bool,
    structured_content: bool,
    sessions: Option<SessionStore>,
    session_key_header: Option<String>,
    /// Path (base path + `a2aPath`) that forwarded calls are rewritten to.
    forward_path: Option<String>,
    id_counter: Cell<u64>,
}

impl Bridge {
    fn new(config: Config, builder: &DataStorageBuilder, base_path: Option<&str>) -> Self {
        let ttl = config.session_ttl_seconds.unwrap_or(1800).max(60) as u64;
        let mode = config.session_store_mode.as_deref().unwrap_or("disabled");
        Bridge {
            tool_name: config.tool_name,
            tool_description: config.tool_description.unwrap_or_default(),
            server_name: config
                .server_name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "mcp-a2a-bridge".to_string()),
            server_instructions: config.server_instructions.filter(|s| !s.is_empty()),
            a2a_version: A2aVersion::from_config(&config.a_2_a_protocol_version),
            pass_through_a2a: config.pass_through_a_2_a.unwrap_or(false),
            structured_content: config.include_structured_content.unwrap_or(true),
            sessions: SessionStore::from_config(builder, mode, ttl),
            session_key_header: config
                .session_key_header
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
            forward_path: resolve_forward_path(base_path, config.a_2_a_path.as_deref()),
            id_counter: Cell::new(0),
        }
    }

    /// Envoy's `x-request-id` is a random UUID, unique per request; the counter keeps ids
    /// unique when it is absent or reused by the client.
    fn new_id(&self, request_id: Option<&str>) -> String {
        let n = self.id_counter.get().wrapping_add(1);
        self.id_counter.set(n);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        match request_id.filter(|r| !r.is_empty()) {
            Some(r) => format!("{}-{:x}", r, n),
            None => format!("{:x}-{:x}", nanos, n),
        }
    }
}

/// The gateway maps `<base path><rest>` to `<upstream URL path><rest>`, so rewriting the request
/// path to `<base path><a2aPath>` sends the call to `<upstream URL path><a2aPath>` whatever path
/// the MCP client used. The route still matches because the base path prefix is kept; the
/// upstream root keeps its trailing slash because the gateway route is `<base path>/`.
fn resolve_forward_path(base_path: Option<&str>, a2a_path: Option<&str>) -> Option<String> {
    let a2a_path = a2a_path.map(str::trim).filter(|p| !p.is_empty())?;
    let Some(base_path) = base_path else {
        logger::warn!("a2aPath is set but the API base path is unknown; paths will not be rewritten.");
        return None;
    };
    let base = base_path.trim_end_matches('*').trim_end_matches('/');
    let rest = a2a_path.trim_matches('/');
    Some(if rest.is_empty() {
        format!("{}/", base)
    } else {
        format!("{}/{}", base, rest)
    })
}

fn with_query(path: &str, original: &str) -> String {
    match original.split_once('?') {
        Some((_, query)) => format!("{}?{}", path, query),
        None => path.to_string(),
    }
}

/// Carried from the request filter to the response filter for transcoded `tools/call`s.
#[derive(Debug, Clone)]
struct Transcoded {
    rpc_id: Value,
    session_key: Option<String>,
}

/// Request-side facts that must be captured before the body is read, because headers are
/// immutable once the exchange enters the body state.
struct RequestHeaders {
    session_id: Option<String>,
    request_id: Option<String>,
    identity: Option<String>,
}

fn json_response(status: u32, body: &Value, extra_headers: Vec<(String, String)>) -> Response {
    let mut headers = vec![("content-type".to_string(), JSON_CONTENT_TYPE.to_string())];
    headers.extend(extra_headers);
    Response::new(status)
        .with_headers(headers)
        .with_body(body.to_string().into_bytes())
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Flow<Option<Transcoded>> {
    Flow::Break(json_response(
        200,
        &mcp::error_envelope(id, code, message),
        vec![],
    ))
}

fn rpc_result(id: &Value, result: Value) -> Flow<Option<Transcoded>> {
    Flow::Break(json_response(200, &mcp::result_envelope(id, result), vec![]))
}

fn plain_status(status: u32) -> Flow<Option<Transcoded>> {
    Flow::Break(Response::new(status))
}

fn method_not_allowed() -> Flow<Option<Transcoded>> {
    Flow::Break(Response::new(405).with_headers(vec![("allow".to_string(), "POST".to_string())]))
}

async fn request_filter(request_state: RequestState, bridge: &Bridge) -> Flow<Option<Transcoded>> {
    let headers_state = request_state.into_headers_state().await;
    let handler = headers_state.handler();
    let http_method = headers_state.method();

    match http_method.as_str() {
        "POST" => {}
        "GET" => {
            // MCP clients open GET for server-initiated SSE; this bridge never pushes, so 405
            // is the spec answer. Other GETs (e.g. the A2A agent card) belong to the agent.
            let wants_sse = handler
                .header("accept")
                .map(|a| a.contains("text/event-stream"))
                .unwrap_or(false);
            if wants_sse || !bridge.pass_through_a2a {
                return method_not_allowed();
            }
            return Flow::Continue(None);
        }
        "DELETE" => {
            // MCP session termination.
            if let Some(session_id) = handler.header(mcp::SESSION_ID_HEADER) {
                if let Some(store) = &bridge.sessions {
                    let identity = identity_of(bridge, handler);
                    store
                        .remove(&session::session_key(&session_id, identity.as_deref()))
                        .await;
                    return plain_status(204);
                }
                return method_not_allowed();
            }
            return pass_or_reject(bridge);
        }
        _ => return pass_or_reject(bridge),
    }

    let is_json = handler
        .header("content-type")
        .map(|ct| ct.to_ascii_lowercase().contains("json"))
        .unwrap_or(false);
    if !is_json {
        return if bridge.pass_through_a2a {
            Flow::Continue(None)
        } else {
            plain_status(415)
        };
    }
    if !headers_state.contains_body() {
        return rpc_error(&Value::Null, mcp::INVALID_REQUEST, "Empty JSON-RPC request body");
    }

    // The body may be rewritten into an A2A request, so settle every header now.
    handler.remove_header("content-length");
    if bridge.a2a_version == A2aVersion::V1 && handler.header(a2a::A2A_VERSION_HEADER).is_none() {
        handler.set_header(a2a::A2A_VERSION_HEADER, a2a::A2A_V1_HEADER_VALUE);
    }
    // Only forwarded calls reach the upstream, so rewriting every JSON POST here is safe:
    // locally answered methods never use the path.
    if let Some(forward_path) = &bridge.forward_path {
        handler.set_header(":path", &with_query(forward_path, &headers_state.path()));
    }
    let headers = RequestHeaders {
        session_id: handler.header(mcp::SESSION_ID_HEADER),
        request_id: handler.header("x-request-id"),
        identity: identity_of(bridge, handler),
    };

    let body_state = headers_state.into_body_state().await;
    let body = body_state.handler().body();

    let rpc: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) if bridge.pass_through_a2a => {
            logger::debug!("Body is not JSON; passing through untouched.");
            return Flow::Continue(None);
        }
        Err(_) => return rpc_error(&Value::Null, mcp::PARSE_ERROR, "Parse error"),
    };
    let Some(envelope) = rpc.as_object() else {
        return rpc_error(
            &Value::Null,
            mcp::INVALID_REQUEST,
            "JSON-RPC batch requests are not supported",
        );
    };
    let id = envelope.get("id").cloned().unwrap_or(Value::Null);

    let Some(method) = envelope.get("method").and_then(Value::as_str) else {
        // A client response to a server request; this server never sends any.
        if envelope.contains_key("result") || envelope.contains_key("error") {
            return plain_status(202);
        }
        return rpc_error(&id, mcp::INVALID_REQUEST, "Missing JSON-RPC method");
    };

    if a2a::is_a2a_method(method) {
        if bridge.pass_through_a2a {
            return Flow::Continue(None);
        }
        return rpc_error(&id, mcp::METHOD_NOT_FOUND, "A2A methods are not accepted on this MCP endpoint");
    }
    if mcp::is_notification(method) {
        return plain_status(202);
    }

    let params = envelope.get("params");
    match method {
        mcp::INITIALIZE_METHOD_NAME => initialize(bridge, &id, params, &headers),
        mcp::PING_METHOD_NAME => rpc_result(&id, json!({})),
        mcp::TOOLS_LIST_METHOD_NAME => rpc_result(
            &id,
            mcp::tools_list_result(mcp::tool_definition(
                &bridge.tool_name,
                &bridge.tool_description,
                bridge.sessions.is_some(),
            )),
        ),
        mcp::TOOLS_CALL_METHOD_NAME => {
            let tool = params.and_then(|p| p.get("name")).and_then(Value::as_str);
            if tool != Some(bridge.tool_name.as_str()) {
                return rpc_error(
                    &id,
                    mcp::INVALID_PARAMS,
                    &format!("Unknown tool: {}", tool.unwrap_or("<missing>")),
                );
            }
            let call = match mcp::parse_tool_arguments(params.and_then(|p| p.get("arguments"))) {
                Ok(call) => call,
                Err(message) => return rpc_error(&id, mcp::INVALID_PARAMS, &message),
            };
            transcode_tool_call(bridge, id, call, &headers, &body_state).await
        }
        _ => rpc_error(&id, mcp::METHOD_NOT_FOUND, &format!("Method not found: {}", method)),
    }
}

fn pass_or_reject(bridge: &Bridge) -> Flow<Option<Transcoded>> {
    if bridge.pass_through_a2a {
        Flow::Continue(None)
    } else {
        method_not_allowed()
    }
}

fn identity_of(bridge: &Bridge, handler: &dyn HeadersHandler) -> Option<String> {
    bridge
        .session_key_header
        .as_deref()
        .and_then(|name| handler.header(name))
}

fn initialize(
    bridge: &Bridge,
    id: &Value,
    params: Option<&Value>,
    headers: &RequestHeaders,
) -> Flow<Option<Transcoded>> {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let result = mcp::initialize_result(
        requested,
        &bridge.server_name,
        SERVER_VERSION,
        bridge.server_instructions.as_deref(),
    );
    // A session id is only useful (and only issued) when there is a store to key on it.
    let extra = match bridge.sessions {
        Some(_) => vec![(
            mcp::SESSION_ID_HEADER.to_string(),
            bridge.new_id(headers.request_id.as_deref()),
        )],
        None => vec![],
    };
    Flow::Break(json_response(200, &mcp::result_envelope(id, result), extra))
}

async fn transcode_tool_call(
    bridge: &Bridge,
    id: Value,
    call: ToolCall,
    headers: &RequestHeaders,
    body_state: &RequestBodyState,
) -> Flow<Option<Transcoded>> {
    let session_key = match (&bridge.sessions, &headers.session_id) {
        (Some(_), Some(sid)) => Some(session::session_key(sid, headers.identity.as_deref())),
        _ => None,
    };

    // Explicit arguments win; the store only fills in a missing context.
    let (context_id, task_id) = match (&call.context_id, &bridge.sessions, &session_key) {
        (Some(ctx), _, _) => (Some(ctx.clone()), call.task_id.clone()),
        (None, Some(store), Some(key)) if !call.new_conversation => match store.load(key).await {
            Some(entry) => (Some(entry.context_id), call.task_id.clone().or(entry.task_id)),
            None => (None, call.task_id.clone()),
        },
        _ => (None, call.task_id.clone()),
    };

    let message_id = bridge.new_id(headers.request_id.as_deref());
    let a2a_request = a2a::build_send_message(
        bridge.a2a_version,
        &id,
        &message_id,
        &call.message,
        context_id.as_deref(),
        task_id.as_deref(),
    );
    if let Err(err) = body_state
        .handler()
        .set_body(a2a_request.to_string().as_bytes())
    {
        logger::error!("Failed to rewrite tools/call into an A2A request: {:?}", err);
        return rpc_error(&id, mcp::INTERNAL_ERROR, "Failed to forward the call to the A2A agent");
    }
    logger::debug!(
        "tools/call -> A2A {:?} (context: {:?}, task: {:?})",
        bridge.a2a_version,
        context_id,
        task_id
    );
    Flow::Continue(Some(Transcoded {
        rpc_id: id,
        session_key,
    }))
}

async fn response_filter(
    response_state: ResponseState,
    request_data: RequestData<Option<Transcoded>>,
    bridge: &Bridge,
) {
    let RequestData::Continue(Some(transcoded)) = request_data else {
        return;
    };
    let headers_state = response_state.into_headers_state().await;
    let status = headers_state.status_code();
    let content_type = headers_state
        .handler()
        .header("content-type")
        .unwrap_or_default();

    if !headers_state.contains_body() {
        let result = mcp::tool_error_result(&format!(
            "The A2A agent returned HTTP {} with an empty body.",
            status
        ));
        headers_state.send_response(json_response(
            200,
            &mcp::result_envelope(&transcoded.rpc_id, result),
            vec![],
        ));
        return;
    }

    // The MCP client always gets an in-band JSON-RPC answer at HTTP 200.
    let handler = headers_state.handler();
    handler.remove_header("content-length");
    handler.set_header(":status", "200");
    handler.set_header("content-type", JSON_CONTENT_TYPE);

    let body_state = headers_state.into_body_state().await;
    let body = body_state.handler().body();

    let result = match a2a::parse_agent_response(status, &content_type, &body) {
        Ok(reply) => {
            remember(bridge, &transcoded, &reply).await;
            a2a::tool_result_from_reply(&reply, &bridge.tool_name, bridge.structured_content)
        }
        Err(message) => {
            logger::warn!("A2A call failed: {}", message);
            mcp::tool_error_result(&message)
        }
    };
    let envelope = mcp::result_envelope(&transcoded.rpc_id, result);
    if let Err(err) = body_state.handler().set_body(envelope.to_string().as_bytes()) {
        logger::error!("Failed to rewrite the A2A response into an MCP result: {:?}", err);
    }
}

async fn remember(bridge: &Bridge, transcoded: &Transcoded, reply: &a2a::AgentReply) {
    let (Some(store), Some(key), Some(context_id)) =
        (&bridge.sessions, &transcoded.session_key, &reply.context_id)
    else {
        return;
    };
    let pending_task = reply
        .task_id
        .clone()
        .filter(|_| reply.state.awaits_client());
    store.save(key, context_id.clone(), pending_task).await;
}

#[entrypoint]
async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    store_builder: DataStorageBuilder,
    metadata: Metadata,
) -> Result<()> {
    let config: Config = serde_json::from_slice(&bytes).map_err(|err| {
        anyhow!(
            "Failed to parse configuration '{}'. Cause: {}",
            String::from_utf8_lossy(&bytes),
            err
        )
    })?;
    let bridge = Bridge::new(
        config,
        &store_builder,
        metadata.api_metadata.base_path.as_deref(),
    );

    let filter = on_request(|rs| request_filter(rs, &bridge))
        .on_response(|rs, request_data| response_filter(rs, request_data, &bridge));
    launcher.launch(filter).await?;
    Ok(())
}
