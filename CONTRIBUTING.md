# Contributing to ensemble

Thanks for your interest. ensemble is early — the design lives in
[`docs/2026-06-19-ensemble-design.md`](docs/2026-06-19-ensemble-design.md); read it first.

## Ground rules

- **Discuss before large changes.** Open an issue describing the problem and your approach
  before a big PR, so we don't duplicate or diverge from the design.
- **One concern per PR.** Keep changes focused and reviewable.
- **Tests required.** New behavior needs tests. The conductor/blackboard/gate are designed to
  be testable without a live AI CLI via a mock adapter; live-CLI paths are `#[ignore]` smokes.
- **No faked passes.** A core value is that a flaking/unavailable agent is *degraded and
  logged*, never silently treated as approval. Don't add code paths that hide failure.

## Dev loop

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## Licensing

By contributing, you agree your contributions are licensed under the project's
[Apache-2.0](LICENSE) license.
