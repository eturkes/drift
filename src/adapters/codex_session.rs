use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{BufReader, Cursor, Write},
};

use serde_json::{Map, Value, json};

use crate::{
    Completeness, DriftError, OutcomeStatus, Result, Source, SourceTrust, ToolStatus, TraceRecord,
    digest::sha256_bytes,
    json::ensure_unique_keys,
    parse::{MAX_EVENTS, MAX_FILE_BYTES, MAX_LINE_BYTES, parse_trace},
};

const TRACE_SCHEMA: &str = "drift.trace/v1";
const ADAPTER: &str = "codex-session-jsonl";

/// Convert one persisted Codex rollout into strict `drift.trace/v1` JSONL.
///
/// Rollouts contain host instructions, compaction payloads, and encrypted reasoning alongside the
/// visible transcript. This adapter imports only `event_msg` user/agent messages and
/// `response_item` tool calls/results. Unknown and private record families remain excluded.
pub fn import(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(DriftError::new(
            "E_CODEX_SESSION_TOO_LARGE",
            format!("Codex session exceeds {MAX_FILE_BYTES} bytes"),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        DriftError::new(
            "E_CODEX_SESSION_JSON",
            format!("Codex session is not UTF-8: {error}"),
        )
    })?;
    let mut importer = Importer::default();
    for (index, raw_line) in text.split_terminator('\n').enumerate() {
        let line_number = index + 1;
        if line_number > MAX_EVENTS {
            return Err(at_line(
                "E_CODEX_SESSION_TOO_MANY_RECORDS",
                line_number,
                format!("Codex session exceeds {MAX_EVENTS} records"),
            ));
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.len() > MAX_LINE_BYTES {
            return Err(at_line(
                "E_CODEX_SESSION_LINE_TOO_LARGE",
                line_number,
                format!("Codex session record exceeds {MAX_LINE_BYTES} bytes"),
            ));
        }
        if line.trim().is_empty() {
            return Err(at_line(
                "E_CODEX_SESSION_BLANK_LINE",
                line_number,
                "blank lines are not valid Codex session records",
            ));
        }
        ensure_unique_keys(line.as_bytes()).map_err(|error| {
            at_line(
                "E_CODEX_SESSION_JSON",
                line_number,
                format!(
                    "invalid Codex session record at column {}: {error}",
                    error.column()
                ),
            )
        })?;
        let value: Value = serde_json::from_str(line).map_err(|error| {
            at_line(
                "E_CODEX_SESSION_JSON",
                line_number,
                format!(
                    "invalid Codex session record at column {}: {error}",
                    error.column()
                ),
            )
        })?;
        importer.accept(value, line_number)?;
    }
    importer.finish(sha256_bytes(bytes))
}

#[derive(Debug)]
struct SessionMeta {
    id: String,
    started_at: Option<String>,
    cli_version: Option<String>,
    source: Option<String>,
}

#[derive(Default)]
struct Importer {
    meta: Option<SessionMeta>,
    task: Option<String>,
    events: Vec<TraceRecord>,
    calls: HashMap<String, usize>,
    results: HashSet<String>,
    ignored: BTreeMap<String, usize>,
    latest_turn_complete: bool,
    terminal_at: Option<String>,
    next_event: usize,
}

impl Importer {
    fn accept(&mut self, value: Value, line: usize) -> Result<()> {
        let envelope = object(&value, "record", line)?;
        let record_type = string(envelope, "type", line)?;
        let payload = object_field(envelope, "payload", line)?;
        let timestamp = optional_string(envelope, "timestamp", line)?;

        if self.meta.is_none() && record_type != "session_meta" {
            return Err(at_line(
                "E_CODEX_SESSION_PROTOCOL",
                line,
                "first record must be session_meta",
            ));
        }

        match record_type {
            "session_meta" => self.accept_meta(payload, line)?,
            "event_msg" => self.accept_event(payload, timestamp, line)?,
            "response_item" => self.accept_response_item(payload, timestamp, line)?,
            other => self.ignore(other),
        }
        Ok(())
    }

    fn accept_meta(&mut self, payload: &Map<String, Value>, line: usize) -> Result<()> {
        if self.meta.is_some() {
            return Err(at_line(
                "E_CODEX_SESSION_PROTOCOL",
                line,
                "session_meta may appear only once",
            ));
        }
        let id = string(payload, "id", line)?.to_owned();
        let session_id = optional_string(payload, "session_id", line)?;
        if let Some(session_id) = session_id.as_deref()
            && session_id != id
        {
            return Err(at_line(
                "E_CODEX_SESSION_PROTOCOL",
                line,
                "session_meta id and session_id differ",
            ));
        }
        self.meta = Some(SessionMeta {
            id,
            started_at: optional_string(payload, "timestamp", line)?,
            cli_version: optional_string(payload, "cli_version", line)?,
            source: payload
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
        Ok(())
    }

    fn accept_event(
        &mut self,
        payload: &Map<String, Value>,
        timestamp: Option<String>,
        line: usize,
    ) -> Result<()> {
        let event_type = string(payload, "type", line)?;
        match event_type {
            "task_started" => {
                self.latest_turn_complete = false;
                self.terminal_at = None;
            }
            "task_complete" => {
                self.latest_turn_complete = true;
                self.terminal_at = timestamp;
            }
            "user_message" => {
                self.latest_turn_complete = false;
                self.terminal_at = None;
                let message = string(payload, "message", line)?.to_owned();
                if self.task.is_none() && !message.trim().is_empty() {
                    self.task = Some(message.clone());
                }
                self.push_message("user:codex", message, timestamp, "user_message", payload);
            }
            "agent_message" => {
                let message = string(payload, "message", line)?.to_owned();
                self.push_message("agent:codex", message, timestamp, "agent_message", payload);
            }
            other => self.ignore(&format!("event_msg.{other}")),
        }
        Ok(())
    }

    fn accept_response_item(
        &mut self,
        payload: &Map<String, Value>,
        timestamp: Option<String>,
        line: usize,
    ) -> Result<()> {
        let item_type = string(payload, "type", line)?;
        match item_type {
            "custom_tool_call" => {
                let name = string(payload, "name", line)?.to_owned();
                let input = payload.get("input").cloned().ok_or_else(|| {
                    at_line(
                        "E_CODEX_SESSION_PROTOCOL",
                        line,
                        "custom_tool_call requires input",
                    )
                })?;
                self.push_tool_call(payload, name, input, timestamp, item_type, line)?;
            }
            "function_call" => {
                let name = string(payload, "name", line)?;
                let namespace = optional_string(payload, "namespace", line)?;
                let full_name =
                    namespace.map_or_else(|| name.to_owned(), |value| format!("{value}.{name}"));
                let input = payload.get("arguments").cloned().ok_or_else(|| {
                    at_line(
                        "E_CODEX_SESSION_PROTOCOL",
                        line,
                        "function_call requires arguments",
                    )
                })?;
                self.push_tool_call(payload, full_name, input, timestamp, item_type, line)?;
            }
            "custom_tool_call_output" | "function_call_output" => {
                let call_id = string(payload, "call_id", line)?.to_owned();
                if !self.results.insert(call_id.clone()) {
                    return Err(at_line(
                        "E_CODEX_SESSION_TOOL_LIFECYCLE",
                        line,
                        format!("duplicate result for tool call {call_id:?}"),
                    ));
                }
                let output = payload.get("output").cloned().ok_or_else(|| {
                    at_line(
                        "E_CODEX_SESSION_PROTOCOL",
                        line,
                        format!("{item_type} requires output"),
                    )
                })?;
                let id = self.event_id();
                self.events.push(TraceRecord::ToolResult {
                    id,
                    actor: "tool:codex".into(),
                    call_id,
                    status: ToolStatus::Unknown,
                    output,
                    at: timestamp,
                    duration_ms: None,
                    extensions: response_extension(item_type, payload),
                });
            }
            // Visible messages arrive through event_msg without injected developer/system context.
            "message" | "agent_message" | "reasoning" => {
                self.ignore(&format!("response_item.{item_type}"));
            }
            other => self.ignore(&format!("response_item.{other}")),
        }
        Ok(())
    }

    fn push_tool_call(
        &mut self,
        payload: &Map<String, Value>,
        name: String,
        input: Value,
        timestamp: Option<String>,
        item_type: &str,
        line: usize,
    ) -> Result<()> {
        let call_id = string(payload, "call_id", line)?.to_owned();
        if self.calls.insert(call_id.clone(), line).is_some() {
            return Err(at_line(
                "E_CODEX_SESSION_TOOL_LIFECYCLE",
                line,
                format!("duplicate tool call {call_id:?}"),
            ));
        }
        let id = self.event_id();
        self.events.push(TraceRecord::ToolCall {
            id,
            actor: "agent:codex".into(),
            call_id,
            name,
            input,
            at: timestamp,
            duration_ms: None,
            extensions: response_extension(item_type, payload),
        });
        Ok(())
    }

    fn push_message(
        &mut self,
        actor: &str,
        text: String,
        timestamp: Option<String>,
        event_type: &str,
        payload: &Map<String, Value>,
    ) {
        let id = self.event_id();
        let mut detail = Map::from_iter([("event".into(), json!(event_type))]);
        if let Some(phase) = payload.get("phase").and_then(Value::as_str) {
            detail.insert("phase".into(), json!(phase));
        }
        self.events.push(TraceRecord::Message {
            id,
            actor: actor.into(),
            text,
            at: timestamp,
            duration_ms: None,
            extensions: BTreeMap::from([("codex_session".into(), Value::Object(detail))]),
        });
    }

    fn ignore(&mut self, kind: &str) {
        *self.ignored.entry(kind.to_owned()).or_default() += 1;
    }

    fn event_id(&mut self) -> String {
        self.next_event += 1;
        format!("e{}", self.next_event)
    }

    fn finish(mut self, source_digest: String) -> Result<Vec<u8>> {
        let meta = self.meta.take().ok_or_else(|| {
            DriftError::new(
                "E_CODEX_SESSION_PROTOCOL",
                "Codex session has no session_meta record",
            )
        })?;
        let fully_linked = self
            .calls
            .keys()
            .all(|call_id| self.results.contains(call_id))
            && self
                .results
                .iter()
                .all(|call_id| self.calls.contains_key(call_id));
        let completeness = if self.latest_turn_complete && fully_linked {
            Completeness::Complete
        } else {
            Completeness::Partial
        };
        if self.latest_turn_complete {
            let id = self.event_id();
            self.events.push(TraceRecord::Outcome {
                id,
                actor: "environment:codex".into(),
                status: OutcomeStatus::Unknown,
                text: "Codex recorded task completion; the session does not independently establish task success.".into(),
                at: self.terminal_at,
                extensions: BTreeMap::from([(
                    "codex_session".into(),
                    json!({"event": "task_complete"}),
                )]),
            });
        }

        let task_contract_source = if self.task.is_some() {
            "event_msg.user_message"
        } else {
            "adapter_fallback"
        };
        let task = self
            .task
            .unwrap_or_else(|| format!("Imported Codex session {}", meta.id));
        let header = TraceRecord::Session {
            schema: TRACE_SCHEMA.into(),
            id: meta.id.clone(),
            task,
            constraints: Vec::new(),
            success_criteria: Vec::new(),
            completeness,
            source: Some(Source {
                adapter: ADAPTER.into(),
                adapter_version: env!("CARGO_PKG_VERSION").into(),
                trust: SourceTrust::AdapterAsserted,
                session_id: Some(meta.id),
            }),
            extensions: BTreeMap::from([(
                "codex_session".into(),
                json!({
                    "protocol": "rollout-jsonl",
                    "source_digest": source_digest,
                    "task_contract_source": task_contract_source,
                    "started_at": meta.started_at,
                    "cli_version": meta.cli_version,
                    "source": meta.source,
                    "excluded_records": self.ignored,
                }),
            )]),
        };

        let mut output = Vec::new();
        for record in std::iter::once(&header).chain(&self.events) {
            serde_json::to_writer(&mut output, record).map_err(|error| {
                DriftError::new(
                    "E_INTERNAL",
                    format!("serialize imported Codex session record: {error}"),
                )
            })?;
            output
                .write_all(b"\n")
                .map_err(|error| DriftError::io("serialize imported Codex session", error))?;
        }
        parse_trace(BufReader::new(Cursor::new(&output)))?;
        Ok(output)
    }
}

fn response_extension(item_type: &str, payload: &Map<String, Value>) -> BTreeMap<String, Value> {
    let mut detail = Map::from_iter([("item_type".into(), json!(item_type))]);
    if let Some(id) = payload.get("id").and_then(Value::as_str) {
        detail.insert("item_id".into(), json!(id));
    }
    BTreeMap::from([("codex_session".into(), Value::Object(detail))])
}

fn object<'a>(value: &'a Value, label: &str, line: usize) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| {
        at_line(
            "E_CODEX_SESSION_PROTOCOL",
            line,
            format!("{label} must be an object"),
        )
    })
}

fn object_field<'a>(
    value: &'a Map<String, Value>,
    field: &str,
    line: usize,
) -> Result<&'a Map<String, Value>> {
    value.get(field).and_then(Value::as_object).ok_or_else(|| {
        at_line(
            "E_CODEX_SESSION_PROTOCOL",
            line,
            format!("{field} must be an object"),
        )
    })
}

fn string<'a>(value: &'a Map<String, Value>, field: &str, line: usize) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            at_line(
                "E_CODEX_SESSION_PROTOCOL",
                line,
                format!("{field} must be a nonempty string"),
            )
        })
}

fn optional_string(value: &Map<String, Value>, field: &str, line: usize) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(at_line(
            "E_CODEX_SESSION_PROTOCOL",
            line,
            format!("{field} must be a nonempty string when present"),
        )),
    }
}

fn at_line(code: &'static str, line: usize, message: impl Into<String>) -> DriftError {
    DriftError::new(code, format!("line {line}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    fn meta() -> &'static str {
        "{\"timestamp\":\"2026-08-12T11:54:16Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"019ff5d2-ba6d-7592-82bd-e2de3a418790\",\"session_id\":\"019ff5d2-ba6d-7592-82bd-e2de3a418790\",\"timestamp\":\"2026-08-12T11:54:16Z\",\"cwd\":\"/repo\",\"source\":\"cli\",\"cli_version\":\"0.146.0\"}}\n"
    }

    #[test]
    fn imports_visible_transcript_and_tools_without_private_records() {
        let source = format!(
            "{}{}",
            meta(),
            concat!(
                "{\"timestamp\":\"2026-08-12T11:54:17Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:18Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"sys\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"PRIVATE SYSTEM PROMPT\"}]}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:19Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Inspect the tests\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:20Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"id\":\"r1\",\"summary\":[],\"encrypted_content\":\"PRIVATE REASONING\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:21Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"I will inspect them.\",\"phase\":\"commentary\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:22Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"id\":\"i1\",\"call_id\":\"call-1\",\"name\":\"exec\",\"input\":\"await tools.exec_command({cmd: 'cargo test'})\",\"status\":\"completed\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:23Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"id\":\"i2\",\"call_id\":\"call-1\",\"output\":[{\"type\":\"input_text\",\"text\":\"all passed\"}]}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:24Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"id\":\"i3\",\"call_id\":\"call-2\",\"namespace\":\"collaboration\",\"name\":\"wait_agent\",\"arguments\":\"{\\\"timeout_ms\\\":10000}\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:25Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"id\":\"i4\",\"call_id\":\"call-2\",\"output\":\"done\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:26Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Tests pass.\",\"phase\":\"final\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:27Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            )
        );
        let converted = import(source.as_bytes()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(&converted))).unwrap();

        assert_eq!(trace.task(), "Inspect the tests");
        assert_eq!(trace.completeness(), Completeness::Complete);
        assert_eq!(trace.source().unwrap().adapter, ADAPTER);
        assert!(matches!(
            &trace.events[0],
            TraceRecord::Message { actor, text, at, .. }
                if actor == "user:codex"
                    && text == "Inspect the tests"
                    && at.as_deref() == Some("2026-08-12T11:54:19Z")
        ));
        assert!(trace.events.iter().any(|record| matches!(
            record,
            TraceRecord::ToolCall { name, input, .. }
                if name == "collaboration.wait_agent"
                    && input == &json!("{\"timeout_ms\":10000}")
        )));
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|record| matches!(record, TraceRecord::ToolCall { .. }))
                .count(),
            2
        );
        assert!(matches!(
            trace.events.last(),
            Some(TraceRecord::Outcome {
                status: OutcomeStatus::Unknown,
                ..
            })
        ));
        let text = String::from_utf8(converted).unwrap();
        assert!(!text.contains("PRIVATE SYSTEM PROMPT"));
        assert!(!text.contains("PRIVATE REASONING"));
        assert!(text.contains("response_item.reasoning"));
    }

    #[test]
    fn interrupted_or_unlinked_session_is_partial() {
        let source = format!(
            "{}{}",
            meta(),
            concat!(
                "{\"timestamp\":\"2026-08-12T11:54:17Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:18Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Wait\"}}\n",
                "{\"timestamp\":\"2026-08-12T11:54:19Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"pending\",\"name\":\"exec\",\"input\":\"sleep\"}}\n",
            )
        );
        let converted = import(source.as_bytes()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert_eq!(trace.completeness(), Completeness::Partial);
        assert_eq!(trace.warnings.len(), 1);
        assert!(
            !trace
                .events
                .iter()
                .any(|record| matches!(record, TraceRecord::Outcome { .. }))
        );
    }

    #[test]
    fn latest_turn_controls_completeness() {
        let source = format!(
            "{}{}",
            meta(),
            concat!(
                "{\"timestamp\":\"1\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"First task\"}}\n",
                "{\"timestamp\":\"2\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n",
                "{\"timestamp\":\"4\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"Follow-up\"}}\n",
            )
        );
        let converted = import(source.as_bytes()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert_eq!(trace.completeness(), Completeness::Partial);
        assert_eq!(trace.task(), "First task");
        assert!(
            !trace
                .events
                .iter()
                .any(|record| matches!(record, TraceRecord::Outcome { .. }))
        );
    }

    #[test]
    fn rejects_ambiguous_metadata_and_relevant_protocol_corruption() {
        let mismatched = "{\"timestamp\":\"1\",\"type\":\"session_meta\",\"payload\":{\"id\":\"a\",\"session_id\":\"b\"}}\n";
        assert_eq!(
            import(mismatched.as_bytes()).unwrap_err().code,
            "E_CODEX_SESSION_PROTOCOL"
        );

        let duplicate = "{\"timestamp\":\"1\",\"type\":\"session_meta\",\"type\":\"event_msg\",\"payload\":{}}\n";
        assert_eq!(
            import(duplicate.as_bytes()).unwrap_err().code,
            "E_CODEX_SESSION_JSON"
        );

        let malformed = format!(
            "{}{}",
            meta(),
            "{\"timestamp\":\"2\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":4}}\n"
        );
        assert_eq!(
            import(malformed.as_bytes()).unwrap_err().code,
            "E_CODEX_SESSION_PROTOCOL"
        );
    }

    #[test]
    fn preserves_unknown_records_only_as_exclusion_counts() {
        let source = format!(
            "{}{}",
            meta(),
            "{\"timestamp\":\"2\",\"type\":\"future_private_state\",\"payload\":{\"secret\":\"DO NOT COPY\"}}\n"
        );
        let converted = String::from_utf8(import(source.as_bytes()).unwrap()).unwrap();

        assert!(converted.contains("future_private_state"));
        assert!(!converted.contains("DO NOT COPY"));
    }
}
