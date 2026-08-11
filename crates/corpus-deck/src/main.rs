//! corpus-deck: the operator's window into the research team (egui).
//!
//! One window, four views: Arena, Missions, Corpus, Prompts. Milestone M0
//! ships the scaffold + the Corpus view (store browser + markdown viewer)
//! plus a status bar reporting environment health via `corpus-core`.

mod mission;
mod remote;
mod store;
mod views;

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::store::Store;
use crate::views::corpus;
use crate::views::missions;

/// Which top-level view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Arena,
    Missions,
    Corpus,
    Prompts,
}

impl View {
    const ALL: [View; 4] = [View::Arena, View::Missions, View::Corpus, View::Prompts];

    fn label(self) -> &'static str {
        match self {
            View::Arena => "Arena",
            View::Missions => "Missions",
            View::Corpus => "Corpus",
            View::Prompts => "Prompts",
        }
    }
}

/// A background environment-status report (probe + targets).
#[derive(Debug)]
struct EnvStatus {
    ready: bool,
    notes: String,
    targets: Vec<String>,
    plugins: usize,
}

/// corpus-deck application state.
struct App {
    store: Store,
    view: View,
    corpus: corpus::View,
    missions: missions::View,
    /// Last time `store/` was polled (1 Hz refresh).
    last_scan: Instant,
    /// Worker channel carrying environment status.
    status_rx: Receiver<EnvStatus>,
    status_tx: Sender<EnvStatus>,
    status: Option<EnvStatus>,
    /// Last time a probe worker was launched (re-probe cadence).
    last_probe: Instant,
}

/// Store poll cadence.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Environment re-probe cadence: the environment changes under us (mints
/// get restarted mid-demo); the status bar must heal like the gate does.
const PROBE_INTERVAL: Duration = Duration::from_secs(15);
/// UI refresh cadence: without this the store poll would be invisible —
/// egui only repaints on input unless asked.
const REPAINT_INTERVAL: Duration = Duration::from_millis(500);

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let (status_tx, status_rx) = mpsc::channel();
        let mut app = Self {
            store: Store::scan(),
            view: View::Corpus,
            corpus: corpus::View::default(),
            missions: missions::View::default(),
            last_scan: Instant::now(),
            status_rx,
            status_tx,
            status: None,
            last_probe: Instant::now() - PROBE_INTERVAL,
        };
        app.start_probe();
        app
    }

    /// Kick off a worker thread that discovers plugins and probes the
    /// first one, reporting readiness + targets (never blocks the UI).
    fn start_probe(&mut self) {
        self.last_probe = Instant::now();
        let tx = self.status_tx.clone();
        std::thread::spawn(move || {
            let report = probe_environment();
            let _ = tx.send(report);
        });
    }

    /// Poll the store and re-probe the environment on their cadences.
    fn poll(&mut self) {
        if self.last_scan.elapsed() >= SCAN_INTERVAL {
            self.last_scan = Instant::now();
            self.store = Store::scan();
        }
        if self.last_probe.elapsed() >= PROBE_INTERVAL {
            self.start_probe();
        }
    }

    fn drain_status(&mut self) {
        while let Ok(status) = self.status_rx.try_recv() {
            self.status = Some(status);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.drain_status();

        egui::TopBottomPanel::top("title_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("corpus-deck").strong());
                ui.separator();
                ui.weak("the operator's window into the research team");
            });
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                match &self.status {
                    Some(status) => {
                        let (color, text) = if status.ready {
                            (egui::Color32::from_rgb(120, 200, 120), "● ready")
                        } else {
                            (egui::Color32::from_rgb(255, 140, 100), "● not ready")
                        };
                        ui.colored_label(color, text);
                        if !status.notes.is_empty() {
                            ui.weak(&status.notes);
                        }
                        ui.separator();
                        if status.targets.is_empty() {
                            ui.weak("no targets");
                        } else {
                            ui.monospace(status.targets.join("  "));
                        }
                        ui.separator();
                        ui.weak(format!("{} plugins", status.plugins));
                    }
                    None => {
                        ui.spinner();
                        ui.weak("probing environment…");
                    }
                }
            });
            ui.add_space(2.0);
        });

        egui::SidePanel::left("nav")
            .resizable(false)
            .default_width(140.0)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                for view in View::ALL {
                    let selected = self.view == view;
                    let button = egui::Button::new(egui::RichText::new(view.label()).size(17.0))
                        .selected(selected)
                        .min_size(egui::vec2(ui.available_width() - 8.0, 34.0));
                    if ui.add(button).clicked() {
                        self.view = view;
                    }
                    ui.add_space(4.0);
                }
            });

        // The Corpus view's tree is a real panel, not a hand-rolled
        // split: it fills the window height by construction.
        if self.view == View::Corpus {
            egui::SidePanel::left("corpus_tree")
                .resizable(true)
                .default_width(320.0)
                .width_range(220.0..=520.0)
                .show(ctx, |ui| {
                    self.corpus.show_tree(ui, &self.store);
                });
        }

        // Missions: controls left, transcript in the central panel.
        if self.view == View::Missions {
            egui::SidePanel::left("missions_controls")
                .resizable(true)
                .default_width(300.0)
                .width_range(240.0..=460.0)
                .show(ctx, |ui| {
                    self.missions.controls(ui, &self.store);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.view {
            View::Corpus => {
                self.corpus.show_preview(ui, &self.store);
            }
            View::Arena => stub(ui, "Arena", "sandbox topology + agent chips (M3)"),
            View::Missions => {
                self.missions.transcript(ui);
            }
            View::Prompts => stub(ui, "Prompts", "mission prompt library (M4)"),
        });

        // Dashboard cadence: the store poll and probe re-arm need frames
        // even when nobody is clicking.
        ctx.request_repaint_after(REPAINT_INTERVAL);
    }
}

/// Dark theme + readable type.
fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(24, 25, 30);
    visuals.window_fill = egui::Color32::from_rgb(24, 25, 30);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(16.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::proportional(24.0),
    );
    ctx.set_style(style);
}

/// Placeholder for not-yet-implemented views.
fn stub(ui: &mut egui::Ui, title: &str, hint: &str) {
    ui.add_space(24.0);
    ui.heading(title);
    ui.add_space(8.0);
    ui.weak(hint);
}

/// Discover plugins and probe the first one (background thread).
fn probe_environment() -> EnvStatus {
    use corpus_core::{discover, plugins_dir, Plugin};
    let dir = plugins_dir();
    let plugins = discover(&dir).unwrap_or_default();
    let mut report = EnvStatus {
        ready: false,
        notes: String::new(),
        targets: Vec::new(),
        plugins: plugins.len(),
    };
    let Some(first) = plugins.first() else {
        report.notes = format!("no plugins in {}", dir.display());
        return report;
    };
    match Plugin::spawn(&first.dir) {
        Ok(mut plugin) => {
            match plugin.probe() {
                Ok(result) => {
                    report.ready = result.ready;
                    report.notes = result.notes;
                }
                Err(error) => report.notes = format!("probe failed: {error}"),
            }
            if let Ok(targets) = plugin.targets() {
                report.targets = targets;
            }
        }
        Err(error) => {
            report.notes = format!("spawn failed: {error}");
        }
    }
    report
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("corpus-deck"),
        ..Default::default()
    };
    eframe::run_native(
        "corpus-deck",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
