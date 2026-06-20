# ensemble — two firewalls: automated test gate + circuit breaker / abort (design spec)

> Status: DESIGN — for operator review. No code yet. Next: operator approves → implementation plan (writing-plans) → TDD build → double-gate.

## Why
The "dark-factory swarm" engineering lessons reduce to: *don't let AI run naked — gate it, isolate it, and give it a fuse.* ensemble already embodies the governance shape (quorum gate, bounded rounds, flake-never-faked, worktree isolation). The two highest-leverage gaps the article names — and the ones it says to build FIRST — are:

1. **Automated test gate (green-light-unlock):** today a task LANDS on AI votes (reviewers say LGTM). Add a *structural* gate: the project's real test suite must pass (green) before a task can land; a red suite bounces the work back to the implementer with the traceback. AI review stays as a second, semantic gate.
2. **Circuit breaker + one-key abort (the fuse):** detect a task that loops without progress and break it early (before burning rounds/tokens); let the operator instantly abort a run and reset to the pre-run checkpoint.

Both slot into the EXISTING `Conductor::run` loop + worktree isolation — no rearchitecture.

---

## Part A — Automated test gate

### A.1 Where it sits in the round loop
`Conductor::run` today is, per round: implementer → reviewers → `gate::decide`. Insert the test gate between the implementer and the reviewers:

```
round N:
  implementer runs → edits land in the worktree (cwd)
  ── TEST GATE (new) ──
     run `test.command` in the worktree
       RED  → post the traceback to the blackboard, route it as a CHANGES to the
              implementer, start round N+1 — DO NOT run the AI reviewers this round
              (don't spend reviewer turns on code that doesn't even build/pass)
       GREEN → run the AI reviewers → gate::decide (existing quorum)
  LAND iff: tests GREEN in this round AND quorum approves
```

Rationale: mirrors the article's interceptor (test immediately after the producer, before passing context on); saves reviewer tokens on broken code; keeps the quorum as the semantic second gate. A task can therefore only land if it both **passes tests** and **convinces ≥`min_approvals` reviewers**.

### A.2 Config (crew.toml) — additive, backward-compatible
```toml
[test]
command = "cargo test --quiet"   # any shell command; runs in the worktree; exit 0 = GREEN
# timeout_secs = 600             # optional; red on timeout (see B.2)
```
- No `[test]` table → no test gate → today's behaviour exactly (no surprise for existing crews).

### A.3 Component
New `src/test_gate.rs`:
```rust
pub struct TestOutcome { pub passed: bool, pub output: String } // output = excerpted combined stdout+stderr
pub fn run_tests(worktree: &Path, command: &str, timeout_secs: Option<u64>) -> TestOutcome;
```
- Runs `command` via the platform shell (`cmd /C` on Windows, `sh -c` on Unix) in `worktree`, captures combined output, excerpts to a bounded size for the prompt budget.
- A spawn failure (e.g. shell missing) = RED with the error as `output` (fail-closed: never treat an un-runnable test as green).

### A.4 Conductor wiring
In the round loop, after the implementer posts its result and BEFORE building reviewer prompts:
```rust
if let Some(test) = &self.crew.test {
    let t = test_gate::run_tests(cwd, &test.command, test.timeout_secs);
    bb.post("test", if t.passed { "test_pass" } else { "test_failure" }, &t.output);
    if !t.passed {
        // route the traceback back to the implementer as this round's feedback, skip reviewers
        feedback = vec![format!(
            "Your changes did not pass the test suite. Fix WITHOUT breaking existing behaviour. Test output:\n{}",
            t.output
        )];
        // consume a round (respects max_rounds); if rounds remain, loop; else Escalate (see A.5)
        continue_to_next_round_or_escalate();
        continue;
    }
}
// tests green → run reviewers → gate::decide (unchanged)
```

### A.5 Never fake-land on red
If `max_rounds` is exhausted while tests are still RED → `Escalate("tests never passed after N rounds")`. A red suite can NEVER land — this is the structural equivalent of ensemble's existing "flake never faked into approval".

### A.6 Remote implementers
When the implementer runs on a remote node, its edits flow back into the orchestrator's worktree via the existing git-sync (Phase 3b-1). The test gate therefore runs centrally on the **orchestrator's** worktree after sync. Constraint (honest): the orchestrator must have the project's test toolchain installed. Acceptable — the orchestrator is the operator's main box.

### A.7 Testing (hermetic)
Temp git repo + mock adapters + a fake `test.command`:
- `exit 0` (green) + quorum approve → **Land**.
- `exit 1` (red) one round then a mock implementer that "fixes" it (green) → reviewers run → **Land** on round 2.
- `exit 1` forever + `max_rounds` reached → **Escalate("tests never passed")**, never Land.
- red round does NOT call the reviewer adapters (assert via a probe adapter that counts calls).

---

## Part B — Circuit breaker + abort (the fuse)

ensemble already bounds a single task with `max_rounds` (it can't loop forever — it escalates). Part B adds: (B.1) break EARLY when rounds make no progress, (B.2) a wall-clock budget, (B.3) a clean one-key abort.

### B.1 No-progress circuit breaker
`max_rounds` bounds the COUNT; it does not notice a task that is spinning (producing the same thing each round). Add early-break on no progress:
- Track, per round, a **progress signature** = (implementer output, test-failure signature).
- If the signature is identical for `stall_limit` (default 2) consecutive rounds → break with `Escalate("circuit-broken: no progress across N rounds")` — distinct from "max rounds reached".
- Deterministic + cheap + hermetically testable (byte-identical comparison). Semantic-similarity detection (embeddings) = a later, scale-driven refinement.
- Config: `[gate] stall_limit = 2` (absent/0 → disabled; only `max_rounds` applies).

The repeated-identical-**test-failure** case is the sharpest no-progress signal — so Part A feeds Part B: a task that keeps failing the same test the same way trips the breaker fast.

### B.2 Wall-clock budget
CLIs don't report tokens uniformly, so use wall-clock as the practical "token-budget" stand-in:
- `[gate] max_task_secs = N` → if a task's pipeline exceeds N seconds, break: `Escalate("timed out after Ns")`.
- Also drives A.2's `timeout_secs` for a hung test command.
- OPEN DECISION: per-task budget (simpler) vs per-run/whole-swarm budget. Recommend per-task for slice 1.

### B.3 One-key abort + clean teardown
The operator must be able to abort a running `ensemble run` / `run-many` / `dispatch` and have it reset cleanly:
1. **Stop** spawning new work.
2. **Kill** in-flight CLI subprocesses.
3. **Reset to checkpoint:** discard worktrees → back to pre-run HEAD. (Mostly FREE: `Worktree::Drop` already removes the worktree + the unkept branch; Phase-2c only keeps a branch on LAND, so an aborted run discards by construction.)
4. **`dispatch`:** leave the ledger consistent — in-flight claims are simply left `claimed` and `recover_orphans` requeues them on the next run.

Mechanism: a Ctrl-C / SIGINT handler (cross-platform via the `ctrlc` crate) sets an `aborted: AtomicBool`; the conductor checks it between steps and bails to a clean Escalate.

HONEST — the heaviest piece: today `ExecAdapter`/`AgyAdapter` block on `wait_with_output()`, so a *mid-call* kill needs the adapter to hold the `Child` handle and kill it on abort (instead of blocking to completion). That is a real change to the adapter run-loop. Therefore slice B.3 in two steps:
- **B.3a (cheap, deterministic):** the `aborted` flag is checked at every round boundary + before each adapter call → a run stops at the next safe point and tears down cleanly. No mid-subprocess kill yet (an in-flight CLI call finishes, then we bail).
- **B.3b (fast-follow):** true mid-call subprocess kill (track `Child`, `child.kill()` on abort) for instant ESC even during a long agent call.

### B.4 The "daemon"
The article's privileged Daemon Line is overkill at ensemble's scale: the orchestrator process + the ledger ARE the supervisor (max_rounds + stall_limit + max_task_secs + abort flag; `recover_orphans` reclaims dead workers). A separate watchdog PROCESS that can kill a wedged orchestrator from outside = a scale follow-up (note only).

### B.5 Testing (hermetic)
- stall break: a mock implementer returning byte-identical output each round → `Escalate("circuit-broken")` BEFORE `max_rounds`.
- repeated-test-failure break: fake test always red + unchanged implementer → breaker trips.
- wall-clock: a mock adapter that sleeps > `max_task_secs` → `Escalate("timed out")` (use an injected clock to keep it fast/deterministic).
- abort (B.3a): inject `aborted=true` before a round → conductor bails to Escalate, no Land, worktree cleaned. (The real SIGINT wiring is verified by a manual/integration check, not a unit test.)

---

## How A + B compose
Green-light-unlock (A) with a real fuse (B): a task lands only on **green tests + quorum**; a task that can't get green **bounces with the traceback**, and if it spins or runs too long the **breaker cuts it** instead of grinding to max_rounds or burning the operator's budget. This is exactly the article's "嚴格 KPI(測試)+ 開除機制(熔斷)".

## Connection to the interactive-conductor direction
When ensemble later exposes delegation to an interactive conductor (Claude Code via `ensemble agent` / a `run_crew` tool), these firewalls are what make a delegated `run_crew` **safe to hand off** — the conductor can fire a crew run knowing it self-gates on tests and self-breaks on a loop. So A + B are a prerequisite hardening for the bigger direction, not a detour.

## Reuse (not rearchitecting)
- `Conductor::run` round loop — the test gate + breaker are inserted into it.
- `gate.rs` (`GateDecision::Iterate` already routes change-feedback to the implementer) — unchanged; the test gate produces the same kind of feedback.
- `crew.rs` — add optional `[test]` table + `[gate] stall_limit` / `max_task_secs`.
- `worktree.rs` Drop — already gives checkpoint-reset on abort/escalate (no keep ⇒ discard).
- `blackboard.rs` — the feedback/transcript channel the test traceback rides on.
- `ledger.rs` `recover_orphans` — keeps `dispatch` consistent across an abort.

## Open decisions for operator review
1. **Test-gate placement:** test-BEFORE-reviewers (bounce red immediately — recommended) vs. reviewers-always-run + tests only block the final land. Recommended: before-reviewers (cheaper, matches the article's interceptor).
2. **Abort scope for slice 1:** ship just B.1 (no-progress) + B.2 (wall-clock) + B.3a (flag-checked clean stop) now, and B.3b (true mid-call subprocess kill) as a fast-follow? Recommended: yes — B.3a gives a clean ESC at round boundaries without the adapter-loop surgery.
3. **Test command granularity:** one global `[test] command` (slice 1) vs. per-role/per-lane test commands (later, ties into the "lanes" feature).
4. **Build order:** A and B are independent. Recommended order: **A (test gate) first** (it's the article's #1 and it strengthens B.1's signal), then B.

## Deferred (explicitly NOT in these firewalls)
- Lanes / risk-routing + human-or-phone approval (article §2) — borrows phantom-mesh's governor pattern; separate feature.
- Container/cgroup resource limits (article §4) — worktree isolation covers slice 1; containers later.
- Vector-embedding log topology (article §5) and failure-memory RAG / Swarm Gym (article §6) — scale-driven; later.
