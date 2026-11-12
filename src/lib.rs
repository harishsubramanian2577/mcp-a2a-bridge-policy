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
//! With `enableElicitation` (experimental, off by default) an `input-required` question is asked
//! through MCP elicitation instead; see [`elicitation`].

mod a2a;
mod elicitation;
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
use crate::elicitation::{Answer, Ending, Pending, Settings, Step};
use crate::generated::config::Config;
use crate::mcp::ToolCall;
use crate::session::{SessionEntry, SessionStore};

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
    /// Set when elicitation is enabled and there is a session store to hold questions.
    elicitation: Option<Settings>,
    /// Store namespace; also hashed into every session key.
    session_scope: String,
    session_key_header: Option<String>,
    /// Path (base path + `a2aPath`) that forwarded calls are rewritten to.
    forward_path: Option<String>,
    id_counter: Cell<u64>,
}

impl Bridge {
    fn new(config: Config, builder: &DataStorageBuilder, metadata: &Metadata) -> Self {
        let base_path = metadata.api_metadata.base_path.as_deref();
        let ttl = config.session_ttl_seconds.unwrap_or(1800).max(0) as u64;
        let mode = config.session_store_mode.as_deref().unwrap_or("disabled");
        let enable_elicitation = config.enable_elicitation.unwrap_or(false);
        let forward_path = resolve_forward_path(base_path, config.a_2_a_path.as_deref());
        if forward_path.is_none()
            && base_path.is_none()
            && config
                .a_2_a_path
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty())
        {
            logger::warn!(
                "a2aPath is set but the API base path is unknown; paths will not be rewritten."
            );
        }
        let policy = &metadata.policy_metadata;
        let session_scope = session::namespace(
            metadata.api_metadata.id.as_deref(),
            base_path,
            &format!("{}.{}", policy.policy_name, policy.policy_namespace),
        );
        let sessions =
            SessionStore::from_config(builder, mode, ttl, &session_scope, enable_elicitation);
        let elicitation = Settings::from_config(
            enable_elicitation,
            sessions.is_some(),
            config.elicitation_timeout_seconds,
            config.max_elicitation_rounds,
            session::clamp_ttl(ttl),
        );
        if enable_elicitation && elicitation.is_none() {
            logger::warn!(
                "enableElicitation needs a session store; questions are returned as tool results."
            );
        }
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
            sessions,
            elicitation,
            session_scope,
            session_key_header: config
                .session_key_header
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
            forward_path,
            id_counter: Cell::new(0),
        }
    }

    /// A2A `messageId`. Random when the host provides randomness; otherwise still unique per
    /// worker. It only identifies the message, so it need not be secret.
    fn message_id(&self) -> String {
        if let Some(id) = session::random_uuid() {
            return id;
        }
        let n = self.id_counter.get().wrapping_add(1);
        self.id_counter.set(n);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:x}-{:x}", nanos, n)
    }
}

/// The gateway maps `<base path><rest>` to `<upstream URL path><rest>`, so rewriting the request
/// path to `<base path><a2aPath>` sends the call to `<upstream URL path><a2aPath>` whatever path
/// the MCP client used. The route still matches because the base path prefix is kept; the
/// upstream root keeps its trailing slash because the gateway route is `<base path>/`.
fn resolve_forward_path(base_path: Option<&str>, a2a_path: Option<&str>) -> Option<String> {
    let a2a_path = a2a_path.map(str::trim).filter(|p| !p.is_empty())?;
    let base = base_path?.trim_end_matches('*').trim_end_matches('/');
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
    mode: Mode,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    /// A JSON `CallToolResult`.
    Plain,
    /// A `tools/call` that may ask its question through elicitation: answered as SSE.
    MayElicit,
    /// The user's answer to question `round` of `stream`, sent to the agent. The client gets 202;
    /// the agent's reply waits in the store for the resumed stream.
    Answer { stream: String, round: u32 },
}

/// The caller identity a session is bound to (`sessionKeyHeader`).
enum Identity {
    /// No identity binding is configured.
    NotRequired,
    Present(String),
    /// Binding is configured but the header is missing: no session is used or issued.
    Missing,
}

impl Identity {
    fn of(bridge: &Bridge, handler: &dyn HeadersHandler) -> Self {
        let Some(name) = bridge.session_key_header.as_deref() else {
            return Identity::NotRequired;
        };
        match handler.header(name).filter(|v| !v.trim().is_empty()) {
            Some(value) => Identity::Present(value),
            None => Identity::Missing,
        }
    }

    /// The store key of `session_id` for this caller, if the caller may use sessions at all.
    fn session_key(&self, bridge: &Bridge, session_id: &str) -> Option<String> {
        if !session::is_valid_session_id(session_id) {
            return None;
        }
        let identity = match self {
            Identity::NotRequired => None,
            Identity::Present(value) => Some(value.as_str()),
            Identity::Missing => return None,
        };
        Some(session::session_key(
            &bridge.session_scope,
            session_id,
            identity,
        ))
    }
}

/// Request-side facts read from the headers before the body is classified.
struct RequestHeaders {
    session_id: Option<String>,
    identity: Identity,
    protocol_version: Option<String>,
    path: String,
    accepts_sse: bool,
}

fn accepts_sse(handler: &dyn HeadersHandler) -> bool {
    handler
        .header("accept")
        .map(|a| {
            a.to_ascii_lowercase()
                .contains(elicitation::SSE_CONTENT_TYPE)
        })
        .unwrap_or(false)
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
    Flow::Break(json_response(
        200,
        &mcp::result_envelope(id, result),
        vec![],
    ))
}

/// An HTTP-level rejection with a JSON-RPC error body; the id is included when one was read.
fn http_error(
    status: u32,
    id: Option<&Value>,
    code: i64,
    message: &str,
) -> Flow<Option<Transcoded>> {
    Flow::Break(json_response(
        status,
        &mcp::error_body(id, code, message),
        vec![],
    ))
}

fn invalid_message(invalid: mcp::Invalid) -> Flow<Option<Transcoded>> {
    match &invalid.id {
        Some(id) => rpc_error(id, invalid.code, &invalid.message),
        None => http_error(400, None, invalid.code, &invalid.message),
    }
}

/// Streamable HTTP answers a request for an unknown or terminated session with 404, which tells
/// the client to start a new session with `initialize`.
fn session_not_found(id: Option<&Value>) -> Flow<Option<Transcoded>> {
    http_error(404, id, mcp::SESSION_NOT_FOUND, "Session not found")
}

fn plain_status(status: u32) -> Flow<Option<Transcoded>> {
    Flow::Break(Response::new(status))
}

fn method_not_allowed() -> Flow<Option<Transcoded>> {
    Flow::Break(Response::new(405).with_headers(vec![("allow".to_string(), "POST".to_string())]))
}

fn sse_headers() -> Vec<(String, String)> {
    vec![
        (
            "content-type".to_string(),
            elicitation::SSE_CONTENT_TYPE.to_string(),
        ),
        ("cache-control".to_string(), "no-cache".to_string()),
    ]
}

fn sse_response(body: Vec<u8>) -> Flow<Option<Transcoded>> {
    Flow::Break(
        Response::new(200)
            .with_headers(sse_headers())
            .with_body(body),
    )
}

async fn request_filter(request_state: RequestState, bridge: &Bridge) -> Flow<Option<Transcoded>> {
    let headers_state = request_state.into_headers_state().await;
    let handler = headers_state.handler();
    let http_method = headers_state.method();
    let path = headers_state.path();

    // The A2A HTTP+JSON binding is recognised from its route and forwarded untouched.
    if bridge.pass_through_a2a && a2a::is_http_json_route(&http_method, &path) {
        return Flow::Continue(None);
    }

    match http_method.as_str() {
        "POST" => {}
        "GET" => {
            // MCP clients open GET for server-initiated SSE; this bridge never pushes, so 405
            // is the spec answer, except to resume the stream of a question (elicitation).
            // Other GETs (e.g. the A2A agent card) belong to the agent.
            let wants_sse = accepts_sse(handler);
            if let (true, Some(_), Some(last_event_id)) = (
                wants_sse,
                &bridge.elicitation,
                handler.header(elicitation::LAST_EVENT_ID_HEADER),
            ) {
                return resume_stream(bridge, handler, &last_event_id).await;
            }
            if wants_sse || !bridge.pass_through_a2a {
                return method_not_allowed();
            }
            return Flow::Continue(None);
        }
        "DELETE" => {
            // MCP session termination.
            if let Some(session_id) = handler.header(mcp::SESSION_ID_HEADER) {
                let Some(store) = &bridge.sessions else {
                    return method_not_allowed();
                };
                let version = handler.header(mcp::PROTOCOL_VERSION_HEADER);
                if let Err(message) = mcp::check_protocol_version_header(version.as_deref()) {
                    return http_error(400, None, mcp::INVALID_REQUEST, &message);
                }
                let key = Identity::of(bridge, handler).session_key(bridge, &session_id);
                return match key {
                    Some(key) if store.load(&key).await.is_some() => {
                        store.remove(&key).await;
                        plain_status(204)
                    }
                    _ => plain_status(404),
                };
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
        return pass_or_reject_media(bridge);
    }
    if !headers_state.contains_body() {
        return http_error(
            400,
            None,
            mcp::INVALID_REQUEST,
            "Empty JSON-RPC request body",
        );
    }
    let headers = RequestHeaders {
        session_id: handler.header(mcp::SESSION_ID_HEADER),
        identity: Identity::of(bridge, handler),
        protocol_version: handler.header(mcp::PROTOCOL_VERSION_HEADER),
        path,
        accepts_sse: accepts_sse(handler),
    };

    // Headers stay writable until the body is classified, so only translated calls change them.
    let state = headers_state.into_headers_body_state().await;
    let handler = state.handler();
    let body = handler.body();

    let rpc: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) if bridge.pass_through_a2a => {
            logger::debug!("Body is not JSON; passing through untouched.");
            return Flow::Continue(None);
        }
        Err(_) => return http_error(400, None, mcp::PARSE_ERROR, "Parse error"),
    };
    let Some(envelope) = rpc.as_object() else {
        return http_error(
            400,
            None,
            mcp::INVALID_REQUEST,
            "JSON-RPC batch requests are not supported",
        );
    };

    // Native A2A is recognised before any MCP validation: a bare HTTP+JSON payload on a route not
    // recognised above, or an A2A JSON-RPC method, keeps its own path, version header and body.
    let method = envelope.get("method").and_then(Value::as_str);
    let native_a2a = !envelope.contains_key("jsonrpc") || method.is_some_and(a2a::is_a2a_method);
    if bridge.pass_through_a2a && native_a2a {
        return Flow::Continue(None);
    }

    // Nothing below reaches the agent unless the message is a valid MCP request.
    let incoming = match mcp::classify(envelope) {
        Ok(incoming) => incoming,
        Err(invalid) => return invalid_message(invalid),
    };
    let request_id = match &incoming {
        mcp::Incoming::Request { id, .. } => Some(*id),
        _ => None,
    };
    let is_initialize = matches!(
        incoming,
        mcp::Incoming::Request {
            method: mcp::INITIALIZE_METHOD_NAME,
            ..
        }
    );
    if !is_initialize {
        if let Err(message) =
            mcp::check_protocol_version_header(headers.protocol_version.as_deref())
        {
            return http_error(400, request_id, mcp::INVALID_REQUEST, &message);
        }
    }

    // A session id must name a live session of this caller; `initialize` starts a new one.
    let mut session = None;
    if let (Some(store), Some(session_id), false) =
        (&bridge.sessions, &headers.session_id, is_initialize)
    {
        let key = headers.identity.session_key(bridge, session_id);
        let entry = match &key {
            Some(key) => store.load(key).await,
            None => None,
        };
        match (key, entry) {
            (Some(key), Some(entry)) => session = Some((key, entry)),
            _ => {
                logger::debug!("Mcp-Session-Id is unknown, expired or not this caller's.");
                return session_not_found(request_id);
            }
        }
    }

    let (id, method, params) = match incoming {
        mcp::Incoming::Request { id, method, params } => (id.clone(), method, params),
        // Accepted notifications and client responses get 202 with no body; an answer to an
        // elicitation goes on to the agent.
        mcp::Incoming::Response => {
            if let (Some(settings), Some((key, _))) = (&bridge.elicitation, session) {
                return answer_elicitation(bridge, settings, envelope, key, &headers, handler)
                    .await;
            }
            return plain_status(202);
        }
        mcp::Incoming::Notification { method } => {
            if let (true, Some(_), Some((key, _))) = (
                method == mcp::CANCELLED_NOTIFICATION,
                &bridge.elicitation,
                &session,
            ) {
                cancel_elicitation(bridge, key, envelope.get("params")).await;
            }
            return plain_status(202);
        }
    };
    if a2a::is_a2a_method(method) {
        return rpc_error(
            &id,
            mcp::METHOD_NOT_FOUND,
            "A2A methods are not accepted on this MCP endpoint",
        );
    }
    match method {
        mcp::INITIALIZE_METHOD_NAME => match mcp::initialize_protocol_version(params) {
            Ok(requested) => {
                let elicit = bridge.elicitation.is_some()
                    && elicitation::version_supports(mcp::negotiate_protocol_version(Some(
                        requested,
                    )))
                    && elicitation::client_declares(params);
                initialize(bridge, &id, requested, &headers, elicit).await
            }
            Err(message) => rpc_error(&id, mcp::INVALID_PARAMS, message),
        },
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
            transcode_tool_call(bridge, id, call, session, &headers, handler).await
        }
        _ => rpc_error(
            &id,
            mcp::METHOD_NOT_FOUND,
            &format!("Method not found: {}", method),
        ),
    }
}

fn pass_or_reject(bridge: &Bridge) -> Flow<Option<Transcoded>> {
    if bridge.pass_through_a2a {
        Flow::Continue(None)
    } else {
        method_not_allowed()
    }
}

fn pass_or_reject_media(bridge: &Bridge) -> Flow<Option<Transcoded>> {
    if bridge.pass_through_a2a {
        Flow::Continue(None)
    } else {
        plain_status(415)
    }
}

async fn initialize(
    bridge: &Bridge,
    id: &Value,
    requested: &str,
    headers: &RequestHeaders,
    elicit: bool,
) -> Flow<Option<Transcoded>> {
    let result = mcp::initialize_result(
        Some(requested),
        &bridge.server_name,
        SERVER_VERSION,
        bridge.server_instructions.as_deref(),
    );
    let extra = match issue_session(bridge, &headers.identity, elicit).await {
        Some(session_id) => vec![(mcp::SESSION_ID_HEADER.to_string(), session_id)],
        None => vec![],
    };
    Flow::Break(json_response(200, &mcp::result_envelope(id, result), extra))
}

/// A session id is only issued when there is a store to key on it, the caller identity is
/// available (if binding is configured) and the id is registered, so only issued ids are honoured.
async fn issue_session(bridge: &Bridge, identity: &Identity, elicit: bool) -> Option<String> {
    let store = bridge.sessions.as_ref()?;
    if matches!(identity, Identity::Missing) {
        logger::debug!("Session identity header is missing; no MCP session issued.");
        return None;
    }
    let Some(session_id) = session::random_uuid() else {
        logger::warn!("No secure randomness available; no MCP session issued.");
        return None;
    };
    let key = identity.session_key(bridge, &session_id)?;
    store.register(&key, elicit).await.then_some(session_id)
}

async fn transcode_tool_call(
    bridge: &Bridge,
    id: Value,
    call: ToolCall,
    session: Option<(String, SessionEntry)>,
    headers: &RequestHeaders,
    handler: &dyn HeadersBodyHandler,
) -> Flow<Option<Transcoded>> {
    // `session` is the caller's live session, already resolved by the request filter.
    let (session_key, entry) = match session {
        Some((key, entry)) => (Some(key), Some(entry)),
        None => (None, None),
    };

    // Explicit arguments win; the store only fills in a missing context.
    let (context_id, task_id) = match (&call.context_id, entry) {
        (Some(ctx), _) => (Some(ctx.clone()), call.task_id.clone()),
        (None, Some(entry)) if !call.new_conversation => {
            (entry.context_id, call.task_id.clone().or(entry.task_id))
        }
        _ => (None, call.task_id.clone()),
    };

    let a2a_request = a2a::build_send_message(
        bridge.a2a_version,
        &id,
        &bridge.message_id(),
        &call.message,
        context_id.as_deref(),
        task_id.as_deref(),
    )
    .to_string();
    if a2a_request.len() >= mcp::MAX_BODY_BYTES {
        return rpc_error(
            &id,
            mcp::INVALID_PARAMS,
            "The message is too large to forward to the agent",
        );
    }

    // A new call supersedes any question still open in this session.
    let mut mode = Mode::Plain;
    if let (Some(store), Some(key), Some(_)) = (&bridge.sessions, &session_key, &bridge.elicitation)
    {
        if store.load_elicitation(key).await.is_some() {
            store
                .update_elicitation(key, |entry| entry.pending.take().is_some())
                .await;
            if headers.accepts_sse {
                mode = Mode::MayElicit;
            }
        }
    }

    if forward_to_agent(bridge, handler, headers, &a2a_request).is_err() {
        return rpc_error(
            &id,
            mcp::INTERNAL_ERROR,
            "Failed to forward the call to the A2A agent",
        );
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
        mode,
    }))
}

/// Replaces the request with `a2a_request` for the agent. The generated body decides the
/// protocol, so path and version follow it, not the client.
fn forward_to_agent(
    bridge: &Bridge,
    handler: &dyn HeadersBodyHandler,
    headers: &RequestHeaders,
    a2a_request: &str,
) -> Result<(), ()> {
    if let Err(err) = handler.set_body(a2a_request.as_bytes()) {
        logger::error!(
            "Failed to rewrite the request into an A2A request: {:?}",
            err
        );
        return Err(());
    }
    handler.remove_header("content-length");
    let target = match &bridge.forward_path {
        Some(forward_path) => with_query(forward_path, &headers.path),
        None => headers.path.clone(),
    };
    let target = a2a::strip_version_query(&target);
    if target != headers.path {
        handler.set_header(":path", &target);
    }
    match bridge.a2a_version {
        A2aVersion::V1 => handler.set_header(a2a::A2A_VERSION_HEADER, a2a::A2A_V1_HEADER_VALUE),
        A2aVersion::V03 => handler.remove_header(a2a::A2A_VERSION_HEADER),
    }
    Ok(())
}

/// The open question of this session, if the client is still expected to answer or resume it.
async fn pending_of(store: &SessionStore, key: &str) -> Option<Pending> {
    store
        .load_elicitation(key)
        .await
        .and_then(|entry| entry.pending)
}

/// Settles the question with `result` as the `tools/call` answer. `end` also drops the stored
/// context and task, so the next call starts a new conversation. Returns the latest state, which
/// is another request's if that one settled it first.
async fn settle(
    store: &SessionStore,
    key: &str,
    pending: &Pending,
    result: Value,
    end: bool,
) -> Option<Pending> {
    let response = mcp::result_envelope(&pending.call_id(), result).to_string();
    let mut settled = None;
    let stored = store
        .update_elicitation(key, |entry| match entry.pending.as_mut() {
            Some(p) if p.stream == pending.stream && p.is_open() => {
                p.finish(response);
                settled = Some(p.clone());
                true
            }
            _ => false,
        })
        .await;
    if !stored {
        return pending_of(store, key).await;
    }
    if end {
        store.update(key, None, None).await;
    }
    logger::debug!("Elicitation settled (conversation ended: {}).", end);
    settled
}

async fn end_conversation(
    bridge: &Bridge,
    store: &SessionStore,
    key: &str,
    pending: &Pending,
    ending: Ending,
) -> Option<Pending> {
    let result = elicitation::ended_result(ending, bridge.structured_content);
    settle(store, key, pending, result, true).await
}

/// The client's JSON-RPC response to `elicitation/create`. Only the current question of this
/// caller's session is answered; anything else is acknowledged and ignored.
async fn answer_elicitation(
    bridge: &Bridge,
    settings: &Settings,
    envelope: &serde_json::Map<String, Value>,
    key: String,
    headers: &RequestHeaders,
    handler: &dyn HeadersBodyHandler,
) -> Flow<Option<Transcoded>> {
    let Some(store) = &bridge.sessions else {
        return plain_status(202);
    };
    let Some(request_id) = envelope.get("id").and_then(Value::as_str) else {
        return plain_status(202);
    };
    let Some(pending) = pending_of(store, &key)
        .await
        .filter(|p| p.asked_request_id() == Some(request_id))
    else {
        logger::debug!("Response to an unknown or settled elicitation; ignored.");
        return plain_status(202);
    };
    let answer = match elicitation::parse_answer(envelope) {
        Ok(answer) => answer,
        Err(message) => return http_error(400, None, mcp::INVALID_PARAMS, &message),
    };
    if session::now_seconds() >= pending.deadline {
        end_conversation(bridge, store, &key, &pending, Ending::TimedOut).await;
        return plain_status(202);
    }
    let text = match answer {
        Answer::Accept(text) => text,
        Answer::Decline => {
            end_conversation(bridge, store, &key, &pending, Ending::Declined).await;
            return plain_status(202);
        }
        Answer::Cancel => {
            end_conversation(bridge, store, &key, &pending, Ending::Cancelled).await;
            return plain_status(202);
        }
        Answer::Failed => {
            // The client could not ask; the question goes back as a normal tool result.
            let reply = elicitation::client_managed_reply(&pending);
            let result =
                a2a::tool_result_from_reply(&reply, &bridge.tool_name, bridge.structured_content);
            settle(store, &key, &pending, result, false).await;
            return plain_status(202);
        }
    };

    let call_id = pending.call_id();
    let a2a_request = a2a::build_send_message(
        bridge.a2a_version,
        &call_id,
        &bridge.message_id(),
        &text,
        pending.context_id.as_deref(),
        Some(&pending.task_id),
    )
    .to_string();
    if a2a_request.len() >= mcp::MAX_BODY_BYTES {
        return http_error(
            400,
            None,
            mcp::INVALID_PARAMS,
            "The answer is too large to forward to the agent",
        );
    }
    // Claiming the question first means a repeated answer is never sent to the agent twice.
    let claimed = store
        .update_elicitation(&key, |entry| match entry.pending.as_mut() {
            Some(p) if p.asked_request_id() == Some(request_id) => {
                p.step = Step::Answering;
                true
            }
            _ => false,
        })
        .await;
    if !claimed {
        return plain_status(202);
    }
    if forward_to_agent(bridge, handler, headers, &a2a_request).is_err() {
        let result = mcp::tool_error_result("Failed to forward the answer to the A2A agent");
        settle(store, &key, &pending, result, false).await;
        return plain_status(202);
    }
    logger::debug!(
        "Elicitation answer -> A2A {:?} (round {} of {})",
        bridge.a2a_version,
        pending.round,
        settings.max_rounds
    );
    Flow::Continue(Some(Transcoded {
        rpc_id: call_id,
        session_key: Some(key),
        mode: Mode::Answer {
            stream: pending.stream,
            round: pending.round,
        },
    }))
}

/// `notifications/cancelled` for a `tools/call` with an open question: the client gave up on
/// the call, so the question and the conversation end. No response is sent for a cancelled call.
async fn cancel_elicitation(bridge: &Bridge, key: &str, params: Option<&Value>) {
    let Some(store) = &bridge.sessions else {
        return;
    };
    let Some(request_id) = params.and_then(|p| p.get("requestId")) else {
        return;
    };
    let Some(pending) = pending_of(store, key).await else {
        return;
    };
    if pending.call_id() != *request_id || !pending.is_open() {
        return;
    }
    let cleared = store
        .update_elicitation(key, |entry| {
            let open = entry
                .pending
                .as_ref()
                .is_some_and(|p| p.stream == pending.stream && p.is_open());
            if open {
                entry.pending = None;
            }
            open
        })
        .await;
    if cleared {
        store.update(key, None, None).await;
    }
}

/// GET with `Last-Event-ID`: the client resumes the stream of a `tools/call` that asked a
/// question, and gets whatever is new since that event, or a `retry` hint to come back later.
async fn resume_stream(
    bridge: &Bridge,
    handler: &dyn HeadersHandler,
    last_event_id: &str,
) -> Flow<Option<Transcoded>> {
    let Some(store) = &bridge.sessions else {
        return method_not_allowed();
    };
    let version = handler.header(mcp::PROTOCOL_VERSION_HEADER);
    if let Err(message) = mcp::check_protocol_version_header(version.as_deref()) {
        return http_error(400, None, mcp::INVALID_REQUEST, &message);
    }
    let Some(session_id) = handler.header(mcp::SESSION_ID_HEADER) else {
        return http_error(
            400,
            None,
            mcp::INVALID_REQUEST,
            "Mcp-Session-Id is required to resume a stream",
        );
    };
    let key = Identity::of(bridge, handler).session_key(bridge, &session_id);
    let live = match &key {
        Some(key) => store.load(key).await.is_some(),
        None => false,
    };
    let (Some(key), true) = (key, live) else {
        return session_not_found(None);
    };
    let unknown = || {
        http_error(
            400,
            None,
            mcp::INVALID_REQUEST,
            "No stream to resume for this Last-Event-ID",
        )
    };
    let Some((stream, last_seq)) = elicitation::parse_event_id(last_event_id) else {
        return unknown();
    };
    let Some(mut pending) = pending_of(store, &key).await.filter(|p| p.stream == stream) else {
        return unknown();
    };
    if pending.is_open() && session::now_seconds() >= pending.deadline {
        match end_conversation(bridge, store, &key, &pending, Ending::TimedOut).await {
            Some(latest) if latest.stream == pending.stream => pending = latest,
            _ => return unknown(),
        }
    }
    let fresh = last_seq < pending.seq;
    match (&pending.step, fresh) {
        (Step::Asked { .. }, true) => {
            sse_response(elicitation::question_stream(&pending).unwrap_or_default())
        }
        (Step::Done { .. }, true) => {
            sse_response(elicitation::result_stream(&pending).unwrap_or_default())
        }
        (Step::Done { .. }, false) => unknown(),
        _ => sse_response(elicitation::waiting_stream()),
    }
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
        if let Mode::Answer { stream, round } = &transcoded.mode {
            settle_answer(bridge, &transcoded, stream, *round, Err(result)).await;
            headers_state.send_response(Response::new(202));
            return;
        }
        headers_state.send_response(json_response(
            200,
            &mcp::result_envelope(&transcoded.rpc_id, result),
            vec![],
        ));
        return;
    }

    // Headers are committed before the body is read, so the content type follows the mode: the
    // MCP client always gets an in-band JSON-RPC answer at HTTP 200, as JSON or as an SSE stream
    // when a question may be asked, and an answer to a question gets a bodyless 202.
    let handler = headers_state.handler();
    handler.remove_header("content-length");
    match &transcoded.mode {
        Mode::Plain => {
            handler.set_header(":status", "200");
            handler.set_header("content-type", JSON_CONTENT_TYPE);
        }
        Mode::MayElicit => {
            handler.set_header(":status", "200");
            for (name, value) in sse_headers() {
                handler.set_header(&name, &value);
            }
        }
        Mode::Answer { .. } => {
            handler.set_header(":status", "202");
            handler.remove_header("content-type");
        }
    }

    let body_state = headers_state.into_body_state().await;
    let body = body_state.handler().body();
    let write = |bytes: &[u8]| match body_state.handler().set_body(bytes) {
        Ok(()) => true,
        Err(err) => {
            logger::error!("Failed to write the MCP result: {:?}", err);
            false
        }
    };

    let parsed = a2a::parse_agent_response(status, &content_type, &body, &transcoded.rpc_id);
    if let Err(message) = &parsed {
        logger::warn!("A2A call failed: {}", message);
    }

    match &transcoded.mode {
        Mode::Answer { stream, round } => {
            let outcome = parsed.map_err(|message| mcp::tool_error_result(&message));
            settle_answer(bridge, &transcoded, stream, *round, outcome).await;
            write(b"");
            return;
        }
        Mode::MayElicit => {
            if let Some(reply) = parsed
                .as_ref()
                .ok()
                .filter(|r| elicitation::is_elicitable(r))
            {
                if let Some(stream) = start_elicitation(bridge, &transcoded, reply).await {
                    write(&stream);
                    return;
                }
            }
        }
        Mode::Plain => {}
    }

    let (result, reply) = match parsed {
        Ok(reply) => (
            a2a::tool_result_from_reply(&reply, &bridge.tool_name, bridge.structured_content),
            Some(reply),
        ),
        Err(message) => (mcp::tool_error_result(&message), None),
    };
    // The body must be replaced on every path: a bounded answer, or a small tool error if even
    // that cannot be written.
    let sse = transcoded.mode == Mode::MayElicit;
    let delivered = mcp::deliver_result(&transcoded.rpc_id, result, |bytes| {
        if sse {
            write(&elicitation::sse_event(None, bytes))
        } else {
            write(bytes)
        }
    });
    match reply {
        Some(reply) if delivered => remember(bridge, &transcoded, &reply).await,
        Some(_) => {
            logger::warn!("The agent's answer could not be returned in full; session not updated.")
        }
        None => {}
    }
}

/// Stores the agent's question and returns the SSE stream that asks it, or `None` to answer the
/// call the client-managed way (no randomness, or the store refused the write).
async fn start_elicitation(
    bridge: &Bridge,
    transcoded: &Transcoded,
    reply: &a2a::AgentReply,
) -> Option<Vec<u8>> {
    let (Some(store), Some(key), Some(settings)) = (
        &bridge.sessions,
        &transcoded.session_key,
        &bridge.elicitation,
    ) else {
        return None;
    };
    let deadline = session::now_seconds() + settings.timeout_seconds;
    let pending = Pending::first(session::random_uuid()?, &transcoded.rpc_id, reply, deadline)?;
    let stream = elicitation::question_stream(&pending)?;
    let stored = store
        .update_elicitation(key, |entry| {
            entry.pending = Some(pending);
            true
        })
        .await;
    if !stored {
        return None;
    }
    remember(bridge, transcoded, reply).await;
    Some(stream)
}

/// Records the agent's reply to the user's answer for the resumed stream: its next question
/// (while rounds remain) or the `tools/call` result. A reply for a question that was settled in
/// the meantime (timeout, cancel, a newer call) is dropped.
async fn settle_answer(
    bridge: &Bridge,
    transcoded: &Transcoded,
    stream: &str,
    round: u32,
    outcome: Result<a2a::AgentReply, Value>,
) {
    let (Some(store), Some(key), Some(settings)) = (
        &bridge.sessions,
        &transcoded.session_key,
        &bridge.elicitation,
    ) else {
        return;
    };
    let ask_again = matches!(&outcome, Ok(reply) if elicitation::is_elicitable(reply))
        && round < settings.max_rounds;
    let (response, fit) = match &outcome {
        _ if ask_again => (String::new(), mcp::Fit::Full),
        Ok(reply) => {
            let result =
                a2a::tool_result_from_reply(reply, &bridge.tool_name, bridge.structured_content);
            let (bytes, fit) =
                mcp::bounded_result_envelope(&transcoded.rpc_id, result, mcp::MAX_BODY_BYTES);
            (String::from_utf8_lossy(&bytes).into_owned(), fit)
        }
        Err(result) => (
            mcp::result_envelope(&transcoded.rpc_id, result.clone()).to_string(),
            mcp::Fit::Replaced,
        ),
    };
    let deadline = session::now_seconds() + settings.timeout_seconds;
    let stored = store
        .update_elicitation(key, |entry| match entry.pending.as_mut() {
            Some(p) if p.stream == stream && p.round == round && p.step == Step::Answering => {
                match &outcome {
                    Ok(reply) if ask_again => p.ask_again(reply, deadline),
                    _ => p.finish(response),
                }
                true
            }
            _ => false,
        })
        .await;
    match outcome {
        Ok(reply) if stored && fit != mcp::Fit::Replaced => {
            remember(bridge, transcoded, &reply).await
        }
        _ if !stored => {
            logger::debug!("The question was settled before the agent answered; reply dropped.")
        }
        _ => {}
    }
}

/// Records the reply's context and, while the agent waits on the caller, its task. A task-only
/// reply is remembered too, since the agent infers the context from the task id.
async fn remember(bridge: &Bridge, transcoded: &Transcoded, reply: &a2a::AgentReply) {
    let (Some(store), Some(key)) = (&bridge.sessions, &transcoded.session_key) else {
        return;
    };
    let pending_task = reply
        .task_id
        .clone()
        .filter(|_| reply.state.awaits_client());
    if reply.context_id.is_none() && pending_task.is_none() {
        return;
    }
    store
        .update(key, reply.context_id.clone(), pending_task)
        .await;
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
    let bridge = Bridge::new(config, &store_builder, &metadata);

    let filter = on_request(|rs| request_filter(rs, &bridge))
        .on_response(|rs, request_data| response_filter(rs, request_data, &bridge));
    launcher.launch(filter).await?;
    Ok(())
}
