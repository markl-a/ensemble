//! Cross-machine git sync via git bundles (Phase 3b-1). A bundle is a self-contained, transferable
//! packfile: the orchestrator bundles its base commit, the node materializes it in an isolated repo,
//! runs the agent, commits onto a `dispatch/<job_id>` branch, and bundles that branch back. The
//! orchestrator fast-forwards its worktree to the returned tip — so a remote agent's edits land in
//! the orchestrator's worktree just like a local agent's. No git server; rides the HTTP wire.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The filesystem name prefix every node-side scratch repo carries (see `NodeJobDir::new`). GC keys
/// off it to find leftovers without touching anything else in the temp dir.
const NODE_SCRATCH_PREFIX: &str = "ensemble-node-";

/// A git bundle — raw bytes, transferable over any byte channel.
pub type Bundle = Vec<u8>;

/// Process-wide counters making scratch/staging paths unique even when two callers pass the same
/// logical id (e.g. two orchestrators sending `job_id = "codex-0"` to one node).
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);
static NODE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Make a ref/id safe to embed in a filesystem path (a branch like `dispatch/codex-0` contains `/`,
/// which is a path separator). Non `[A-Za-z0-9_-]` → `-`.
fn sanitize_ref(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// A file removed on drop — so a staged bundle never leaks, even on an early `?` return.
struct TempFile {
    path: PathBuf,
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn git_capture(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out)
}

fn git_run(dir: &Path, args: &[&str]) -> std::io::Result<()> {
    git_capture(dir, args).map(|_| ())
}

/// The HEAD commit SHA of `repo`.
pub fn head_sha(repo: &Path) -> std::io::Result<String> {
    let out = git_capture(repo, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Outcome of landing one branch onto another.
#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Merged cleanly (fast-forward or a true merge commit); `into` now contains `branch`.
    Landed,
    /// Conflicting paths; the merge was ABORTED and the worktree restored — NEVER auto-resolved.
    Conflict(Vec<String>),
}

/// True if `repo` is mid-merge (MERGE_HEAD set). Worktree-safe (asks git, not a `.git/` path).
fn is_mid_merge(repo: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Refuse to start a merge unless the worktree is clean AND not already mid-merge — so a later
/// `merge --abort` / restore provably returns to a PRISTINE state, never to dirt or a half-merge.
fn preflight_clean(repo: &Path) -> std::io::Result<()> {
    if !git_capture(repo, &["status", "--porcelain"])?.stdout.is_empty() {
        return Err(std::io::Error::other(
            "worktree not clean — commit or stash before `ensemble merge`",
        ));
    }
    if is_mid_merge(repo) {
        return Err(std::io::Error::other(
            "a merge is already in progress — finish it or `git merge --abort` first",
        ));
    }
    Ok(())
}

/// The paths git still reports as UNMERGED (conflicted index entries) in `repo`. A query failure is
/// surfaced (not swallowed) so a caller never treats "couldn't ask git" as "nothing unmerged".
fn unmerged_paths(repo: &Path) -> std::io::Result<Vec<String>> {
    let o = git_capture(repo, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(String::from)
        .collect())
}

/// Does the working-tree file at `p` still contain a git conflict marker? `<<<<<<<` / `>>>>>>>`
/// (7 chars) are git's own boundary markers and effectively never start a real source line, so their
/// presence is positive proof a content resolution is incomplete. BYTE scan (not UTF-8 decode) so a
/// non-UTF-8 file with ASCII markers can't slip past. Unreadable/missing → no markers (the conflict,
/// for that path, was resolved by deletion).
fn path_has_conflict_markers(repo: &Path, p: &str) -> bool {
    std::fs::read(repo.join(p))
        .map(|bytes| {
            bytes
                .split(|&b| b == b'\n')
                .any(|line| line.starts_with(b"<<<<<<<") || line.starts_with(b">>>>>>>"))
        })
        .unwrap_or(false)
}

fn files_have_conflict_markers(repo: &Path, paths: &[String]) -> bool {
    paths.iter().any(|p| path_has_conflict_markers(repo, p))
}

/// Is `p` a conflict a text-editing resolver can SAFELY handle — i.e. a both-modified TEXT conflict?
/// Decided by git's index stages (`ls-files -u`), NOT marker scan alone: a path qualifies only if
/// BOTH stage 2 (ours) AND stage 3 (theirs) exist (a genuine two-sided conflict, excluding
/// modify/delete & rename which have one side) AND git actually wrote textual markers (excluding
/// BINARY both-modified, which has both stages but no markers). The stage check is what stops a
/// structural conflict whose file merely contains a marker-like line from being misclassified as
/// content. On any query failure, returns false → the conflict is treated as structural and escalates
/// (the safe direction — never lands a bad merge).
fn is_text_content_conflict(repo: &Path, p: &str) -> bool {
    let out = match git_capture(repo, &["ls-files", "-u", "--", p]) {
        Ok(o) => o,
        Err(_) => return false,
    };
    let (mut ours, mut theirs) = (false, false);
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // format: "<mode> <sha> <stage>\t<path>"
        match line.split('\t').next().and_then(|m| m.split_whitespace().nth(2)) {
            Some("2") => ours = true,
            Some("3") => theirs = true,
            _ => {}
        }
    }
    ours && theirs && path_has_conflict_markers(repo, p)
}

/// Restore `repo`'s `into` branch to EXACTLY its pre-merge commit `pre_sha`. A single `reset --hard`
/// clears any in-progress merge (MERGE_HEAD) AND undoes a commit the resolver may have made — one
/// path that is correct whether we are mid-merge or the resolver already committed, and that can't
/// preserve stray tracked edits the way a bare `merge --abort` might. Then sweep the resolver's
/// untracked scratch (safe: `preflight_clean` proved the tree had no untracked files before, and
/// `git clean` skips gitignored paths like `.ensemble/`). BOTH steps surface failure so a caller
/// never reports escalation over a half-restored `into`. (If `reset` succeeds but `clean` fails on a
/// benign undeletable untracked file, the caller's escalation becomes an Err rather than a
/// `Conflict` — the SAFE direction: tracked state + the merge ref are already restored, and we would
/// rather over-report than silently leave resolver scratch behind.)
fn restore_to(repo: &Path, pre_sha: &str) -> std::io::Result<()> {
    git_run(repo, &["reset", "--hard", "--quiet", pre_sha])?;
    git_run(repo, &["clean", "-fdq"])?;
    Ok(())
}

/// Land `branch` onto `into` in `repo`: fast-forward when possible, else a true merge. A conflict is
/// NEVER auto-resolved — the merge is aborted (worktree restored) and the conflicting paths returned
/// so the caller can escalate (or spawn a resolver round). REFUSES a dirty or already-merging worktree
/// up front, so a conflict-abort provably restores a PRISTINE state (this lands onto a real branch, so
/// it is strict). On success it leaves `repo` checked out on `into`.
pub fn merge_branch(repo: &Path, branch: &str, into: &str) -> std::io::Result<MergeOutcome> {
    // Preflight: clean + not mid-merge, else a later `merge --abort` would restore to dirt, not clean.
    preflight_clean(repo)?;

    git_run(repo, &["checkout", "--quiet", into])?;
    let merge = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge", "--no-edit", branch])
        .output()?;
    if merge.status.success() {
        return Ok(MergeOutcome::Landed);
    }

    // The merge failed. Capture conflicts BEFORE aborting (abort wipes the unmerged index); a failed
    // capture is itself fatal. Then ALWAYS abort any in-progress merge, surfacing an abort failure —
    // never return while `into` is left half-merged.
    let conflicts: Vec<String> =
        match git_capture(repo, &["diff", "--name-only", "--diff-filter=U"]) {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(String::from)
                .collect(),
            Err(e) => {
                let mut msg = format!("could not query conflicts after a failed merge: {e}");
                if is_mid_merge(repo) {
                    if let Err(ae) = git_run(repo, &["merge", "--abort"]) {
                        msg.push_str(&format!(
                            "; AND `git merge --abort` failed — {into} may be left mid-merge: {ae}"
                        ));
                    }
                }
                return Err(std::io::Error::other(msg));
            }
        };
    if is_mid_merge(repo) {
        git_run(repo, &["merge", "--abort"]).map_err(|e| {
            std::io::Error::other(format!(
                "merge of {branch} into {into} failed AND `git merge --abort` failed — {into} may be \
                 left mid-merge, resolve manually: {e}"
            ))
        })?;
    }
    if conflicts.is_empty() {
        // A non-conflict failure (e.g. unknown branch) — nothing was merged, nothing to abort.
        return Err(std::io::Error::other(format!(
            "git merge {branch} into {into}: {}",
            String::from_utf8_lossy(&merge.stderr)
        )));
    }
    Ok(MergeOutcome::Conflict(conflicts))
}

/// Land `branch` onto `into`, but on a CONFLICT run ONE AI-resolver round (`resolve`, given the repo
/// and the conflicting paths, edits files in place) before deciding — the locked conflict policy
/// (design decision 2). The resolution is COMPLETED only if it is PROVABLY clean: no git conflict
/// marker survives in any conflicting file AND nothing is left unmerged; otherwise the merge is
/// restored to `into`'s exact pre-merge commit and `Conflict(paths)` is returned so the caller
/// escalates. NEVER force / auto-accept, and NEVER commit a half-resolved merge.
///
/// Only CONTENT conflicts (those carrying `<<<<<<<`/`>>>>>>>` markers) are handed to the resolver: a
/// markerless STRUCTURAL conflict (modify/delete, rename, binary, submodule) cannot be safely
/// resolved by a text-editing CLI and — crucially — `git add` would silently "resolve" it to an
/// arbitrary side, so any structural conflict escalates immediately WITHOUT running the resolver.
/// `resolve` should ONLY edit the conflicting files (it need not `git add`/commit); a resolver that
/// commits on its own is still validated by the marker scan and, if it left markers, undone.
pub fn merge_with_resolver(
    repo: &Path,
    branch: &str,
    into: &str,
    resolve: impl FnOnce(&Path, &[String]) -> std::io::Result<()>,
) -> std::io::Result<MergeOutcome> {
    // Restore `into` to pre_sha, folding a restore failure into `base` so it's never lost.
    fn fail_restoring(
        repo: &Path,
        pre_sha: &str,
        into: &str,
        base: String,
    ) -> std::io::Error {
        let mut msg = base;
        if let Err(re) = restore_to(repo, pre_sha) {
            msg.push_str(&format!("; AND restore failed — {into} may be left mid-merge: {re}"));
        }
        std::io::Error::other(msg)
    }

    preflight_clean(repo)?;
    git_run(repo, &["checkout", "--quiet", into])?;
    // Capture into's tip BEFORE merging so we can hard-reset to it even if the resolver commits.
    let pre_sha = head_sha(repo)?;

    let merge = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge", "--no-edit", branch])
        .output()?;
    if merge.status.success() {
        return Ok(MergeOutcome::Landed); // clean / fast-forward — resolver not needed
    }

    // Capture conflicts BEFORE touching the merge state (restore wipes the unmerged index).
    let conflicts: Vec<String> = match unmerged_paths(repo) {
        Ok(c) => c,
        Err(e) => {
            return Err(fail_restoring(
                repo,
                &pre_sha,
                into,
                format!("could not query conflicts after a failed merge: {e}"),
            ))
        }
    };
    if conflicts.is_empty() {
        // A non-conflict failure (e.g. unknown branch) — nothing merged; keep the never-half-state
        // invariant, then surface the original error.
        restore_to(repo, &pre_sha)?;
        return Err(std::io::Error::other(format!(
            "git merge {branch} into {into}: {}",
            String::from_utf8_lossy(&merge.stderr)
        )));
    }

    // Hand the resolver ONLY both-modified TEXT conflicts. A structural/binary conflict (and a
    // structural file that merely contains a marker-like line) is escalated WITHOUT running the
    // resolver — `git add` would otherwise auto-pick a side.
    if conflicts.iter().any(|p| !is_text_content_conflict(repo, p)) {
        restore_to(repo, &pre_sha)?;
        return Ok(MergeOutcome::Conflict(conflicts));
    }

    // ── ONE AI-resolver round ── the resolver edits the conflicted (content) files in place.
    if resolve(repo, &conflicts).is_err() {
        restore_to(repo, &pre_sha)?;
        return Ok(MergeOutcome::Conflict(conflicts)); // resolver failed → escalate
    }

    // The resolver's contract is EDIT-ONLY (don't `git add`/commit). If it left the repo no longer
    // mid-merge it committed/aborted on its own — a self-completed merge we cannot cleanly validate
    // (it could have committed markers then overwritten the worktree clean), so don't trust it:
    // restore to the exact pre-merge commit and escalate. (Well-behaved resolvers stay mid-merge.)
    if !is_mid_merge(repo) {
        restore_to(repo, &pre_sha)?;
        return Ok(MergeOutcome::Conflict(conflicts));
    }

    // Still mid-merge (expected). Stage ONLY the named conflict paths (not `-A`: that would
    // auto-resolve anything and sweep in resolver scratch) — so what we validate IS what we commit.
    let mut add_args: Vec<&str> = vec!["add", "--"];
    add_args.extend(conflicts.iter().map(|s| s.as_str()));
    if let Err(e) = git_run(repo, &add_args) {
        return Err(fail_restoring(
            repo,
            &pre_sha,
            into,
            format!("staging the AI resolution failed: {e}"),
        ));
    }

    // GUARD: refuse to complete unless PROVABLY clean — no surviving conflict marker in any
    // conflicting file, and nothing left unmerged (a query failure is itself fatal-with-restore).
    let still_unmerged = match unmerged_paths(repo) {
        Ok(u) => u,
        Err(e) => {
            return Err(fail_restoring(
                repo,
                &pre_sha,
                into,
                format!("could not re-check unmerged paths after resolution: {e}"),
            ))
        }
    };
    if files_have_conflict_markers(repo, &conflicts) || !still_unmerged.is_empty() {
        restore_to(repo, &pre_sha)?;
        return Ok(MergeOutcome::Conflict(conflicts)); // still conflicting → escalate
    }

    // Clean resolution, still mid-merge → complete the merge commit with exactly the staged content.
    if let Err(e) = git_run(repo, &["commit", "--no-edit", "--quiet"]) {
        return Err(fail_restoring(
            repo,
            &pre_sha,
            into,
            format!("committing the resolved merge failed: {e}"),
        ));
    }
    Ok(MergeOutcome::Landed)
}

/// True if `dir` is inside a git work tree.
pub fn is_git_worktree(dir: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// Bundle `rev` (e.g. "HEAD") of `repo` into transferable bytes (`git bundle create - <rev>`).
pub fn bundle_rev(repo: &Path, rev: &str) -> std::io::Result<Bundle> {
    let out = git_capture(repo, &["bundle", "create", "-", rev])?;
    Ok(out.stdout)
}

/// NODE side: materialize `bundle` (carrying `base_ref`, e.g. "HEAD") into a fresh repo at `dest`,
/// then create + check out `job_branch` at that base. After this the agent edits files in `dest`.
pub fn materialize_base(
    dest: &Path,
    bundle: &[u8],
    base_ref: &str,
    job_branch: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    let bundle_path = dest.join(".ensemble-base.bundle");
    std::fs::write(&bundle_path, bundle)?;
    git_run(dest, &["init", "-q"])?;
    git_run(dest, &["config", "user.email", "ensemble@node"])?;
    git_run(dest, &["config", "user.name", "ensemble-node"])?;
    let bp = bundle_path.to_string_lossy().to_string();
    git_run(dest, &["fetch", "--quiet", &bp, base_ref])?;
    git_run(dest, &["checkout", "-q", "-b", job_branch, "FETCH_HEAD"])?;
    let _ = std::fs::remove_file(&bundle_path);
    Ok(())
}

/// NODE side: stage + commit everything in `dir` onto the current branch. Returns Ok(false) if there
/// was nothing to commit (the agent produced no edits).
pub fn commit_all(dir: &Path, message: &str) -> std::io::Result<bool> {
    git_run(dir, &["add", "-A"])?;
    let clean = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--cached", "--quiet"])
        .status()?
        .success();
    if clean {
        return Ok(false);
    }
    git_run(dir, &["commit", "-q", "-m", message])?;
    Ok(true)
}

/// NODE side: bundle `branch` of `repo` into transferable bytes.
pub fn bundle_branch(repo: &Path, branch: &str) -> std::io::Result<Bundle> {
    bundle_rev(repo, branch)
}

/// ORCHESTRATOR side: fetch `branch` from `bundle` into `repo`, then fast-forward `worktree` to that
/// tip so the remote agent's edits appear in the worktree. `worktree` must be a worktree of `repo`
/// sitting at (an ancestor of) the dispatch tip — true under `run_in_repo` (the worktree is at base).
pub fn apply_result(
    repo: &Path,
    worktree: &Path,
    bundle: &[u8],
    branch: &str,
) -> std::io::Result<()> {
    // Stage in a unique temp file (NOT a fixed path in the repo): concurrent `run_many` applies must
    // not clobber each other's bundle between write and fetch, and it must never leak into the
    // worktree. The TempFile guard removes it on every return path.
    let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stage = TempFile {
        path: std::env::temp_dir().join(format!(
            "ensemble-stage-{}-{}-{seq}.bundle",
            sanitize_ref(branch),
            std::process::id()
        )),
    };
    std::fs::write(&stage.path, bundle)?;
    let bp = stage.path.to_string_lossy().to_string();
    let local_ref = format!("refs/ensemble/{branch}");
    git_run(
        repo,
        &["fetch", "--quiet", &bp, &format!("{branch}:{local_ref}")],
    )?;
    git_run(worktree, &["merge", "--ff-only", "--quiet", &local_ref])?;
    // The merged commit is now reachable via the worktree's own branch, so the temporary tracking
    // ref is dead weight — prune it (best-effort) so `refs/ensemble/*` doesn't accumulate one ref
    // per remote run forever. A prune hiccup must not fail an otherwise-successful apply.
    let _ = git_run(repo, &["update-ref", "-d", &local_ref]);
    Ok(())
    // `stage` drops → staged bundle removed
}

/// A scratch directory for a node-side job, removed on drop.
pub struct NodeJobDir {
    pub path: PathBuf,
}
impl NodeJobDir {
    pub fn new(job_id: &str) -> Self {
        // Unique per node PROCESS, not just per job_id: two different orchestrators can each send
        // `job_id = "codex-0"`, and a deterministic dir would let one job's Drop delete the other's
        // scratch repo mid-run. pid + a local counter keep them isolated.
        let seq = NODE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ensemble-node-{}-{}-{seq}",
            sanitize_ref(job_id),
            std::process::id()
        ));
        Self { path }
    }
}
impl Drop for NodeJobDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Parse the owner pid embedded in a node-scratch dir name `ensemble-node-<job>-<pid>-<seq>`. `<job>`
/// is sanitized but may itself contain '-' and digits, so the pid is the SECOND-to-last '-'-separated
/// field (seq is the last, job is everything before). Returns None if the shape doesn't match — and a
/// None pid is never swept, so an unrecognized name is always kept.
fn scratch_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(NODE_SCRATCH_PREFIX)?;
    let mut tail = rest.rsplitn(3, '-'); // [seq, pid, job(maybe-with-dashes)]
    let _seq = tail.next()?;
    let pid = tail.next()?;
    tail.next()?; // a job segment must precede the pid (else this isn't our shape)
    pid.parse::<u32>().ok()
}

/// PURE: pick the node-scratch dirs that are safe to delete. An entry `(name, pid)` qualifies iff its
/// name has the node-scratch prefix, its owner pid is known, that pid is NOT this process (`self_pid`),
/// and `is_alive(pid)` is false — i.e. the owning `serve` is provably gone, so its `NodeJobDir` Drop
/// will never fire. Liveness (not age) is the key: a long-running live job is never swept. Deterministic
/// given the predicate, so fully unit-testable.
pub fn orphan_scratch(
    entries: &[(String, Option<u32>)],
    is_alive: impl Fn(u32) -> bool,
    self_pid: u32,
) -> Vec<&str> {
    entries
        .iter()
        .filter(|(name, pid)| {
            name.starts_with(NODE_SCRATCH_PREFIX)
                && matches!(pid, Some(p) if *p != self_pid && !is_alive(*p))
        })
        .map(|(name, _)| name.as_str())
        .collect()
}

/// Is process `pid` currently alive? Used to spare a live job's scratch dir from GC. STRICTLY
/// fail-safe: returns `false` (→ eligible for sweep) ONLY on positive proof the process is gone;
/// every uncertain case (probe unavailable, permission denied, unknown error) returns `true` so a
/// live — or merely indeterminate — owner's dir is never deleted.
fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    let alive = {
        // Trust /proc only when it is positively authoritative — proven by OUR OWN pid being visible
        // there. A chroot/container with an empty or non-procfs /proc dir fails this, so we can't tell
        // → keep. Only when /proc/self resolves is a missing /proc/<pid> real proof of death.
        if Path::new("/proc/self").exists() {
            Path::new("/proc").join(pid.to_string()).exists()
        } else {
            true
        }
    };
    #[cfg(all(unix, not(target_os = "linux")))]
    let alive = match Command::new("kill").args(["-0", &pid.to_string()]).output() {
        Ok(o) if o.status.success() => true, // signalable → alive
        // `kill -0` failed: ESRCH ("no such process") is proof of death; EPERM (alive but not
        // signalable) and any other/locale-translated error → keep (assume alive).
        Ok(o) => !String::from_utf8_lossy(&o.stderr)
            .to_lowercase()
            .contains("no such process"),
        Err(_) => true, // couldn't even run `kill` → can't tell → keep
    };
    #[cfg(windows)]
    let alive = match Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    {
        // Trust the result ONLY on a successful enumeration: a matching row contains the pid → alive;
        // the "no tasks" INFO line does not → dead. A nonzero exit / enumeration error or a spawn
        // failure means we can't tell → keep.
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())
        }
        _ => true,
    };
    #[cfg(not(any(unix, windows)))]
    let alive = {
        let _ = pid;
        true
    };
    alive
}

/// Sweep node-scratch dirs left in the system temp dir by a crashed / killed `serve` whose owning
/// process is gone (a clean run removes its own via `NodeJobDir`'s Drop). Best-effort: unreadable
/// entries and failed removes are skipped, never fatal. Returns how many dirs were removed. Call once
/// on `serve` startup, AFTER the port is bound (so a duplicate serve that fails to bind never sweeps).
pub fn gc_node_scratch() -> usize {
    gc_node_scratch_in(&std::env::temp_dir(), pid_alive, std::process::id())
}

/// `gc_node_scratch` with an explicit temp root + liveness predicate + self pid, so it is hermetically
/// testable without depending on real process ids.
fn gc_node_scratch_in(temp: &Path, is_alive: impl Fn(u32) -> bool, self_pid: u32) -> usize {
    let read = match std::fs::read_dir(temp) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    // Collect (name, pid, path) for directories only — a node-prefixed *file* is never ours to remove.
    let mut dirs: Vec<(String, Option<u32>, PathBuf)> = Vec::new();
    for ent in read.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = match ent.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let pid = scratch_pid(&name);
        dirs.push((name, pid, path));
    }
    let listing: Vec<(String, Option<u32>)> =
        dirs.iter().map(|(n, p, _)| (n.clone(), *p)).collect();
    let doomed: std::collections::HashSet<&str> = orphan_scratch(&listing, is_alive, self_pid)
        .into_iter()
        .collect();
    let mut removed = 0;
    for (name, _, path) in &dirs {
        if doomed.contains(name.as_str()) && std::fs::remove_dir_all(path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(repo: &Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("seed"), "base").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "init"]);
        git(repo, &["branch", "-M", "main"]);
    }

    #[test]
    fn merge_fast_forward_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        git(repo, &["checkout", "-q", "-b", "ensemble/x"]);
        std::fs::write(repo.join("a"), "A").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "add a"]);
        git(repo, &["checkout", "-q", "main"]); // main is behind → fast-forward
        assert_eq!(
            merge_branch(repo, "ensemble/x", "main").unwrap(),
            MergeOutcome::Landed
        );
        assert!(repo.join("a").exists(), "ff should bring `a` onto main");
    }

    #[test]
    fn merge_diverged_non_conflicting_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        git(repo, &["checkout", "-q", "-b", "ensemble/y"]);
        std::fs::write(repo.join("b"), "B").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "b"]);
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("c"), "C").unwrap(); // main diverged on a different file
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "c"]);
        assert_eq!(
            merge_branch(repo, "ensemble/y", "main").unwrap(),
            MergeOutcome::Landed
        );
        assert!(repo.join("b").exists() && repo.join("c").exists(), "true merge keeps both");
    }

    #[test]
    fn merge_conflict_aborts_and_lists_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("foo"), "main-1\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "foo"]);
        git(repo, &["checkout", "-q", "-b", "ensemble/z"]);
        std::fs::write(repo.join("foo"), "branch-edit\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "edit foo on branch"]);
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("foo"), "main-edit\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "edit foo on main"]);
        match merge_branch(repo, "ensemble/z", "main").unwrap() {
            MergeOutcome::Conflict(paths) => {
                assert!(paths.contains(&"foo".to_string()), "conflicting paths: {paths:?}")
            }
            o => panic!("expected Conflict, got {o:?}"),
        }
        // aborted: tree restored to main's version, clean status
        assert_eq!(
            std::fs::read_to_string(repo.join("foo")).unwrap(),
            "main-edit\n"
        );
        let st = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(st.stdout.is_empty(), "worktree must be clean after merge --abort");
    }

    #[test]
    fn merge_refuses_dirty_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        git(repo, &["checkout", "-q", "-b", "ensemble/d"]);
        std::fs::write(repo.join("x"), "X").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "x"]);
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("seed"), "uncommitted-edit").unwrap(); // dirty worktree
        assert!(
            merge_branch(repo, "ensemble/d", "main").is_err(),
            "must refuse to merge onto a dirty worktree"
        );
    }

    /// main and `ensemble/z` both edit `foo` from a shared base → a true conflict; leaves `repo` on
    /// main with foo="main-edit\n".
    fn setup_conflict(repo: &Path) {
        init_repo(repo);
        std::fs::write(repo.join("foo"), "main-1\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "foo"]);
        git(repo, &["checkout", "-q", "-b", "ensemble/z"]);
        std::fs::write(repo.join("foo"), "branch-edit\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "edit foo on branch"]);
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("foo"), "main-edit\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "edit foo on main"]);
    }

    fn is_clean(repo: &Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap()
            .stdout
            .is_empty()
    }
    fn mid_merge(repo: &Path) -> bool {
        is_mid_merge(repo)
    }

    #[test]
    fn resolver_resolves_cleanly_and_lands_a_merge_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_conflict(repo);
        let out = merge_with_resolver(repo, "ensemble/z", "main", |r, paths| {
            assert_eq!(paths, &["foo".to_string()], "resolver gets the conflicting paths");
            std::fs::write(r.join("foo"), "RESOLVED\n")?; // marker-free resolution
            Ok(())
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Landed);
        assert_eq!(std::fs::read_to_string(repo.join("foo")).unwrap(), "RESOLVED\n");
        assert!(is_clean(repo), "worktree clean after a resolved land");
        // HEAD is a real merge commit (has a second parent)
        let second_parent = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "--quiet", "HEAD^2"])
            .output()
            .unwrap();
        assert!(second_parent.status.success(), "landed result must be a merge commit");
    }

    #[test]
    fn resolver_leaving_markers_escalates_and_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_conflict(repo);
        // a no-op resolver leaves git's conflict markers in foo → must NOT land
        let out = merge_with_resolver(repo, "ensemble/z", "main", |_r, _p| Ok(())).unwrap();
        assert_eq!(out, MergeOutcome::Conflict(vec!["foo".to_string()]));
        assert_eq!(
            std::fs::read_to_string(repo.join("foo")).unwrap(),
            "main-edit\n",
            "foo restored to main's pre-merge version"
        );
        assert!(is_clean(repo) && !mid_merge(repo), "restored to a pristine, non-merging state");
    }

    #[test]
    fn resolver_error_escalates_and_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_conflict(repo);
        let out = merge_with_resolver(repo, "ensemble/z", "main", |_r, _p| {
            Err(std::io::Error::other("resolver crashed"))
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Conflict(vec!["foo".to_string()]));
        assert_eq!(std::fs::read_to_string(repo.join("foo")).unwrap(), "main-edit\n");
        assert!(is_clean(repo) && !mid_merge(repo));
    }

    #[test]
    fn clean_merge_never_invokes_the_resolver() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        git(repo, &["checkout", "-q", "-b", "ensemble/y"]);
        std::fs::write(repo.join("b"), "B").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "b"]);
        git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("c"), "C").unwrap(); // diverged on a DIFFERENT file → no conflict
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "c"]);
        let out = merge_with_resolver(repo, "ensemble/y", "main", |_r, _p| {
            panic!("resolver must not run when the merge is clean");
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Landed);
        assert!(repo.join("b").exists() && repo.join("c").exists());
    }

    #[test]
    fn structural_modify_delete_conflict_escalates_without_running_resolver() {
        // branch modifies foo; main deletes foo → a markerless modify/delete conflict. A text
        // resolver can't safely resolve it AND `git add` would auto-pick a side, so it must escalate
        // WITHOUT invoking the resolver, and restore main (foo stays deleted).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        std::fs::write(repo.join("foo"), "orig\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "add foo"]);
        git(repo, &["checkout", "-q", "-b", "ensemble/m"]);
        std::fs::write(repo.join("foo"), "branch-mod\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "modify foo on branch"]);
        git(repo, &["checkout", "-q", "main"]);
        git(repo, &["rm", "-q", "foo"]);
        git(repo, &["commit", "-q", "-m", "delete foo on main"]);
        let pre = head_sha(repo).unwrap();

        let out = merge_with_resolver(repo, "ensemble/m", "main", |_r, _p| {
            panic!("resolver must NOT run on a markerless structural conflict");
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Conflict(vec!["foo".to_string()]));
        assert_eq!(head_sha(repo).unwrap(), pre, "main restored to its pre-merge tip");
        assert!(!repo.join("foo").exists(), "foo stays deleted (main's side)");
        assert!(is_clean(repo) && !mid_merge(repo));
    }

    #[test]
    fn resolver_that_commits_markers_is_undone_and_escalates() {
        // A misbehaving resolver that COMMITS the merge itself while markers survive must still be
        // caught (the file scan is staging/commit-independent) and undone via reset to pre_sha.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_conflict(repo);
        let pre = head_sha(repo).unwrap();
        let out = merge_with_resolver(repo, "ensemble/z", "main", |r, _p| {
            // leave foo's conflict markers in place, but commit the merge anyway
            git(r, &["add", "-A"]);
            git(r, &["commit", "--no-edit", "-q"]);
            Ok(())
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Conflict(vec!["foo".to_string()]));
        assert_eq!(head_sha(repo).unwrap(), pre, "the resolver's bad merge commit was undone");
        assert_eq!(std::fs::read_to_string(repo.join("foo")).unwrap(), "main-edit\n");
        assert!(is_clean(repo) && !mid_merge(repo));
    }

    #[test]
    fn resolver_committing_markers_then_cleaning_worktree_still_escalates() {
        // The sneakiest attack: commit the merge WITH markers, then overwrite the worktree marker-free
        // before returning. A worktree-only scan would see "clean" and land a marker-containing commit.
        // Because the resolver left the repo not-mid-merge, we refuse to trust the self-commit at all.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        setup_conflict(repo);
        let pre = head_sha(repo).unwrap();
        let out = merge_with_resolver(repo, "ensemble/z", "main", |r, _p| {
            git(r, &["add", "-A"]); // stage foo WITH conflict markers
            git(r, &["commit", "--no-edit", "-q"]); // commit the bad merge
            std::fs::write(r.join("foo"), "looks-clean\n").unwrap(); // hide markers in the worktree
            Ok(())
        })
        .unwrap();
        assert_eq!(out, MergeOutcome::Conflict(vec!["foo".to_string()]));
        assert_eq!(head_sha(repo).unwrap(), pre, "the bad self-commit was undone");
        assert_eq!(std::fs::read_to_string(repo.join("foo")).unwrap(), "main-edit\n");
        assert!(is_clean(repo) && !mid_merge(repo));
    }

    #[test]
    fn bundle_roundtrip_carries_remote_edits_back() {
        // ORCH: a repo at a base commit, with a worktree sitting at base.
        let orch_tmp = tempfile::tempdir().unwrap();
        let orch = orch_tmp.path();
        init_repo(orch);
        let wt = orch.join("wt");
        git(
            orch,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "ensemble/x",
                wt.to_str().unwrap(),
                "HEAD",
            ],
        );

        // ORCH → NODE: bundle the base.
        let base_bundle = bundle_rev(orch, "HEAD").unwrap();

        // NODE: materialize the base in its OWN temp dir, the "agent" writes a file, commit, bundle.
        let node_tmp = tempfile::tempdir().unwrap();
        let node = node_tmp.path().join("job");
        materialize_base(&node, &base_bundle, "HEAD", "dispatch/job-1").unwrap();
        std::fs::write(node.join("remote.txt"), "FROM-REMOTE").unwrap();
        assert!(commit_all(&node, "ensemble: job-1").unwrap());
        let result_bundle = bundle_branch(&node, "dispatch/job-1").unwrap();

        // NODE → ORCH: apply the result into the worktree.
        apply_result(orch, &wt, &result_bundle, "dispatch/job-1").unwrap();

        // the remote agent's file is now in the orchestrator's worktree
        assert_eq!(
            std::fs::read_to_string(wt.join("remote.txt")).unwrap(),
            "FROM-REMOTE"
        );

        // the temporary tracking ref was pruned (refs/ensemble/* must not accumulate per run).
        // NOTE: refs/ensemble/dispatch/job-1 lives in a different namespace from the worktree's
        // branch refs/heads/ensemble/x, so this only checks the tracking refs.
        let refs = std::process::Command::new("git")
            .arg("-C")
            .arg(orch)
            .args(["for-each-ref", "--format=%(refname)", "refs/ensemble/"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
            "refs/ensemble/* should be pruned after apply, found: {}",
            String::from_utf8_lossy(&refs.stdout)
        );
    }

    #[test]
    fn commit_all_reports_nothing_to_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // no new edits → Ok(false)
        assert!(!commit_all(repo, "noop").unwrap());
    }

    #[test]
    fn is_git_worktree_detects_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_git_worktree(tmp.path()));
    }

    #[test]
    fn scratch_pid_parses_owner_pid() {
        assert_eq!(scratch_pid("ensemble-node-codex-0-111-7"), Some(111));
        assert_eq!(scratch_pid("ensemble-node-a-1-0"), Some(1));
        assert_eq!(scratch_pid("other-dir"), None);
        // no job segment before the pid → not our shape → None (kept)
        assert_eq!(scratch_pid("ensemble-node-111-0"), None);
    }

    #[test]
    fn orphan_scratch_sweeps_only_dead_foreign_node_dirs() {
        let entries = vec![
            ("ensemble-node-codex-0-111-0".to_string(), Some(111u32)), // dead → swept
            ("ensemble-node-claude-1-222-3".to_string(), Some(222u32)), // alive → kept
            ("ensemble-node-mine-1-999-0".to_string(), Some(999u32)),  // our own pid → kept
            ("ensemble-stage-main-333-0.bundle".to_string(), Some(333)), // wrong prefix → kept
            ("some-other".to_string(), None),                          // unrelated → kept
        ];
        // only pid 222 is alive; self_pid = 999
        let doomed = orphan_scratch(&entries, |p| p == 222, 999);
        assert_eq!(doomed, vec!["ensemble-node-codex-0-111-0"]);
    }

    #[test]
    fn gc_removes_dead_keeps_live_and_ignores_files() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        std::fs::create_dir(r.join("ensemble-node-x-100-0")).unwrap(); // pid 100 dead → swept
        std::fs::create_dir(r.join("ensemble-node-y-200-1")).unwrap(); // pid 200 alive → kept
        std::fs::create_dir(r.join("keep-me")).unwrap(); // unrelated → kept
        std::fs::write(r.join("ensemble-node-not-a-dir"), b"x").unwrap(); // file → ignored

        let removed = gc_node_scratch_in(r, |p| p == 200, 0);
        assert_eq!(removed, 1, "only the dead-owner node dir is removed");
        assert!(!r.join("ensemble-node-x-100-0").exists());
        assert!(r.join("ensemble-node-y-200-1").exists(), "live job kept");
        assert!(r.join("keep-me").exists(), "unrelated dir untouched");
        assert!(
            r.join("ensemble-node-not-a-dir").exists(),
            "a node-prefixed FILE is never ours to remove"
        );
    }
}
