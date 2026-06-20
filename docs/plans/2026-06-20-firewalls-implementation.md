# Two firewalls — implementation plan (test gate + circuit breaker/abort)

> REQUIRED SUB-SKILL: TDD per task. Build/test via WSL (`cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=$HOME/ensemble-target cargo test`). Work on `main`. Gate every change with codex+claude. Spec: `docs/specs/2026-06-20-firewalls-test-gate-and-circuit-breaker.md` (operator-approved, all 4 decisions = recommended defaults). **Ship Part A first (gate+push), then Part B.**

**Goal:** add a structural automated TEST gate (green-light-unlock) and a circuit breaker + one-key abort to the conductor, slotting into the existing `Conductor::run` round loop.

**Architecture:** `crew.toml` gains `[test]` + `[gate] stall_limit`/`max_task_secs`. New `src/test_gate.rs`. `Conductor::run` runs tests after the implementer (red → bounce to implementer, skip reviewers; green → reviewers → quorum), tracks a no-progress signature, enforces a wall-clock budget, and checks an abort flag. `main.rs` wires Ctrl-C.

---

## PART A — automated test gate

### Task A1: `crew.toml` config — `[test]` table

**Files:** `src/crew.rs`.

- [ ] **Step 1 (impl):** add the struct + field:
```rust
#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    /// shell command run in the worktree; exit 0 = GREEN.
    pub command: String,
    /// optional hard timeout; a test command that exceeds it is treated as RED ("timed out").
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}
```
Add to `CrewConfig`:
```rust
    #[serde(default)]
    pub test: Option<TestConfig>,
```

- [ ] **Step 2 (test):** add to crew.rs tests:
```rust
    #[test]
    fn parses_optional_test_gate() {
        let toml = r#"
            pipeline = ["implement","review"]
            [gate]
            min_approvals = 1
            max_rounds = 2
            on_flake = "exclude"
            [roles.implement]
            agent = "codex"
            [roles.review]
            agent = "claude"
            [test]
            command = "cargo test --quiet"
        "#;
        let c = CrewConfig::from_toml(toml).unwrap();
        assert_eq!(c.test.as_ref().unwrap().command, "cargo test --quiet");
        // absent [test] → None (backward compatible)
        let c2 = CrewConfig::from_toml(
            "pipeline=[\"i\"]\n[gate]\nmin_approvals=1\nmax_rounds=1\non_flake=\"exclude\"\n[roles.i]\nagent=\"codex\"",
        )
        .unwrap();
        assert!(c2.test.is_none());
    }
```

- [ ] **Step 3:** `src/lib.rs` re-export: add `TestConfig` to the `pub use crew::{...}` line. **Step 4:** test green; fmt; clippy. Commit `feat(firewall-A): crew.toml [test] table`.

---

### Task A2: `src/test_gate.rs` — run the test command

**Files:** Create `src/test_gate.rs`; `src/lib.rs` (`pub mod test_gate;` + re-export `run_tests, TestOutcome`).

- [ ] **Step 1 (impl):**
```rust
//! The automated test gate (firewall A): run a project's real test command in the worktree; GREEN
//! (exit 0) unlocks the AI reviewers, RED bounces the traceback back to the implementer. A test that
//! can't even be RUN is treated as RED (fail-closed — never let an un-runnable suite look green).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct TestOutcome {
    pub passed: bool,
    pub output: String, // excerpted combined stdout+stderr (the tail — failures live at the end)
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(command);
    c
}
#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(command);
    c
}

/// Run `command` in `worktree`. Returns RED on non-zero exit, on a spawn failure, or on timeout.
pub fn run_tests(worktree: &Path, command: &str, timeout_secs: Option<u64>) -> TestOutcome {
    let mut cmd = shell(command);
    cmd.current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestOutcome {
                passed: false,
                output: format!("could not run test command `{command}`: {e}"),
            }
        }
    };
    if let Some(secs) = timeout_secs {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return TestOutcome {
                            passed: false,
                            output: format!("test command timed out after {secs}s"),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return TestOutcome {
                        passed: false,
                        output: format!("test command wait failed: {e}"),
                    }
                }
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return TestOutcome {
                passed: false,
                output: format!("test command failed: {e}"),
            }
        }
    };
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    TestOutcome {
        passed: out.status.success(),
        output: tail(&combined, 2000),
    }
}

/// Keep the LAST `max` chars (test failures/tracebacks are at the end of the output).
fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let skip = n - max;
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_on_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(dir.path(), "exit 0", None);
        assert!(t.passed);
    }

    #[test]
    fn red_on_nonzero_with_output() {
        let dir = tempfile::tempdir().unwrap();
        // print to stderr then fail
        let t = run_tests(dir.path(), "echo boom 1>&2; exit 1", None);
        assert!(!t.passed);
        assert!(t.output.contains("boom"));
    }

    #[test]
    fn timeout_is_red() {
        let dir = tempfile::tempdir().unwrap();
        let t = run_tests(dir.path(), "sleep 5", Some(1));
        assert!(!t.passed);
        assert!(t.output.contains("timed out"));
    }
}
```
> NOTE: the `timeout_is_red` test uses `sleep` — available on the WSL/Linux test runner. If the suite must also pass on native Windows, gate that single test with `#[cfg(unix)]` (the build runs via WSL, so Unix is the test target). Keep `green`/`red` tests cross-platform (`exit 0/1`, `echo ... 1>&2` work in both `sh` and `cmd`).

- [ ] **Step 2:** test green; fmt; clippy. Commit `feat(firewall-A): test_gate::run_tests (shell, fail-closed, timeout)`.

---

### Task A3: wire the test gate into `Conductor::run`

**Files:** `src/conductor.rs`; `tests/` (new `tests/test_gate_e2e.rs`).

- [ ] **Step 1 (impl):** in `Conductor::run`, AFTER the implementer posts its `result` and BEFORE the reviewer loop, insert:
```rust
// ── TEST GATE (firewall A) ── tests must be GREEN before the AI reviewers run.
if let Some(test) = &self.crew.test {
    let t = crate::test_gate::run_tests(cwd, &test.command, test.timeout_secs);
    bb.post(
        "test",
        if t.passed { "test_pass" } else { "test_failure" },
        &t.output,
    );
    if !t.passed {
        if round + 1 >= max {
            return RunOutcome {
                decision: Decision::Escalated(format!(
                    "tests never passed after {} round(s)",
                    round + 1
                )),
                rounds: round + 1,
                blackboard: bb,
                branch: None,
            };
        }
        // bounce the traceback back to the implementer; skip reviewers this round.
        feedback = vec![format!(
            "Your changes did not pass the test suite. Fix WITHOUT breaking existing behaviour. \
             Test output:\n{}",
            t.output
        )];
        continue;
    }
}
```
(The implementer's `out.text` was just posted; `cwd` holds its edits. Reviewers + `gate::decide` run unchanged when tests are green.)

- [ ] **Step 2 (test):** `tests/test_gate_e2e.rs` — a WriterThenLgtm-style harness with a crew that has a `[test]`. Use adapters that write a file; the test command checks the file's content so RED/GREEN is deterministic without a real toolchain:
```rust
use ensemble::*;
use std::collections::HashMap;

// implementer writes `marker` with `content`; reviewer LGTMs.
struct Writer { name: String, file: String, content: String }
impl Adapter for Writer {
    fn name(&self) -> &str { &self.name }
    fn run(&self, _p: &str, cwd: &std::path::Path) -> Result<AgentOutput, AdapterError> {
        if !self.file.is_empty() { std::fs::write(cwd.join(&self.file), &self.content).unwrap(); }
        Ok(AgentOutput { agent: self.name.clone(), text: if self.file.is_empty() { "VERDICT: LGTM".into() } else { format!("wrote {}", self.file) } })
    }
}

fn crew_with_test(cmd: &str) -> CrewConfig {
    CrewConfig::from_toml(&format!(r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 1
        max_rounds = 2
        on_flake = "exclude"
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
        [test]
        command = "{cmd}"
    "#)).unwrap()
}

#[test]
fn green_tests_allow_a_landing() {
    // implementer writes ok.txt="PASS"; the test command greps it → exit 0
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert("codex".into(), Box::new(Writer { name: "codex".into(), file: "ok.txt".into(), content: "PASS".into() }));
    m.insert("claude".into(), Box::new(Writer { name: "claude".into(), file: String::new(), content: String::new() }));
    let c = Conductor::new(crew_with_test("grep -q PASS ok.txt"), m);
    let out = c.run("do it", std::path::Path::new("."));
    assert!(matches!(out.decision, Decision::Landed), "green tests + LGTM must land: {:?}", out.decision);
}

#[test]
fn red_tests_never_land_and_escalate() {
    // implementer writes ok.txt="NOPE"; the test command greps for PASS → exit 1 every round
    let mut m: HashMap<String, Box<dyn Adapter>> = HashMap::new();
    m.insert("codex".into(), Box::new(Writer { name: "codex".into(), file: "ok.txt".into(), content: "NOPE".into() }));
    m.insert("claude".into(), Box::new(Writer { name: "claude".into(), file: String::new(), content: String::new() }));
    let c = Conductor::new(crew_with_test("grep -q PASS ok.txt"), m);
    let out = c.run("do it", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(why) => assert!(why.contains("tests never passed"), "got: {why}"),
        other => panic!("red tests must escalate, never land: {other:?}"),
    }
}
```
> NOTE: these run `c.run(..., ".")` (the test command runs in cwd = the crate root for this hermetic test; it only reads/writes `ok.txt` there). To avoid leaving `ok.txt` in the repo, run in a tempdir cwd instead: create a `tempfile::tempdir()`, write nothing else, and pass its path as cwd. (Adjust the harness to use a tempdir cwd; `grep` is available on the WSL runner — gate the file with `#[cfg(unix)]` if Windows-native test runs are needed.)

- [ ] **Step 3:** full `cargo test` green; fmt; clippy. Commit `feat(firewall-A): conductor runs the test gate — green unlocks reviewers, red bounces`.

**→ GATE Part A (codex+claude) on the A1–A3 diff, then push. Then start Part B.**

---

## PART B — circuit breaker + abort

### Task B1: gate config — `stall_limit` + `max_task_secs`

**Files:** `src/crew.rs`; update `GatePolicy` literals in `gate.rs` tests.

- [ ] **Step 1 (impl):** add to `GatePolicy` (serde default 0 = disabled, backward compatible):
```rust
    /// Break early if the implementer makes no progress for this many consecutive rounds (0 = off).
    #[serde(default)]
    pub stall_limit: u32,
    /// Wall-clock budget per task in seconds (0 = off) — a practical stand-in for a token budget.
    #[serde(default)]
    pub max_task_secs: u64,
```
- [ ] **Step 2:** every `GatePolicy { ... }` literal in tests (gate.rs `policy()` helper + any other) gains `stall_limit: 0, max_task_secs: 0`. Search: `rg "GatePolicy \{" src tests`.
- [ ] **Step 3:** add a crew.rs test that `[gate] stall_limit = 2` + `max_task_secs = 30` parse. test green; fmt; clippy. Commit `feat(firewall-B): gate stall_limit + max_task_secs config`.

---

### Task B2: no-progress breaker + wall-clock budget + abort flag in `Conductor::run`

**Files:** `src/conductor.rs`.

- [ ] **Step 1 (impl):**
  - Add an abort flag to `Conductor`:
```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Conductor {
    crew: CrewConfig,
    adapters: HashMap<String, Box<dyn Adapter>>,
    abort: Arc<AtomicBool>,
}
impl Conductor {
    pub fn new(crew: CrewConfig, adapters: HashMap<String, Box<dyn Adapter>>) -> Self {
        Self { crew, adapters, abort: Arc::new(AtomicBool::new(false)) }
    }
    /// Wire an external abort flag (set by a Ctrl-C handler) so a run stops cleanly at the next
    /// round boundary.
    pub fn with_abort(mut self, flag: Arc<AtomicBool>) -> Self {
        self.abort = flag;
        self
    }
}
```
  - A pure helper for the budget (hermetically testable):
```rust
/// True when `elapsed_secs` has exceeded a configured wall-clock budget (`budget == 0` ⇒ no budget).
fn over_budget(elapsed_secs: u64, budget: u64) -> bool {
    budget > 0 && elapsed_secs >= budget
}
```
  - In `run`, before the round loop: `let started = std::time::Instant::now(); let mut last_sig: Option<String> = None; let mut same = 0u32;`
  - At the TOP of each round (before the implementer), check abort + budget:
```rust
if self.abort.load(Ordering::Relaxed) {
    return RunOutcome { decision: Decision::Escalated("aborted by operator".into()), rounds: round, blackboard: bb, branch: None };
}
if over_budget(started.elapsed().as_secs(), self.crew.gate.max_task_secs) {
    return RunOutcome { decision: Decision::Escalated(format!("timed out after {}s", self.crew.gate.max_task_secs)), rounds: round, blackboard: bb, branch: None };
}
```
  - After the implementer result + test gate (compute the progress signature from the implementer output + the test output, then check stall). Capture the implementer's text into a variable when posting it (`let impl_text = out.text.clone(); bb.post(&out.agent, "result", &out.text);`). After the test gate block, BEFORE the reviewers (and BEFORE the red-bounce `continue` — so a task stuck failing the same test trips the breaker):
```rust
// ── CIRCUIT BREAKER (firewall B): break early on no progress ──
let sig = format!("{impl_text}\u{1}{last_test_output}"); // last_test_output = "" if no test gate
if last_sig.as_deref() == Some(sig.as_str()) { same += 1; } else { same = 1; last_sig = Some(sig); }
if self.crew.gate.stall_limit > 0 && same >= self.crew.gate.stall_limit {
    return RunOutcome {
        decision: Decision::Escalated(format!("circuit-broken: no progress across {same} identical rounds")),
        rounds: round + 1, blackboard: bb, branch: None,
    };
}
```
> Structure the test-gate block to expose `last_test_output` (the `t.output` when a `[test]` ran, else an empty string) so the signature can include it. Put the breaker check AFTER computing `last_test_output` but BEFORE the test-red `continue`, so repeated identical test failures trip it.

- [ ] **Step 2 (test):** in `tests/pipeline_hermetic.rs` (or a new `tests/firewall_b.rs`):
```rust
#[test]
fn stall_limit_breaks_on_no_progress() {
    // implementer returns byte-identical output every round; reviewer always CHANGES (never lands)
    let crew = CrewConfig::from_toml(r#"
        pipeline = ["implement","review"]
        [gate]
        min_approvals = 2
        max_rounds = 10
        on_flake = "exclude"
        stall_limit = 2
        [roles.implement]
        agent = "codex"
        [roles.review]
        agent = "claude"
    "#).unwrap();
    let mut m: std::collections::HashMap<String, Box<dyn Adapter>> = std::collections::HashMap::new();
    m.insert("codex".into(), Box::new(AlwaysOk { name: "codex".into(), reply: "same output".into() }));
    m.insert("claude".into(), Box::new(AlwaysOk { name: "claude".into(), reply: "VERDICT: CHANGES: nope".into() }));
    let out = Conductor::new(crew, m).run("t", std::path::Path::new("."));
    match out.decision {
        Decision::Escalated(why) => assert!(why.contains("circuit-broken"), "got: {why}"),
        other => panic!("identical output must trip the breaker before max_rounds: {other:?}"),
    }
    assert!(out.rounds < 10, "must break EARLY, not grind to max_rounds");
}

#[test]
fn abort_flag_stops_a_run_cleanly() {
    let crew = /* a normal 1-approval crew */;
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)); // pre-aborted
    let c = Conductor::new(crew, map).with_abort(flag);
    let out = c.run("t", std::path::Path::new("."));
    match out.decision { Decision::Escalated(why) => assert!(why.contains("aborted")), o => panic!("{o:?}") }
}
```
(`AlwaysOk` already exists in `tests/pipeline_hermetic.rs`. Add a pure `over_budget` unit test in conductor.rs: `assert!(over_budget(5,3)); assert!(!over_budget(2,3)); assert!(!over_budget(99,0));`.)

- [ ] **Step 3:** `cargo test` green; fmt; clippy. Commit `feat(firewall-B): no-progress breaker + wall-clock budget + abort flag`.

---

### Task B3: Ctrl-C wiring in the CLI + docs

**Files:** `Cargo.toml` (`ctrlc = "3"`); `src/main.rs`; docs.

- [ ] **Step 1 (impl):** in `main()`, install a Ctrl-C handler that flips a shared flag, and thread it into every `Conductor::new(...)` via `.with_abort(flag.clone())` in `run_single`/`run_many`/`dispatch_cmd`:
```rust
// in main(), before dispatch:
let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
{
    let a = abort.clone();
    let _ = ctrlc::set_handler(move || a.store(true, std::sync::atomic::Ordering::Relaxed));
}
```
Pass `abort` down (e.g. via a thread-local or by giving the cmd fns the flag). Simplest: make `abort` a process-global `static` `Lazy`/`OnceLock<Arc<AtomicBool>>` set in `main`, read in the cmd fns when building the Conductor. Document the choice in the code.
- [ ] **Step 2:** `cargo build` + `cargo test` green; fmt; clippy. Manual: a long `ensemble run` + Ctrl-C → it stops at the next round boundary and exits non-zero, worktree cleaned.
- [ ] **Step 3:** update `docs/2026-06-19-ensemble-design.md` (firewalls done), `docs/AUTONOMOUS-BACKLOG.md` (log), the example `crew.toml` (commented `[test]` + `stall_limit`/`max_task_secs`). Commit `feat(firewall-B): Ctrl-C abort wiring + docs`.

**→ GATE Part B (codex+claude) on the B1–B3 diff, then push.**

---

## Notes / deferred (per spec)
- B.3b true mid-call subprocess kill (kill the CLI child the instant abort fires, not just at the round boundary) — needs the adapter run-loop to hold the `Child` + a kill path. Fast-follow.
- Semantic (embedding) no-progress detection instead of byte-identical. Later.
- Per-role/per-lane test commands; lanes + phone-approval; container resource limits; failure-memory RAG; embedding log topology — separate features (article §2/§4/§5/§6).
