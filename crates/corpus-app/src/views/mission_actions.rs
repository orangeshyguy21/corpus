//! Shared mission commands used by both the sidebar row menu and the mission
//! splash. Views choose presentation; this module keeps availability and
//! state transitions identical at every entry point.

use std::time::Duration;

use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::{AppState, DeleteMissionResult};

pub(crate) fn launch(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) -> bool {
    state.select_mission(project, slug);
    match state.launch_mission(project, slug) {
        Ok(()) => {
            toast(toasts, ToastKind::Info, "mission started");
            true
        }
        Err(error) => {
            toast(toasts, ToastKind::Error, error.to_string());
            false
        }
    }
}

pub(crate) fn delete(state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
    let was_selected = state.selected_mission.as_deref() == Some(slug);
    match state.delete_mission(project, slug) {
        Ok(DeleteMissionResult::Scheduled) => {
            // The mission pane owns this progress state; a second transient
            // toast only obscures it and survives after the record is gone.
        }
        Ok(DeleteMissionResult::Completed) => {
            toast(toasts, ToastKind::Success, "mission deleted");
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
