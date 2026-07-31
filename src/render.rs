use std::{collections::HashMap, fmt::Write};

use crate::model::{
    Completeness, EvidenceBasis, Incident, IncidentBurden, IncidentStatus, Recommendation, Report,
    Severity, TraceRecord,
};

pub fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let event_index: HashMap<_, _> = report
        .trace
        .events
        .iter()
        .enumerate()
        .map(|(position, event)| (event.id(), (position, event)))
        .collect();
    let decision = decision_label(report.rerun.recommendation);
    writeln!(
        out,
        "{decision} (conditional on trace fidelity) - {}",
        inline(&report.rerun.reason, 600)
    )
    .unwrap();

    let (id, task, constraints, criteria, completeness, source) = session(&report.trace.session);
    writeln!(out).unwrap();
    writeln!(out, "Session: {}", inline(id, 256)).unwrap();
    writeln!(out, "Task: {}", inline(task, 1_000)).unwrap();
    writeln!(
        out,
        "Evidence ceiling: outcome={} | trajectory={} | judge-confidence={} | declared-completeness={} | source-trust={}",
        enum_label(report.summary.task_outcome),
        enum_label(report.summary.trajectory),
        enum_label(report.summary.confidence),
        enum_label(completeness),
        source.map_or("unverified".to_owned(), |source| enum_label(source.trust)),
    )
    .unwrap();
    writeln!(out, "Overview: {}", inline(&report.overview, 900)).unwrap();

    if !report.gaps.is_empty()
        || !report.trace.parser_warnings.is_empty()
        || !report.validation_warnings.is_empty()
    {
        writeln!(out).unwrap();
        writeln!(out, "Decision-limiting gaps").unwrap();
        for gap in &report.gaps {
            writeln!(out, "  - {}", inline(gap, 500)).unwrap();
        }
        for warning in &report.trace.parser_warnings {
            writeln!(out, "  - Trace: {}", inline(warning, 500)).unwrap();
        }
        for warning in &report.validation_warnings {
            writeln!(out, "  - Analysis: {}", inline(warning, 500)).unwrap();
        }
    }

    writeln!(out).unwrap();
    writeln!(
        out,
        "Outcome: {}",
        inline(&report.summary.outcome_reason, 1_000)
    )
    .unwrap();
    for event_id in &report.summary.outcome_evidence {
        render_event_ref(&mut out, "Outcome evidence", event_id, &event_index);
    }
    writeln!(
        out,
        "Trajectory: {}",
        inline(&report.summary.trajectory_reason, 1_000)
    )
    .unwrap();

    if !constraints.is_empty() || !criteria.is_empty() || has_task_updates(report) {
        writeln!(out).unwrap();
        writeln!(out, "Task contract").unwrap();
        for constraint in constraints {
            writeln!(out, "  - Constraint: {}", inline(constraint, 600)).unwrap();
        }
        for criterion in criteria {
            writeln!(out, "  - Success: {}", inline(criterion, 600)).unwrap();
        }
        for event in &report.trace.events {
            if let TraceRecord::TaskUpdate {
                id,
                actor,
                mode,
                text,
                ..
            } = event
            {
                writeln!(
                    out,
                    "  - Update {} [{} by {}]: {}",
                    inline(id, 120),
                    enum_label(*mode),
                    inline(actor, 120),
                    inline(text, 600)
                )
                .unwrap();
            }
        }
    }

    if !report.rerun.inspect.is_empty() || !report.rerun.prerequisites.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Decision checks").unwrap();
        for item in &report.rerun.inspect {
            writeln!(out, "  - Inspect: {}", inline(item, 500)).unwrap();
        }
        for item in &report.rerun.prerequisites {
            writeln!(out, "  - Before rerun: {}", inline(item, 500)).unwrap();
        }
    }
    if !report.rerun.follow_up.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Optional follow-up").unwrap();
        for item in &report.rerun.follow_up {
            writeln!(out, "  - {}", inline(item, 500)).unwrap();
        }
    }

    let (agent_action_count, tool_result_count) = coverage_totals(&report.trace.events);
    writeln!(out).unwrap();
    writeln!(
        out,
        "Judge-attributed incident coverage: {}/{} session events | {}/{} agent tool calls | {}/{} tool results | {}",
        report.burden.attributed_event_count,
        report.trace.events.len(),
        report.burden.attributed_agent_action_count,
        agent_action_count,
        report.burden.attributed_tool_result_count,
        tool_result_count,
        duration_label(
            report.burden.known_duration_ms,
            report.burden.duration_event_count
        ),
    )
    .unwrap();

    writeln!(out).unwrap();
    writeln!(out, "Incidents ({})", report.incidents.len()).unwrap();
    if report.incidents.is_empty() {
        writeln!(
            out,
            "  No incidents supported by the available {} trace.",
            enum_label(completeness)
        )
        .unwrap();
    } else {
        let mut incidents: Vec<&Incident> = report.incidents.iter().collect();
        incidents.sort_by_key(|incident| incident_order(incident));
        for (index, incident) in incidents.into_iter().enumerate() {
            let burden = report
                .burden
                .episodes
                .iter()
                .find(|episode| episode.incident_id == incident.id);
            render_incident(&mut out, index + 1, incident, burden, &event_index);
        }
    }

    writeln!(out).unwrap();
    writeln!(
        out,
        "Provenance: Drift {} | {} | {} | model={} | rubric={} {} | attempts={} | thread={} | generated-at-unix-ms={}",
        inline(&report.analysis.drift_version, 40),
        inline(&report.analysis.judge, 80),
        inline(&report.analysis.codex_cli, 120),
        inline(&report.analysis.model, 120),
        inline(&report.analysis.rubric, 80),
        inline(&report.analysis.rubric_digest, 80),
        report.analysis.attempts,
        inline(&report.analysis.thread_id, 120),
        report.analysis.generated_at_unix_ms,
    )
    .unwrap();
    writeln!(
        out,
        "Trace digests: input={} | normalized={}",
        inline(&report.trace.input_digest, 80),
        inline(&report.trace.normalized_digest, 80)
    )
    .unwrap();
    out
}

fn coverage_totals(events: &[TraceRecord]) -> (usize, usize) {
    let agent_actions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                TraceRecord::ToolCall { actor, .. }
                    if actor.split_once(':').map_or(actor.as_str(), |(role, _)| role) == "agent"
            )
        })
        .count();
    let tool_results = events
        .iter()
        .filter(|event| matches!(event, TraceRecord::ToolResult { .. }))
        .count();
    (agent_actions, tool_results)
}

fn render_incident(
    out: &mut String,
    index: usize,
    incident: &Incident,
    burden: Option<&IncidentBurden>,
    event_index: &HashMap<&str, (usize, &TraceRecord)>,
) {
    writeln!(
        out,
        "  {index}. [{} | {} | {}] {}",
        enum_label(incident.status),
        enum_label(incident.severity),
        enum_label(incident.task_relation),
        inline(&incident.title, 300)
    )
    .unwrap();
    writeln!(
        out,
        "     Cause={} | effect={} | category={}",
        enum_label(incident.cause),
        enum_label(incident.task_effect),
        enum_label(incident.category)
    )
    .unwrap();
    if incident.status == IncidentStatus::Completed {
        writeln!(
            out,
            "     Lifecycle: onset={} -> detected={} -> recovery=n/a -> confirmed=n/a",
            event_ref(incident.lifecycle.onset.as_deref()),
            event_ref(incident.lifecycle.detected.as_deref()),
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "     Lifecycle: onset={} -> detected={} -> recovery={} -> confirmed={}",
            event_ref(incident.lifecycle.onset.as_deref()),
            event_ref(incident.lifecycle.detected.as_deref()),
            event_ref(incident.lifecycle.recovery_started.as_deref()),
            event_ref(incident.lifecycle.recovery_confirmed.as_deref())
        )
        .unwrap();
    }
    if let Some(burden) = burden {
        let span = burden.span.as_ref().map_or_else(
            || "unknown".to_owned(),
            |span| {
                format!(
                    "{}..{} ({} ordered events)",
                    inline(&span.first_event_id, 100),
                    inline(&span.last_event_id, 100),
                    span.inclusive_event_count
                )
            },
        );
        writeln!(
            out,
            "     Coverage: {} touched events, {} agent tool calls, {}; span={span}",
            burden.affected_event_count,
            burden.affected_agent_action_count,
            duration_label(burden.known_duration_ms, burden.duration_event_count),
        )
        .unwrap();
    }
    render_affected_timeline(out, incident, event_index);
    for evidence in &incident.evidence {
        let context = event_index.get(evidence.event_id.as_str()).map_or_else(
            || "unknown event".to_owned(),
            |(_, event)| event_context(event),
        );
        writeln!(
            out,
            "     Evidence {} [{}; {}]: excerpt={} | {}",
            inline(&evidence.event_id, 120),
            evidence_label(evidence.basis),
            context,
            inline(&evidence.excerpt, 240),
            inline(&evidence.supports, 400)
        )
        .unwrap();
    }
    for unknown in &incident.causal_unknowns {
        writeln!(out, "     Causal unknown: {}", inline(unknown, 400)).unwrap();
    }
    for unknown in &incident.unknowns {
        writeln!(out, "     Unknown: {}", inline(unknown, 400)).unwrap();
    }
    writeln!(
        out,
        "     Rerun relevance: {}",
        inline(&incident.rerun_relevance, 600)
    )
    .unwrap();
}

fn render_affected_timeline(
    out: &mut String,
    incident: &Incident,
    event_index: &HashMap<&str, (usize, &TraceRecord)>,
) {
    const DISPLAY_LIMIT: usize = 80;
    let mut ordered = incident
        .affected_events
        .iter()
        .filter_map(|event_id| event_index.get(event_id.as_str()).copied())
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(position, _)| *position);
    writeln!(
        out,
        "     Affected timeline (showing {} of {}):",
        ordered.len().min(DISPLAY_LIMIT),
        ordered.len()
    )
    .unwrap();
    for (_, event) in ordered.iter().take(DISPLAY_LIMIT) {
        writeln!(
            out,
            "       - {} [{}]: {}",
            inline(event.id(), 120),
            event_context(event),
            inline(&event.searchable_text(), 300)
        )
        .unwrap();
    }
    if ordered.len() > DISPLAY_LIMIT {
        writeln!(
            out,
            "       - ... {} additional affected events omitted from terminal output; retained in report JSON",
            ordered.len() - DISPLAY_LIMIT
        )
        .unwrap();
    }
}

fn duration_label(known_duration_ms: u128, duration_event_count: usize) -> String {
    if duration_event_count == 0 {
        "duration unavailable".to_owned()
    } else {
        format!("{known_duration_ms} ms known across {duration_event_count} timed events")
    }
}

fn session(
    record: &TraceRecord,
) -> (
    &str,
    &str,
    &[String],
    &[String],
    Completeness,
    Option<&crate::model::Source>,
) {
    match record {
        TraceRecord::Session {
            id,
            task,
            constraints,
            success_criteria,
            completeness,
            source,
            ..
        } => (
            id,
            task,
            constraints,
            success_criteria,
            *completeness,
            source.as_ref(),
        ),
        _ => (
            "<invalid>",
            "<invalid>",
            &[],
            &[],
            Completeness::Unknown,
            None,
        ),
    }
}

fn has_task_updates(report: &Report) -> bool {
    report
        .trace
        .events
        .iter()
        .any(|event| matches!(event, TraceRecord::TaskUpdate { .. }))
}

fn render_event_ref(
    out: &mut String,
    label: &str,
    event_id: &str,
    event_index: &HashMap<&str, (usize, &TraceRecord)>,
) {
    let event = event_index.get(event_id).map(|(_, event)| *event);
    let context = event.map_or_else(|| "unknown event".to_owned(), event_context);
    let excerpt = event.map_or_else(
        || "unavailable".to_owned(),
        |event| inline(&event.searchable_text(), 400),
    );
    writeln!(
        out,
        "  - {label} {} [{}]: {}",
        inline(event_id, 120),
        context,
        excerpt
    )
    .unwrap();
}

fn event_context(event: &TraceRecord) -> String {
    let actor = match event {
        TraceRecord::Message { actor, .. }
        | TraceRecord::ToolCall { actor, .. }
        | TraceRecord::ToolResult { actor, .. }
        | TraceRecord::State { actor, .. }
        | TraceRecord::TaskUpdate { actor, .. }
        | TraceRecord::Outcome { actor, .. } => actor,
        TraceRecord::Session { .. } => "system",
    };
    format!("{} by {}", event.kind_name(), inline(actor, 120))
}

fn incident_order(incident: &Incident) -> (u8, u8, &str) {
    let resolved = u8::from(matches!(
        incident.status,
        IncidentStatus::Recovered | IncidentStatus::Completed
    ));
    let severity = match incident.severity {
        Severity::Critical => 0,
        Severity::High => 1,
        Severity::Unknown => 2,
        Severity::Medium => 3,
        Severity::Low => 4,
    };
    (resolved, severity, incident.id.as_str())
}

fn event_ref(value: Option<&str>) -> String {
    value.map_or_else(|| "?".into(), |value| inline(value, 120))
}

fn evidence_label(basis: EvidenceBasis) -> &'static str {
    match basis {
        EvidenceBasis::AgentAction => "agent action",
        EvidenceBasis::ToolObservation => "tool result record",
        EvidenceBasis::StateObservation => "state record",
        EvidenceBasis::TaskUpdateObservation => "task update record",
        EvidenceBasis::OutcomeObservation => "outcome record",
        EvidenceBasis::AgentReport => "agent report",
        EvidenceBasis::UserReport => "user report",
        EvidenceBasis::Inference => "judge inference",
    }
}

pub fn inline(value: &str, max_chars: usize) -> String {
    let mut clean = String::new();
    for (count, character) in value.chars().enumerate() {
        if count == max_chars {
            clean.push('…');
            break;
        }
        match character {
            '\n' | '\r' | '\t' => clean.push(' '),
            '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}' => {
                write!(clean, "\\u{{{:x}}}", character as u32).unwrap();
            }
            control if control.is_control() => {
                write!(clean, "\\u{{{:x}}}", control as u32).unwrap();
            }
            visible => clean.push(visible),
        }
    }
    clean
}

fn enum_label<T: serde::Serialize>(value: T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(label)) => label.replace('_', "-"),
        _ => "unknown".into(),
    }
}

fn decision_label(value: Recommendation) -> &'static str {
    match value {
        Recommendation::Keep => "KEEP",
        Recommendation::Rerun => "RERUN",
        Recommendation::Inspect => "INSPECT",
        Recommendation::Unknown => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{coverage_totals, inline};
    use crate::model::{SourceTrust, ToolStatus, TraceRecord};

    #[test]
    fn coverage_totals_distinguish_agent_calls_from_other_calls() {
        let call = |id: &str, actor: &str, call_id: &str| TraceRecord::ToolCall {
            id: id.into(),
            actor: actor.into(),
            call_id: call_id.into(),
            name: "shell".into(),
            input: serde_json::Value::Null,
            at: None,
            duration_ms: None,
            extensions: BTreeMap::new(),
        };
        let result = TraceRecord::ToolResult {
            id: "e3".into(),
            actor: "tool".into(),
            call_id: "c1".into(),
            status: ToolStatus::Ok,
            output: serde_json::Value::Null,
            at: None,
            duration_ms: None,
            extensions: BTreeMap::new(),
        };
        assert_eq!(
            coverage_totals(&[
                call("e1", "agent:worker", "c1"),
                call("e2", "system", "c2"),
                result,
            ]),
            (1, 1)
        );
    }

    #[test]
    fn terminal_text_is_single_line_and_control_safe() {
        assert_eq!(
            inline(
                "a\n\u{1b}[31m\u{061c}\u{200f}\u{202e}b\u{2028}\u{206f}c",
                100
            ),
            "a \\u{1b}[31m\\u{61c}\\u{200f}\\u{202e}b\\u{2028}\\u{206f}c"
        );
    }

    #[test]
    fn terminal_text_is_bounded() {
        assert_eq!(inline("abcd", 3), "abc…");
    }

    #[test]
    fn source_trust_serializes_to_schema_spelling() {
        assert_eq!(
            serde_json::to_value(SourceTrust::Unverified).unwrap(),
            "unverified"
        );
    }
}
