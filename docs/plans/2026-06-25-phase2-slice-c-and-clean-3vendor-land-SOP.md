# SOP — Phase 2 Slice C 五機部署 + codex 乾淨三家 LAND

對應目標：`docs/plans/2026-06-24-phase2-goal.md` 的 **Slice C（五機配置）** 與「治理不變（2 家不同 vendor 審核）」。
機器對照：`docs/plans/2026-06-24-five-machine-allocation-main-satellites.md`。

> 本 SOP 設計成「在 4 台機器 `git pull` 下來 build 就能跑」。`m1`（conductor）為操作中樞，
> `m2~m5` 為衛星。實際主機名稱不同沒關係，只要保留對照即可。

---

## 0. 每台機器一次性前置

1. **關 Surfshark 再上 Tailscale**（重要）：Surfshark 的 WireGuard 會撞 Tailscale，跨機會悄悄連不到。
   ```powershell
   # 關閉 Surfshark（GUI 關閉或停服務），然後：
   tailscale up
   tailscale status   # 確認每台都在同一 tailnet、有 100.x IP
   ```
2. **裝好並登入 4 個 AI CLI**：`codex`、`claude`、`opencode`、`agy`（主專案每台都要四個）。
   ```powershell
   codex --version; claude --version; opencode --version; agy --help
   ```
3. **Rust toolchain**（build 用）：`rustc --version`、`cargo --version`。

---

## 1. Pull + Build + Install（在 m2~m5 四台執行；m1 同樣方式）

```bash
# 在 ensemble repo 目錄
git fetch origin
git checkout phase2-verify-fixes      # 若已 merge 進 main 就用 main
git pull --ff-only

cargo build --release                 # 先確認可編譯

# 跨平台安裝（Windows / macOS 皆可）：裝到 ~/.cargo/bin/ensemble
cargo install --path . --force
```

- **Windows 替代安裝**（裝到 `%LOCALAPPDATA%\ensemble\bin` 並更新 User PATH）：
  ```powershell
  pwsh scripts\install.ps1 -SourceExe target\release\ensemble.exe -SkipBuild
  ```
- **macOS（m1/m5 arm64）**：用上面的 `cargo install --path .`；確認 `~/.cargo/bin` 在 PATH。
  `scripts/phase2-fleet.ps1` 與 verify scripts 需要 PowerShell 7 (`pwsh`)；若 Mac 尚未安裝，先裝 `pwsh`，再用同一套 fleet manifest 流程。

安裝後每台驗收環境：
```bash
ensemble doctor      # 期待四個 CLI + tailscale + git-repo 都 [ok]
```

---

## 2. 啟動節點（每台機器）

```bash
ensemble up          # 前景常駐；可放背景終端 / Start-Job / tmux
```

若要把節點變成登入/開機後可重建的常駐服務，先預覽 OS service plan：

```bash
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service install-print -RunService
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service uninstall-print -RunService
```

確認內容正確後，拿掉 `--print` 進行實際安裝或移除：

```bash
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service install -RunService
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service uninstall -RunService
```

實際 install 會建立/更新並立即啟動或重啟 `serve`；uninstall 會先停止再移除 service 設定。

`--install-service` 預設執行 `ensemble serve`，所以會繼承 `serve` 的安全 bind 行為（有 tailnet IP 則綁 tailnet，否則 loopback）。若你要固定 bind，可加 `--bind <addr>`；若要讓遠端 mutation 需要 token，可明確加 `--token <token>`，但這會寫入系統 service 設定。

在 **m1** 確認整個 fleet：
```bash
ensemble mesh        # 期待 local CLIs + remote peers（m2~m5）
ensemble nodes       # agent→host 輔助視圖；不列本機 m1
pwsh scripts/phase2-verify.ps1 -Repo . -SkipSliceA -SkipSliceB -SkipSliceD -FleetManifest phase2-fleet.local.json -FleetNode m1 -CheckFleetManifestNodes
```
> 看不到節點 → 回 Step 0：先關 Surfshark 再 `tailscale up`，並確認每台都已 `ensemble up`。

---

## 3. 產生五機 crew 與 run 指令（不用手工複製/手工路由）

先在每台機器的 `ensemble` repo 內建立一份本機私有 manifest：

```bash
pwsh scripts/phase2-fleet.ps1 -InitSample -Manifest phase2-fleet.local.json
```

編輯 `phase2-fleet.local.json`：
- `nodes` / `conductor`：填實際五台 host 對照。
- `main.repo`：主專案在本機的路徑。
- `main.routes`：主專案 headless governed run 使用的路由（腳本會把 `m2` 正規化為 `http://m2:7878`）。
- `satellites[]`：四個衛星專案的 `repo` / `node` / `team` / `test`。

`phase2-fleet.local.json` 已列入 `.gitignore`，不要提交內部路徑或機器名稱。

編輯後先做 goal-shape 檢查，這一步只驗 manifest 形狀，不會寫入專案：

```bash
pwsh scripts/phase2-goal-shape.ps1 -Manifest phase2-fleet.local.json
```

通過條件：剛好 5 個 fleet nodes、conductor 在 nodes 內、主專案有 `codex`/`claude`/`agy`
routes 且都指向 fleet nodes、剛好 4 個 satellite projects，且 satellite 的 name/team/watch 不重複。

每台機器依自己的角色 materialize：

```bash
# m1
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -PlanOnly

# m2~m5：把 Node 換成對應 host alias
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m2 -Materialize -PlanOnly
```

腳本會：
- 產生 `.ensemble/phase2-fleet/crew-main.generated.toml`（m1/conductor）。
- 產生 `.ensemble/phase2-fleet/crew-<sat>.generated.toml`（對應衛星機）。
- 列出該節點要跑的 `ensemble up`、`ensemble run`、`ensemble watch` 指令。

確認 plan 正確後，也可以讓腳本直接執行該節點被選中的 `ensemble run`（只跑 `run`，不會自動跑 `up` 或 `watch`）。`-RunSelected` 必須搭配明確的 `-Node <host>`，不接受預設的 `all`。service bootstrap 則用 `-Service install-print|install|uninstall-print|uninstall -RunService`，同樣必須指定明確節點：

```bash
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -RunSelected
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m2 -Materialize -RunSelected
```

在 m1 也可以加上節點檢查：

```bash
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -CheckNodes -PlanOnly
pwsh scripts/phase2-verify.ps1 -Repo . -SkipSliceA -SkipSliceB -SkipSliceD -FleetManifest phase2-fleet.local.json -FleetNode m1 -CheckFleetManifestNodes
```
`-CheckNodes` / `-CheckFleetManifestNodes` 會用 `ensemble mesh` 檢查 remote peers（m2~m5）；conductor `m1` 是本機，
不會出現在 tailnet peer 清單中，需由同一次 `mesh` 的 `local CLIs` 區塊確認。

---

## 4. Slice C 驗收

### 4a 主專案（team = main，五機可見、可介入）

在 **m1**（主 repo 目錄）：
```bash
ensemble run "<主專案任務>" --crew .ensemble/phase2-fleet/crew-main.generated.toml --repo <主repo路徑> --team main --watch main --merge
```
或由 manifest 直接執行本節點分配到的主專案 run：
```bash
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -RunSelected
```
任一台操作機可監控 / 介入：
```bash
ensemble watch main --follow
ensemble steer main "請偏重 error handling" --team main
ensemble abort main --hard --team main        # 偏離時硬中斷
```
> generated main crew 已是 `implement=codex / review=claude / audit=agy`、`min_approvals=2`，
> 且 `codex`、`claude` 都 `backup="agy"`（額度爆掉自動換 agy，不整個 escalate）。

### 4b 四個衛星專案（各機各專案，codex + claude）

在各衛星機器（`m2→sat-a`、`m3→sat-b`、`m4→sat-c`、`m5→sat-d`）的專案目錄：
```bash
ensemble run "<衛星任務>" --crew .ensemble/phase2-fleet/crew-sat-a.generated.toml --repo . --team sat-a --watch sat-a
ensemble watch sat-a --follow
```
> 更建議先用 `phase2-fleet.ps1 -PlanOnly` 預覽，確認後用 `-RunSelected` 執行，避免 crew path / team / watch 打錯。

### 完成判定（Slice C）
- `ensemble mesh` 在 m1 顯示本機 CLIs，且 tailnet peers 看得到 m2~m5；`ensemble nodes` 可作為 agent→host 輔助視圖。
- 每個 run 都有可讀的 `stream/control` 事件（`ensemble watch <name> --since 0`）。
- 每個 run 末端為 **LANDED** 或 **ESCALATED**（含 escalate＝治理不落盤，也算正確終局）。
- 任務可重跑一次仍到達同等終局。

---

## 5. codex 額度重置後：乾淨三家 LAND（main）

codex 每日額度重置後（觀測到的訊息：`retry after 11:54 PM`），跑一次 **codex 當 implementer 的乾淨三家版**，
證明「test pass + 2 家不同 vendor 審核」可正常 land：

```bash
ensemble run "<可驗證的小任務>" --crew .ensemble/phase2-fleet/crew-main.generated.toml --repo <主repo> --team main --watch main --merge
```

期待 transcript：
```
[implement · result]  codex 實作（不再 rate-limited）
[test · test_pass]
[review · verdict]     claude … VERDICT: LGTM
[audit · verdict]      agy  … VERDICT: LGTM
[conductor · decision] LANDED        # codex + claude + agy 三家
```

- 若 codex 又限流：會自動 `auto-substituting backup 'agy'` 並仍可 LAND，但那是**降級版**（agy 實作 + claude/agy 審），
  不是乾淨三家。要乾淨三家就等額度真的回來再跑。
- 想先用 trivial test gate 快速驗 quorum（不跑 cargo test）：把 `[test].command` 暫時改成 `cmd /c exit 0`（Windows）
  或 `true`（macOS），在 throwaway repo 驗 `LANDED at min_approvals=2`，再換回真正測試指令。

---

## 6. 疑難排解

| 症狀 | 處置 |
|---|---|
| 跨機 `ensemble nodes` 看不到別台 | 先關 Surfshark 再 `tailscale up`（WireGuard 衝突）；確認別台已 `ensemble up` |
| `ensemble nodes` 沒列出 m1 | 正常；`nodes`/tailnet peer discovery 不列本機，請看 `ensemble mesh` 的 `local CLIs` |
| `agy` flake / timeout | 給足 timeout（≥120s；crew 已設 `[agents.agy] timeout=180`）。短 timeout（1~5s）一定 flake |
| 把 `opencode` 當自動 reviewer 卡住 | opencode headless 會 hang，**別放進自動角色**；互動式 `ensemble agent`/MCP 不受限 |
| `ensemble merge` 拒絕：worktree not clean | 跑 run 的 repo 要 `.gitignore` 掉 `.ensemble/`，且 `crew.toml` 放 repo 外或 ignore |
| codex `rate-limited … no backup` | 確認 crew 有設 `backup`（本 repo 的 crew 已設 agy）；本版已修「backup adapter 沒被建」的 bug |
| service install plan 看起來不對 | 先用 `pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service install-print -RunService` 檢查；路徑不對時直接用 `ensemble serve --install-service --print --exe <path>` 深入排查 |
| 重裝 | Windows：`pwsh scripts\uninstall.ps1` → `install.ps1`；macOS：`cargo install --path . --force` 覆蓋 |

---

## 7. 一頁速查（每台機器照抄）

```bash
# 0) 關 Surfshark；tailscale up; tailscale status
# 1) pull + build + install
git pull --ff-only && cargo build --release && cargo install --path . --force
ensemble doctor
# 1.5) 產生本機角色 crew + 指令（第一次先 -InitSample 並編輯 manifest；Node 改成本機 alias）
pwsh scripts/phase2-goal-shape.ps1 -Manifest phase2-fleet.local.json
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -PlanOnly
# 2) 起節點
ensemble up           # 背景/新終端
# 或常駐服務：先 preview，再去掉 --print
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Service install-print -RunService
# 3) m1 看 fleet
ensemble mesh && ensemble nodes
# 4) 主專案（m1）
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m1 -Materialize -RunSelected
# 5) 衛星（m2~m5 各自）
pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node m2 -Materialize -RunSelected
```
