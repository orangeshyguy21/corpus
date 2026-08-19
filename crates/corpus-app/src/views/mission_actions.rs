//! Shared mission commands used by both the sidebar row menu and the mission
//! splash. Views choose presentation; this module keeps availability and
//! state transitions identical at every entry point.

use std::time::Duration;

use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::{AppState, StopMissionResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Availability {
    pub launch: bool,
    pub resume: bool,
    pub stop: bool,
    pub retry_cleanup: bool,
    pub delete: bool,
}

impl Availability {
    pub(crate) fn resolve(live: bool, resumable: bool, inflight: bool, cleanup: bool) -> Self {
        Self {
            launch: !live && !inflight && !cleanup,
            resume: !live && resumable && !inflight && !cleanup,
            stop: live,
            retry_cleanup: !live && cleanup,
            delete: !inflight && !live && !cleanup,
        }
    }
}

pub(crate) fn launch(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) -> bool {
    state.select_mission(project, slug);
    match state.launch_mission(project, slug) {
        Ok(()) => true,
        Err(error) => {
            toast(toasts, ToastKind::Error, error.to_string());
            false
        }
    }
}

pub(crate) fn resume(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) -> bool {
    state.select_mission(project, slug);
    match state.resume_mission(project, slug) {
        Ok(()) => true,
        Err(error) => {
            toast(toasts, ToastKind::Error, error.to_string());
            false
        }
    }
}

pub(crate) fn stop(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
    match state.stop_mission(project, slug) {
        Ok(StopMissionResult::Scheduled) => {
            toast(toasts, ToastKind::Info, format!("stopping mission {slug}…"));
        }
        Ok(StopMissionResult::Completed(path)) => {
            let detail = if path.is_empty() {
                format!("stopped mission {slug}")
            } else {
                format!("stopped mission {slug} — transcript: {path}")
            };
            toast(toasts, ToastKind::Success, detail);
            state.refresh_missions(project);
        }
        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
    }
}

pub(crate) fn delete(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
    let was_selected = state.selected_mission.as_deref() == Some(slug);
    match state.delete_mission(project, slug) {
        Ok(()) => {
            toast(
                toasts,
                ToastKind::Success,
                format!("deleted mission {slug}"),
            );
            state.refresh_missions(project);
            if was_selected {
                state.selected_mission = None;
            }
        }
        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
    }
}

fn toast(toasts: &mut Toasts, kind: ToastKind, text: impl Into<String>) {
    toasts.add(
        Toast::new()
            .kind(kind)
            .text(text.into())
            .options(ToastOptions::default().duration(Duration::from_secs(4))),
    );
}

#[cfg(test)]
mod tests {
    use super::Availability;

    #[test]
    fn availability_follows_run_lifecycle() {
        assert_eq!(
            Availability::resolve(false, false, false, false),
            Availability {
                launch: true,
                resume: false,
                stop: false,
                retry_cleanup: false,
                delete: true,
            }
        );
        assert_eq!(
            Availability::resolve(false, true, false, false),
            Availability {
                launch: true,
                resume: true,
                stop: false,
                retry_cleanup: false,
                delete: true,
            }
        );
        assert_eq!(
            Availability::resolve(true, true, false, false),
            Availability {
                launch: false,
                resume: false,
                stop: true,
                retry_cleanup: false,
                delete: false,
            }
        );
        assert_eq!(
            Availability::resolve(false, true, true, false),
            Availability {
                launch: false,
                resume: false,
                stop: false,
                retry_cleanup: false,
                delete: false,
            }
        );
        assert_eq!(
            Availability::resolve(false, true, false, true),
            Availability {
                launch: false,
                resume: false,
                stop: false,
                retry_cleanup: true,
                delete: false,
            }
        );
    }
}
