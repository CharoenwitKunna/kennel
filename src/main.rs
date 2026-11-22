// GUI app: never open a console window (dev builds included).
#![windows_subsystem = "windows"]

mod agent;
mod icon;
#[cfg(test)]
mod live_tests;
mod runner;
mod status;
mod term;
mod term_view;
mod theme;
mod ui;
mod update;
mod usage;

use agent::{Agent, Permission, Provider, SavedView, State, Usage, WindowLayout};
use eframe::egui::{self, Align, Color32, CornerRadius, Key, Layout, Margin, RichText, Sense, Stroke, Vec2};
use status::Status;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use term::TermSession;
use theme::*;
use ui::widgets::*;
use usage::ClaudeTracker;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--statusline") {
        runner::statusline_main(args.get(2).map(String::as_str).unwrap_or(""));
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--session-hook") {
        runner::session_hook_main(args.get(2).map(String::as_str).unwrap_or(""));
        return Ok(());
    }
    term::scrub_parent_session_env();
    if let Ok(exe) = std::env::current_exe() {
        update::cleanup(&exe);
    }
    let state = State::load();
    let w = state.layout.width.unwrap_or(1040.0).clamp(720.0, 4096.0);
    let h = state.layout.height.unwrap_or(620.0).clamp(440.0, 2160.0);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Kennel")
        .with_icon(egui::IconData { rgba: icon::icon_rgba(64), width: 64, height: 64 })
        .with_inner_size([w, h])
        .with_min_inner_size([720.0, 440.0])
        // Our toolbar is the title bar (drag, double-click, window buttons).
        .with_decorations(false);
    if let (Some(x), Some(y)) = (state.layout.x, state.layout.y) {
        if x.is_finite() && y.is_finite() && (-3840.0..=7680.0).contains(&x) && (-2160.0..=4320.0).contains(&y) {
            viewport = viewport.with_position([x, y]);
        }
    }
    if state.layout.maximized {
        viewport = viewport.with_maximized(true);
    }
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, ..Default::default() };
    eframe::run_native("Kennel", options, Box::new(move |cc| Ok(Box::new(App::new(cc, state)))))
}

// ---------- runtime state ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Focus,
    Grid,
}

impl From<View> for SavedView {
    fn from(v: View) -> Self {
        match v {
            View::Focus => SavedView::Focus,
            View::Grid => SavedView::Grid,
        }
    }
}

struct Live {
    sess: TermSession,
    /// Incremental scrollback search bar state for this terminal.
    search: term_view::TermSearch,
    /// Screen shows a question / permission prompt (refreshed by `scan_prompts`).
    asking: bool,
    last_scan: Instant,
    /// Last check of this Claude agent's running subagents.
    last_sub_scan: Instant,
    /// The user has typed into this agent (gates "needs you").
    interacted: bool,
    claude: Option<ClaudeTracker>,
    last_poll: Instant,
    poll_inflight: bool,
    last_export: Option<Instant>,
    /// Model, thinking and context as the harness last reported them.
    info: usage::StatusInfo,
    started: Instant,
    /// Resumed Claude conversation: auto-answer its trust prompt for this folder.
    pending_trust: Option<String>,
    /// `line_mark` when you last had this agent on screen (unread = new lines since).
    seen_mark: usize,
    /// Status history: current status, since when, and change events.
    track: status::Tracker,
}

enum Bg {
    UpdateCheck { result: Result<Option<update::Release>, String>, manual: bool },
    UpdateInstalled(Result<String, String>),
    OpenCodeUsage { agent_id: u64, session_id: String, usage: Option<(Usage, usage::StatusInfo)> },
}

/// Where the self-updater is.
#[derive(Clone, Debug, PartialEq)]
enum UpdateState {
    Idle,
    Checking,
    Available(update::Release),
    Installing(String),
    /// Installed this version; restart to run it.
    Ready(String),
}

/// How often to look for a new release while running.
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);

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
    /// Each running agent's status, computed once per frame in `tick`.
    statuses: HashMap<u64, Status>,
    /// The Thai fallback font has been loaded.
    thai_font: bool,
    /// Cached CLI resolution for the Settings window: (claude_input, opencode_input, claude_exe, opencode_exe).
    cli_cache: Option<(String, String, Option<PathBuf>, Option<PathBuf>)>,
    update: UpdateState,
    last_update_check: Option<Instant>,
}

/// Length of the fade when switching views or agents.
const FADE: f32 = 0.16;

fn term_id(agent_id: u64) -> egui::Id {
    egui::Id::unique(("term", agent_id))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, state: State) -> Self {
        let thai_font = state.agents.iter().any(|a| has_thai(&a.name));
        install_fonts(&cc.egui_ctx, thai_font);
        apply_style(&cc.egui_ctx);
        // Reserve Ctrl+=/Ctrl+-/Ctrl+0 for terminal font size rather than whole-UI zoom.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let (bg_tx, bg_rx) = channel();
        let selected = state.agents.first().map(|a| a.id);
        let new_folder = state.settings.last_folder.clone();
        // Always open in Focus view (Grid is one click / Ctrl+Shift+G away).
        let view = View::Focus;
        let grid_cols = if WindowLayout::valid_tracks(&state.layout.grid_cols) {
            state.layout.grid_cols.clone()
        } else {
            Vec::new()
        };
        let grid_rows = if WindowLayout::valid_tracks(&state.layout.grid_rows) {
            state.layout.grid_rows.clone()
        } else {
            Vec::new()
        };
        App {
            state,
            live: HashMap::new(),
            selected,
            view,
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
            grid_cols,
            grid_rows,
            view_key: (view, selected),
            view_changed: Instant::now() - Duration::from_secs(1),
            statuses: HashMap::new(),
            thai_font,
            cli_cache: None,
            update: UpdateState::Idle,
            last_update_check: None,
        }
    }

    /// Load the Thai font the first time Thai text appears (terminal or typed).
    fn ensure_thai_font(&mut self, ctx: &egui::Context) {
        if self.thai_font {
            return;
        }
        let typed = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if has_thai(t))));
        if typed || WANT_THAI.load(std::sync::atomic::Ordering::Relaxed) {
            self.thai_font = true;
            install_fonts(ctx, true);
            // Cached terminal frames hold glyphs from the old fonts.
            for l in self.live.values_mut() {
                l.sess.paint_cache = None;
            }
            ctx.request_repaint();
        }
    }

    /// New output lines since you last had this agent on screen.
    fn unread(&self, id: u64) -> usize {
        self.live.get(&id).map_or(0, |l| l.sess.line_mark().saturating_sub(l.seen_mark))
    }

    /// How long the agent has been in its current status.
    fn status_age(&self, id: u64) -> Option<Duration> {
        self.live.get(&id).map(|l| l.track.since.elapsed())
    }

    /// Ask GitHub for a newer release in the background.
    fn check_for_update(&mut self, ctx: &egui::Context, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Installing(_)) {
            return;
        }
        self.update = UpdateState::Checking;
        self.last_update_check = Some(Instant::now());
        let (tx, ctx) = (self.bg_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Bg::UpdateCheck { result: update::check(), manual });
            ctx.request_repaint();
        });
    }

    /// Download and swap in the available release.
    fn install_update(&mut self, ctx: &egui::Context) {
        let UpdateState::Available(r) = self.update.clone() else { return };
        self.update = UpdateState::Installing(r.version.clone());
        let (tx, ctx, exe) = (self.bg_tx.clone(), ctx.clone(), self.self_exe.clone());
        std::thread::spawn(move || {
            let result = update::install(&r, &exe).map(|_| r.version.clone());
            let _ = tx.send(Bg::UpdateInstalled(result));
            ctx.request_repaint();
        });
    }

    /// Close (saving which agents were running) and start the new version.
    fn restart_for_update(&mut self, ctx: &egui::Context) {
        match update::relaunch_after_exit(&self.self_exe) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.toast(format!("Couldn't restart: {e}. Close and reopen Kennel.")),
        }
    }

    /// Hover text for an agent, with live model/context info while it runs.
    fn info_text(&self, a: &Agent) -> String {
        let l = self.live.get(&a.id);
        let pct = l.and_then(|l| l.claude.as_ref()).and_then(|t| t.context_pct);
        ui::widgets::agent_info(a, l.map(|l| &l.info), pct)
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toasts.push((Instant::now(), msg.into()));
    }

    /// This frame's status (cached by `tick`; computed if the agent changed since).
    fn status(&self, id: u64) -> Status {
        match self.statuses.get(&id) {
            Some(s) if self.live.contains_key(&id) => *s,
            _ => self.compute_status(id),
        }
    }

    fn compute_status(&self, id: u64) -> Status {
        let Some(l) = self.live.get(&id) else { return Status::Stopped };
        status::classify(l.sess.exited(), l.sess.quiet_for().is_some(), l.sess.busy(), l.asking)
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
            // Prompts sit at the bottom of the screen; no need to read all of it.
            l.asking = idle && runner::screen_asks_user(&l.sess.tail_text(15));
        }
    }

    // ---------- lifecycle ----------

    fn start(&mut self, id: u64, ctx: &egui::Context) {
        if self.status(id).alive() {
            return;
        }
        self.live.remove(&id);
        self.statuses.remove(&id);
        let Some(agent) = self.state.agent(id).cloned() else { return };
        let launch = match runner::launch(&agent, &self.state.settings, &self.self_exe) {
            Ok(l) => l,
            Err(e) => return self.toast(format!("{}: {e}", agent.name)),
        };
        let scrollback = self.state.settings.scrollback;
        let sess = match TermSession::spawn(launch.spec, 120, 32, scrollback, ctx.clone()) {
            Ok(s) => s,
            Err(e) => return self.toast(format!("Could not start {}: {e}", agent.name)),
        };
        let auto_trust = self.state.settings.auto_trust_on_resume;
        let a = self.state.agent_mut(id).unwrap();
        a.session_id = Some(launch.session_id.clone());
        a.runs += 1;
        let claude = (a.provider == Provider::Claude).then(|| ClaudeTracker::new(id, &launch.session_id));
        let pending_trust = (a.provider == Provider::Claude && launch.resumed && auto_trust).then(|| a.cwd.clone());
        self.live.insert(
            id,
            Live {
                sess,
                search: term_view::TermSearch::default(),
                asking: false,
                last_scan: Instant::now(),
                last_sub_scan: Instant::now() - Duration::from_secs(10),
                interacted: false,
                claude,
                last_poll: Instant::now(),
                poll_inflight: false,
                last_export: None,
                info: Default::default(),
                started: Instant::now(),
                pending_trust,
                seen_mark: 0,
                track: status::Tracker::new(Instant::now()),
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
        self.statuses.remove(&id);
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
                    l.info = t.info.clone();
                    let a = self.state.agent_mut(id).unwrap();
                    if a.session_id.as_deref() != Some(t.session_id()) {
                        // Resume the new session next launch.
                        a.session_id = Some(t.session_id().to_string());
                        self.dirty = true;
                    }
                    if a.session_usage != u {
                        a.session_usage = u;
                        self.dirty = true;
                    }
                }
            }
            Provider::OpenCode => {
                // An export is a whole-session dump: often while working, rarely when idle.
                let every = if l.track.shown == Status::Working { 15 } else { 60 };
                let due = force || l.last_export.is_none_or(|t| t.elapsed() > Duration::from_secs(every));
                let (Some(sid), false, true) = (sid, l.poll_inflight, due) else { return };
                let Some(exe) = runner::exe_for(provider, &self.state.settings) else { return };
                l.poll_inflight = true;
                l.last_export = Some(Instant::now());
                let tx = self.bg_tx.clone();
                std::thread::spawn(move || {
                    let usage = usage::opencode_session_usage(&exe, &sid, cwd.as_deref());
                    let _ = tx.send(Bg::OpenCodeUsage { agent_id: id, session_id: sid, usage });
                });
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                Bg::OpenCodeUsage { agent_id, session_id, usage } => {
                    let current =
                        self.state.agent(agent_id).is_some_and(|a| a.session_id.as_deref() == Some(&session_id));
                    if let Some(l) = self.live.get_mut(&agent_id) {
                        l.poll_inflight = false;
                        if let (true, Some((_, info))) = (current, &usage) {
                            l.info = info.clone();
                        }
                    }
                    if let (Some(a), Some((u, _)), true) = (self.state.agent_mut(agent_id), usage, current) {
                        if a.session_usage != u {
                            a.session_usage = u;
                            self.dirty = true;
                        }
                    }
                }
                Bg::UpdateCheck { result, manual } => {
                    self.update = match result {
                        Ok(Some(r)) => {
                            self.toast(format!("Kennel {} is available. Use the Update button at the top.", r.version));
                            UpdateState::Available(r)
                        }
                        Ok(None) => {
                            if manual {
                                self.toast(format!("Kennel {} is the latest version", update::CURRENT));
                            }
                            UpdateState::Idle
                        }
                        Err(e) => {
                            if manual {
                                self.toast(format!("Couldn't check for updates: {e}"));
                            }
                            UpdateState::Idle
                        }
                    };
                }
                Bg::UpdateInstalled(result) => match result {
                    Ok(v) => {
                        self.toast(format!("Kennel {v} installed. Restart to use it."));
                        self.update = UpdateState::Ready(v);
                    }
                    Err(e) => {
                        self.toast(format!("Update failed: {e}"));
                        self.update = UpdateState::Idle;
                        self.last_update_check = None;
                    }
                },
            }
        }
        if self.state.settings.check_updates
            && self.update == UpdateState::Idle
            && self.last_update_check.is_none_or(|t| t.elapsed() > UPDATE_EVERY)
        {
            self.check_for_update(ctx, false);
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
        let now = Instant::now();
        self.statuses.clear();
        for id in ids {
            let status = self.compute_status(id);
            self.statuses.insert(id, status);
            let focused = focused_term == Some(term_id(id));
            let l = self.live.get_mut(&id).unwrap();
            let shown = visible.contains(&id);
            l.sess.set_visible(shown);
            if shown {
                l.seen_mark = l.sess.line_mark();
            }
            let ev = l.track.update(status, now);
            let bell = l.sess.take_bell();
            if ev.wants_attention(l.interacted, bell) && !focused {
                self.attention.insert(id);
            }
            if focused {
                self.attention.remove(&id);
            }
            if status.alive() && l.last_sub_scan.elapsed() > Duration::from_secs(2) {
                l.last_sub_scan = now;
                if let Some(t) = l.claude.as_mut() {
                    t.scan_subagents();
                }
            }
            let claude = l.claude.is_some();
            if ev.finished_turn || ev.exited {
                self.poll_usage(id, true);
            } else if status == Status::Working || (claude && status.alive()) {
                // Also while idle: /resume and /clear switch sessions without a turn.
                self.poll_usage(id, false);
            }
        }
    }

    /// When the next frame is needed if nothing else asks for one. Status
    /// changes (working -> ready, questions) happen without new output, so
    /// keep checking briefly after activity, and rarely otherwise.
    fn next_tick(&self) -> Option<Duration> {
        let settling = self.live.values().any(|l| {
            l.sess.exited().is_none()
                && (l.pending_trust.is_some() || l.sess.quiet_for().is_none_or(|d| d < Duration::from_secs(3)))
        });
        if settling {
            Some(Duration::from_millis(250))
        } else if !self.live.is_empty() {
            Some(Duration::from_secs(5))
        } else if self.dirty {
            Some(Duration::from_secs(2))
        } else {
            None
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

    /// Record window bounds, maximized state, active view, and grid splits for persistence.
    fn track_window_layout(&mut self, ctx: &egui::Context) {
        let (maximized, minimized, inner_rect, outer_rect) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.maximized.unwrap_or(false), vp.minimized.unwrap_or(false), vp.inner_rect, vp.outer_rect)
        });
        let layout = &mut self.state.layout;
        if layout.maximized != maximized {
            layout.maximized = maximized;
            self.dirty = true;
        }
        if !maximized && !minimized {
            if let Some(r) = inner_rect {
                let w = r.width().round();
                let h = r.height().round();
                if w >= 720.0 && h >= 440.0 && (layout.width != Some(w) || layout.height != Some(h)) {
                    layout.width = Some(w);
                    layout.height = Some(h);
                    self.dirty = true;
                }
            }
            if let Some(r) = outer_rect {
                let x = r.min.x.round();
                let y = r.min.y.round();
                if x.is_finite()
                    && y.is_finite()
                    && (-3840.0..=7680.0).contains(&x)
                    && (-2160.0..=4320.0).contains(&y)
                    && (layout.x != Some(x) || layout.y != Some(y))
                {
                    layout.x = Some(x);
                    layout.y = Some(y);
                    self.dirty = true;
                }
            }
        }
        let saved_view = SavedView::from(self.view);
        if layout.view != saved_view {
            layout.view = saved_view;
            self.dirty = true;
        }
        if WindowLayout::valid_tracks(&self.grid_cols) && layout.grid_cols != self.grid_cols {
            layout.grid_cols = self.grid_cols.clone();
            self.dirty = true;
        }
        if WindowLayout::valid_tracks(&self.grid_rows) && layout.grid_rows != self.grid_rows {
            layout.grid_rows = self.grid_rows.clone();
            self.dirty = true;
        }
    }
}

/// `KENNEL_SCREENSHOT=out.png`: after `KENNEL_SCREENSHOT_DELAY` seconds (default
/// 2), save what was actually rendered and exit. For checking rendering (e.g.
/// after a GUI library upgrade); point APPDATA at a scratch folder first, since
/// it starts the agents saved there.
fn screenshot_for_testing(ctx: &egui::Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    let Some(path) = std::env::var_os("KENNEL_SCREENSHOT") else { return };
    let delay: f32 = std::env::var("KENNEL_SCREENSHOT_DELAY").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0);
    ctx.request_repaint();
    if START.get_or_init(Instant::now).elapsed().as_secs_f32() >= delay && !REQUESTED.swap(true, Ordering::Relaxed) {
        ctx.request_screenshot(move |img| {
            let [w, h] = img.size;
            let ok = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()).map(|i| i.save(&path));
            std::process::exit(if matches!(ok, Some(Ok(()))) { 0 } else { 1 });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        screenshot_for_testing(ctx);
        if !self.autostarted {
            self.autostarted = true;
            let ids: Vec<u64> = self.state.agents.iter().filter(|a| a.resume_on_launch).map(|a| a.id).collect();
            for id in ids {
                self.actions.push(Action::Start(id));
            }
        }
        self.ensure_thai_font(ctx);
        self.tick(ctx);
        self.shortcuts(ctx);
        self.toolbar(ui);
        self.sidebar(ui);
        self.central(ui);
        self.editor_window(ctx);
        self.settings_window(ctx);
        self.delete_window(ctx);
        self.toasts(ctx);
        window_edges(ctx);
        self.apply_actions(ctx);
        self.track_window_layout(ctx);
        self.maybe_save(false);
        if let Some(d) = self.next_tick() {
            ctx.request_repaint_after(d);
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
