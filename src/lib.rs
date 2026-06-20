//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod agy_adapter;
pub mod blackboard;
pub mod conductor;
pub mod crew;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod exec_adapter;
pub mod gate;
pub mod ledger;
pub mod remote_adapter;
pub mod repo_sync;
pub mod serve;
pub mod test_gate;
pub mod verdict;
pub mod wire;
pub mod worktree;

pub use adapter::{Adapter, AdapterError, AgentOutput, MockAdapter};
pub use agy_adapter::AgyAdapter;
pub use blackboard::{Blackboard, Message};
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{AgentConfig, CrewConfig, CrewError, GatePolicy, OnFlake, RoleConfig, TestConfig};
pub use discovery::{
    build_agent_hosts, discover_agent_hosts, discover_nodes, parse_health_agents, probe_agents,
    Node,
};
pub use doctor::{check_tools, is_ready, run_checks, ToolStatus};
pub use exec_adapter::ExecAdapter;
pub use gate::{decide, GateDecision, RoleVerdict};
pub use ledger::{Counts, Ledger, LedgerError, Task, TaskState};
pub use remote_adapter::RemoteAdapter;
pub use repo_sync::{apply_result, bundle_rev, head_sha, is_git_worktree};
pub use serve::serve;
pub use test_gate::{run_tests, TestOutcome};
pub use verdict::{parse_verdict, Verdict};
pub use wire::{RunRequest, RunResponse};
pub use worktree::Worktree;
