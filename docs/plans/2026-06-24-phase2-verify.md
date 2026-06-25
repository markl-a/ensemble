# Phase 2 驗收作法（Phase 2 Verify）

本文件對應 `/goal` 中的 Phase 2 目標，提供「可自動化（本機）」與「跨機時需手動接上」兩層驗收流程。

## 準備

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble
```

若要在上五機前一次跑完本機 readiness（Phase 1 deterministic acceptance、Phantom bridge、Phase 2 Slice A/B-preflight/C-local），用：

```powershell
pwsh -NoProfile -File scripts\phase2-local-ready.ps1 -Repo D:\Projects\ensemble
```

這個 wrapper 只串接既有 verifier，不取代下方每個 Slice 的完成條件。預設會要求 `phantom` 在 PATH 上；若此機器刻意不測 Phantom bridge，需顯式加 `-SkipPhantom`。預設也會跑 `cargo test --test cross_machine`，用 in-process `serve`/`RemoteAdapter` 驗證跨機治理 invariant；快速排障時可顯式加 `-SkipCrossMachineRegression`。`Slice D` clean reinstall 會動到 user-level install，需顯式加 `-RunCleanReinstall` 才會跑。

建議先確保：

- `ensemble` 可在 PATH 取到（或 `-TargetDir` 有對應 `target/*/ensemble.exe`）
- `git` 可用
- `crew-main.toml`（或你自己的 `--crew`）可用於 `ensemble run`；若尚未產生，Slice B 會退回本機範例 `examples/crew-phase2.toml`

## 目前已驗證的本機證據（2026-06-25）

- `cargo build --release --bin ensemble --target-dir <tmp-target>` 通過
- `scripts\acceptance-single-machine.ps1 -NoBuild -TargetDir <tmp-target> -AgyTimeoutSecs 1` 通過，覆蓋 team status/say/inbox、watch/steer/abort、MCP team/control tools、controlled codex/agy launcher、bounded agy 可見 result/flake
- `scripts\smoke.ps1 -NoBuild -TargetDir <tmp-target> -TimeoutSecs 180 -AgyTimeoutSecs 1` 通過，實際跑出 `codex -> test gate -> claude -> LANDED -> merge`，且 `phase2-run-evidence.ps1` 驗證 team/watch terminal evidence，`supervise` 回傳 `on_track`
- `scripts\phase2-verify.ps1 -TargetDir <tmp-target> -SkipSliceA -SkipSliceB -SkipSliceC -UpBind 127.0.0.1:0` 通過 Slice D：baseline uninstall、install、service dry-run、smoke、up、mesh、nodes、final uninstall；結束後本機 install binary 與 User PATH entry 都已清掉
- `scripts\phantom-single-machine.ps1 -Repo D:\Projects\ensemble -TargetDir <tmp-target> -NoBuild` 通過，覆蓋 Phantom -> shell tool -> ensemble `agent --node local` -> local Codex 的單機橋接路徑；這只證明 Phantom 可在單機調用 ensemble，不等於 Phase 2 五機 fleet 已完成
- `scripts\phantom-single-machine.ps1 -Repo <phantom-repo> -TargetDir D:\tmp\ensemble-phase2-local-ready-target -NoBuild -Agent codex -Prompt PONG` 通過，覆蓋 Phantom 在目標 Phantom repo 內透過 shell tool 調用 ensemble 並強制 `--node local`（本機 private path 不寫入文件）
- `scripts\phase2-local-ready.ps1 -Repo D:\Projects\ensemble -TargetDir <tmp-target> -SmokeRoot <tmp-root>` 通過，串接 Phase 1 deterministic acceptance、Phantom bridge、Slice A、Slice B governance preflight、Slice C local mesh/nodes、`cross_machine` hermetic regression；`-SkipPhase1 -SkipPhantom -SkipCrossMachineRegression` 快速路徑與全 skip 防呆也通過
- 2026-06-25 再次執行 `scripts\phase2-local-ready.ps1 -Repo D:\Projects\ensemble -TargetDir D:\tmp\ensemble-phase2-local-ready-target -SmokeRoot D:\tmp\ensemble-phase2-local-ready-rerun -AgyTimeoutSecs 1` 通過；同日用 `scripts\install.ps1 -SourceExe D:\tmp\ensemble-phase2-local-ready-target\release\ensemble.exe` 將通過驗證的 binary 裝回 `%LOCALAPPDATA%\ensemble\bin`。Windows 既有 PowerShell process 不會自動刷新 User PATH，手動測試請新開 terminal 後再直接使用 `ensemble`。

這些證據只證明本機 baseline 與 clean reinstall path；真實 5-node Slice B/C 仍需在 m1~m5 fleet 上跑。

## Slice A：控制面（控制 plane）

必要項：

- `ensemble team status --team <team> --json`
- `ensemble nodes`
- `ensemble watch <member[@node]> --team <team> --since 0`
- 若 fleet manifest 設定 `service.port`（例如 `8788`），裸 host 與 `member@node` 控制面命令需同時加 `--port <service.port>`；explicit URL 或 `host:port` 仍以 URL/host 內的 port 為準。

自動驗證：

- `scripts/phase2-verify.ps1` 會先執行 `team status/watch/nodes`，並驗證 watch 的錯誤路由：
  - `team status --node local` 必須強制使用本機 file-backed control plane
  - `team status --node auto` 必須**非零失敗**，並回傳明確訊息（`--node auto is not supported`）
  - `watch codex@auto --node auto` 必須**非零失敗**，並回傳明確訊息（`--node auto is not supported`）
  - `--port <n>` 必須套用到裸 host 與 `member@node` discovery，不可仍打預設 `7878`
- 同一個 Slice A 會啟動暫時的 loopback remote control server：
  - `ensemble serve --bind 127.0.0.1:<free-port> --token <ephemeral>`
  - 經由 `--node http://127.0.0.1:<port>` 驗證 remote `team status/say/inbox`、`watch`、`steer`、`abort`
  - 另用非 local-escape 的 loopback alias 驗證 `watch/steer/abort <name>@127.0.0.2:<port>` suffix routing，不帶 `--node` 也必須走遠端 control plane
  - mutation（`team say`、`steer`、`abort`）必須帶正確 token；錯 token 必須明確非零失敗且包含 `Unauthorized`
- 跨機時，若要測 `member@node`，請設定 `-RemoteNode <node>`

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -Team main -RemoteNode m1 -ServicePort 8788
```

## Slice B：跨機 run + intervention

必要項：

- `ensemble run "<task>" --crew <crew.toml> --repo <repo> --team <team> --watch <watch>`
- `ensemble watch <watch> --follow`（可選）
- `ensemble steer <watch> "<prompt>"`
- `ensemble abort <watch> [--hard]`
- run 前先記下三個 feed cursor，run 結束後用 `scripts\phase2-run-evidence.ps1` 讀取 team/watch/control 證據：

```powershell
$teamCursor = (ensemble team inbox --repo <repo> --team <team> --json | ConvertFrom-Json).next
$watchCursor = @((ensemble watch <watch> --repo <repo> --team <team> --since 0 --json 2>$null) | Where-Object { $_.Trim() }).Count
$controlPath = Join-Path <repo> ".ensemble/control/<watch>.ndjson"
$controlCursor = if (Test-Path $controlPath) { @((Get-Content $controlPath) | Where-Object { $_.Trim() }).Count } else { 0 }

# run finishes here
pwsh scripts\phase2-run-evidence.ps1 -Repo <repo> -Team <team> -Watch <watch> -TeamSince $teamCursor -WatchSince $watchCursor -RequireControl -ControlSince $controlCursor
```

自動驗證：

- 先執行 `ensemble crew inspect --crew <crew.toml> --json`，檢查 Phase 2 governance：
  - `gate.min_approvals >= 2`
  - 必須有 `[test] command = ...`
  - 至少兩個 reviewer role，且至少兩個不同 reviewer vendor
  - 若正式測跨機 crew，加 `-RequireExplicitRemoteAgents`，會要求至少一個 active pipeline role 有 `[agents.<name>].node`
- 執行 `ensemble run`，檢查輸出包含 `LANDED` 或 `ESCALATED`
- `ensemble watch <watch> --since 0` 至少可讀取回溯訊息
- `ensemble steer` 與 `ensemble abort` 可行，且 `.ensemble/control/<watch>.ndjson` 可看到對應控制事件
- `scripts\phase2-run-evidence.ps1` 可在 run 後獨立檢查 team board 與 watch stream 都有同一個 conductor terminal decision，並可要求 control feed 非空或包含 steer/abort
- 重複使用同一 repo/team/watch 時，run 前先分別記下 team/watch/control cursor，run 後用 `-TeamSince` / `-WatchSince` / `-ControlSince`，避免舊 run 的 terminal 或 control event 被算進新 run 的驗收
- Hermetic regression：`cargo test --test cross_machine`
  會用 in-process `serve` + `RemoteAdapter` 驗證遠端 codex implementer、遠端 claude/agy reviewers、test gate、`min_approvals = 2` 的治理路徑；同時覆蓋 red test gate 不可 land、同 vendor reviewer 不能湊滿 Phase 2 quorum 的負案例

只測 Slice B governance，不啟動 AI run：

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -SkipSliceA -SkipSliceC -SkipSliceD -SliceBPreflightOnly
```

正式跨機 crew 應使用 `phase2-fleet.ps1 -Materialize` 產生的 crew，然後加：

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo <repo> -Crew <generated-crew.toml> -RequireExplicitRemoteAgents
```

## Slice C：五機配置（fleet）可重跑性

這一段在純本機腳本中不能直接 SSH 到 m1~m5 下命令；因此提供「可觀測」與「部署指令」兩段：

### Phantom bootstrap fallback

若 SSH / Tailscale SSH 不可用，但 peer 上的 `phantom serve` 可連到 `:7878/healthz`，可用 Phantom 的 HMAC-protected admin shell 與 tool RPC 啟動 `ensemble`，避免手工複製 binary 或手工登入節點：

```powershell
# Probe Phantom + ensemble health. Secret can come from PHANTOM_CLUSTER_SECRET instead of -SecretFile.
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -SecretFile <untracked-secret-file> `
  -Action probe

# Start already-installed ensemble on reachable peers.
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -SecretFile <untracked-secret-file> `
  -Action start

# Windows peer fallback when ensemble is missing and inbound HTTP is blocked:
# uploads a local Windows ensemble.exe in chunks via Phantom file_write, decodes it
# into %LOCALAPPDATA%\ensemble\bin\ensemble.exe, then starts `ensemble up --port <service.port>`.
pwsh -NoProfile -File scripts\phase2-phantom-bootstrap.ps1 `
  -Manifest phase2-fleet.local.json `
  -Node <windows-peer> `
  -SecretFile <untracked-secret-file> `
  -Action start `
  -WindowsExePath D:\tmp\ensemble-phase2-control-port-red\release\ensemble.exe
```

Notes:

- The script never prints the cluster secret. Prefer `PHANTOM_CLUSTER_SECRET`; `-SecretFile` may point at an untracked local operator file.
- This transport is intended for Tailscale/private-link peers. Phantom HMAC authenticates requests but does not encrypt HTTP payloads by itself.
- `verify` treats `ensemble` health as sufficient even if Phantom is not running on the same node. This covers the conductor case where only `ensemble up --port <service.port>` is required.
- `probe` and `start` still require Phantom reachability on nodes that need bootstrap.
- macOS/Linux peers without `ensemble` can build from `-MacRepoPath` on `-Branch` in the background; Windows peers can use `-WindowsExePath` chunk upload when URL download is blocked. If `-WindowsExeUrl` is used instead, `-WindowsExeSha256 <sha256>` is required and verified before the downloaded binary is installed or started.

自動可驗證：

- `ensemble mesh`
- `ensemble nodes`
- 若 manifest 設了非預設 `service.port`（例如 Phantom 已佔用 7878 時用 8788），手動檢查需改跑 `ensemble mesh --port <port>` / `ensemble nodes --port <port>`，控制面檢查也要對裸 host 或 `member@node` 加 `--port <port>`；`phase2-fleet.ps1 -CheckNodes` 與 `phase2-verify.ps1 -FleetManifest <manifest>` 會自動使用 manifest 的 `service.port`
- 檢查 `ensemble mesh` 中有預期 remote peers（可用 `-ExpectedFleetNodes m1,m2,m3,m4,m5 -LocalFleetNode m1`；本機 conductor 會被跳過，因為 tailnet peer discovery 不列自己）
- `pwsh scripts\phase2-goal-shape.ps1 -Manifest phase2-fleet.local.json` 可在上機前檢查 manifest 是否符合 Phase 2 形狀：5 nodes、1 main project、4 satellites、main codex/claude/agy routes 都指向 fleet nodes
- 若已有 `phase2-fleet.local.json`，可直接讓 verifier 驗證同一份 manifest 會生成完整 fleet plan（5 nodes、1 main run、4 satellite runs、對應 watch/service commands，且每個 generated project 都揭露 `min_approvals >= 2` 與足夠 distinct reviewers），並印出指定節點的 Slice C plan：`-FleetManifest phase2-fleet.local.json -FleetNode m1`
- `scripts\phase2-fleet.ps1 -RunSelected -VerifyEvidence -RepeatCount 2` 會先清除 selected projects 的舊 acceptance reports，刷新 selected generated crew 檔案，接著在每個 selected run 前自動抓 team/watch/control cursor，並用同一指令重跑驗證可重複性，run 後自動呼叫 `phase2-run-evidence.ps1 -ExpectTerminal <landed|escalated>`；全部 repeat 通過後，會在 generated crew 旁寫出 `<repo>/.ensemble/phase2-fleet/acceptance-<project>-<node>.json`，內容包含 generated crew SHA-256、project spec SHA-256、每次 repeat 的 terminal、exit code、cursor 與 evidence verification 結果；失敗或未驗證成功時 report 應缺席；非零 `ESCALATED` 預設仍會讓腳本失敗，若本次驗收接受 escalate 作為明確終局，需顯式加 `-AllowEscalatedRun`；若該 run 有實際介入，額外加 `-RequireControlEvidence`、`-RequireSteerEvidence` 或 `-RequireAbortEvidence`
- `scripts\phase2-fleet.ps1 -VerifyReports -RepeatCount 2` 會根據同一份 manifest 檢查 selected projects 的 acceptance reports 是否存在且有效：`ok=true`、由 `-VerifyEvidence` 產生、generated crew SHA-256 與 project spec SHA-256 符合目前 manifest、至少指定 repeat 次數、每筆 run 都有 `evidenceVerified=true` 與 `landed|escalated` terminal。若 m1 能讀到主專案與四顆衛星 repo，可用 `-Node all -VerifyReports -RepeatCount 2` 做 Slice C 總驗收。
- 若要從 manifest.nodes 自動檢查 expected peers，加 `-CheckFleetManifestNodes`；本機節點可用 `-FleetNode <this-node>` 或 `-LocalFleetNode <this-node>` 排除，因為 `mesh/nodes` 不列自己

手動（每台主機）：

1. 編輯 manifest 後先跑 `pwsh scripts\phase2-goal-shape.ps1 -Manifest phase2-fleet.local.json`
2. 所有機器：可先用 `ensemble up` 前景啟動；若 manifest 設了 `service.port`，用 `ensemble up --port <port>`。若要常駐，改用同一份 manifest 執行 `pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service install-print -RunService` 預覽，確認後執行 `-Service install -RunService`。若 conductor 可用 Tailscale SSH 連到所有節點，可從 conductor 執行 `pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node all -Service install-print -RunService -RemoteService` 預覽，再執行 `-Service install -RunService -RemoteService` 遠端安裝/重啟服務；若本機 `tailscale ssh` wrapper 被 OpenSSH host-key policy 擋住，但一般 OpenSSH 可連 Tailnet host，改加 `-RemoteServiceTransport ssh`。`ssh` transport 會強制 `BatchMode=yes` 與 `ConnectTimeout=10`，不可卡在密碼 prompt。`-RemoteService` 不支援前景長跑的 `-Service up`。
3. m1 執行 `ensemble mesh` / `ensemble nodes`；非預設 port 時加 `--port <port>`
4. 每台依角色先預覽與產生 crew：`pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -PlanOnly`
5. 確認 plan 正確後，該節點直接跑被選中的任務並自動收斂 evidence：`pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -RunSelected -VerifyEvidence -RepeatCount 2`（`-RunSelected` 必須指定非 `all` 的 `-Node`；`-RepeatCount 2` 是正式 Slice C rerun 驗收；成功後檢查 `.ensemble/phase2-fleet/acceptance-<project>-<node>.json`）
6. 匯總驗收：在可讀取全部 repo 的節點跑 `pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node all -VerifyReports -RepeatCount 2`；若每台只讀自己的 repo，則各台用自己的 `-Node <m1..m5> -VerifyReports -RepeatCount 2`
7. 監控：`ensemble watch <watch-name> --follow`

## Slice D：clean reinstall + smoke 重建

自動驗證流程：

1. `uninstall.ps1 -RemoveMcpConfig -Repo <repo> -Clients codex,claude,opencode`
2. `install.ps1`
3. `ensemble serve --install-service --print` 與 `ensemble serve --uninstall-service --print`（service plan dry-run，不改系統）
4. `ensemble up`（背景執行，確認可啟動且不立即結束）
5. `ensemble mesh [--port <service.port>]`、`ensemble nodes [--port <service.port>]`
6. `smoke.ps1 -Reviewers claude -AllowEscalatedRun`
7. `uninstall.ps1`

對應腳本入口：

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -SkipSliceB -SkipSliceC
```

`SkipSliceB/SkipSliceC` 可先分段排障；`-SkipCleanSmoke` 可先只做 up/nodes/nodes 後不跑 smoke。

## 目前 `main` 對應腳本

- `scripts/phase2-verify.ps1`（本文件對應自動切片）
- `scripts/phase2-local-ready.ps1`（上五機前的本機 readiness wrapper；預設跑 `cross_machine`，但不跑 clean reinstall）
- `scripts/phase2-run-evidence.ps1`（單一主/衛星 run 完成後，檢查 team/watch/control 證據與 board/watch 的 `LANDED` 或 `escalated: ...` 終局）
- `scripts/phase2-goal-shape.ps1`（檢查 5-node + 4 satellites manifest 形狀）
- `scripts/phase2-fleet.ps1 -PlanOnly -Json`（輸出機器可讀的 full-fleet project/command plan，供 Slice C verifier 檢查）；`-RunSelected -VerifyEvidence -RepeatCount 2` 另會寫出 per-project acceptance report；`-VerifyReports -RepeatCount 2` 匯總驗證這些 reports
- `scripts/phantom-single-machine.ps1`（在進 Phase 2 前確認 Phantom 可在單機透過 shell tool 調用 ensemble，且 `--node local` 不會誤走遠端）
- `docs/plans/2026-06-24-phase2-goal.md`（目標定義）

若你要放進 `/goal`，可直接使用該 goal 檔中的
**「一、完成條件 + 二、驗收切片」**。
