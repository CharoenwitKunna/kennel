// GUI app: never open a console window (dev builds included).
#![windows_subsystem = "windows"]

mod agent;
mod runner;
mod term;
mod term_view;
mod theme;
#[cfg(test)]
mod live_tests;

use agent::{Agent, Permission, Provider, State, Usage};
use eframe::egui::{self, Align, Color32, CornerRadius, Key, Layout, Margin, RichText, Sense, Stroke, Vec2};
use runner::ClaudeTracker;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use term::TermSession;
use theme::*;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--statusline") {
        runner::statusline_main(args.get(2).map(String::as_str).unwrap_or(""));
        return Ok(());
    }
    term::scrub_parent_session_env();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kennel")
            .with_inner_size([1040.0, 620.0])
            .with_min_inner_size([720.0, 440.0]),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native("Kennel", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

// ---------- runtime state ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Focus,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Stopped,
    Starting,
    Working,
    Waiting,
    Exited(i32),
}

impl Status {
    fn label(self) -> String {
        match self {
            Status::Stopped => "Stopped".into(),
            Status::Starting => "Starting".into(),
            Status::Working => "Working".into(),
            Status::Waiting => "Ready".into(),
            Status::Exited(0) => "Exited".into(),
            Status::Exited(c) => format!("Exited ({c})"),
        }
    }
    fn color(self) -> Color32 {
        match self {
            Status::Stopped => GREY,
            Status::Starting => ACCENT,
            Status::Working => GREEN,
            Status::Waiting => AMBER,
            Status::Exited(0) => GREY,
            Status::Exited(_) => RED,
        }
    }
    fn alive(self) -> bool {
        matches!(self, Status::Starting | Status::Working | Status::Waiting)
    }
}

/// Output quieter than this counts as "ready for you".
const QUIET: Duration = Duration::from_millis(1500);

struct Live {
    sess: TermSession,
    was_working: bool,
    /// The user has typed into or broadcast to this agent (gates "needs you").
    interacted: bool,
    claude: Option<ClaudeTracker>,
    last_poll: Instant,
    poll_inflight: bool,
    last_export: Option<Instant>,
    final_polled: bool,
}

enum Bg {
    OpenCodeUsage { agent_id: u64, session_id: String, usage: Option<Usage> },
}

#[derive(Clone)]
struct Draft {
    name: String,
    provider: Provider,
    model: String,
    cwd: String,
    system_prompt: String,
    permission: Permission,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            name: String::new(),
            provider: Provider::Claude,
            model: String::new(),
            cwd: std::env::var("USERPROFILE").unwrap_or_default(),
            system_prompt: String::new(),
            permission: Permission::Safe,
        }
    }
}

enum Action {
    Select(u64),
    Start(u64),
    Stop(u64),
    Restart(u64),
    Edit(u64),
    NewSession(u64),
    Delete(u64),
    OpenFocus(u64),
    NewAgent,
    /// Create an agent for this app and open its terminal right away.
    Create(Provider),
}

struct App {
    state: State,
    live: HashMap<u64, Live>,
    selected: Option<u64>,
    view: View,
    attention: HashSet<u64>,
    editor: Option<(Option<u64>, Draft)>,
    show_settings: bool,
    confirm_delete: Option<u64>,
    broadcast: String,
    broadcast_off: HashSet<u64>,
    bg_tx: Sender<Bg>,
    bg_rx: Receiver<Bg>,
    dirty: bool,
    last_save: Instant,
    self_exe: PathBuf,
    focus_term: Option<u64>,
    /// Agents flagged `resume_on_launch` have been started.
    autostarted: bool,
    /// Open the "New agent" dropdown on the next frame.
    open_new_menu: bool,
    /// Folder shown in the "New agent" dropdown.
    new_folder: String,
    toasts: Vec<(Instant, String)>,
    actions: Vec<Action>,
}

fn term_id(agent_id: u64) -> egui::Id {
    egui::Id::new(("term", agent_id))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);
        let state = State::load();
        let (bg_tx, bg_rx) = channel();
        let selected = state.agents.first().map(|a| a.id);
        let new_folder = state.settings.last_folder.clone();
        App {
            state,
            live: HashMap::new(),
            selected,
            view: View::Focus,
            attention: HashSet::new(),
            editor: None,
            show_settings: false,
            confirm_delete: None,
            broadcast: String::new(),
            broadcast_off: HashSet::new(),
            bg_tx,
            bg_rx,
            dirty: false,
            last_save: Instant::now(),
            self_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kennel.exe")),
            focus_term: None,
            autostarted: false,
            open_new_menu: false,
            new_folder,
            toasts: Vec::new(),
            actions: Vec::new(),
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toasts.push((Instant::now(), msg.into()));
    }

    fn status(&self, id: u64) -> Status {
        let Some(l) = self.live.get(&id) else { return Status::Stopped };
        if let Some(code) = l.sess.exited() {
            return Status::Exited(code);
        }
        match l.sess.quiet_for() {
            None => Status::Starting,
            Some(d) if d < QUIET => Status::Working,
            Some(_) => Status::Waiting,
        }
    }

    // ---------- lifecycle ----------

    fn start(&mut self, id: u64, ctx: &egui::Context) {
        if self.status(id).alive() {
            return;
        }
        self.live.remove(&id);
        let Some(agent) = self.state.agent(id).cloned() else { return };
        let launch = match runner::launch(&agent, &self.state.settings, &self.self_exe) {
            Ok(l) => l,
            Err(e) => return self.toast(format!("{}: {e}", agent.name)),
        };
        let sess = match TermSession::spawn(launch.spec, 120, 32, ctx.clone()) {
            Ok(s) => s,
            Err(e) => return self.toast(format!("Could not start {}: {e}", agent.name)),
        };
        let a = self.state.agent_mut(id).unwrap();
        a.session_id = Some(launch.session_id.clone());
        a.runs += 1;
        let claude =
            (a.provider == Provider::Claude).then(|| ClaudeTracker::new(id, &launch.session_id));
        self.live.insert(
            id,
            Live {
                sess,
                was_working: false,
                interacted: false,
                claude,
                last_poll: Instant::now(),
                poll_inflight: false,
                last_export: None,
                final_polled: false,
            },
        );
        self.focus_term = Some(id);
        self.dirty = true;
    }

    /// "New agent" picker: create an agent for `p` in the chosen folder and
    /// open its terminal immediately.
    fn create_agent(&mut self, p: Provider, ctx: &egui::Context) {
        let folder = match self.new_folder.trim() {
            "" => std::env::var("USERPROFILE").unwrap_or_default(),
            f => f.to_string(),
        };
        if !std::path::Path::new(&folder).is_dir() {
            return self.toast(format!("Folder not found: {folder}"));
        }
        let mut n = self.state.agents.iter().filter(|a| a.provider == p).count() + 1;
        while self.state.agents.iter().any(|a| a.name == format!("{} {n}", p.label())) {
            n += 1;
        }
        let id = self.state.alloc_id();
        let mut agent = Agent::new(id, format!("{} {n}", p.label()), p);
        agent.cwd = folder.clone();
        agent.model = p.default_model().into();
        self.state.agents.push(agent);
        self.state.settings.last_folder = folder;
        self.selected = Some(id);
        self.dirty = true;
        self.start(id, ctx);
    }

    fn stop(&mut self, id: u64) {
        self.poll_usage(id, true);
        if let Some(l) = self.live.remove(&id) {
            l.sess.kill();
        }
        self.attention.remove(&id);
    }

    /// Refresh an agent's session totals from its CLI's files.
    fn poll_usage(&mut self, id: u64, force: bool) {
        let Some(agent) = self.state.agent(id) else { return };
        let provider = agent.provider;
        let sid = agent.session_id.clone();
        let cwd = Some(PathBuf::from(agent.cwd.trim())).filter(|p| p.is_dir());
        let Some(l) = self.live.get_mut(&id) else { return };
        match provider {
            Provider::Claude => {
                if !force && l.last_poll.elapsed() < Duration::from_secs(2) {
                    return;
                }
                l.last_poll = Instant::now();
                if let Some(t) = l.claude.as_mut() {
                    let u = t.poll();
                    let a = self.state.agent_mut(id).unwrap();
                    if a.session_usage != u {
                        a.session_usage = u;
                        self.dirty = true;
                    }
                }
            }
            Provider::OpenCode => {
                let due = force || l.last_export.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
                let (Some(sid), false, true) = (sid, l.poll_inflight, due) else { return };
                let Some(exe) = runner::exe_for(provider, &self.state.settings) else { return };
                l.poll_inflight = true;
                l.last_export = Some(Instant::now());
                let tx = self.bg_tx.clone();
                std::thread::spawn(move || {
                    let usage = runner::opencode_session_usage(&exe, &sid, cwd.as_deref());
                    let _ = tx.send(Bg::OpenCodeUsage { agent_id: id, session_id: sid, usage });
                });
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                Bg::OpenCodeUsage { agent_id, session_id, usage } => {
                    if let Some(l) = self.live.get_mut(&agent_id) {
                        l.poll_inflight = false;
                    }
                    if let (Some(a), Some(u)) = (self.state.agent_mut(agent_id), usage) {
                        if a.session_id.as_deref() == Some(&session_id) && a.session_usage != u {
                            a.session_usage = u;
                            self.dirty = true;
                        }
                    }
                }
            }
        }

        let focused_term = ctx.memory(|m| m.focused());
        let ids: Vec<u64> = self.live.keys().copied().collect();
        for id in ids {
            let status = self.status(id);
            let focused = focused_term == Some(term_id(id));
            let l = self.live.get_mut(&id).unwrap();
            let working = status == Status::Working;
            let finished_turn = l.was_working && status == Status::Waiting;
            l.was_working = working;
            let bell = l.sess.take_bell();
            let exited = matches!(status, Status::Exited(_)) && !l.final_polled;
            if exited {
                l.final_polled = true;
            }
            if ((finished_turn && l.interacted) || bell || exited) && !focused {
                self.attention.insert(id);
            }
            if focused {
                self.attention.remove(&id);
            }
            if finished_turn || exited {
                self.poll_usage(id, true);
            } else if working {
                self.poll_usage(id, false);
            }
        }
    }

    fn apply_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.actions) {
            match action {
                Action::Select(id) => {
                    self.selected = Some(id);
                    self.attention.remove(&id);
                    self.focus_term = Some(id);
                }
                Action::OpenFocus(id) => {
                    self.selected = Some(id);
                    self.view = View::Focus;
                    self.focus_term = Some(id);
                }
                Action::Start(id) => {
                    self.selected = Some(id);
                    self.start(id, ctx);
                }
                Action::Stop(id) => self.stop(id),
                Action::Restart(id) => {
                    self.stop(id);
                    self.start(id, ctx);
                }
                Action::Edit(id) => {
                    if let Some(a) = self.state.agent(id) {
                        let d = Draft {
                            name: a.name.clone(),
                            provider: a.provider,
                            model: a.model.clone(),
                            cwd: a.cwd.clone(),
                            system_prompt: a.system_prompt.clone(),
                            permission: a.permission,
                        };
                        self.editor = Some((Some(id), d));
                    }
                }
                Action::NewSession(id) => {
                    let was_running = self.status(id).alive();
                    self.stop(id);
                    if let Some(a) = self.state.agent_mut(id) {
                        a.reset_session();
                    }
                    self.dirty = true;
                    if was_running {
                        self.start(id, ctx);
                    }
                }
                Action::Delete(id) => self.confirm_delete = Some(id),
                Action::NewAgent => self.open_new_menu = true,
                Action::Create(p) => self.create_agent(p, ctx),
            }
        }
    }

    fn maybe_save(&mut self, force: bool) {
        if self.dirty && (force || self.last_save.elapsed() > Duration::from_secs(2)) {
            if let Err(e) = self.state.save() {
                eprintln!("save failed: {e}");
            }
            self.dirty = false;
            self.last_save = Instant::now();
        }
    }

    // ---------- panels ----------

    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(52.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    logo(ui);
                    ui.label(RichText::new("Kennel").font(bold(16.0)));
                    ui.add_space(18.0);
                    segmented(ui, &mut self.view);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let resp = ui.add(primary_button("+  New agent")).on_hover_text("Ctrl+Shift+N");
                        let popup_id = egui::Id::new("new_agent_menu");
                        if resp.clicked() || std::mem::take(&mut self.open_new_menu) {
                            ui.memory_mut(|m| m.toggle_popup(popup_id));
                        }
                        egui::popup_below_widget(
                            ui,
                            popup_id,
                            &resp,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                if let Some(p) = new_agent_menu(ui, &mut self.new_folder) {
                                    self.actions.push(Action::Create(p));
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            },
                        );
                        if ui.add(ghost_button("⚙")).on_hover_text("Settings").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        let running = self.state.agents.iter().filter(|a| self.status(a.id).alive()).count();
                        let stopped: Vec<u64> =
                            self.state.agents.iter().map(|a| a.id).filter(|id| !self.status(*id).alive()).collect();
                        if !stopped.is_empty()
                            && ui
                                .add(ghost_button(&format!("▶  Start all ({})", stopped.len())))
                                .on_hover_text("Start every stopped agent")
                                .clicked()
                        {
                            for id in stopped {
                                self.actions.push(Action::Start(id));
                            }
                            if running + 1 > 1 {
                                self.view = View::Grid;
                            }
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(272.0)
            .width_range(220.0..=420.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::same(12)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("AGENTS").small().color(MUTED).strong());
                    ui.label(RichText::new(self.state.agents.len().to_string()).small().color(FAINT));
                });
                ui.add_space(4.0);

                // Totals card pinned at the bottom.
                egui::TopBottomPanel::bottom("totals").frame(egui::Frame::new().inner_margin(Margin::symmetric(0, 8))).show_inside(
                    ui,
                    |ui| {
                        let u = self.state.total_usage();
                        egui::Frame::new().fill(RAISED).corner_radius(10).inner_margin(Margin::same(12)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("TOTAL SPEND").small().color(MUTED).strong());
                            ui.label(RichText::new(format!("${:.4}", u.cost_usd)).font(bold(22.0)));
                            ui.label(
                                RichText::new(format!(
                                    "in {} · out {} · cache {}",
                                    fmt_tokens(u.input),
                                    fmt_tokens(u.output),
                                    fmt_tokens(u.cache_read + u.cache_write)
                                ))
                                .small()
                                .color(MUTED),
                            );
                        });
                    },
                );

                if self.state.agents.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No agents yet").color(MUTED));
                        ui.add_space(6.0);
                        if ui.add(primary_button("Create an agent")).clicked() {
                            self.actions.push(Action::NewAgent);
                        }
                    });
                    return;
                }

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for (i, a) in self.state.agents.iter().enumerate() {
                        let status = self.status(a.id);
                        let selected = self.selected == Some(a.id);
                        let attention = self.attention.contains(&a.id);
                        let resp = agent_card(ui, a, status, selected, attention, i);
                        if resp.clicked() {
                            self.actions.push(Action::Select(a.id));
                        }
                        if resp.double_clicked() && !status.alive() {
                            self.actions.push(Action::Start(a.id));
                        }
                        resp.context_menu(|ui| {
                            ui.set_min_width(170.0);
                            if status.alive() {
                                if ui.button("Restart").clicked() {
                                    self.actions.push(Action::Restart(a.id));
                                    ui.close_menu();
                                }
                                if ui.button("Stop").clicked() {
                                    self.actions.push(Action::Stop(a.id));
                                    ui.close_menu();
                                }
                            } else if ui.button("Start").clicked() {
                                self.actions.push(Action::Start(a.id));
                                ui.close_menu();
                            }
                            if ui.button("Edit…").clicked() {
                                self.actions.push(Action::Edit(a.id));
                                ui.close_menu();
                            }
                            if ui.button("New session").clicked() {
                                self.actions.push(Action::NewSession(a.id));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(RichText::new("Delete…").color(RED)).clicked() {
                                self.actions.push(Action::Delete(a.id));
                                ui.close_menu();
                            }
                        });
                        ui.add_space(6.0);
                    }
                });
            });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::same(12)))
            .show(ctx, |ui| {
                if self.state.agents.is_empty() {
                    empty_state(ui, &mut self.actions, &mut self.new_folder);
                    return;
                }
                if self.live.values().any(|l| l.sess.exited().is_none()) {
                    self.broadcast_bar(ui);
                }
                match self.view {
                    View::Focus => self.focus_view(ui),
                    View::Grid => self.grid_view(ui),
                }
            });
    }

    fn broadcast_bar(&mut self, ui: &mut egui::Ui) {
        egui::TopBottomPanel::bottom("broadcast")
            .frame(egui::Frame::new().inner_margin(Margin { left: 0, right: 0, top: 10, bottom: 0 }))
            .show_inside(ui, |ui| {
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        let targets: Vec<(u64, String, Color32)> = self
                            .state
                            .agents
                            .iter()
                            .filter(|a| self.live.get(&a.id).is_some_and(|l| l.sess.exited().is_none()))
                            .map(|a| (a.id, a.name.clone(), a.provider.color()))
                            .collect();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("📣").color(MUTED));
                            for (id, name, color) in &targets {
                                let on = !self.broadcast_off.contains(id);
                                if chip(ui, name, *color, on).clicked() {
                                    if on {
                                        self.broadcast_off.insert(*id);
                                    } else {
                                        self.broadcast_off.remove(id);
                                    }
                                }
                            }
                            let n = targets.iter().filter(|(id, _, _)| !self.broadcast_off.contains(id)).count();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let can = n > 0 && !self.broadcast.trim().is_empty();
                                let send = ui.add_enabled(can, primary_button(&format!("Send to {n}"))).clicked();
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut self.broadcast)
                                        .hint_text("Broadcast a message to the selected running agents…")
                                        .desired_width(f32::INFINITY)
                                        .margin(Margin::symmetric(8, 5)),
                                );
                                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                                if (send || enter) && can {
                                    let text = std::mem::take(&mut self.broadcast);
                                    for (id, _, _) in &targets {
                                        if !self.broadcast_off.contains(id) {
                                            if let Some(l) = self.live.get_mut(id) {
                                                l.sess.submit(text.trim());
                                                l.interacted = true;
                                            }
                                        }
                                    }
                                }
                            });
                        });
                    });
            });
    }

    fn focus_view(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected.filter(|id| self.state.agent(*id).is_some()) else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Select an agent").color(MUTED)));
            return;
        };
        let status = self.status(id);
        let a = self.state.agent(id).unwrap().clone();

        // Header card
        egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(12).inner_margin(Margin::same(14)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    app_icon(ui, a.provider, 26.0);
                    ui.label(RichText::new(&a.name).font(bold(19.0)));
                    status_pill(ui, status);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button(RichText::new("⋯").size(16.0), |ui| {
                            ui.set_min_width(170.0);
                            if ui.button("New session").clicked() {
                                self.actions.push(Action::NewSession(id));
                                ui.close_menu();
                            }
                            if ui.button("Copy session id").clicked() {
                                if let Some(s) = &a.session_id {
                                    ui.ctx().copy_text(s.clone());
                                }
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(RichText::new("Delete…").color(RED)).clicked() {
                                self.actions.push(Action::Delete(id));
                                ui.close_menu();
                            }
                        });
                        if ui.add(ghost_button("Edit")).clicked() {
                            self.actions.push(Action::Edit(id));
                        }
                        if status.alive() {
                            if ui.add(danger_button("■  Stop")).clicked() {
                                self.actions.push(Action::Stop(id));
                            }
                            if ui.add(ghost_button("↻  Restart")).clicked() {
                                self.actions.push(Action::Restart(id));
                            }
                        } else {
                            let label = if a.session_id.is_some() { "▶  Resume" } else { "▶  Start" };
                            if ui.add(primary_button(label)).clicked() {
                                self.actions.push(Action::Start(id));
                            }
                        }
                    });
                });
                ui.add_space(8.0);
                let s = a.session_usage;
                let t = a.total_usage();
                ui.horizontal_wrapped(|ui| {
                    stat(ui, "Session", &format!("${:.4}", s.cost_usd));
                    stat(ui, "In", &fmt_tokens(s.input));
                    stat(ui, "Out", &fmt_tokens(s.output));
                    stat(ui, "Cache", &fmt_tokens(s.cache_read + s.cache_write));
                    stat(ui, "All time", &format!("${:.4}", t.cost_usd));
                    stat(ui, "Runs", &a.runs.to_string());
                });
            },
        );
        ui.add_space(10.0);

        let focused = ui.memory(|m| m.has_focus(term_id(id)));
        let border = if focused { a.provider.color().gamma_multiply(0.8) } else { BORDER };
        egui::Frame::new()
            .fill(TERM_BG)
            .stroke(Stroke::new(1.0_f32, border))
            .corner_radius(10)
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.terminal_or_placeholder(ui, id, status, &a);
            });
    }

    fn grid_view(&mut self, ui: &mut egui::Ui) {
        let ids: Vec<u64> = self.state.agents.iter().map(|a| a.id).filter(|id| self.live.contains_key(id)).collect();
        if ids.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No agents running. Start some from the sidebar, or use “Start all”.").color(MUTED));
            });
            return;
        }
        let n = ids.len();
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        let gap = 10.0;
        let area = ui.available_rect_before_wrap();
        let tile_w = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let tile_h = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
        ui.allocate_rect(area, Sense::hover());

        for (i, id) in ids.into_iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let min = area.min + Vec2::new(c as f32 * (tile_w + gap), r as f32 * (tile_h + gap));
            let rect = egui::Rect::from_min_size(min, Vec2::new(tile_w, tile_h));
            let a = self.state.agent(id).unwrap().clone();
            let status = self.status(id);
            let focused = ui.memory(|m| m.has_focus(term_id(id)));
            let attention = self.attention.contains(&id);
            let border = if focused {
                a.provider.color().gamma_multiply(0.85)
            } else if attention {
                AMBER.gamma_multiply(0.7)
            } else {
                BORDER
            };
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0_f32, border))
                .corner_radius(10)
                .inner_margin(Margin::same(6))
                .show(&mut child, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        app_icon(ui, a.provider, 18.0);
                        ui.label(RichText::new(&a.name).font(bold(13.5)));
                        status_pill(ui, status);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(icon_button("⤢")).on_hover_text("Open in focus view").clicked() {
                                self.actions.push(Action::OpenFocus(id));
                            }
                            if status.alive() {
                                if ui.add(icon_button("■")).on_hover_text("Stop").clicked() {
                                    self.actions.push(Action::Stop(id));
                                }
                            } else if ui.add(icon_button("↻")).on_hover_text("Restart").clicked() {
                                self.actions.push(Action::Restart(id));
                            }
                            ui.label(
                                RichText::new(format!("${:.3}", a.session_usage.cost_usd)).small().color(MUTED),
                            );
                        });
                    });
                    egui::Frame::new().fill(TERM_BG).corner_radius(6).inner_margin(Margin::same(2)).show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.terminal_or_placeholder(ui, id, status, &a);
                    });
                });
        }
    }

    fn terminal_or_placeholder(&mut self, ui: &mut egui::Ui, id: u64, status: Status, a: &Agent) {
        let font_size = self.state.settings.font_size;
        if let Some(l) = self.live.get_mut(&id) {
            if self.focus_term == Some(id) {
                ui.memory_mut(|m| m.request_focus(term_id(id)));
                self.focus_term = None;
            }
            let out = term_view::show(ui, &mut l.sess, term_id(id), font_size);
            if out.typed {
                l.interacted = true;
            }
            if out.typed || out.response.clicked() {
                self.attention.remove(&id);
                if self.selected != Some(id) {
                    self.selected = Some(id);
                }
            }
            if let Status::Exited(code) = status {
                let r = out.response.rect;
                let banner = egui::Rect::from_min_size(
                    egui::pos2(r.center().x - 170.0, r.bottom() - 56.0),
                    Vec2::new(340.0, 44.0),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(banner));
                egui::Frame::new().fill(RAISED).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(10).inner_margin(Margin::symmetric(12, 7)).show(
                    &mut child,
                    |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.label(RichText::new(format!("Process exited (code {code})")).color(MUTED));
                            if ui.add(primary_button("↻  Restart")).clicked() {
                                self.actions.push(Action::Restart(id));
                            }
                        });
                    },
                );
            }
        } else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() / 2.0 - 70.0).max(0.0));
                    ui.label(RichText::new("◇").size(34.0).color(a.provider.color()));
                    ui.label(RichText::new(format!("{} isn't running", a.name)).font(bold(16.0)));
                    let hint = if a.session_id.is_some() {
                        "Resume picks up the previous conversation."
                    } else {
                        "Start opens a fresh interactive session in this pane."
                    };
                    ui.label(RichText::new(hint).small().color(MUTED));
                    ui.add_space(10.0);
                    let label = if a.session_id.is_some() { "▶  Resume session" } else { "▶  Start" };
                    if ui.add(primary_button(label)).clicked() {
                        self.actions.push(Action::Start(id));
                    }
                });
            });
        }
    }

    // ---------- windows ----------

    fn editor_window(&mut self, ctx: &egui::Context) {
        let Some((editing, draft)) = &mut self.editor else { return };
        let editing = *editing;
        let mut open = true;
        let (mut save, mut cancel) = (false, false);
        let running = editing.is_some_and(|id| self.live.contains_key(&id));
        egui::Window::new(if editing.is_some() { "Edit agent" } else { "New agent" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([500.0, 0.0])
            .show(ctx, |ui| {
                field_label(ui, "Name");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .hint_text("e.g. backend-refactor")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(8, 6)),
                );
                ui.add_space(8.0);
                field_label(ui, "Provider");
                ui.columns(2, |cols| {
                    for (i, p) in Provider::ALL.into_iter().enumerate() {
                        let sub = match p {
                            Provider::Claude => "Anthropic's coding agent",
                            Provider::OpenCode => "Open-source, many models",
                        };
                        if provider_card(&mut cols[i], p, sub, draft.provider == p).clicked() && draft.provider != p {
                            draft.provider = p;
                            draft.model = p.default_model().into();
                        }
                    }
                });
                ui.add_space(8.0);
                field_label(ui, "Model");
                if draft.provider == Provider::OpenCode {
                    draft.model.clear();
                    egui::Frame::new().fill(RAISED).corner_radius(8).inner_margin(Margin::symmetric(10, 7)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(
                                "OpenCode uses the model you last picked inside it. Type /models in its terminal to switch (e.g. Hy4 Flash Free).",
                            )
                            .small()
                            .color(MUTED),
                        );
                    });
                } else {
                ui.horizontal(|ui| {
                    let hint = "default";
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.model)
                            .hint_text(hint)
                            .desired_width(ui.available_width() - 40.0)
                            .margin(Margin::symmetric(8, 6)),
                    );
                    ui.menu_button("▾", |ui| {
                        ui.set_min_width(260.0);
                        if ui.button("Default").clicked() {
                            draft.model.clear();
                            ui.close_menu();
                        }
                        for m in draft.provider.model_suggestions() {
                            if ui.button(*m).clicked() {
                                draft.model = m.to_string();
                                ui.close_menu();
                            }
                        }
                    });
                });
                }
                ui.add_space(8.0);
                field_label(ui, "Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.cwd)
                        .hint_text("C:\\path\\to\\project")
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(8, 6)),
                );
                if !draft.cwd.trim().is_empty() && !std::path::Path::new(draft.cwd.trim()).is_dir() {
                    ui.label(RichText::new("Folder not found").small().color(RED));
                }
                ui.add_space(8.0);
                field_label(ui, "Permissions");
                for p in Permission::ALL {
                    let selected = draft.permission == p;
                    let resp = egui::Frame::new()
                        .fill(if selected { RAISED } else { Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0_f32, if selected { ACCENT.gamma_multiply(0.6) } else { BORDER }))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(if selected { "◉" } else { "○" }).color(if selected { ACCENT } else { MUTED }));
                                ui.vertical(|ui| {
                                    let color = if p == Permission::FullAuto { AMBER } else { TEXT };
                                    ui.label(RichText::new(p.label()).color(color));
                                    ui.label(RichText::new(p.hint()).small().color(MUTED));
                                });
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        draft.permission = p;
                    }
                    ui.add_space(2.0);
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if running {
                        ui.label(RichText::new("Changes apply on the next start.").small().color(MUTED));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ok = !draft.name.trim().is_empty();
                        let label = if editing.is_some() { "Save" } else { "Create agent" };
                        if ui.add_enabled(ok, primary_button(label)).clicked() {
                            save = true;
                        }
                        if ui.add(ghost_button("Cancel")).clicked() {
                            cancel = true;
                        }
                    });
                });
            });

        if save {
            let (editing, d) = self.editor.take().unwrap();
            let id = match editing {
                Some(id) => id,
                None => {
                    let id = self.state.alloc_id();
                    self.state.agents.push(Agent::new(id, String::new(), d.provider));
                    id
                }
            };
            if let Some(a) = self.state.agent_mut(id) {
                if a.provider != d.provider {
                    a.reset_session(); // sessions don't carry across CLIs
                }
                a.name = d.name.trim().into();
                a.provider = d.provider;
                a.model = d.model.trim().into();
                a.cwd = d.cwd.trim().into();
                a.system_prompt = d.system_prompt;
                a.permission = d.permission;
            }
            if editing.is_none() {
                self.selected = Some(id);
                self.view = View::Focus;
            }
            self.dirty = true;
        } else if cancel || !open {
            self.editor = None;
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let claude = runner::exe_for(Provider::Claude, &self.state.settings);
        let opencode = runner::exe_for(Provider::OpenCode, &self.state.settings);
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size([520.0, 0.0])
            .show(ctx, |ui| {
                field_label(ui, "Claude Code CLI");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.state.settings.claude_path)
                            .hint_text("auto-detect (claude)")
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(8, 6)),
                    )
                    .changed();
                found_label(ui, &claude);
                ui.add_space(8.0);
                field_label(ui, "OpenCode CLI");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.state.settings.opencode_path)
                            .hint_text("auto-detect (opencode)")
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(8, 6)),
                    )
                    .changed();
                found_label(ui, &opencode);
                ui.add_space(8.0);
                field_label(ui, "Terminal font size");
                self.dirty |= ui.add(egui::Slider::new(&mut self.state.settings.font_size, 10.0..=20.0).step_by(0.5)).changed();
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(agent::data_dir().display().to_string()).small().color(MUTED));
                    if ui.add(ghost_button("Open folder")).clicked() {
                        let _ = std::process::Command::new("explorer").arg(agent::data_dir()).spawn();
                    }
                });
            });
        if !open {
            self.show_settings = false;
        }
    }

    fn delete_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete else { return };
        let name = self.state.agent(id).map(|a| a.name.clone()).unwrap_or_default();
        let mut decision = None;
        egui::Window::new("Delete agent")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("Delete “{name}”? Its terminal is closed and its cost history removed."));
                ui.label(RichText::new("The conversation itself stays in the CLI's own history.").small().color(MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(danger_button("Delete")).clicked() {
                        decision = Some(true);
                    }
                    if ui.add(ghost_button("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.stop(id);
                self.state.agents.retain(|a| a.id != id);
                self.selected = self.state.agents.first().map(|a| a.id);
                self.confirm_delete = None;
                self.dirty = true;
            }
            Some(false) => self.confirm_delete = None,
            None => {}
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|(t, _)| t.elapsed() < Duration::from_secs(7));
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -40.0))
            .show(ctx, |ui| {
                for (_, msg) in &self.toasts {
                    egui::Frame::new()
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0_f32, RED.gamma_multiply(0.6)))
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.label(RichText::new(msg).color(TEXT));
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(cs, Key::G) {
                self.view = if self.view == View::Focus { View::Grid } else { View::Focus };
            }
            if i.consume_key(cs, Key::N) {
                self.actions.push(Action::NewAgent);
            }
            let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            for (n, k) in digits.into_iter().enumerate() {
                if i.consume_key(cs, k) {
                    if let Some(a) = self.state.agents.get(n) {
                        self.actions.push(Action::OpenFocus(a.id));
                    }
                }
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.autostarted {
            self.autostarted = true;
            let ids: Vec<u64> = self.state.agents.iter().filter(|a| a.resume_on_launch).map(|a| a.id).collect();
            if ids.len() > 1 {
                self.view = View::Grid;
            }
            for id in ids {
                self.actions.push(Action::Start(id));
            }
        }
        self.tick(ctx);
        self.shortcuts(ctx);
        self.toolbar(ctx);
        self.sidebar(ctx);
        self.central(ctx);
        self.editor_window(ctx);
        self.settings_window(ctx);
        self.delete_window(ctx);
        self.toasts(ctx);
        self.apply_actions(ctx);
        self.maybe_save(false);
        if !self.live.is_empty() {
            // Status (working -> ready) changes without output, so tick a little.
            ctx.request_repaint_after(Duration::from_millis(500));
        } else if self.dirty {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Remember which agents were running so the next launch brings them back.
        let alive: HashSet<u64> = self.live.keys().copied().filter(|id| self.status(*id).alive()).collect();
        for a in &mut self.state.agents {
            a.resume_on_launch = alive.contains(&a.id);
        }
        let ids: Vec<u64> = self.live.keys().copied().collect();
        for id in ids {
            self.stop(id);
        }
        self.dirty = true;
        self.maybe_save(true);
    }
}

// ---------- small widgets ----------

fn logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 6.0, ACCENT);
    p.rect_filled(rect.shrink(6.0), 2.0, PANEL);
}

fn segmented(ui: &mut egui::Ui, view: &mut View) {
    egui::Frame::new().fill(BG).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(8).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (v, label) in [(View::Focus, "▣  Focus"), (View::Grid, "▦  Grid")] {
                let btn = Btn { text: label.into(), kind: BtnKind::Segment(*view == v) };
                if ui.add(btn).on_hover_text("Ctrl+Shift+G").clicked() {
                    *view = v;
                }
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq)]
enum BtnKind {
    Primary,
    Ghost,
    Danger,
    Icon,
    /// Focus/Grid toggle; `true` = the active one.
    Segment(bool),
    /// Broadcast target chip in the agent's color; `true` = included.
    Chip(Color32, bool),
}

/// Our button: smooth hover fade, pressed state, hand cursor.
struct Btn {
    text: String,
    kind: BtnKind,
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

impl egui::Widget for Btn {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        use BtnKind::*;
        let enabled = ui.is_enabled();
        let (font, pad_x, h, radius, min_w) = match self.kind {
            Primary => (bold(13.5), 14.0, 30.0, 8.0, 0.0),
            Ghost | Danger => (egui::FontId::proportional(13.5), 14.0, 30.0, 8.0, 0.0),
            Icon => (egui::FontId::proportional(14.0), 6.0, 26.0, 6.0, 26.0),
            Segment(_) => (egui::FontId::proportional(13.5), 14.0, 26.0, 6.0, 78.0),
            Chip(..) => (egui::FontId::proportional(11.5), 10.0, 22.0, 11.0, 0.0),
        };
        let galley = ui.painter().layout_no_wrap(self.text, font, Color32::PLACEHOLDER);
        let size = Vec2::new((galley.size().x + pad_x * 2.0).max(min_w), h);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovered = resp.hovered() && enabled;
        let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), hovered, 0.12);
        let lp = |a: Color32, b: Color32| lerp_color(a, b, t);
        let clear = Color32::TRANSPARENT;
        let accent_hi = Color32::from_rgb(0x95, 0xA3, 0xFF);
        let red_hi = Color32::from_rgb(0xF0, 0x6E, 0x6E);
        let (mut fill, stroke, mut fg) = match self.kind {
            Primary => (lp(ACCENT.gamma_multiply(0.9), accent_hi), Stroke::NONE, Color32::WHITE),
            Ghost => (lp(RAISED, HOVER), Stroke::new(1.0_f32, lp(BORDER, Color32::from_rgb(0x3E, 0x46, 0x5E))), lp(TEXT, Color32::WHITE)),
            Danger => (lp(RED.gamma_multiply(0.75), red_hi), Stroke::NONE, Color32::WHITE),
            Icon => (lp(clear, HOVER), Stroke::NONE, lp(MUTED, TEXT)),
            Segment(true) => (RAISED, Stroke::NONE, TEXT),
            Segment(false) => (lp(clear, HOVER.gamma_multiply(0.7)), Stroke::NONE, lp(MUTED, TEXT)),
            Chip(c, true) => (lp(c.gamma_multiply(0.18), c.gamma_multiply(0.3)), Stroke::new(1.0_f32, lp(c.gamma_multiply(0.5), c.gamma_multiply(0.85))), TEXT),
            Chip(c, false) => (lp(clear, HOVER), Stroke::new(1.0_f32, lp(BORDER, c.gamma_multiply(0.5))), lp(FAINT, MUTED)),
        };
        if resp.is_pointer_button_down_on() && enabled {
            fill = lerp_color(fill, Color32::BLACK, 0.18);
        }
        if !enabled {
            fill = fill.gamma_multiply(0.45);
            fg = fg.gamma_multiply(0.45);
        }
        let painter = ui.painter();
        painter.rect_filled(rect, radius, fill);
        if stroke.width > 0.0 {
            painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
        }
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, fg);
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

fn primary_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Primary }
}

fn ghost_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Ghost }
}

fn danger_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Danger }
}

fn icon_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Icon }
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32, with_dot: bool) -> egui::Response {
    let font = egui::FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let dot_w = if with_dot { 12.0 } else { 0.0 };
    let size = Vec2::new(galley.size().x + 16.0 + dot_w, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(10), color.gamma_multiply(0.14));
    let mut x = rect.left() + 8.0;
    if with_dot {
        p.circle_filled(egui::pos2(x + 3.0, rect.center().y), 3.0, color);
        x += dot_w;
    }
    p.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

fn status_pill(ui: &mut egui::Ui, s: Status) {
    pill(ui, &s.label(), s.color(), true);
}

fn chip(ui: &mut egui::Ui, text: &str, color: Color32, on: bool) -> egui::Response {
    let btn = Btn { text: text.into(), kind: BtnKind::Chip(color, on) };
    ui.add(btn).on_hover_text(if on { "Click to exclude" } else { "Click to include" })
}

fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::new().fill(RAISED).corner_radius(7).inner_margin(Margin::symmetric(9, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(RichText::new(label).small().color(MUTED));
            ui.label(RichText::new(value).small().color(TEXT).strong());
        });
    });
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED).strong());
}

fn found_label(ui: &mut egui::Ui, p: &Option<PathBuf>) {
    match p {
        Some(p) => ui.label(RichText::new(format!("✓ {}", p.display())).small().color(GREEN)),
        None => ui.label(RichText::new("✗ Not found").small().color(RED)),
    };
}

fn agent_card(ui: &mut egui::Ui, a: &Agent, status: Status, selected: bool, attention: bool, index: usize) -> egui::Response {
    let fill = if selected { RAISED } else { PANEL };
    let stroke = if selected { Stroke::new(1.0_f32, Color32::from_rgb(0x34, 0x3B, 0x50)) } else { Stroke::new(1.0_f32, Color32::TRANSPARENT) };
    let inner = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(10)
        .inner_margin(Margin { left: 16, right: 10, top: 9, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                app_icon(ui, a.provider, 18.0);
                ui.label(RichText::new(&a.name).font(bold(14.0)));
                if attention {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, AMBER);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| status_pill(ui, status));
            });
            let model = if a.provider == Provider::OpenCode { "/models" } else if a.model.trim().is_empty() { "default" } else { a.model.trim() };
            ui.label(RichText::new(format!("{} · {model}", a.provider.label())).small().color(MUTED));
            let u = a.total_usage();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("${:.4}", u.cost_usd)).small().color(TEXT));
                ui.label(RichText::new(format!("· {} tok", fmt_tokens(u.total_tokens()))).small().color(FAINT));
                if index < 9 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("⌃⇧{}", index + 1)).small().color(FAINT));
                    });
                }
            });
        });
    let rect = inner.response.rect;
    let resp = ui.interact(rect, egui::Id::new(("card", a.id)), Sense::click());
    let p = ui.painter();
    if resp.hovered() && !selected {
        p.rect_filled(rect, 10.0, Color32::from_white_alpha(6));
    }
    resp
}

fn provider_card(ui: &mut egui::Ui, p: Provider, sub: &str, selected: bool) -> egui::Response {
    let inner = egui::Frame::new()
        .fill(if selected { p.color().gamma_multiply(0.12) } else { RAISED })
        .stroke(Stroke::new(1.0_f32, if selected { p.color().gamma_multiply(0.7) } else { BORDER }))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, p.color());
                ui.label(RichText::new(p.label()).font(bold(14.0)));
            });
            ui.label(RichText::new(sub).small().color(MUTED));
        });
    ui.interact(inner.response.rect, egui::Id::new(("provider_card", p as u8)), Sense::click())
}

fn empty_state(ui: &mut egui::Ui, actions: &mut Vec<Action>, folder: &mut String) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 150.0).max(20.0));
        ui.label(RichText::new("Run your agents side by side").font(bold(22.0)));
        ui.add_space(2.0);
        ui.label(RichText::new("Pick an app to open your first agent's terminal.").color(MUTED));
        ui.add_space(16.0);
        ui.allocate_ui(Vec2::new(300.0, 0.0), |ui| {
            if let Some(p) = new_agent_menu(ui, folder) {
                actions.push(Action::Create(p));
            }
        });
    });
}

/// The "new agent" picker: one row per app (icon + name). Clicking a row
/// creates the agent and opens its terminal straight away.
fn new_agent_menu(ui: &mut egui::Ui, folder: &mut String) -> Option<Provider> {
    ui.set_width(300.0);
    let folder_ok = folder.trim().is_empty() || std::path::Path::new(folder.trim()).is_dir();
    let mut picked = None;
    for p in Provider::ALL {
        let sub = match p {
            Provider::Claude => "Anthropic's coding agent",
            Provider::OpenCode => "Open-source coding agent",
        };
        if app_row(ui, p, sub, folder_ok).clicked() && folder_ok {
            picked = Some(p);
        }
    }
    ui.add_space(4.0);
    ui.separator();
    ui.label(RichText::new("Opens in").small().color(MUTED));
    ui.add(
        egui::TextEdit::singleline(folder)
            .hint_text("home folder")
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 5)),
    );
    if !folder_ok {
        ui.label(RichText::new("Folder not found").small().color(RED));
    }
    picked
}

fn app_row(ui: &mut egui::Ui, p: Provider, sub: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() && enabled {
        painter.rect_filled(rect, 8.0, HOVER);
    }
    let icon = egui::Rect::from_min_size(rect.min + Vec2::new(8.0, 8.0), Vec2::splat(30.0));
    draw_app_icon(painter, icon, p);
    let x = icon.right() + 12.0;
    let name_color = if enabled { TEXT } else { FAINT };
    painter.text(egui::pos2(x, rect.top() + 7.0), egui::Align2::LEFT_TOP, p.label(), bold(14.0), name_color);
    painter.text(egui::pos2(x, rect.top() + 26.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(11.5), MUTED);
    if resp.hovered() && enabled {
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "Open ›",
            egui::FontId::proportional(12.0),
            p.color(),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// App icon as a vector tile: Claude's orange spark, OpenCode's square "o".
fn draw_app_icon(painter: &egui::Painter, rect: egui::Rect, p: Provider) {
    let w = rect.width();
    let c = rect.center();
    match p {
        Provider::Claude => {
            painter.rect_filled(rect, w * 0.24, CLAUDE);
            let stroke = Stroke::new((w * 0.085).max(1.5), Color32::from_rgb(0xFF, 0xF4, 0xEE));
            for i in 0..10 {
                let a = i as f32 / 10.0 * std::f32::consts::TAU;
                let dir = Vec2::new(a.cos(), a.sin());
                let len = if i % 2 == 0 { 0.34 } else { 0.27 };
                painter.line_segment([c + dir * (w * 0.07), c + dir * (w * len)], stroke);
            }
        }
        Provider::OpenCode => {
            painter.rect_filled(rect, w * 0.24, Color32::from_rgb(0x1A, 0x1C, 0x22));
            painter.rect_stroke(rect, w * 0.24, Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x3F, 0x4C)), egui::StrokeKind::Inside);
            let o = egui::Rect::from_center_size(c, Vec2::splat(w * 0.44));
            painter.rect_stroke(o, 0.0, Stroke::new((w * 0.1).max(1.5), Color32::WHITE), egui::StrokeKind::Inside);
            let inner = egui::Rect::from_min_max(egui::pos2(o.left(), o.center().y), o.max).shrink(w * 0.1);
            painter.rect_filled(inner, 0.0, Color32::from_gray(0x8A));
        }
    }
}

fn app_icon(ui: &mut egui::Ui, p: Provider, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    draw_app_icon(ui.painter(), rect, p);
}

fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}
