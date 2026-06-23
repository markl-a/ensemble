//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod agy_adapter;
pub mod blackboard;
pub mod board;
pub mod conductor;
pub mod control_plane;
pub mod controlled;
pub mod council;
pub mod crew;
pub mod discovery;
pub mod dispatch;
pub mod doctor;
pub mod exec_adapter;
pub mod gate;
pub mod journal;
pub mod ledger;
pub mod mcp;
pub mod mcp_install;
pub mod mesh;
pub mod ndjson;
pub mod remote_adapter;
pub mod repo_sync;
pub mod serve;
pub mod supervise;
pub mod supervisor;
pub mod team;
pub mod test_gate;
pub mod verdict;
pub mod wire;
pub mod worktree;

pub use adapter::{Adapter, AdapterError, AgentOutput, MockAdapter};
pub use agy_adapter::AgyAdapter;
pub use blackboard::{Blackboard, Message};
pub use board::FileBoard;
pub use conductor::{Conductor, Decision, RunOutcome};
pub use control_plane::{ControlPlane, LocalControlPlane, RemoteControlPlane};
pub use controlled::{
    control_script, pty_program_for_vendor, run_controlled_pty, ControlledPtyConfig,
};
pub use council::{council_targets, render_council, short_host, CouncilTarget};
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
pub use ndjson::Feed;
pub use remote_adapter::RemoteAdapter;
pub use repo_sync::{
    apply_result, bundle_rev, gc_node_scratch, head_sha, is_git_worktree, merge_branch,
    merge_with_resolver, orphan_scratch, MergeOutcome,
};
pub use serve::{resolve_bind, serve, BindAddr};
pub use supervise::{
    drain_control, member_control_path, member_stream_path, parse_watch_args, render_event,
    render_line, ControlCmd, ControlState, FeedObserver, RunObserver, StreamEvent, WatchArgs,
};
pub use supervisor::{
    build_supervisor_prompt, collect_supervisor_evidence, control_action_for_report,
    parse_supervisor_report, EvidenceLine, SupervisorApply, SupervisorEvidence,
    SupervisorRecommendation, SupervisorReport,
};
pub use team::{
    default_member_name, default_team_name, member_file_stem, post_team_message, read_team_inbox,
    render_team_inbox, render_team_status, resolve_team_session, team_root, team_status, TeamInbox,
    TeamLedgerCounts, TeamSession, TeamStatus,
};
pub use test_gate::{run_tests, TestOutcome};
pub use verdict::{parse_verdict, Verdict};
pub use wire::{RunRequest, RunResponse};
pub use worktree::{ensure_kept_worktree, KeptWorktree, Worktree};
