# Project memory

- Intent = post-hoc agent-session observability → human decides keep/inspect/rerun; scores stay secondary/absent.
- Canonical input = strict ordered `drift.trace/v1` JSONL; adapters normalize agent-native logs.
- Semantic layer = private-temp/read-only `codex exec`; known tools/context requested off, while
  ambient authenticated runtime/system layers remain → hostile input needs outer isolation.
- Trusted rubric bytes stay immutable; trace, rejected output, + retry diagnostics remain untrusted
  user JSON.
- Trust boundary = parser + output schema + grounding/lifecycle/strict-KEEP validator. “Observed” =
  trace-recorded, conditional on adapter fidelity; `adapter_asserted` is metadata, not attestation.
- Report embeds normalized trace + hashes + deterministic incident-coverage ledger; `render` revalidates
  internal consistency, not authorship.
- Verify = `cargo fmt --all -- --check && cargo test --all-targets && cargo clippy --all-targets -- -D warnings`.
