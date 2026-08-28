# Drift

Drift turns an agent-session trace into a source-linked execution postmortem. It answers: what
diverted the run, how much recorded work the diversion touched, what recovery is actually present
in the trace, and what remains worth checking before keeping or rerunning the work.

There is no composite score. The primary output is an incident ledger, lifecycle, evidence gaps,
and judge-attributed incident coverage. Codex supplies semantic classification through `codex
exec`; local Rust code owns parsing, structural grounding, conservative decision gates, provenance,
and rendering.

Illustrative output - wording and `RERUN` versus `INSPECT` can vary with the Codex model:

```text
RERUN (conditional on trace fidelity) - A clean run can record the missing CI result.

Session: cwd-quote
Task: Check CI for the repository.
Evidence ceiling: outcome=unknown | trajectory=unknown | judge-confidence=high |
  declared-completeness=partial | source-trust=unverified

Decision-limiting gaps
  - No tool, state, or outcome record confirms recovery or CI.

Judge-attributed incident coverage: 1/1 session events | 0/0 agent tool calls | 0/0 tool results |
  duration unavailable

Incidents (1)
  1. [recovery-intended | unknown | incidental-detour] Reported cwd drift
     Lifecycle: onset=? -> detected=e1 -> recovery=? -> confirmed=?
     Evidence e1 [agent report; message by agent]: excerpt=cwd had drifted...
```

## Use

Prerequisites: Rust 1.88+ and an authenticated Codex CLI. Observer and import protocols are
compatibility-tested with Codex CLI 0.146.0 and deliberately fail closed when strict flags or JSONL
shapes change.

```sh
cargo build --release
target/release/drift import codex-exec --task "Inspect the working directory." \
  -o trace.jsonl examples/codex-exec.source.jsonl
target/release/drift import codex-session -o recent.drift.jsonl path/to/rollout.jsonl
target/release/drift validate examples/cwd-quote.jsonl
target/release/drift analyze examples/cwd-quote.jsonl
target/release/drift analyze --model MODEL -o report.json examples/cwd-recovered.jsonl
target/release/drift render report.json
```

`import` prints strict trace JSONL or atomically writes it with `-o`. `analyze` prints the human
report. `--json` prints report JSON; `-o` atomically writes that same self-contained report. File
outputs use mode `0600` on Unix. `render` revalidates embedded trace records, semantic invariants,
provenance hashes, judgment hash, and derived coverage before printing. `drift schema
trace|judgment|report` prints a standalone JSON Schema. Input paths accept `-` for stdin.

Exit status reports whether Drift completed successfully, not whether the advisory result is
`KEEP`, `RERUN`, `INSPECT`, or `UNKNOWN`.

## Codex exec import

Capture the structured stdout stream from a non-interactive Codex turn, then import it with the
original task and any decision-relevant contract:

```sh
task='Inspect the repository state and report the test result.'
codex exec --json "$task" > run.codex.jsonl
target/release/drift import codex-exec \
  --task "$task" \
  --constraint 'Preserve the worktree.' \
  --success 'Report the executed test and its result.' \
  -o run.drift.jsonl \
  run.codex.jsonl
target/release/drift analyze run.drift.jsonl
```

Or stream without intermediate files:

```sh
codex exec --json "$task" \
  | target/release/drift import codex-exec --task "$task" - \
  | target/release/drift analyze -
```

The adapter preserves source item IDs/phases in `extensions`, hashes the exact Codex stream, links
started/completed calls, and maps:

| Codex item/event | Drift record |
|---|---|
| agent message, reasoning | agent `message` |
| command, file change, MCP, collaboration, web search | linked `tool_call` + `tool_result` |
| todo list | agent `message` |
| item or stream error | environment `state` |
| `turn.failed` | failure `outcome` |
| `turn.completed` | unknown `outcome` |

`turn.completed` proves only that Codex finished the turn; it cannot establish task success. A
terminal stream is structurally `complete`; a captured stream without a terminal turn event remains
`partial`, including unmatched-call warnings. Unknown event/item variants, unknown fields,
duplicate keys, changed call-identity payloads, and impossible lifecycles are rejected rather than
omitted.

The task/contract comes from CLI arguments because `codex exec --json` does not include the original
prompt. The source digest detects later byte changes but is not authentication.

## Persisted Codex session import

`codex-session` converts one local Codex rollout without task flags: the first visible user message
becomes the task. Visible user/assistant `event_msg` records and `response_item` tool calls/results
are retained. Developer/system messages, compaction payloads, encrypted reasoning, and unrecognized
record families are excluded; counts of excluded record families remain in the session extension.

```sh
target/release/drift import codex-session \
  -o recent.drift.jsonl \
  path/to/rollout-TIMESTAMP-SESSION_ID.jsonl
target/release/drift validate recent.drift.jsonl
```

The adapter accepts interrupted rollouts as `partial`, including linkage warnings. A final Codex
`task_complete` makes a fully-linked trace structurally `complete` and adds an `unknown` outcome:
agent transport completion still does not establish task success. Unknown record families are
omitted rather than interpreted, while malformed imported message/tool records fail closed.

## Generic trace JSONL

Line 1 is one session header. Each following line is one ordered event with a stable unique ID.
Line order is authoritative; `at` timestamps are optional metadata.

```jsonl
{"kind":"session","schema":"drift.trace/v1","id":"run-42","task":"Check CI.","completeness":"partial","source":{"adapter":"my-agent","adapter_version":"1","trust":"unverified"}}
{"kind":"tool_call","id":"e1","actor":"agent","call_id":"c1","name":"shell","input":{"cmd":"pwd"}}
{"kind":"tool_result","id":"e2","actor":"tool","call_id":"c1","status":"ok","output":"/repo/.scratch/m41a"}
{"kind":"message","id":"e3","actor":"agent","text":"cwd had drifted. Let me fix that."}
```

Core record kinds:

| Kind | Required payload | Allowed actor role |
|---|---|---|
| `session` | `schema`, `id`, `task`; optional contract, source, completeness | n/a |
| `message` | `id`, `actor`, `text` | agent, user, system, environment |
| `tool_call` | `id`, `actor`, unique `call_id`, `name`, arbitrary JSON `input` | agent, system |
| `tool_result` | `id`, `actor`, matching `call_id`, `status`, arbitrary JSON `output` | tool, system, environment |
| `state` | `id`, `actor`, JSON object `values` | tool, system, environment |
| `task_update` | `id`, `actor`, `mode = add|replace|clarify`, `text` | user, system |
| `outcome` | `id`, `actor`, `status`, `text`; when present, last event | tool, system, environment, user |

Messages, calls, and results also accept `duration_ms`. Adapter-specific data belongs under
`extensions`. Unknown top-level fields are rejected so adapter/schema disagreement fails visibly.

An actor is a role or `role:identity`, such as `agent:reviewer`. Kind-specific roles prevent an
agent-authored `state` or `outcome` object from being treated as a harness observation.

Set `completeness` deliberately:

- `complete` - known full boundary; unmatched calls/results are errors.
- `partial` - known omissions; linkage gaps are warnings and strong clean/keep claims are blocked.
- `unknown` - boundary not established; treated at least as conservatively as partial.

If `source` is present, `adapter`, `adapter_version`, and `trust` are required.
`trust: adapter_asserted` means the adapter asserts faithful structural normalization and known trace
boundaries. It is not a signature or cryptographic attestation. Omit the source or use `unverified`
for hand-authored, lossy, or unknown-provenance traces. Local `KEEP` validation requires
`adapter_asserted`; every conclusion still remains conditional on trace fidelity.

Adapter responsibilities:

- preserve event order and stable producer IDs;
- map agent actions, tool observations, state, task updates, and harness outcomes to the correct
  kinds and actors;
- mark completeness from producer boundaries rather than guesswork;
- use `outcome` for task-level harness/user disposition, not mere command transport success;
- redact before emitting JSONL when the provider must not receive a field.

See [trace.schema.json](schemas/trace.schema.json) and [examples](examples).

## Observation model

The [rubric](.codex/prompts/observe.md) distinguishes incidental detours, task-intrinsic
troubleshooting, legitimate exploration, and external friction. Each incident carries:

- category, relation to task, cause, severity, and task effect;
- onset, detection, recovery action, and later recovery check;
- a chronological affected-event timeline plus exact source excerpts and evidence bases;
- causal unknowns, decision-relevant unknowns, and rerun relevance.

“I will fix it” is recovery intent. “I fixed it” is still an agent report. `recovered` requires a
detected incident, a later recovery action, and a still-later trace-recorded tool/state/outcome
observation. `trace_supported_success` requires a cited final non-agent success `outcome`; an
ordinary `tool_result(status=ok)` proves only that invocation completed successfully.
`completed` records bounded intrinsic troubleshooting, legitimate exploration, or ended external
friction without inventing a recovery lifecycle. Task changes have their own
`task_update_observation` evidence basis.

The JSON field remains named `burden`, but its values are deterministic incident-touched coverage
after the model selects `affected_events`: unique attributed session events, agent tool calls, tool
results, supplied durations, and inclusive ordered episode spans. It measures episode extent, not
pure off-task overhead, and does not pretend unlike harms belong on one numeric scale.

`KEEP` is a strict local allowlist: complete adapter-asserted trace, final success outcome, high
judge confidence, clean or fully recovered trajectory, no decision-relevant gaps/unknowns/parser
warnings, and no unresolved, high/critical, constraint/safety, state-risk, artifact-risk, or
outcome-risk incident. Missing causal history may remain explicit without forcing a rerun; it is
reported separately. Codex cannot bypass this gate by merely writing `"recommendation":"keep"`.

## Report and trust boundaries

The report embeds the normalized session header and every event, plus raw-input and normalized
digests, embedded rubric/judgment schema with exact hashes, Codex thread/CLI/model-request
provenance, judgment hash, and the derived incident-coverage ledger. This makes a report
independently inspectable and internally revalidatable. Hashes detect accidental or naive
alteration; they do not authenticate a malicious report author, because no signing key is involved.

Local validation proves structural grounding, not semantic truth. It checks unique JSON keys,
resource bounds, event/call linkage, kind-specific actors, exact excerpts, evidence-kind matches,
strict recovery ordering, outcome consistency, conservative `KEEP`, and all cross-references. Codex
can still miss an incident, choose the wrong affected span, or attach a grounded excerpt to a weak
interpretation. Judge confidence is uncalibrated model self-assessment and is labeled accordingly.

Trace fields are prompt-untrusted. Drift keeps the rubric byte-identical across retries and sends
validation feedback only inside the untrusted JSON user envelope. It runs Codex in a private
temporary working directory with a read-only sandbox, ignores `config.toml` and rules, asks Codex to
omit project/environment/app/skill context, disables known tool surfaces, strictly parses one
thread/turn/final-message JSONL sequence, and bounds:

- trace: 32 MiB total, 4 MiB per line, 50,000 events;
- Codex request/output: 8 MiB each;
- attempts: 1-3; timeout: 1-7,200 seconds per attempt.

On Unix, Drift places ordinary Codex descendants in a process group and makes a best-effort group
termination at the configured deadline or output limit. A descendant that deliberately escapes the
group and retains a captured pipe can outlive that deadline; use an outer process supervisor or
container when a hard hostile-input deadline is required. Equivalent process-tree termination is
not guaranteed on non-Unix platforms.

Codex CLI does not expose a universal no-tools switch. The existing authenticated Codex runtime,
`CODEX_HOME`, environment, and provider/system layers still exist; ignored user config and the
isolated/read-only working directory are defense in depth, not a hostile multi-tenant sandbox. Use
an outer container or API call with an empty tool set for adversarial deployment.

The normalized trace is sent to the provider configured for Codex, and report excerpts/full events
retain its content. Redact secrets, personal data, and regulated data before `analyze`; protect the
report accordingly. When `--model` is absent, provenance says `unknown (Codex CLI default)` because
the CLI protocol does not report the resolved model.

## in-progress view

[`plugin/`](plugin/) is a zero-build, self-contained in-progress API 1.0 report view. Configure that
exact directory as a plugin root. It combines repository candidates from `project.tree` with private
external paths from `drift.workspace`. The host retains only candidates accepted by native
`drift validate`. It can render a selected report or analyze one selected valid trace after trusted
confirmation. The iframe receives paths, validity, and rendered text—never raw trace/report JSON.

Discovery calls `project.tree` with `{ "depth": 6, "limit": 2000 }`. Repository candidates are
regular JSON files under `.drift/`, in a `reports/` directory, or with `drift`/`report` in the
filename. External virtual paths use `.in-progress/drift/{traces,reports}/...`; they never resolve
beneath the repository. Rendering uses this RPC:

```json
{"kind": "request", "id": "…", "method": "drift.render", "params": {"path": "session.drift.json"}}
```

The successful result is exactly `{ "path": "session.drift.json", "text": "native output…" }`.
The host must bind the project root and accept only a tree-returnable or selected-project virtual
regular-file path. It must reject absolute, traversing, and symlink-escaping paths. It invokes
`drift render` directly without shell interpolation. Return output only after exit zero; cap input,
output, and time. Safe RPC errors are
`Invalid Drift report path`, `Drift report not found`, `Drift report is too large`, `Drift render
timed out`, `Drift report validation failed`, or `Drift executable unavailable`. The UI surfaces
errors without branching on their text and inserts native output with `textContent`.

Discovery submits at most 32 unique repository or virtual candidates through read-only
`drift.validateTraces`. The host resolves each regular file, runs bounded native validation, and
returns only valid paths. Arbitrary JSONL, including Align journals and raw Codex streams, never
enables analysis.

`drift.recentSessions` lists bounded metadata for matching local Codex sessions. Confirmed
`drift.importSession` publishes a mode-`0600` virtual trace under external private state. Import is
local and model-free. It cannot create `.drift` in the repository.

`Analyze trace` submits only `{ "path": "trace.jsonl" }` through `drift.analyze`. The trusted host
validates that exact request, then confirms stable plugin/project identity, trace disclosure to the
Codex provider, ChatGPT subscription use, and deterministic external create/replace output.
After approval, the host fixes configured Drift/Codex executables, `gpt-5.6-sol`, argv, environment,
deadline, model attempts, and `.in-progress/drift/reports/<trace>-<digest>.drift.json` virtual
destination. It resolves and prevalidates the regular input before creating output directories, rejects unsafe
output directories/files, serializes analyses per canonical project, runs native `analyze`, then
native `render`; the result uses the same
`{ "path", "text" }` shape. A disconnect leaves the admitted analysis running; rescan discovers a
completed report. Drift validates before provider use, but approved trace content reaches the Codex
provider and remains embedded in the mode-`0600` report.

Validate the static package from an in-progress checkout:

```sh
pnpm plugin:validate -- /absolute/path/to/drift/plugin
```

## Development

```sh
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo deny check
```

The interchange is intentionally smaller than current AI telemetry conventions. OpenAI describes
trace grading as structured evaluation of end-to-end decisions and tool calls, while
[OpenInference](https://arize-ai.github.io/openinference/spec/) and the developmental
[OpenTelemetry GenAI conventions](https://github.com/open-telemetry/semantic-conventions-genai)
remain useful future adapter targets without becoming the core contract.
