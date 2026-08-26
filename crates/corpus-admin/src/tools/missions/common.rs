use corpus_observe::{MissionActivity, MissionRunState};
use corpus_store::Mission;

pub(super) fn live_label(mission: &Mission, live: &[String]) -> &'static str {
    if mission
        .session
        .as_deref()
        .is_some_and(|session| live.iter().any(|candidate| candidate == session))
    {
        "yes"
    } else {
        "no"
    }
}

pub(super) fn status_label(state: &MissionRunState) -> String {
    match state.activity {
        MissionActivity::Working => "running".to_string(),
        MissionActivity::Waiting => match state.idle_secs {
            Some(secs) => format!("waiting · last active {}", fmt_idle(secs)),
            None => "waiting".to_string(),
        },
        MissionActivity::Idle => "idle".to_string(),
    }
}

fn fmt_idle(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_duration_format_stays_compact_at_boundaries() {
        assert_eq!(fmt_idle(59), "59s");
        assert_eq!(fmt_idle(60), "1m");
        assert_eq!(fmt_idle(3_659), "1h0m");
        assert_eq!(fmt_idle(3_660), "1h1m");
    }
}
