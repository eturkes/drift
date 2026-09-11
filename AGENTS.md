# Alignment

## Collaboration

- Ground claims in evidence + state uncertainty. Chat = blockers + essentials; I'm technically proficient.
- During exploratory work, open useful discussions: surface settled context, probe uncertainties, articulate tacit knowledge, examine options/assumptions; offer vocabulary, examples, counterexamples, tradeoffs + testable probes as useful.
- Stay objective; push back on or criticize my ideas when warranted — these are collaborations. Use deduction, first principles, scientific + Socratic methods for root causes; experiments + benchmarks must resolve material uncertainty.
- A failed attempt is evidence: report what it taught; revise the approach or restart when warranted. Novel approaches are welcome where they outperform the default.

## Execution

- Install/configure project-local; work within the launch dir + children.
- Reason, research + execute at full capability through completion; efficiency preserves required scope, depth + verification.
- Use planning + checkpoints when they help the task; revise them as evidence changes. Resume from conversation, working tree + git history; save only context those do not recover.
- Open tooling, method or design choices → research with available search/fetch tools + authenticated browser access where needed. Primary sources + measurements outrank popularity.
- Git: creds in the global gitconfig; authorized change/build work includes all local-repo commands, I handle remote. One commit per cohesive piece, deferred mid-iteration to the closing turn; subject = `<scope>: <cause> → <fix>`, body = measurements + SHAs as payload. Keep `.gitignore` current.

## Authoring

- AI agents = the sole developers → agent-optimized = the default for EVERY text artifact, durable + throwaway alike: reports, scratch notes, code + config comments, internal docs, instruction files, filenames. Write them dense, symbol-forward, human-sparse — telegraphic phrasing, `→`/`=` notation. Aggressively compress whatever you read, however works best. Prune unhelpful, implicit, obsolete, redundant content + structures whenever encountered; route each rule to one owning scope.
- State rules, facts + warnings plainly; omit + prune provenance — dates, verification/discovery events, origin stories.
- Future-facing text, esp. prompts → state the desired action/target positively (`always`/`must`).
- Maintain task-touched instruction + skill files during authorized work; improve them when useful. Route durable guidance to one scope: global `~/.codex/AGENTS.md` = project-independent behavior + Codex environment/tooling + machine capabilities; project/scoped `AGENTS.md` = repo principles + binding rules; `.agents/skills/` = repo workflows.
- Preserve project-specific rules when refreshing templates. Conventions, stack decisions + verification entry points belong in applicable `AGENTS.md`; optional task notes hold changing state.
- UI/UX: unique fonts, cohesive colors/themes, style fitted to project + human audience.
- Human-facing = surfaces a person reads at consumption time: shipped README + docs, UI copy, CLI help…; machine-consumed payload (JSON fields, logs, codes) = code surface. Write it natural + direct in ASD-STE100 register: ≤20 words/sentence in instructions, ≤25 in descriptions; imperative steps, one instruction per sentence, condition before command; simple tenses, finite verbs, active voice, definite modality (`must`); terminology fixed + sentence shape varied; full forms with articles + `that`; flexible enumeration; code + identifiers verbatim.

## Engineering

- Elegant, tightly-scoped modular components; deduplicate; KISS + UNIX where apt; refactor proactively.
- Code = agent-read artifact → concise within three bounds: performant, bug-free, maximally agent-legible. Idiom serves human readers → keep the idiomatic form where it also serves those bounds.
- Comments cost tokens → spend them on the `why` fresh agents would otherwise re-derive every pass: the constraint, measurement, or upstream quirk behind a peculiar decision. Code states the `what` on its own.
- Target sufficient scope, evidence-backed claims, and real success criteria.
- Established methods (TDD red-green-refactor, differential oracles, adversarial review) + practices that measure better than the default; unconventional is fine where it wins.
- Open tooling decisions (language/library/package…) → research (`Execution`) + select for SOTA task/agent fit; my preselection is authoritative. Training overweights human-popular convenience. Library availability alone = insufficient; code is cheap and reimplementation viable. Consider agent-oriented languages (agentlanguages.dev) + AI-targeted tooling. Build on mature work when it is SOTA.
- Within required verification scope, deterministic checks own every rule a tool can decide: linters, type checkers, static analysis, formatters, schema/contract validators; judgment passes spend on what no tool decides. Configure + extend proven checkers first; uncovered required invariant → dedicated check wired into the gate.
- Tests/verification: scope = requested outcome + regression risk + repo posture. Reversible edits with low impact → direct checks; add tests only when meaningful + necessary to verify behavior independently of implementation. Fuzzing/property/formal methods require a task-specific advantage.
- Complete appropriate tests + required checks, then finish delivery. Repeat/broaden verification only for new changes, failures or unresolved concerns; focus checks on that evidence.
- A gate backing a durable claim must rerun from committed state. Keep its implementation or complete regeneration recipe + invocation in tracked code, skills or docs; applicable `AGENTS.md` points to the entry point.
- Repairs to a generated artifact land as one idempotent script replayable from a clean base → the wave stays re-derivable; credit by rerunning to byte-identical output.
- Adversarial review (code or session) → scrutinize correctness + logic, claim soundness, guarantee-vs-claim gaps; weigh honesty + overreach above style. Report every issue, incl. uncertain/low-severity; I filter findings.
- Review terminates on a check set fixed before the diff is read: adjudicate every row, ship the table, count rows adjudicated as the deliverable — an all-`pass` table is a complete review. Findings bind to the change under review; everything outside it reports as a deferred item, and this pass fixes the adjudicated rows alone. An accepted ruling holds until new evidence reverses it, and a fix earns one re-review round against that finding's check alone. Model opinion drifts run to run, so an open-ended review→fix loop flip-flops, creeps scope + injects defects — the fixed set + evidence bar are what make it converge.
- Remotely-exploitable code → highest security standard: periodically audit, update software to latest, verify behavior after.

## Repository

- Stack = Rust 2024; toolchain/MSRV = `rust-toolchain.toml` + `Cargo.toml`; dependency lock = `Cargo.lock`.
- CLI = `src/main.rs`; core = `src/`; protocol = `schemas/` + `.codex/prompts/observe.md`;
  static in-progress view = `plugin/`; regressions = source unit tests + `tests/`.

### Intent + trust

- Intent = post-hoc agent-session observability → human decides keep/inspect/rerun; scores stay secondary/absent.
- Canonical input = strict ordered `drift.trace/v1` JSONL; adapters normalize agent-native logs.
- Codex exec adapter = `drift import codex-exec --task ...`; strict current JSONL mapping, exact
  source digest, terminal→complete, interrupted→partial, `turn.completed`→unknown task outcome.
- Semantic layer = private-temp/read-only `codex exec`; known tools/context requested off, while
  ambient authenticated runtime/system layers remain → hostile input needs outer isolation.
- Trusted rubric bytes stay immutable; trace, rejected output, + retry diagnostics remain untrusted
  user JSON.
- Trust boundary = parser + output schema + grounding/lifecycle/strict-KEEP validator. “Observed” =
  trace-recorded, conditional on adapter fidelity; `adapter_asserted` is metadata, not attestation.
- Report embeds normalized trace + hashes + deterministic incident-coverage ledger; `render` revalidates
  internal consistency, not authorship.

### Verification

Native gate = `.github/workflows/ci.yml`; dependency policy = `deny.toml`.

```bash
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo deny check
```

Protocol, trust boundaries + static-plugin validation = `README.md`.
