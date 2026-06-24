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

### 2.1 主專案 `crew` 範本

存成 `crew-main.toml`（放在主專案根目錄）：

```toml
pipeline = ["implement", "review", "debug", "safety"]

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

[roles.debug]
agent = "opencode"
blind = true

[roles.safety]
agent = "agy"
blind = true

# 可先不指定 node，讓 discover_mesh/discovery 自動分發。
# 如需固化，指定每位 agent 到單一主機:
[agents.codex]
node = "m1"
[agents.claude]
node = "m2"
[agents.opencode]
node = "m3"
[agents.agy]
node = "m4"
```

### 2.2 主專案啟動與執行

每台機器都要先加入服務並確認可見：
```powershell
ensemble up
```

確認 fleet 與能力：
```powershell
ensemble mesh
ensemble nodes
```

啟動主專案治理 run：
```powershell
ensemble run "主專案任務描述" --crew crew-main.toml --repo <主專案路徑> --team main --watch main --merge
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

### 3.1 衛星專案 `crew` 範本（`crew-sat-two-ai.toml`）

```toml
pipeline = ["implement", "review"]

[gate]
min_approvals = 1
max_rounds = 3
on_flake = "exclude"

[roles.implement]
agent = "codex"

[roles.review]
agent = "claude"
blind = true

# 若你要固定節點，請指定到各衛星機器 host
[agents.codex]
node = "<sat-machine>"
[agents.claude]
node = "<sat-machine>"
```

例如給 `m2` 使用：
```toml
[agents.codex]
node = "m2"
[agents.claude]
node = "m2"
```

### 3.2 衛星專案執行命令

在各自衛星專案目錄：
```powershell
ensemble up
ensemble run "衛星任務" --crew crew-sat-two-ai.toml --repo <衛星專案路徑> --team sat-a --watch sat-a
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
- 主專案想要「五機都看到、都可介入」的效果，請把所有機器都先 `ensemble up` 並保持 `team=main` 一致。
- 衛星專案保持最小路由，先用 `min_approvals = 1` 會快；當流程穩定再改 2（至少兩家不同 vendor 時）。
- `on_flake = "exclude"` 是建議值，避免 flake 被誤算為 approval。
- 任何 run 前先跑 `ensemble nodes` 與 `ensemble doctor`，確認節點與 CLI 狀態正確。

## 六、日常作業順序（簡版）

1. 所有節點：`ensemble up`
2. 全員確認：`ensemble mesh`、`ensemble nodes`、`ensemble doctor`
3. 主專案：`ensemble run "..." --crew crew-main.toml --team main --merge`
4. 衛星專案：各機各自執行 `crew-sat-two-ai.toml`
5. 監控與介入：`ensemble watch` / `ensemble steer` / `ensemble abort`

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

主專案最小 run（五機可用）：

```powershell
ensemble run "phase2 smoke run" --crew crew-main.toml --repo <main-repo> --team main --watch main --no-discover
ensemble watch main --follow
ensemble steer main "先維持 test 驗證為主，修正偏差" --team main
ensemble abort main --hard --team main   # 當 run 持續偏離時
```

衛星專案最小 run（各機各執行）：

```powershell
cd /path/to/satellite-repo
ensemble run "satellite smoke run" --crew crew-sat-two-ai.toml --repo . --team <sat-team> --watch <sat-team> --no-discover
ensemble watch <sat-team> --follow
```

完成判定：

- `ensemble nodes` 能看到預期主機（m1~m5）與對應服務狀態。
- 每次 run 有可讀取的 `stream/control` 事件。
- `ensemble run` 末端為 `LANDED` 或 `ESCALATED`（含 `escalated` 就是治理不落盤）。
- 任務可以重跑一次仍可到達同等終局。

