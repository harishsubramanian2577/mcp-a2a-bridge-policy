// Copyright 2026 Salesforce, Inc. All rights reserved.
//! A2A client-side vocabulary: building the outbound `SendMessage` and normalising whatever the
//! agent answers (v1.0 or v0.3, Task or Message, JSON or SSE) into an [`AgentReply`].

use serde_json::{json, Map, Value};

pub const A2A_VERSION_HEADER: &str = "a2a-version";
pub const A2A_V1_HEADER_VALUE: &str = "1.0";

pub const V1_SEND_MESSAGE_METHOD_NAME: &str = "SendMessage";
pub const V03_SEND_MESSAGE_METHOD_NAME: &str = "message/send";

/// Every A2A JSON-RPC method in both vocabularies; used to let native A2A traffic through.
const A2A_METHOD_NAMES: &[&str] = &[
    "message/send",
    "message/stream",
    "tasks/get",
    "tasks/list",
    "tasks/cancel",
    "tasks/stream",
    "tasks/resubscribe",
    "tasks/pushNotificationConfig/set",
    "tasks/pushNotificationConfig/get",
    "tasks/pushNotificationConfig/list",
    "tasks/pushNotificationConfig/delete",
    "agent/card",
    "agent/capabilities",
    "agent/getAuthenticatedExtendedCard",
    "SendMessage",
    "SendStreamingMessage",
    "GetTask",
    "ListTasks",
    "CancelTask",
    "SubscribeToTask",
    "CreateTaskPushNotificationConfig",
    "GetTaskPushNotificationConfig",
    "ListTaskPushNotificationConfigs",
    "DeleteTaskPushNotificationConfig",
    "GetExtendedAgentCard",
];

pub fn is_a2a_method(method: &str) -> bool {
    A2A_METHOD_NAMES.contains(&method)
}

/// Recognises the A2A v1.0 HTTP+JSON binding and the agent card from the verb and path alone, so
/// native REST calls are classified before any MCP handling. The route may sit under the API
/// base path and an optional tenant segment, so it is matched on the trailing segments.
pub fn is_http_json_route(http_method: &str, path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let tail = |n: usize| segments.len().checked_sub(n).map(|start| &segments[start..]);

    let action = segments.last().and_then(|last| last.rsplit_once(':'));
    if let Some((target, verb)) = action {
        let task_action = !target.is_empty()
            && segments.len() >= 2
            && segments[segments.len() - 2] == "tasks";
        return match (http_method, target, verb) {
            ("POST", "message", "send" | "stream") => true,
            ("POST", _, "cancel") => task_action,
            ("POST" | "GET", _, "subscribe") => task_action,
            _ => false,
        };
    }
    if path.ends_with("/.well-known/agent-card.json") || path.ends_with("/.well-known/agent.json") {
        return http_method == "GET";
    }
    match http_method {
        "GET" => {
            matches!(segments.last(), Some(&"extendedAgentCard") | Some(&"tasks"))
                || matches!(tail(2), Some(["tasks", _]))
                || matches!(tail(3), Some(["tasks", _, "pushNotificationConfigs"]))
                || matches!(tail(4), Some(["tasks", _, "pushNotificationConfigs", _]))
        }
        "POST" => matches!(tail(3), Some(["tasks", _, "pushNotificationConfigs"])),
        "DELETE" => matches!(tail(4), Some(["tasks", _, "pushNotificationConfigs", _])),
        _ => false,
    }
}

/// Removes an `A2A-Version` query parameter (any case), which would otherwise contradict the
/// version of a request the bridge generated itself.
pub fn strip_version_query(path: &str) -> String {
    let Some((base, query)) = path.split_once('?') else {
        return path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            let key = kv.split_once('=').map_or(*kv, |(k, _)| k);
            !kv.is_empty() && !key.eq_ignore_ascii_case(A2A_VERSION_HEADER)
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{}?{}", base, kept.join("&"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A2aVersion {
    V1,
    V03,
}

impl A2aVersion {
    pub fn from_config(value: &str) -> Self {
        if value.trim() == "0.3" {
            A2aVersion::V03
        } else {
            A2aVersion::V1
        }
    }
}

/// Builds the JSON-RPC `SendMessage` (v1.0) / `message/send` (v0.3) request. The MCP request id
/// is reused so upstream logs correlate with the client's call.
pub fn build_send_message(
    version: A2aVersion,
    rpc_id: &Value,
    message_id: &str,
    text: &str,
    context_id: Option<&str>,
    task_id: Option<&str>,
) -> Value {
    let mut message = match version {
        A2aVersion::V1 => json!({
            "messageId": message_id,
            "role": "ROLE_USER",
            "parts": [{ "text": text }],
        }),
        A2aVersion::V03 => json!({
            "kind": "message",
            "messageId": message_id,
            "role": "user",
            "parts": [{ "kind": "text", "text": text }],
        }),
    };
    if let Some(id) = context_id {
        message["contextId"] = Value::String(id.to_string());
    }
    if let Some(id) = task_id {
        message["taskId"] = Value::String(id.to_string());
    }
    let method = match version {
        A2aVersion::V1 => V1_SEND_MESSAGE_METHOD_NAME,
        A2aVersion::V03 => V03_SEND_MESSAGE_METHOD_NAME,
    };
    json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": method,
        "params": { "message": message },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    AuthRequired,
    Completed,
    Failed,
    Rejected,
    Canceled,
    Unknown,
}

impl TaskState {
    /// Accepts v1.0 (`TASK_STATE_INPUT_REQUIRED`) and v0.3 (`input-required`) spellings.
    pub fn parse(raw: &str) -> Self {
        let lower = raw.trim().to_ascii_lowercase().replace('_', "-");
        match lower.strip_prefix("task-state-").unwrap_or(&lower) {
            "submitted" => TaskState::Submitted,
            "working" => TaskState::Working,
            "input-required" => TaskState::InputRequired,
            "auth-required" => TaskState::AuthRequired,
            "completed" => TaskState::Completed,
            "failed" => TaskState::Failed,
            "rejected" => TaskState::Rejected,
            "canceled" | "cancelled" => TaskState::Canceled,
            _ => TaskState::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Submitted => "submitted",
            TaskState::Working => "working",
            TaskState::InputRequired => "input-required",
            TaskState::AuthRequired => "auth-required",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Rejected => "rejected",
            TaskState::Canceled => "canceled",
            TaskState::Unknown => "unknown",
        }
    }

    pub fn is_error(self) -> bool {
        matches!(
            self,
            TaskState::Failed | TaskState::Rejected | TaskState::Canceled
        )
    }

    /// The agent is waiting for the caller; the next message must carry this task id.
    pub fn awaits_client(self) -> bool {
        matches!(self, TaskState::InputRequired | TaskState::AuthRequired)
    }

    pub fn is_running(self) -> bool {
        matches!(self, TaskState::Submitted | TaskState::Working)
    }
}

/// The agent's answer, independent of A2A version and transport.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentReply {
    pub state: TaskState,
    pub context_id: Option<String>,
    pub task_id: Option<String>,
    pub texts: Vec<String>,
    pub data: Vec<Value>,
}

impl AgentReply {
    fn empty(state: TaskState) -> Self {
        AgentReply {
            state,
            context_id: None,
            task_id: None,
            texts: Vec::new(),
            data: Vec::new(),
        }
    }

    fn absorb_parts(&mut self, parts: Option<&Value>) {
        let Some(Value::Array(parts)) = parts else {
            return;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                self.texts.push(text.to_string());
            } else if let Some(data) = part.get("data") {
                // Data parts go to the LLM as compact JSON and to structuredContent as-is.
                self.texts.push(data.to_string());
                self.data.push(data.clone());
            } else {
                self.texts.push(describe_file_part(part));
            }
        }
    }
}

/// File parts are summarised rather than inlined: v0.3 `{file:{name,mimeType,uri|bytes}}`,
/// v1.0 `{url|raw, mediaType, filename}`.
fn describe_file_part(part: &Value) -> String {
    let file = part.get("file").unwrap_or(part);
    let name = str_field(file, &["name", "filename"]).unwrap_or("file");
    let mime = str_field(file, &["mimeType", "mediaType"])
        .or_else(|| str_field(part, &["mediaType"]))
        .unwrap_or("application/octet-stream");
    match str_field(file, &["uri", "url"]) {
        Some(uri) => format!("[file {} ({}): {}]", name, mime, uri),
        None if str_field(file, &["bytes", "raw"]).is_some() => {
            format!("[file {} ({}): inline content omitted]", name, mime)
        }
        None => format!("[unsupported part: {}]", part),
    }
}

fn str_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|n| value.get(*n).and_then(Value::as_str))
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Normalises a JSON-RPC `result`. v1.0 wraps it (`{task}` / `{message}`); v0.3 returns the
/// Task or Message itself, discriminated by `kind`.
pub fn parse_agent_result(result: &Value) -> AgentReply {
    if let Some(task) = result.get("task") {
        return parse_task(task);
    }
    if let Some(message) = result.get("message") {
        return parse_message(message);
    }
    let kind = result.get("kind").and_then(Value::as_str);
    if kind == Some("task") || (kind.is_none() && result.get("status").is_some()) {
        return parse_task(result);
    }
    parse_message(result)
}

fn parse_task(task: &Value) -> AgentReply {
    let state = task
        .pointer("/status/state")
        .and_then(Value::as_str)
        .map(TaskState::parse)
        .unwrap_or(TaskState::Unknown);
    let mut reply = AgentReply::empty(state);
    reply.context_id = string_field(task, "contextId");
    reply.task_id = string_field(task, "id");

    if let Some(Value::Array(artifacts)) = task.get("artifacts") {
        for artifact in artifacts {
            reply.absorb_parts(artifact.get("parts"));
        }
    }
    // A completed task's answer normally lives in artifacts; the status message is the answer
    // only when there are none, and is always relevant for prompts and failures.
    if reply.texts.is_empty() || state != TaskState::Completed {
        reply.absorb_parts(task.pointer("/status/message/parts"));
    }
    if reply.texts.is_empty() {
        if let Some(last) = last_agent_message(task) {
            reply.absorb_parts(last.get("parts"));
        }
    }
    reply
}

fn last_agent_message(task: &Value) -> Option<&Value> {
    task.get("history")?.as_array()?.iter().rev().find(|m| {
        matches!(
            m.get("role").and_then(Value::as_str),
            Some("ROLE_AGENT") | Some("agent")
        )
    })
}

/// A bare Message reply ends the interaction: there is no task to continue.
fn parse_message(message: &Value) -> AgentReply {
    let mut reply = AgentReply::empty(TaskState::Completed);
    reply.context_id = string_field(message, "contextId");
    reply.task_id = string_field(message, "taskId");
    reply.absorb_parts(message.get("parts"));
    reply
}

/// Converts the upstream HTTP response into an [`AgentReply`]; `Err` carries a message for an
/// MCP `isError` tool result.
pub fn parse_agent_response(
    http_status: u32,
    content_type: &str,
    body: &[u8],
) -> Result<AgentReply, String> {
    let is_sse = content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream");
    let outcome = if is_sse {
        parse_sse_body(body)
    } else {
        match serde_json::from_slice::<Value>(body) {
            Ok(json) => parse_rpc_response(&json),
            Err(_) => Err(format!(
                "The A2A agent returned a non-JSON response: {}",
                excerpt(body)
            )),
        }
    };
    if (200..300).contains(&http_status) {
        return outcome;
    }
    // Prefer the in-band JSON-RPC error text when the agent sent one.
    match outcome {
        Err(message) if message.starts_with("A2A error") => Err(message),
        _ => Err(format!(
            "The A2A agent returned HTTP {}: {}",
            http_status,
            excerpt(body)
        )),
    }
}

fn parse_rpc_response(json: &Value) -> Result<AgentReply, String> {
    if let Some(error) = json.get("error") {
        return Err(format_rpc_error(error));
    }
    match json.get("result") {
        Some(result) => Ok(parse_agent_result(result)),
        None => Err(format!(
            "The A2A agent returned a JSON body without a JSON-RPC result: {}",
            excerpt(json.to_string().as_bytes())
        )),
    }
}

fn format_rpc_error(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let detail = error
        .pointer("/data/0/metadata/detail")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| error.get("data").map(Value::to_string));
    match detail {
        Some(d) => format!("A2A error {}: {} ({})", code, message, d),
        None => format!("A2A error {}: {}", code, message),
    }
}

/// Folds a streamed response (SendStreamingMessage-style) into one reply: a Task or Message
/// event replaces the snapshot, status/artifact update events amend it.
fn parse_sse_body(body: &[u8]) -> Result<AgentReply, String> {
    let text = String::from_utf8_lossy(body).replace("\r\n", "\n");
    let mut reply: Option<AgentReply> = None;
    for event in text.split("\n\n") {
        let data: Vec<&str> = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        if data.is_empty() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(&data.join("\n")) else {
            continue;
        };
        if let Some(error) = json.get("error") {
            return Err(format_rpc_error(error));
        }
        let Some(result) = json.get("result") else {
            continue;
        };
        let current = reply.get_or_insert_with(|| AgentReply::empty(TaskState::Unknown));
        if let Some(update) = stream_update(result, "statusUpdate", "status-update") {
            if let Some(state) = update.pointer("/status/state").and_then(Value::as_str) {
                current.state = TaskState::parse(state);
            }
            current.absorb_parts(update.pointer("/status/message/parts"));
            adopt_ids(current, update, "taskId");
        } else if let Some(update) = stream_update(result, "artifactUpdate", "artifact-update") {
            current.absorb_parts(update.pointer("/artifact/parts"));
            adopt_ids(current, update, "taskId");
        } else {
            *current = parse_agent_result(result);
        }
    }
    reply.ok_or_else(|| "The A2A agent stream contained no result events".to_string())
}

fn stream_update<'a>(result: &'a Value, v1_key: &str, v03_kind: &str) -> Option<&'a Value> {
    result.get(v1_key).or_else(|| {
        (result.get("kind").and_then(Value::as_str) == Some(v03_kind)).then_some(result)
    })
}

fn adopt_ids(reply: &mut AgentReply, update: &Value, task_key: &str) {
    if reply.context_id.is_none() {
        reply.context_id = string_field(update, "contextId");
    }
    if reply.task_id.is_none() {
        reply.task_id = string_field(update, task_key);
    }
}

fn excerpt(body: &[u8]) -> String {
    const MAX: usize = 512;
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "<empty body>".to_string();
    }
    match trimmed.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &trimmed[..cut]),
        None => trimmed.to_string(),
    }
}

/// Builds the MCP `CallToolResult`. The continuation ids are repeated in the text content
/// because many MCP clients only forward text to the model, and the model must echo them back.
pub fn tool_result_from_reply(reply: &AgentReply, tool_name: &str, structured: bool) -> Value {
    let answer = if reply.texts.is_empty() {
        format!(
            "The agent returned no content (state: {}).",
            reply.state.as_str()
        )
    } else {
        reply.texts.join("\n\n")
    };
    let mut content = vec![json!({ "type": "text", "text": answer })];
    if let Some(hint) = continuation_hint(reply, tool_name) {
        content.push(json!({ "type": "text", "text": hint }));
    }

    let mut result = Map::new();
    result.insert("content".to_string(), Value::Array(content));
    result.insert("isError".to_string(), Value::Bool(reply.state.is_error()));
    if structured {
        let mut sc = Map::new();
        sc.insert("state".to_string(), json!(reply.state.as_str()));
        if let Some(id) = &reply.context_id {
            sc.insert("contextId".to_string(), json!(id));
        }
        if let Some(id) = &reply.task_id {
            sc.insert("taskId".to_string(), json!(id));
        }
        if !reply.data.is_empty() {
            sc.insert("data".to_string(), Value::Array(reply.data.clone()));
        }
        result.insert("structuredContent".to_string(), Value::Object(sc));
    }
    Value::Object(result)
}

fn continuation_hint(reply: &AgentReply, tool_name: &str) -> Option<String> {
    let context = reply.context_id.as_deref()?;
    let state = reply.state.as_str();
    Some(match (reply.state, reply.task_id.as_deref()) {
        (s, Some(task)) if s.awaits_client() => format!(
            "[a2a state={}] The agent is waiting for your reply. Call {} again with \
             context_id=\"{}\" and task_id=\"{}\".",
            state, tool_name, context, task
        ),
        (s, Some(task)) if s.is_running() => format!(
            "[a2a state={}] The agent is still working on task_id=\"{}\". Continue the \
             conversation with context_id=\"{}\".",
            state, task, context
        ),
        _ => format!(
            "[a2a state={}] To continue this conversation, call {} with context_id=\"{}\".",
            state, tool_name, context
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RESPONSE: &str = include_str!("../tests/fixtures/a2a_v1_send_message_response.json");

    #[test]
    fn builds_v1_request_like_the_broker_sample() {
        let req = build_send_message(
            A2aVersion::V1,
            &json!("broker-1"),
            "19f04ec3",
            "Get me details of the wiki page with id 100200300",
            None,
            None,
        );
        assert_eq!(
            req,
            json!({
                "jsonrpc": "2.0",
                "id": "broker-1",
                "method": "SendMessage",
                "params": { "message": {
                    "messageId": "19f04ec3",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "Get me details of the wiki page with id 100200300" }]
                }}
            })
        );
    }

    #[test]
    fn builds_v03_request_with_continuation_ids() {
        let req = build_send_message(A2aVersion::V03, &json!(7), "m1", "yes", Some("c1"), Some("t1"));
        assert_eq!(req["method"], "message/send");
        assert_eq!(req["id"], 7);
        let message = &req["params"]["message"];
        assert_eq!(message["role"], "user");
        assert_eq!(message["kind"], "message");
        assert_eq!(message["parts"][0]["kind"], "text");
        assert_eq!(message["contextId"], "c1");
        assert_eq!(message["taskId"], "t1");
    }

    #[test]
    fn parses_broker_sample_completed_task_with_data_part() {
        let reply = parse_agent_response(200, "application/json", SAMPLE_RESPONSE.as_bytes()).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.context_id.as_deref(), Some("59ec80cd-988c-47fd-8917-1f84dd3bd409"));
        assert_eq!(reply.task_id.as_deref(), Some("94da0605-5da3-4eaf-8aae-a34cea57ddb6"));
        assert_eq!(reply.data.len(), 1);
        assert_eq!(reply.data[0]["id"], "100200300");
        assert!(reply.texts[0].contains("SOP 2.2"));
    }

    #[test]
    fn maps_sample_to_mcp_tool_result() {
        let reply = parse_agent_response(200, "application/json", SAMPLE_RESPONSE.as_bytes()).unwrap();
        let result = tool_result_from_reply(&reply, "ask_agent", true);
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["state"], "completed");
        assert_eq!(result["structuredContent"]["contextId"], "59ec80cd-988c-47fd-8917-1f84dd3bd409");
        assert_eq!(result["structuredContent"]["data"][0]["spaceKey"], "SAMPLE");
        let hint = result["content"][1]["text"].as_str().unwrap();
        assert!(hint.contains("context_id=\"59ec80cd-988c-47fd-8917-1f84dd3bd409\""));

        let plain = tool_result_from_reply(&reply, "ask_agent", false);
        assert!(plain.get("structuredContent").is_none());
    }

    #[test]
    fn input_required_prefers_status_prompt_and_asks_for_task_id() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "task": {
            "id": "t9", "contextId": "c9",
            "status": { "state": "TASK_STATE_INPUT_REQUIRED", "message": {
                "role": "ROLE_AGENT", "parts": [{ "text": "Which space?" }] } }
        }}});
        let reply = parse_agent_response(200, "application/json", body.to_string().as_bytes()).unwrap();
        assert_eq!(reply.state, TaskState::InputRequired);
        assert_eq!(reply.texts, vec!["Which space?".to_string()]);
        let result = tool_result_from_reply(&reply, "ask_agent", true);
        assert_eq!(result["isError"], false);
        assert!(result["content"][1]["text"].as_str().unwrap().contains("task_id=\"t9\""));
    }

    #[test]
    fn v03_task_result_is_not_wrapped() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": {
            "kind": "task", "id": "t1", "contextId": "c1",
            "status": { "state": "completed" },
            "artifacts": [{ "parts": [{ "kind": "text", "text": "done" }] }]
        }});
        let reply = parse_agent_response(200, "application/json", body.to_string().as_bytes()).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.texts, vec!["done".to_string()]);
    }

    #[test]
    fn message_result_is_completed() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "message": {
            "role": "ROLE_AGENT", "contextId": "c2", "parts": [{ "text": "hello" }] }}});
        let reply = parse_agent_response(200, "application/json", body.to_string().as_bytes()).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.context_id.as_deref(), Some("c2"));
        assert_eq!(reply.task_id, None);
    }

    #[test]
    fn failed_task_is_an_mcp_tool_error() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "task": {
            "id": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_FAILED", "message": { "parts": [{ "text": "boom" }] } }
        }}});
        let reply = parse_agent_response(200, "application/json", body.to_string().as_bytes()).unwrap();
        assert!(tool_result_from_reply(&reply, "ask_agent", true)["isError"].as_bool().unwrap());
    }

    #[test]
    fn jsonrpc_error_and_http_errors_become_messages() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32001, "message": "Task not found" } });
        let err = parse_agent_response(200, "application/json", body.to_string().as_bytes()).unwrap_err();
        assert_eq!(err, "A2A error -32001: Task not found");

        let err = parse_agent_response(401, "application/json", body.to_string().as_bytes()).unwrap_err();
        assert!(err.starts_with("A2A error -32001"));

        let err = parse_agent_response(502, "text/html", b"<html>Bad gateway</html>").unwrap_err();
        assert_eq!(err, "The A2A agent returned HTTP 502: <html>Bad gateway</html>");
    }

    #[test]
    fn folds_sse_stream_updates() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"task\":{\"id\":\"t\",\"contextId\":\"c\",\"status\":{\"state\":\"TASK_STATE_WORKING\"}}}}\r\n\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"artifactUpdate\":{\"taskId\":\"t\",\"contextId\":\"c\",\"artifact\":{\"parts\":[{\"text\":\"part one\"}]}}}}\n\n",
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"statusUpdate\":{\"taskId\":\"t\",\"contextId\":\"c\",\"status\":{\"state\":\"TASK_STATE_COMPLETED\"}}}}\n\n",
        );
        let reply = parse_agent_response(200, "text/event-stream", body.as_bytes()).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.texts, vec!["part one".to_string()]);
        assert_eq!(reply.task_id.as_deref(), Some("t"));
    }

    #[test]
    fn describes_file_parts_in_both_versions() {
        let mut reply = AgentReply::empty(TaskState::Completed);
        reply.absorb_parts(Some(&json!([
            { "kind": "file", "file": { "name": "a.pdf", "mimeType": "application/pdf", "uri": "https://x/a.pdf" } },
            { "url": "https://x/b.png", "mediaType": "image/png", "filename": "b.png" },
            { "raw": "AAAA", "mediaType": "image/png" }
        ])));
        assert_eq!(reply.texts[0], "[file a.pdf (application/pdf): https://x/a.pdf]");
        assert_eq!(reply.texts[1], "[file b.png (image/png): https://x/b.png]");
        assert_eq!(reply.texts[2], "[file file (image/png): inline content omitted]");
    }

    #[test]
    fn task_state_accepts_both_spellings() {
        assert_eq!(TaskState::parse("TASK_STATE_AUTH_REQUIRED"), TaskState::AuthRequired);
        assert_eq!(TaskState::parse("input-required"), TaskState::InputRequired);
        assert_eq!(TaskState::parse("cancelled"), TaskState::Canceled);
        assert_eq!(TaskState::parse("TASK_STATE_UNSPECIFIED"), TaskState::Unknown);
    }

    #[test]
    fn recognises_a2a_methods_but_not_mcp_methods() {
        assert!(is_a2a_method("SendMessage"));
        assert!(is_a2a_method("tasks/get"));
        assert!(!is_a2a_method("tools/call"));
    }

    #[test]
    fn http_json_routes_are_recognised_under_any_prefix() {
        for (verb, path) in [
            ("POST", "/message:send"),
            ("POST", "/base/message:stream"),
            ("POST", "/base/tenant-1/message:send?A2A-Version=1.0"),
            ("POST", "/base/tasks/t-1:cancel"),
            ("POST", "/base/tasks/t-1:subscribe"),
            ("GET", "/base/tasks/t-1:subscribe"),
            ("GET", "/base/tasks"),
            ("GET", "/base/tasks/t-1"),
            ("POST", "/base/tasks/t-1/pushNotificationConfigs"),
            ("GET", "/base/tasks/t-1/pushNotificationConfigs"),
            ("GET", "/base/tasks/t-1/pushNotificationConfigs/c-1"),
            ("DELETE", "/base/tasks/t-1/pushNotificationConfigs/c-1"),
            ("GET", "/base/extendedAgentCard"),
            ("GET", "/base/.well-known/agent-card.json"),
        ] {
            assert!(is_http_json_route(verb, path), "{} {}", verb, path);
        }
        for (verb, path) in [
            ("POST", "/mcp"),
            ("POST", "/"),
            ("POST", "/base/tasks"),
            ("POST", "/base/tasks/t-1"),
            ("POST", "/base/:cancel"),
            ("POST", "/base/x:cancel"),
            ("DELETE", "/mcp"),
            ("DELETE", "/base/tasks/t-1"),
            ("PUT", "/base/message:send"),
            ("GET", "/base/message:send"),
        ] {
            assert!(!is_http_json_route(verb, path), "{} {}", verb, path);
        }
    }

    #[test]
    fn version_query_is_stripped_case_insensitively() {
        assert_eq!(strip_version_query("/a?A2A-Version=0.3"), "/a");
        assert_eq!(strip_version_query("/a?x=1&a2a-version=1.0&y=2"), "/a?x=1&y=2");
        assert_eq!(strip_version_query("/a?x=1"), "/a?x=1");
        assert_eq!(strip_version_query("/a"), "/a");
    }
}
