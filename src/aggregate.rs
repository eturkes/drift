use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    digest::{canonical_digest, normalized_trace_digest},
    error::{DriftError, Result},
    model::{
        AnalysisProvenance, Burden, EventSpan, Incident, IncidentBurden, Judgment, Report, Trace,
        TraceRecord, TraceSnapshot,
    },
    parse::validate_normalized_trace,
    validate::validate_judgment,
};

pub const REPORT_SCHEMA: &str = "drift.report/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisRun {
    pub rubric: String,
    pub rubric_digest: String,
    pub rubric_source: String,
    pub judgment_schema_digest: String,
    pub judgment_schema_source: String,
    pub judge: String,
    pub codex_cli: String,
    pub model: String,
    pub thread_id: String,
    pub attempts: usize,
}

/// Validate and assemble one judgment into a self-contained report.
pub fn aggregate_report(trace: &Trace, judgment: Judgment, run: AnalysisRun) -> Result<Report> {
    aggregate_report_at(trace, judgment, run, current_unix_ms())
}

fn aggregate_report_at(
    trace: &Trace,
    judgment: Judgment,
    run: AnalysisRun,
    generated_at_unix_ms: u128,
) -> Result<Report> {
    validate_normalized_trace(trace).map_err(|error| {
        DriftError::new(
            "E_TRACE_INVALID",
            format!("cannot aggregate invalid trace: {error}"),
        )
    })?;
    let issues = validate_judgment(trace, &judgment);
    if !issues.fatal.is_empty() {
        return Err(DriftError::new(
            "E_JUDGMENT_INVALID",
            format!(
                "cannot aggregate invalid judgment: {}",
                issues.fatal.join("; ")
            ),
        ));
    }
    if !(1..=3).contains(&run.attempts) {
        return Err(DriftError::new(
            "E_PROVENANCE",
            "analysis attempts must be between 1 and 3",
        ));
    }
    if [
        run.rubric.as_str(),
        run.rubric_source.as_str(),
        run.judgment_schema_source.as_str(),
        run.judge.as_str(),
        run.codex_cli.as_str(),
        run.model.as_str(),
        run.thread_id.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(DriftError::new(
            "E_PROVENANCE",
            "analysis provenance fields must be nonempty",
        ));
    }
    for (label, digest) in [
        ("rubric_digest", run.rubric_digest.as_str()),
        (
            "judgment_schema_digest",
            run.judgment_schema_digest.as_str(),
        ),
    ] {
        if !is_sha256(digest) {
            return Err(DriftError::new(
                "E_PROVENANCE",
                format!("{label} is not a sha256 digest"),
            ));
        }
    }
    if crate::digest::sha256_bytes(run.rubric_source.as_bytes()) != run.rubric_digest
        || crate::digest::sha256_bytes(run.judgment_schema_source.as_bytes())
            != run.judgment_schema_digest
    {
        return Err(DriftError::new(
            "E_PROVENANCE",
            "analysis source text does not match its recorded digest",
        ));
    }
    if run.rubric_source.len() > 256 * 1024
        || run.judgment_schema_source.len() > 1024 * 1024
        || !serde_json::from_str::<serde_json::Value>(&run.judgment_schema_source)
            .is_ok_and(|schema| schema.is_object())
    {
        return Err(DriftError::new(
            "E_PROVENANCE",
            "analysis rubric/schema source is oversized or judgment schema is not a JSON object",
        ));
    }

    let burden = derive_burden(trace, &judgment.incidents);
    let judgment_digest = canonical_digest(&judgment)?;
    Ok(Report {
        schema: REPORT_SCHEMA.to_owned(),
        trace: TraceSnapshot {
            input_digest: trace.input_digest.clone(),
            normalized_digest: normalized_trace_digest(trace)?,
            parser_warnings: trace.warnings.clone(),
            session: trace.header.clone(),
            events: trace.events.clone(),
        },
        analysis: AnalysisProvenance {
            drift_version: env!("CARGO_PKG_VERSION").to_owned(),
            rubric: run.rubric,
            rubric_digest: run.rubric_digest,
            rubric_source: run.rubric_source,
            judgment_schema_digest: run.judgment_schema_digest,
            judgment_schema_source: run.judgment_schema_source,
            judgment_digest,
            judge: run.judge,
            codex_cli: run.codex_cli,
            model: run.model,
            thread_id: run.thread_id,
            generated_at_unix_ms,
            attempts: run.attempts,
        },
        burden,
        overview: judgment.overview,
        summary: judgment.summary,
        rerun: judgment.rerun,
        incidents: judgment.incidents,
        gaps: judgment.gaps,
        validation_warnings: issues.warnings,
    })
}

pub fn derive_burden(trace: &Trace, incidents: &[Incident]) -> Burden {
    let index: HashMap<_, _> = trace
        .events
        .iter()
        .enumerate()
        .map(|(position, event)| (event.id(), (position, event)))
        .collect();
    let mut attributed = HashSet::new();
    let episodes = incidents
        .iter()
        .map(|incident| {
            attributed.extend(incident.affected_events.iter().map(String::as_str));
            incident_burden(&index, incident)
        })
        .collect();
    let totals = event_totals(&index, attributed.into_iter());
    Burden {
        attributed_event_count: totals.event_count,
        attributed_agent_action_count: totals.agent_action_count,
        attributed_tool_result_count: totals.tool_result_count,
        known_duration_ms: totals.known_duration_ms,
        duration_event_count: totals.duration_event_count,
        episodes,
    }
}

fn incident_burden(
    index: &HashMap<&str, (usize, &TraceRecord)>,
    incident: &Incident,
) -> IncidentBurden {
    let unique: HashSet<_> = incident
        .affected_events
        .iter()
        .map(String::as_str)
        .collect();
    let totals = event_totals(index, unique.iter().copied());
    let positions = unique
        .iter()
        .filter_map(|id| index.get(id).map(|(position, _)| (*position, *id)))
        .collect::<Vec<_>>();
    let span = positions
        .iter()
        .min_by_key(|(position, _)| *position)
        .zip(positions.iter().max_by_key(|(position, _)| *position))
        .map(|((first, first_id), (last, last_id))| EventSpan {
            first_event_id: (*first_id).to_owned(),
            last_event_id: (*last_id).to_owned(),
            inclusive_event_count: last - first + 1,
        });
    IncidentBurden {
        incident_id: incident.id.clone(),
        affected_event_count: totals.event_count,
        affected_agent_action_count: totals.agent_action_count,
        affected_tool_result_count: totals.tool_result_count,
        known_duration_ms: totals.known_duration_ms,
        duration_event_count: totals.duration_event_count,
        span,
    }
}

#[derive(Default)]
struct EventTotals {
    event_count: usize,
    agent_action_count: usize,
    tool_result_count: usize,
    known_duration_ms: u128,
    duration_event_count: usize,
}

fn event_totals<'a>(
    index: &HashMap<&str, (usize, &TraceRecord)>,
    ids: impl Iterator<Item = &'a str>,
) -> EventTotals {
    let mut totals = EventTotals::default();
    for id in ids {
        let Some((_, event)) = index.get(id) else {
            continue;
        };
        let event = *event;
        totals.event_count += 1;
        if matches!(event, TraceRecord::ToolCall { actor, .. } if role(actor) == "agent") {
            totals.agent_action_count += 1;
        }
        if matches!(event, TraceRecord::ToolResult { .. }) {
            totals.tool_result_count += 1;
        }
        if let Some(duration) = duration_ms(event) {
            totals.known_duration_ms += u128::from(duration);
            totals.duration_event_count += 1;
        }
    }
    totals
}

fn duration_ms(event: &TraceRecord) -> Option<u64> {
    match event {
        TraceRecord::Message { duration_ms, .. }
        | TraceRecord::ToolCall { duration_ms, .. }
        | TraceRecord::ToolResult { duration_ms, .. } => *duration_ms,
        _ => None,
    }
}

fn role(actor: &str) -> &str {
    actor.split_once(':').map_or(actor, |(role, _)| role)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn current_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{
        Cause, Completeness, Confidence, IncidentCategory, IncidentStatus, JudgedOutcome,
        Lifecycle, Recommendation, RerunAssessment, Severity, Source, SourceTrust, Summary,
        TaskEffect, TaskRelation, ToolStatus, Trajectory,
    };

    fn trace() -> Trace {
        Trace {
            header: TraceRecord::Session {
                schema: "drift.trace/v1".to_owned(),
                id: "trace-7".to_owned(),
                task: "Do the task".to_owned(),
                constraints: vec![],
                success_criteria: vec![],
                completeness: Completeness::Complete,
                source: Some(Source {
                    adapter: "test".to_owned(),
                    adapter_version: "1".to_owned(),
                    trust: SourceTrust::AdapterAsserted,
                    session_id: None,
                }),
                extensions: BTreeMap::new(),
            },
            events: vec![TraceRecord::Outcome {
                id: "e1".to_owned(),
                actor: "system".to_owned(),
                status: crate::model::OutcomeStatus::Success,
                text: "done".to_owned(),
                at: None,
                extensions: BTreeMap::new(),
            }],
            input_digest: format!("sha256:{}", "a".repeat(64)),
            warnings: vec![],
        }
    }

    fn judgment() -> Judgment {
        Judgment {
            schema: "drift.judgment/v1".to_owned(),
            overview: "Completed".to_owned(),
            summary: Summary {
                task_outcome: JudgedOutcome::TraceSupportedSuccess,
                outcome_reason: "Final success outcome".to_owned(),
                outcome_evidence: vec!["e1".to_owned()],
                trajectory: Trajectory::Clean,
                trajectory_reason: "No diversion".to_owned(),
                confidence: Confidence::High,
            },
            rerun: RerunAssessment {
                recommendation: Recommendation::Keep,
                reason: "No rerun signal".to_owned(),
                prerequisites: vec![],
                inspect: vec![],
                follow_up: vec![],
            },
            incidents: vec![],
            gaps: vec![],
        }
    }

    fn run() -> AnalysisRun {
        AnalysisRun {
            rubric: "drift-rubric/v1".to_owned(),
            rubric_digest: format!("sha256:{}", "b".repeat(64)),
            rubric_source: "test rubric".to_owned(),
            judgment_schema_digest: format!("sha256:{}", "c".repeat(64)),
            judgment_schema_source: "{}".to_owned(),
            judge: "codex".to_owned(),
            codex_cli: "codex 1.2.3".to_owned(),
            model: "gpt-test".to_owned(),
            thread_id: "thread-1".to_owned(),
            attempts: 2,
        }
    }

    #[test]
    fn assembles_self_contained_validated_report() {
        let mut run = run();
        run.rubric_digest = crate::digest::sha256_bytes(run.rubric_source.as_bytes());
        run.judgment_schema_digest =
            crate::digest::sha256_bytes(run.judgment_schema_source.as_bytes());
        let report = aggregate_report_at(&trace(), judgment(), run, 1_234_567).unwrap();
        assert_eq!(report.schema, REPORT_SCHEMA);
        assert_eq!(report.trace.session.id(), "trace-7");
        assert_eq!(report.trace.events.len(), 1);
        assert!(report.trace.normalized_digest.starts_with("sha256:"));
        assert_eq!(report.analysis.drift_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.analysis.thread_id, "thread-1");
        assert_eq!(report.analysis.generated_at_unix_ms, 1_234_567);
        assert_eq!(report.rerun.recommendation, Recommendation::Keep);
        assert_eq!(report.burden.attributed_event_count, 0);
    }

    #[test]
    fn rejects_invalid_judgment_at_public_aggregation_boundary() {
        let mut invalid = judgment();
        invalid.summary.outcome_evidence.clear();
        let error = aggregate_report(&trace(), invalid, valid_run()).unwrap_err();
        assert_eq!(error.code, "E_JUDGMENT_INVALID");
    }

    #[test]
    fn incident_coverage_deduplicates_global_overlap_and_preserves_episode_spans() {
        let mut trace = trace();
        trace.events = vec![
            TraceRecord::Message {
                id: "m1".to_owned(),
                actor: "agent".to_owned(),
                text: "noticed diversion".to_owned(),
                at: None,
                duration_ms: Some(5),
                extensions: BTreeMap::new(),
            },
            TraceRecord::ToolCall {
                id: "c1".to_owned(),
                actor: "agent:reviewer".to_owned(),
                call_id: "call-1".to_owned(),
                name: "shell".to_owned(),
                input: serde_json::json!({"cmd": "pwd"}),
                at: None,
                duration_ms: Some(10),
                extensions: BTreeMap::new(),
            },
            TraceRecord::ToolResult {
                id: "r1".to_owned(),
                actor: "tool".to_owned(),
                call_id: "call-1".to_owned(),
                status: ToolStatus::Ok,
                output: serde_json::json!("/repo"),
                at: None,
                duration_ms: Some(20),
                extensions: BTreeMap::new(),
            },
            TraceRecord::State {
                id: "s1".to_owned(),
                actor: "environment".to_owned(),
                values: BTreeMap::from([("cwd".to_owned(), serde_json::json!("/repo"))]),
                at: None,
                extensions: BTreeMap::new(),
            },
        ];
        let incident = |id: &str, affected_events: Vec<&str>| Incident {
            id: id.to_owned(),
            title: "episode".to_owned(),
            category: IncidentCategory::ContextState,
            task_relation: TaskRelation::IncidentalDetour,
            cause: Cause::Agent,
            severity: Severity::Low,
            status: IncidentStatus::Recovered,
            task_effect: TaskEffect::Delay,
            lifecycle: Lifecycle {
                onset: None,
                detected: None,
                recovery_started: None,
                recovery_confirmed: None,
            },
            affected_events: affected_events.into_iter().map(str::to_owned).collect(),
            evidence: vec![],
            causal_unknowns: vec![],
            unknowns: vec![],
            rerun_relevance: "test".to_owned(),
        };
        let burden = derive_burden(
            &trace,
            &[
                incident("a", vec!["m1", "c1", "r1"]),
                incident("b", vec!["r1", "s1"]),
            ],
        );

        assert_eq!(burden.attributed_event_count, 4);
        assert_eq!(burden.attributed_agent_action_count, 1);
        assert_eq!(burden.attributed_tool_result_count, 1);
        assert_eq!(burden.known_duration_ms, 35);
        assert_eq!(burden.duration_event_count, 3);
        assert_eq!(burden.episodes[0].affected_event_count, 3);
        assert_eq!(burden.episodes[1].affected_event_count, 2);
        assert_eq!(burden.episodes[1].affected_tool_result_count, 1);
        assert_eq!(burden.episodes[1].known_duration_ms, 20);
        assert_eq!(
            burden.episodes[0].span,
            Some(EventSpan {
                first_event_id: "m1".to_owned(),
                last_event_id: "r1".to_owned(),
                inclusive_event_count: 3,
            })
        );
    }

    fn valid_run() -> AnalysisRun {
        let mut run = run();
        run.rubric_digest = crate::digest::sha256_bytes(run.rubric_source.as_bytes());
        run.judgment_schema_digest =
            crate::digest::sha256_bytes(run.judgment_schema_source.as_bytes());
        run
    }
}
