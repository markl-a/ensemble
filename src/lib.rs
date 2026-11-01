//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.
//!
//! Cargo feature `cli` (default) builds everything the `ensemble` binary needs. With
//! `default-features = false` only the crew core is compiled: [`adapter`], [`blackboard`],
//! [`conductor`] (the in-memory `Conductor::run`), [`crew`], [`gate`], [`verdict`], [`test_gate`],
//! [`util`] and, from [`supervise`], `RunObserver` + `ControlState`.

// --- crew core: always built ---
pub mod adapter;
pub mod blackboard;
pub mod conductor;
pub mod crew;
pub mod gate;
pub mod supervise;
pub mod test_gate;
pub mod util;
pub mod verdict;

// --- the rest of the program: `cli` feature ---
#[cfg(feature = "cli")]
pub mod agy_adapter;
#[cfg(feature = "cli")]
pub mod board;
#[cfg(feature = "cli")]
pub mod control_plane;
#[cfg(feature = "cli")]
pub mod controlled;
#[cfg(feature = "cli")]
pub mod council;
#[cfg(feature = "cli")]
pub mod discovery;
#[cfg(feature = "cli")]
pub mod dispatch;
#[cfg(feature = "cli")]
pub mod doctor;
#[cfg(feature = "cli")]
pub mod exec_adapter;
#[cfg(feature = "cli")]
pub mod journal;
#[cfg(feature = "cli")]
pub mod ledger;
#[cfg(feature = "cli")]
pub mod mcp;
#[cfg(feature = "cli")]
pub mod mcp_install;
#[cfg(feature = "cli")]
pub mod mesh;
#[cfg(feature = "cli")]
pub mod ndjson;
#[cfg(feature = "cli")]
pub mod remote_adapter;
#[cfg(feature = "cli")]
pub mod repo_sync;
#[cfg(feature = "cli")]
pub mod serve;
#[cfg(feature = "cli")]
pub mod service;
#[cfg(feature = "cli")]
pub mod supervisor;
#[cfg(feature = "cli")]
pub mod team;
#[cfg(feature = "cli")]
pub mod wire;
#[cfg(feature = "cli")]
pub mod worktree;

pub use adapter::{
    detect_rate_limit, Adapter, AdapterError, AgentOutput, MockAdapter, RateLimitInfo,
};
pub use blackboard::{Blackboard, Message};
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{
    AgentConfig, CrewConfig, CrewError, CrewInspection, CrewRoleInspection, GatePolicy, OnFlake,
    RoleConfig, TestConfig,
};
pub use gate::{decide, GateDecision, RoleVerdict};
#[cfg(feature = "cli")]
pub use supervise::{
    drain_control, member_control_path, member_stream_path, parse_watch_args, render_event,
    render_line, ControlCmd, FeedObserver, StreamEvent, WatchArgs,
};
pub use supervise::{ControlState, RunObserver};
pub use test_gate::{run_tests, TestOutcome};
pub use util::is_ipv4_literal;
pub use verdict::{parse_verdict, Verdict};

#[cfg(feature = "cli")]
pub use agy_adapter::AgyAdapter;
#[cfg(feature = "cli")]
pub use board::FileBoard;
#[cfg(feature = "cli")]
pub use control_plane::{ControlPlane, LocalControlPlane, RemoteControlPlane};
#[cfg(feature = "cli")]
pub use controlled::{
    control_script, pty_program_for_vendor, run_controlled_pty, ControlledPtyConfig,
};
#[cfg(feature = "cli")]
pub use council::{council_targets, render_council, short_host, CouncilTarget};
#[cfg(feature = "cli")]
pub use discovery::{
    build_agent_hosts, discover_agent_hosts, discover_mesh, discover_nodes, parse_health_agents,
    probe_agents, Node,
};
#[cfg(feature = "cli")]
pub use doctor::{check_tools, is_ready, present_clis, run_checks, ToolStatus};
#[cfg(feature = "cli")]
pub use exec_adapter::ExecAdapter;
#[cfg(feature = "cli")]
pub use journal::{
    journal_path, parse as parse_journal, sanitize_slug, write_run, Entry as JournalEntry,
};
#[cfg(feature = "cli")]
pub use ledger::{Counts, Ledger, LedgerError, Task, TaskState};
#[cfg(feature = "cli")]
pub use mesh::{render_mesh, render_up};
#[cfg(feature = "cli")]
pub use ndjson::Feed;
#[cfg(feature = "cli")]
pub use remote_adapter::RemoteAdapter;
#[cfg(feature = "cli")]
pub use repo_sync::{
    apply_result, bundle_rev, gc_node_scratch, head_sha, is_git_worktree, merge_branch,
    merge_with_resolver, orphan_scratch, MergeOutcome,
};
#[cfg(feature = "cli")]
pub use serve::{resolve_bind, serve, serve_with_token, BindAddr};
#[cfg(feature = "cli")]
pub use service::{
    launchd_agent_path, launchd_plist, serve_program_args, systemd_unit, systemd_user_unit_path,
    windows_end_argv, windows_install_argv, windows_run_argv, windows_uninstall_argv,
    ServeServiceConfig, LAUNCHD_LABEL, SYSTEMD_UNIT_NAME, WINDOWS_TASK_NAME,
};
#[cfg(feature = "cli")]
pub use supervisor::{
    build_supervisor_prompt, collect_supervisor_evidence, control_action_for_report,
    parse_supervisor_report, EvidenceLine, SupervisorApply, SupervisorEvidence,
    SupervisorRecommendation, SupervisorReport,
};
#[cfg(feature = "cli")]
pub use team::{
    default_member_name, default_team_name, member_file_stem, post_team_message, read_team_inbox,
    render_team_inbox, render_team_status, resolve_team_session, team_root, team_status,
    validate_repo_path, TeamInbox, TeamLedgerCounts, TeamObserver, TeamSession, TeamStatus,
};
#[cfg(feature = "cli")]
pub use wire::{RunRequest, RunResponse};
#[cfg(feature = "cli")]
pub use worktree::{ensure_kept_worktree, KeptWorktree, Worktree};
