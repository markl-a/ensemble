# Fleet Federation — Night-Run Runbook (2026-06-21)

> Written for a middle-of-the-night execution. Operator actions are marked **[YOU]**; z13-side
> orchestration is marked **[Z13]** (Claude can run these in-session, or you run them yourself).
> Everything here is grounded in what was empirically proven on 2026-06-21.

---

## 0. TL;DR — cold → multi-machine governed run

1. **[YOU]** On every machine: turn **Surfshark OFF**, then `tailscale up`. (This is the #1 blocker —
   Surfshark's WireGuard collides with Tailscale's.)
2. **[YOU]** On each WORKER (ayaneo/acer/Mac): get the `ensemble` binary there and run `ensemble serve`.
   ayaneo is already serving. acer already has the binary Taildropped (just `serve`). A Mac must
   `cargo build --release` first (no cross-compile from Windows).
3. **[Z13]** `ensemble nodes` → confirm z13 sees each serving peer + its agents.
4. **[Z13]** Per peer, validate cheaply BEFORE any long run:
   `ensemble agent <cli> "Reply with exactly: VERDICT: LGTM" --node <peer>` (≈10–30s; the lesson from
   the opencode 23-minute waste — never start a long governed run with an unvalidated remote agent).
5. **[Z13]** Run a governed crew pinned across machines (see §6 for the crew.toml + command).
6. **[Z13]** Watch it live (if S1a landed): add `--watch fleet` to the run, and in another shell
   `ensemble watch fleet --follow`.

The single binary for all of this on z13 (release): `C:\ctgt\ensemble\release\ensemble.exe`
(git-bash path `/c/ctgt/ensemble/release/ensemble.exe`).

---

## 1. What is ALREADY proven today (don't re-prove)

- **All four cross-machine federation paths hold** (z13 ⇄ ayaneo, over tailnet):
  remote agent execution · remote reviewer in the gate · cross-machine auto-merge · remote edit-return.
- **The conductor's automated governed pipeline lands across machines**: implement → cross-vendor
  double-gate → land, with an agent running on a different box.
- **ayaneo is serving** (`100.107.205.98:7878`, all 4 CLIs `[ok]`).
- z13 is the conductor (online, all 4 CLIs, release binary built).
- Cross-machine commit naming has a minor cosmetic bug (a remote-bundled commit lands as
  `ensemble: codex-0` instead of the task text) — known follow-up, not a blocker.

So tonight is **operational** (bring up the remaining peers + run), not a research task.

---

## 2. The fleet (facts — verified `tailscale status` 2026-06-21)

Tailnet domain: `tailf31eff.ts.net`. tailnet user `m4932981@`.

| Friendly | tailnet name | IP | OS | Role | CLIs | Status tonight | Action |
|---|---|---|---|---|---|---|---|
| **z13** | yoyogood | 100.87.70.65 | Win 32T/111GB | **PRIME conductor** | all 4 | ✅ ready | run the show; do NOT `serve` (stay local conductor) |
| **ayaneo** | ayaneo | 100.107.205.98 | Win 8T/12.7GB | worker | all 4 | ✅ **serving** | leave it serving (don't close that window) |
| **acer** | laptop-gur943mk | 100.106.176.125 | Win 4T/24GB | worker | (verify) | active; binary Taildropped | **[YOU]** `ensemble serve` (§4a) |
| **M5** | marklmacbook-pro | 100.119.139.86 | macOS 24GB | worker (cross-platform) | (verify) | online, idle, NO binary | **[YOU]** build + serve (§4b) |
| **M1** | markmacbook-air | 100.87.93.58 | macOS 12GB | worker (intermittent) | (verify) | **was offline** | **[YOU]** wake → build → serve (§4b); optional |
| dev-host | dev-host | 100.115.176.90 | macOS | spare Mac | (verify) | online | alternative to M5/M1 if it already has a build |

z13 local CLI versions (re-verify with `ensemble doctor`): claude 2.1.x, codex 0.14x, opencode 1.1x,
agy 1.0.x. codex's node.json may say `working:false` — **that flag is stale; codex works** (it's half
the daily double-gate).

---

## 3. Pre-flight (do this first, on every machine)

**[YOU]** On z13, ayaneo, acer, and each Mac:
```
# 1. Disable Surfshark (its WireGuard tunnel blocks Tailscale). Then:
tailscale up
# 2. Sanity: you should see the other machines listed
tailscale status
```
**[Z13]** Confirm the conductor sees the tailnet and (after workers serve) their agents:
```
ensemble nodes        # lists each serving peer and the agents it offers
```
`ensemble nodes` was ~1.6s today even on this busy tailnet (the connect-timeout fix works). If it hangs,
a peer's Tailscale is down — fix that peer, don't wait.

---

## 4. Bring up each worker

### 4a. acer (Windows) — binary already Taildropped today
**[YOU]** On acer, in PowerShell:
```powershell
# pull whatever Taildrop delivered (Windows GUI may auto-save copies to Downloads as ensemble (N).exe)
tailscale file get $HOME
# take the NEWEST copy, confirm size = 4300288 (today's build), copy to a clean name
$src = Get-ChildItem "$HOME\Downloads\ensemble*.exe" | Sort-Object LastWriteTime | Select-Object -Last 1
Copy-Item $src.FullName "$HOME\ens.exe" -Force
& "$HOME\ens.exe" doctor      # note: [MISSING] git-repo is FINE for serve
& "$HOME\ens.exe" serve       # prints "ensemble serve on 100.106.176.125:7878" — leave it running
```
> PowerShell gotcha: to run a quoted exe path you MUST prefix `&` (the call operator). `"C:\..\ens.exe" serve`
> without `&` is a parser error.

If acer's Taildrop inbox is empty, **[Z13]** re-send: `tailscale file cp C:\ctgt\ensemble\release\ensemble.exe laptop-gur943mk:`

### 4b. A Mac (M5 / dev-host / M1) — build from source (no cross-compile from Windows)
**[YOU]** On the Mac:
```bash
# 1. Surfshark OFF (if on), tailscale up
# 2. Rust (if not installed): curl https://sh.rustup.rs -sSf | sh   (then: source ~/.cargo/env)
# 3. Get the repo (git clone the ensemble repo, or copy it over), then:
cd <ensemble-repo>
cargo build --release          # produces ./target/release/ensemble (aarch64-apple-darwin)
./target/release/ensemble doctor    # see which CLIs this Mac has
./target/release/ensemble serve     # binds this Mac's tailnet IP:7878 — leave it running
```
> If dev-host already has an ensemble build from before, just `ensemble serve` it — fastest Mac path.
> M1 is the least reliable (offline + intermittent + 12GB); treat it as optional. If it won't wake/build,
> a 4-machine fleet (z13 + ayaneo + acer + one Mac) is the solid result and already spans both platforms.

---

## 5. Validate each peer BEFORE the big run (cheap insurance)

**[Z13]** For each serving peer, confirm reachability + that the agent emits a clean verdict promptly:
```
ensemble agent claude "Reply with exactly: PONG" --node <peer>
ensemble agent agy    "Reply with exactly: VERDICT: LGTM" --node <peer>
```
`<peer>` can be the short tailnet name (e.g. `ayaneo`, `laptop-gur943mk`, `marklmacbook-pro`) — explicit
`--node` skips discovery and is fast. Today: claude@ayaneo PONG in 9s, agy@ayaneo verdict in 27s.
**If a peer's agent doesn't answer cleanly in <~30s, do NOT put it in a governed crew** — fix it first.

---

## 6. The federation run

### ⚠️ Read this first — the topology constraint (the key architectural fact)

ensemble routes adapters **per AGENT NAME**, not per role: `[agents.codex] node = X` sends EVERY codex
role to X. There are only 4 agent names (codex/claude/opencode/agy). So:
- A **single governed run** can pin at most **4 distinct (agent, node) pairs** — i.e. up to 4 machines,
  one per vendor. With `opencode` excluded (it HANGS headless — §8), that's **3 reliable machines** per
  single run (codex + claude + agy on 3 nodes).
- To genuinely exercise **all 5 machines at once**, run **several governed runs in parallel**, each with a
  crew pinned to a different machine subset (see §6c). The fleet infra (serve / discovery / RemoteAdapter /
  repo_sync) supports this; there is no single "spread one task across 5 boxes" command.

### 6a. RECOMMENDED single-run demo: 3 machines, cross-vendor, cross-platform

A crew that pins the implementer + two reviewers to three different machines. Use codex + claude as the
reliable workhorses; agy only as an EXTRA (see §8 — agy can't reliably see worktree files, so don't let it
be the SOLE reviewer of a file-existence task). Best reliable shape: **codex impl + claude + (codex OR
claude on another node) review**. Since the same vendor can't split nodes, the clean 3-machine reliable
crew is **codex@z13 (impl) + claude@<MacOrPeer> (review) + agy@<thirdPeer> (review2)**, min_approvals=2,
with the understanding that agy's vote is the soft one.

`fleet-crew.toml`:
```toml
pipeline = ["implement", "review", "review2"]

[gate]
min_approvals = 2
max_rounds    = 2
on_flake      = "exclude"
stall_limit   = 0
max_task_secs = 0

[roles.implement]
agent = "codex"            # runs on z13 (no node = local conductor)

[roles.review]
agent = "claude"
blind = true

[roles.review2]
agent = "agy"
blind = true

# pin reviewers to other machines (use each peer's full serve URL):
[agents.claude]
node = "http://marklmacbook-pro.tailf31eff.ts.net:7878"   # claude reviews ON M5 (macOS leg)

[agents.agy]
node = "http://laptop-gur943mk.tailf31eff.ts.net:7878"    # agy reviews ON acer
```
**[Z13]** Run it (the repo MUST have `.ensemble/` and `crew.toml` gitignored so the worktree stays clean
for `--merge` — see §8):
```
ensemble run "<your task>" --crew fleet-crew.toml --repo <repo> --no-discover --merge --watch fleet
```
- `--no-discover` keeps codex local and honors the explicit reviewer nodes (explicit node always wins).
- `--watch fleet` streams it live (S1a) → in another shell: `ensemble watch fleet --follow`.
This is **codex@z13 + claude@M5 + agy@acer** = 3 machines, Win + macOS, governed, auto-merging. That IS
federated joint operation.

### 6b. Most reliable 2-machine alternative (if a Mac isn't up)

codex@z13 (impl) + claude@ayaneo (review), min_approvals=1 — guaranteed-reliable, lands cleanly. Proven
shape (today's z13↔ayaneo runs). Drop `review2`/`[agents.agy]` from the crew, set `min_approvals = 1`.

### 6c. TRUE 5-machine utilization: parallel pinned runs

To have all 5 boxes working at once, launch several runs concurrently, each crew pinned to a different
machine. e.g. (each is its own `fleet-crew-N.toml` with different `[agents.X] node = ...`):
```
ensemble run "task A" --crew crewA.toml --repo <repoA> --no-discover --merge --watch runA &
ensemble run "task B" --crew crewB.toml --repo <repoB> --no-discover --merge --watch runB &
# ... watch each: ensemble watch runA --follow   (separate shells)
```
Use SEPARATE repos/worktrees per concurrent run to avoid them mutating the same tree. This is the honest
way to say "all 5 machines are collaborating."

---

## 7. Watch it live (S1a — `ensemble watch`)

If S1a has landed (it was double-gating overnight), any run started with `--watch <name>` streams its
events into `<repo>/.ensemble/stream/<name>.ndjson`, renderable live:
```
ensemble watch <name> --follow      # [codex · result] … / [claude · verdict] VERDICT: LGTM / [conductor · decision] LANDED
```
This is the "看得到" capability. (Live INJECT/ABORT — steer/abort — is the NEXT S1 sub-slice, not built
yet; today you can still Ctrl-C a run to stop it at the next round boundary.)

---

## 8. Gotchas / troubleshooting (everything we hit today)

| Symptom | Cause | Fix |
|---|---|---|
| Peers can't reach each other | **Surfshark** (WireGuard) collides with Tailscale | Surfshark OFF, then `tailscale up`. #1 blocker. |
| `tailscale file get` finds nothing, but `ensemble.exe` is in Downloads | Windows Tailscale GUI auto-accepts Taildrop into **Downloads** (as `ensemble (N).exe`) | use the newest `Downloads\ensemble*.exe`; copy to a clean name |
| `"C:\..\ens.exe" doctor` → "unexpected token" | PowerShell treats a quoted path as a string | prefix with the call operator: `& "C:\..\ens.exe" doctor` |
| A governed run wastes ~10–20 min then escalates | **opencode HANGS** as a headless reviewer (600s timeout ×2) | **keep opencode OUT of governed crews**; use codex/claude (and agy as a soft extra) |
| Reviewer falsely says "file does not exist" | **agy** can't reliably see the worktree files under ConPTY (it referenced a phantom `diff.txt`) | don't make agy the SOLE reviewer of a file-existence task; pair it with codex/claude |
| `--merge` → "worktree not clean — commit or stash" | the run repo has untracked `crew.toml` / `.ensemble/` | add a `.gitignore` with `.ensemble/` and `crew.toml` in the run repo |
| `ensemble doctor` says `[MISSING] git-repo` on a worker | doctor checks cwd-is-git-repo | irrelevant for `serve` (remote runs sync their own repo) — ignore on workers |
| `ensemble nodes` slow / a peer unreachable | that peer's Tailscale is down or it isn't serving | fix the peer; explicit `--node <name>` skips discovery meanwhile |

SSH automation to peers is **blocked** (codex sandbox ACL polluted `~/.ssh/config`), so you can't remote-run
`serve` on a peer — it must be started ON each machine. z13 can push the binary via `tailscale file cp`,
but the peer's human runs `serve`.

---

## 9. Who to put where (agent reliability — proven today)

| Agent | Implementer? | Reviewer? | Notes |
|---|---|---|---|
| **codex** | ✅ reliable | ✅ reliable, sees files | the workhorse |
| **claude** | ✅ reliable | ✅ reliable, sees files + emits clean verdicts | the other workhorse |
| **agy** | ⚠️ usable | ⚠️ emits verdict but **can't reliably see worktree files** (ConPTY) | soft extra only; ~slow |
| **opencode** | ❓ untested headless | ❌ **HANGS** (600s timeout) | **avoid in governed crews** until per-agent timeout (backlog item 6) |

Reliable gate pairs: **codex + claude** (the proven double-gate). For a 3rd machine, agy is the only
remaining option (opencode hangs) — keep its vote non-decisive (min_approvals reachable without it).

---

## 10. Decisions locked + open questions for you

**Locked:** Council broadcast (`ensemble all`) chosen over Tournament/Broadcast-steer (backlog 0.7);
WSL node shares z13's tunnel; OSS release is scrub-first (don't make the repo public until docs are
scrubbed of IPs/hostnames/email — this runbook + the Stage-2 plan + the backlog all need scrubbing).

**Open (answer when you're up):**
1. M1 tonight — wake + build it for the 5th machine, or settle for the solid 4 (z13+ayaneo+acer+M5)?
2. Single 3-machine governed run (§6a) vs parallel pinned runs across all 5 (§6c) — which do you want to
   demo first?
3. Real task for the fleet: a trivial proof task, or point it at a real **phantom-mesh** change (the
   end goal — federated dev of phantom-mesh)?
4. Once a Mac is up, which vendor reviews there (claude recommended — reliable + sees files)?

**Plan it sits next to:** `docs/plans/2026-06-21-stage2-federation-and-oss-release.md` (fuller topology +
OSS-release detail). Fleet hardware/CLI table + reliability notes are also in the project memory.
