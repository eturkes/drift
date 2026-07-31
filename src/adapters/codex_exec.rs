use std::{
    collections::{BTreeMap, HashMap},
    io::{BufReader, Cursor, Write},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Completeness, DriftError, OutcomeStatus, Result, Source, SourceTrust, ToolStatus, TraceRecord,
    digest::sha256_bytes,
    json::ensure_unique_keys,
    parse::{MAX_EVENTS, MAX_FILE_BYTES, MAX_LINE_BYTES, parse_trace},
};

const TRACE_SCHEMA: &str = "drift.trace/v1";
const ADAPTER: &str = "codex-exec-jsonl";

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub task: String,
    pub constraints: Vec<String>,
    pub success_criteria: Vec<String>,
}

/// Convert one Codex `exec --json` turn into strict `drift.trace/v1` JSONL.
///
/// The adapter rejects unknown protocol variants and fields. A terminal Codex turn makes the
/// trace structurally complete, but `turn.completed` maps to an unknown task outcome: it proves
/// transport completion, not task success.
pub fn import(bytes: &[u8], options: ImportOptions) -> Result<Vec<u8>> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(DriftError::new(
            "E_CODEX_IMPORT_TOO_LARGE",
            format!("Codex JSONL exceeds {MAX_FILE_BYTES} bytes"),
        ));
    }
    if options.task.trim().is_empty() {
        return Err(DriftError::new(
            "E_ARGUMENT",
            "--task requires non-whitespace text",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        DriftError::new(
            "E_CODEX_IMPORT_JSON",
            format!("Codex JSONL is not UTF-8: {error}"),
        )
    })?;
    let mut importer = Importer::new(options);
    for (index, raw_line) in text.split_terminator('\n').enumerate() {
        let line_number = index + 1;
        if line_number > MAX_EVENTS {
            return Err(at_line(
                "E_CODEX_IMPORT_TOO_MANY_EVENTS",
                line_number,
                format!("Codex stream exceeds {MAX_EVENTS} events"),
            ));
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.len() > MAX_LINE_BYTES {
            return Err(at_line(
                "E_CODEX_IMPORT_LINE_TOO_LARGE",
                line_number,
                format!("Codex event exceeds {MAX_LINE_BYTES} bytes"),
            ));
        }
        if line.trim().is_empty() {
            return Err(at_line(
                "E_CODEX_IMPORT_BLANK_LINE",
                line_number,
                "blank lines are not valid Codex JSONL events",
            ));
        }
        ensure_unique_keys(line.as_bytes()).map_err(|error| {
            at_line(
                "E_CODEX_IMPORT_JSON",
                line_number,
                format!("invalid Codex event at column {}: {error}", error.column()),
            )
        })?;
        let event: CodexEvent = serde_json::from_str(line).map_err(|error| {
            at_line(
                "E_CODEX_IMPORT_PROTOCOL",
                line_number,
                format!("unsupported Codex event: {error}"),
            )
        })?;
        importer.accept(event, line_number)?;
    }
    importer.finish(sha256_bytes(bytes))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum CodexEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted {},
    #[serde(rename = "turn.completed")]
    TurnCompleted { usage: Value },
    #[serde(rename = "turn.failed")]
    TurnFailed { error: CodexError },
    #[serde(rename = "item.started")]
    ItemStarted { item: CodexItem },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: CodexItem },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: CodexItem },
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexError {
    message: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum CodexItem {
    AgentMessage {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        text: String,
    },
    CommandExecution {
        id: String,
        command: String,
        aggregated_output: String,
        exit_code: Option<i32>,
        status: CommandStatus,
    },
    FileChange {
        id: String,
        changes: Vec<FileChange>,
        status: FileStatus,
    },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        #[serde(default)]
        arguments: Value,
        result: Option<Value>,
        error: Option<Value>,
        status: ToolCallStatus,
    },
    CollabToolCall {
        id: String,
        tool: String,
        sender_thread_id: String,
        receiver_thread_ids: Vec<String>,
        prompt: Option<String>,
        agents_states: Value,
        status: ToolCallStatus,
    },
    WebSearch {
        id: String,
        query: String,
        action: Value,
    },
    TodoList {
        id: String,
        items: Vec<TodoItem>,
    },
    Error {
        id: String,
        message: String,
    },
}

impl CodexItem {
    fn kind(&self) -> &'static str {
        match self {
            Self::AgentMessage { .. } => "agent_message",
            Self::Reasoning { .. } => "reasoning",
            Self::CommandExecution { .. } => "command_execution",
            Self::FileChange { .. } => "file_change",
            Self::McpToolCall { .. } => "mcp_tool_call",
            Self::CollabToolCall { .. } => "collab_tool_call",
            Self::WebSearch { .. } => "web_search",
            Self::TodoList { .. } => "todo_list",
            Self::Error { .. } => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CommandStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FileStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileChange {
    path: String,
    kind: FileChangeKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FileChangeKind {
    Add,
    Delete,
    Update,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TodoItem {
    text: String,
    completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolState {
    Start,
    ThreadStarted,
    TurnStarted,
    Terminal,
}

#[derive(Debug)]
struct ItemLifecycle {
    kind: &'static str,
    complete: bool,
}

#[derive(Debug)]
struct OpenCall {
    call_id: String,
    kind: &'static str,
    name: String,
    input: Value,
}

struct Importer {
    options: ImportOptions,
    state: ProtocolState,
    thread_id: Option<String>,
    events: Vec<TraceRecord>,
    items: HashMap<String, ItemLifecycle>,
    open_calls: HashMap<String, OpenCall>,
    next_event: usize,
    next_call: usize,
}

impl Importer {
    fn new(options: ImportOptions) -> Self {
        Self {
            options,
            state: ProtocolState::Start,
            thread_id: None,
            events: Vec::new(),
            items: HashMap::new(),
            open_calls: HashMap::new(),
            next_event: 1,
            next_call: 1,
        }
    }

    fn accept(&mut self, event: CodexEvent, line: usize) -> Result<()> {
        if self.state == ProtocolState::Terminal {
            return Err(at_line(
                "E_CODEX_IMPORT_PROTOCOL",
                line,
                "event follows terminal Codex turn event",
            ));
        }
        match event {
            CodexEvent::ThreadStarted { thread_id } if self.state == ProtocolState::Start => {
                if thread_id.trim().is_empty() {
                    return Err(at_line(
                        "E_CODEX_IMPORT_PROTOCOL",
                        line,
                        "thread.started has an empty thread_id",
                    ));
                }
                self.thread_id = Some(thread_id);
                self.state = ProtocolState::ThreadStarted;
            }
            CodexEvent::TurnStarted {} if self.state == ProtocolState::ThreadStarted => {
                self.state = ProtocolState::TurnStarted;
            }
            CodexEvent::ItemStarted { item } => self.accept_item(item, Phase::Started, line)?,
            CodexEvent::ItemUpdated { item } => self.accept_item(item, Phase::Updated, line)?,
            CodexEvent::ItemCompleted { item } => self.accept_item(item, Phase::Completed, line)?,
            CodexEvent::Error { message }
                if matches!(
                    self.state,
                    ProtocolState::ThreadStarted | ProtocolState::TurnStarted
                ) =>
            {
                self.push_state(
                    BTreeMap::from([
                        ("codex_error".into(), json!(message)),
                        ("scope".into(), json!("stream")),
                    ]),
                    protocol_extension("error"),
                );
            }
            CodexEvent::TurnCompleted { usage } if self.state == ProtocolState::TurnStarted => {
                self.push_outcome(
                    OutcomeStatus::Unknown,
                    "Codex turn completed; exec JSONL does not establish task success.".into(),
                    BTreeMap::from([(
                        "codex_exec".into(),
                        json!({"event": "turn.completed", "usage": usage}),
                    )]),
                );
                self.state = ProtocolState::Terminal;
            }
            CodexEvent::TurnFailed { error } if self.state == ProtocolState::TurnStarted => {
                self.push_outcome(
                    OutcomeStatus::Failure,
                    if error.message.trim().is_empty() {
                        "Codex turn failed.".into()
                    } else {
                        error.message.clone()
                    },
                    BTreeMap::from([(
                        "codex_exec".into(),
                        json!({"event": "turn.failed", "error": error.message}),
                    )]),
                );
                self.state = ProtocolState::Terminal;
            }
            other => {
                return Err(at_line(
                    "E_CODEX_IMPORT_PROTOCOL",
                    line,
                    format!(
                        "Codex event is invalid in protocol state {:?}: {other:?}",
                        self.state
                    ),
                ));
            }
        }
        Ok(())
    }

    fn accept_item(&mut self, item: CodexItem, phase: Phase, line: usize) -> Result<()> {
        let error_before_turn =
            matches!(&item, CodexItem::Error { .. }) && self.state == ProtocolState::ThreadStarted;
        if self.state != ProtocolState::TurnStarted && !error_before_turn {
            return Err(at_line(
                "E_CODEX_IMPORT_PROTOCOL",
                line,
                format!("{} appeared outside a started turn", item.kind()),
            ));
        }
        match item {
            CodexItem::AgentMessage { id, text } => {
                require_phase(phase, Phase::Completed, "agent_message", line)?;
                self.complete_item(&id, "agent_message", true, line)?;
                self.push_message(
                    "agent:codex",
                    text,
                    item_extension(phase, &id, "agent_message"),
                );
            }
            CodexItem::Reasoning { id, text } => {
                require_phase(phase, Phase::Completed, "reasoning", line)?;
                self.complete_item(&id, "reasoning", true, line)?;
                self.push_message("agent:codex", text, item_extension(phase, &id, "reasoning"));
            }
            CodexItem::Error { id, message } => {
                require_phase(phase, Phase::Completed, "error", line)?;
                self.complete_item(&id, "error", true, line)?;
                self.push_state(
                    BTreeMap::from([
                        ("codex_error".into(), json!(message)),
                        ("scope".into(), json!("item")),
                    ]),
                    item_extension(phase, &id, "error"),
                );
            }
            CodexItem::TodoList { id, items } => {
                match phase {
                    Phase::Started => self.start_item(&id, "todo_list", line)?,
                    Phase::Updated => self.update_item(&id, "todo_list", line)?,
                    Phase::Completed => self.complete_item(&id, "todo_list", false, line)?,
                }
                self.push_message(
                    "agent:codex",
                    json!({"codex_todo_list": items, "phase": phase.label()}).to_string(),
                    item_extension(phase, &id, "todo_list"),
                );
            }
            CodexItem::CommandExecution {
                id,
                command,
                aggregated_output,
                exit_code,
                status,
            } => {
                let input = json!({"command": command});
                match phase {
                    Phase::Started => {
                        require_status(status == CommandStatus::InProgress, &id, line)?;
                        self.start_tool(&id, "command_execution", "shell", input, phase, line)?;
                    }
                    Phase::Completed => {
                        require_status(status != CommandStatus::InProgress, &id, line)?;
                        let result = match status {
                            CommandStatus::Completed => ToolStatus::Ok,
                            CommandStatus::Failed | CommandStatus::Declined => ToolStatus::Error,
                            CommandStatus::InProgress => unreachable!(),
                        };
                        self.complete_tool(
                            &id,
                            "command_execution",
                            "shell",
                            input,
                            result,
                            json!({
                                "aggregated_output": aggregated_output,
                                "exit_code": exit_code,
                                "status": status,
                            }),
                            phase,
                            line,
                        )?;
                    }
                    Phase::Updated => return Err(invalid_phase("command_execution", phase, line)),
                }
            }
            CodexItem::FileChange {
                id,
                changes,
                status,
            } => {
                let input = json!({"changes": changes});
                match phase {
                    Phase::Started => {
                        require_status(status == FileStatus::InProgress, &id, line)?;
                        self.start_tool(&id, "file_change", "file_change", input, phase, line)?;
                    }
                    Phase::Completed => {
                        require_status(status != FileStatus::InProgress, &id, line)?;
                        let result = if status == FileStatus::Completed {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Error
                        };
                        self.complete_tool(
                            &id,
                            "file_change",
                            "file_change",
                            input,
                            result,
                            json!({"status": status}),
                            phase,
                            line,
                        )?;
                    }
                    Phase::Updated => return Err(invalid_phase("file_change", phase, line)),
                }
            }
            CodexItem::McpToolCall {
                id,
                server,
                tool,
                arguments,
                result,
                error,
                status,
            } => {
                let input = json!({"server": server, "tool": tool, "arguments": arguments});
                match phase {
                    Phase::Started => {
                        require_status(status == ToolCallStatus::InProgress, &id, line)?;
                        self.start_tool(&id, "mcp_tool_call", "mcp", input, phase, line)?;
                    }
                    Phase::Completed => {
                        require_status(status != ToolCallStatus::InProgress, &id, line)?;
                        let mapped = if status == ToolCallStatus::Completed {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Error
                        };
                        self.complete_tool(
                            &id,
                            "mcp_tool_call",
                            "mcp",
                            input,
                            mapped,
                            json!({"result": result, "error": error, "status": status}),
                            phase,
                            line,
                        )?;
                    }
                    Phase::Updated => return Err(invalid_phase("mcp_tool_call", phase, line)),
                }
            }
            CodexItem::CollabToolCall {
                id,
                tool,
                sender_thread_id,
                receiver_thread_ids,
                prompt,
                agents_states,
                status,
            } => {
                let input = json!({
                    "tool": tool,
                    "sender_thread_id": sender_thread_id,
                    "receiver_thread_ids": receiver_thread_ids,
                    "prompt": prompt,
                });
                match phase {
                    Phase::Started => {
                        require_status(status == ToolCallStatus::InProgress, &id, line)?;
                        self.start_tool(
                            &id,
                            "collab_tool_call",
                            "collaboration",
                            input,
                            phase,
                            line,
                        )?;
                    }
                    Phase::Completed => {
                        require_status(status != ToolCallStatus::InProgress, &id, line)?;
                        let mapped = if status == ToolCallStatus::Completed {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Error
                        };
                        self.complete_tool(
                            &id,
                            "collab_tool_call",
                            "collaboration",
                            input,
                            mapped,
                            json!({"agents_states": agents_states, "status": status}),
                            phase,
                            line,
                        )?;
                    }
                    Phase::Updated => return Err(invalid_phase("collab_tool_call", phase, line)),
                }
            }
            CodexItem::WebSearch { id, query, action } => match phase {
                Phase::Started => self.start_tool(
                    &id,
                    "web_search",
                    "web_search",
                    json!({"query": query, "action": action}),
                    phase,
                    line,
                )?,
                Phase::Completed => {
                    let query_value = Value::String(query.clone());
                    let input = self
                        .open_calls
                        .get(&id)
                        .filter(|open| open.input.get("query") == Some(&query_value))
                        .map(|open| open.input.clone())
                        .unwrap_or_else(|| json!({"query": query, "action": action.clone()}));
                    self.complete_tool(
                        &id,
                        "web_search",
                        "web_search",
                        input,
                        ToolStatus::Ok,
                        json!({"action": action}),
                        phase,
                        line,
                    )?;
                }
                Phase::Updated => return Err(invalid_phase("web_search", phase, line)),
            },
        }
        Ok(())
    }

    fn start_tool(
        &mut self,
        raw_id: &str,
        kind: &'static str,
        name: &str,
        input: Value,
        phase: Phase,
        line: usize,
    ) -> Result<()> {
        self.start_item(raw_id, kind, line)?;
        let call_id = self.call_id();
        let event_id = self.event_id();
        self.events.push(TraceRecord::ToolCall {
            id: event_id,
            actor: "agent:codex".into(),
            call_id: call_id.clone(),
            name: name.into(),
            input: input.clone(),
            at: None,
            duration_ms: None,
            extensions: item_extension(phase, raw_id, kind),
        });
        self.open_calls.insert(
            raw_id.into(),
            OpenCall {
                call_id,
                kind,
                name: name.into(),
                input,
            },
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_tool(
        &mut self,
        raw_id: &str,
        kind: &'static str,
        name: &str,
        input: Value,
        status: ToolStatus,
        output: Value,
        phase: Phase,
        line: usize,
    ) -> Result<()> {
        self.complete_item(raw_id, kind, kind == "file_change", line)?;
        let call_id = match self.open_calls.remove(raw_id) {
            Some(open) if open.kind == kind && open.name == name && open.input == input => {
                open.call_id
            }
            Some(open) => {
                return Err(at_line(
                    "E_CODEX_IMPORT_ITEM_LIFECYCLE",
                    line,
                    format!(
                        "item {raw_id:?} changed between start and completion \
                         ({} {} -> {kind} {name})",
                        open.kind, open.name,
                    ),
                ));
            }
            None => {
                let call_id = self.call_id();
                let event_id = self.event_id();
                let mut extensions = item_extension(phase, raw_id, kind);
                extensions.insert("codex_mapping".into(), json!("completion_only_call"));
                self.events.push(TraceRecord::ToolCall {
                    id: event_id,
                    actor: "agent:codex".into(),
                    call_id: call_id.clone(),
                    name: name.into(),
                    input,
                    at: None,
                    duration_ms: None,
                    extensions,
                });
                call_id
            }
        };
        let event_id = self.event_id();
        self.events.push(TraceRecord::ToolResult {
            id: event_id,
            actor: "tool:codex".into(),
            call_id,
            status,
            output,
            at: None,
            duration_ms: None,
            extensions: item_extension(phase, raw_id, kind),
        });
        Ok(())
    }

    fn start_item(&mut self, id: &str, kind: &'static str, line: usize) -> Result<()> {
        require_item_id(id, line)?;
        if let Some(existing) = self.items.get(id) {
            return Err(item_reuse(id, existing.kind, kind, line));
        }
        self.items.insert(
            id.into(),
            ItemLifecycle {
                kind,
                complete: false,
            },
        );
        Ok(())
    }

    fn update_item(&self, id: &str, kind: &'static str, line: usize) -> Result<()> {
        require_item_id(id, line)?;
        match self.items.get(id) {
            Some(existing) if existing.kind == kind && !existing.complete => Ok(()),
            Some(existing) => Err(item_reuse(id, existing.kind, kind, line)),
            None => Err(at_line(
                "E_CODEX_IMPORT_ITEM_LIFECYCLE",
                line,
                format!("item.updated for {id:?} has no matching item.started"),
            )),
        }
    }

    fn complete_item(
        &mut self,
        id: &str,
        kind: &'static str,
        allow_without_start: bool,
        line: usize,
    ) -> Result<()> {
        require_item_id(id, line)?;
        match self.items.get_mut(id) {
            Some(existing) if existing.kind == kind && !existing.complete => {
                existing.complete = true;
                Ok(())
            }
            Some(existing) => Err(item_reuse(id, existing.kind, kind, line)),
            None if allow_without_start => {
                self.items.insert(
                    id.into(),
                    ItemLifecycle {
                        kind,
                        complete: true,
                    },
                );
                Ok(())
            }
            None => Err(at_line(
                "E_CODEX_IMPORT_ITEM_LIFECYCLE",
                line,
                format!("item.completed for {id:?} has no matching item.started"),
            )),
        }
    }

    fn push_message(&mut self, actor: &str, text: String, extensions: BTreeMap<String, Value>) {
        let id = self.event_id();
        self.events.push(TraceRecord::Message {
            id,
            actor: actor.into(),
            text,
            at: None,
            duration_ms: None,
            extensions,
        });
    }

    fn push_state(&mut self, values: BTreeMap<String, Value>, extensions: BTreeMap<String, Value>) {
        let id = self.event_id();
        self.events.push(TraceRecord::State {
            id,
            actor: "environment:codex".into(),
            values,
            at: None,
            extensions,
        });
    }

    fn push_outcome(
        &mut self,
        status: OutcomeStatus,
        text: String,
        extensions: BTreeMap<String, Value>,
    ) {
        let id = self.event_id();
        self.events.push(TraceRecord::Outcome {
            id,
            actor: "environment:codex".into(),
            status,
            text,
            at: None,
            extensions,
        });
    }

    fn event_id(&mut self) -> String {
        let id = format!("e{}", self.next_event);
        self.next_event += 1;
        id
    }

    fn call_id(&mut self) -> String {
        let id = format!("c{}", self.next_call);
        self.next_call += 1;
        id
    }

    fn finish(self, source_digest: String) -> Result<Vec<u8>> {
        let thread_id = self.thread_id.ok_or_else(|| {
            DriftError::new(
                "E_CODEX_IMPORT_PROTOCOL",
                "Codex stream has no thread.started event",
            )
        })?;
        let completeness = if self.state == ProtocolState::Terminal {
            Completeness::Complete
        } else {
            Completeness::Partial
        };
        if completeness == Completeness::Complete
            && let Some((id, lifecycle)) = self.items.iter().find(|(_, item)| !item.complete)
        {
            return Err(DriftError::new(
                "E_CODEX_IMPORT_ITEM_LIFECYCLE",
                format!(
                    "terminal Codex stream left {} item {id:?} unfinished",
                    lifecycle.kind
                ),
            ));
        }
        let header = TraceRecord::Session {
            schema: TRACE_SCHEMA.into(),
            id: thread_id.clone(),
            task: self.options.task,
            constraints: self.options.constraints,
            success_criteria: self.options.success_criteria,
            completeness,
            source: Some(Source {
                adapter: ADAPTER.into(),
                adapter_version: env!("CARGO_PKG_VERSION").into(),
                trust: SourceTrust::AdapterAsserted,
                session_id: Some(thread_id),
            }),
            extensions: BTreeMap::from([(
                "codex_exec".into(),
                json!({
                    "protocol": "exec-jsonl",
                    "source_digest": source_digest,
                    "task_contract_source": "cli_arguments",
                }),
            )]),
        };
        let mut output = Vec::new();
        for record in std::iter::once(&header).chain(&self.events) {
            serde_json::to_writer(&mut output, record).map_err(|error| {
                DriftError::new(
                    "E_INTERNAL",
                    format!("serialize imported trace record: {error}"),
                )
            })?;
            output
                .write_all(b"\n")
                .map_err(|error| DriftError::io("serialize imported trace", error))?;
        }
        parse_trace(BufReader::new(Cursor::new(&output)))?;
        Ok(output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Started,
    Updated,
    Completed,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Started => "item.started",
            Self::Updated => "item.updated",
            Self::Completed => "item.completed",
        }
    }
}

fn item_extension(phase: Phase, id: &str, kind: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([(
        "codex_exec".into(),
        json!({"event": phase.label(), "item_id": id, "item_type": kind}),
    )])
}

fn protocol_extension(event: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("codex_exec".into(), json!({"event": event}))])
}

fn require_phase(actual: Phase, expected: Phase, kind: &str, line: usize) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(invalid_phase(kind, actual, line))
    }
}

fn invalid_phase(kind: &str, phase: Phase, line: usize) -> DriftError {
    at_line(
        "E_CODEX_IMPORT_ITEM_LIFECYCLE",
        line,
        format!("{kind} is invalid in {}", phase.label()),
    )
}

fn require_status(valid: bool, id: &str, line: usize) -> Result<()> {
    if valid {
        Ok(())
    } else {
        Err(at_line(
            "E_CODEX_IMPORT_ITEM_STATUS",
            line,
            format!("item {id:?} has a status incompatible with its event phase"),
        ))
    }
}

fn require_item_id(id: &str, line: usize) -> Result<()> {
    if id.trim().is_empty() {
        Err(at_line(
            "E_CODEX_IMPORT_ITEM_LIFECYCLE",
            line,
            "Codex item has an empty id",
        ))
    } else {
        Ok(())
    }
}

fn item_reuse(id: &str, existing: &str, incoming: &str, line: usize) -> DriftError {
    at_line(
        "E_CODEX_IMPORT_ITEM_LIFECYCLE",
        line,
        format!("item {id:?} already appeared as {existing}; cannot accept {incoming}"),
    )
}

fn at_line(code: &'static str, line: usize, message: impl Into<String>) -> DriftError {
    DriftError::new(code, format!("line {line}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::*;
    use crate::{OutcomeStatus, ToolStatus, parse::parse_trace};

    fn options() -> ImportOptions {
        ImportOptions {
            task: "Inspect the working directory".into(),
            constraints: vec!["Do not modify files".into()],
            success_criteria: vec!["Report pwd".into()],
        }
    }

    #[test]
    fn distributed_codex_fixture_imports() {
        let source = include_bytes!("../../examples/codex-exec.source.jsonl");
        let converted = import(source, options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();
        assert_eq!(trace.events.len(), 5);
        assert_eq!(trace.completeness(), Completeness::Complete);
    }

    #[test]
    fn imports_realistic_completed_command_without_inventing_task_success() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"agent_message\",\"text\":\"Checking.\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"item_1\",\"type\":\"command_execution\",\"command\":\"pwd\",\"aggregated_output\":\"\",\"exit_code\":null,\"status\":\"in_progress\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_1\",\"type\":\"command_execution\",\"command\":\"pwd\",\"aggregated_output\":\"/repo\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_2\",\"type\":\"agent_message\",\"text\":\"The path is /repo.\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0}}\n",
        );
        let converted = import(source.as_bytes(), options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert_eq!(trace.completeness(), Completeness::Complete);
        assert_eq!(trace.source().unwrap().adapter, ADAPTER);
        assert!(matches!(
            &trace.header,
            TraceRecord::Session { extensions, .. }
                if extensions["codex_exec"]["source_digest"] == sha256_bytes(source.as_bytes())
        ));
        assert_eq!(trace.events.len(), 5);
        assert!(matches!(
            &trace.events[1],
            TraceRecord::ToolCall { name, input, .. }
                if name == "shell" && input == &json!({"command": "pwd"})
        ));
        assert!(matches!(
            &trace.events[2],
            TraceRecord::ToolResult { status: ToolStatus::Ok, output, .. }
                if output["exit_code"] == 0
        ));
        assert!(matches!(
            trace.events.last(),
            Some(TraceRecord::Outcome {
                status: OutcomeStatus::Unknown,
                ..
            })
        ));
    }

    #[test]
    fn preserves_an_interrupted_stream_as_partial_with_linkage_warning() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-2\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"item_0\",\"type\":\"command_execution\",\"command\":\"make test\",\"aggregated_output\":\"\",\"exit_code\":null,\"status\":\"in_progress\"}}\n",
        );
        let converted = import(source.as_bytes(), options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert_eq!(trace.completeness(), Completeness::Partial);
        assert_eq!(trace.warnings.len(), 1);
        assert!(trace.warnings[0].contains("has no matching tool result"));
    }

    #[test]
    fn maps_completion_only_file_changes_to_a_linked_call_and_result() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-3\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"file_change\",\"changes\":[{\"path\":\"README.md\",\"kind\":\"update\"}],\"status\":\"completed\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n",
        );
        let converted = import(source.as_bytes(), options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert!(matches!(
            &trace.events[0],
            TraceRecord::ToolCall { name, .. } if name == "file_change"
        ));
        assert!(matches!(
            &trace.events[1],
            TraceRecord::ToolResult {
                status: ToolStatus::Ok,
                ..
            }
        ));
    }

    #[test]
    fn maps_failed_turn_to_a_failure_outcome() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-4\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"provider unavailable\"}}\n",
        );
        let converted = import(source.as_bytes(), options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert!(matches!(
            trace.events.last(),
            Some(TraceRecord::Outcome {
                status: OutcomeStatus::Failure,
                text,
                ..
            }) if text == "provider unavailable"
        ));
    }

    #[test]
    fn imports_every_supported_structured_item_family() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-all\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"reason\",\"type\":\"reasoning\",\"text\":\"Need evidence.\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"todo\",\"type\":\"todo_list\",\"items\":[{\"text\":\"Inspect\",\"completed\":false}]}}\n",
            "{\"type\":\"item.updated\",\"item\":{\"id\":\"todo\",\"type\":\"todo_list\",\"items\":[{\"text\":\"Inspect\",\"completed\":true}]}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"todo\",\"type\":\"todo_list\",\"items\":[{\"text\":\"Inspect\",\"completed\":true}]}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"mcp\",\"type\":\"mcp_tool_call\",\"server\":\"docs\",\"tool\":\"read\",\"arguments\":{\"id\":1},\"result\":null,\"error\":null,\"status\":\"in_progress\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"mcp\",\"type\":\"mcp_tool_call\",\"server\":\"docs\",\"tool\":\"read\",\"arguments\":{\"id\":1},\"result\":{\"content\":[]},\"error\":null,\"status\":\"completed\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"collab\",\"type\":\"collab_tool_call\",\"tool\":\"spawn_agent\",\"sender_thread_id\":\"a\",\"receiver_thread_ids\":[\"b\"],\"prompt\":\"Inspect\",\"agents_states\":{},\"status\":\"in_progress\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"collab\",\"type\":\"collab_tool_call\",\"tool\":\"spawn_agent\",\"sender_thread_id\":\"a\",\"receiver_thread_ids\":[\"b\"],\"prompt\":\"Inspect\",\"agents_states\":{\"b\":{\"status\":\"completed\",\"message\":\"done\"}},\"status\":\"completed\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"web\",\"type\":\"web_search\",\"query\":\"Rust\",\"action\":{\"type\":\"search\"}}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"web\",\"type\":\"web_search\",\"query\":\"Rust\",\"action\":{\"type\":\"search\"}}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"files\",\"type\":\"file_change\",\"changes\":[{\"path\":\"a\",\"kind\":\"add\"}],\"status\":\"failed\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"warning\",\"type\":\"error\",\"message\":\"non-fatal warning\"}}\n",
            "{\"type\":\"error\",\"message\":\"stream warning\"}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n",
        );
        let converted = import(source.as_bytes(), options()).unwrap();
        let trace = parse_trace(BufReader::new(Cursor::new(converted))).unwrap();

        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event, TraceRecord::ToolCall { .. }))
                .count(),
            4
        );
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event, TraceRecord::ToolResult { .. }))
                .count(),
            4
        );
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| matches!(event, TraceRecord::State { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn rejects_protocol_growth_and_duplicate_keys_instead_of_dropping_evidence() {
        let unknown = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-5\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"future_tool\"}}\n",
        );
        let error = import(unknown.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_PROTOCOL");

        let duplicate = "{\"type\":\"thread.started\",\"thread_id\":\"a\",\"thread_id\":\"b\"}\n";
        let error = import(duplicate.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_JSON");

        let added_field = "{\"type\":\"thread.started\",\"thread_id\":\"a\",\"future\":true}\n";
        let error = import(added_field.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_PROTOCOL");
    }

    #[test]
    fn rejects_changed_call_payloads_and_blank_source_records() {
        let changed = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-change\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"item_0\",\"type\":\"command_execution\",\"command\":\"pwd\",\"aggregated_output\":\"\",\"exit_code\":null,\"status\":\"in_progress\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"command_execution\",\"command\":\"whoami\",\"aggregated_output\":\"eturkes\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
        );
        let error = import(changed.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_ITEM_LIFECYCLE");

        let missing_start = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-missing\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"id\":\"item_0\",\"type\":\"command_execution\",\"command\":\"pwd\",\"aggregated_output\":\"/repo\\n\",\"exit_code\":0,\"status\":\"completed\"}}\n",
        );
        let error = import(missing_start.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_ITEM_LIFECYCLE");

        let blank = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-blank\"}\n",
            "\n",
        );
        let error = import(blank.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_BLANK_LINE");
    }

    #[test]
    fn rejects_a_structurally_complete_stream_with_an_unfinished_tool() {
        let source = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-6\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"id\":\"item_0\",\"type\":\"command_execution\",\"command\":\"sleep 10\",\"aggregated_output\":\"\",\"exit_code\":null,\"status\":\"in_progress\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n",
        );
        let error = import(source.as_bytes(), options()).unwrap_err();
        assert_eq!(error.code, "E_CODEX_IMPORT_ITEM_LIFECYCLE");
    }
}
