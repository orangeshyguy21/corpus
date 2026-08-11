//! The corpus TUI dashboard (ratatui).
//!
//! Drive environments directly: probe, up/down, load targets, and run
//! oracles with live verdicts. All plugin actions run on worker threads
//! (results stream back over a channel) so a slow `up` — e.g. a pinned
//! Nutshell image build — never freezes the interface.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

use corpus_core::{discover, plugins_dir, ModelRegistry, OracleInfo, Plugin, PluginDir};

/// Which panel receives navigation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Plugins,
    Oracles,
}

/// What a worker thread did.
#[derive(Debug)]
enum ActionKind {
    Probe,
    Up,
    Down,
    LoadOracles,
    RunOracle,
    LoadTargets,
}

/// A finished worker action, sent back to the UI thread.
#[derive(Debug)]
struct ActionResult {
    plugin_index: usize,
    oracle_index: Option<usize>,
    line: String,
    plugin_status: Option<String>,
    oracles: Option<Vec<(String, String)>>,
    verdict: Option<String>,
}

/// Dashboard state.
#[derive(Debug)]
struct App {
    plugins: Vec<PluginDir>,
    plugin_state: ListState,
    probe_status: Vec<Option<String>>,
    oracles: Vec<OracleInfo>,
    oracle_state: ListState,
    oracle_verdicts: Vec<Option<String>>,
    models: Vec<String>,
    log: Vec<String>,
    focus: Focus,
    jobs_running: usize,
    tx: Sender<ActionResult>,
    rx: Receiver<ActionResult>,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            plugins: Vec::new(),
            plugin_state: ListState::default(),
            probe_status: Vec::new(),
            oracles: Vec::new(),
            oracle_state: ListState::default(),
            oracle_verdicts: Vec::new(),
            models: Vec::new(),
            log: Vec::new(),
            focus: Focus::Plugins,
            jobs_running: 0,
            tx,
            rx,
        }
    }

    fn load(&mut self) {
        let dir = plugins_dir();
        match discover(&dir) {
            Ok(found) => {
                self.probe_status = vec![None; found.len()];
                self.plugins = found;
                if !self.plugins.is_empty() {
                    self.plugin_state.select(Some(0));
                }
                self.log(format!(
                    "{} plugin(s) in {}",
                    self.plugins.len(),
                    dir.display()
                ));
            }
            Err(error) => self.log(format!("plugin discovery failed: {error}")),
        }
        let models_path =
            std::env::var("CORPUS_MODELS").unwrap_or_else(|_| "benchmarks/models.yaml".to_string());
        match ModelRegistry::load(models_path.as_ref()) {
            Ok(registry) => {
                self.models = registry
                    .models
                    .iter()
                    .map(|m| {
                        format!(
                            "{} ({}, {})",
                            m.tag,
                            m.params_b
                                .map(|p| format!("{p}B"))
                                .unwrap_or_else(|| "?".to_string()),
                            m.capabilities.join(",")
                        )
                    })
                    .collect();
            }
            Err(error) => self.log(format!("model registry failed: {error}")),
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
    }

    /// Spawn a worker thread performing one plugin action.
    fn dispatch(&mut self, kind: ActionKind) {
        let Some(plugin_index) = self.plugin_state.selected() else {
            return;
        };
        let Some((dir, name)) = self
            .plugins
            .get(plugin_index)
            .map(|p| (p.dir.clone(), p.manifest.name.clone()))
        else {
            return;
        };
        let oracle_index = match kind {
            ActionKind::RunOracle => self.oracle_state.selected(),
            _ => None,
        };
        let oracle_name = oracle_index.and_then(|i| self.oracles.get(i).map(|o| o.name.clone()));
        if matches!(kind, ActionKind::RunOracle) && oracle_name.is_none() {
            self.log("no oracle selected (load oracles first: l)".to_string());
            return;
        }

        self.jobs_running += 1;
        self.log(format!("{name}: {kind:?} started..."));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = run_action(&dir, &name, &kind, plugin_index, oracle_index, oracle_name);
            // If the UI is gone, there is nothing to report to.
            let _ = tx.send(result);
        });
    }

    /// Drain finished worker actions.
    fn drain_results(&mut self) {
        while let Ok(result) = self.rx.try_recv() {
            self.jobs_running = self.jobs_running.saturating_sub(1);
            self.apply_result(result);
        }
    }

    fn apply_result(&mut self, result: ActionResult) {
        self.log(result.line);
        if let (Some(index), Some(status)) = (Some(result.plugin_index), result.plugin_status) {
            if let Some(slot) = self.probe_status.get_mut(index) {
                *slot = Some(status);
            }
        }
        if let Some(oracles) = result.oracles {
            self.oracles = oracles
                .into_iter()
                .map(|(name, description)| OracleInfo { name, description })
                .collect();
            self.oracle_verdicts = vec![None; self.oracles.len()];
            if !self.oracles.is_empty() {
                self.oracle_state.select(Some(0));
            }
        }
        if let (Some(oracle_index), Some(verdict)) = (result.oracle_index, result.verdict) {
            if let Some(slot) = self.oracle_verdicts.get_mut(oracle_index) {
                *slot = Some(verdict);
            }
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Plugins => Focus::Oracles,
            Focus::Oracles => Focus::Plugins,
        };
    }

    fn select_next(&mut self) {
        match self.focus {
            Focus::Plugins => {
                if !self.plugins.is_empty() {
                    let index = self
                        .plugin_state
                        .selected()
                        .map(|i| (i + 1) % self.plugins.len())
                        .unwrap_or(0);
                    self.plugin_state.select(Some(index));
                    // Plugin switch invalidates the oracle list.
                    self.oracles.clear();
                    self.oracle_verdicts.clear();
                }
            }
            Focus::Oracles => {
                if !self.oracles.is_empty() {
                    let index = self
                        .oracle_state
                        .selected()
                        .map(|i| (i + 1) % self.oracles.len())
                        .unwrap_or(0);
                    self.oracle_state.select(Some(index));
                }
            }
        }
    }

    fn select_prev(&mut self) {
        match self.focus {
            Focus::Plugins => {
                if !self.plugins.is_empty() {
                    let index = self
                        .plugin_state
                        .selected()
                        .map(|i| i.checked_sub(1).unwrap_or(self.plugins.len() - 1))
                        .unwrap_or(0);
                    self.plugin_state.select(Some(index));
                    self.oracles.clear();
                    self.oracle_verdicts.clear();
                }
            }
            Focus::Oracles => {
                if !self.oracles.is_empty() {
                    let index = self
                        .oracle_state
                        .selected()
                        .map(|i| i.checked_sub(1).unwrap_or(self.oracles.len() - 1))
                        .unwrap_or(0);
                    self.oracle_state.select(Some(index));
                }
            }
        }
    }
}

/// Execute one plugin action on a worker thread.
fn run_action(
    dir: &std::path::Path,
    name: &str,
    kind: &ActionKind,
    plugin_index: usize,
    oracle_index: Option<usize>,
    oracle_name: Option<String>,
) -> ActionResult {
    let failure = |message: String| ActionResult {
        plugin_index,
        oracle_index,
        line: format!("{name}: {message}"),
        plugin_status: None,
        oracles: None,
        verdict: None,
    };

    let mut plugin = match Plugin::spawn(dir) {
        Ok(plugin) => plugin,
        Err(error) => return failure(format!("spawn failed: {error}")),
    };

    match kind {
        ActionKind::Probe => match plugin.probe() {
            Ok(result) => ActionResult {
                plugin_index,
                oracle_index,
                line: format!(
                    "{name}: {} — {}",
                    if result.ready { "ready" } else { "not ready" },
                    result.notes
                ),
                plugin_status: Some(if result.ready {
                    "ready".to_string()
                } else {
                    "not ready".to_string()
                }),
                oracles: None,
                verdict: None,
            },
            Err(error) => failure(format!("probe failed: {error}")),
        },
        ActionKind::Up => match plugin.up() {
            Ok(()) => ActionResult {
                plugin_index,
                oracle_index,
                line: format!("{name}: environment up"),
                plugin_status: Some("up".to_string()),
                oracles: None,
                verdict: None,
            },
            Err(error) => failure(format!("up failed: {error}")),
        },
        ActionKind::Down => match plugin.down() {
            Ok(()) => ActionResult {
                plugin_index,
                oracle_index,
                line: format!("{name}: environment down"),
                plugin_status: Some("down".to_string()),
                oracles: None,
                verdict: None,
            },
            Err(error) => failure(format!("down failed: {error}")),
        },
        ActionKind::LoadOracles => match plugin.oracles() {
            Ok(list) => {
                let names: Vec<(String, String)> =
                    list.into_iter().map(|o| (o.name, o.description)).collect();
                ActionResult {
                    plugin_index,
                    oracle_index,
                    line: format!("{name}: {} oracle(s)", names.len()),
                    plugin_status: None,
                    oracles: Some(names),
                    verdict: None,
                }
            }
            Err(error) => failure(format!("oracles failed: {error}")),
        },
        ActionKind::RunOracle => {
            let oracle = oracle_name.unwrap_or_default();
            match plugin.call_oracle(&oracle) {
                Ok(result) => ActionResult {
                    plugin_index,
                    oracle_index,
                    line: format!("{name}: {oracle} -> {}", result.verdict),
                    plugin_status: None,
                    oracles: None,
                    verdict: Some(result.verdict),
                },
                Err(error) => failure(format!("oracle {oracle} failed: {error}")),
            }
        }
        ActionKind::LoadTargets => match plugin.targets() {
            Ok(targets) => ActionResult {
                plugin_index,
                oracle_index,
                line: format!("{name}: targets: {}", targets.join(", ")),
                plugin_status: None,
                oracles: None,
                verdict: None,
            },
            Err(error) => failure(format!("targets failed: {error}")),
        },
    }
}

/// Render one frame.
fn draw(frame: &mut Frame, app: &mut App) {
    let columns = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(frame.area());

    let plugin_items: Vec<ListItem> = app
        .plugins
        .iter()
        .enumerate()
        .map(|(index, plugin)| {
            let status = app
                .probe_status
                .get(index)
                .and_then(|s| s.as_deref())
                .unwrap_or("unprobed");
            ListItem::new(format!("{} [{}]", plugin.manifest.name, status))
        })
        .collect();
    let plugins = List::new(plugin_items)
        .block(panel_block("plugins", app.focus == Focus::Plugins))
        .highlight_symbol("> ");
    frame.render_stateful_widget(plugins, columns[0], &mut app.plugin_state);

    let right = Layout::vertical([
        Constraint::Percentage(30),
        Constraint::Percentage(35),
        Constraint::Percentage(35),
    ])
    .split(columns[1]);

    let models = List::new(
        app.models
            .iter()
            .map(|m| ListItem::new(m.as_str()))
            .collect::<Vec<_>>(),
    )
    .block(Block::default().title("models").borders(Borders::ALL));
    frame.render_widget(models, right[0]);

    let oracle_items: Vec<ListItem> = app
        .oracles
        .iter()
        .enumerate()
        .map(|(index, oracle)| {
            let verdict = app
                .oracle_verdicts
                .get(index)
                .and_then(|v| v.as_deref())
                .unwrap_or("-");
            ListItem::new(format!("{} [{}]", oracle.name, verdict))
        })
        .collect();
    let oracles = List::new(oracle_items)
        .block(panel_block("oracles", app.focus == Focus::Oracles))
        .highlight_symbol("> ");
    frame.render_stateful_widget(oracles, right[1], &mut app.oracle_state);

    let title = if app.jobs_running > 0 {
        format!(
            "activity ({} job(s) running — tab: panel, p: probe, u: up, d: down, l: load oracles, o: run oracle, t: targets, q: quit)",
            app.jobs_running
        )
    } else {
        "activity (tab: panel, p: probe, u: up, d: down, l: load oracles, o: run oracle, t: targets, q: quit)".to_string()
    };
    let log_text = app
        .log
        .iter()
        .rev()
        .take(20)
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let log = Paragraph::new(log_text).block(Block::default().title(title).borders(Borders::ALL));
    frame.render_widget(log, right[2]);
}

/// Panel border; highlighted when focused.
fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Block::default()
        .title(format!("{title}{}", if focused { " *" } else { "" }))
        .borders(Borders::ALL)
        .border_style(style)
}

/// Run the dashboard until quit.
pub fn run() -> Result<(), String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let mut app = App::new();
    app.load();

    let result = loop {
        app.drain_results();
        if let Err(error) = terminal.draw(|frame| draw(frame, &mut app)) {
            break Err(error.to_string());
        }
        if event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Tab => app.toggle_focus(),
                    KeyCode::Char('p') => app.dispatch(ActionKind::Probe),
                    KeyCode::Char('u') => app.dispatch(ActionKind::Up),
                    KeyCode::Char('d') => app.dispatch(ActionKind::Down),
                    KeyCode::Char('l') => app.dispatch(ActionKind::LoadOracles),
                    KeyCode::Char('o') => app.dispatch(ActionKind::RunOracle),
                    KeyCode::Char('t') => app.dispatch(ActionKind::LoadTargets),
                    KeyCode::Char('r') => app.load(),
                    KeyCode::Char('j') | KeyCode::Down => app.select_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|e| e.to_string())?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    /// The dashboard renders plugins, oracles with verdicts, and models.
    #[test]
    fn renders_panels() {
        let mut app = App::new();
        let manifest: corpus_core::PluginManifest =
            toml::from_str("name = \"example-plugin\"\nexec = \"x.sh\"\n").expect("manifest");
        app.plugins.push(PluginDir {
            dir: std::path::PathBuf::from("plugins/example"),
            manifest,
        });
        app.probe_status = vec![Some("ready".to_string())];
        app.plugin_state.select(Some(0));
        app.oracles.push(OracleInfo {
            name: "002-double-spend-rejected".to_string(),
            description: String::new(),
        });
        app.oracle_verdicts = vec![Some("hold".to_string())];
        app.oracle_state.select(Some(0));
        app.models
            .push("qwen3.6:35b (35B, coding,tool-use)".to_string());

        // Simulate a finished probe result landing in state.
        app.apply_result(ActionResult {
            plugin_index: 0,
            oracle_index: None,
            line: "example-plugin: ready — mint up".to_string(),
            plugin_status: Some("ready".to_string()),
            oracles: None,
            verdict: None,
        });

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("example-plugin [ready]"), "plugin: {text}");
        assert!(
            text.contains("002-double-spend-rejected [hold]"),
            "oracle: {text}"
        );
        assert!(text.contains("qwen3.6:35b"), "model: {text}");
        assert!(text.contains("mint up"), "log: {text}");
    }
}
