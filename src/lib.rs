//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod agy_adapter;
pub mod blackboard;
pub mod board;
pub mod conductor;
pub mod crew;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod exec_adapter;
pub mod gate;
pub mod journal;
pub mod ledger;
pub mod mcp;
pub mod mesh;
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
pub use board::FileBoard;
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{AgentConfig, CrewConfig, CrewError, GatePolicy, OnFlake, RoleConfig, TestConfig};
pub use discovery::{
    build_agent_hosts, discover_agent_hosts, discover_mesh, discover_nodes, parse_health_agents,
    probe_agents, Node,
};
pub use doctor::{check_tools, is_ready, present_clis, run_checks, ToolStatus};
pub use exec_adapter::ExecAdapter;
pub use gate::{decide, GateDecision, RoleVerdict};
pub use journal::{journal_path, parse as parse_journal, write_run, Entry as JournalEntry};
pub use ledger::{Counts, Ledger, LedgerError, Task, TaskState};
pub use mesh::{render_mesh, render_up};
pub use remote_adapter::RemoteAdapter;
pub use repo_sync::{
    apply_result, bundle_rev, gc_node_scratch, head_sha, is_git_worktree, merge_branch,
    merge_with_resolver, orphan_scratch, MergeOutcome,
};
pub use serve::{resolve_bind, serve, BindAddr};
pub use test_gate::{run_tests, TestOutcome};
pub use verdict::{parse_verdict, Verdict};
pub use wire::{RunRequest, RunResponse};
pub use worktree::Worktree;
