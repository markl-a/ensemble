# Phase 2 End State — Cross-Machine Governed Crew

Status: Proposed
Date: 2026-06-23

狀態：提案中
日期：2026-06-23

## Purpose / 目的

Phase 2 is complete when the Phase 1 single-machine workflow can be expanded to several trusted
machines without changing the operator's mental model.

第二階段完成的定義是：第一階段的單機流程可以自然擴展到多台可信任機器，而且使用者不需要換一套操作模型。

The operator still uses `ensemble <cli>`, `team`, `watch`, `steer`, `abort`, `supervise`, and
governed `run`. Each machine can host local AI CLIs as controlled members. Any trusted node can
observe, steer, or abort a controlled member running on another node. Governed work can be executed
across machines and still lands only after the same test gate and cross-vendor approval rules.

使用者仍然使用 `ensemble <cli>`、`team`、`watch`、`steer`、`abort`、`supervise` 和受治理的 `run`。每台機器都可以把本地 AI CLI 作為受控成員加入團隊。任一可信任節點都可以觀察、修正或中斷另一台機器上的受控成員。跨機執行的工作仍然必須通過相同的測試閘門與跨 vendor 審核規則，才能 landing。

The endpoint is not "HTTP routes exist". The endpoint is a clean reinstall on at least two real
machines, followed by a real repo task where the crew spans machines, remains observable and
interruptible, and lands through governance.

終點不是「HTTP route 已經存在」。終點是至少兩台真實機器可以乾淨重裝，接著在真實 repo 上執行一個跨機團隊任務；過程可觀察、可修正、可中斷，最後經過治理後 landing。

## End-State User Experience / 終點使用體驗

From a fresh install on each machine:

從每台機器全新安裝開始：

1. The operator installs `ensemble` and runs `ensemble doctor`.
   使用者安裝 `ensemble`，並執行 `ensemble doctor`。
2. The operator starts a node with `ensemble up` or a user-level service.
   使用者透過 `ensemble up` 或 user-level service 啟動節點。
3. `ensemble mesh` shows local and tailnet peers with available AI CLIs.
   `ensemble mesh` 顯示本機與 tailnet peers，以及每台機器可用的 AI CLI。
4. The operator can start controlled members with the same local syntax:
   使用者可以用和單機相同的語法啟動受控成員：

```powershell
ensemble --team phase2 codex
ensemble --team phase2 claude
ensemble --team phase2 opencode
ensemble --team phase2 agy
```

A member can be local or remote, but it joins the same team identity model and can be addressed by a
stable name such as `claude@macbook` or by an explicit `--node`.

成員可以是本機或遠端，但它加入的是同一套 team identity model，並且可以用穩定名稱，例如 `claude@macbook`，或明確的 `--node` 來指定。

From any operator shell or MCP-capable lead CLI, the operator can run:

從任一使用者 shell 或支援 MCP 的主控 AI CLI，可以執行：

```powershell
ensemble team status --team phase2
ensemble watch claude@macbook
ensemble steer claude@macbook "Refocus on the failing test"
ensemble abort claude@macbook
ensemble watch claude@macbook --node macbook
```

The explicit `--node` form is still available and takes precedence when the operator wants to force a
specific route. `--node local` is the escape hatch for forcing the file-backed local plane when a
local member name contains an `@` suffix. Explicit loopback targets such as `--node localhost` still
use the HTTP remote-control path, which lets the same token boundary be tested on one machine.

明確的 `--node` 形式仍然可用；當使用者想強制指定 route 時，它的優先權最高。當本機 member name 本身含有 `@` suffix 時，`--node local` 可以強制使用 file-backed local plane。明確指定的 loopback 目標，例如 `--node localhost`，仍然會走 HTTP remote-control path，方便在單機測同一套 token 邊界。

A governed run can pin roles to nodes in `crew.toml` or rely on discovery. Remote edits return to
the conductor, tests run, reviewers issue verdicts, and code lands only after the configured
approval policy passes.

受治理的 run 可以在 `crew.toml` 把角色固定到指定 node，也可以依賴 discovery 自動解析。遠端修改會回到 conductor，接著跑測試、取得 reviewer verdict，最後只有在設定的 approval policy 通過後才 landing。

## Required Capabilities / 必要能力

### 1. Phase 1 Regression Stays Green / 第一階段回歸必須維持通過

Every Phase 2 slice must keep the single-machine path working:

每一個第二階段 slice 都必須維持單機路徑可用：

- team board/status/inbox;
  team board/status/inbox；
- MCP team and control tools;
  MCP team 與 control tools；
- controlled `codex`, `claude`, `opencode`, and `agy` launch;
  受控的 `codex`、`claude`、`opencode` 與 `agy` 啟動；
- local `watch`, `steer`, and `abort`;
  本機 `watch`、`steer` 與 `abort`；
- `supervise`;
  `supervise`；
- install/uninstall readiness;
  install/uninstall readiness；
- deterministic single-machine acceptance script.
  deterministic single-machine acceptance script。

Phase 2 must not fork the UX into "local commands" and "remote commands". Remote control is an
extension of the same command surface.

第二階段不能把 UX 分裂成「本地指令」和「遠端指令」。遠端控制應該是同一組指令表面的延伸。

### 2. Remote Control Plane Is Operator-Visible / 遠端控制平面必須能被使用者操作

The already-started `ControlPlane` boundary must be reachable from CLI and MCP:

已經開始實作的 `ControlPlane` 邊界必須能從 CLI 和 MCP 操作：

- local path uses `LocalControlPlane`;
  本機路徑使用 `LocalControlPlane`；
- remote path uses `RemoteControlPlane`;
  遠端路徑使用 `RemoteControlPlane`；
- explicit routing works with `--node <host-or-url>`;
  明確 routing 可透過 `--node <host-or-url>` 運作；
- member routing can resolve `member@node` or an equivalent stable identity;
  member routing 可以解析 `member@node` 或等價的穩定 identity；
- `team_status`, `team_say`, `team_inbox`, `watch`, `steer`, and `abort` share the same semantics
  locally and remotely.
  `team_status`、`team_say`、`team_inbox`、`watch`、`steer` 與 `abort` 在本機與遠端維持相同語意。

第一個可接受的遠端實作可以仍然是 node-local：讀寫目標 node 自己的 `.ensemble/` 狀態。但第二階段的最終終點需要跨 node 的一致團隊視圖，可能透過 shared coordinator，也可能透過明確記錄的同步模型完成。

The first acceptable remote implementation may remain node-local: it reads and mutates the target
node's `.ensemble/` state. The final Phase 2 endpoint needs a coherent team view across nodes, either
through a shared coordinator or a documented synchronization model.

### 3. Cross-Machine Team State Is Coherent / 跨機團隊狀態必須一致可理解

Remote nodes cannot feel like isolated local boards. The operator must be able to answer:

遠端 node 不能像彼此孤立的本機 board。使用者必須能回答：

- which nodes are online;
  哪些 node 在線；
- which controlled members are running;
  哪些受控成員正在執行；
- which member owns which work;
  哪個 member 負責哪個工作；
- what each member recently said or did;
  每個 member 最近說了什麼或做了什麼；
- what controls were sent and whether they were observed.
  哪些 control 指令已送出，以及對方是否觀察到。

Acceptable designs include a coordinator-backed control plane, HTTP pull federation, or a clearly
bounded replicated ledger. The chosen design must preserve append-only audit behavior and avoid
silently losing `steer` or `abort` commands.

可接受設計包含 coordinator-backed control plane、HTTP pull federation，或邊界清楚的 replicated ledger。選定設計必須保留 append-only audit 行為，並避免靜默遺失 `steer` 或 `abort` 指令。

### 4. Live Control Works Across Machines / Live Control 必須跨機可用

A controlled member launched by `ensemble <cli>` on machine B must be controllable from machine A:

在機器 B 透過 `ensemble <cli>` 啟動的受控成員，必須可以從機器 A 控制：

- `watch` shows recent session/team activity;
  `watch` 顯示近期 session/team activity；
- `steer` injects a corrective prompt or queues it at the next safe point;
  `steer` 注入修正 prompt，或在下一個安全點排隊送出；
- `abort` performs a soft interrupt;
  `abort` 執行 soft interrupt；
- `abort --hard` terminates the owned child process where the backend supports it;
  `abort --hard` 在 backend 支援時終止受管理的 child process；
- failures are visible as board/stream events, not silent hangs.
  failure 必須以 board/stream event 顯示，而不是靜默卡住。

對 PTY-only CLI 而言，精準的 mid-turn 行為可以仍然受 vendor 影響，但 command 不能無限卡住。如果 backend 無法套用某個 control，必須清楚回報限制。

For PTY-only CLIs, exact mid-turn behavior can remain vendor-dependent, but the command must not hang
indefinitely. If a backend cannot apply a control, it must report that limitation clearly.

### 5. Node Discovery And Routing Are Stable / Node Discovery 與 Routing 必須穩定

The operator should not have to hand-wire every run after initial bring-up:

初次 bring-up 後，使用者不應該每次都手動接線：

- `ensemble mesh` shows reachable peers and hosted CLIs;
  `ensemble mesh` 顯示可連線 peers 與其 hosted CLIs；
- `crew.toml` can pin an agent to a node;
  `crew.toml` 可以把 agent 固定到指定 node；
- unpinned agents can be resolved from discovery;
  未固定的 agent 可以透過 discovery 解析；
- explicit `--node` always wins;
  明確指定的 `--node` 永遠優先；
- node names and member names are stable across restarts;
  node names 與 member names 在 restart 後仍保持穩定；
- offline or slow peers degrade quickly and visibly.
  offline 或 slow peers 必須快速且可見地降級。

### 6. Security Boundary Is Explicit / 安全邊界必須明確

Remote control can push prompts into powerful local CLIs, including sessions launched with permissive
vendor flags. Phase 2 therefore needs a minimal but real trust boundary:

遠端控制可以把 prompt 推進權限很高的本地 CLI，包含用 permissive vendor flags 啟動的 session。因此第二階段需要最小但真實的信任邊界：

- serve binds to tailnet IP or loopback, never implicit `0.0.0.0`;
- remote mutation routes require an auth mechanism such as `ENSEMBLE_TOKEN` or a tailnet identity
  allow-policy;
- the current shared-secret implementation uses the `x-ensemble-token` header, with server/client
  tokens supplied by `--token <token>` or `ENSEMBLE_TOKEN`;
- unsafe repo paths and feed/member names are rejected server-side;
- public docs describe that tailnet peers with control access can influence live AI CLI sessions.

中文對應：

- serve 只綁定 tailnet IP 或 loopback，不隱式綁定 `0.0.0.0`；
- 遠端 mutation routes 需要 auth 機制，例如 `ENSEMBLE_TOKEN` 或 tailnet identity allow-policy；
- 目前 shared-secret 實作使用 `x-ensemble-token` header，server/client token 來源是 `--token <token>` 或 `ENSEMBLE_TOKEN`；
- server-side 會拒絕不安全的 repo path 與 feed/member name；
- public docs 必須說明：有 control access 的 tailnet peer 可以影響 live AI CLI session。

### 7. Service Install Is Usable / Service Install 必須可用

For the cross-machine endpoint, manually starting `ensemble serve` on every node is not enough.
Phase 2 should include user-level service lifecycle:

對跨機終點而言，每台 node 都手動啟動 `ensemble serve` 還不夠。第二階段應包含 user-level service lifecycle：

- install;
  install；
- uninstall;
  uninstall；
- status;
  status；
- start/stop or a documented equivalent;
  start/stop，或文件化的等價操作；
- Windows, macOS, and Linux/WSL coverage where practical.
  在可行範圍內覆蓋 Windows、macOS 與 Linux/WSL。

The service must use the same bind/auth behavior as foreground `ensemble up`.

service 必須使用與前景 `ensemble up` 相同的 bind/auth 行為。

### 8. Governed Cross-Machine Landing Works / 受治理的跨機 Landing 必須可用

The governance property is the product's core. A Phase 2 run must prove:

治理屬性是這個產品的核心。第二階段 run 必須證明：

- at least one implementer or reviewer runs on a remote node;
  至少一個 implementer 或 reviewer 在遠端 node 上執行；
- remote edits return to the conductor;
  遠端 edits 會回到 conductor；
- the test gate runs before approval;
  approval 前必須先通過 test gate；
- at least two distinct-vendor approvals are required when configured;
  當設定要求時，至少需要兩個不同 vendor 的 approval；
- flaked, missing, or stuck reviewers do not count as approval;
  flaked、missing 或 stuck reviewers 不計入 approval；
- merge conflicts are escalated or resolved through the existing safe merge path, never forced.
  merge conflicts 必須升級處理，或透過既有 safe merge path 解決，不能強制合併。

## Acceptance Gates / 驗收門檻

### Automated Gates / 自動化門檻

- Phase 1 focused regression: `control`, `team_`, `launcher`, and the single-machine acceptance
  script remain green.
- Remote control-plane contract tests cover success and rejection paths.
- CLI routing tests cover local default, explicit `--node`, and invalid target handling.
- Discovery/routing tests cover offline nodes and explicit-node precedence.
- Service install render/plan tests are hermetic per platform.

中文對應：

- 第一階段 focused regression：`control`、`team_`、`launcher` 與 single-machine acceptance script 必須維持通過。
- remote control-plane contract tests 必須覆蓋成功與拒絕路徑。
- CLI routing tests 必須覆蓋本地預設、明確 `--node`、invalid target handling。
- discovery/routing tests 必須覆蓋 offline nodes 與 explicit-node precedence。
- service install render/plan tests 必須針對各平台 hermetic。

### Operator Gates / 使用者實機門檻

- Windows local single-machine test still passes.
- macOS local controlled CLI test passes for at least codex, claude, and opencode if installed.
- Two-machine smoke: machine A watches, steers, and aborts a controlled member on machine B.
- Governed cross-machine repo task: one machine conducts, at least one other machine runs a role, and
  the result lands only after the configured gate.
- Clean reinstall rehearsal: uninstall, reinstall, start service, rejoin mesh, run a small real task.

中文對應：

- Windows 本地單機測試仍然通過。
- macOS 本地 controlled CLI 測試至少通過 codex、claude、opencode，如果已安裝。
- 兩機 smoke：機器 A 可以 watch、steer、abort 機器 B 上的受控成員。
- 受治理跨機 repo task：一台機器擔任 conductor，至少另一台機器執行角色，結果只有在設定 gate 通過後 landing。
- clean reinstall rehearsal：uninstall、reinstall、start service、rejoin mesh，接著跑一個小型真實 task。

## Non-Goals For Phase 2 / 第二階段非目標

- A public web dashboard.
  公開 web dashboard。
- A general terminal multiplexer.
  通用 terminal multiplexer。
- Replacing the native vendor CLI interfaces.
  取代 vendor CLI 原生介面。
- Counting raw PTY transcripts as governance-grade review evidence.
  把 raw PTY transcript 當成 governance-grade review evidence。
- Full internet-hosted multi-tenant service operation.
  完整 internet-hosted multi-tenant service operation。
- Open-source release polish, except where install/uninstall and docs are required for the Phase 2
  rehearsal.
  開源 release polish，除非它是第二階段 rehearsal 所需的 install/uninstall 或 docs。

## Practical Completion Definition / 實際完成定義

Phase 2 is done when the operator can sit at one machine, start or discover a team spread across at
least one other machine, observe and correct live members, and complete a governed development task
against a real repo without manual networking, manual file copying, or fake-green approvals.

第二階段完成時，使用者應該能坐在一台機器前，啟動或發現至少橫跨另一台機器的團隊，觀察並修正 live members，並在不手動處理 networking、不手動複製檔案、不假裝 green approval 的前提下，完成一個真實 repo 的受治理開發任務。
