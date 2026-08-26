//! Run-launch seam: materialize an agent to `.opencode/agent/`, then launch
//! an opencode mission — shared by the CLI (`corpus run`, headless) and the
//! app.
//!
//! Naming scheme: bare names (no team prefix — teams are gone). The spawned
//! opencode inherits CORPUS_PROJECT / CORPUS_STORE so the MCP server routes
//! writes into the project corpus.
//!
//! Supervisor + full-TUI decision (2026-08-11, corrected same-day):
//! an interactive launch spawns the REAL opencode TUI in a DETACHED
//! tmux session (`opencode --agent <a> --model <m> --prompt "<mission>"`)
//! — a detached session IS the headless mode, so attach/detach/close/
//! re-attach never kill the run, and attaching shows a steerable TUI,
//! not a one-shot `[exited]` dump. The TUI has no stdout, so:
//!
//! - tail = `tmux pipe-pane` raw capture (ANSI-stripped for the app), written
//!   into the project corpus runs/ as `<epoch>-<agent>.raw` from the first
//!   output — the durable run log, not the record;
//! - record = `opencode export <id>` (the newest session in the project dir) ->
//!   `<epoch>-<agent>.json` in the project corpus runs/ on Stop (best-effort —
//!   the .raw log is the durable fallback);
//! - completion = operator-driven: a TUI session doesn't exit, a run stays
//!   live until Stop (best-effort export + `tmux kill-session`) or opencode
//!   itself exits.
//!
//! The app NEVER inherits opencode's ambient default model: the model
//! is resolved primary-agent-model -> launch arg -> registry tool-use
//! default, and a launch with none fails loudly instead of spawning.
//!
//! Headless `opencode run` stays for automation (`corpus run`,
//! scripted missions): the piped spawn behind the same handle. It is
//! also the no-tmux fallback for the app (attach greys).

mod command;
mod executables;
mod plan;
mod policy;
mod process;
mod session;
mod start;
mod tmux;
mod transcript;

pub use policy::{
    agent_default_model, agent_file_stem, opencode_agent_handle, opencode_control_password,
};
pub use session::{RunLine, RunSession, StopOutcome};
pub use tmux::{kill_tmux_session, kill_tmux_session_checked, tui_attach_command};
pub use transcript::{export_session, run_idle_secs, session_conversation, session_raw_log};

use crate::store::Store;

/// Live corpus run sessions on the tmux server (the `corpus-` prefix) —
/// the app's re-attach list after a relaunch: a run outlives the app
/// by design, so a reopened app offers these for in-pane attach.
/// Empty on any failure (no tmux, no server running).
pub fn live_tui_sessions() -> Vec<String> {
    corpus_observe::live_tui_sessions()
}

/// How recently a run's TUI must have painted for the agent to count as
/// WORKING. opencode animates while a turn is in flight (spinner, token
/// stream, tool output), so a live-but-quiet capture for this long means
/// the turn is over and it's waiting. Long enough to ride out a slow frame,
/// short enough that the state settles as soon as the answer lands.
pub use corpus_observe::{MissionActivity, MissionRunState, WORKING_WINDOW_SECS};

/// What a mission is actually doing right now — the signal behind the app's
/// sidebar dots, and what the curator polls to pace a team. Derived live
/// from tmux + the raw capture; never persisted.
/// The pure activity rule, split out so it is testable and SHARED by the
/// app (which feeds an aged in-memory reading) and the one-shot
/// `mission_run_state` (which stats the capture fresh): a live session is
/// only `Working` when something was painted within `WORKING_WINDOW_SECS`.
/// No reading at all (`None`) is NOT evidence of work — it falls through to
/// `Waiting`, never `Working` (the case that used to pulse forever).
pub fn activity_from_idle(live: bool, idle_secs: Option<u64>) -> MissionActivity {
    corpus_observe::activity_from_idle(live, idle_secs)
}

/// Compute a mission's live run state ONE-SHOT — the same answer the app's
/// dots show, from any process that has the store + tmux (the curator's MCP
/// server). `live` is the current tmux listing (`live_tui_sessions()`),
/// passed in so a caller reporting many missions shells out once. A mission
/// whose recorded `session` is not live — or that has none (a piped
/// headless run, which only the app can see) — reads as `Idle`.
pub fn mission_run_state(
    store: &Store,
    project: &str,
    mission: &crate::store::Mission,
    live: &[String],
) -> MissionRunState {
    corpus_observe::mission_run_state(store, project, mission, live)
}

#[cfg(test)]
mod tests;
