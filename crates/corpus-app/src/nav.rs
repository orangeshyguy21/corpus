//! The three screens reachable from the sidebar. Mission launch and live-run
//! state share the Missions screen. The active screen and chat-panel toggle
//! live on `AppState`, not here.

/// One nav entry per sidebar screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Agents,
    Missions,
}
