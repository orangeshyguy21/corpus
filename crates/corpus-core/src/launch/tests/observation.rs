use std::fs;

use super::support::tmp_store;
use crate::launch::{
    activity_from_idle, mission_run_state, run_idle_secs, MissionActivity, WORKING_WINDOW_SECS,
};

#[test]
fn activity_rule_maps_liveness_and_idle_to_state() {
    use MissionActivity::*;
    // Not live is always Idle, whatever the reading.
    assert_eq!(activity_from_idle(false, Some(0)), Idle);
    assert_eq!(activity_from_idle(false, None), Idle);
    // Live + painted within the window = Working.
    assert_eq!(activity_from_idle(true, Some(0)), Working);
    assert_eq!(
        activity_from_idle(true, Some(WORKING_WINDOW_SECS - 1)),
        Working
    );
    // Live but quiet past the window = Waiting (not Working).
    assert_eq!(activity_from_idle(true, Some(WORKING_WINDOW_SECS)), Waiting);
    assert_eq!(activity_from_idle(true, Some(600)), Waiting);
    // Live with no reading is absence of evidence, not work.
    assert_eq!(activity_from_idle(true, None), Waiting);
}

#[test]
fn mission_run_state_is_idle_without_a_live_session() {
    let (store, dir) = tmp_store("run-state");
    let mut mission = crate::store::Mission {
        agent: "recon".to_string(),
        pins: Default::default(),
        budget: None,
        created: 0,
        name: None,
        session: None,
        control: None,
        opencode_session: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    // No session at all → Idle, no reading.
    let s = mission_run_state(&store, "p", &mission, &[]);
    assert_eq!(s.activity, MissionActivity::Idle);
    assert_eq!(s.idle_secs, None);
    // A recorded session that is NOT in the live listing → still Idle.
    mission.session = Some("corpus-recon-1786911614".to_string());
    let s = mission_run_state(&store, "p", &mission, &["corpus-other-1".to_string()]);
    assert_eq!(s.activity, MissionActivity::Idle);
    let _ = fs::remove_dir_all(&dir);
}

/// `run_idle_secs` reads the capture's age — the busy signal. A
/// missing capture is None (no evidence), a fresh write is ~0s.
#[test]
fn run_idle_secs_reads_capture_age() {
    let dir = std::env::temp_dir().join(format!("corpus-idle-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("run.raw");
    assert_eq!(run_idle_secs(&log), None, "no capture yet is not activity");
    fs::write(&log, "painting\n").unwrap();
    assert!(
        run_idle_secs(&log).unwrap() < 2,
        "a just-written capture reads as fresh"
    );
    let _ = fs::remove_dir_all(&dir);
}
