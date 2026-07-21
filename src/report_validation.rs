use std::io::{BufReader, Cursor, Write};

use crate::{
    aggregate::{REPORT_SCHEMA, derive_burden},
    digest::{canonical_digest, normalized_trace_digest},
    error::{DriftError, Result},
    model::{Judgment, Report, Trace},
    parse::parse_trace,
    validate::validate_judgment,
};

/// Revalidate every internally checkable report invariant before presentation.
///
/// This proves consistency with the embedded trace, not trace authenticity.
pub fn validate_report(report: &Report) -> Result<()> {
    if report.schema != REPORT_SCHEMA {
        return Err(DriftError::new(
            "E_REPORT_SCHEMA",
            format!("unsupported report schema {:?}", report.schema),
        ));
    }
    for (label, digest) in [
        ("trace.input_digest", report.trace.input_digest.as_str()),
        (
            "trace.normalized_digest",
            report.trace.normalized_digest.as_str(),
        ),
        (
            "analysis.rubric_digest",
            report.analysis.rubric_digest.as_str(),
        ),
        (
            "analysis.judgment_schema_digest",
            report.analysis.judgment_schema_digest.as_str(),
        ),
        (
            "analysis.judgment_digest",
            report.analysis.judgment_digest.as_str(),
        ),
    ] {
        if !is_sha256(digest) {
            return Err(DriftError::new(
                "E_REPORT_INTEGRITY",
                format!("{label} is not a lowercase sha256 digest"),
            ));
        }
    }
    if report.analysis.rubric.trim().is_empty()
        || report.analysis.rubric_source.trim().is_empty()
        || report.analysis.rubric_source.len() > 256 * 1024
        || report.analysis.judgment_schema_source.len() > 1024 * 1024
        || crate::digest::sha256_bytes(report.analysis.rubric_source.as_bytes())
            != report.analysis.rubric_digest
        || crate::digest::sha256_bytes(report.analysis.judgment_schema_source.as_bytes())
            != report.analysis.judgment_schema_digest
        || !serde_json::from_str::<serde_json::Value>(&report.analysis.judgment_schema_source)
            .is_ok_and(|schema| schema.is_object())
    {
        return Err(DriftError::new(
            "E_REPORT_PROVENANCE",
            "embedded rubric or judgment-schema source does not match its provenance",
        ));
    }
    if report.analysis.drift_version.trim().is_empty()
        || report.analysis.codex_cli.trim().is_empty()
        || report.analysis.judge.trim().is_empty()
        || report.analysis.model.trim().is_empty()
        || report.analysis.thread_id.trim().is_empty()
        || !(1..=3).contains(&report.analysis.attempts)
    {
        return Err(DriftError::new(
            "E_REPORT_PROVENANCE",
            "report analysis provenance is incomplete or invalid",
        ));
    }

    let trace = reconstruct_trace(report)?;
    if normalized_trace_digest(&trace)? != report.trace.normalized_digest {
        return Err(DriftError::new(
            "E_REPORT_INTEGRITY",
            "embedded normalized trace does not match trace.normalized_digest",
        ));
    }
    let judgment = report_judgment(report);
    if canonical_digest(&judgment)? != report.analysis.judgment_digest {
        return Err(DriftError::new(
            "E_REPORT_INTEGRITY",
            "embedded judgment does not match analysis.judgment_digest",
        ));
    }
    let issues = validate_judgment(&trace, &judgment);
    if !issues.fatal.is_empty() {
        return Err(DriftError::new(
            "E_REPORT_INVALID",
            format!("report judgment is invalid: {}", issues.fatal.join("; ")),
        ));
    }
    if issues.warnings != report.validation_warnings {
        return Err(DriftError::new(
            "E_REPORT_INTEGRITY",
            "validation_warnings do not match deterministic revalidation",
        ));
    }
    if derive_burden(&trace, &report.incidents) != report.burden {
        return Err(DriftError::new(
            "E_REPORT_INTEGRITY",
            "burden ledger does not match the embedded trace and incidents",
        ));
    }
    Ok(())
}

fn reconstruct_trace(report: &Report) -> Result<Trace> {
    let mut jsonl = Vec::new();
    for record in std::iter::once(&report.trace.session).chain(&report.trace.events) {
        serde_json::to_writer(&mut jsonl, record).map_err(|error| {
            DriftError::new(
                "E_REPORT_INVALID",
                format!("serialize embedded trace record: {error}"),
            )
        })?;
        jsonl
            .write_all(b"\n")
            .map_err(|error| DriftError::io("serialize embedded trace", error))?;
    }
    let parsed = parse_trace(BufReader::new(Cursor::new(jsonl))).map_err(|error| {
        DriftError::new(
            "E_REPORT_INVALID",
            format!("embedded trace violates drift.trace/v1: {error}"),
        )
    })?;
    if parsed.header != report.trace.session
        || parsed.events != report.trace.events
        || parsed.warnings != report.trace.parser_warnings
    {
        return Err(DriftError::new(
            "E_REPORT_INTEGRITY",
            "embedded trace or parser warnings changed during canonical revalidation",
        ));
    }
    Ok(Trace {
        header: parsed.header,
        events: parsed.events,
        input_digest: report.trace.input_digest.clone(),
        warnings: parsed.warnings,
    })
}

fn report_judgment(report: &Report) -> Judgment {
    Judgment {
        schema: "drift.judgment/v1".to_owned(),
        overview: report.overview.clone(),
        summary: report.summary.clone(),
        rerun: report.rerun.clone(),
        incidents: report.incidents.clone(),
        gaps: report.gaps.clone(),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{is_sha256, report_judgment, validate_report};
    use crate::{
        aggregate::{AnalysisRun, aggregate_report},
        digest::canonical_digest,
        model::{
            Completeness, Confidence, JudgedOutcome, Judgment, OutcomeStatus, Recommendation,
            RerunAssessment, Source, SourceTrust, Summary, Trace, TraceRecord, Trajectory,
        },
        observer::{
            RUBRIC_VERSION, judgment_schema_digest, judgment_schema_source, rubric_digest,
            rubric_source,
        },
    };

    #[test]
    fn accepts_only_canonical_sha256_spelling() {
        assert!(is_sha256(&format!("sha256:{}", "a".repeat(64))));
        assert!(!is_sha256("sha256:abc"));
        assert!(!is_sha256(&format!("sha256:{}", "A".repeat(64))));
    }

    fn report() -> crate::model::Report {
        let trace = Trace {
            header: TraceRecord::Session {
                schema: "drift.trace/v1".to_owned(),
                id: "s1".to_owned(),
                task: "finish".to_owned(),
                constraints: vec![],
                success_criteria: vec!["finish".to_owned()],
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
                id: "o1".to_owned(),
                actor: "system".to_owned(),
                status: OutcomeStatus::Success,
                text: "finished".to_owned(),
                at: None,
                extensions: BTreeMap::new(),
            }],
            input_digest: format!("sha256:{}", "a".repeat(64)),
            warnings: vec![],
        };
        let judgment = Judgment {
            schema: "drift.judgment/v1".to_owned(),
            overview: "Finished cleanly".to_owned(),
            summary: Summary {
                task_outcome: JudgedOutcome::TraceSupportedSuccess,
                outcome_reason: "Final success outcome".to_owned(),
                outcome_evidence: vec!["o1".to_owned()],
                trajectory: Trajectory::Clean,
                trajectory_reason: "No incident".to_owned(),
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
        };
        aggregate_report(
            &trace,
            judgment,
            AnalysisRun {
                rubric: RUBRIC_VERSION.to_owned(),
                rubric_digest: rubric_digest(),
                rubric_source: rubric_source().to_owned(),
                judgment_schema_digest: judgment_schema_digest(),
                judgment_schema_source: judgment_schema_source().to_owned(),
                judge: "codex exec".to_owned(),
                codex_cli: "codex-cli test".to_owned(),
                model: "test".to_owned(),
                thread_id: "thread-1".to_owned(),
                attempts: 1,
            },
        )
        .unwrap()
    }

    #[test]
    fn accepts_untampered_self_contained_report() {
        validate_report(&report()).unwrap();
    }

    #[test]
    fn rejects_rehashed_but_semantically_impossible_keep() {
        let mut report = report();
        report.summary.task_outcome = JudgedOutcome::Unknown;
        report.summary.outcome_evidence.clear();
        report.analysis.judgment_digest = canonical_digest(&report_judgment(&report)).unwrap();

        let error = validate_report(&report).unwrap_err();
        assert_eq!(error.code, "E_REPORT_INVALID");
        assert!(
            error
                .message
                .contains("keep requires trace_supported_success")
        );
    }

    #[test]
    fn rejects_tampered_trace_and_derived_burden() {
        let mut trace_tamper = report();
        if let TraceRecord::Outcome { text, .. } = &mut trace_tamper.trace.events[0] {
            *text = "tampered".to_owned();
        }
        assert_eq!(
            validate_report(&trace_tamper).unwrap_err().code,
            "E_REPORT_INTEGRITY"
        );

        let mut burden_tamper = report();
        burden_tamper.burden.attributed_event_count = 99;
        assert_eq!(
            validate_report(&burden_tamper).unwrap_err().code,
            "E_REPORT_INTEGRITY"
        );
    }
}
