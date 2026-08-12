//! The app's nav: one entry per planned screen, greyed until its chunk
//! lands, plus the screen-change requests views issue (launch switches
//! the operator to the run view).

/// One nav entry per planned screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Agents,
    Missions,
    Launch,
}

impl Screen {
    pub const ALL: [Screen; 4] = [Screen::Projects, Screen::Agents, Screen::Missions, Screen::Launch];

    pub fn label(self) -> &'static str {
        match self {
            Screen::Projects => "Projects",
            Screen::Agents => "Agents",
            Screen::Missions => "Missions",
            Screen::Launch => "Launch",
        }
    }

    /// Where the screen comes from in the plan (nav tooltips).
    pub fn note(self) -> &'static str {
        match self {
            Screen::Projects => "project list, create, clone, delete",
            Screen::Agents => "the selected project's agents: raw JSON editor with core-side validation",
            Screen::Missions => "mission list + launch reusing the existing run view",
            Screen::Launch => "run view: embedded terminal pane on the run's tmux session + abort/dismiss",
        }
    }
}