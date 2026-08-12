//! The deck's nav: one entry per planned screen, greyed until its chunk
//! lands, plus the screen-change requests views issue (launch switches
//! the operator to the run view).

/// One nav entry per planned screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Teams,
    Agents,
    Launch,
}

impl Screen {
    pub const ALL: [Screen; 4] = [Screen::Projects, Screen::Teams, Screen::Agents, Screen::Launch];

    pub fn label(self) -> &'static str {
        match self {
            Screen::Projects => "Projects",
            Screen::Teams => "Teams",
            Screen::Agents => "Agents",
            Screen::Launch => "Launch",
        }
    }

    /// Where the screen comes from in the plan (nav tooltips).
    pub fn note(self) -> &'static str {
        match self {
            Screen::Projects => "project list, create, clone, delete",
            Screen::Teams => "the selected project's teams: create, edit, clone, delete, wipe, launch",
            Screen::Agents => "template editors (permission/prompt/agent) + add-agent-to-team",
            Screen::Launch => "run view: embedded terminal pane on the run's tmux session + abort/dismiss",
        }
    }
}