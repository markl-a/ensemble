# Handoff — Phase 2 verify pass + 下一步交接

> 接手對象:操作者本人 / 另一個 AI CLI / 另一台機器。讀完這份就能接著做。
> 本次工作機:conductor (Windows)。日期:2026-06-25。

---

## 0. 一句話狀態

單機基線 + 單機可驗的 Phase 2 切片（A/B/D）已驗綠並修掉沿途真實 bug,**6+1 commits 在 branch `phase2-verify-fixes`(PR #2,未 merge)**;剩 **Slice C 五機 loop(需實機 fleet)** 與 **codex 額度重置後的乾淨三家 LAND**。

---

## 1. 這次 session 做了什麼(都有實測證據)

Branch:`phase2-verify-fixes`　PR:#2 → https://github.com/<you>/ensemble/pull/2
（base commit `5417cf2`)

| commit | 內容 | 為何 |
|---|---|---|
| `f1bbded` | feat(launcher):`ENSEMBLE_<CLIENT>_BIN` vendor-binary override + acceptance 改用它 | 這台裝了真 codex,會蓋掉 acceptance 的 fake shim(受控 launcher 經 `cmd /C` 解析真 binary,PATH/cwd 注入不進去)→ 加 override 釘選 binary,測試變 hermetic |
| `3aff030` | test:repo_sync 臨時 repo 強制 `core.autocrlf=false`/`core.eol=lf` + remote token route 測試 | 7 個 repo_sync 測試在 native Windows 因 CRLF 假失敗(`main-edit\r\n` vs `\n`) |
| `b988d7c` | docs(phase2):goal/verify/五機分配 plan + `crew-main.toml` + `phase2-verify.ps1` | Phase 2 目標與驗收切片腳手架(codex 先前產出,整理進 commit) |
| `4d2c423` | fix(install):`Stop-EnsembleProcesses`(install+uninstall) | clean reinstall 時若 `ensemble up` 還在跑會鎖住 binary → 先停掉再換 |
| `6f7b82e` | **fix(conductor):建 backup vendor 的 adapter**(`crew_agents_with_backups`) | **真 bug**:`adapters_for` 只建 role agent 的 adapter,backup-only(如 agy)不建 → 設了 `backup="agy"` 也印 "no backup configured",quota-degrade 永遠不發生 |
| `90894b3` | fix(crew):`crew-main.toml` 加 `audit=agy` 第二 reviewer | 原本 `min_approvals=2` 卻只有 1 個 reviewer → 永遠湊不到 2 票、必 escalate。現在 claude+agy 兩家可達 quorum |
| `b88e1dd` | docs(phase2):五機 SOP + `crew-sat-two-ai.toml` | 讓 4 台 pull 下來 build 就能跑;補上衛星 crew 實檔 |

### 實測證據(本機 conductor)
- **WSL `cargo test`**:全綠(authoritative path)。**native `cargo test`**:repo_sync 已綠;唯 2 個 `exec_adapter` timeout 測試在「高併發負載」下 timing flake,單獨跑全過。
- **`cargo clippy --all-targets -D warnings`**:clean。
- **`acceptance-single-machine.ps1`**:PASS。**`smoke.ps1 -PreflightOnly`**:PASS。
- **Slice A**(`phase2-verify.ps1`):team status/nodes/watch + `--node auto` 明確非零失敗,PASS。
- **Slice B**:governed run **LANDED**(claude impl→gate→blind 審→land);並兩次 **ESCALATED**(codex 限流、opencode 審核 flake)→ 從不假綠。steer/abort 控制事件於 acceptance 已驗。
- **Slice D**:uninstall→install→**對執行中節點重裝(Stop-EnsembleProcesses 砍掉 PID)**→up/mesh/nodes→smoke preflight→uninstall 乾淨移除,全程驗過。
- **quota-degrade 實測**:codex 限流 → `auto-substituting backup 'agy'` → agy 實作 → test_pass → claude LGTM → **LANDED**(agy+claude 兩家)。
- **2-reviewer quorum 實測**:`crew-main.toml` 拓樸 → 第二輪 claude LGTM + agy LGTM → **LANDED at min_approvals=2**。

---

## 2. 接下來要做(依優先序)

### A. 立即收尾(低風險、解鎖採用)
1. **決定 merge PR #2 → `main`**(這樣 4 台直接 pull `main`;否則 pull `phase2-verify-fixes`)。
2. **Release 衛生**:`Cargo.toml` 版本 `0.0.0`→`0.1.0`;README 還寫「Phase 0 / not usable」要重寫(放 90 秒 governed-landing demo 截圖/錄影)。
3. **跨平台安裝**:Mac 沒有 shell installer,SOP 已改用 `cargo install --path .`;可考慮補一個 `scripts/install.sh` 或 README 寫清楚。

### B. Slice C 五機 loop(需實機,operator 主導)
照 `docs/plans/2026-06-25-phase2-slice-c-and-clean-3vendor-land-SOP.md`:
- 每台:關 your VPN → `tailscale up` → `git pull` → `cargo build --release && cargo install --path . --force` → `ensemble doctor` → `ensemble up`。
- m1:`ensemble mesh && ensemble nodes`(期待看到 m1~m5)。
- 主專案(`crew-main.toml`,team=main)+ 4 衛星(`crew-sat-two-ai.toml`)各跑最小 run。
- **完成判定**:nodes 看到 5 機、每 run 有 stream/control 事件、末端 LANDED/ESCALATED、可重跑。

### C. codex 乾淨三家 LAND(等額度重置)
codex 額度重置後(觀測訊息 `retry after 11:54 PM`),跑一次 codex 當 implementer 的乾淨版:
```
ensemble run "<task>" --crew crew-main.toml --repo <repo> --team main --watch main --merge
```
期待 transcript:codex impl → test_pass → claude LGTM → agy LGTM → LANDED(三家不同 vendor)。
若 codex 又限流會自動 degrade 到 agy(仍 LAND,但是降級版,非乾淨三家)。

### D. 深度方向建議(策略,待你定主目標再展開)
我這次的深度分析結論(尚未動工,等你選主目標):
- **護城河 = 單機跨廠商 governed landing**;federation(5 機)是投入最多、回報最不明確處 → 傾向**先深度、後廣度**。
- **vendor 可靠度是真正天花板**:opencode headless 會 hang(不可當自動角色)、codex 每天限流、agy 要給足 timeout(≥120s,crew 已設 180s)→ 實際穩定組合常是 **claude+agy**。建議 `ensemble doctor` 從「在不在 PATH」升級成「會不會即時回 marker」的健康探測;backup 支援多層 chain。
- **未決策(等你回答)**:ensemble 主目標是「自己 5 機日常工具 / 開源給人用 / 作品集 / 衝產出」?不同答案會讓我把精力分別放在 federation 硬化 / 砍摩擦 / 治理 evidence 故事 / 衛星實跑。

---

## 3. 已知雷區(接手必讀)

- **跨機看不到節點**:先關 your VPN 再 `tailscale up`(your VPN WireGuard 撞 Tailscale)。
- **opencode**:headless 會 hang,**別放進自動 governed 角色**;互動式 `ensemble agent`/MCP 不受限。
- **codex**:每天會限流;靠 crew 的 `backup="agy"` 自動 degrade(本版已修 backup adapter 沒被建的 bug)。
- **agy**:沒壞,但要給足 timeout(短 timeout 1~5s 一定 flake,那是 smoke/acceptance 的故意設定);default adapter timeout 180s。
- **`ensemble merge` 拒絕髒 worktree**:跑 run 的 repo 要 `.gitignore` 掉 `.ensemble/`,`crew.toml` 放 repo 外或 ignore。
- **native Windows `cargo test`**:`exec_adapter` 兩個 timeout 測試在高負載下會 timing flake → 單獨重跑即綠;權威測試路徑是 WSL。
- **git identity**:這 repo 一律用 `<you> <you@example.com>`;commit message 含反引號/`<...>` 用 `git commit -F file`;落地 `git add <具體檔案>` 不要 `-A`。

---

## 4. 快速上手(接手者照抄)

```bash
# 看這次的改動
git fetch origin && git checkout phase2-verify-fixes && git log --oneline -7

# 本機重驗(WSL 權威路徑)
wsl -e bash -lc "cd /mnt/d/Projects/ensemble && CARGO_TARGET_DIR=\$HOME/ensemble-target cargo test"

# 單機 acceptance(用已建好的 release binary)
pwsh scripts\acceptance-single-machine.ps1 -NoBuild -TargetDir D:\tmp\ensemble-target -AgyTimeoutSecs 1

# Phase 2 自動切片(A + 本機可驗部分)
pwsh scripts\phase2-verify.ps1 -Repo D:\Projects\ensemble -Team main -TargetDir D:\tmp\ensemble-target -SkipSliceB -SkipSliceC -SkipSliceD
```

相關文件:
- 目標:`docs/plans/2026-06-24-phase2-goal.md`
- 五機分配:`docs/plans/2026-06-24-five-machine-allocation-main-satellites.md`
- 五機 SOP:`docs/plans/2026-06-25-phase2-slice-c-and-clean-3vendor-land-SOP.md`
- 本交接:`docs/plans/2026-06-25-handoff-phase2-verify-and-next.md`
