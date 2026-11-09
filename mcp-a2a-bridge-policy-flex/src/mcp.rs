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
}
