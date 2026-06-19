# ensemble

**A local-first, governed orchestrator that turns different-vendor AI coding CLIs into one
collaborative dev crew.**

Drive **Claude Code**, **OpenAI Codex**, **Google Antigravity (`agy`)**, and **opencode**
together as a crew that **develops, reviews, and debugs** one project — running **parallel
tasks** *and* a **role pipeline** (implement → review → debug), with the agents
**communicating with each other** through a mediated blackboard. The subscriptions can live
on **several machines connected by Tailscale**.

What makes it different from the existing multi-CLI runners: the **intersection** of
governance (quorum gate, graceful degradation when a vendor flakes, signed provenance),
cross-vendor, cross-machine over Tailscale, and inter-agent communication — which no single
open-source tool covers today.

> **Status: early.** Design in [`docs/2026-06-19-ensemble-design.md`](docs/2026-06-19-ensemble-design.md).
> Phase 0 (project shell) in progress. Not yet usable.

## The crew (how it works)

```
task → worktree → codex implements + posts "did X" to the blackboard
                → claude reads the diff + blackboard → posts APPROVE / CHANGES + a message
                → on CHANGES, codex receives claude's message and revises  (← agents talk)
                → agy runs the tests / finds bugs + posts findings
                → quorum gate → merge the worktree, or escalate
```

A vendor that flakes/empties/rate-limits is **degraded** (retried, substituted, or excluded
from the quorum with a logged reason) — never faked into a pass.

## Roadmap

- **Phase 0** — project shell: `Adapter` trait + mock adapter, `crew.toml`, hermetic tests.
- **Phase 1** — single-machine role pipeline + blackboard (the first working 4-CLI loop).
- **Phase 2** — single-machine parallel tasks.
- **Phase 3** — cross-machine over Tailscale (remote adapters + shared blackboard).
- **Phase 4** — governance hardening (signed proofpack, blind review, ACP/MCP alignment).

## License

[Apache-2.0](LICENSE).
