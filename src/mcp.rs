// Copyright 2026 Salesforce, Inc. All rights reserved.
//! MCP (Model Context Protocol) server-side vocabulary. The upstream Agent Broker only speaks
//! A2A, so this policy answers the MCP handshake itself and exposes exactly one tool.

use serde_json::{json, Map, Value};

pub const JSONRPC_VERSION: &str = "2.0";

pub const INITIALIZE_METHOD_NAME: &str = "initialize";
pub const PING_METHOD_NAME: &str = "ping";
pub const TOOLS_LIST_METHOD_NAME: &str = "tools/list";
pub const TOOLS_CALL_METHOD_NAME: &str = "tools/call";
pub const NOTIFICATION_METHOD_PREFIX: &str = "notifications/";
pub const CANCELLED_NOTIFICATION: &str = "notifications/cancelled";

pub const SESSION_ID_HEADER: &str = "mcp-session-id";
pub const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Newest first. An unknown client version is answered with the newest one, per the MCP
/// version-negotiation rules; the client decides whether it can proceed.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// Body code of the HTTP 404 for an unknown or terminated session, as the MCP SDKs use it.
pub const SESSION_NOT_FOUND: i64 = -32001;

pub const ARG_MESSAGE: &str = "message";
pub const ARG_CONTEXT_ID: &str = "context_id";
pub const ARG_TASK_ID: &str = "task_id";
pub const ARG_NEW_CONVERSATION: &str = "new_conversation";

pub fn is_notification(method: &str) -> bool {
    method.starts_with(NOTIFICATION_METHOD_PREFIX)
}

/// A JSON-RPC message from the MCP client, classified by its structure rather than its method.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming<'a> {
    Request {
        id: &'a Value,
        method: &'a str,
        params: Option<&'a Map<String, Value>>,
    },
    Notification { method: &'a str },
    /// The client's answer to a server request.
    Response,
}

/// A message that cannot be processed. `id` is set only when a valid request id was read; the
/// answer is then an in-band JSON-RPC error, otherwise an HTTP 400 with an id-less error.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub id: Option<Value>,
    pub code: i64,
    pub message: String,
}

impl Invalid {
    fn new(id: Option<&Value>, code: i64, message: &str) -> Self {
        Invalid {
            id: id.cloned(),
            code,
            message: message.to_string(),
        }
    }
}

/// MCP request ids are strings or integers; `null`, fractions, booleans, objects and arrays are not.
fn is_valid_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

/// Classifies a JSON-RPC 2.0 message: a request has a method and an id, a notification a method
/// and no id, a response an id and exactly one of `result` / `error`. Params, when present, must
/// be an object. Only `notifications/*` methods are accepted as notifications: any other method
/// sent without an id is a request missing its id.
pub fn classify(envelope: &Map<String, Value>) -> Result<Incoming<'_>, Invalid> {
    let id = envelope.get("id");
    let request_id = id.filter(|id| is_valid_id(id));
    if envelope.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(Invalid::new(request_id, INVALID_REQUEST, "Not a JSON-RPC 2.0 message"));
    }
    if id.is_some() && request_id.is_none() && envelope.contains_key("method") {
        return Err(Invalid::new(None, INVALID_REQUEST, "The id must be a string or an integer"));
    }

    let Some(method) = envelope.get("method") else {
        let has_result = envelope.contains_key("result");
        let has_error = envelope.contains_key("error");
        if !has_result && !has_error {
            return Err(Invalid::new(request_id, INVALID_REQUEST, "Missing JSON-RPC method"));
        }
        // An error may answer a request whose id the client could not read.
        let id_ok = request_id.is_some() || (has_error && id == Some(&Value::Null));
        if has_result == has_error || !id_ok {
            return Err(Invalid::new(None, INVALID_REQUEST, "Invalid JSON-RPC response"));
        }
        return Ok(Incoming::Response);
    };
    let Some(method) = method.as_str() else {
        return Err(Invalid::new(request_id, INVALID_REQUEST, "The method must be a string"));
    };
    let params = match envelope.get("params") {
        None => None,
        Some(Value::Object(params)) => Some(params),
        Some(_) => return Err(Invalid::new(request_id, INVALID_PARAMS, "Params must be an object")),
    };
    match request_id {
        Some(id) => Ok(Incoming::Request { id, method, params }),
        None if is_notification(method) => Ok(Incoming::Notification { method }),
        None => Err(Invalid::new(None, INVALID_REQUEST, "Requests must have an id")),
    }
}

/// Streamable HTTP: every message after `initialize` names the negotiated version in
/// `MCP-Protocol-Version`. An absent header is accepted (the server then assumes 2025-03-26);
/// an unsupported or malformed one is rejected with HTTP 400.
pub fn check_protocol_version_header(value: Option<&str>) -> Result<(), String> {
    match value {
        None => Ok(()),
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v.trim()) => Ok(()),
        Some(_) => Err(format!(
            "Unsupported MCP-Protocol-Version header. Supported versions: {}",
            SUPPORTED_PROTOCOL_VERSIONS.join(", ")
        )),
    }
}

/// Checks the `initialize` parameters and returns the requested protocol version. Missing or
/// mistyped fields are invalid params; a well-formed but unknown version is negotiated instead.
pub fn initialize_protocol_version(
    params: Option<&Map<String, Value>>,
) -> Result<&str, &'static str> {
    let params = params.ok_or("initialize requires params")?;
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or("initialize params.protocolVersion must be a non-empty string")?;
    if !params.get("capabilities").map_or(false, Value::is_object) {
        return Err("initialize params.capabilities must be an object");
    }
    let client_info = params
        .get("clientInfo")
        .and_then(Value::as_object)
        .ok_or("initialize params.clientInfo must be an object")?;
    if !client_info.get("name").map_or(false, Value::is_string) {
        return Err("initialize params.clientInfo.name must be a string");
    }
    if !client_info.get("version").map_or(false, Value::is_string) {
        return Err("initialize params.clientInfo.version must be a string");
    }
    Ok(version)
}

pub fn result_envelope(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

pub fn error_envelope(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": { "code": code, "message": message } })
}

/// The JSON-RPC error body of an HTTP-level rejection. Without a request id the `id` member is
/// left out, as Streamable HTTP allows for errors that answer no particular request.
pub fn error_body(id: Option<&Value>, code: i64, message: &str) -> Value {
    match id {
        Some(id) => error_envelope(id, code, message),
        None => json!({ "jsonrpc": JSONRPC_VERSION, "error": { "code": code, "message": message } }),
    }
}

pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r).copied())
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

pub fn initialize_result(
    requested_version: Option<&str>,
    server_name: &str,
    server_version: &str,
    instructions: Option<&str>,
) -> Value {
    let mut result = json!({
        "protocolVersion": negotiate_protocol_version(requested_version),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": server_name, "version": server_version },
    });
    if let Some(text) = instructions.filter(|t| !t.is_empty()) {
        result["instructions"] = Value::String(text.to_string());
    }
    result
}

/// The single tool that fronts the A2A agent. `new_conversation` is only advertised when a
/// server-side session store exists, because without one there is nothing to reset.
pub fn tool_definition(name: &str, description: &str, session_store_enabled: bool) -> Value {
    let mut properties = json!({
        ARG_MESSAGE: {
            "type": "string",
            "description": "The request, or the reply to the agent's question, in natural language."
        },
        ARG_CONTEXT_ID: {
            "type": "string",
            "description": "context_id returned by a previous call. Pass it to continue that conversation; omit it to start a new one."
        },
        ARG_TASK_ID: {
            "type": "string",
            "description": "task_id returned by a previous call whose state was input-required or auth-required. Pass it, together with context_id when one was returned, to answer the agent's follow-up question."
        }
    });
    if session_store_enabled {
        properties[ARG_NEW_CONVERSATION] = json!({
            "type": "boolean",
            "description": "Set to true to start a new conversation instead of continuing the one remembered for this MCP session."
        });
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": [ARG_MESSAGE]
        }
    })
}

pub fn tools_list_result(tool: Value) -> Value {
    json!({ "tools": [tool] })
}

/// Arguments of a `tools/call` aimed at the bridge tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub message: String,
    pub context_id: Option<String>,
    pub task_id: Option<String>,
    pub new_conversation: bool,
}

pub fn parse_tool_arguments(arguments: Option<&Value>) -> Result<ToolCall, String> {
    let empty = Map::new();
    let args = match arguments {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(map)) => map,
        Some(_) => return Err("Tool arguments must be a JSON object".to_string()),
    };

    let message = match args.get(ARG_MESSAGE) {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::String(_)) | None => {
            return Err(format!("Missing required argument `{}`", ARG_MESSAGE))
        }
        Some(_) => return Err(format!("Argument `{}` must be a string", ARG_MESSAGE)),
    };

    Ok(ToolCall {
        message,
        context_id: optional_id(args, ARG_CONTEXT_ID)?,
        task_id: optional_id(args, ARG_TASK_ID)?,
        new_conversation: args
            .get(ARG_NEW_CONVERSATION)
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// LLMs frequently send `""` or `null` for "no value"; treat both as absent.
fn optional_id(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(format!("Argument `{}` must be a string", key)),
    }
}

pub fn tool_error_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// PDK refuses to write a body of 1 MiB or more; converted answers stay clear of that.
pub const MAX_BODY_BYTES: usize = 1_000_000;
/// Room for the truncation note and JSON escaping of the bytes around the cut.
const TRUNCATION_MARGIN: usize = 1024;
const TRUNCATION_NOTE: &str =
    "\n\n[Answer truncated: it exceeded the gateway's 1 MB response limit. Ask for a shorter answer.]";
const TOO_LARGE_MESSAGE: &str =
    "The agent's answer exceeded the gateway's 1 MB response limit and could not be returned. Ask for a shorter answer.";
const UNDELIVERABLE_MESSAGE: &str =
    "The gateway could not convert the agent's answer into an MCP result.";

/// How much of the converted result survived the size limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Unchanged.
    Full,
    /// `structuredContent.data` dropped and/or the answer text truncated; still a real answer.
    Reduced,
    /// Replaced by a tool error; nothing of the answer is left.
    Replaced,
}

/// Serialises a `tools/call` result envelope that is always under `limit` bytes. Shrinks in
/// order: drop `structuredContent.data` (data parts are also in the text), truncate the answer
/// text, then replace the result with a tool error. Removing k raw bytes of a JSON string removes
/// at least k serialised bytes, so one truncation pass is enough.
pub fn bounded_result_envelope(id: &Value, result: Value, limit: usize) -> (Vec<u8>, Fit) {
    let mut envelope = result_envelope(id, result);
    let bytes = envelope.to_string().into_bytes();
    if bytes.len() < limit {
        return (bytes, Fit::Full);
    }

    if let Some(sc) = envelope
        .pointer_mut("/result/structuredContent")
        .and_then(Value::as_object_mut)
    {
        sc.remove("data");
    }
    let mut bytes = envelope.to_string().into_bytes();
    if bytes.len() >= limit {
        let cut = bytes.len() - limit + TRUNCATION_MARGIN;
        if let Some(Value::String(text)) = envelope.pointer_mut("/result/content/0/text") {
            if text.len() > cut {
                let mut keep = text.len() - cut;
                while !text.is_char_boundary(keep) {
                    keep -= 1;
                }
                text.truncate(keep);
                text.push_str(TRUNCATION_NOTE);
            }
        }
        bytes = envelope.to_string().into_bytes();
    }
    if bytes.len() < limit {
        return (bytes, Fit::Reduced);
    }
    let replacement = result_envelope(id, tool_error_result(TOO_LARGE_MESSAGE));
    (replacement.to_string().into_bytes(), Fit::Replaced)
}

/// Writes the converted answer with `write`. If that write fails, a small tool error is written
/// instead, so the client never receives the raw A2A body as if it were an MCP response.
/// Returns `true` only when the agent's answer itself (in full or reduced) was delivered, which
/// is the condition for remembering its context.
pub fn deliver_result(id: &Value, result: Value, mut write: impl FnMut(&[u8]) -> bool) -> bool {
    let (bytes, fit) = bounded_result_envelope(id, result, MAX_BODY_BYTES);
    if write(&bytes) {
        return fit != Fit::Replaced;
    }
    let fallback = result_envelope(id, tool_error_result(UNDELIVERABLE_MESSAGE));
    write(fallback.to_string().as_bytes());
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_known_version_and_falls_back_to_latest() {
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_protocol_version(Some("1999-01-01")), "2025-06-18");
        assert_eq!(negotiate_protocol_version(None), "2025-06-18");
    }

    fn classify_json(value: Value) -> Result<String, Invalid> {
        let envelope = value.as_object().expect("an object").clone();
        classify(&envelope).map(|incoming| format!("{:?}", incoming))
    }

    #[test]
    fn classifies_requests_notifications_and_responses_structurally() {
        let ok = classify_json(json!({ "jsonrpc": "2.0", "id": "a", "method": "ping" })).unwrap();
        assert!(ok.starts_with("Request"), "{}", ok);
        let ok = classify_json(json!({ "jsonrpc": "2.0", "id": 0, "method": "tools/list", "params": {} })).unwrap();
        assert!(ok.starts_with("Request"), "{}", ok);
        let ok = classify_json(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).unwrap();
        assert!(ok.starts_with("Notification"), "{}", ok);
        assert_eq!(classify_json(json!({ "jsonrpc": "2.0", "id": 4, "result": {} })).unwrap(), "Response");
        assert_eq!(
            classify_json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": 1, "message": "x" } })).unwrap(),
            "Response"
        );
        // A notification method that carries an id is a request, and gets a JSON-RPC reply.
        let ok = classify_json(json!({ "jsonrpc": "2.0", "id": 9, "method": "notifications/initialized" })).unwrap();
        assert!(ok.starts_with("Request"), "{}", ok);
    }

    #[test]
    fn rejects_invalid_envelopes_ids_and_params() {
        // (message, expected id, expected code)
        let cases: Vec<(Value, Option<Value>, i64)> = vec![
            (json!({ "jsonrpc": "1.0", "id": 1, "method": "tools/call" }), Some(json!(1)), INVALID_REQUEST),
            (json!({ "jsonrpc": 2, "id": 1, "method": "ping" }), Some(json!(1)), INVALID_REQUEST),
            (json!({ "id": 1, "method": "ping" }), Some(json!(1)), INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "method": "tools/call", "params": {} }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": null, "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": { "a": 1 }, "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": [1], "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": true, "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": 1.5, "method": "ping" }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": 1, "method": 7 }), Some(json!(1)), INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": 3 }), Some(json!(1)), INVALID_PARAMS),
            (json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": [1] }), Some(json!(1)), INVALID_PARAMS),
            (json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": null }), Some(json!(1)), INVALID_PARAMS),
            (json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": "x" }), None, INVALID_PARAMS),
            (json!({ "jsonrpc": "2.0", "id": 1 }), Some(json!(1)), INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": 1, "result": {}, "error": {} }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "result": {} }), None, INVALID_REQUEST),
            (json!({ "jsonrpc": "2.0", "id": null, "result": {} }), None, INVALID_REQUEST),
        ];
        for (message, id, code) in cases {
            let invalid = classify_json(message.clone()).expect_err(&message.to_string());
            assert_eq!(invalid.id, id, "{}", message);
            assert_eq!(invalid.code, code, "{}", message);
        }
    }

    #[test]
    fn initialize_params_are_validated() {
        let valid = json!({ "protocolVersion": "1999-01-01", "capabilities": {},
                            "clientInfo": { "name": "c", "version": "1" } });
        let params = valid.as_object().unwrap();
        // A well-formed unknown version is negotiated, not rejected.
        assert_eq!(initialize_protocol_version(Some(params)), Ok("1999-01-01"));

        assert!(initialize_protocol_version(None).is_err());
        for (key, bad) in [
            ("protocolVersion", Value::Null),
            ("protocolVersion", json!(20250618)),
            ("protocolVersion", json!("")),
            ("capabilities", Value::Null),
            ("capabilities", json!([])),
            ("clientInfo", Value::Null),
            ("clientInfo", json!("c")),
            ("clientInfo", json!({ "name": "c" })),
            ("clientInfo", json!({ "version": "1" })),
            ("clientInfo", json!({ "name": 1, "version": "1" })),
        ] {
            let mut params = valid.as_object().unwrap().clone();
            if bad.is_null() {
                params.remove(key);
            } else {
                params.insert(key.to_string(), bad.clone());
            }
            assert!(initialize_protocol_version(Some(&params)).is_err(), "{} = {}", key, bad);
        }
    }

    #[test]
    fn protocol_version_header_must_be_supported_when_present() {
        assert!(check_protocol_version_header(None).is_ok());
        for version in SUPPORTED_PROTOCOL_VERSIONS {
            assert!(check_protocol_version_header(Some(version)).is_ok());
        }
        assert!(check_protocol_version_header(Some("1999-01-01")).is_err());
        assert!(check_protocol_version_header(Some("garbage")).is_err());
        assert!(check_protocol_version_header(Some("")).is_err());
    }

    #[test]
    fn error_body_omits_a_missing_id() {
        assert!(error_body(None, INVALID_REQUEST, "x").get("id").is_none());
        assert_eq!(error_body(Some(&json!(3)), INVALID_REQUEST, "x")["id"], 3);
    }

    #[test]
    fn initialize_omits_empty_instructions() {
        let result = initialize_result(None, "bridge", "1.0.0", Some(""));
        assert!(result.get("instructions").is_none());
        assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
    }

    #[test]
    fn tool_definition_advertises_reset_only_with_store() {
        let without = tool_definition("ask_agent", "d", false);
        assert!(without["inputSchema"]["properties"].get(ARG_NEW_CONVERSATION).is_none());
        let with = tool_definition("ask_agent", "d", true);
        assert!(with["inputSchema"]["properties"].get(ARG_NEW_CONVERSATION).is_some());
        assert_eq!(with["inputSchema"]["required"], json!([ARG_MESSAGE]));
    }

    #[test]
    fn parses_arguments_and_blank_ids_as_absent() {
        let args = json!({ "message": "hi", "context_id": "", "task_id": null });
        let call = parse_tool_arguments(Some(&args)).unwrap();
        assert_eq!(call.message, "hi");
        assert_eq!(call.context_id, None);
        assert_eq!(call.task_id, None);
        assert!(!call.new_conversation);

        let args = json!({ "message": "yes", "context_id": " ctx-1 ", "task_id": "t-1", "new_conversation": true });
        let call = parse_tool_arguments(Some(&args)).unwrap();
        assert_eq!(call.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(call.task_id.as_deref(), Some("t-1"));
        assert!(call.new_conversation);
    }

    #[test]
    fn rejects_missing_or_mistyped_message() {
        assert!(parse_tool_arguments(None).is_err());
        assert!(parse_tool_arguments(Some(&json!({ "message": "  " }))).is_err());
        assert!(parse_tool_arguments(Some(&json!({ "message": 3 }))).is_err());
        assert!(parse_tool_arguments(Some(&json!(["message"]))).is_err());
        assert!(parse_tool_arguments(Some(&json!({ "message": "x", "context_id": 7 }))).is_err());
    }

    fn answer(text: &str, data: Option<Value>) -> Value {
        let mut sc = json!({ "state": "completed", "contextId": "ctx" });
        if let Some(data) = data {
            sc["data"] = json!([data]);
        }
        json!({ "content": [{ "type": "text", "text": text }, { "type": "text", "text": "hint" }],
                "isError": false, "structuredContent": sc })
    }

    fn parse(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("valid JSON")
    }

    #[test]
    fn small_results_are_untouched() {
        let (bytes, fit) = bounded_result_envelope(&json!(1), answer("hi", None), MAX_BODY_BYTES);
        assert_eq!(fit, Fit::Full);
        assert_eq!(parse(&bytes)["result"]["content"][0]["text"], "hi");
    }

    #[test]
    fn duplicated_data_part_is_dropped_from_structured_content_first() {
        // A 600,000-character data part appears twice (text + structuredContent): ~1.2 MB.
        let big = "x".repeat(600_000);
        let data = json!(big);
        let (bytes, fit) = bounded_result_envelope(&json!(1), answer(&data.to_string(), Some(data)), MAX_BODY_BYTES);
        assert_eq!(fit, Fit::Reduced);
        assert!(bytes.len() < MAX_BODY_BYTES);
        let env = parse(&bytes);
        assert!(env["result"]["structuredContent"].get("data").is_none());
        assert_eq!(env["result"]["structuredContent"]["contextId"], "ctx");
        assert_eq!(env["result"]["content"][0]["text"].as_str().unwrap().len(), 600_002);
        assert_eq!(env["result"]["content"][1]["text"], "hint");
    }

    #[test]
    fn escaped_text_expansion_is_truncated_under_the_limit() {
        // 600,000 quotes serialise to 1,200,000 bytes even after the data copy is dropped.
        let quotes = "\"".repeat(600_000);
        let (bytes, fit) = bounded_result_envelope(&json!(1), answer(&quotes, Some(json!(quotes.clone()))), MAX_BODY_BYTES);
        assert_eq!(fit, Fit::Reduced);
        assert!(bytes.len() < MAX_BODY_BYTES, "{} bytes", bytes.len());
        let text = parse(&bytes)["result"]["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.ends_with("Ask for a shorter answer.]"));
        assert!(text.len() < 600_000);
    }

    #[test]
    fn multibyte_text_is_cut_on_a_char_boundary() {
        let text = "é".repeat(600_000); // 1.2 MB of UTF-8
        let (bytes, fit) = bounded_result_envelope(&json!(1), answer(&text, None), MAX_BODY_BYTES);
        assert_eq!(fit, Fit::Reduced);
        assert!(bytes.len() < MAX_BODY_BYTES);
    }

    #[test]
    fn oversized_non_text_parts_replace_the_result_with_a_tool_error() {
        let huge_id = "i".repeat(MAX_BODY_BYTES);
        let mut result = answer("short", None);
        result["structuredContent"]["taskId"] = json!(huge_id);
        let (bytes, fit) = bounded_result_envelope(&json!(5), result, MAX_BODY_BYTES);
        assert_eq!(fit, Fit::Replaced);
        let env = parse(&bytes);
        assert_eq!(env["id"], 5);
        assert_eq!(env["result"]["isError"], true);
    }

    #[test]
    fn failed_write_falls_back_to_a_tool_error_and_reports_not_delivered() {
        let mut written: Vec<Vec<u8>> = vec![];
        let delivered = deliver_result(&json!(9), answer("hi", None), |bytes| {
            written.push(bytes.to_vec());
            written.len() > 1 // the first (real) write fails
        });
        assert!(!delivered);
        assert_eq!(written.len(), 2);
        let env = parse(&written[1]);
        assert_eq!(env["id"], 9);
        assert_eq!(env["result"]["isError"], true);
        assert!(env["result"].get("task").is_none());
    }

    #[test]
    fn successful_write_reports_delivered_unless_replaced() {
        assert!(deliver_result(&json!(1), answer("hi", None), |_| true));
        let mut result = answer("short", None);
        result["structuredContent"]["taskId"] = json!("i".repeat(MAX_BODY_BYTES));
        assert!(!deliver_result(&json!(1), result, |_| true));
    }
}
