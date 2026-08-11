//! The Missions view: launch/abort local missions, remote opencode
//! servers, or replay a stored run — with a live, capped transcript.
//!
//! M1: local `opencode run` + replay. M2 adds remote servers (thin HTTP
//! client, transcript via message polling). One active mission at a time,
//! driven as an `Active` runner so all three flavours share one renderer.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use egui::{RichText, ScrollArea, Ui};

use crate::mission::Runner;
use crate::remote::Session;
use crate::store::{strip_ansi, Category, Store};

/// How many transcript lines to keep in memory (they grow fast and the
/// view must not accumulate megabytes).
const MAX_TRANSCRIPT_LINES: usize = 2000;

/// Which mission flavour the controls panel is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Replay,
    Remote,
}

/// The currently active mission, whichever flavour.
#[derive(Debug)]
enum Active {
    Local(Runner),
    Remote(Session),
}

/// Result of a background remote liveness/agent fetch.
#[derive(Debug)]
struct RemoteResult {
    health_ok: bool,
    mcp_ok: bool,
    notes: String,
    agents: Vec<String>,
}

/// Missions view state.
#[derive(Debug)]
pub struct View {
    mode: Mode,
    agent: String,
    model: String,
    mission: String,
    active: Option<Active>,
    transcript: Vec<String>,
    replay: Option<PathBuf>,
    started_at: Option<Instant>,

    // Remote mode state.
    servers: Vec<crate::remote::ServerConfig>,
    sel_server: usize,
    sel_agent: usize,
    health: Option<RemoteResult>,
    remote_rx: Option<Receiver<RemoteResult>>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            mode: Mode::Local,
            agent: "operator".into(),
            model: String::new(),
            mission: String::new(),
            active: None,
            transcript: Vec::new(),
            replay: None,
            started_at: None,
            servers: crate::remote::ServersConfig::load().servers,
            sel_server: 0,
            sel_agent: 0,
            health: None,
            remote_rx: None,
        }
    }
}

impl View {
    /// Left controls panel: mode, form, and Run/Abort.
    pub fn controls(&mut self, ui: &mut Ui, store: &Store) {
        self.drain_remote();

        ui.add_space(8.0);
        ui.heading("Missions");
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Local, "Local");
            ui.selectable_value(&mut self.mode, Mode::Replay, "Replay");
            ui.selectable_value(&mut self.mode, Mode::Remote, "Remote");
        });
        ui.separator();

        match self.mode {
            Mode::Local => self.local_controls(ui),
            Mode::Replay => self.replay_controls(ui, store),
            Mode::Remote => self.remote_controls(ui),
        }

        ui.add_space(8.0);
        ui.separator();
        match self.active.as_ref() {
            Some(active) => {
                let running = is_running(active);
                let state = if running {
                    RichText::new("● running").color(egui::Color32::from_rgb(140, 220, 140))
                } else {
                    RichText::new("● finished").color(egui::Color32::from_rgb(150, 150, 160))
                };
                ui.label(state);
                if let Some(path) = active_log_path(active) {
                    ui.label(RichText::new(path.display().to_string()).small().weak())
                        .on_hover_text("run log");
                }
            }
            None => {
                ui.weak("no mission running");
            }
        }
    }

    /// Local form: agent, model, mission box, Run/Abort.
    fn local_controls(&mut self, ui: &mut Ui) {
        ui.label("agent");
        ui.text_edit_singleline(&mut self.agent);
        ui.label("model (blank = default)");
        ui.text_edit_singleline(&mut self.model);
        ui.label("mission");
        self.mission_box(ui);

        ui.add_space(8.0);
        let running = self.active.as_ref().map(is_running).unwrap_or(false);
        ui.horizontal(|ui| {
            let run_enabled = !running && !self.mission.trim().is_empty();
            if ui
                .add_enabled(run_enabled, egui::Button::new("Run"))
                .on_hover_text("opencode run — logged to store/runs/")
                .clicked()
            {
                self.start_local();
            }
            if ui.add_enabled(running, egui::Button::new("Abort")).clicked() {
                self.abort_active();
            }
        });
    }

    /// Remote form: server picker + refresh, agent picker, Fire/Abort.
    fn remote_controls(&mut self, ui: &mut Ui) {
        ui.label("server");
        if self.servers.is_empty() {
            ui.weak("no servers configured");
            ui.label(
                RichText::new("~/.config/corpus/servers.toml (or CORPUS_SERVERS)")
                    .small()
                    .weak(),
            );
            return;
        }
        self.sel_server = self.sel_server.min(self.servers.len() - 1);
        egui::ComboBox::from_id_salt("server")
            .selected_text(self.servers[self.sel_server].name.clone())
            .show_ui(ui, |ui| {
                for (i, srv) in self.servers.iter().enumerate() {
                    ui.selectable_value(&mut self.sel_server, i, &srv.name);
                }
            });

        ui.horizontal(|ui| {
            match &self.health {
                Some(h) => {
                    let (color, text) = if h.health_ok {
                        (egui::Color32::from_rgb(140, 220, 140), "● online")
                    } else {
                        (egui::Color32::from_rgb(255, 140, 100), "● offline")
                    };
                    ui.colored_label(color, text);
                    let mcp = if h.mcp_ok { "mcp ✓" } else { "mcp ✗" };
                    ui.weak(mcp);
                    if !h.notes.is_empty() {
                        ui.weak(&h.notes);
                    }
                }
                None => {
                    ui.weak("not probed");
                }
            }
            if ui.button("Refresh").clicked() {
                self.refresh_remote();
            }
        });

        ui.label("agent");
        self.sel_agent = self.sel_agent.min(self.agents().len().saturating_sub(1));
        if self.agents().is_empty() {
            ui.weak("(refresh to discover agents)");
            ui.text_edit_singleline(&mut self.agent);
        } else {
            let names = self.agents().to_vec();
            egui::ComboBox::from_id_salt("agent")
                .selected_text(names[self.sel_agent.min(names.len() - 1)].clone())
                .show_ui(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.sel_agent, i, name);
                    }
                });
        }

        ui.label("model (blank = default)");
        ui.text_edit_singleline(&mut self.model);
        ui.label("mission");
        self.mission_box(ui);

        ui.add_space(8.0);
        let running = self.active.as_ref().map(is_running).unwrap_or(false);
        ui.horizontal(|ui| {
            let ready = !running
                && !self.mission.trim().is_empty()
                && !self.servers.is_empty()
                && !self.agents().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Fire")).clicked() {
                self.start_remote();
            }
            if ui.add_enabled(running, egui::Button::new("Abort")).clicked() {
                self.abort_active();
            }
        });
    }

    /// Replay form: pick a stored run, then Replay/Abort.
    fn replay_controls(&mut self, ui: &mut Ui, store: &Store) {
        ui.weak("pick a stored run to replay");
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("replay-list")
            .max_height(320.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in store.of(Category::Runs) {
                    let selected = self.replay.as_ref() == Some(&entry.path);
                    if ui
                        .selectable_label(selected, entry.title())
                        .on_hover_text(entry.path.display().to_string())
                        .clicked()
                    {
                        self.replay = Some(entry.path.clone());
                    }
                }
            });

        ui.add_space(8.0);
        let running = self.active.as_ref().map(is_running).unwrap_or(false);
        ui.horizontal(|ui| {
            let ready = !running && self.replay.is_some();
            if ui.add_enabled(ready, egui::Button::new("Replay")).clicked() {
                self.start_replay();
            }
            if ui.add_enabled(running, egui::Button::new("Abort")).clicked() {
                self.abort_active();
            }
        });
    }

    /// Central panel: the live transcript (scroll-capped).
    pub fn transcript(&mut self, ui: &mut Ui) {
        // Drain any new lines from the active runner.
        if let Some(active) = self.active.as_mut() {
            let lines = drain(active);
            if !lines.is_empty() {
                for line in lines {
                    self.transcript.push(line);
                }
                if self.transcript.len() > MAX_TRANSCRIPT_LINES {
                    let excess = self.transcript.len() - MAX_TRANSCRIPT_LINES;
                    self.transcript.drain(0..excess);
                }
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Transcript");
            if let Some(started) = self.started_at {
                ui.weak(format!("· {}s elapsed", started.elapsed().as_secs()));
            }
            if self.transcript.is_empty() {
                ui.label(RichText::new("(no mission yet)").weak());
            }
        });
        ui.separator();

        let mut scroll = ScrollArea::vertical()
            .id_salt("missions-transcript")
            .auto_shrink([false, false]);
        if self.active.as_ref().map(is_running).unwrap_or(false) {
            scroll = scroll.stick_to_bottom(true);
        }
        scroll.show(ui, |ui| {
            for line in &self.transcript {
                ui.monospace(strip_ansi(line));
            }
        });
    }

    /// Shared mission text box (multiline, fills the panel).
    fn mission_box(&mut self, ui: &mut Ui) {
        ui.add(
            egui::TextEdit::multiline(&mut self.mission)
                .desired_rows(8)
                .desired_width(f32::INFINITY),
        );
    }

    /// Kick off a real local mission from the form fields.
    fn start_local(&mut self) {
        let agent = self.agent.trim().to_string();
        if agent.is_empty() {
            self.push_status("agent is required");
            return;
        }
        let mission = self.mission.trim().to_string();
        if mission.is_empty() {
            self.push_status("mission is required");
            return;
        }
        let model = optional_model(&self.model);
        match Runner::spawn(&agent, model.as_deref(), &mission) {
            Ok(runner) => self.activate(Active::Local(runner)),
            Err(error) => self.push_status(format!("failed: {error}")),
        }
    }

    /// Kick off a replay of the selected stored run.
    fn start_replay(&mut self) {
        let Some(path) = self.replay.clone() else { return };
        match Runner::replay(&path) {
            Ok(runner) => self.activate(Active::Local(runner)),
            Err(error) => self.push_status(format!("replay failed: {error}")),
        }
    }

    /// Kick off a mission on the selected remote server.
    fn start_remote(&mut self) {
        let mission = self.mission.trim().to_string();
        if mission.is_empty() {
            self.push_status("mission is required");
            return;
        }
        let Some(cfg) = self.servers.get(self.sel_server).cloned() else {
            self.push_status("select a server");
            return;
        };
        let agent = if self.agents().is_empty() {
            self.agent.trim().to_string()
        } else {
            self.agents()[self.sel_agent.min(self.agents().len() - 1)].clone()
        };
        let model = optional_model(&self.model);
        match Session::start(&cfg, &agent, model.as_deref(), &mission) {
            Ok(session) => self.activate(Active::Remote(session)),
            Err(error) => self.push_status(format!("remote failed: {error}")),
        }
    }

    /// Swap in a new active mission and reset the transcript.
    fn activate(&mut self, active: Active) {
        self.transcript.clear();
        self.started_at = Some(Instant::now());
        self.active = Some(active);
    }

    /// Abort whatever is running.
    fn abort_active(&mut self) {
        if let Some(active) = self.active.as_mut() {
            abort(active);
        }
    }

    /// The remote agents list (empty if never fetched).
    fn agents(&self) -> &[String] {
        self.health
            .as_ref()
            .map(|h| h.agents.as_slice())
            .unwrap_or(&[])
    }

    /// Fire a background thread to probe the selected server.
    fn refresh_remote(&mut self) {
        let Some(cfg) = self.servers.get(self.sel_server).cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.remote_rx = Some(rx);
        std::thread::spawn(move || {
            let result = probe_server(&cfg);
            let _ = tx.send(result);
        });
    }

    fn drain_remote(&mut self) {
        if let Some(rx) = self.remote_rx.as_ref() {
            match rx.try_recv() {
                Ok(result) => {
                    self.health = Some(result);
                    self.remote_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.remote_rx = None;
                }
            }
        }
    }

    /// Append a status line to the transcript (e.g. an error).
    fn push_status(&mut self, text: impl Into<String>) {
        let line = format!("> {}\n", text.into());
        self.transcript.push(line);
        if self.transcript.len() > MAX_TRANSCRIPT_LINES {
            self.transcript.remove(0);
        }
    }
}

fn is_running(active: &Active) -> bool {
    match active {
        Active::Local(r) => r.is_running(),
        Active::Remote(s) => s.is_running(),
    }
}

fn drain(active: &mut Active) -> Vec<String> {
    match active {
        Active::Local(r) => r.poll(),
        Active::Remote(s) => s.poll(),
    }
}

fn abort(active: &mut Active) {
    match active {
        Active::Local(r) => r.abort(),
        Active::Remote(s) => s.abort(),
    }
}

fn active_log_path(active: &Active) -> Option<&std::path::Path> {
    match active {
        Active::Local(r) => r.log_path(),
        Active::Remote(_) => None,
    }
}

fn optional_model(model: &str) -> Option<String> {
    let m = model.trim();
    if m.is_empty() {
        None
    } else {
        Some(m.to_string())
    }
}

/// Probe a remote server's health + discovery on a worker thread.
fn probe_server(cfg: &crate::remote::ServerConfig) -> RemoteResult {
    use crate::remote::RemoteClient;
    match RemoteClient::new(cfg) {
        Ok(client) => {
            let status = client.health().unwrap_or_default();
            let agents = client.agents().unwrap_or_default();
            RemoteResult {
                health_ok: status.healthy,
                mcp_ok: status.mcp_ok,
                notes: status.notes,
                agents,
            }
        }
        Err(e) => RemoteResult {
            health_ok: false,
            mcp_ok: false,
            notes: e,
            agents: Vec::new(),
        },
    }
}
