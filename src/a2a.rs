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
    /// Accepts v1.0 (`TASK_STATE_INPUT_REQUIRED`) and v0.3 (`input-required`) spellings. `None`
    /// for anything else, including v1.0 `TASK_STATE_UNSPECIFIED`, which no valid task carries.
    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase().replace('_', "-");
        Some(match lower.strip_prefix("task-state-").unwrap_or(&lower) {
            "submitted" => TaskState::Submitted,
            "working" => TaskState::Working,
            "input-required" => TaskState::InputRequired,
            "auth-required" => TaskState::AuthRequired,
            "completed" => TaskState::Completed,
            "failed" => TaskState::Failed,
            "rejected" => TaskState::Rejected,
            "canceled" | "cancelled" => TaskState::Canceled,
            "unknown" => TaskState::Unknown,
            _ => return None,
        })
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

    fn absorb_parts(&mut self, parts: &[Value]) {
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

const INVALID_RESPONSE: &str = "The A2A agent returned an invalid response";

fn invalid(detail: impl std::fmt::Display) -> String {
    format!("{}: {}", INVALID_RESPONSE, detail)
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{} is not an object", what)))
}

/// An optional string member: absent, `null` and `""` all count as absent, any other type is
/// invalid.
fn optional_string(
    obj: &Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone()).filter(|s| !s.is_empty())),
        Some(_) => Err(invalid(format!("{} {} is not a string", what, key))),
    }
}

fn parts(obj: &Map<String, Value>, what: &str) -> Result<Vec<Value>, String> {
    match obj.get("parts") {
        Some(Value::Array(parts)) if parts.iter().all(Value::is_object) => Ok(parts.clone()),
        Some(Value::Array(_)) => Err(invalid(format!("{} has a part that is not an object", what))),
        _ => Err(invalid(format!("{} has no parts array", what))),
    }
}

/// The `status` of a Task or status update: a recognised state and the status message's parts.
fn task_status(obj: &Map<String, Value>, what: &str) -> Result<(TaskState, Vec<Value>), String> {
    let status = object(
        obj.get("status").unwrap_or(&Value::Null),
        &format!("the {} status", what),
    )?;
    let raw = status
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("the {} status has no state", what)))?;
    let state =
        TaskState::parse(raw).ok_or_else(|| invalid(format!("unrecognised task state {:?}", raw)))?;
    let message_parts = match status.get("message") {
        None | Some(Value::Null) => Vec::new(),
        Some(message) => parts(object(message, "the status message")?, "the status message")?,
    };
    Ok((state, message_parts))
}

/// One artifact of the task, folded by `artifactId` across stream events.
#[derive(Debug, Clone, PartialEq)]
struct Artifact {
    id: Option<String>,
    parts: Vec<Value>,
}

fn artifact(value: &Value) -> Result<Artifact, String> {
    let obj = object(value, "an artifact")?;
    Ok(Artifact {
        id: optional_string(obj, "artifactId", "the artifact")?,
        parts: parts(obj, "an artifact")?,
    })
}

/// The task as the agent last described it. Stream events amend it, and the reply is flattened
/// from it only once the response is complete, so replaced output never reaches the client.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    state: TaskState,
    context_id: Option<String>,
    task_id: Option<String>,
    artifacts: Vec<Artifact>,
    /// Parts of the current status message, or of a bare Message reply.
    status_parts: Vec<Value>,
    /// Parts of the last agent message in the history; used only when nothing else answered.
    history_parts: Vec<Value>,
}

impl Snapshot {
    fn new(state: TaskState) -> Self {
        Snapshot {
            state,
            context_id: None,
            task_id: None,
            artifacts: Vec::new(),
            status_parts: Vec::new(),
            history_parts: Vec::new(),
        }
    }

    fn into_reply(self) -> AgentReply {
        let mut reply = AgentReply::empty(self.state);
        reply.context_id = self.context_id;
        reply.task_id = self.task_id;
        for artifact in &self.artifacts {
            reply.absorb_parts(&artifact.parts);
        }
        // A completed task's answer normally lives in artifacts; the status message is the answer
        // only when there are none, and is always relevant for prompts and failures.
        if reply.texts.is_empty() || self.state != TaskState::Completed {
            reply.absorb_parts(&self.status_parts);
        }
        if reply.texts.is_empty() {
            reply.absorb_parts(&self.history_parts);
        }
        reply
    }

    /// A status update replaces the whole status: state and status message.
    fn apply_status(&mut self, update: &Value) -> Result<(), String> {
        let obj = object(update, "a status update")?;
        let (state, message_parts) = task_status(obj, "status update")?;
        self.state = state;
        self.status_parts = message_parts;
        self.adopt_ids(obj)
    }

    /// An artifact update replaces the artifact with the same `artifactId`, or extends it when
    /// `append` is true; an artifact not seen before is added.
    fn apply_artifact(&mut self, update: &Value) -> Result<(), String> {
        let obj = object(update, "an artifact update")?;
        let new = artifact(obj.get("artifact").unwrap_or(&Value::Null))?;
        let append = match obj.get("append") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(append)) => *append,
            Some(_) => return Err(invalid("artifact update append is not a boolean")),
        };
        let existing = new
            .id
            .as_ref()
            .and_then(|id| self.artifacts.iter().position(|a| a.id.as_ref() == Some(id)));
        match existing {
            Some(i) if append => self.artifacts[i].parts.extend(new.parts),
            Some(i) => self.artifacts[i] = new,
            None => self.artifacts.push(new),
        }
        self.adopt_ids(obj)
    }

    fn adopt_ids(&mut self, update: &Map<String, Value>) -> Result<(), String> {
        let context_id = optional_string(update, "contextId", "the stream update")?;
        let task_id = optional_string(update, "taskId", "the stream update")?;
        if self.context_id.is_none() {
            self.context_id = context_id;
        }
        if self.task_id.is_none() {
            self.task_id = task_id;
        }
        Ok(())
    }
}

fn task_snapshot(value: &Value) -> Result<Snapshot, String> {
    let task = object(value, "the task")?;
    let task_id = optional_string(task, "id", "the task")?
        .ok_or_else(|| invalid("the task has no id"))?;
    let (state, status_parts) = task_status(task, "task")?;
    let artifacts = match task.get("artifacts") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(artifact).collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("the task artifacts are not an array")),
    };
    Ok(Snapshot {
        state,
        context_id: optional_string(task, "contextId", "the task")?,
        task_id: Some(task_id),
        artifacts,
        status_parts,
        history_parts: last_agent_message_parts(task),
    })
}

/// History is only a fallback, so it is read leniently.
fn last_agent_message_parts(task: &Map<String, Value>) -> Vec<Value> {
    let last = task
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history.iter().rev().find(|m| {
                matches!(
                    m.get("role").and_then(Value::as_str),
                    Some("ROLE_AGENT") | Some("agent")
                )
            })
        });
    last.and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| parts.iter().filter(|p| p.is_object()).cloned().collect())
        .unwrap_or_default()
}

/// A bare Message reply ends the interaction: there is no task to continue.
fn message_snapshot(value: &Value) -> Result<Snapshot, String> {
    let message = object(value, "the message")?;
    let mut snapshot = Snapshot::new(TaskState::Completed);
    snapshot.context_id = optional_string(message, "contextId", "the message")?;
    snapshot.task_id = optional_string(message, "taskId", "the message")?;
    snapshot.status_parts = parts(message, "the message")?;
    Ok(snapshot)
}

/// Validates and normalises a JSON-RPC `result`. v1.0 wraps it (`{task}` / `{message}`); v0.3
/// returns the Task or Message itself, discriminated by `kind`.
fn result_snapshot(result: &Value) -> Result<Snapshot, String> {
    let obj = object(result, "the result")?;
    if let Some(task) = obj.get("task") {
        return task_snapshot(task);
    }
    if let Some(message) = obj.get("message") {
        return message_snapshot(message);
    }
    match obj.get("kind").and_then(Value::as_str) {
        Some("task") => task_snapshot(result),
        Some("message") => message_snapshot(result),
        None if obj.contains_key("status") => task_snapshot(result),
        None if obj.contains_key("parts") => message_snapshot(result),
        _ => Err(invalid("the result is neither a Task nor a Message")),
    }
}

/// Checks the JSON-RPC envelope of an agent response and returns its `result`. The id must be
/// the one the bridge sent, so another call's answer is never passed off as this one's. An error
/// may carry a `null` id, as JSON-RPC allows when the agent could not read the request id.
fn rpc_result<'a>(json: &'a Value, expected_id: &Value) -> Result<&'a Value, String> {
    let envelope = object(json, "the JSON-RPC response")?;
    if envelope.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid("it is not a JSON-RPC 2.0 response"));
    }
    let id = envelope.get("id").unwrap_or(&Value::Null);
    match (envelope.get("result"), envelope.get("error")) {
        (Some(_), Some(_)) => Err(invalid("it has both a result and an error")),
        (None, None) => Err(invalid("it has neither a result nor an error")),
        (None, Some(error)) if id.is_null() || id == expected_id => Err(format_rpc_error(error)),
        (Some(result), None) if id == expected_id => Ok(result),
        _ => Err(invalid(format!(
            "its id {} does not match the request id {}",
            id, expected_id
        ))),
    }
}

/// Converts the upstream HTTP response into an [`AgentReply`]; `Err` carries a message for an
/// MCP `isError` tool result. `expected_id` is the JSON-RPC id the bridge sent upstream.
pub fn parse_agent_response(
    http_status: u32,
    content_type: &str,
    body: &[u8],
    expected_id: &Value,
) -> Result<AgentReply, String> {
    let is_sse = content_type
        .to_ascii_lowercase()
        .starts_with("text/event-stream");
    let outcome = if is_sse {
        parse_sse_body(body, expected_id)
    } else {
        match serde_json::from_slice::<Value>(body) {
            Ok(json) => rpc_result(&json, expected_id).and_then(result_snapshot),
            Err(_) => Err(format!(
                "The A2A agent returned a non-JSON response: {}",
                excerpt(body)
            )),
        }
    };
    if (200..300).contains(&http_status) {
        return outcome.map(Snapshot::into_reply);
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

/// Splits an SSE body into the data of each dispatched event, following the WHATWG parsing rules:
/// LF, CRLF and CR line ends, one leading BOM, `:` comment lines, and a blank line to dispatch.
/// Other fields (`event`, `id`, `retry`) carry nothing the bridge uses. A body that ends in the
/// middle of an event was cut off, so it is an error rather than a shorter answer.
fn sse_event_data(body: &[u8]) -> Result<Vec<String>, String> {
    let text = String::from_utf8_lossy(body);
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut events = Vec::new();
    let mut data = String::new();
    while let Some(end) = rest.find(['\r', '\n']) {
        let line = &rest[..end];
        let next = if rest[end..].starts_with("\r\n") { end + 2 } else { end + 1 };
        rest = &rest[next..];
        if line.is_empty() {
            if !data.is_empty() {
                data.pop(); // the line feed after the last data line
                events.push(std::mem::take(&mut data));
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            data.push_str(value);
            data.push('\n');
        }
    }
    if !data.is_empty() || !(rest.is_empty() || rest.starts_with(':')) {
        return Err(invalid("the stream ended in the middle of an event"));
    }
    Ok(events)
}

/// Folds a streamed response (SendStreamingMessage-style) into one snapshot: a Task or Message
/// event replaces it, a status update replaces the status, and an artifact update replaces or
/// extends its artifact. One corrupt event fails the whole stream, so an incomplete answer is
/// never reported as a successful one.
fn parse_sse_body(body: &[u8], expected_id: &Value) -> Result<Snapshot, String> {
    let mut snapshot: Option<Snapshot> = None;
    for data in sse_event_data(body)? {
        if data.trim().is_empty() {
            continue;
        }
        let json: Value = serde_json::from_str(&data).map_err(|_| {
            invalid(format!(
                "a stream event is not JSON: {}",
                excerpt(data.as_bytes())
            ))
        })?;
        let result = rpc_result(&json, expected_id)?;
        if let Some(update) = stream_update(result, "statusUpdate", "status-update") {
            snapshot
                .get_or_insert_with(|| Snapshot::new(TaskState::Unknown))
                .apply_status(update)?;
        } else if let Some(update) = stream_update(result, "artifactUpdate", "artifact-update") {
            snapshot
                .get_or_insert_with(|| Snapshot::new(TaskState::Unknown))
                .apply_artifact(update)?;
        } else {
            snapshot = Some(result_snapshot(result)?);
        }
    }
    snapshot.ok_or_else(|| "The A2A agent stream contained no result events".to_string())
}

fn stream_update<'a>(result: &'a Value, v1_key: &str, v03_kind: &str) -> Option<&'a Value> {
    result.get(v1_key).or_else(|| {
        (result.get("kind").and_then(Value::as_str) == Some(v03_kind)).then_some(result)
    })
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

/// A2A lets a task omit its context, and the agent then infers it from the task id, so an
/// interrupted task is continued with whichever ids the agent returned.
fn continuation_hint(reply: &AgentReply, tool_name: &str) -> Option<String> {
    let state = reply.state.as_str();
    let context = reply.context_id.as_deref();
    let ids = match (context, reply.task_id.as_deref()) {
        (Some(c), Some(t)) => format!("context_id=\"{}\" and task_id=\"{}\"", c, t),
        (None, Some(t)) => format!("task_id=\"{}\"", t),
        _ => String::new(),
    };
    Some(match (reply.state, reply.task_id.as_deref()) {
        (TaskState::InputRequired, Some(_)) => format!(
            "[a2a state={}] The agent is waiting for your reply. Call {} again with {}.",
            state, tool_name, ids
        ),
        (TaskState::AuthRequired, Some(_)) => format!(
            "[a2a state={}] The agent needs authorization before it can continue. Complete it \
             outside this conversation as the agent describes, and never send passwords, tokens \
             or other secrets in a message: replying does not grant authorization by itself. \
             Once it is done, call {} again with {}.",
            state, tool_name, ids
        ),
        (s, Some(task)) if s.is_running() => match context {
            Some(context) => format!(
                "[a2a state={}] The agent is still working on task_id=\"{}\". Continue the \
                 conversation with context_id=\"{}\".",
                state, task, context
            ),
            None => format!(
                "[a2a state={}] The agent is still working on task_id=\"{}\".",
                state, task
            ),
        },
        _ => format!(
            "[a2a state={}] To continue this conversation, call {} with context_id=\"{}\".",
            state,
            tool_name,
            context?
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RESPONSE: &str = include_str!("../tests/fixtures/a2a_v1_send_message_response.json");

    fn parse_json(body: &Value, expected_id: &Value) -> Result<AgentReply, String> {
        parse_agent_response(200, "application/json", body.to_string().as_bytes(), expected_id)
    }

    fn parse_sse(body: &str) -> Result<AgentReply, String> {
        parse_agent_response(200, "text/event-stream", body.as_bytes(), &json!(1))
    }

    /// One SSE event carrying a JSON-RPC result with id 1.
    fn event(result: Value) -> String {
        format!("data: {}\n\n", json!({ "jsonrpc": "2.0", "id": 1, "result": result }))
    }

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
    fn builds_task_only_continuation() {
        let req = build_send_message(A2aVersion::V1, &json!(3), "m1", "acct-7", None, Some("t9"));
        let message = &req["params"]["message"];
        assert_eq!(message["taskId"], "t9");
        assert!(message.get("contextId").is_none());
    }

    #[test]
    fn parses_broker_sample_completed_task_with_data_part() {
        let reply =
            parse_agent_response(200, "application/json", SAMPLE_RESPONSE.as_bytes(), &json!("broker-1")).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.context_id.as_deref(), Some("59ec80cd-988c-47fd-8917-1f84dd3bd409"));
        assert_eq!(reply.task_id.as_deref(), Some("94da0605-5da3-4eaf-8aae-a34cea57ddb6"));
        assert_eq!(reply.data.len(), 1);
        assert_eq!(reply.data[0]["id"], "100200300");
        assert!(reply.texts[0].contains("SOP 2.2"));
    }

    #[test]
    fn maps_sample_to_mcp_tool_result() {
        let reply =
            parse_agent_response(200, "application/json", SAMPLE_RESPONSE.as_bytes(), &json!("broker-1")).unwrap();
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
        let reply = parse_json(&body, &json!(1)).unwrap();
        assert_eq!(reply.state, TaskState::InputRequired);
        assert_eq!(reply.texts, vec!["Which space?".to_string()]);
        let result = tool_result_from_reply(&reply, "ask_agent", true);
        assert_eq!(result["isError"], false);
        let hint = result["content"][1]["text"].as_str().unwrap();
        assert!(hint.contains("context_id=\"c9\" and task_id=\"t9\""), "{}", hint);
    }

    #[test]
    fn v03_task_result_is_not_wrapped() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": {
            "kind": "task", "id": "t1", "contextId": "c1",
            "status": { "state": "completed" },
            "artifacts": [{ "parts": [{ "kind": "text", "text": "done" }] }]
        }});
        let reply = parse_json(&body, &json!(1)).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.texts, vec!["done".to_string()]);
    }

    #[test]
    fn message_result_is_completed() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "message": {
            "role": "ROLE_AGENT", "contextId": "c2", "parts": [{ "text": "hello" }] }}});
        let reply = parse_json(&body, &json!(1)).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.context_id.as_deref(), Some("c2"));
        assert_eq!(reply.task_id, None);

        let v03 = json!({ "jsonrpc": "2.0", "id": 1, "result": {
            "kind": "message", "role": "agent", "parts": [{ "kind": "text", "text": "hi" }] }});
        assert_eq!(parse_json(&v03, &json!(1)).unwrap().texts, vec!["hi".to_string()]);
    }

    #[test]
    fn failed_task_is_an_mcp_tool_error() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "task": {
            "id": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_FAILED", "message": { "parts": [{ "text": "boom" }] } }
        }}});
        let reply = parse_json(&body, &json!(1)).unwrap();
        assert!(tool_result_from_reply(&reply, "ask_agent", true)["isError"].as_bool().unwrap());
    }

    #[test]
    fn jsonrpc_error_and_http_errors_become_messages() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32001, "message": "Task not found" } });
        let err = parse_json(&body, &json!(1)).unwrap_err();
        assert_eq!(err, "A2A error -32001: Task not found");

        let err = parse_agent_response(401, "application/json", body.to_string().as_bytes(), &json!(1)).unwrap_err();
        assert!(err.starts_with("A2A error -32001"));

        let err = parse_agent_response(502, "text/html", b"<html>Bad gateway</html>", &json!(1)).unwrap_err();
        assert_eq!(err, "The A2A agent returned HTTP 502: <html>Bad gateway</html>");
    }

    // Issue #6: malformed results are tool errors, not completed answers.
    #[test]
    fn malformed_results_are_invalid_responses() {
        let task = |t: Value| json!({ "task": t });
        for result in [
            json!(null),
            json!(42),
            json!("text"),
            json!([]),
            json!({}),
            json!({ "message": null }),
            json!({ "task": null }),
            json!({ "message": 3 }),
            json!({ "message": {} }),
            json!({ "message": { "parts": "hello" } }),
            json!({ "message": { "parts": [1] } }),
            json!({ "kind": "status-update", "status": { "state": "completed" } }),
            task(json!({ "status": { "state": "TASK_STATE_COMPLETED" } })),
            task(json!({ "id": 7, "status": { "state": "TASK_STATE_COMPLETED" } })),
            task(json!({ "id": "t" })),
            task(json!({ "id": "t", "status": null })),
            task(json!({ "id": "t", "status": {} })),
            task(json!({ "id": "t", "status": { "state": 3 } })),
            task(json!({ "id": "t", "status": { "state": "TASK_STATE_UNSPECIFIED" } })),
            task(json!({ "id": "t", "status": { "state": "done" } })),
            task(json!({ "id": "t", "status": { "state": "completed", "message": "hi" } })),
            task(json!({ "id": "t", "status": { "state": "completed", "message": { "parts": null } } })),
            task(json!({ "id": "t", "contextId": 5, "status": { "state": "completed" } })),
            task(json!({ "id": "t", "status": { "state": "completed" }, "artifacts": {} })),
            task(json!({ "id": "t", "status": { "state": "completed" }, "artifacts": [{}] })),
            task(json!({ "id": "t", "status": { "state": "completed" }, "artifacts": [{ "parts": [null] }] })),
        ] {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "result": result });
            let err = parse_json(&body, &json!(1)).expect_err(&result.to_string());
            assert!(err.starts_with(INVALID_RESPONSE), "{} -> {}", result, err);
        }
    }

    #[test]
    fn malformed_envelopes_are_invalid_responses() {
        let ok = json!({ "message": { "parts": [{ "text": "hi" }] } });
        for body in [
            json!({ "id": 1, "result": ok }),
            json!({ "jsonrpc": "1.0", "id": 1, "result": ok }),
            json!({ "jsonrpc": 2.0, "id": 1, "result": ok }),
            json!({ "jsonrpc": "2.0", "result": ok }),
            json!({ "jsonrpc": "2.0", "id": 2, "result": ok }),
            json!({ "jsonrpc": "2.0", "id": "1", "result": ok }),
            json!({ "jsonrpc": "2.0", "id": null, "result": ok }),
            json!({ "jsonrpc": "2.0", "id": 1 }),
            json!({ "jsonrpc": "2.0", "id": 1, "result": ok, "error": { "code": 1, "message": "x" } }),
            json!({ "jsonrpc": "2.0", "id": 2, "error": { "code": -32001, "message": "Task not found" } }),
            json!([{ "jsonrpc": "2.0", "id": 1, "result": ok }]),
        ] {
            let err = parse_json(&body, &json!(1)).expect_err(&body.to_string());
            assert!(err.starts_with(INVALID_RESPONSE), "{} -> {}", body, err);
        }
        // An error the agent could not correlate may carry a null id.
        let body = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } });
        assert_eq!(parse_json(&body, &json!(1)).unwrap_err(), "A2A error -32700: Parse error");
        // String ids are compared exactly.
        let body = json!({ "jsonrpc": "2.0", "id": "abc", "result": ok });
        assert!(parse_json(&body, &json!("abc")).is_ok());
    }

    #[test]
    fn folds_sse_stream_updates() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"task\":{\"id\":\"t\",\"contextId\":\"c\",\"status\":{\"state\":\"TASK_STATE_WORKING\"}}}}\r\n\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"artifactUpdate\":{\"taskId\":\"t\",\"contextId\":\"c\",\"artifact\":{\"parts\":[{\"text\":\"part one\"}]}}}}\n\n",
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"statusUpdate\":{\"taskId\":\"t\",\"contextId\":\"c\",\"status\":{\"state\":\"TASK_STATE_COMPLETED\"}}}}\n\n",
        );
        let reply = parse_sse(body).unwrap();
        assert_eq!(reply.state, TaskState::Completed);
        assert_eq!(reply.texts, vec!["part one".to_string()]);
        assert_eq!(reply.task_id.as_deref(), Some("t"));
    }

    // Issue #7: artifacts fold by artifactId; replace unless append is true.
    #[test]
    fn artifact_updates_replace_unless_appending() {
        let v1 = |id: &str, data: Value, append: Option<bool>| {
            let mut update = json!({ "taskId": "t", "contextId": "c",
                "artifact": { "artifactId": id, "parts": [{ "data": data }] } });
            if let Some(append) = append {
                update["append"] = json!(append);
            }
            event(json!({ "artifactUpdate": update }))
        };
        let start = event(json!({ "task": { "id": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_WORKING" },
            "artifacts": [{ "artifactId": "a", "parts": [{ "data": { "approved": "initial" } }] }] } }));
        let done = event(json!({ "statusUpdate": { "taskId": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_COMPLETED" } } }));

        // The reproduction from the issue, seeded from the initial Task, replaced twice.
        let body = [
            start.clone(),
            v1("a", json!({ "approved": true }), Some(false)),
            v1("a", json!({ "approved": false }), None),
            done.clone(),
        ]
        .concat();
        let reply = parse_sse(&body).unwrap();
        assert_eq!(reply.data, vec![json!({ "approved": false })]);
        assert_eq!(reply.texts, vec![json!({ "approved": false }).to_string()]);
        let result = tool_result_from_reply(&reply, "ask_agent", true);
        assert_eq!(result["structuredContent"]["data"], json!([{ "approved": false }]));

        // append: true extends; interleaved ids keep their own parts and order of first sight.
        let body = [
            start,
            v1("b", json!("b1"), None),
            v1("a", json!("a2"), Some(true)),
            v1("b", json!("b2"), Some(true)),
            v1("a", json!("a3"), Some(true)),
            done,
        ]
        .concat();
        let reply = parse_sse(&body).unwrap();
        assert_eq!(
            reply.data,
            vec![json!({ "approved": "initial" }), json!("a2"), json!("a3"), json!("b1"), json!("b2")]
        );

        // v0.3 kinds follow the same rules.
        let v03 = |id: &str, text: &str, append: bool| {
            event(json!({ "kind": "artifact-update", "taskId": "t", "contextId": "c", "append": append,
                "artifact": { "artifactId": id, "parts": [{ "kind": "text", "text": text }] } }))
        };
        let body = [
            event(json!({ "kind": "task", "id": "t", "contextId": "c", "status": { "state": "working" } })),
            v03("x", "draft", false),
            v03("x", "final", false),
            v03("x", " answer", true),
            event(json!({ "kind": "status-update", "taskId": "t", "contextId": "c",
                "status": { "state": "completed" }, "final": true })),
        ]
        .concat();
        let reply = parse_sse(&body).unwrap();
        assert_eq!(reply.texts, vec!["final".to_string(), " answer".to_string()]);
    }

    #[test]
    fn status_update_replaces_the_status_message() {
        let body = [
            event(json!({ "task": { "id": "t", "status": { "state": "TASK_STATE_WORKING",
                "message": { "parts": [{ "text": "Looking it up…" }] } } } })),
            event(json!({ "statusUpdate": { "taskId": "t", "status": { "state": "TASK_STATE_INPUT_REQUIRED",
                "message": { "parts": [{ "text": "Which account?" }] } } } })),
        ]
        .concat();
        let reply = parse_sse(&body).unwrap();
        assert_eq!(reply.state, TaskState::InputRequired);
        assert_eq!(reply.texts, vec!["Which account?".to_string()]);
    }

    // Issue #8: SSE framing and corrupt events.
    #[test]
    fn sse_accepts_every_line_ending_bom_comments_and_multiline_data() {
        let task = json!({ "jsonrpc": "2.0", "id": 1, "result": { "task": { "id": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_WORKING" } } } })
        .to_string();
        let artifact = json!({ "jsonrpc": "2.0", "id": 1, "result": { "artifactUpdate": {
            "artifact": { "artifactId": "a", "parts": [{ "text": "answer" }] } } } })
        .to_string();
        let done = json!({ "jsonrpc": "2.0", "id": 1, "result": { "statusUpdate": {
            "status": { "state": "TASK_STATE_COMPLETED" } } } })
        .to_string();
        for eol in ["\n", "\r\n", "\r"] {
            let body = format!(
                "\u{feff}: keep-alive{eol}{eol}event: message{eol}data: {task}{eol}{eol}data:{artifact}{eol}{eol}:{eol}data: {done}{eol}{eol}",
            );
            let reply = parse_sse(&body).unwrap_or_else(|e| panic!("{:?}: {}", eol, e));
            assert_eq!(reply.state, TaskState::Completed, "{:?}", eol);
            assert_eq!(reply.texts, vec!["answer".to_string()], "{:?}", eol);
        }

        // A single event split over several data lines, and an empty data event that is ignored.
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\ndata: \"result\":{\"message\":{\"parts\":[{\"text\":\"hi\"}]}}}\n\ndata:\n\n";
        assert_eq!(parse_sse(body).unwrap().texts, vec!["hi".to_string()]);

        // The BOM case from the issue: one valid event that used to report no result events.
        let body = format!("\u{feff}data: {}\n\n", task);
        assert_eq!(parse_sse(&body).unwrap().state, TaskState::Working);
    }

    #[test]
    fn corrupt_or_truncated_streams_are_errors() {
        let valid = event(json!({ "task": { "id": "t", "contextId": "c",
            "status": { "state": "TASK_STATE_COMPLETED" },
            "artifacts": [{ "parts": [{ "text": "earlier answer" }] }] } }));
        for (case, tail) in [
            ("malformed JSON", "data: {not json\n\n".to_string()),
            ("non-JSON-RPC envelope", "data: {\"result\":{}}\n\n".to_string()),
            ("event without result or error", "data: {\"jsonrpc\":\"2.0\",\"id\":1}\n\n".to_string()),
            ("result for another request", "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"message\":{\"parts\":[]}}}\n\n".to_string()),
            ("malformed status update", event(json!({ "statusUpdate": { "status": {} } }))),
            ("malformed artifact update", event(json!({ "artifactUpdate": { "artifact": { "parts": "x" } } }))),
            ("unterminated last event", "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"message\":{\"parts\":[]}}}".to_string()),
            ("event without its blank line", "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"message\":{\"parts\":[]}}}\n".to_string()),
        ] {
            let err = parse_sse(&format!("{}{}", valid, tail)).expect_err(case);
            assert!(err.starts_with(INVALID_RESPONSE), "{}: {}", case, err);
        }

        let error = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32603,\"message\":\"Internal error\"}}\n\n";
        assert_eq!(parse_sse(&format!("{}{}", valid, error)).unwrap_err(), "A2A error -32603: Internal error");
        // A trailing comment without a line end is not an event.
        assert!(parse_sse(&format!("{}: bye", valid)).is_ok());
        assert_eq!(parse_sse(": only comments\n\n").unwrap_err(), "The A2A agent stream contained no result events");
    }

    // Issue #9: an interrupted task without a contextId is still continuable.
    #[test]
    fn input_required_without_context_keeps_the_task_hint() {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "task": { "id": "t9",
            "status": { "state": "TASK_STATE_INPUT_REQUIRED", "message": {
                "role": "ROLE_AGENT", "parts": [{ "text": "Which account?" }] } } } } });
        let reply = parse_json(&body, &json!(1)).unwrap();
        assert_eq!(reply.context_id, None);

        let plain = tool_result_from_reply(&reply, "ask_agent", false);
        assert_eq!(plain["content"][0]["text"], "Which account?");
        let hint = plain["content"][1]["text"].as_str().expect("a continuation hint");
        assert!(hint.starts_with("[a2a state=input-required]"), "{}", hint);
        assert!(hint.contains("Call ask_agent again with task_id=\"t9\"."), "{}", hint);
        assert!(!hint.contains("context_id"), "{}", hint);

        let structured = tool_result_from_reply(&reply, "ask_agent", true);
        assert_eq!(structured["structuredContent"], json!({ "state": "input-required", "taskId": "t9" }));
    }

    #[test]
    fn auth_required_hint_never_implies_the_reply_authorizes() {
        for context in [Some("c1"), None] {
            let mut reply = AgentReply::empty(TaskState::AuthRequired);
            reply.task_id = Some("t1".to_string());
            reply.context_id = context.map(str::to_string);
            let hint = continuation_hint(&reply, "ask_agent").unwrap();
            assert!(hint.starts_with("[a2a state=auth-required]"), "{}", hint);
            assert!(hint.contains("outside this conversation"), "{}", hint);
            assert!(hint.contains("never send passwords, tokens or other secrets"), "{}", hint);
            assert!(hint.contains("replying does not grant authorization"), "{}", hint);
            assert!(hint.contains("task_id=\"t1\""), "{}", hint);
            assert_eq!(hint.contains("context_id=\"c1\""), context.is_some(), "{}", hint);
        }
    }

    #[test]
    fn hints_for_running_and_finished_tasks() {
        let mut reply = AgentReply::empty(TaskState::Working);
        reply.task_id = Some("t2".to_string());
        assert_eq!(
            continuation_hint(&reply, "ask_agent").unwrap(),
            "[a2a state=working] The agent is still working on task_id=\"t2\"."
        );
        reply.context_id = Some("c2".to_string());
        assert!(continuation_hint(&reply, "ask_agent").unwrap().contains("context_id=\"c2\""));

        // A finished task without a context has nothing to continue.
        let mut done = AgentReply::empty(TaskState::Completed);
        done.task_id = Some("t3".to_string());
        assert_eq!(continuation_hint(&done, "ask_agent"), None);
    }

    #[test]
    fn describes_file_parts_in_both_versions() {
        let mut reply = AgentReply::empty(TaskState::Completed);
        reply.absorb_parts(&[
            json!({ "kind": "file", "file": { "name": "a.pdf", "mimeType": "application/pdf", "uri": "https://x/a.pdf" } }),
            json!({ "url": "https://x/b.png", "mediaType": "image/png", "filename": "b.png" }),
            json!({ "raw": "AAAA", "mediaType": "image/png" }),
        ]);
        assert_eq!(reply.texts[0], "[file a.pdf (application/pdf): https://x/a.pdf]");
        assert_eq!(reply.texts[1], "[file b.png (image/png): https://x/b.png]");
        assert_eq!(reply.texts[2], "[file file (image/png): inline content omitted]");
    }

    #[test]
    fn task_state_accepts_both_spellings_and_rejects_others() {
        assert_eq!(TaskState::parse("TASK_STATE_AUTH_REQUIRED"), Some(TaskState::AuthRequired));
        assert_eq!(TaskState::parse("input-required"), Some(TaskState::InputRequired));
        assert_eq!(TaskState::parse("cancelled"), Some(TaskState::Canceled));
        assert_eq!(TaskState::parse("unknown"), Some(TaskState::Unknown));
        assert_eq!(TaskState::parse("TASK_STATE_UNSPECIFIED"), None);
        assert_eq!(TaskState::parse("done"), None);
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
