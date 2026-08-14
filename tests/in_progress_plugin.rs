use serde_json::Value;

const MANIFEST: &str = include_str!("../plugin/in-progress.plugin.json");
const ENTRY: &str = include_str!("../plugin/index.html");

#[test]
fn plugin_manifest_is_static_and_versioned() {
    let manifest: Value = serde_json::from_str(MANIFEST).expect("manifest JSON");
    assert_eq!(manifest["apiVersion"], "1.0");
    assert_eq!(manifest["id"], "drift");
    assert_eq!(manifest["name"], "Drift");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["entry"], "index.html");
    assert_eq!(manifest["assets"], serde_json::json!([]));
    assert_eq!(
        manifest["capabilities"],
        serde_json::json!([
            "project.tree",
            "drift.render",
            "drift.validateTraces",
            "drift.recentSessions",
            "drift.importSession",
            "drift.analyze"
        ])
    );
    assert!(ENTRY.len() < 375_000);
}

#[test]
fn plugin_entry_uses_v1_channel_and_native_render_contract() {
    for required in [
        "in-progress-protocol:start",
        "InProgressProtocol",
        "in-progress:init",
        "Unsupported or invalid in-progress host API",
        "\"drift.recentSessions\"",
        "\"drift.importSession\"",
        "const MAX_TRACE_CANDIDATES = 32;",
        "const IMPORT_TIMEOUT_MS = 75_000;",
        "const ANALYZE_TIMEOUT_MS = 21 * 60_000;",
        "const ANALYSIS_CANCELED = \"Drift analysis canceled by the user\";",
        "requiredCapabilities: REQUIRED",
        "call(\"project.tree\", { depth: 6, limit: 2_000 })",
        "call(\"drift.render\", { path })",
        "await call(\"drift.validateTraces\", {",
        "call(\"drift.analyze\", { path }, ANALYZE_TIMEOUT_MS)",
        "call(\"drift.recentSessions\")",
        "await call(\"drift.importSession\", { sessionId }, IMPORT_TIMEOUT_MS)",
        "Import recent Codex session",
        "No model or provider is contacted",
        "Analyze trace",
        "Drift trace JSONL",
        "basename.endsWith(\".schema.json\")",
        "basename.endsWith(\".source.jsonl\")",
        "No valid Drift trace found",
        "value.path !== expectedPath",
        "value.text",
        "output.textContent = report.text",
    ] {
        assert!(
            ENTRY.contains(required),
            "missing contract marker: {required}"
        );
    }
    for duplicate in [
        "const API_VERSION",
        "const pending = new Map",
        "function receiveHost",
    ] {
        assert!(
            !ENTRY.contains(duplicate),
            "duplicate transport marker: {duplicate}"
        );
    }
    for forbidden in ["<script src=", "<link rel=", "@import", "url(http"] {
        assert!(
            !ENTRY.to_ascii_lowercase().contains(forbidden),
            "external asset marker: {forbidden}"
        );
    }
}
