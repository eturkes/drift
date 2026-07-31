# Roadmap

## Delivered

- [x] Generic JSONL trace/schema + bounded streaming validation.
- [x] Codex observer protocol + prompt-injection separation + constrained structured output.
- [x] Grounded incident/recovery/outcome + conservative KEEP validation; self-contained report.
- [x] Deterministic incident-coverage ledger + decision-first, revalidating renderer.
- [x] Sparse and trace-recorded-recovery cwd examples + live Codex smoke coverage.
- [x] Codex `exec --json` adapter + import CLI, source digest, task-contract flags, and fixtures.
- [x] Pinned MSRV CI for formatting, tests, and warning-free Clippy.

## Next when demanded by real traces

- [ ] Native adapters for additional concrete agent export formats; fixtures from each producer.
- [ ] Codex rollout/direct-run ingestion when it adds task capture or evidence absent from exec JSONL.
- [ ] Signed/verified adapter provenance for conclusions stronger than trace-recorded evidence.
- [ ] Hierarchical/chunked observation beyond the bounded single-context request.
- [ ] Artifact/state provenance edges for precise contaminated-output rerun manifests.
- [ ] Calibrated human-labeled corpus; measure observer precision, recall, and rerun-decision utility.
