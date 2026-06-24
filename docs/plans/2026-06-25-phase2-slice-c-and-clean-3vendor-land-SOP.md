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
  （`scripts\*.ps1` 是 Windows 專用，Mac 不要用。）

安裝後每台驗收環境：
```bash
ensemble doctor      # 期待四個 CLI + tailscale + git-repo 都 [ok]
```

---

## 2. 啟動節點（每台機器）

```bash
ensemble up          # 前景常駐；可放背景終端 / Start-Job / tmux
```

在 **m1** 確認整個 fleet：
```bash
ensemble mesh        # 本機 4 CLI + 每個 tailnet peer host 哪些 agent
ensemble nodes       # 期待看到 m1~m5
```
> 看不到節點 → 回 Step 0：先關 Surfshark 再 `tailscale up`，並確認每台都已 `ensemble up`。

---

## 3. Slice C 驗收

### 3a 主專案（team = main，五機可見、可介入）

在 **m1**（主 repo 目錄）：
```bash
ensemble run "<主專案任務>" --crew crew-main.toml --repo <主repo路徑> --team main --watch main --merge
```
任一台操作機可監控 / 介入：
```bash
ensemble watch main --follow
ensemble steer main "請偏重 error handling" --team main
ensemble abort main --hard --team main        # 偏離時硬中斷
```
> `crew-main.toml` 已是 `implement=codex / review=claude / audit=agy`、`min_approvals=2`，
> 且 `codex`、`claude` 都 `backup="agy"`（額度爆掉自動換 agy，不整個 escalate）。

### 3b 四個衛星專案（各機各專案，codex + claude）

在各衛星機器（`m2→sat-a`、`m3→sat-b`、`m4→sat-c`、`m5→sat-d`）的專案目錄：
```bash
# 先把 crew-sat-two-ai.toml 複製到該衛星 repo 根目錄，並把 [test].command 改成該專案真正的測試指令
ensemble run "<衛星任務>" --crew crew-sat-two-ai.toml --repo . --team sat-a --watch sat-a
ensemble watch sat-a --follow
```

### 完成判定（Slice C）
- `ensemble nodes` 在 m1 看得到 `m1~m5`。
- 每個 run 都有可讀的 `stream/control` 事件（`ensemble watch <name> --since 0`）。
- 每個 run 末端為 **LANDED** 或 **ESCALATED**（含 escalate＝治理不落盤，也算正確終局）。
- 任務可重跑一次仍到達同等終局。

---

## 4. codex 額度重置後：乾淨三家 LAND（main）

codex 每日額度重置後（觀測到的訊息：`retry after 11:54 PM`），跑一次 **codex 當 implementer 的乾淨三家版**，
證明「test pass + 2 家不同 vendor 審核」可正常 land：

```bash
ensemble run "<可驗證的小任務>" --crew crew-main.toml --repo <主repo> --team main --watch main --merge
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

## 5. 疑難排解

| 症狀 | 處置 |
|---|---|
| 跨機 `ensemble nodes` 看不到別台 | 先關 Surfshark 再 `tailscale up`（WireGuard 衝突）；確認別台已 `ensemble up` |
| `agy` flake / timeout | 給足 timeout（≥120s；crew 已設 `[agents.agy] timeout=180`）。短 timeout（1~5s）一定 flake |
| 把 `opencode` 當自動 reviewer 卡住 | opencode headless 會 hang，**別放進自動角色**；互動式 `ensemble agent`/MCP 不受限 |
| `ensemble merge` 拒絕：worktree not clean | 跑 run 的 repo 要 `.gitignore` 掉 `.ensemble/`，且 `crew.toml` 放 repo 外或 ignore |
| codex `rate-limited … no backup` | 確認 crew 有設 `backup`（本 repo 的 crew 已設 agy）；本版已修「backup adapter 沒被建」的 bug |
| 重裝 | Windows：`pwsh scripts\uninstall.ps1` → `install.ps1`；macOS：`cargo install --path . --force` 覆蓋 |

---

## 6. 一頁速查（每台機器照抄）

```bash
# 0) 關 Surfshark；tailscale up; tailscale status
# 1) pull + build + install
git pull --ff-only && cargo build --release && cargo install --path . --force
ensemble doctor
# 2) 起節點
ensemble up           # 背景/新終端
# 3) m1 看 fleet
ensemble mesh && ensemble nodes
# 4) 主專案（m1）
ensemble run "<task>" --crew crew-main.toml --repo <主repo> --team main --watch main --merge
# 5) 衛星（m2~m5 各自）
ensemble run "<task>" --crew crew-sat-two-ai.toml --repo . --team sat-a --watch sat-a
```
