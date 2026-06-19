//! ensemble — a governed orchestrator that runs different-vendor AI coding CLIs as one
//! collaborative dev crew. See docs/2026-06-19-ensemble-design.md.

pub mod adapter;
pub mod blackboard;
pub mod conductor;
pub mod crew;
pub mod gate;
pub mod verdict;

pub use adapter::{Adapter, AdapterError, AgentOutput, MockAdapter};
pub use blackboard::{Blackboard, Message};
pub use conductor::{Conductor, Decision, RunOutcome};
pub use crew::{CrewConfig, GatePolicy, OnFlake, RoleConfig};
pub use gate::{decide, GateDecision, RoleVerdict};
pub use verdict::{parse_verdict, Verdict};
