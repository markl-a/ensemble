//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod agy_adapter;
pub mod blackboard;
pub mod conductor;
pub mod crew;
pub mod exec_adapter;
pub mod gate;
pub mod remote_adapter;
pub mod serve;
pub mod verdict;
pub mod wire;
pub mod worktree;

pub use adapter::{Adapter, AdapterError, AgentOutput, MockAdapter};
pub use agy_adapter::AgyAdapter;
pub use blackboard::{Blackboard, Message};
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{AgentConfig, CrewConfig, CrewError, GatePolicy, OnFlake, RoleConfig};
pub use exec_adapter::ExecAdapter;
pub use gate::{decide, GateDecision, RoleVerdict};
pub use remote_adapter::RemoteAdapter;
pub use serve::serve;
pub use verdict::{parse_verdict, Verdict};
pub use wire::{RunRequest, RunResponse};
pub use worktree::Worktree;
