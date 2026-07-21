use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Extensions = BTreeMap<String, Value>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub adapter: String,
    pub adapter_version: String,
    pub trust: SourceTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceTrust {
    AdapterAsserted,
    #[default]
    Unverified,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TraceRecord {
    Session {
        schema: String,
        id: String,
        task: String,
        #[serde(default)]
        constraints: Vec<String>,
        #[serde(default)]
        success_criteria: Vec<String>,
        #[serde(default)]
        completeness: Completeness,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<Source>,
        #[serde(default)]
        extensions: Extensions,
    },
    Message {
        id: String,
        actor: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default)]
        extensions: Extensions,
    },
    ToolCall {
        id: String,
        actor: String,
        call_id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default)]
        extensions: Extensions,
    },
    ToolResult {
        id: String,
        actor: String,
        call_id: String,
        status: ToolStatus,
        output: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default)]
        extensions: Extensions,
    },
    State {
        id: String,
        actor: String,
        values: BTreeMap<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default)]
        extensions: Extensions,
    },
    TaskUpdate {
        id: String,
        actor: String,
        mode: TaskUpdateMode,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default)]
        extensions: Extensions,
    },
    Outcome {
        id: String,
        actor: String,
        status: OutcomeStatus,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(default)]
        extensions: Extensions,
    },
}

impl TraceRecord {
    pub fn id(&self) -> &str {
        match self {
            Self::Session { id, .. }
            | Self::Message { id, .. }
            | Self::ToolCall { id, .. }
            | Self::ToolResult { id, .. }
            | Self::State { id, .. }
            | Self::TaskUpdate { id, .. }
            | Self::Outcome { id, .. } => id,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Session { .. } => "session",
            Self::Message { .. } => "message",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::State { .. } => "state",
            Self::TaskUpdate { .. } => "task_update",
            Self::Outcome { .. } => "outcome",
        }
    }

    pub(crate) fn searchable_text(&self) -> String {
        match self {
            Self::Session { task, .. } => task.clone(),
            Self::Message { text, .. } => text.clone(),
            Self::TaskUpdate { mode, text, .. } => {
                format!("{} {text}", task_update_mode_label(*mode))
            }
            Self::Outcome { status, text, .. } => {
                format!("{} {text}", outcome_status_label(*status))
            }
            Self::ToolCall { name, input, .. } => format!("{name} {}", compact_json(input)),
            Self::ToolResult { status, output, .. } => {
                format!("{} {}", tool_status_label(*status), compact_json(output))
            }
            Self::State { values, .. } => serde_json::to_string(values).unwrap_or_default(),
        }
    }
}

fn task_update_mode_label(mode: TaskUpdateMode) -> &'static str {
    match mode {
        TaskUpdateMode::Add => "add",
        TaskUpdateMode::Replace => "replace",
        TaskUpdateMode::Clarify => "clarify",
    }
}

fn outcome_status_label(status: OutcomeStatus) -> &'static str {
    match status {
        OutcomeStatus::Success => "success",
        OutcomeStatus::Partial => "partial",
        OutcomeStatus::Failure => "failure",
        OutcomeStatus::Unknown => "unknown",
    }
}

fn tool_status_label(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
        ToolStatus::Unknown => "unknown",
    }
}

fn compact_json(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Ok,
    Error,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskUpdateMode {
    Add,
    Replace,
    Clarify,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Success,
    Partial,
    Failure,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Trace {
    pub header: TraceRecord,
    pub events: Vec<TraceRecord>,
    pub input_digest: String,
    pub warnings: Vec<String>,
}

impl Trace {
    pub fn id(&self) -> &str {
        self.header.id()
    }

    pub fn task(&self) -> &str {
        match &self.header {
            TraceRecord::Session { task, .. } => task,
            _ => unreachable!("validated traces always have a session header"),
        }
    }

    pub fn completeness(&self) -> Completeness {
        match &self.header {
            TraceRecord::Session { completeness, .. } => *completeness,
            _ => unreachable!("validated traces always have a session header"),
        }
    }

    pub fn source(&self) -> Option<&Source> {
        match &self.header {
            TraceRecord::Session { source, .. } => source.as_ref(),
            _ => None,
        }
    }

    pub fn event(&self, id: &str) -> Option<&TraceRecord> {
        self.events.iter().find(|event| event.id() == id)
    }

    pub fn event_position(&self, id: &str) -> Option<usize> {
        self.events.iter().position(|event| event.id() == id)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JudgedOutcome {
    TraceSupportedSuccess,
    ClaimedSuccess,
    Partial,
    Failure,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Trajectory {
    Clean,
    RecoveredDetour,
    UnrecoveredDetour,
    Derailed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Keep,
    Rerun,
    Inspect,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub task_outcome: JudgedOutcome,
    pub outcome_reason: String,
    pub outcome_evidence: Vec<String>,
    pub trajectory: Trajectory,
    pub trajectory_reason: String,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RerunAssessment {
    pub recommendation: Recommendation,
    pub reason: String,
    pub prerequisites: Vec<String>,
    pub inspect: Vec<String>,
    pub follow_up: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncidentCategory {
    ContextState,
    Scope,
    WrongObjective,
    UnrelatedWork,
    ToolExecution,
    LoopChurn,
    Coordination,
    ConstraintOrSafety,
    Other,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRelation {
    IncidentalDetour,
    IntrinsicTroubleshooting,
    LegitimateExploration,
    ExternalFriction,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Agent,
    Environment,
    Tool,
    User,
    External,
    Inherited,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Reported,
    Detected,
    RecoveryIntended,
    Recovering,
    Recovered,
    Completed,
    Unrecovered,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskEffect {
    Delay,
    Rework,
    StateRisk,
    ArtifactRisk,
    OutcomeRisk,
    None,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    #[serde(deserialize_with = "deserialize_nullable")]
    pub onset: Option<String>,
    #[serde(deserialize_with = "deserialize_nullable")]
    pub detected: Option<String>,
    #[serde(deserialize_with = "deserialize_nullable")]
    pub recovery_started: Option<String>,
    #[serde(deserialize_with = "deserialize_nullable")]
    pub recovery_confirmed: Option<String>,
}

fn deserialize_nullable<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBasis {
    AgentAction,
    ToolObservation,
    StateObservation,
    TaskUpdateObservation,
    OutcomeObservation,
    AgentReport,
    UserReport,
    Inference,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub event_id: String,
    pub basis: EvidenceBasis,
    pub supports: String,
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Incident {
    pub id: String,
    pub title: String,
    pub category: IncidentCategory,
    pub task_relation: TaskRelation,
    pub cause: Cause,
    pub severity: Severity,
    pub status: IncidentStatus,
    pub task_effect: TaskEffect,
    pub lifecycle: Lifecycle,
    pub affected_events: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub causal_unknowns: Vec<String>,
    pub unknowns: Vec<String>,
    pub rerun_relevance: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Judgment {
    pub schema: String,
    pub overview: String,
    pub summary: Summary,
    pub rerun: RerunAssessment,
    pub incidents: Vec<Incident>,
    pub gaps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceSnapshot {
    pub input_digest: String,
    pub normalized_digest: String,
    pub parser_warnings: Vec<String>,
    pub session: TraceRecord,
    pub events: Vec<TraceRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AnalysisProvenance {
    pub drift_version: String,
    pub rubric: String,
    pub rubric_digest: String,
    pub rubric_source: String,
    pub judgment_schema_digest: String,
    pub judgment_schema_source: String,
    pub judgment_digest: String,
    pub judge: String,
    pub codex_cli: String,
    pub model: String,
    pub thread_id: String,
    pub generated_at_unix_ms: u128,
    pub attempts: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EventSpan {
    pub first_event_id: String,
    pub last_event_id: String,
    pub inclusive_event_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IncidentBurden {
    pub incident_id: String,
    pub affected_event_count: usize,
    pub affected_agent_action_count: usize,
    pub affected_tool_result_count: usize,
    pub known_duration_ms: u128,
    pub duration_event_count: usize,
    pub span: Option<EventSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Burden {
    pub attributed_event_count: usize,
    pub attributed_agent_action_count: usize,
    pub attributed_tool_result_count: usize,
    pub known_duration_ms: u128,
    pub duration_event_count: usize,
    pub episodes: Vec<IncidentBurden>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: String,
    pub trace: TraceSnapshot,
    pub analysis: AnalysisProvenance,
    pub burden: Burden,
    pub overview: String,
    pub summary: Summary,
    pub rerun: RerunAssessment,
    pub incidents: Vec<Incident>,
    pub gaps: Vec<String>,
    pub validation_warnings: Vec<String>,
}
