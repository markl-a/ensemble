# 五機主軸 + 四機衛星專案編排規格（主專案 4-AI，全機齊備；衛星專案 2-AI）

目的：在「主專案」與「4 個衛星專案」上建立可反覆執行的 fleet 角色分工與命令流程。  
原則如下：
- 主專案：每台機器都可參與同一個 `team` 的治理流程。
- 主專案每台機器都安裝並可啟動 `codex`、`claude`、`opencode`、`agy`（四個 AI CLI）。
- 另外 4 個衛星專案：只用 `codex`、`claude`，固定分到四台機器。
- 單一 run 使用 `crew.toml` 時，路由是依 `agent` 名稱（`[agents.codex]`、`[agents.claude]`…）而非任務名稱。

## 一、機器角色定義

建議命名：
- `m1`：主節點（主專案 conductor）
- `m2`：衛星 1
- `m3`：衛星 2
- `m4`：衛星 3
- `m5`：衛星 4

> 若你的實際機器名稱不同，保留對照表，後續文件中只要改 host 名稱即可。

## 二、主專案編排（每機 4 AI）

主專案運行時，用一個集中式 team（建議 `main`）讓五台機器都可被觀測與介入。

### 2.1 主專案 generated `crew`

主專案 crew 不再手工複製或手工改 node；用 `scripts/phase2-fleet.ps1` 從
`phase2-fleet.local.json` 產生到主專案。先預覽與 materialize：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m1 -Materialize -PlanOnly
```

確認後可直接執行本節點選中的主專案 run：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m1 -Materialize -RunSelected
```

生成內容的核心形狀如下（`node` 會是完整 URL，不是裸 host）：

```toml
pipeline = ["implement", "review", "audit"]

[gate]
min_approvals = 2
max_rounds = 2
on_flake = "exclude"
stall_limit = 0
max_task_secs = 0

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

[roles.audit]
agent = "agy"
blind = true

[agents.codex]
node = "http://m1:7878"
[agents.claude]
node = "http://m2:7878"
[agents.agy]
node = "http://m3:7878"
```

`opencode` 仍要求每台主機安裝並可作為互動 CLI 使用；但它目前不放進 headless governed
roles，因為既有測試顯示它的 headless reviewer 容易 hang。

### 2.2 主專案啟動與執行

每台機器都要先加入服務並確認可見：
```powershell
ensemble up
```

若希望節點在登入/開機後自動提供 `serve`，先 dry-run 檢查 OS service plan，再實際安裝：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <this-node> -Service install-print -RunService
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <this-node> -Service install -RunService
```

實際 install 會建立/更新並立即啟動或重啟 `serve`；uninstall 會先停止再移除 service 設定。

移除時同樣先 preview：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <this-node> -Service uninstall-print -RunService
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <this-node> -Service uninstall -RunService
```

確認 fleet 與能力：
```powershell
ensemble mesh
ensemble nodes
pwsh /path/to/ensemble/scripts/phase2-verify.ps1 -Repo <repo> -SkipSliceA -SkipSliceB -SkipSliceD -FleetManifest /path/to/ensemble/phase2-fleet.local.json -FleetNode <this-node> -CheckFleetManifestNodes
```

啟動主專案治理 run：
```powershell
ensemble run "主專案任務描述" --crew .ensemble/phase2-fleet/crew-main.generated.toml --repo <主專案路徑> --team main --watch main --merge
```

監控/介入（可在任何一台操作機）：
```powershell
ensemble watch main --follow
ensemble steer main "請偏重 error handling"
ensemble abort main --hard
```

## 三、衛星專案編排（每專案 2 AI：codex+claude）

每台衛星機器分到一個衛星專案，`team` 不共用主專案；每個衛星專案的 run 都只用 `codex` + `claude`。

- 衛星專案 A：指派給 `m2`
- 衛星專案 B：指派給 `m3`
- 衛星專案 C：指派給 `m4`
- 衛星專案 D：指派給 `m5`

### 3.1 衛星專案 generated `crew`

衛星專案同樣由 `phase2-fleet.local.json` 產生，不手工複製 `crew-sat-two-ai.toml`：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m2 -Materialize -PlanOnly
```

確認後可直接執行該衛星節點選中的 run：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m2 -Materialize -RunSelected
```

生成內容的核心形狀如下：

```toml
pipeline = ["implement", "review", "audit"]

[gate]
min_approvals = 2
max_rounds = 3
on_flake = "exclude"

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

[roles.audit]
agent = "codex"
blind = true

[agents.codex]
node = "http://m2:7878"
[agents.claude]
node = "http://m2:7878"
```

### 3.2 衛星專案執行命令

在各自衛星專案目錄：
```powershell
ensemble up
ensemble run "衛星任務" --crew .ensemble/phase2-fleet/crew-sat-a.generated.toml --repo <衛星專案路徑> --team sat-a --watch sat-a
```

## 四、實際分配（主節點與衛星對映）

| 專案 | 任務屬性 | 分派機器 | 參與 AI |
|---|---|---|---|
| 主專案 | 治理主流程、landing、總控 | m1~m5 全部 | codex / claude / opencode / agy |
| 衛星專案 A | 並行開發任務 | m2 | codex / claude |
| 衛星專案 B | 並行開發任務 | m3 | codex / claude |
| 衛星專案 C | 並行開發任務 | m4 | codex / claude |
| 衛星專案 D | 並行開發任務 | m5 | codex / claude |

## 五、重要限制與建議

- 單一 `crew.toml` 對同一 agent 只能指定單一節點；若想在同一時間把同一角色分散更多機器，需要多個 run / 多個 crew 分割任務。
- `crew.toml` 的 `[agents.<name>] node` 目前要使用完整 URL（例如 `http://m2:7878`）。日常不要手工改這段，改用 `scripts/phase2-fleet.ps1` 從 manifest 產生，腳本會把 `m2` 這種 host alias 正規化成可用 URL。
- `ensemble nodes` 是 agent→host 的輔助視圖，且只列 tailnet peers；完整五機可見性請用 `ensemble mesh`，本機 conductor 由 `local CLIs` 區塊確認。
- 主專案想要「五機都看到、都可介入」的效果，請把所有機器都先 `ensemble up`，或安裝 `ensemble serve --install-service`，並保持 `team=main` 一致。
- 衛星專案保持最小 CLI 集合（codex + claude），但 generated crew 仍維持 `min_approvals = 2`：`claude` review + `codex` audit 兩個 distinct vendor 才能 land。
- `on_flake = "exclude"` 是建議值，避免 flake 被誤算為 approval。
- 任何 run 前先跑 `ensemble nodes` 與 `ensemble doctor`，確認節點與 CLI 狀態正確。

## 六、日常作業順序（簡版）

1. 所有節點：`ensemble up`；若要常駐則先 `pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <this-node> -Service install-print -RunService`，確認後改成 `-Service install -RunService`
2. 全員確認：`ensemble mesh`、`ensemble nodes`、`ensemble doctor`
3. 每台依角色執行 `pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -PlanOnly`
4. 確認 plan 正確後，該節點執行 `pwsh scripts/phase2-fleet.ps1 -Manifest phase2-fleet.local.json -Node <m1..m5> -Materialize -RunSelected`（`-RunSelected` 必須指定非 `all` 的 `-Node`）
5. 主專案與衛星專案都由 manifest 內的 `team` / `watch` / `crew` / `repo` 設定產生，不手工改路由
6. 監控與介入：`ensemble watch` / `ensemble steer` / `ensemble abort`

## 七、Slice C 可直接執行的驗收指令（建議）

在各節點（m1~m5）先完成一次性基礎檢查：

```powershell
# 每台機器
cd /path/to/ensemble/repo-or-satellite
ensemble doctor
# 可將 up 放後台執行（例如 Start-Job/新終端）
ensemble up
```

在主導節點 m1 進行切片驗收：

```powershell
cd /path/to/main-repo
# m1 上可先在獨立終端啟動 up
ensemble up
ensemble mesh
ensemble nodes
```

先從 manifest 產生本機角色的 crew 與命令：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m1 -Materialize -PlanOnly
```

主專案最小 run（五機可用）：

```powershell
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node m1 -Materialize -RunSelected
ensemble watch main --follow
ensemble steer main "先維持 test 驗證為主，修正偏差" --team main
ensemble abort main --hard --team main   # 當 run 持續偏離時
```

衛星專案最小 run（各機各執行）：

```powershell
cd /path/to/satellite-repo
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <sat-node> -Materialize -PlanOnly
pwsh /path/to/ensemble/scripts/phase2-fleet.ps1 -Manifest /path/to/ensemble/phase2-fleet.local.json -Node <sat-node> -Materialize -RunSelected
ensemble watch <sat-team> --follow
```

完成判定：

- `ensemble mesh` 能看到本機 CLIs 與預期 remote peers；`ensemble nodes` 能顯示 agent→host 路由輔助狀態。
- 每次 run 有可讀取的 `stream/control` 事件。
- `ensemble run` 末端為 `LANDED` 或 `ESCALATED`（含 `escalated` 就是治理不落盤）。
- 任務可以重跑一次仍可到達同等終局。

