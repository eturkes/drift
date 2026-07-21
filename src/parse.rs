use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Cursor, Write},
};

use sha2::{Digest, Sha256};

use crate::{
    error::{DriftError, Result},
    json::ensure_unique_keys,
    model::{Completeness, Trace, TraceRecord},
};

pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EVENTS: usize = 50_000;

const TRACE_SCHEMA: &str = "drift.trace/v1";

#[derive(Clone, Copy)]
struct Limits {
    file_bytes: usize,
    line_bytes: usize,
    events: usize,
}

const DEFAULT_LIMITS: Limits = Limits {
    file_bytes: MAX_FILE_BYTES,
    line_bytes: MAX_LINE_BYTES,
    events: MAX_EVENTS,
};

#[derive(Clone)]
struct CallSite {
    call_id: String,
    line: usize,
}

#[derive(Clone, Copy)]
enum LinkIssueKind {
    MissingResult,
    OrphanResult,
}

struct LinkIssue {
    kind: LinkIssueKind,
    call_id: String,
    line: usize,
}

#[derive(Default)]
struct ParserState {
    header: Option<TraceRecord>,
    events: Vec<TraceRecord>,
    ids: HashMap<String, usize>,
    calls: Vec<CallSite>,
    call_ids: HashMap<String, usize>,
    results: Vec<CallSite>,
    result_ids: HashMap<String, usize>,
    outcome_line: Option<usize>,
}

/// Parse and validate one `drift.trace/v1` JSONL stream.
pub fn parse_trace<R: BufRead>(reader: R) -> Result<Trace> {
    parse_trace_with_limits(reader, DEFAULT_LIMITS)
}

/// Reapply parser invariants to a programmatically constructed trace.
pub fn validate_normalized_trace(trace: &Trace) -> Result<()> {
    if !is_sha256(&trace.input_digest) {
        return Err(DriftError::new(
            "E_TRACE_DIGEST",
            "trace input_digest must be a lowercase sha256 digest",
        ));
    }
    let mut jsonl = Vec::new();
    for record in std::iter::once(&trace.header).chain(&trace.events) {
        serde_json::to_writer(&mut jsonl, record).map_err(|error| {
            DriftError::new(
                "E_TRACE_JSON",
                format!("serialize normalized trace record: {error}"),
            )
        })?;
        jsonl
            .write_all(b"\n")
            .map_err(|error| DriftError::io("serialize normalized trace", error))?;
    }
    let parsed = parse_trace(BufReader::new(Cursor::new(jsonl)))?;
    if parsed.header != trace.header
        || parsed.events != trace.events
        || parsed.warnings != trace.warnings
    {
        return Err(DriftError::new(
            "E_TRACE_NORMALIZED",
            "normalized trace or parser warnings changed during revalidation",
        ));
    }
    Ok(())
}

fn parse_trace_with_limits<R: BufRead>(mut reader: R, limits: Limits) -> Result<Trace> {
    let mut state = ParserState::default();
    let mut digest = Sha256::new();
    let mut file_bytes = 0usize;
    let mut line_number = 1usize;
    let mut line = Vec::new();

    loop {
        let (consumed, ended_line, reached_eof) = {
            let available = reader.fill_buf().map_err(|error| {
                at_line("E_TRACE_IO", line_number, format!("read input: {error}"))
            })?;

            if available.is_empty() {
                (0, false, true)
            } else {
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |position| position + 1);
                file_bytes = file_bytes.checked_add(consumed).ok_or_else(|| {
                    at_line(
                        "E_TRACE_FILE_TOO_LARGE",
                        line_number,
                        format!("input exceeds {limit} bytes", limit = limits.file_bytes),
                    )
                })?;
                if file_bytes > limits.file_bytes {
                    return Err(at_line(
                        "E_TRACE_FILE_TOO_LARGE",
                        line_number,
                        format!("input exceeds {limit} bytes", limit = limits.file_bytes),
                    ));
                }

                let content_bytes = if newline.is_some() {
                    consumed - 1
                } else {
                    consumed
                };
                let next_line_bytes = line.len().checked_add(content_bytes).ok_or_else(|| {
                    at_line(
                        "E_TRACE_LINE_TOO_LARGE",
                        line_number,
                        format!("line exceeds {limit} bytes", limit = limits.line_bytes),
                    )
                })?;
                if next_line_bytes > limits.line_bytes {
                    return Err(at_line(
                        "E_TRACE_LINE_TOO_LARGE",
                        line_number,
                        format!("line exceeds {limit} bytes", limit = limits.line_bytes),
                    ));
                }

                digest.update(&available[..consumed]);
                line.extend_from_slice(&available[..content_bytes]);
                (consumed, newline.is_some(), false)
            }
        };

        if reached_eof {
            if !line.is_empty() {
                parse_line(&line, line_number, limits, &mut state)?;
            }
            break;
        }

        reader.consume(consumed);
        if ended_line {
            parse_line(&line, line_number, limits, &mut state)?;
            line.clear();
            line_number += 1;
        }
    }

    let completeness = match state.header.as_ref().ok_or_else(|| {
        at_line(
            "E_TRACE_EMPTY",
            1,
            "expected a session record, found end of input",
        )
    })? {
        TraceRecord::Session { completeness, .. } => *completeness,
        _ => unreachable!("the first-record check only stores a session header"),
    };
    let issues = link_issues(&state);
    let warnings = match completeness {
        Completeness::Complete => {
            if let Some(issue) = issues.first() {
                return Err(link_error(issue));
            }
            Vec::new()
        }
        Completeness::Partial | Completeness::Unknown => issues.iter().map(link_warning).collect(),
    };
    let header = state
        .header
        .expect("the completeness check established a session header");

    Ok(Trace {
        header,
        events: state.events,
        input_digest: format!("sha256:{}", hex_lower(&digest.finalize())),
        warnings,
    })
}

fn parse_line(bytes: &[u8], line: usize, limits: Limits, state: &mut ParserState) -> Result<()> {
    if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Err(at_line(
            "E_TRACE_BLANK_LINE",
            line,
            "blank lines are not valid JSONL records",
        ));
    }

    ensure_unique_keys(bytes).map_err(|error| {
        at_line(
            "E_TRACE_JSON",
            line,
            format!(
                "invalid trace record at column {column}: {error}",
                column = error.column()
            ),
        )
    })?;
    let record: TraceRecord = serde_json::from_slice(bytes).map_err(|error| {
        at_line(
            "E_TRACE_JSON",
            line,
            format!(
                "invalid trace record at column {column}: {error}",
                column = error.column()
            ),
        )
    })?;
    state.accept(record, line, limits)
}

impl ParserState {
    fn accept(&mut self, record: TraceRecord, line: usize, limits: Limits) -> Result<()> {
        if self.header.is_none() {
            let TraceRecord::Session {
                schema,
                id,
                task,
                constraints,
                success_criteria,
                source,
                ..
            } = &record
            else {
                return Err(at_line(
                    "E_TRACE_SESSION_FIRST",
                    line,
                    format!("first record must be session, found {}", record.kind_name()),
                ));
            };

            require_identifier(id, "session id", line, "E_TRACE_ID")?;
            require_nonempty(task, "session task", line, "E_TRACE_EMPTY_TASK")?;
            if constraints.len() > 256 || success_criteria.len() > 256 {
                return Err(at_line(
                    "E_TRACE_TASK_CONTRACT",
                    line,
                    "constraints and success_criteria are limited to 256 items each",
                ));
            }
            if let Some(source) = source {
                require_identifier(&source.adapter, "source adapter", line, "E_TRACE_ADAPTER")?;
                require_identifier(
                    &source.adapter_version,
                    "source adapter version",
                    line,
                    "E_TRACE_ADAPTER_VERSION",
                )?;
                if let Some(session_id) = &source.session_id {
                    require_identifier(
                        session_id,
                        "source session id",
                        line,
                        "E_TRACE_SOURCE_SESSION_ID",
                    )?;
                }
            }
            if schema != TRACE_SCHEMA {
                return Err(at_line(
                    "E_TRACE_SCHEMA",
                    line,
                    format!(
                        "session schema must be {expected}, found {actual}",
                        expected = quoted(TRACE_SCHEMA),
                        actual = quoted(schema)
                    ),
                ));
            }

            self.ids.insert(id.clone(), line);
            self.header = Some(record);
            return Ok(());
        }

        if let Some(outcome_line) = self.outcome_line {
            if matches!(record, TraceRecord::Outcome { .. }) {
                return Err(at_line(
                    "E_TRACE_OUTCOME_SOLE",
                    line,
                    format!("duplicate outcome; first seen on line {outcome_line}"),
                ));
            }
            return Err(at_line(
                "E_TRACE_OUTCOME_LAST",
                line,
                format!("record follows outcome on line {outcome_line}; outcome must be last"),
            ));
        }
        if matches!(record, TraceRecord::Session { .. }) {
            return Err(at_line(
                "E_TRACE_SESSION_SOLE",
                line,
                "session record may appear only once, on line 1",
            ));
        }
        if self.events.len() >= limits.events {
            return Err(at_line(
                "E_TRACE_TOO_MANY_EVENTS",
                line,
                format!("trace exceeds {limit} events", limit = limits.events),
            ));
        }

        let id = record.id();
        require_identifier(id, "event id", line, "E_TRACE_ID")?;
        if let Some(first_line) = self.ids.get(id) {
            return Err(at_line(
                "E_TRACE_DUPLICATE_ID",
                line,
                format!(
                    "duplicate record id {id}; first seen on line {first_line}",
                    id = quoted(id)
                ),
            ));
        }

        let actor = actor(&record).expect("non-session records always have an actor");
        require_nonempty(actor, "event actor", line, "E_TRACE_EMPTY_ACTOR")?;
        if !valid_actor(actor) {
            return Err(at_line(
                "E_TRACE_ACTOR",
                line,
                "actor must be agent, user, tool, system, environment, or role:identity",
            ));
        }
        if !actor_matches_kind(actor, &record) {
            return Err(at_line(
                "E_TRACE_ACTOR_KIND",
                line,
                format!(
                    "actor role {:?} is not valid for {}",
                    actor_role(actor),
                    record.kind_name()
                ),
            ));
        }
        if let Some(at) = at(&record) {
            require_nonempty(at, "event timestamp", line, "E_TRACE_EMPTY_TIMESTAMP")?;
        }

        match &record {
            TraceRecord::ToolCall { call_id, name, .. } => {
                require_identifier(call_id, "tool call id", line, "E_TRACE_CALL_ID")?;
                require_identifier(name, "tool name", line, "E_TRACE_TOOL_NAME")?;
                if let Some(first_line) = self.call_ids.get(call_id) {
                    return Err(at_line(
                        "E_TRACE_DUPLICATE_CALL_ID",
                        line,
                        format!(
                            "duplicate tool call id {call_id}; first seen on line {first_line}",
                            call_id = quoted(call_id)
                        ),
                    ));
                }
                self.call_ids.insert(call_id.clone(), line);
                self.calls.push(CallSite {
                    call_id: call_id.clone(),
                    line,
                });
            }
            TraceRecord::ToolResult { call_id, .. } => {
                require_identifier(call_id, "tool result call id", line, "E_TRACE_CALL_ID")?;
                if let Some(first_line) = self.result_ids.get(call_id) {
                    return Err(at_line(
                        "E_TRACE_DUPLICATE_TOOL_RESULT",
                        line,
                        format!(
                            "duplicate tool result for call id {call_id}; first seen on line {first_line}",
                            call_id = quoted(call_id)
                        ),
                    ));
                }
                self.result_ids.insert(call_id.clone(), line);
                self.results.push(CallSite {
                    call_id: call_id.clone(),
                    line,
                });
            }
            TraceRecord::Outcome { .. } => self.outcome_line = Some(line),
            TraceRecord::Message { .. }
            | TraceRecord::State { .. }
            | TraceRecord::TaskUpdate { .. } => {}
            TraceRecord::Session { .. } => unreachable!("second sessions are rejected above"),
        }

        self.ids.insert(id.to_owned(), line);
        self.events.push(record);
        Ok(())
    }
}

fn actor(record: &TraceRecord) -> Option<&str> {
    match record {
        TraceRecord::Session { .. } => None,
        TraceRecord::Message { actor, .. }
        | TraceRecord::ToolCall { actor, .. }
        | TraceRecord::ToolResult { actor, .. }
        | TraceRecord::State { actor, .. }
        | TraceRecord::TaskUpdate { actor, .. }
        | TraceRecord::Outcome { actor, .. } => Some(actor),
    }
}

fn at(record: &TraceRecord) -> Option<&str> {
    match record {
        TraceRecord::Message { at, .. }
        | TraceRecord::ToolCall { at, .. }
        | TraceRecord::ToolResult { at, .. }
        | TraceRecord::State { at, .. }
        | TraceRecord::TaskUpdate { at, .. }
        | TraceRecord::Outcome { at, .. } => at.as_deref(),
        TraceRecord::Session { .. } => None,
    }
}

fn require_nonempty(value: &str, field: &str, line: usize, code: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        Err(at_line(code, line, format!("{field} must be nonempty")))
    } else {
        Ok(())
    }
}

fn require_identifier(value: &str, field: &str, line: usize, code: &'static str) -> Result<()> {
    let length = value.chars().count();
    if value.trim().is_empty() {
        return Err(at_line(code, line, format!("{field} must be nonempty")));
    }
    if length > 256 {
        return Err(at_line(
            code,
            line,
            format!("{field} is {length} characters; maximum is 256"),
        ));
    }
    if value.chars().any(disallowed_identifier_character) {
        return Err(at_line(
            code,
            line,
            format!("{field} must not contain control or byte-order-mark characters"),
        ));
    }
    Ok(())
}

fn valid_actor(actor: &str) -> bool {
    let (role, identity) = actor
        .split_once(':')
        .map_or((actor, None), |(role, identity)| (role, Some(identity)));
    actor.chars().count() <= 256
        && matches!(role, "agent" | "user" | "tool" | "system" | "environment")
        && identity.is_none_or(|identity| {
            !identity.trim().is_empty() && !identity.chars().any(disallowed_identifier_character)
        })
}

fn disallowed_identifier_character(character: char) -> bool {
    character.is_control() || character == '\u{feff}'
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn actor_role(actor: &str) -> &str {
    actor.split_once(':').map_or(actor, |(role, _)| role)
}

fn actor_matches_kind(actor: &str, record: &TraceRecord) -> bool {
    let role = actor_role(actor);
    match record {
        TraceRecord::Session { .. } => false,
        TraceRecord::Message { .. } => {
            matches!(role, "agent" | "user" | "system" | "environment")
        }
        TraceRecord::ToolCall { .. } => matches!(role, "agent" | "system"),
        TraceRecord::ToolResult { .. } | TraceRecord::State { .. } => {
            matches!(role, "tool" | "system" | "environment")
        }
        TraceRecord::TaskUpdate { .. } => matches!(role, "user" | "system"),
        TraceRecord::Outcome { .. } => {
            matches!(role, "tool" | "system" | "environment" | "user")
        }
    }
}

fn link_issues(state: &ParserState) -> Vec<LinkIssue> {
    let mut sites = state
        .calls
        .iter()
        .map(|site| (site.line, true, site))
        .chain(state.results.iter().map(|site| (site.line, false, site)))
        .collect::<Vec<_>>();
    sites.sort_by_key(|(line, _, _)| *line);

    let mut pending = HashMap::new();
    let mut issues = Vec::new();
    for (_, is_call, site) in sites {
        if is_call {
            pending.insert(site.call_id.as_str(), site);
        } else if pending.remove(site.call_id.as_str()).is_none() {
            issues.push(LinkIssue {
                kind: LinkIssueKind::OrphanResult,
                call_id: site.call_id.clone(),
                line: site.line,
            });
        }
    }
    for site in pending.values() {
        issues.push(LinkIssue {
            kind: LinkIssueKind::MissingResult,
            call_id: site.call_id.clone(),
            line: site.line,
        });
    }
    issues.sort_by_key(|issue| issue.line);
    issues
}

fn link_error(issue: &LinkIssue) -> DriftError {
    match issue.kind {
        LinkIssueKind::MissingResult => at_line(
            "E_TRACE_MISSING_TOOL_RESULT",
            issue.line,
            format!(
                "tool call {call_id} has no matching tool result in complete trace",
                call_id = quoted(&issue.call_id)
            ),
        ),
        LinkIssueKind::OrphanResult => at_line(
            "E_TRACE_ORPHAN_TOOL_RESULT",
            issue.line,
            format!(
                "tool result {call_id} has no matching tool call in complete trace",
                call_id = quoted(&issue.call_id)
            ),
        ),
    }
}

fn link_warning(issue: &LinkIssue) -> String {
    match issue.kind {
        LinkIssueKind::MissingResult => format!(
            "line {line}: tool call {call_id} has no matching tool result",
            line = issue.line,
            call_id = quoted(&issue.call_id)
        ),
        LinkIssueKind::OrphanResult => format!(
            "line {line}: tool result {call_id} has no matching tool call",
            line = issue.line,
            call_id = quoted(&issue.call_id)
        ),
    }
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn at_line(code: &'static str, line: usize, message: impl Into<String>) -> DriftError {
    DriftError::new(code, format!("line {line}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use sha2::{Digest, Sha256};

    use super::*;

    const SESSION_COMPLETE: &str = r#"{"kind":"session","schema":"drift.trace/v1","id":"s1","task":"check it","completeness":"complete"}"#;
    const SESSION_PARTIAL: &str = r#"{"kind":"session","schema":"drift.trace/v1","id":"s1","task":"check it","completeness":"partial"}"#;

    fn parse(raw: &str) -> Result<Trace> {
        parse_trace(BufReader::with_capacity(7, Cursor::new(raw.as_bytes())))
    }

    fn error(raw: &str) -> DriftError {
        parse(raw).expect_err("trace should be rejected")
    }

    #[test]
    fn parses_chunked_jsonl_and_hashes_exact_raw_input() {
        let raw = format!(
            "{SESSION_COMPLETE}\r\n{}\n{}\n{}",
            r#"{"kind":"tool_call","id":"e1","actor":"agent","call_id":"c1","name":"pwd","input":{}}"#,
            r#"{"kind":"tool_result","id":"e2","actor":"tool","call_id":"c1","status":"ok","output":"/repo"}"#,
            r#"{"kind":"outcome","id":"e3","actor":"system","status":"success","text":"done"}"#
        );

        let trace = parse(&raw).expect("valid trace");

        assert_eq!(trace.id(), "s1");
        assert_eq!(trace.events.len(), 3);
        assert!(trace.warnings.is_empty());
        assert_eq!(
            trace.input_digest,
            format!("sha256:{}", hex_lower(&Sha256::digest(raw.as_bytes())))
        );
    }

    #[test]
    fn rejects_whitespace_only_line_with_physical_line_number() {
        let failure = error(&format!("{SESSION_COMPLETE}\n \t\r\n"));

        assert_eq!(failure.code, "E_TRACE_BLANK_LINE");
        assert_eq!(
            failure.message,
            "line 2: blank lines are not valid JSONL records"
        );
    }

    #[test]
    fn requires_supported_first_and_sole_session() {
        let failure = error(r#"{"kind":"message","id":"e1","actor":"agent","text":"hello"}"#);
        assert_eq!(failure.code, "E_TRACE_SESSION_FIRST");
        assert!(failure.message.starts_with("line 1:"));

        let bad_schema = r#"{"kind":"session","schema":"drift.trace/v2","id":"s1","task":"x"}"#;
        let failure = error(bad_schema);
        assert_eq!(failure.code, "E_TRACE_SCHEMA");
        assert!(failure.message.starts_with("line 1:"));

        let failure = error(&format!("{SESSION_COMPLETE}\n{SESSION_COMPLETE}"));
        assert_eq!(failure.code, "E_TRACE_SESSION_SOLE");
        assert!(failure.message.starts_with("line 2:"));
    }

    #[test]
    fn rejects_blank_required_strings_and_duplicate_ids() {
        let blank_actor = format!(
            "{SESSION_COMPLETE}\n{}",
            r#"{"kind":"message","id":"e1","actor":"  ","text":"hello"}"#
        );
        assert_eq!(error(&blank_actor).code, "E_TRACE_EMPTY_ACTOR");

        let blank_name = format!(
            "{SESSION_COMPLETE}\n{}",
            r#"{"kind":"tool_call","id":"e1","actor":"agent","call_id":"c1","name":"","input":null}"#
        );
        assert_eq!(error(&blank_name).code, "E_TRACE_TOOL_NAME");

        let byte_order_mark_id =
            r#"{"kind":"session","schema":"drift.trace/v1","id":"\uFEFF","task":"x"}"#;
        assert_eq!(error(byte_order_mark_id).code, "E_TRACE_ID");

        let byte_order_mark_actor = format!(
            "{SESSION_COMPLETE}\n{}",
            r#"{"kind":"message","id":"e1","actor":"agent:\uFEFF","text":"hello"}"#
        );
        assert_eq!(error(&byte_order_mark_actor).code, "E_TRACE_ACTOR");

        let duplicate = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"message","id":"e1","actor":"agent","text":"a"}"#,
            r#"{"kind":"state","id":"e1","actor":"tool","values":{}}"#
        );
        let failure = error(&duplicate);
        assert_eq!(failure.code, "E_TRACE_DUPLICATE_ID");
        assert_eq!(
            failure.message,
            "line 3: duplicate record id \"e1\"; first seen on line 2"
        );
    }

    #[test]
    fn rejects_duplicate_call_ids_and_results() {
        let duplicate_call = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"tool_call","id":"e1","actor":"agent","call_id":"c1","name":"a","input":null}"#,
            r#"{"kind":"tool_call","id":"e2","actor":"agent","call_id":"c1","name":"b","input":null}"#
        );
        assert_eq!(error(&duplicate_call).code, "E_TRACE_DUPLICATE_CALL_ID");

        let duplicate_result = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"tool_result","id":"e1","actor":"tool","call_id":"c1","status":"ok","output":null}"#,
            r#"{"kind":"tool_result","id":"e2","actor":"tool","call_id":"c1","status":"ok","output":null}"#
        );
        assert_eq!(
            error(&duplicate_result).code,
            "E_TRACE_DUPLICATE_TOOL_RESULT"
        );
    }

    #[test]
    fn enforces_kind_specific_actor_roles_and_allows_identity_suffixes() {
        let spoofed = format!(
            "{SESSION_PARTIAL}\n{}",
            r#"{"kind":"state","id":"e1","actor":"agent:worker","values":{"cwd":"/repo"}}"#
        );
        assert_eq!(error(&spoofed).code, "E_TRACE_ACTOR_KIND");

        let valid = format!(
            "{SESSION_PARTIAL}\n{}",
            r#"{"kind":"state","id":"e1","actor":"environment:harness","values":{"cwd":"/repo"}}"#
        );
        assert!(parse(&valid).is_ok());
    }

    #[test]
    fn complete_trace_rejects_missing_and_orphan_results() {
        let missing = format!(
            "{SESSION_COMPLETE}\n{}",
            r#"{"kind":"tool_call","id":"e1","actor":"agent","call_id":"c1","name":"pwd","input":null}"#
        );
        let failure = error(&missing);
        assert_eq!(failure.code, "E_TRACE_MISSING_TOOL_RESULT");
        assert!(failure.message.starts_with("line 2:"));

        let orphan = format!(
            "{SESSION_COMPLETE}\n{}",
            r#"{"kind":"tool_result","id":"e1","actor":"tool","call_id":"c1","status":"ok","output":null}"#
        );
        let failure = error(&orphan);
        assert_eq!(failure.code, "E_TRACE_ORPHAN_TOOL_RESULT");
        assert!(failure.message.starts_with("line 2:"));
    }

    #[test]
    fn incomplete_trace_warns_for_all_unmatched_links_in_line_order() {
        let raw = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"tool_result","id":"e1","actor":"tool","call_id":"orphan","status":"ok","output":null}"#,
            r#"{"kind":"tool_call","id":"e2","actor":"agent","call_id":"missing","name":"pwd","input":null}"#
        );

        let trace = parse(&raw).expect("partial linkage is allowed");

        assert_eq!(
            trace.warnings,
            [
                "line 2: tool result \"orphan\" has no matching tool call",
                "line 3: tool call \"missing\" has no matching tool result"
            ]
        );
    }

    #[test]
    fn result_must_follow_the_call_it_answers() {
        let raw = format!(
            "{SESSION_COMPLETE}\n{}\n{}",
            r#"{"kind":"tool_result","id":"e1","actor":"tool","call_id":"c1","status":"ok","output":null}"#,
            r#"{"kind":"tool_call","id":"e2","actor":"agent","call_id":"c1","name":"pwd","input":null}"#
        );

        let failure = error(&raw);

        assert_eq!(failure.code, "E_TRACE_ORPHAN_TOOL_RESULT");
        assert!(failure.message.starts_with("line 2:"));
    }

    #[test]
    fn outcome_must_be_unique_and_last() {
        let duplicate = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"outcome","id":"e1","actor":"system","status":"success","text":"done"}"#,
            r#"{"kind":"outcome","id":"e2","actor":"system","status":"success","text":"again"}"#
        );
        let failure = error(&duplicate);
        assert_eq!(failure.code, "E_TRACE_OUTCOME_SOLE");
        assert_eq!(
            failure.message,
            "line 3: duplicate outcome; first seen on line 2"
        );

        let raw = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"outcome","id":"e1","actor":"system","status":"success","text":"done"}"#,
            r#"{"kind":"message","id":"e2","actor":"agent","text":"more"}"#
        );

        let failure = error(&raw);
        assert_eq!(failure.code, "E_TRACE_OUTCOME_LAST");
        assert_eq!(
            failure.message,
            "line 3: record follows outcome on line 2; outcome must be last"
        );
    }

    #[test]
    fn enforces_each_resource_limit_incrementally() {
        let line_failure = parse_trace_with_limits(
            Cursor::new(SESSION_COMPLETE.as_bytes()),
            Limits {
                file_bytes: usize::MAX,
                line_bytes: SESSION_COMPLETE.len() - 1,
                events: usize::MAX,
            },
        )
        .expect_err("oversized line");
        assert_eq!(line_failure.code, "E_TRACE_LINE_TOO_LARGE");
        assert!(line_failure.message.starts_with("line 1:"));

        let file_failure = parse_trace_with_limits(
            Cursor::new(SESSION_COMPLETE.as_bytes()),
            Limits {
                file_bytes: SESSION_COMPLETE.len() - 1,
                line_bytes: usize::MAX,
                events: usize::MAX,
            },
        )
        .expect_err("oversized file");
        assert_eq!(file_failure.code, "E_TRACE_FILE_TOO_LARGE");
        assert!(file_failure.message.starts_with("line 1:"));

        let raw = format!(
            "{SESSION_PARTIAL}\n{}\n{}",
            r#"{"kind":"message","id":"e1","actor":"agent","text":"one"}"#,
            r#"{"kind":"message","id":"e2","actor":"agent","text":"two"}"#
        );
        let event_failure = parse_trace_with_limits(
            Cursor::new(raw.as_bytes()),
            Limits {
                file_bytes: usize::MAX,
                line_bytes: usize::MAX,
                events: 1,
            },
        )
        .expect_err("too many events");
        assert_eq!(event_failure.code, "E_TRACE_TOO_MANY_EVENTS");
        assert!(event_failure.message.starts_with("line 3:"));
    }
}
