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
        serde_json::json!(["project.tree", "drift.render"])
    );
    assert!(ENTRY.len() < 100_000);
}

#[test]
fn plugin_entry_uses_v1_channel_and_native_render_contract() {
    for required in [
        "event.source !== window.parent",
        "type !== \"in-progress:init\"",
        "apiVersion !== API_VERSION",
        "kind: \"ready\"",
        "const REQUIRED = [\"project.tree\", \"drift.render\"]",
        "call(\"project.tree\", { depth: 6, limit: 2_000 })",
        "call(\"drift.render\", { path })",
        "value.path !== expectedPath",
        "value.text",
        "output.textContent = report.text",
    ] {
        assert!(
            ENTRY.contains(required),
            "missing contract marker: {required}"
        );
    }
    for forbidden in ["<script src=", "<link rel=", "@import", "url(http"] {
        assert!(
            !ENTRY.to_ascii_lowercase().contains(forbidden),
            "external asset marker: {forbidden}"
        );
    }
}
