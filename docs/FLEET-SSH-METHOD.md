# Fleet SSH Method — how the conductor drives the other desktops

How a conductor machine drives the other desktops' shells over SSH to run
builds / tests / checks / `ensemble serve`. **This file documents the METHOD
only.** The real node identities (IPs, usernames, hostnames, tailnet domain)
must NEVER be committed here — this is a public repo.

## Where the real values live (NOT in this repo)

- **`phase2-fleet.local.json`** (gitignored) — the real fleet for ensemble.
- **`~/.phantom-mesh/fleet.nodes`** (outside any repo working tree) — one line
  per node: `name  host  platform  caps  role`, where `host` is
  `<user>@<tailscale-ip>` for a remote node or `local` for the conductor.

Keeping identities in a gitignored / out-of-repo file is the rule, not a
convenience: hardcoding an IP / hostname / tailnet domain in a tracked file
leaks operator infra the moment it is pushed.

## Transport: Tailscale + SSH key auth

- All nodes sit on one tailnet; reach a node by its tailscale IP.
- Disable any conflicting userspace VPN first (e.g. your VPN's WireGuard
  collides with Tailscale — peers go unreachable until it is off).
- Auth = **SSH public key** (add each machine's pubkey once). Do NOT rely on a
  credential that lives in the macOS **Keychain** (e.g. a `gh` token): it is
  unreadable over a non-interactive ssh session, so key-file auth is the only
  thing that works headlessly.

## The core recipe — pipe a script to the node's LOGIN shell

The technique that survives cross-shell quoting hell: don't pass a quoted
command string over `ssh`; **pipe a POSIX-sh script to the node's login shell
over stdin.**

```sh
# macOS node (zsh or bash login shell):
ssh <user>@<ip> 'zsh -ls'  < ./node-script.sh
ssh <user>@<ip> 'bash -ls' < ./node-script.sh

# Windows node (drive Git Bash explicitly — NOT System32\bash.exe, that's WSL):
ssh <user>@<ip> '"C:\Program Files\Git\bin\bash.exe" -ls' < ./node-script.sh
```

Use a **login** shell (`-l`) so `PATH` includes `~/.local/bin` + `~/.cargo/bin`.
On macs, add those to `~/.zprofile` so the AI CLIs (codex / agy / claude) are on
the *login* PATH, not just whatever your interactive shell happens to export.

## Gotchas (each one cost real debugging time)

1. **macOS has no `timeout`.** Don't wrap remote commands in `timeout`; let the
   tool self-time, or guard from the conductor side.
2. **A bare `ssh -T` *inside* a stdin-piped script eats that script's stdin.**
   To test a peer's reachability from within a piped script, use
   `git ls-remote` (or a `tailscale ping`) — never an inner `ssh`.
3. **zsh does not word-split an unquoted `$VAR`.** A multi-word node list in a
   variable silently truncates to one word. Pass args as `"$@"` and drive via
   `bash -ls`, or quote every expansion explicitly.
4. **CRLF.** A script authored on Windows carries `\r`; strip it with
   `tr -d '\r'` before piping or the remote shell chokes on `command^M`.
5. **Windows paths.** `C:\Windows\System32\bash.exe` is WSL (needs `/mnt/c/...`
   paths). Prefer the Git Bash full path above + forward-slash paths.
6. **Seeding source without rsync** (often absent, and the repo can be huge):
   ```sh
   git archive HEAD | gzip | \
     ssh <node> 'rm -rf ~/Projects/<repo> && mkdir -p ~/Projects/<repo> && tar xzf - -C ~/Projects/<repo>'
   ```

## Building / verifying on a node

- Rust: `cargo build --lib` (or the relevant target) from the crate dir.
- **Capture the REAL exit code to a file — never pipe-mask it.** A trailing
  `| tail` / `| grep` returns the pipe's rc (0) and hides the command's:
  ```sh
  cmd > out.txt 2>&1; echo "RC=$?" >> out.txt   # then read out.txt
  ```
- On a Windows conductor that builds via WSL, point `CARGO_TARGET_DIR` at a
  Linux path (e.g. `/root/<crate>-target`) — Windows Defender locks the native
  `target/` dir mid-build.

## Consumers of this method

- **ensemble**: `ensemble serve` on each worker, then the conductor federates
  the run (see the federation runbook).
- **phantom-mesh** dev loop: `scripts/dev-loop/node-check.sh` (piped to each
  node's login shell — reports reach / which AI CLIs / caps / source / warm
  cache) and `scripts/dev-loop/goal-setup.sh [--smoke]` (fleet readiness probe
  + cross-node smoke).
