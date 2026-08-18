//! The app's nav: the three screens reachable from the sidebar. The old
//! standalone `Screen::Launch` is gone — `LaunchView` merges into the
//! mission view at chunk 5 (its machinery stays intact; the run view
//! takes over the main column while a run is live). The active screen and
//! the chat-panel toggle live on `AppState`, not here.

/// One nav entry per sidebar screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Agents,
    Missions,
}
