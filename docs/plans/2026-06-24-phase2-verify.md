# Phase 2 驗收作法（Phase 2 Verify）

本文件對應 `/goal` 中的 Phase 2 目標，提供「可自動化（本機）」與「跨機時需手動接上」兩層驗收流程。

## 準備

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble
```

建議先確保：

- `ensemble` 可在 PATH 取到（或 `-TargetDir` 有對應 `target/*/ensemble.exe`）
- `git` 可用
- `crew-main.toml`（或你自己的 `--crew`）可用於 `ensemble run`

## Slice A：控制面（控制 plane）

必要項：

- `ensemble team status --team <team> --json`
- `ensemble nodes`
- `ensemble watch <member[@node]> --team <team> --since 0`

自動驗證：

- `scripts/phase2-verify.ps1` 會先執行 `team status/watch/nodes`，並驗證 watch 的錯誤路由：
  - `watch codex@auto --node auto` 必須**非零失敗**，並回傳明確訊息（`--node auto is not supported`）
- 跨機時，若要測 `member@node`，請設定 `-RemoteNode <node>`

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -Team main -RemoteNode m1
```

## Slice B：跨機 run + intervention

必要項：

- `ensemble run "<task>" --crew <crew.toml> --repo <repo> --team <team> --watch <watch>`
- `ensemble watch <watch> --follow`（可選）
- `ensemble steer <watch> "<prompt>"`
- `ensemble abort <watch> [--hard]`

自動驗證：

- 執行 `ensemble run`，檢查輸出包含 `LANDED` 或 `ESCALATED`
- `ensemble watch <watch> --since 0` 至少可讀取回溯訊息
- `ensemble steer` 與 `ensemble abort` 可行，且 `.ensemble/control/<watch>.ndjson` 可看到對應控制事件

## Slice C：五機配置（fleet）可重跑性

這一段在純本機腳本中不能直接 SSH 到 m1~m5 下命令；因此提供「可觀測」與「部署指令」兩段：

自動可驗證：

- `ensemble mesh`
- `ensemble nodes`
- 檢查 `ensemble mesh` 中有預期 remote peers（可用 `-ExpectedFleetNodes m1,m2,m3,m4,m5 -LocalFleetNode m1`；本機 conductor 會被跳過，因為 tailnet peer discovery 不列自己）

手動（每台主機）：

1. 所有機器：`ensemble up`
2. m1 執行 `ensemble mesh` / `ensemble nodes`
3. 每台依角色先預覽與產生 crew：`pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -PlanOnly`
4. 確認 plan 正確後，該節點直接跑被選中的任務：`pwsh scripts\phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -RunSelected`（`-RunSelected` 必須指定非 `all` 的 `-Node`）
5. 監控：`ensemble watch <watch-name> --follow`

## Slice D：clean reinstall + smoke 重建

自動驗證流程：

1. `uninstall.ps1 -RemoveMcpConfig -Repo <repo> -Clients codex,claude,opencode`
2. `install.ps1`
3. `ensemble serve --install-service --print` 與 `ensemble serve --uninstall-service --print`（service plan dry-run，不改系統）
4. `ensemble up`（背景執行，確認可啟動且不立即結束）
5. `ensemble mesh`、`ensemble nodes`
6. `smoke.ps1 -Reviewers claude -AllowEscalatedRun`
7. `uninstall.ps1`

對應腳本入口：

```powershell
pwsh -NoProfile -File scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -SkipSliceB -SkipSliceC
```

`SkipSliceB/SkipSliceC` 可先分段排障；`-SkipCleanSmoke` 可先只做 up/nodes/nodes 後不跑 smoke。

## 目前 `main` 對應腳本

- `scripts/phase2-verify.ps1`（本文件對應自動切片）
- `docs/plans/2026-06-24-phase2-goal.md`（目標定義）

若你要放進 `/goal`，可直接使用該 goal 檔中的
**「一、完成條件 + 二、驗收切片」**。
