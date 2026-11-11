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

pub const SESSION_ID_HEADER: &str = "mcp-session-id";

/// Newest first. An unknown client version is answered with the newest one, per the MCP
/// version-negotiation rules; the client decides whether it can proceed.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

pub const ARG_MESSAGE: &str = "message";
pub const ARG_CONTEXT_ID: &str = "context_id";
pub const ARG_TASK_ID: &str = "task_id";
pub const ARG_NEW_CONVERSATION: &str = "new_conversation";

pub fn is_notification(method: &str) -> bool {
    method.starts_with(NOTIFICATION_METHOD_PREFIX)
}

pub fn result_envelope(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result })
}

pub fn error_envelope(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "error": { "code": code, "message": message } })
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
            "description": "task_id returned by a previous call whose state was input-required or auth-required. Pass it with context_id to answer the agent's follow-up question."
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
