// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Experimental: MCP elicitation for A2A `input-required` tasks (`enableElicitation`, off by
//! default).
//!
//! A policy cannot keep a response open while the user thinks, so a question travels on a short
//! SSE stream that the client resumes, as Streamable HTTP allows:
//! 1. `tools/call` -> the agent answers `input-required` -> the bridge answers with an SSE stream
//!    holding one `elicitation/create` request (event id `<stream>-<seq>`) and a `retry` hint,
//!    and ends the stream.
//! 2. The client POSTs its JSON-RPC response. An accepted answer is sent to the agent as the next
//!    message of the task; the client gets 202.
//! 3. The client reconnects with GET and `Last-Event-ID` and receives the agent's next question
//!    or the `tools/call` result under the original request id.
//!
//! Decline, cancel and timeout end the conversation: the stored context and task are dropped.
//! `auth-required` is never turned into a form, and the form never asks for secrets.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::a2a::{AgentReply, TaskState};
use crate::mcp;

pub const ELICITATION_CREATE_METHOD_NAME: &str = "elicitation/create";
pub const LAST_EVENT_ID_HEADER: &str = "last-event-id";
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";
/// The first MCP revision with elicitation.
const FIRST_VERSION_WITH_ELICITATION: &str = "2025-06-18";
/// How soon the client reconnects while the user has not answered yet.
pub const RETRY_MILLIS: u64 = 2000;
pub const MIN_TIMEOUT_SECONDS: u64 = 30;
pub const MAX_TIMEOUT_SECONDS: u64 = 1800;
pub const MAX_ROUNDS_LIMIT: u32 = 10;
/// Longer questions are returned as a normal tool result instead of a form.
pub const MAX_QUESTION_BYTES: usize = 32 * 1024;
const ANSWER_FIELD: &str = "answer";
const DEFAULT_QUESTION: &str = "The agent needs more information to continue.";

const DECLINED_TEXT: &str = "The user declined to answer the agent's question, so this conversation \
     with the agent was ended. Start a new conversation to try again.";
const CANCELLED_TEXT: &str = "The user cancelled the agent's question, so this conversation with the \
     agent was ended. Start a new conversation to try again.";
const TIMEOUT_TEXT: &str = "The user did not answer the agent's question in time, so this \
     conversation with the agent was ended. Start a new conversation to try again.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub timeout_seconds: u64,
    pub max_rounds: u32,
}

impl Settings {
    /// `None` unless enabled and a session store exists to hold the question between requests.
    /// The wait is capped by the session TTL, since an expired session cannot be resumed.
    pub fn from_config(
        enabled: bool,
        has_store: bool,
        timeout_seconds: Option<i64>,
        max_rounds: Option<i64>,
        session_ttl_seconds: u64,
    ) -> Option<Self> {
        if !enabled || !has_store {
            return None;
        }
        let timeout = timeout_seconds.unwrap_or(300).max(0) as u64;
        let rounds = max_rounds.unwrap_or(3).clamp(1, MAX_ROUNDS_LIMIT as i64) as u32;
        Some(Settings {
            timeout_seconds: timeout
                .clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS)
                .min(session_ttl_seconds),
            max_rounds: rounds,
        })
    }
}

/// A question waiting on the client, stored with the session so any replica can resume it.
/// Plain types only: the store serialises with bincode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// Random id of this `tools/call`'s event stream; event ids are `<stream>-<seq>`.
    pub stream: String,
    /// The `tools/call` request id the final result answers, as JSON text.
    pub call_id: String,
    pub context_id: Option<String>,
    pub task_id: String,
    /// Questions asked so far for this call (1-based).
    pub round: u32,
    /// Unix seconds after which the question has expired.
    pub deadline: u64,
    /// Sequence number of the latest event (question or result).
    pub seq: u32,
    pub step: Step,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// `elicitation/create` with this id is (or is about to be) on the stream.
    Asked { request_id: String, question: String },
    /// The answer was sent to the agent; its reply is not in yet.
    Answering,
    /// The serialised JSON-RPC response to the `tools/call`.
    Done { response: String },
}

impl Pending {
    /// The first question of a `tools/call`; `None` unless the agent named the task to continue.
    pub fn first(stream: String, call_id: &Value, reply: &AgentReply, deadline: u64) -> Option<Self> {
        let task_id = reply.task_id.clone()?;
        let request_id = request_id(&stream, 1);
        Some(Pending {
            stream,
            call_id: call_id.to_string(),
            context_id: reply.context_id.clone(),
            task_id,
            round: 1,
            deadline,
            seq: 1,
            step: Step::Asked { request_id, question: question_text(reply) },
        })
    }

    /// The agent asked again: the next question goes on the same stream.
    pub fn ask_again(&mut self, reply: &AgentReply, deadline: u64) {
        self.round += 1;
        self.seq += 1;
        if let Some(task_id) = &reply.task_id {
            self.task_id = task_id.clone();
        }
        if reply.context_id.is_some() {
            self.context_id = reply.context_id.clone();
        }
        self.deadline = deadline;
        self.step = Step::Asked {
            request_id: request_id(&self.stream, self.round),
            question: question_text(reply),
        };
    }

    pub fn finish(&mut self, response: String) {
        self.seq += 1;
        self.step = Step::Done { response };
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.step, Step::Done { .. })
    }

    pub fn asked_request_id(&self) -> Option<&str> {
        match &self.step {
            Step::Asked { request_id, .. } => Some(request_id),
            _ => None,
        }
    }

    pub fn event_id(&self) -> String {
        format!("{}-{}", self.stream, self.seq)
    }

    pub fn call_id(&self) -> Value {
        serde_json::from_str(&self.call_id).unwrap_or(Value::Null)
    }
}

/// Unique within the session, so a stale answer can never be taken for the current question.
fn request_id(stream: &str, round: u32) -> String {
    format!("{}-q{}", stream, round)
}

/// The client declared form elicitation at `initialize` (`{}` means form in every revision).
pub fn client_declares(initialize_params: Option<&Map<String, Value>>) -> bool {
    initialize_params
        .and_then(|p| p.get("capabilities"))
        .and_then(|c| c.get("elicitation"))
        .map_or(false, Value::is_object)
}

/// Elicitation exists from 2025-06-18; dates compare correctly as strings.
pub fn version_supports(negotiated: &str) -> bool {
    negotiated >= FIRST_VERSION_WITH_ELICITATION
}

/// Only a question the user can answer in a plain form: `input-required` with a task to resume.
/// `auth-required` is never elicited, because form mode must not collect credentials.
pub fn is_elicitable(reply: &AgentReply) -> bool {
    reply.state == TaskState::InputRequired
        && reply.task_id.is_some()
        && reply.texts.iter().map(String::len).sum::<usize>() <= MAX_QUESTION_BYTES
}

fn question_text(reply: &AgentReply) -> String {
    let text = reply.texts.join("\n\n");
    if text.trim().is_empty() {
        DEFAULT_QUESTION.to_string()
    } else {
        text
    }
}

/// `elicitation/create` in form mode with one free-text answer.
pub fn create_request(request_id: &str, question: &str) -> Value {
    json!({
        "jsonrpc": mcp::JSONRPC_VERSION,
        "id": request_id,
        "method": ELICITATION_CREATE_METHOD_NAME,
        "params": {
            "message": question,
            "requestedSchema": {
                "type": "object",
                "properties": {
                    ANSWER_FIELD: {
                        "type": "string",
                        "title": "Your answer",
                        "description": "Your reply to the agent. Never enter passwords, tokens or other secrets."
                    }
                },
                "required": [ANSWER_FIELD]
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Accept(String),
    Decline,
    Cancel,
    /// The client answered with a JSON-RPC error, e.g. it could not show the form.
    Failed,
}

/// Reads the client's response to `elicitation/create`.
pub fn parse_answer(envelope: &Map<String, Value>) -> Result<Answer, String> {
    if envelope.contains_key("error") {
        return Ok(Answer::Failed);
    }
    let result = envelope.get("result").and_then(Value::as_object).ok_or("The elicitation result must be an object")?;
    match result.get("action").and_then(Value::as_str) {
        Some("accept") => result
            .get("content")
            .and_then(|c| c.get(ANSWER_FIELD))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| Answer::Accept(a.to_string()))
            .ok_or_else(|| format!("An accepted elicitation must carry a non-empty content.{}", ANSWER_FIELD)),
        Some("decline") => Ok(Answer::Decline),
        Some("cancel") => Ok(Answer::Cancel),
        _ => Err("The elicitation action must be accept, decline or cancel".to_string()),
    }
}

/// The `<stream>-<seq>` parts of a `Last-Event-ID`. Stream ids are UUIDs, so the last `-` splits.
pub fn parse_event_id(value: &str) -> Option<(&str, u32)> {
    let (stream, seq) = value.trim().rsplit_once('-')?;
    Some((stream, seq.parse().ok()?)).filter(|(s, _)| !s.is_empty())
}

/// One SSE event. JSON from serde has no raw newlines, so a single `data` line is enough.
pub fn sse_event(id: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    if let Some(id) = id {
        out.extend_from_slice(format!("id: {}\n", id).as_bytes());
    }
    out.extend_from_slice(b"data: ");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\n\n");
    out
}

fn retry_field() -> Vec<u8> {
    format!("retry: {}\n\n", RETRY_MILLIS).into_bytes()
}

/// The question, then a `retry` hint; the stream then ends and the client resumes with GET.
pub fn question_stream(pending: &Pending) -> Option<Vec<u8>> {
    let Step::Asked { request_id, question } = &pending.step else {
        return None;
    };
    let request = create_request(request_id, question).to_string();
    let mut out = sse_event(Some(&pending.event_id()), request.as_bytes());
    out.extend(retry_field());
    Some(out)
}

/// Nothing new yet: a comment and a `retry` hint, so the client reconnects with the same id.
pub fn waiting_stream() -> Vec<u8> {
    let mut out = b": waiting for the user's answer\n".to_vec();
    out.extend(retry_field());
    out
}

pub fn result_stream(pending: &Pending) -> Option<Vec<u8>> {
    let Step::Done { response } = &pending.step else {
        return None;
    };
    Some(sse_event(Some(&pending.event_id()), response.as_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Declined,
    Cancelled,
    TimedOut,
}

/// The tool result of a conversation the user ended or abandoned.
pub fn ended_result(ending: Ending, structured: bool) -> Value {
    let (text, reason, is_error) = match ending {
        Ending::Declined => (DECLINED_TEXT, "declined", false),
        Ending::Cancelled => (CANCELLED_TEXT, "cancelled", false),
        Ending::TimedOut => (TIMEOUT_TEXT, "timeout", true),
    };
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": is_error });
    if structured {
        result["structuredContent"] = json!({ "state": "ended", "reason": reason });
    }
    result
}

/// The question as an ordinary tool result with continuation ids, for a client that could not
/// show the form: the conversation goes on the client-managed way.
pub fn client_managed_reply(pending: &Pending) -> AgentReply {
    let question = match &pending.step {
        Step::Asked { question, .. } => question.clone(),
        _ => DEFAULT_QUESTION.to_string(),
    };
    AgentReply {
        state: TaskState::InputRequired,
        context_id: pending.context_id.clone(),
        task_id: Some(pending.task_id.clone()),
        texts: vec![question],
        data: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(state: TaskState, task: Option<&str>, texts: &[&str]) -> AgentReply {
        AgentReply {
            state,
            context_id: Some("ctx".to_string()),
            task_id: task.map(str::to_string),
            texts: texts.iter().map(|t| t.to_string()).collect(),
            data: Vec::new(),
        }
    }

    fn envelope(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn settings_are_off_by_default_need_a_store_and_are_bounded() {
        assert_eq!(Settings::from_config(false, true, None, None, 1800), None);
        assert_eq!(Settings::from_config(true, false, None, None, 1800), None);
        assert_eq!(
            Settings::from_config(true, true, None, None, 1800),
            Some(Settings { timeout_seconds: 300, max_rounds: 3 })
        );
        let s = Settings::from_config(true, true, Some(1), Some(0), 1800).unwrap();
        assert_eq!((s.timeout_seconds, s.max_rounds), (30, 1));
        let s = Settings::from_config(true, true, Some(99_999), Some(99), 1800).unwrap();
        assert_eq!((s.timeout_seconds, s.max_rounds), (1800, 10));
        // An expired session cannot be resumed, so the TTL caps the wait.
        assert_eq!(Settings::from_config(true, true, Some(900), None, 120).unwrap().timeout_seconds, 120);
    }

    #[test]
    fn capability_and_version_gate_elicitation() {
        let params = |caps: Value| envelope(json!({ "capabilities": caps }));
        assert!(client_declares(Some(&params(json!({ "elicitation": {} })))));
        assert!(client_declares(Some(&params(json!({ "elicitation": { "form": {} } })))));
        assert!(!client_declares(Some(&params(json!({})))));
        assert!(!client_declares(Some(&params(json!({ "elicitation": true })))));
        assert!(!client_declares(None));
        assert!(version_supports("2025-06-18"));
        assert!(version_supports("2025-11-25"));
        assert!(!version_supports("2025-03-26"));
        assert!(!version_supports("2024-11-05"));
    }

    #[test]
    fn only_input_required_tasks_are_elicited() {
        assert!(is_elicitable(&reply(TaskState::InputRequired, Some("t"), &["Which city?"])));
        assert!(!is_elicitable(&reply(TaskState::InputRequired, None, &["Which city?"])));
        assert!(!is_elicitable(&reply(TaskState::AuthRequired, Some("t"), &["Sign in"])));
        assert!(!is_elicitable(&reply(TaskState::Completed, Some("t"), &["Done"])));
        let long = "x".repeat(MAX_QUESTION_BYTES + 1);
        assert!(!is_elicitable(&reply(TaskState::InputRequired, Some("t"), &[&long])));
    }

    #[test]
    fn the_form_asks_for_one_free_text_answer_and_never_for_secrets() {
        let request = create_request("s-q1", "Which city?");
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(request["id"], "s-q1");
        assert_eq!(request["params"]["message"], "Which city?");
        let schema = &request["params"]["requestedSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["answer"]));
        assert_eq!(schema["properties"]["answer"]["type"], "string");
        assert!(schema["properties"]["answer"].get("format").is_none());
        assert!(schema["properties"]["answer"]["description"].as_str().unwrap().contains("Never enter passwords"));
        assert!(request["params"].get("mode").is_none(), "2025-06-18 has no mode field");
    }

    #[test]
    fn answers_are_read_strictly() {
        let ok = |result: Value| parse_answer(&envelope(json!({ "jsonrpc": "2.0", "id": "q", "result": result })));
        assert_eq!(ok(json!({ "action": "accept", "content": { "answer": " Paris " } })), Ok(Answer::Accept("Paris".into())));
        assert_eq!(ok(json!({ "action": "decline" })), Ok(Answer::Decline));
        assert_eq!(ok(json!({ "action": "cancel" })), Ok(Answer::Cancel));
        assert!(ok(json!({ "action": "accept", "content": {} })).is_err());
        assert!(ok(json!({ "action": "accept", "content": { "answer": "  " } })).is_err());
        assert!(ok(json!({ "action": "accept", "content": { "answer": 5 } })).is_err());
        assert!(ok(json!({ "action": "maybe" })).is_err());
        assert!(ok(json!("accept")).is_err());
        let failed = envelope(json!({ "jsonrpc": "2.0", "id": "q", "error": { "code": -32602, "message": "no" } }));
        assert_eq!(parse_answer(&failed), Ok(Answer::Failed));
    }

    #[test]
    fn event_ids_round_trip_through_uuid_streams() {
        let stream = "8f0e7c1a-1b2c-4d3e-8f00-112233445566";
        assert_eq!(parse_event_id(&format!("{}-7", stream)), Some((stream, 7)));
        assert_eq!(parse_event_id("nodash"), None);
        assert_eq!(parse_event_id("abc-x"), None);
        assert_eq!(parse_event_id("-3"), None);
    }

    #[test]
    fn streams_carry_the_question_then_a_retry_hint() {
        let mut pending = Pending::first(
            "s".into(),
            &json!(7),
            &reply(TaskState::InputRequired, Some("t1"), &["Which city?"]),
            100,
        )
        .unwrap();
        let text = String::from_utf8(question_stream(&pending).unwrap()).unwrap();
        assert!(text.starts_with("id: s-1\ndata: {"), "{}", text);
        assert!(text.ends_with("\n\nretry: 2000\n\n"), "{}", text);
        let data: Value = serde_json::from_str(text.lines().nth(1).unwrap().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(data["id"], "s-q1");

        pending.ask_again(&reply(TaskState::InputRequired, Some("t1"), &[]), 200);
        assert_eq!((pending.round, pending.seq, pending.deadline), (2, 2, 200));
        assert_eq!(pending.asked_request_id(), Some("s-q2"));
        assert!(matches!(&pending.step, Step::Asked { question, .. } if question == DEFAULT_QUESTION));

        pending.finish("{\"id\":7}".into());
        assert!(!pending.is_open());
        assert_eq!(String::from_utf8(result_stream(&pending).unwrap()).unwrap(), "id: s-3\ndata: {\"id\":7}\n\n");
        assert!(String::from_utf8(waiting_stream()).unwrap().ends_with("retry: 2000\n\n"));
    }

    #[test]
    fn ended_conversations_say_why() {
        let declined = ended_result(Ending::Declined, true);
        assert_eq!(declined["isError"], false);
        assert_eq!(declined["structuredContent"]["reason"], "declined");
        assert_eq!(ended_result(Ending::TimedOut, false)["isError"], true);
        assert!(ended_result(Ending::TimedOut, false).get("structuredContent").is_none());
    }
}
