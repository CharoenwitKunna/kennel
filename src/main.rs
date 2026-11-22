// GUI app: never open a console window (dev builds included).
#![windows_subsystem = "windows"]

mod agent;
mod icon;
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
            .with_icon(egui::IconData { rgba: icon::icon_rgba(64), width: 64, height: 64 })
            .with_inner_size([1040.0, 620.0])
            .with_min_inner_size([720.0, 440.0])
            // Our toolbar is the title bar (drag, double-click, window buttons).
            .with_decorations(false),
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
    /// Idle on a question, permission prompt or confirmation: blocked on you.
    Asking,
    Exited(i32),
}

impl Status {
    fn label(self) -> String {
        match self {
            Status::Stopped => "Stopped".into(),
            Status::Starting => "Starting".into(),
            Status::Working => "Working".into(),
            Status::Waiting => "Ready".into(),
            Status::Asking => "Needs you".into(),
            Status::Exited(0) => "Exited".into(),
            Status::Exited(c) => format!("Exited ({c})"),
        }
    }
    fn color(self) -> Color32 {
        match self {
            Status::Stopped => GREY,
            Status::Starting => ACCENT,
            Status::Working => GREEN,
            Status::Waiting => Color32::from_rgb(0x8F, 0x9B, 0xB8),
            Status::Asking => AMBER,
            Status::Exited(0) => GREY,
            Status::Exited(_) => RED,
        }
    }
    fn alive(self) -> bool {
        matches!(self, Status::Starting | Status::Working | Status::Waiting | Status::Asking)
    }
}

struct Live {
    sess: TermSession,
    was_working: bool,
    /// Screen shows a question / permission prompt (refreshed by `scan_prompts`).
    asking: bool,
    was_asking: bool,
    last_scan: Instant,
    /// The user has typed into this agent (gates "needs you").
    interacted: bool,
    claude: Option<ClaudeTracker>,
    last_poll: Instant,
    poll_inflight: bool,
    last_export: Option<Instant>,
    final_polled: bool,
    started: Instant,
    /// Resumed Claude conversation: auto-answer its trust prompt for this folder.
    pending_trust: Option<String>,
    /// `line_mark` when you last had this agent on screen (unread = new lines since).
    seen_mark: usize,
    /// Status shown last frame, and since when (for "Working for 2m").
    shown: Status,
    since: Instant,
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
    /// Agent being renamed inline, with the text typed so far.
    renaming: Option<(u64, String)>,
    /// Focus the rename box on its first frame.
    rename_focus: bool,
    /// Sidebar card being dragged to a new position.
    dragging: Option<u64>,
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
    /// Grid column widths / row heights as fractions (each list sums to 1).
    grid_cols: Vec<f32>,
    grid_rows: Vec<f32>,
    /// What the central area shows, and when that last changed (for the fade).
    view_key: (View, Option<u64>),
    view_changed: Instant,
}

/// Length of the fade when switching views or agents.
const FADE: f32 = 0.16;

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
            renaming: None,
            rename_focus: false,
            dragging: None,
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
            grid_cols: Vec::new(),
            grid_rows: Vec::new(),
            view_key: (View::Focus, selected),
            view_changed: Instant::now() - Duration::from_secs(1),
        }
    }

    /// New output lines since you last had this agent on screen.
    fn unread(&self, id: u64) -> usize {
        self.live.get(&id).map_or(0, |l| l.sess.line_mark().saturating_sub(l.seen_mark))
    }

    /// How long the agent has been in its current status.
    fn status_age(&self, id: u64) -> Option<Duration> {
        self.live.get(&id).map(|l| l.since.elapsed())
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toasts.push((Instant::now(), msg.into()));
    }

    fn status(&self, id: u64) -> Status {
        let Some(l) = self.live.get(&id) else { return Status::Stopped };
        if let Some(code) = l.sess.exited() {
            return Status::Exited(code);
        }
        if l.sess.quiet_for().is_none() {
            Status::Starting
        } else if l.sess.busy() {
            Status::Working
        } else if l.asking {
            Status::Asking
        } else {
            Status::Waiting
        }
    }

    /// Look at idle agents' screens for questions / permission prompts.
    /// Throttled; skipped while an agent is producing output.
    fn scan_prompts(&mut self) {
        for l in self.live.values_mut() {
            if l.last_scan.elapsed() < Duration::from_millis(400) {
                continue;
            }
            l.last_scan = Instant::now();
            let idle = l.sess.exited().is_none()
                && l.pending_trust.is_none() // Kennel is about to answer that one itself
                && !l.sess.busy()
                && l.sess.quiet_for().is_some_and(|d| d > Duration::from_millis(600));
            l.asking = idle && runner::screen_asks_user(&l.sess.screen_text());
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
        let auto_trust = self.state.settings.auto_trust_on_resume;
        let a = self.state.agent_mut(id).unwrap();
        a.session_id = Some(launch.session_id.clone());
        a.runs += 1;
        let claude =
            (a.provider == Provider::Claude).then(|| ClaudeTracker::new(id, &launch.session_id));
        let pending_trust = (a.provider == Provider::Claude && launch.resumed && auto_trust).then(|| a.cwd.clone());
        self.live.insert(
            id,
            Live {
                sess,
                was_working: false,
                asking: false,
                was_asking: false,
                last_scan: Instant::now(),
                interacted: false,
                claude,
                last_poll: Instant::now(),
                poll_inflight: false,
                last_export: None,
                final_polled: false,
                started: Instant::now(),
                pending_trust,
                seen_mark: 0,
                shown: Status::Starting,
                since: Instant::now(),
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

        self.answer_resume_trust();
        self.scan_prompts();
        let focused_term = ctx.memory(|m| m.focused());
        let ids: Vec<u64> = self.live.keys().copied().collect();
        let on_screen = |id: u64| match self.view {
            View::Focus => self.selected == Some(id),
            View::Grid => true,
        };
        let visible: HashSet<u64> = ids.iter().copied().filter(|id| on_screen(*id)).collect();
        for id in ids {
            let status = self.status(id);
            let focused = focused_term == Some(term_id(id));
            let l = self.live.get_mut(&id).unwrap();
            if status != l.shown {
                l.shown = status;
                l.since = Instant::now();
            }
            if visible.contains(&id) {
                l.seen_mark = l.sess.line_mark();
            }
            let working = status == Status::Working;
            let asking = status == Status::Asking;
            let finished_turn = l.was_working && matches!(status, Status::Waiting | Status::Asking);
            let newly_asking = asking && !l.was_asking;
            l.was_working = working;
            l.was_asking = asking;
            let bell = l.sess.take_bell();
            let exited = matches!(status, Status::Exited(_)) && !l.final_polled;
            if exited {
                l.final_polled = true;
            }
            // A question always needs you; a finished turn only if you gave it one.
            if ((finished_turn && l.interacted) || newly_asking || bell || exited) && !focused {
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

    /// A resumed Claude conversation re-asks "trust this folder?" (always, for the
    /// home folder). The folder was trusted when the conversation began, so pick
    /// "Yes", but only if the prompt names this agent's own folder.
    fn answer_resume_trust(&mut self) {
        for l in self.live.values_mut() {
            let Some(folder) = l.pending_trust.as_deref() else { continue };
            if l.started.elapsed() > Duration::from_secs(30) || l.sess.exited().is_some() {
                l.pending_trust = None;
                continue;
            }
            // Wait until the prompt has finished drawing and is ready for keys.
            if !l.sess.quiet_for().is_some_and(|d| d > Duration::from_millis(700)) {
                continue;
            }
            let screen = l.sess.screen_text();
            if !screen.contains("Yes, I trust this folder") {
                // Claude may show its normal prompt (folder already trusted).
                if screen.contains('\u{276f}') && l.started.elapsed() > Duration::from_secs(8) {
                    l.pending_trust = None;
                }
                continue;
            }
            let names_folder = screen.lines().any(|line| same_folder(line, folder));
            if names_folder {
                // Options are "No, exit" (default) then "Yes, I trust this folder".
                l.sess.write(b"\x1b[B".to_vec());
                l.sess.write_after(b"\r", Duration::from_millis(300));
                // The prompt stays on screen a moment longer; don't call it a question.
                l.last_scan = Instant::now() + Duration::from_secs(2);
            }
            l.pending_trust = None;
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
            .exact_height(32.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin { left: 6, right: 0, top: 0, bottom: 0 }).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                // Empty toolbar space acts as the title bar. Registered first so
                // the buttons drawn on top keep their clicks.
                let bar = ui.interact(ui.max_rect(), egui::Id::new("titlebar"), Sense::click_and_drag());
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                if bar.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                } else if bar.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal_centered(|ui| {
                    let tip = if self.state.settings.sidebar_hidden { "Show sidebar (Ctrl+Shift+B)" } else { "Hide sidebar (Ctrl+Shift+B)" };
                    if ui.add(icon_button("☰")).on_hover_text(tip).clicked() {
                        self.state.settings.sidebar_hidden = !self.state.settings.sidebar_hidden;
                        self.dirty = true;
                    }
                    logo(ui);
                    ui.label(RichText::new("Kennel").font(bold(14.0)));
                    ui.add_space(8.0);
                    segmented(ui, &mut self.view);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if window_button(ui, WinBtn::Close).on_hover_text("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let (kind, tip) = if maximized { (WinBtn::Restore, "Restore") } else { (WinBtn::Maximize, "Maximize") };
                        if window_button(ui, kind).on_hover_text(tip).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if window_button(ui, WinBtn::Minimize).on_hover_text("Minimize").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(8.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
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
                        if ui.add(icon_button("⚙")).on_hover_text("Settings").clicked() {
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
        if self.state.settings.sidebar_hidden {
            return;
        }
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(184.0)
            .width_range(150.0..=360.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::same(4)).stroke(Stroke::new(1.0_f32, BORDER)))
            .show(ctx, |ui| {
                if self.state.agents.is_empty() {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No agents yet").color(MUTED));
                        ui.add_space(6.0);
                        if ui.add(primary_button("New agent")).clicked() {
                            self.actions.push(Action::NewAgent);
                        }
                    });
                    return;
                }

                let agents = self.state.agents.clone();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let mut rects = Vec::with_capacity(agents.len());
                    for a in &agents {
                        let status = self.status(a.id);
                        let selected = self.selected == Some(a.id);
                        let attention = self.attention.contains(&a.id);
                        let (rect, resp) = self.agent_card(ui, a, status, selected, attention);
                        rects.push((a.id, rect));
                        let Some(resp) = resp else { continue };
                        if resp.drag_started() {
                            self.dragging = Some(a.id);
                        }
                        if resp.clicked() {
                            self.actions.push(Action::Select(a.id));
                        }
                        if resp.double_clicked() {
                            self.begin_rename(a.id);
                        }
                        resp.context_menu(|ui| {
                            ui.set_min_width(160.0);
                            if ui.button("Rename").clicked() {
                                self.begin_rename(a.id);
                                ui.close_menu();
                            }
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
                    }
                    self.drag_reorder(ui, &rects);
                });
            });
    }

    /// One-row sidebar card: icon, name, status, cost. Returns its rect, and
    /// its response unless it is being renamed (the text box owns input then).
    fn agent_card(
        &mut self,
        ui: &mut egui::Ui,
        a: &Agent,
        status: Status,
        selected: bool,
        attention: bool,
    ) -> (egui::Rect, Option<egui::Response>) {
        let renaming = self.renaming.as_ref().is_some_and(|(id, _)| *id == a.id);
        let unread = self.unread(a.id);
        let age = self.status_age(a.id);
        let inner = egui::Frame::new()
            .fill(if selected { RAISED } else { PANEL })
            .stroke(Stroke::new(1.0_f32, if selected { Color32::from_rgb(0x34, 0x3B, 0x50) } else { Color32::TRANSPARENT }))
            .corner_radius(5)
            .inner_margin(Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.horizontal(|ui| {
                    ui.set_height(18.0);
                    ui.spacing_mut().item_spacing.x = 5.0;
                    app_icon(ui, a.provider, 14.0);
                    if renaming {
                        self.rename_box(ui, a.id, 12.5, ui.available_width() - 2.0);
                        return;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        status_indicator(ui, status, false, age);
                        unread_badge(ui, unread);
                        if attention {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 3.0, AMBER);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&a.name).font(bold(12.5))).truncate().selectable(false));
                        });
                    });
                });
                if !renaming {
                    ui.horizontal(|ui| {
                        ui.add_space(19.0); // under the name, past the icon
                        ui.add(
                            egui::Label::new(RichText::new(card_subtitle(a)).size(10.5).color(FAINT))
                                .truncate()
                                .selectable(false),
                        );
                    });
                }
            });
        let rect = inner.response.rect;
        if renaming {
            return (rect, None);
        }
        let resp = ui.interact(rect, egui::Id::new(("card", a.id)), Sense::click_and_drag());
        if self.dragging == Some(a.id) {
            ui.painter().rect_filled(rect, 6.0, Color32::from_black_alpha(110));
        }
        let resp = if self.dragging.is_none() {
            resp.on_hover_text(format!("{}\n\nDouble-click to rename · drag to reorder", agent_info(a)))
        } else {
            resp
        };
        (rect, Some(resp))
    }

    /// While a card is dragged: show where it will land, and move it on release.
    fn drag_reorder(&mut self, ui: &mut egui::Ui, rects: &[(u64, egui::Rect)]) {
        let Some(drag_id) = self.dragging else { return };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let (pos, released) = ui.input(|i| (i.pointer.interact_pos(), !i.pointer.primary_down()));
        let (Some(pos), Some(last)) = (pos, rects.last()) else {
            self.dragging = None;
            return;
        };
        // Slot = how many cards sit above the pointer.
        let target = rects.iter().position(|(_, r)| pos.y < r.center().y).unwrap_or(rects.len());
        let y = if target < rects.len() { rects[target].1.top() - 2.0 } else { last.1.bottom() + 2.0 };
        let x = last.1.x_range();
        ui.painter().hline(x, y, Stroke::new(2.0_f32, ACCENT));
        if released {
            self.dragging = None;
            if let Some(from) = self.state.agents.iter().position(|a| a.id == drag_id) {
                let to = if from < target { target - 1 } else { target };
                if to != from {
                    let a = self.state.agents.remove(from);
                    self.state.agents.insert(to.min(self.state.agents.len()), a);
                    self.dirty = true;
                }
            }
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::same(4)))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                if self.state.agents.is_empty() {
                    empty_state(ui, &mut self.actions, &mut self.new_folder);
                    return;
                }
                let key = (self.view, if self.view == View::Focus { self.selected } else { None });
                if key != self.view_key {
                    self.view_key = key;
                    self.view_changed = Instant::now();
                }
                match self.view {
                    View::Focus => self.focus_view(ui),
                    View::Grid => self.grid_view(ui),
                }
                // Fade the new content in: a background-colored veil that clears quickly.
                let t = self.view_changed.elapsed().as_secs_f32() / FADE;
                if t < 1.0 {
                    let veil = (1.0 - t).powi(2);
                    ui.painter().rect_filled(area.expand(4.0), 0.0, BG.gamma_multiply(veil));
                    ui.ctx().request_repaint();
                }
            });
    }

    fn focus_view(&mut self, ui: &mut egui::Ui) {
        // One row: tabs on the left; the open agent's stats and buttons on the right.
        let current = self.selected.and_then(|id| self.state.agent(id).cloned());
        ui.horizontal(|ui| {
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(a) = &current {
                    let context = self.live.get(&a.id).and_then(|l| l.claude.as_ref()).and_then(|t| t.context_pct);
                    self.pane_controls(ui, a, self.status(a.id), false);
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.x = 3.0;
                    pane_stats(ui, a, context, true);
                    ui.add_space(8.0);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.tab_strip(ui));
            });
        });
        ui.add_space(2.0);
        let Some(id) = self.selected.filter(|id| self.state.agent(*id).is_some()) else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Select an agent").color(MUTED)));
            return;
        };
        self.agent_pane(ui, id, false);
    }

    /// Browser-style tabs, one per agent (Focus view).
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let agents = self.state.agents.clone();
        egui::ScrollArea::horizontal()
            .id_salt("tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for a in &agents {
                        let selected = self.selected == Some(a.id);
                        let status = self.status(a.id);
                        let unread = if selected { 0 } else { self.unread(a.id) };
                        let attention = self.attention.contains(&a.id);
                        let age = self.status_age(a.id);
                        let renaming = self.renaming.as_ref().is_some_and(|(rid, _)| *rid == a.id);
                        let inner = egui::Frame::new()
                            .corner_radius(CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 })
                            .inner_margin(Margin { left: 8, right: 8, top: 3, bottom: 3 })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    app_icon(ui, a.provider, 12.0);
                                    if renaming {
                                        self.rename_box(ui, a.id, 12.0, 140.0);
                                        return;
                                    }
                                    let color = if selected { TEXT } else { MUTED };
                                    ui.add(egui::Label::new(RichText::new(&a.name).font(bold(12.0)).color(color)).selectable(false));
                                    status_indicator(ui, status, false, age);
                                    unread_badge(ui, unread);
                                    if attention && !selected {
                                        let (r, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
                                        ui.painter().circle_filled(r.center(), 3.0, AMBER);
                                    }
                                });
                            });
                        let rect = inner.response.rect;
                        if renaming {
                            continue;
                        }
                        let resp = ui.interact(rect, egui::Id::new(("tab", a.id)), Sense::click());
                        if resp.double_clicked() {
                            self.begin_rename(a.id);
                        }
                        if selected {
                            let y = rect.bottom() - 1.0;
                            ui.painter().line_segment(
                                [egui::pos2(rect.left() + 6.0, y), egui::pos2(rect.right() - 6.0, y)],
                                Stroke::new(2.0_f32, a.provider.color()),
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 7.0, Color32::from_white_alpha(8));
                        }
                        if resp.clicked() {
                            self.actions.push(Action::Select(a.id));
                        }
                        if resp.middle_clicked() && status.alive() {
                            self.actions.push(Action::Stop(a.id));
                        }
                        resp.on_hover_text(format!("{}\n\nDouble-click to rename · middle-click to stop", agent_info(a)));
                    }
                });
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
        if self.grid_cols.len() != cols {
            self.grid_cols = vec![1.0 / cols as f32; cols];
        }
        if self.grid_rows.len() != rows {
            self.grid_rows = vec![1.0 / rows as f32; rows];
        }
        let gap = 4.0;
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let avail_w = area.width() - gap * (cols as f32 - 1.0);
        let avail_h = area.height() - gap * (rows as f32 - 1.0);
        let xs = offsets(&self.grid_cols, avail_w, gap, area.left());
        let ys = offsets(&self.grid_rows, avail_h, gap, area.top());

        for (i, id) in ids.into_iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let rect = egui::Rect::from_min_size(
                egui::pos2(xs[c], ys[r]),
                Vec2::new(self.grid_cols[c] * avail_w, self.grid_rows[r] * avail_h),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
            self.agent_pane(&mut child, id, true);
        }

        // Draggable dividers between columns and between rows (double-click resets).
        for c in 0..cols.saturating_sub(1) {
            let x = xs[c + 1] - gap / 2.0;
            let hit = egui::Rect::from_x_y_ranges(x - gap / 2.0..=x + gap / 2.0, area.y_range());
            let resp = ui.interact(hit, egui::Id::new(("col_div", c)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if resp.dragged() {
                resize_pair(&mut self.grid_cols, c, resp.drag_delta().x / avail_w);
            }
            if resp.double_clicked() {
                self.grid_cols = vec![1.0 / cols as f32; cols];
            }
            if resp.hovered() || resp.dragged() {
                ui.painter().vline(x, area.y_range(), Stroke::new(2.0_f32, ACCENT.gamma_multiply(0.7)));
            }
        }
        for r in 0..rows.saturating_sub(1) {
            let y = ys[r + 1] - gap / 2.0;
            let hit = egui::Rect::from_x_y_ranges(area.x_range(), y - gap / 2.0..=y + gap / 2.0);
            let resp = ui.interact(hit, egui::Id::new(("row_div", r)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if resp.dragged() {
                resize_pair(&mut self.grid_rows, r, resp.drag_delta().y / avail_h);
            }
            if resp.double_clicked() {
                self.grid_rows = vec![1.0 / rows as f32; rows];
            }
            if resp.hovered() || resp.dragged() {
                ui.painter().hline(area.x_range(), y, Stroke::new(2.0_f32, ACCENT.gamma_multiply(0.7)));
            }
        }
    }

    /// Stop/start, maximize/back and the ⋯ menu, laid out right to left.
    fn pane_controls(&mut self, ui: &mut egui::Ui, a: &Agent, status: Status, in_grid: bool) {
        let id = a.id;
        ui.spacing_mut().item_spacing.x = 2.0;
        let more = ui.add(icon_button("⋯")).on_hover_text("More");
        let menu_id = egui::Id::new(("pane_menu", id));
        if more.clicked() {
            ui.memory_mut(|m| m.toggle_popup(menu_id));
        }
        egui::popup_below_widget(ui, menu_id, &more, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(160.0);
            if ui.button("Rename").clicked() {
                self.begin_rename(id);
                ui.close_menu();
            }
            if status.alive() && ui.button("Restart").clicked() {
                self.actions.push(Action::Restart(id));
                ui.close_menu();
            }
            if ui.button("Edit…").clicked() {
                self.actions.push(Action::Edit(id));
                ui.close_menu();
            }
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
        let (icon, tip) = if in_grid { ("⤢", "Maximize (or double-click the title)") } else { ("⤡", "Back to grid") };
        if ui.add(icon_button(icon)).on_hover_text(tip).clicked() {
            if in_grid {
                self.actions.push(Action::OpenFocus(id));
            } else {
                self.view = View::Grid;
            }
        }
        if status.alive() {
            if ui.add(icon_button("■")).on_hover_text("Stop").clicked() {
                self.actions.push(Action::Stop(id));
            }
        } else {
            let tip = if a.session_id.is_some() { "Resume" } else { "Start" };
            if ui.add(icon_button("▶")).on_hover_text(tip).clicked() {
                self.actions.push(Action::Start(id));
            }
        }
    }

    /// One agent: a slim title row (grid only) on top of its terminal.
    fn agent_pane(&mut self, ui: &mut egui::Ui, id: u64, in_grid: bool) {
        let Some(a) = self.state.agent(id).cloned() else { return };
        let status = self.status(id);
        let age = self.status_age(id);
        let context = self.live.get(&id).and_then(|l| l.claude.as_ref()).and_then(|t| t.context_pct);
        let focused = ui.memory(|m| m.has_focus(term_id(id)));
        let border = if focused {
            a.provider.color().gamma_multiply(0.85)
        } else if self.attention.contains(&id) {
            AMBER.gamma_multiply(0.7)
        } else {
            BORDER
        };
        egui::Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, border))
            .corner_radius(6)
            .inner_margin(Margin::same(2))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                // In Focus view the tab strip carries the name, stats and buttons.
                if in_grid {
                    // Empty space in the title row: double-click to maximize.
                    let title_rect = egui::Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 22.0));
                    let title_bg = ui.interact(title_rect, egui::Id::new(("title", id)), Sense::click());
                    if title_bg.double_clicked() {
                        self.actions.push(Action::OpenFocus(id));
                    }
                    ui.horizontal(|ui| {
                        ui.set_height(22.0);
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.add_space(3.0);
                        app_icon(ui, a.provider, 14.0);
                        self.name_widget(ui, &a, 12.5);
                        status_indicator(ui, status, true, age);
                        ui.spacing_mut().item_spacing.x = 3.0;
                        ui.label(RichText::new("·").small().color(FAINT));
                        pane_stats(ui, &a, context, false);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            self.pane_controls(ui, &a, status, true);
                        });
                    });
                }
                if let Some(p) = context {
                    // Thin context gauge under the title.
                    let (bar, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
                    let bar = bar.shrink2(Vec2::new(4.0, 0.0));
                    ui.painter().rect_filled(bar, 1.5, BORDER);
                    let fill = egui::Rect::from_min_size(bar.min, Vec2::new(bar.width() * (p / 100.0).clamp(0.0, 1.0), bar.height()));
                    ui.painter().rect_filled(fill, 1.5, context_color(p));
                    resp.on_hover_text(format!("Context {p:.0}% used"));
                }
                egui::Frame::new().fill(TERM_BG).corner_radius(4).inner_margin(Margin::same(1)).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.terminal_or_placeholder(ui, id, status, &a);
                });
            });
    }

    fn begin_rename(&mut self, id: u64) {
        if let Some(a) = self.state.agent(id) {
            self.renaming = Some((id, a.name.clone()));
            self.rename_focus = true;
        }
    }

    /// Agent name; double-click turns it into a text box.
    fn name_widget(&mut self, ui: &mut egui::Ui, a: &Agent, size: f32) {
        if self.renaming.as_ref().is_some_and(|(id, _)| *id == a.id) {
            // As wide as the name (plus room to type), so the title doesn't shrink.
            let name_w = ui.fonts(|f| f.layout_no_wrap(a.name.clone(), bold(size), TEXT).size().x);
            let width = (name_w + 40.0).clamp(140.0, 320.0).min(ui.available_width() - 120.0);
            self.rename_box(ui, a.id, size, width);
            return;
        }
        let resp = ui
            .add(egui::Label::new(RichText::new(&a.name).font(bold(size))).sense(Sense::click()).selectable(false))
            .on_hover_text(format!("{}\n\nDouble-click to rename", agent_info(a)));
        if resp.double_clicked() {
            self.begin_rename(a.id);
        }
    }

    /// Inline rename box: Enter saves, Esc or empty cancels.
    fn rename_box(&mut self, ui: &mut egui::Ui, id: u64, size: f32, width: f32) {
        let Some((_, buf)) = self.renaming.as_mut() else { return };
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .font(bold(size))
                .desired_width(width.max(60.0))
                .margin(Margin::symmetric(4, 1)),
        );
        if std::mem::take(&mut self.rename_focus) {
            resp.request_focus();
        }
        if resp.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            let (_, name) = self.renaming.take().unwrap();
            let name = name.trim();
            if !cancelled && !name.is_empty() {
                if let Some(a) = self.state.agent_mut(id) {
                    a.name = name.to_string();
                    self.dirty = true;
                }
            }
        }
    }

    fn terminal_or_placeholder(&mut self, ui: &mut egui::Ui, id: u64, status: Status, a: &Agent) {
        let font_size = self.state.settings.font_size;
        if let Some(l) = self.live.get_mut(&id) {
            if self.focus_term == Some(id) {
                ui.memory_mut(|m| m.request_focus(term_id(id)));
                self.focus_term = None;
            }
            let cwd = match a.cwd.trim() {
                "" => PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()),
                c => PathBuf::from(c),
            };
            let out = term_view::show(ui, &mut l.sess, term_id(id), font_size, &cwd);
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
                        .margin(Margin::symmetric(6, 4)),
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
                            .margin(Margin::symmetric(6, 4)),
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
                        .margin(Margin::symmetric(6, 4)),
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
                            .margin(Margin::symmetric(6, 4)),
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
                            .margin(Margin::symmetric(6, 4)),
                    )
                    .changed();
                found_label(ui, &opencode);
                ui.add_space(8.0);
                field_label(ui, "Terminal font size");
                self.dirty |= ui.add(egui::Slider::new(&mut self.state.settings.font_size, 10.0..=20.0).step_by(0.5)).changed();
                ui.add_space(8.0);
                field_label(ui, "Resuming");
                self.dirty |= ui
                    .checkbox(
                        &mut self.state.settings.auto_trust_on_resume,
                        "Skip Claude's folder-trust question when resuming a conversation",
                    )
                    .changed();
                ui.label(
                    RichText::new("Only for the agent's own folder, which you trusted when the conversation started.")
                        .small()
                        .color(MUTED),
                );
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
            if i.consume_key(cs, Key::B) {
                self.state.settings.sidebar_hidden = !self.state.settings.sidebar_hidden;
                self.dirty = true;
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
        window_edges(ctx);
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
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, ACCENT);
    p.rect_filled(rect.shrink(5.0), 2.0, PANEL);
}

fn segmented(ui: &mut egui::Ui, view: &mut View) {
    egui::Frame::new().fill(BG).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(6).inner_margin(Margin::same(2)).show(ui, |ui| {
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
            Primary => (bold(12.5), 9.0, 24.0, 6.0, 0.0),
            Ghost | Danger => (egui::FontId::proportional(12.5), 9.0, 24.0, 6.0, 0.0),
            Icon => (egui::FontId::proportional(12.5), 4.0, 20.0, 4.0, 20.0),
            Segment(_) => (egui::FontId::proportional(12.0), 9.0, 20.0, 5.0, 58.0),
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

#[derive(Clone, Copy, PartialEq)]
enum WinBtn {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Title-bar window button, full toolbar height, drawn as crisp shapes.
fn window_button(ui: &mut egui::Ui, kind: WinBtn) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(42.0, ui.available_height()), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.10);
    let hot = if kind == WinBtn::Close { Color32::from_rgb(0xC4, 0x2B, 0x1C) } else { HOVER };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, lerp_color(Color32::TRANSPARENT, hot, t));
    let fg = lerp_color(MUTED, Color32::WHITE, t);
    let s = Stroke::new(1.0_f32, fg);
    let c = rect.center();
    let h = 5.0;
    match kind {
        WinBtn::Minimize => {
            painter.hline(c.x - h..=c.x + h, c.y + 0.5, s);
        }
        WinBtn::Maximize => {
            painter.rect_stroke(egui::Rect::from_center_size(c, Vec2::splat(2.0 * h)), 1.0, s, egui::StrokeKind::Inside);
        }
        WinBtn::Restore => {
            let back = egui::Rect::from_center_size(c + Vec2::new(2.0, -2.0), Vec2::splat(2.0 * h - 2.0));
            let front = egui::Rect::from_center_size(c + Vec2::new(-1.0, 1.0), Vec2::splat(2.0 * h - 2.0));
            painter.rect_stroke(back, 1.0, s, egui::StrokeKind::Inside);
            painter.rect_filled(front, 1.0, PANEL.gamma_multiply(1.0 - t));
            painter.rect_stroke(front, 1.0, s, egui::StrokeKind::Inside);
        }
        WinBtn::Close => {
            painter.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], s);
            painter.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], s);
        }
    }
    resp
}

/// Without native decorations, resize from a thin band along the window edges.
fn window_edges(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D};
    let (maximized, pos, pressed) =
        ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.pointer.hover_pos(), i.pointer.primary_pressed()));
    let screen = ctx.screen_rect();
    if !maximized {
        // Hairline so the window edge is visible on any background.
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("frame")))
            .rect_stroke(screen, 0.0, Stroke::new(1.0_f32, BORDER), egui::StrokeKind::Inside);
    }
    let Some(p) = pos else { return };
    if maximized {
        return;
    }
    const EDGE: f32 = 5.0;
    let (l, r) = (p.x - screen.left() < EDGE, screen.right() - p.x < EDGE);
    let (t, b) = (p.y - screen.top() < EDGE, screen.bottom() - p.y < EDGE);
    let dir = match (l, r, t, b) {
        (true, _, true, _) => Some((D::NorthWest, C::ResizeNorthWest)),
        (_, true, true, _) => Some((D::NorthEast, C::ResizeNorthEast)),
        (true, _, _, true) => Some((D::SouthWest, C::ResizeSouthWest)),
        (_, true, _, true) => Some((D::SouthEast, C::ResizeSouthEast)),
        (true, ..) => Some((D::West, C::ResizeWest)),
        (_, true, ..) => Some((D::East, C::ResizeEast)),
        (_, _, true, _) => Some((D::North, C::ResizeNorth)),
        (.., true) => Some((D::South, C::ResizeSouth)),
        _ => None,
    };
    if let Some((d, cursor)) = dir {
        ctx.set_cursor_icon(cursor);
        if pressed {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(d));
        }
    }
}

/// Status as a dot (plus label if `label`), or a small pulse while working.
/// Small amber "?" disc: the agent is waiting on your answer.
fn question_badge(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(r.center(), 6.5, AMBER);
    painter.text(r.center() + Vec2::new(0.0, 0.5), egui::Align2::CENTER_CENTER, "?", bold(10.0), Color32::from_rgb(0x1A, 0x14, 0x05));
    resp
}

/// Status as a dot (plus label if `label`), a pulse while working, or a "?"
/// badge when waiting on you. Hover shows how long it has been in that state.
fn status_indicator(ui: &mut egui::Ui, s: Status, label: bool, age: Option<Duration>) {
    let tip = match age {
        Some(d) if s.alive() => format!("{} for {}", s.label(), fmt_dur(d)),
        _ => s.label(),
    };
    let mark = match s {
        Status::Working => working_pulse(ui),
        Status::Asking => question_badge(ui),
        _ => status_dot(ui, s),
    };
    mark.on_hover_text(&tip);
    if label && s != Status::Working {
        ui.label(RichText::new(s.label()).small().color(s.color())).on_hover_text(tip);
    }
}

/// Three tiny dots pulsing in sequence.
fn working_pulse(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(15.0, 8.0), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let painter = ui.painter();
    for i in 0..3 {
        let k = ((t * 5.0 - i as f32 * 0.8).sin() + 1.0) / 2.0; // 0..1
        let center = egui::pos2(rect.left() + 2.5 + i as f32 * 5.0, rect.center().y - k);
        painter.circle_filled(center, 1.6, GREEN.gamma_multiply(0.3 + 0.7 * k));
    }
    // ~30 fps, and only while something is working.
    ui.ctx().request_repaint_after(Duration::from_millis(33));
    resp
}

fn status_dot(ui: &mut egui::Ui, s: Status) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
    ui.painter().circle_filled(r.center(), 3.0, s.color());
    resp
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
        ui.allocate_ui(Vec2::new(260.0, 0.0), |ui| {
            if let Some(p) = new_agent_menu(ui, folder) {
                actions.push(Action::Create(p));
            }
        });
    });
}

/// The "new agent" picker: one row per app (icon + name). Clicking a row
/// creates the agent and opens its terminal straight away.
fn new_agent_menu(ui: &mut egui::Ui, folder: &mut String) -> Option<Provider> {
    ui.set_width(260.0);
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
            .margin(Margin::symmetric(6, 3)),
    );
    if !folder_ok {
        ui.label(RichText::new("Folder not found").small().color(RED));
    }
    picked
}

fn app_row(ui: &mut egui::Ui, p: Provider, sub: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() && enabled {
        painter.rect_filled(rect, 6.0, HOVER);
    }
    let icon = egui::Rect::from_min_size(rect.min + Vec2::new(6.0, 7.0), Vec2::splat(22.0));
    draw_app_icon(painter, icon, p);
    let x = icon.right() + 9.0;
    let name_color = if enabled { TEXT } else { FAINT };
    painter.text(egui::pos2(x, rect.top() + 4.0), egui::Align2::LEFT_TOP, p.label(), bold(12.5), name_color);
    painter.text(egui::pos2(x, rect.top() + 20.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(10.5), MUTED);
    if resp.hovered() && enabled {
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "Open ›",
            egui::FontId::proportional(11.5),
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

/// Does a screen line name `folder` (blank = home)? Case- and slash-insensitive.
fn same_folder(line: &str, folder: &str) -> bool {
    let home;
    let folder = if folder.trim().is_empty() {
        home = std::env::var("USERPROFILE").unwrap_or_default();
        home.as_str()
    } else {
        folder
    };
    let norm = |s: &str| s.trim().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    !folder.trim().is_empty() && norm(line) == norm(folder)
}

#[cfg(test)]
mod trust_tests {
    use super::same_folder;

    #[test]
    fn matches_only_the_agents_own_folder() {
        assert!(same_folder(r" C:\Users\User", r"C:\Users\User"));
        assert!(same_folder("c:/users/user/", r"C:\Users\User"));
        assert!(!same_folder(r" C:\Users\User\other", r"C:\Users\User"));
        assert!(!same_folder("Accessing workspace:", r"C:\Users\User"));
    }
}

/// Left/top edge of each grid track, given their fractions of `avail` space.
fn offsets(fracs: &[f32], avail: f32, gap: f32, start: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(fracs.len());
    let mut pos = start;
    for f in fracs {
        out.push(pos);
        pos += f * avail + gap;
    }
    out
}

/// Move the border between tracks `i` and `i + 1` by `delta` (a fraction of the
/// total), keeping both at least `MIN` of the space.
fn resize_pair(fracs: &mut [f32], i: usize, delta: f32) {
    const MIN: f32 = 0.12;
    let total = fracs[i] + fracs[i + 1];
    let a = (fracs[i] + delta).clamp(MIN, (total - MIN).max(MIN));
    fracs[i] = a;
    fracs[i + 1] = total - a;
}

/// "12.3k tok · ctx 34%", in reading order whichever way the layout runs.
fn pane_stats(ui: &mut egui::Ui, a: &Agent, context: Option<f32>, rtl: bool) {
    let mut items = vec![(RichText::new(format!("{} tok", fmt_tokens(a.session_usage.total_tokens()))).small().color(FAINT), None)];
    if let Some(p) = context {
        items.push((RichText::new("·").small().color(FAINT), None));
        items.push((
            RichText::new(format!("ctx {p:.0}%")).small().color(context_color(p)),
            Some("How full this conversation's context window is"),
        ));
    }
    if rtl {
        items.reverse();
    }
    for (text, tip) in items {
        let r = ui.add(egui::Label::new(text).selectable(false));
        if let Some(t) = tip {
            r.on_hover_text(t);
        }
    }
}

/// Second line of a sidebar card: "folder · model".
fn card_subtitle(a: &Agent) -> String {
    let cwd = a.cwd.trim().trim_end_matches(['\\', '/']);
    let folder = if cwd.is_empty() {
        "~".to_string()
    } else {
        std::path::Path::new(cwd).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_string())
    };
    let model = match a.provider {
        Provider::Claude if a.model.trim().is_empty() => "default model",
        Provider::Claude => a.model.trim(),
        Provider::OpenCode => "OpenCode",
    };
    format!("{folder} · {model}")
}

fn context_color(pct: f32) -> Color32 {
    if pct >= 85.0 {
        RED
    } else if pct >= 60.0 {
        AMBER
    } else {
        GREEN
    }
}

/// Hover card for an agent: where it runs and how.
fn agent_info(a: &Agent) -> String {
    let folder = if a.cwd.trim().is_empty() { "home folder" } else { a.cwd.trim() };
    let model = match a.provider {
        Provider::OpenCode => "picked in OpenCode (/models)",
        Provider::Claude if a.model.trim().is_empty() => "default",
        Provider::Claude => a.model.trim(),
    };
    let session = a.session_id.as_deref().map(|s| s.chars().take(13).collect::<String>() + "…").unwrap_or_else(|| "new".into());
    format!(
        "{}\nFolder: {folder}\nModel: {model}\nPermissions: {}\nSession: {session}",
        a.provider.label(),
        a.permission.label()
    )
}

/// Count of new output lines on an agent you're not looking at.
fn unread_badge(ui: &mut egui::Ui, n: usize) {
    if n == 0 {
        return;
    }
    let text = if n > 99 { "99+".to_string() } else { n.to_string() };
    let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(10.0), Color32::WHITE);
    let size = Vec2::new((galley.size().x + 8.0).max(16.0), 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 7.0, ACCENT.gamma_multiply(0.85));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp.on_hover_text(format!("{n} new line{} since you last looked", if n == 1 { "" } else { "s" }));
}

/// "42s", "2m 14s", "1h 5m".
fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn resize_keeps_total_and_minimum() {
        let mut f = vec![0.5, 0.5];
        resize_pair(&mut f, 0, 0.2);
        assert!((f[0] - 0.7).abs() < 1e-6 && (f[0] + f[1] - 1.0).abs() < 1e-6);
        resize_pair(&mut f, 0, 0.9);
        assert!((f[1] - 0.12).abs() < 1e-6);
    }

    #[test]
    fn offsets_include_gaps() {
        assert_eq!(offsets(&[0.5, 0.5], 100.0, 6.0, 10.0), vec![10.0, 66.0]);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_dur(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_dur(Duration::from_secs(134)), "2m 14s");
        assert_eq!(fmt_dur(Duration::from_secs(3900)), "1h 5m");
    }
}
