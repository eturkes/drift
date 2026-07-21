use std::collections::{HashMap, HashSet};

use crate::model::{
    Cause, Completeness, Confidence, EvidenceBasis, Incident, IncidentCategory, IncidentStatus,
    JudgedOutcome, Judgment, OutcomeStatus, Recommendation, Severity, SourceTrust, TaskEffect,
    TaskRelation, ToolStatus, Trace, TraceRecord, Trajectory,
};

pub const TRACE_SCHEMA: &str = "drift.trace/v1";
pub const JUDGMENT_SCHEMA: &str = "drift.judgment/v1";
const MAX_INCIDENTS: usize = 128;
const MAX_EVIDENCE_PER_INCIDENT: usize = 32;
const MAX_AFFECTED_PER_INCIDENT: usize = 512;
const MAX_LIST_ITEMS: usize = 256;
const MAX_PROSE_CHARS: usize = 4_000;

struct IndexedEvent<'a> {
    position: usize,
    event: &'a TraceRecord,
    searchable: String,
}

type EventIndex<'a> = HashMap<&'a str, IndexedEvent<'a>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationIssues {
    pub fatal: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationIssues {
    pub fn is_valid(&self) -> bool {
        self.fatal.is_empty()
    }
}

/// Validate a Codex judgment against the exact trace it claims to describe.
///
/// Fatal issues make the judgment unsafe to render as a rerun recommendation.
/// Warnings preserve uncertainty without discarding an otherwise grounded report.
pub fn validate_judgment(trace: &Trace, judgment: &Judgment) -> ValidationIssues {
    let mut issues = ValidationIssues::default();

    validate_schemas(trace, judgment, &mut issues);
    validate_complexity(judgment, &mut issues);

    let mut events = HashMap::new();
    for (position, event) in trace.events.iter().enumerate() {
        if events
            .insert(
                event.id(),
                IndexedEvent {
                    position,
                    event,
                    searchable: event.searchable_text(),
                },
            )
            .is_some()
        {
            issues.fatal.push(format!(
                "trace event id {:?} is duplicated; judgment references would be ambiguous",
                event.id()
            ));
        }
    }

    let completeness = match &trace.header {
        TraceRecord::Session { completeness, .. } => Some(*completeness),
        _ => None,
    };
    if completeness == Some(Completeness::Unknown) {
        issues
            .warnings
            .push("trace completeness is unknown".to_owned());
    }

    validate_outcome_evidence(judgment, &events, &mut issues);
    validate_incidents(judgment, &events, &mut issues);
    validate_trajectory(judgment, &mut issues);
    validate_rerun_constraints(trace, completeness, judgment, &mut issues);

    issues
}

fn validate_complexity(judgment: &Judgment, issues: &mut ValidationIssues) {
    bounded_text("overview", &judgment.overview, MAX_PROSE_CHARS, issues);
    bounded_text(
        "summary.outcome_reason",
        &judgment.summary.outcome_reason,
        MAX_PROSE_CHARS,
        issues,
    );
    bounded_text(
        "summary.trajectory_reason",
        &judgment.summary.trajectory_reason,
        MAX_PROSE_CHARS,
        issues,
    );
    bounded_text(
        "rerun.reason",
        &judgment.rerun.reason,
        MAX_PROSE_CHARS,
        issues,
    );
    bounded_len(
        "summary.outcome_evidence",
        judgment.summary.outcome_evidence.len(),
        MAX_LIST_ITEMS,
        issues,
    );
    for event_id in &judgment.summary.outcome_evidence {
        bounded_text("summary.outcome_evidence id", event_id, 256, issues);
    }
    bounded_len(
        "rerun.prerequisites",
        judgment.rerun.prerequisites.len(),
        MAX_LIST_ITEMS,
        issues,
    );
    bounded_len(
        "rerun.inspect",
        judgment.rerun.inspect.len(),
        MAX_LIST_ITEMS,
        issues,
    );
    bounded_len(
        "rerun.follow_up",
        judgment.rerun.follow_up.len(),
        MAX_LIST_ITEMS,
        issues,
    );
    bounded_len("gaps", judgment.gaps.len(), MAX_LIST_ITEMS, issues);
    bounded_len("incidents", judgment.incidents.len(), MAX_INCIDENTS, issues);

    for (index, item) in judgment
        .rerun
        .prerequisites
        .iter()
        .chain(&judgment.rerun.inspect)
        .chain(&judgment.rerun.follow_up)
        .chain(&judgment.gaps)
        .enumerate()
    {
        bounded_text(
            &format!("decision text[{index}]"),
            item,
            MAX_PROSE_CHARS,
            issues,
        );
    }
    for incident in &judgment.incidents {
        bounded_text("incident.id", &incident.id, 256, issues);
        bounded_text("incident.title", &incident.title, 1_000, issues);
        bounded_text(
            "incident.rerun_relevance",
            &incident.rerun_relevance,
            MAX_PROSE_CHARS,
            issues,
        );
        bounded_len(
            "incident.affected_events",
            incident.affected_events.len(),
            MAX_AFFECTED_PER_INCIDENT,
            issues,
        );
        for event_id in &incident.affected_events {
            bounded_text("incident.affected_events id", event_id, 256, issues);
        }
        for (phase, event_id) in [
            ("onset", incident.lifecycle.onset.as_deref()),
            ("detected", incident.lifecycle.detected.as_deref()),
            (
                "recovery_started",
                incident.lifecycle.recovery_started.as_deref(),
            ),
            (
                "recovery_confirmed",
                incident.lifecycle.recovery_confirmed.as_deref(),
            ),
        ] {
            if let Some(event_id) = event_id {
                bounded_text(
                    &format!("incident.lifecycle.{phase}"),
                    event_id,
                    256,
                    issues,
                );
            }
        }
        bounded_len(
            "incident.evidence",
            incident.evidence.len(),
            MAX_EVIDENCE_PER_INCIDENT,
            issues,
        );
        bounded_len(
            "incident.causal_unknowns",
            incident.causal_unknowns.len(),
            MAX_LIST_ITEMS,
            issues,
        );
        bounded_len(
            "incident.unknowns",
            incident.unknowns.len(),
            MAX_LIST_ITEMS,
            issues,
        );
        for evidence in &incident.evidence {
            bounded_text("evidence.event_id", &evidence.event_id, 256, issues);
            bounded_text("evidence.supports", &evidence.supports, 1_000, issues);
        }
        for unknown in &incident.causal_unknowns {
            bounded_text("incident.causal_unknown", unknown, MAX_PROSE_CHARS, issues);
        }
        for unknown in &incident.unknowns {
            bounded_text("incident.unknown", unknown, MAX_PROSE_CHARS, issues);
        }
    }
}

fn bounded_text(label: &str, value: &str, maximum: usize, issues: &mut ValidationIssues) {
    let length = value.chars().count();
    if value.trim().is_empty() {
        issues.fatal.push(format!("{label} must be nonempty"));
    } else if length > maximum {
        issues.fatal.push(format!(
            "{label} is {length} characters; maximum is {maximum}"
        ));
    }
}

fn bounded_len(label: &str, actual: usize, maximum: usize, issues: &mut ValidationIssues) {
    if actual > maximum {
        issues
            .fatal
            .push(format!("{label} has {actual} items; maximum is {maximum}"));
    }
}

fn validate_schemas(trace: &Trace, judgment: &Judgment, issues: &mut ValidationIssues) {
    match &trace.header {
        TraceRecord::Session { schema, .. } if schema == TRACE_SCHEMA => {}
        TraceRecord::Session { schema, .. } => issues.fatal.push(format!(
            "trace schema must be {TRACE_SCHEMA:?}, found {schema:?}"
        )),
        other => issues.fatal.push(format!(
            "trace header must be a session record, found {}",
            other.kind_name()
        )),
    }

    if judgment.schema != JUDGMENT_SCHEMA {
        issues.fatal.push(format!(
            "judgment schema must be {JUDGMENT_SCHEMA:?}, found {:?}",
            judgment.schema
        ));
    }
}

fn validate_outcome_evidence<'a>(
    judgment: &Judgment,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    let mut has_trace_supported_success = false;
    let mut has_agent_success_claim = false;
    let mut seen = HashSet::new();

    for event_id in &judgment.summary.outcome_evidence {
        if !seen.insert(event_id.as_str()) {
            issues.warnings.push(format!(
                "summary outcome evidence repeats event {event_id:?}"
            ));
        }

        let Some(indexed) = events.get(event_id.as_str()) else {
            issues.fatal.push(format!(
                "summary outcome evidence references unknown event {event_id:?}"
            ));
            continue;
        };
        has_trace_supported_success |= matches!(
            indexed.event,
            TraceRecord::Outcome {
                status: OutcomeStatus::Success,
                actor,
                ..
            } if actor_has_any_role(actor, &["tool", "system", "environment", "user"])
        );
        has_agent_success_claim |= matches!(
            indexed.event,
            TraceRecord::Message { actor, .. } if actor_has_role(actor, "agent")
        );
    }

    if judgment.summary.task_outcome != JudgedOutcome::Unknown
        && judgment.summary.outcome_evidence.is_empty()
    {
        issues
            .fatal
            .push("non-unknown task outcome requires cited outcome_evidence".to_owned());
    }

    if judgment.summary.task_outcome == JudgedOutcome::ClaimedSuccess && !has_agent_success_claim {
        issues.fatal.push(
            "claimed_success requires outcome_evidence citing an agent message claim".to_owned(),
        );
    }

    if judgment.summary.task_outcome == JudgedOutcome::TraceSupportedSuccess
        && !has_trace_supported_success
    {
        issues.fatal.push(
            "trace_supported_success requires outcome_evidence citing a success outcome from a non-agent actor"
                .to_owned(),
        );
    }

    if let Some(indexed) = events.values().max_by_key(|event| event.position)
        && let TraceRecord::Outcome { status, .. } = indexed.event
    {
        if matches!(status, OutcomeStatus::Failure | OutcomeStatus::Partial)
            && matches!(
                judgment.summary.task_outcome,
                JudgedOutcome::Failure | JudgedOutcome::Partial
            )
            && !judgment
                .summary
                .outcome_evidence
                .iter()
                .any(|event_id| event_id == indexed.event.id())
        {
            issues.fatal.push(format!(
                "summary {:?} must cite final trace outcome {:?} in outcome_evidence",
                judgment.summary.task_outcome,
                indexed.event.id()
            ));
        }
        let contradictory = match status {
            OutcomeStatus::Failure => judgment.summary.task_outcome != JudgedOutcome::Failure,
            OutcomeStatus::Partial => matches!(
                judgment.summary.task_outcome,
                JudgedOutcome::TraceSupportedSuccess | JudgedOutcome::ClaimedSuccess
            ),
            OutcomeStatus::Success | OutcomeStatus::Unknown => false,
        };
        if contradictory {
            issues.fatal.push(format!(
                "summary task outcome {:?} contradicts final trace outcome {:?}",
                judgment.summary.task_outcome, status
            ));
        }
    }
}

fn validate_incidents<'a>(
    judgment: &Judgment,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    let mut incident_ids = HashSet::new();

    for incident in &judgment.incidents {
        if incident.id.trim().is_empty() {
            issues.fatal.push("incident id must be nonempty".to_owned());
        } else if incident
            .id
            .chars()
            .any(|character| character.is_control() || character == '\u{feff}')
        {
            issues.fatal.push(format!(
                "incident id {:?} contains a control or byte-order-mark character",
                incident.id
            ));
        } else if !incident_ids.insert(incident.id.as_str()) {
            issues
                .fatal
                .push(format!("incident id {:?} is duplicated", incident.id));
        }

        validate_lifecycle(incident, events, issues);
        validate_affected_lifecycle(incident, issues);
        validate_evidence(incident, events, issues);
        validate_status_grounding(incident, events, issues);
        validate_recovery_grounding(incident, events, issues);
        if incident.task_relation == TaskRelation::ExternalFriction
            && !matches!(
                incident.cause,
                Cause::Environment | Cause::Tool | Cause::External
            )
        {
            issues.fatal.push(format!(
                "incident {:?} external_friction requires environment, tool, or external cause",
                incident.id
            ));
        }
        if incident.status == IncidentStatus::Completed
            && !matches!(
                incident.task_relation,
                TaskRelation::IntrinsicTroubleshooting
                    | TaskRelation::LegitimateExploration
                    | TaskRelation::ExternalFriction
            )
        {
            issues.fatal.push(format!(
                "incident {:?} completed status is only valid for intrinsic troubleshooting, legitimate exploration, or ended external friction",
                incident.id
            ));
        }
        if incident.cause == Cause::Unknown
            && incident.causal_unknowns.is_empty()
            && incident.unknowns.is_empty()
        {
            issues.fatal.push(format!(
                "incident {:?} has unknown cause but does not explain it",
                incident.id
            ));
        }
        if (incident.severity == Severity::Unknown
            || incident.status == IncidentStatus::Unknown
            || incident.task_effect == TaskEffect::Unknown
            || incident.task_relation == TaskRelation::Unknown)
            && incident.unknowns.is_empty()
        {
            issues.fatal.push(format!(
                "incident {:?} uses a decision-relevant unknown classification but does not explain it in unknowns",
                incident.id
            ));
        }
        if incident.lifecycle.onset.is_none()
            && incident.causal_unknowns.is_empty()
            && incident.unknowns.is_empty()
        {
            issues.fatal.push(format!(
                "incident {:?} has unknown onset but does not explain it",
                incident.id
            ));
        }

        let mut local_affected = HashSet::new();
        if incident.affected_events.is_empty() {
            issues.fatal.push(format!(
                "incident {:?} must attribute at least one affected event",
                incident.id
            ));
        }
        for event_id in &incident.affected_events {
            if !events.contains_key(event_id.as_str()) {
                issues.fatal.push(format!(
                    "incident {:?} affected_events references unknown event {event_id:?}",
                    incident.id
                ));
                continue;
            }
            if !local_affected.insert(event_id.as_str()) {
                issues.fatal.push(format!(
                    "incident {:?} owns affected event {event_id:?} more than once",
                    incident.id
                ));
                continue;
            }
        }

        if incident.evidence.is_empty() {
            issues.fatal.push(format!(
                "incident {:?} must have at least one evidence citation",
                incident.id
            ));
        }
        if incident.lifecycle.detected.is_none() && incident.status != IncidentStatus::Completed {
            issues
                .warnings
                .push(format!("incident {:?} has no detection event", incident.id));
        }
    }
}

fn validate_lifecycle<'a>(
    incident: &Incident,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    let phases = [
        ("onset", incident.lifecycle.onset.as_deref()),
        ("detected", incident.lifecycle.detected.as_deref()),
        (
            "recovery_started",
            incident.lifecycle.recovery_started.as_deref(),
        ),
        (
            "recovery_confirmed",
            incident.lifecycle.recovery_confirmed.as_deref(),
        ),
    ];
    let mut prior: Option<(&str, usize)> = None;

    for (phase, event_id) in phases {
        let Some(event_id) = event_id else {
            continue;
        };
        let Some(indexed) = events.get(event_id) else {
            issues.fatal.push(format!(
                "incident {:?} lifecycle.{phase} references unknown event {event_id:?}",
                incident.id
            ));
            continue;
        };
        if let Some((prior_phase, prior_position)) = prior {
            let strict = matches!(
                (prior_phase, phase),
                ("detected", "recovery_started") | ("recovery_started", "recovery_confirmed")
            );
            if indexed.position < prior_position || (strict && indexed.position == prior_position) {
                issues.fatal.push(format!(
                    "incident {:?} lifecycle is out of order: {phase} must follow {prior_phase}",
                    incident.id
                ));
            }
        }
        prior = Some((phase, indexed.position));
    }

    if matches!(
        incident.status,
        IncidentStatus::Detected
            | IncidentStatus::RecoveryIntended
            | IncidentStatus::Recovering
            | IncidentStatus::Recovered
            | IncidentStatus::Unrecovered
    ) && incident.lifecycle.detected.is_none()
    {
        issues.fatal.push(format!(
            "incident {:?} status {:?} requires a detected event",
            incident.id, incident.status
        ));
    }

    match incident.status {
        IncidentStatus::Recovered
            if incident.lifecycle.recovery_started.is_none()
                || incident.lifecycle.recovery_confirmed.is_none() =>
        {
            issues.fatal.push(format!(
                "incident {:?} is recovered but lacks recovery_started or recovery_confirmed evidence",
                incident.id
            ));
        }
        IncidentStatus::Recovered => {}
        IncidentStatus::Completed
            if incident.lifecycle.recovery_started.is_some()
                || incident.lifecycle.recovery_confirmed.is_some() =>
        {
            issues.fatal.push(format!(
                "incident {:?} is completed without a recovery invariant but records recovery progress",
                incident.id
            ));
        }
        IncidentStatus::Recovering if incident.lifecycle.recovery_started.is_none() => {
            issues.fatal.push(format!(
                "incident {:?} is recovering but has no recovery_started event",
                incident.id
            ));
        }
        IncidentStatus::RecoveryIntended
            if incident.lifecycle.recovery_started.is_some()
                || incident.lifecycle.recovery_confirmed.is_some() =>
        {
            issues.fatal.push(format!(
                "incident {:?} is recovery_intended but records recovery progress",
                incident.id
            ));
        }
        IncidentStatus::Reported
            if incident.lifecycle.detected.is_some()
                || incident.lifecycle.recovery_started.is_some()
                || incident.lifecycle.recovery_confirmed.is_some() =>
        {
            issues.fatal.push(format!(
                "incident {:?} is reported narrative-only but records detection or recovery progress",
                incident.id
            ));
        }
        IncidentStatus::Detected if incident.lifecycle.recovery_started.is_some() => {
            issues.fatal.push(format!(
                "incident {:?} is {:?} but records recovery_started",
                incident.id, incident.status
            ));
        }
        _ => {
            if incident.lifecycle.recovery_confirmed.is_some() {
                issues.fatal.push(format!(
                    "incident {:?} has recovery_confirmed but status is {:?}",
                    incident.id, incident.status
                ));
            }
        }
    }
}

fn validate_evidence<'a>(
    incident: &Incident,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    for (index, evidence) in incident.evidence.iter().enumerate() {
        let label = format!("incident {:?} evidence[{index}]", incident.id);
        let excerpt_len = evidence.excerpt.chars().count();
        if evidence.excerpt.trim().is_empty() {
            issues
                .fatal
                .push(format!("{label} excerpt must be nonempty"));
        } else if excerpt_len > 240 {
            issues.fatal.push(format!(
                "{label} excerpt is {excerpt_len} characters; maximum is 240"
            ));
        }
        if evidence.supports.trim().is_empty() {
            issues
                .fatal
                .push(format!("{label} supports must be nonempty"));
        }

        let Some(indexed) = events.get(evidence.event_id.as_str()) else {
            issues.fatal.push(format!(
                "{label} references unknown event {:?}",
                evidence.event_id
            ));
            continue;
        };

        if !evidence.excerpt.is_empty() && !indexed.searchable.contains(&evidence.excerpt) {
            issues.fatal.push(format!(
                "{label} excerpt is not an exact substring of event {:?}",
                evidence.event_id
            ));
        }
        if !basis_matches_event(evidence.basis, indexed.event) {
            issues.fatal.push(format!(
                "{label} basis {:?} is inconsistent with {} event {:?}",
                evidence.basis,
                indexed.event.kind_name(),
                evidence.event_id
            ));
        }
    }
}

fn basis_matches_event(basis: EvidenceBasis, event: &TraceRecord) -> bool {
    match basis {
        EvidenceBasis::AgentAction => {
            matches!(event, TraceRecord::ToolCall { actor, .. } if actor_has_any_role(actor, &["agent", "system"]))
        }
        EvidenceBasis::ToolObservation => {
            matches!(event, TraceRecord::ToolResult { actor, .. } if actor_has_any_role(actor, &["tool", "system", "environment"]))
        }
        EvidenceBasis::StateObservation => {
            matches!(event, TraceRecord::State { actor, .. } if actor_has_any_role(actor, &["tool", "system", "environment"]))
        }
        EvidenceBasis::TaskUpdateObservation => {
            matches!(event, TraceRecord::TaskUpdate { actor, .. } if actor_has_any_role(actor, &["user", "system"]))
        }
        EvidenceBasis::OutcomeObservation => {
            matches!(event, TraceRecord::Outcome { actor, .. } if actor_has_any_role(actor, &["tool", "system", "environment", "user"]))
        }
        EvidenceBasis::AgentReport => {
            matches!(event, TraceRecord::Message { actor, .. } if actor_has_role(actor, "agent"))
        }
        EvidenceBasis::UserReport => {
            matches!(event, TraceRecord::Message { actor, .. } if actor_has_role(actor, "user"))
        }
        EvidenceBasis::Inference => true,
    }
}

fn validate_status_grounding<'a>(
    incident: &Incident,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    if incident.status == IncidentStatus::Reported {
        let has_narrative = incident.evidence.iter().any(|evidence| {
            events
                .get(evidence.event_id.as_str())
                .is_some_and(|indexed| matches!(indexed.event, TraceRecord::Message { .. }))
        });
        if !has_narrative {
            issues.fatal.push(format!(
                "incident {:?} reported status requires evidence citing a narrative message",
                incident.id
            ));
        }
    }

    if incident.status == IncidentStatus::RecoveryIntended {
        let detected_position = incident
            .lifecycle
            .detected
            .as_deref()
            .and_then(|event_id| events.get(event_id))
            .map(|indexed| indexed.position);
        let has_intent_report = detected_position.is_some_and(|detected_position| {
            incident.evidence.iter().any(|evidence| {
                evidence.basis == EvidenceBasis::AgentReport
                    && events
                        .get(evidence.event_id.as_str())
                        .is_some_and(|indexed| indexed.position >= detected_position)
            })
        });
        if !has_intent_report {
            issues.fatal.push(format!(
                "incident {:?} recovery_intended status requires an agent-report evidence event at or after detection",
                incident.id
            ));
        }
    }
}

fn actor_has_role(actor: &str, role: &str) -> bool {
    actor == role
        || actor
            .strip_prefix(role)
            .is_some_and(|suffix| suffix.starts_with(':'))
}

fn actor_has_any_role(actor: &str, roles: &[&str]) -> bool {
    roles.iter().any(|role| actor_has_role(actor, role))
}

fn validate_recovery_grounding<'a>(
    incident: &Incident,
    events: &EventIndex<'a>,
    issues: &mut ValidationIssues,
) {
    if let Some(started_id) = incident.lifecycle.recovery_started.as_deref()
        && let Some(started) = events.get(started_id)
    {
        let is_recovery_action = matches!(
            started.event,
            TraceRecord::ToolCall { actor, .. }
                if actor_has_any_role(actor, &["agent", "system"])
        ) || matches!(
            started.event,
            TraceRecord::Message { actor, .. } if actor_has_role(actor, "agent")
        );
        if !is_recovery_action {
            issues.fatal.push(format!(
                "incident {:?} recovery_started must reference an agent message or agent/system tool_call action",
                incident.id
            ));
        }
    }

    if incident.status != IncidentStatus::Recovered {
        return;
    }

    let Some(confirmed_id) = incident.lifecycle.recovery_confirmed.as_deref() else {
        issues.fatal.push(format!(
            "incident {:?} is recovered but has no recovery_confirmed event",
            incident.id
        ));
        return;
    };
    let Some(confirmed) = events.get(confirmed_id) else {
        return; // The dangling lifecycle reference is reported by validate_lifecycle.
    };

    let observed_recovery = matches!(
        confirmed.event,
        TraceRecord::ToolResult {
            status: ToolStatus::Ok,
            actor,
            ..
        } if actor_has_any_role(actor, &["tool", "system", "environment"])
    ) || matches!(
        confirmed.event,
        TraceRecord::State { actor, values, .. }
            if !values.is_empty() && actor_has_any_role(actor, &["tool", "system", "environment"])
    ) || matches!(
        confirmed.event,
        TraceRecord::Outcome { actor, status: OutcomeStatus::Success, .. }
            if actor_has_any_role(actor, &["tool", "system", "environment", "user"])
    );
    if !observed_recovery {
        issues.fatal.push(format!(
            "incident {:?} recovery_confirmed must reference a trace-recorded successful tool result, nonempty state, or success outcome",
            incident.id
        ));
    }

    let cited_as_observation = incident.evidence.iter().any(|evidence| {
        evidence.event_id == confirmed_id
            && matches!(
                evidence.basis,
                EvidenceBasis::ToolObservation
                    | EvidenceBasis::StateObservation
                    | EvidenceBasis::OutcomeObservation
            )
    });
    if !cited_as_observation {
        issues.fatal.push(format!(
            "incident {:?} recovered status requires trace-recorded observation evidence citing recovery_confirmed event {confirmed_id:?}",
            incident.id
        ));
    }
}

fn validate_affected_lifecycle(incident: &Incident, issues: &mut ValidationIssues) {
    let affected: HashSet<_> = incident
        .affected_events
        .iter()
        .map(String::as_str)
        .collect();
    for (phase, event_id) in [
        ("onset", incident.lifecycle.onset.as_deref()),
        ("detected", incident.lifecycle.detected.as_deref()),
        (
            "recovery_started",
            incident.lifecycle.recovery_started.as_deref(),
        ),
        (
            "recovery_confirmed",
            incident.lifecycle.recovery_confirmed.as_deref(),
        ),
    ] {
        if let Some(event_id) = event_id
            && !affected.contains(event_id)
        {
            issues.fatal.push(format!(
                "incident {:?} lifecycle.{phase} event {event_id:?} is absent from affected_events",
                incident.id
            ));
        }
    }
}

fn validate_trajectory(judgment: &Judgment, issues: &mut ValidationIssues) {
    let detours: Vec<_> = judgment
        .incidents
        .iter()
        .filter(|incident| is_detour_relation(incident.task_relation))
        .collect();
    let has_unresolved_detour = detours
        .iter()
        .any(|incident| is_unresolved(incident.status));

    match judgment.summary.trajectory {
        Trajectory::Clean if !detours.is_empty() => issues
            .fatal
            .push("clean trajectory is inconsistent with detour incidents".to_owned()),
        Trajectory::RecoveredDetour => {
            if detours.is_empty() {
                issues.fatal.push(
                    "recovered_detour trajectory requires at least one detour incident".to_owned(),
                );
            } else if detours.iter().any(|incident| {
                !matches!(
                    incident.status,
                    IncidentStatus::Recovered | IncidentStatus::Completed
                )
            }) {
                issues.fatal.push(
                    "recovered_detour trajectory requires every detour incident to be recovered or completed"
                        .to_owned(),
                );
            }
        }
        Trajectory::UnrecoveredDetour if !has_unresolved_detour => issues.fatal.push(
            "unrecovered_detour trajectory requires an unresolved detour incident".to_owned(),
        ),
        Trajectory::Derailed if detours.is_empty() => issues
            .fatal
            .push("derailed trajectory requires at least one detour incident".to_owned()),
        Trajectory::Derailed
            if !has_unresolved_detour
                && judgment.summary.task_outcome != JudgedOutcome::Failure =>
        {
            issues.fatal.push(
                "derailed trajectory requires an unresolved detour or failed task outcome"
                    .to_owned(),
            );
        }
        Trajectory::Unknown if !judgment.incidents.is_empty() => issues
            .warnings
            .push("trajectory is unknown despite one or more classified incidents".to_owned()),
        _ => {}
    }
}

fn validate_rerun_constraints(
    trace: &Trace,
    completeness: Option<Completeness>,
    judgment: &Judgment,
    issues: &mut ValidationIssues,
) {
    if matches!(
        completeness,
        Some(Completeness::Partial | Completeness::Unknown)
    ) && judgment.summary.confidence == Confidence::High
    {
        if judgment.summary.trajectory == Trajectory::Clean {
            issues.fatal.push(
                "partial/unknown trace cannot support a high-confidence clean trajectory"
                    .to_owned(),
            );
        }
        if judgment.rerun.recommendation == Recommendation::Keep {
            issues.fatal.push(
                "partial/unknown trace cannot support a high-confidence keep recommendation"
                    .to_owned(),
            );
        }
    }

    if judgment.rerun.recommendation == Recommendation::Keep {
        if !judgment.rerun.inspect.is_empty() || !judgment.rerun.prerequisites.is_empty() {
            issues.fatal.push(
                "keep requires empty inspect and prerequisites; non-blocking work belongs in follow_up"
                    .to_owned(),
            );
        }
        if completeness != Some(Completeness::Complete) {
            issues
                .fatal
                .push("keep requires a complete trace".to_owned());
        }
        if !trace.warnings.is_empty() {
            issues
                .fatal
                .push("keep is forbidden while trace parser warnings remain".to_owned());
        }
        if trace.source().map(|source| source.trust) != Some(SourceTrust::AdapterAsserted) {
            issues.fatal.push(
                "keep requires source.trust=adapter_asserted; trace fidelity is otherwise unverified"
                    .to_owned(),
            );
        }
        if judgment.summary.task_outcome != JudgedOutcome::TraceSupportedSuccess {
            issues.fatal.push(
                "keep requires trace_supported_success from the final outcome record".to_owned(),
            );
        }
        if judgment.summary.confidence != Confidence::High {
            issues
                .fatal
                .push("keep requires high judge confidence".to_owned());
        }
        if !matches!(
            judgment.summary.trajectory,
            Trajectory::Clean | Trajectory::RecoveredDetour
        ) {
            issues
                .fatal
                .push("keep requires a clean or recovered_detour trajectory".to_owned());
        }
        if !judgment.gaps.is_empty() {
            issues
                .fatal
                .push("keep is forbidden while evidence gaps remain".to_owned());
        }
        for incident in &judgment.incidents {
            if is_unresolved(incident.status) {
                issues.fatal.push(format!(
                    "keep recommendation is forbidden with unresolved incident {:?}",
                    incident.id
                ));
            }
            if !incident.unknowns.is_empty() {
                issues.fatal.push(format!(
                    "keep recommendation is forbidden while incident {:?} has unknowns",
                    incident.id
                ));
            }
            if matches!(
                incident.task_effect,
                TaskEffect::StateRisk
                    | TaskEffect::ArtifactRisk
                    | TaskEffect::OutcomeRisk
                    | TaskEffect::Unknown
            ) {
                issues.fatal.push(format!(
                    "keep recommendation is forbidden with residual-risk effect {:?} on incident {:?}",
                    incident.task_effect, incident.id
                ));
            }
            if matches!(
                incident.severity,
                Severity::High | Severity::Critical | Severity::Unknown
            ) || incident.category == IncidentCategory::ConstraintOrSafety
            {
                issues.fatal.push(format!(
                    "keep recommendation requires inspection after material incident {:?}",
                    incident.id
                ));
            }
        }
    }
    if judgment.rerun.recommendation == Recommendation::Inspect && judgment.rerun.inspect.is_empty()
    {
        issues
            .fatal
            .push("inspect recommendation requires at least one concrete inspect check".to_owned());
    }
}

fn is_detour_relation(relation: TaskRelation) -> bool {
    matches!(
        relation,
        TaskRelation::IncidentalDetour | TaskRelation::ExternalFriction | TaskRelation::Unknown
    )
}

fn is_unresolved(status: IncidentStatus) -> bool {
    !matches!(
        status,
        IncidentStatus::Recovered | IncidentStatus::Completed
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::model::{
        Cause, Evidence, IncidentCategory, Lifecycle, OutcomeStatus, RerunAssessment, Severity,
        Source, SourceTrust, Summary, TaskEffect,
    };

    fn trace(completeness: Completeness) -> Trace {
        Trace {
            header: TraceRecord::Session {
                schema: TRACE_SCHEMA.to_owned(),
                id: "run-1".to_owned(),
                task: "Check CI".to_owned(),
                constraints: vec![],
                success_criteria: vec![],
                completeness,
                source: Some(Source {
                    adapter: "test".to_owned(),
                    adapter_version: "1".to_owned(),
                    trust: SourceTrust::AdapterAsserted,
                    session_id: None,
                }),
                extensions: BTreeMap::new(),
            },
            events: vec![
                TraceRecord::Message {
                    id: "m1".to_owned(),
                    actor: "agent".to_owned(),
                    text: "cwd had drifted".to_owned(),
                    at: None,
                    duration_ms: None,
                    extensions: BTreeMap::new(),
                },
                TraceRecord::ToolCall {
                    id: "c1".to_owned(),
                    actor: "agent".to_owned(),
                    call_id: "call-1".to_owned(),
                    name: "shell".to_owned(),
                    input: json!({"cmd": "cd /repo && make ci"}),
                    at: None,
                    duration_ms: None,
                    extensions: BTreeMap::new(),
                },
                TraceRecord::ToolResult {
                    id: "r1".to_owned(),
                    actor: "tool".to_owned(),
                    call_id: "call-1".to_owned(),
                    status: ToolStatus::Ok,
                    output: Value::String("CI passed".to_owned()),
                    at: None,
                    duration_ms: None,
                    extensions: BTreeMap::new(),
                },
                TraceRecord::State {
                    id: "s1".to_owned(),
                    actor: "tool".to_owned(),
                    values: BTreeMap::from([("cwd".to_owned(), json!("/repo"))]),
                    at: None,
                    extensions: BTreeMap::new(),
                },
                TraceRecord::Outcome {
                    id: "o1".to_owned(),
                    actor: "tool".to_owned(),
                    status: OutcomeStatus::Success,
                    text: "CI passed from /repo".to_owned(),
                    at: None,
                    extensions: BTreeMap::new(),
                },
            ],
            input_digest: "sha256:test".to_owned(),
            warnings: vec![],
        }
    }

    fn recovered_incident() -> Incident {
        Incident {
            id: "d1".to_owned(),
            title: "cwd drift".to_owned(),
            category: IncidentCategory::ContextState,
            task_relation: TaskRelation::IncidentalDetour,
            cause: Cause::Agent,
            severity: Severity::Low,
            status: IncidentStatus::Recovered,
            task_effect: TaskEffect::Delay,
            lifecycle: Lifecycle {
                onset: Some("m1".to_owned()),
                detected: Some("m1".to_owned()),
                recovery_started: Some("c1".to_owned()),
                recovery_confirmed: Some("r1".to_owned()),
            },
            affected_events: vec!["m1".to_owned(), "c1".to_owned(), "r1".to_owned()],
            evidence: vec![
                Evidence {
                    event_id: "m1".to_owned(),
                    basis: EvidenceBasis::AgentReport,
                    supports: "Agent reported cwd drift".to_owned(),
                    excerpt: "cwd had drifted".to_owned(),
                },
                Evidence {
                    event_id: "r1".to_owned(),
                    basis: EvidenceBasis::ToolObservation,
                    supports: "CI passed after recovery".to_owned(),
                    excerpt: "CI passed".to_owned(),
                },
            ],
            causal_unknowns: vec![],
            unknowns: vec![],
            rerun_relevance: "Recovered".to_owned(),
        }
    }

    fn judgment() -> Judgment {
        Judgment {
            schema: JUDGMENT_SCHEMA.to_owned(),
            overview: "Recovered cwd detour".to_owned(),
            summary: Summary {
                task_outcome: JudgedOutcome::TraceSupportedSuccess,
                outcome_reason: "CI passed".to_owned(),
                outcome_evidence: vec!["o1".to_owned()],
                trajectory: Trajectory::RecoveredDetour,
                trajectory_reason: "Recovery observed".to_owned(),
                confidence: Confidence::High,
            },
            rerun: RerunAssessment {
                recommendation: Recommendation::Keep,
                reason: "Validated recovery".to_owned(),
                prerequisites: vec![],
                inspect: vec![],
                follow_up: vec![],
            },
            incidents: vec![recovered_incident()],
            gaps: vec![],
        }
    }

    fn has_fatal(issues: &ValidationIssues, needle: &str) -> bool {
        issues.fatal.iter().any(|issue| issue.contains(needle))
    }

    #[test]
    fn accepts_grounded_recovered_judgment() {
        let issues = validate_judgment(&trace(Completeness::Complete), &judgment());
        assert!(issues.is_valid(), "{:?}", issues.fatal);
        assert!(issues.warnings.is_empty(), "{:?}", issues.warnings);
    }

    #[test]
    fn rejects_wrong_schemas_and_every_dangling_reference_site() {
        let mut bad_trace = trace(Completeness::Complete);
        if let TraceRecord::Session { schema, .. } = &mut bad_trace.header {
            *schema = "wrong".to_owned();
        }
        let mut bad_judgment = judgment();
        bad_judgment.schema = "wrong".to_owned();
        bad_judgment.summary.outcome_evidence = vec!["missing-outcome".to_owned()];
        let incident = &mut bad_judgment.incidents[0];
        incident.lifecycle.onset = Some("missing-onset".to_owned());
        incident.affected_events.push("missing-affected".to_owned());
        incident.evidence[0].event_id = "missing-evidence".to_owned();

        let issues = validate_judgment(&bad_trace, &bad_judgment);
        assert!(has_fatal(&issues, "trace schema"));
        assert!(has_fatal(&issues, "judgment schema"));
        assert!(has_fatal(&issues, "missing-outcome"));
        assert!(has_fatal(&issues, "missing-onset"));
        assert!(has_fatal(&issues, "missing-affected"));
        assert!(has_fatal(&issues, "missing-evidence"));

        let mut malformed_header = trace(Completeness::Complete);
        malformed_header.header = malformed_header.events[0].clone();
        let issues = validate_judgment(&malformed_header, &judgment());
        assert!(has_fatal(&issues, "trace header must be a session"));
    }

    #[test]
    fn enforces_exact_bounded_excerpts_and_basis_kind() {
        let base_trace = trace(Completeness::Complete);
        let mut bad_judgment = judgment();
        bad_judgment.incidents[0].evidence[0].excerpt = "not in source".to_owned();
        bad_judgment.incidents[0].evidence[1].basis = EvidenceBasis::StateObservation;
        bad_judgment.incidents[0].evidence.push(Evidence {
            event_id: "m1".to_owned(),
            basis: EvidenceBasis::Inference,
            supports: "Empty excerpt".to_owned(),
            excerpt: "   ".to_owned(),
        });

        let issues = validate_judgment(&base_trace, &bad_judgment);
        assert!(has_fatal(&issues, "not an exact substring"));
        assert!(has_fatal(&issues, "basis StateObservation is inconsistent"));
        assert!(has_fatal(&issues, "excerpt must be nonempty"));

        let long = "x".repeat(241);
        let mut long_trace = trace(Completeness::Complete);
        if let TraceRecord::Message { text, .. } = &mut long_trace.events[0] {
            *text = long.clone();
        }
        let mut long_judgment = judgment();
        long_judgment.incidents[0].evidence[0].excerpt = long;
        let issues = validate_judgment(&long_trace, &long_judgment);
        assert!(has_fatal(&issues, "maximum is 240"));
    }

    #[test]
    fn enforces_lifecycle_order_and_observed_recovery() {
        let base_trace = trace(Completeness::Complete);
        let mut bad_judgment = judgment();
        let incident = &mut bad_judgment.incidents[0];
        incident.lifecycle.detected = Some("r1".to_owned());
        incident.lifecycle.recovery_started = Some("c1".to_owned());
        incident.lifecycle.recovery_confirmed = Some("m1".to_owned());

        let issues = validate_judgment(&base_trace, &bad_judgment);
        assert!(has_fatal(&issues, "lifecycle is out of order"));
        assert!(has_fatal(
            &issues,
            "must reference a trace-recorded successful tool result"
        ));
        assert!(has_fatal(
            &issues,
            "requires trace-recorded observation evidence"
        ));

        let mut uncited = judgment();
        uncited.incidents[0]
            .evidence
            .retain(|evidence| evidence.event_id != "r1");
        let issues = validate_judgment(&base_trace, &uncited);
        assert!(has_fatal(
            &issues,
            "requires trace-recorded observation evidence"
        ));
    }

    #[test]
    fn trace_supported_success_requires_cited_success_outcome() {
        let trace = trace(Completeness::Complete);
        let mut judgment = judgment();
        judgment.summary.outcome_evidence = vec!["m1".to_owned(), "c1".to_owned()];

        let issues = validate_judgment(&trace, &judgment);
        assert!(has_fatal(&issues, "trace_supported_success requires"));
    }

    #[test]
    fn every_asserted_outcome_is_cited_and_claimed_success_cites_agent_message() {
        let trace = trace(Completeness::Complete);
        let mut claimed = judgment();
        claimed.summary.task_outcome = JudgedOutcome::ClaimedSuccess;
        claimed.summary.outcome_evidence.clear();
        claimed.rerun.recommendation = Recommendation::Inspect;
        claimed.rerun.inspect = vec!["Check the claimed result".to_owned()];
        let issues = validate_judgment(&trace, &claimed);
        assert!(has_fatal(&issues, "non-unknown task outcome"));
        assert!(has_fatal(&issues, "citing an agent message"));

        claimed.summary.outcome_evidence = vec!["o1".to_owned()];
        let issues = validate_judgment(&trace, &claimed);
        assert!(has_fatal(&issues, "citing an agent message"));

        claimed.summary.outcome_evidence = vec!["m1".to_owned()];
        let issues = validate_judgment(&trace, &claimed);
        assert!(issues.is_valid(), "{:?}", issues.fatal);
    }

    #[test]
    fn partial_or_failed_final_outcome_must_be_cited() {
        for (status, judged) in [
            (OutcomeStatus::Partial, JudgedOutcome::Partial),
            (OutcomeStatus::Failure, JudgedOutcome::Failure),
        ] {
            let mut trace = trace(Completeness::Complete);
            if let TraceRecord::Outcome {
                status: final_status,
                ..
            } = trace.events.last_mut().unwrap()
            {
                *final_status = status;
            }
            let mut judgment = judgment();
            judgment.summary.task_outcome = judged;
            judgment.summary.outcome_evidence = vec!["m1".to_owned()];
            judgment.rerun.recommendation = Recommendation::Inspect;
            judgment.rerun.inspect = vec!["Inspect the incomplete result".to_owned()];

            let issues = validate_judgment(&trace, &judgment);
            assert!(has_fatal(&issues, "must cite final trace outcome"));

            judgment.summary.outcome_evidence.push("o1".to_owned());
            let issues = validate_judgment(&trace, &judgment);
            assert!(issues.is_valid(), "{:?}", issues.fatal);
        }
    }

    #[test]
    fn incidents_require_evidence_even_when_other_fields_are_grounded() {
        let trace = trace(Completeness::Complete);
        let mut judgment = judgment();
        judgment.incidents[0].evidence.clear();

        let issues = validate_judgment(&trace, &judgment);
        assert!(has_fatal(&issues, "at least one evidence citation"));
    }

    #[test]
    fn affected_events_are_unique_within_incident_but_may_overlap_incidents() {
        let trace = trace(Completeness::Complete);
        let mut overlapping = judgment();
        let mut second = recovered_incident();
        second.id = "d2".to_owned();
        overlapping.incidents.push(second.clone());
        let issues = validate_judgment(&trace, &overlapping);
        assert!(issues.is_valid(), "{:?}", issues.fatal);

        let mut duplicated = judgment();
        second.affected_events = vec!["r1".to_owned(), "r1".to_owned()];
        duplicated.incidents.push(second);

        let issues = validate_judgment(&trace, &duplicated);
        assert!(has_fatal(&issues, "more than once"));
    }

    #[test]
    fn trajectory_matches_detour_status_but_ignores_legitimate_exploration() {
        let trace = trace(Completeness::Complete);
        let mut inconsistent = judgment();
        inconsistent.summary.trajectory = Trajectory::Clean;
        let issues = validate_judgment(&trace, &inconsistent);
        assert!(has_fatal(&issues, "clean trajectory is inconsistent"));

        let mut legitimate = judgment();
        legitimate.summary.trajectory = Trajectory::Clean;
        legitimate.summary.task_outcome = JudgedOutcome::Unknown;
        legitimate.summary.outcome_evidence.clear();
        legitimate.incidents[0].task_relation = TaskRelation::LegitimateExploration;
        legitimate.rerun.recommendation = Recommendation::Inspect;
        legitimate.rerun.inspect = vec!["Review the result".to_owned()];
        let issues = validate_judgment(&trace, &legitimate);
        assert!(issues.is_valid(), "{:?}", issues.fatal);

        let mut unresolved = judgment();
        unresolved.summary.trajectory = Trajectory::RecoveredDetour;
        unresolved.incidents[0].status = IncidentStatus::Unrecovered;
        unresolved.incidents[0].lifecycle.recovery_confirmed = None;
        let issues = validate_judgment(&trace, &unresolved);
        assert!(has_fatal(&issues, "every detour incident to be recovered"));
    }

    #[test]
    fn completed_non_detour_is_resolved_without_fake_recovery() {
        let trace = trace(Completeness::Complete);
        let mut completed = judgment();
        let incident = &mut completed.incidents[0];
        incident.task_relation = TaskRelation::LegitimateExploration;
        incident.status = IncidentStatus::Completed;
        incident.lifecycle.recovery_started = None;
        incident.lifecycle.recovery_confirmed = None;
        completed.summary.trajectory = Trajectory::Clean;

        let issues = validate_judgment(&trace, &completed);
        assert!(issues.is_valid(), "{:?}", issues.fatal);

        let mut material_intrinsic = completed.clone();
        material_intrinsic.incidents[0].task_relation = TaskRelation::IntrinsicTroubleshooting;
        material_intrinsic.incidents[0].severity = Severity::High;
        material_intrinsic.incidents[0].task_effect = TaskEffect::OutcomeRisk;
        material_intrinsic.rerun.recommendation = Recommendation::Inspect;
        material_intrinsic.rerun.inspect = vec!["Inspect the risky result".to_owned()];
        let issues = validate_judgment(&trace, &material_intrinsic);
        assert!(issues.is_valid(), "{:?}", issues.fatal);

        let mut ended_friction = completed.clone();
        ended_friction.incidents[0].task_relation = TaskRelation::ExternalFriction;
        ended_friction.incidents[0].cause = Cause::Environment;
        ended_friction.summary.trajectory = Trajectory::RecoveredDetour;
        let issues = validate_judgment(&trace, &ended_friction);
        assert!(issues.is_valid(), "{:?}", issues.fatal);

        let mut detour = completed.clone();
        detour.incidents[0].task_relation = TaskRelation::IncidentalDetour;
        let issues = validate_judgment(&trace, &detour);
        assert!(has_fatal(&issues, "completed status is only valid"));

        completed.incidents[0].lifecycle.recovery_started = Some("c1".to_owned());
        let issues = validate_judgment(&trace, &completed);
        assert!(has_fatal(&issues, "records recovery progress"));
    }

    #[test]
    fn reported_status_is_narrative_only() {
        let trace = trace(Completeness::Complete);
        let mut reported = judgment();
        let incident = &mut reported.incidents[0];
        incident.status = IncidentStatus::Reported;
        incident.lifecycle.recovery_started = None;
        incident.lifecycle.recovery_confirmed = None;
        reported.summary.trajectory = Trajectory::UnrecoveredDetour;
        reported.rerun.recommendation = Recommendation::Inspect;
        reported.rerun.inspect = vec!["Observe the reported state".to_owned()];

        let issues = validate_judgment(&trace, &reported);
        assert!(has_fatal(&issues, "narrative-only"));

        reported.incidents[0].lifecycle.detected = None;
        reported.incidents[0].evidence = vec![reported.incidents[0].evidence[1].clone()];
        let issues = validate_judgment(&trace, &reported);
        assert!(has_fatal(&issues, "requires evidence citing a narrative"));
    }

    #[test]
    fn recovery_intent_requires_a_cited_agent_report() {
        let trace = trace(Completeness::Complete);
        let mut intended = judgment();
        let incident = &mut intended.incidents[0];
        incident.status = IncidentStatus::RecoveryIntended;
        incident.lifecycle.recovery_started = None;
        incident.lifecycle.recovery_confirmed = None;
        incident.evidence = vec![incident.evidence[1].clone()];
        intended.summary.trajectory = Trajectory::UnrecoveredDetour;
        intended.rerun.recommendation = Recommendation::Inspect;
        intended.rerun.inspect = vec!["Observe the intended recovery".to_owned()];

        let issues = validate_judgment(&trace, &intended);
        assert!(has_fatal(&issues, "requires an agent-report"));

        intended.incidents[0]
            .evidence
            .push(recovered_incident().evidence[0].clone());
        let issues = validate_judgment(&trace, &intended);
        assert!(issues.is_valid(), "{:?}", issues.fatal);
    }

    #[test]
    fn incident_ids_match_schema_identifier_safety() {
        let trace = trace(Completeness::Complete);
        for id in ["bad\u{85}id", "bad\u{feff}id"] {
            let mut judgment = judgment();
            judgment.incidents[0].id = id.to_owned();
            let issues = validate_judgment(&trace, &judgment);
            assert!(has_fatal(&issues, "control or byte-order-mark"));
        }
    }

    #[test]
    fn empty_successful_tool_output_can_confirm_recovery_by_status() {
        let mut trace = trace(Completeness::Complete);
        if let TraceRecord::ToolResult { output, .. } = &mut trace.events[2] {
            *output = Value::String(String::new());
        }
        let mut judgment = judgment();
        judgment.incidents[0].evidence[1].excerpt = "ok".to_owned();
        judgment.incidents[0].evidence[1].supports =
            "The recovery check completed successfully with no output".to_owned();

        let issues = validate_judgment(&trace, &judgment);
        assert!(issues.is_valid(), "{:?}", issues.fatal);
    }

    #[test]
    fn causal_unknowns_do_not_block_an_otherwise_grounded_keep() {
        let trace = trace(Completeness::Complete);
        let mut judgment = judgment();
        judgment.incidents[0].cause = Cause::Unknown;
        judgment.incidents[0].causal_unknowns =
            vec!["The trace begins after the cwd changed".to_owned()];

        let issues = validate_judgment(&trace, &judgment);
        assert!(issues.is_valid(), "{:?}", issues.fatal);
    }

    #[test]
    fn keep_allows_follow_up_but_not_pending_decision_checks() {
        let trace = trace(Completeness::Complete);
        let mut follow_up = judgment();
        follow_up.rerun.follow_up = vec!["Investigate why cwd changed".to_owned()];
        assert!(validate_judgment(&trace, &follow_up).is_valid());

        let mut inspect = judgment();
        inspect.rerun.inspect = vec!["Check an artifact".to_owned()];
        assert!(has_fatal(
            &validate_judgment(&trace, &inspect),
            "keep requires empty inspect and prerequisites"
        ));

        let mut prerequisite = judgment();
        prerequisite.rerun.prerequisites = vec!["Reset the workspace".to_owned()];
        assert!(has_fatal(
            &validate_judgment(&trace, &prerequisite),
            "keep requires empty inspect and prerequisites"
        ));
    }

    #[test]
    fn detected_status_requires_a_detection_event() {
        let trace = trace(Completeness::Complete);
        let mut judgment = judgment();
        let incident = &mut judgment.incidents[0];
        incident.status = IncidentStatus::Detected;
        incident.lifecycle.detected = None;
        incident.lifecycle.recovery_started = None;
        incident.lifecycle.recovery_confirmed = None;
        judgment.summary.trajectory = Trajectory::UnrecoveredDetour;
        judgment.rerun.recommendation = Recommendation::Inspect;
        judgment.rerun.inspect = vec!["Inspect the current cwd".to_owned()];

        let issues = validate_judgment(&trace, &judgment);
        assert!(has_fatal(&issues, "requires a detected event"));
    }

    #[test]
    fn corrected_agent_message_can_start_recovery_before_later_observation() {
        let mut trace = trace(Completeness::Complete);
        trace.events.insert(
            1,
            TraceRecord::Message {
                id: "m2".to_owned(),
                actor: "agent".to_owned(),
                text: "Changed directory back to /repo".to_owned(),
                at: None,
                duration_ms: None,
                extensions: BTreeMap::new(),
            },
        );
        let mut judgment = judgment();
        judgment.incidents[0].lifecycle.recovery_started = Some("m2".to_owned());
        judgment.incidents[0]
            .affected_events
            .insert(1, "m2".to_owned());

        let issues = validate_judgment(&trace, &judgment);
        assert!(issues.is_valid(), "{:?}", issues.fatal);
    }

    #[test]
    fn every_started_recovery_references_an_agent_action() {
        let trace = trace(Completeness::Complete);
        let mut recovering = judgment();
        let incident = &mut recovering.incidents[0];
        incident.status = IncidentStatus::Recovering;
        incident.lifecycle.recovery_started = Some("r1".to_owned());
        incident.lifecycle.recovery_confirmed = None;
        recovering.summary.trajectory = Trajectory::UnrecoveredDetour;
        recovering.rerun.recommendation = Recommendation::Inspect;
        recovering.rerun.inspect = vec!["Observe whether recovery completes".to_owned()];

        let issues = validate_judgment(&trace, &recovering);
        assert!(has_fatal(&issues, "must reference an agent message"));
    }

    #[test]
    fn task_update_has_a_first_class_evidence_basis() {
        let event = TraceRecord::TaskUpdate {
            id: "u1".to_owned(),
            actor: "user".to_owned(),
            mode: crate::model::TaskUpdateMode::Clarify,
            text: "Do not change generated files".to_owned(),
            at: None,
            extensions: BTreeMap::new(),
        };
        assert!(basis_matches_event(
            EvidenceBasis::TaskUpdateObservation,
            &event
        ));
        assert!(!basis_matches_event(EvidenceBasis::AgentReport, &event));
    }

    #[test]
    fn partial_trace_and_unresolved_severity_block_overconfident_keep() {
        let partial = trace(Completeness::Partial);
        let mut clean = judgment();
        clean.summary.trajectory = Trajectory::Clean;
        clean.incidents.clear();
        let issues = validate_judgment(&partial, &clean);
        assert!(has_fatal(&issues, "high-confidence clean"));
        assert!(has_fatal(&issues, "high-confidence keep"));

        let complete = trace(Completeness::Complete);
        let mut risky = judgment();
        risky.summary.trajectory = Trajectory::UnrecoveredDetour;
        risky.incidents[0].status = IncidentStatus::Unrecovered;
        risky.incidents[0].severity = Severity::High;
        risky.incidents[0].lifecycle.recovery_confirmed = None;
        let issues = validate_judgment(&complete, &risky);
        assert!(has_fatal(&issues, "keep recommendation is forbidden"));
    }

    #[test]
    fn keep_is_a_strict_allowlist_not_a_model_assertion() {
        let base_trace = trace(Completeness::Complete);

        let mut low_confidence = judgment();
        low_confidence.summary.confidence = Confidence::Medium;
        assert!(has_fatal(
            &validate_judgment(&base_trace, &low_confidence),
            "keep requires high judge confidence"
        ));

        let mut gap = judgment();
        gap.gaps.push("unknown provenance".to_owned());
        assert!(has_fatal(
            &validate_judgment(&base_trace, &gap),
            "evidence gaps"
        ));

        let mut residual_risk = judgment();
        residual_risk.incidents[0].task_effect = TaskEffect::ArtifactRisk;
        assert!(has_fatal(
            &validate_judgment(&base_trace, &residual_risk),
            "residual-risk"
        ));

        let mut unverified_trace = trace(Completeness::Complete);
        if let TraceRecord::Session { source, .. } = &mut unverified_trace.header {
            source.as_mut().unwrap().trust = SourceTrust::Unverified;
        }
        assert!(has_fatal(
            &validate_judgment(&unverified_trace, &judgment()),
            "adapter_asserted"
        ));
    }

    #[test]
    fn recovery_phases_cannot_collapse_into_one_event() {
        let trace = trace(Completeness::Complete);
        let mut collapsed = judgment();
        collapsed.incidents[0].lifecycle.detected = Some("c1".to_owned());
        collapsed.incidents[0].lifecycle.recovery_started = Some("c1".to_owned());
        collapsed.incidents[0].lifecycle.recovery_confirmed = Some("c1".to_owned());

        let issues = validate_judgment(&trace, &collapsed);
        assert!(has_fatal(&issues, "must follow detected"));
        assert!(has_fatal(&issues, "must follow recovery_started"));
    }
}
