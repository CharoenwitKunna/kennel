//! A real pseudo-terminal (ConPTY on Windows) running one agent's interactive TUI,
//! emulated by `alacritty_terminal`.

use alacritty_terminal::event::{Event as TEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use eframe::egui;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default scrollback per terminal (Settings can change it for new terminals).
pub const SCROLLBACK: usize = 3000;

/// What a terminal's last painted frame depended on. Same key = same picture,
/// so the shapes from last time are reused instead of walking every cell.
#[derive(Clone, PartialEq)]
pub struct PaintKey {
    pub generation: u64,
    pub rect: egui::Rect,
    pub cell: (f32, f32),
    pub font_size: f32,
    pub focused: bool,
    pub display_offset: usize,
    pub selection: Option<alacritty_terminal::selection::SelectionRange>,
}

#[derive(Clone, Copy)]
pub struct Size {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// State shared between the PTY reader thread and the UI.
pub struct Shared {
    ctx: egui::Context,
    start: Instant,
    /// ms since `start` of the last PTY output (0 = none yet)
    last_output_ms: AtomicU64,
    /// Recent quarter-second slots (ms / 250) that had output, oldest first.
    activity: Mutex<VecDeque<u64>>,
    exited: AtomicBool,
    exit_code: AtomicI32,
    pub bell: AtomicBool,
    pub title: Mutex<String>,
    writer: OnceLock<EventLoopSender>,
    window_size: Mutex<WindowSize>,
    /// Bumped on every chunk of output (invalidates the paint cache).
    generation: AtomicU64,
    /// On screen right now? Hidden terminals don't force full-speed redraws.
    visible: AtomicBool,
}

impl Shared {
    fn reply(&self, s: String) {
        if let Some(w) = self.writer.get() {
            let _ = w.send(Msg::Input(Cow::Owned(s.into_bytes())));
        }
    }
}

#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, ev: TEvent) {
        let s = &self.0;
        match ev {
            TEvent::Wakeup => {
                let ms = s.start.elapsed().as_millis() as u64;
                s.last_output_ms.store(ms.max(1), Ordering::Relaxed);
                record_activity(&mut s.activity.lock().unwrap(), ms / SLOT_MS);
                s.generation.fetch_add(1, Ordering::Relaxed);
                if s.visible.load(Ordering::Relaxed) {
                    s.ctx.request_repaint();
                } else {
                    // Only badges / status change for a hidden terminal.
                    s.ctx.request_repaint_after(HIDDEN_REPAINT);
                }
            }
            TEvent::PtyWrite(text) => s.reply(text),
            TEvent::Title(t) => *s.title.lock().unwrap() = t,
            TEvent::ResetTitle => s.title.lock().unwrap().clear(),
            TEvent::Bell => {
                s.bell.store(true, Ordering::Relaxed);
                s.ctx.request_repaint();
            }
            TEvent::ClipboardStore(_, text) => s.ctx.copy_text(text),
            TEvent::ColorRequest(idx, fmt) => s.reply(fmt(crate::theme::term_color_rgb(idx))),
            TEvent::TextAreaSizeRequest(fmt) => {
                let ws = *s.window_size.lock().unwrap();
                s.reply(fmt(ws));
            }
            TEvent::ChildExit(code) => {
                s.exit_code.store(code, Ordering::Relaxed);
                s.exited.store(true, Ordering::Relaxed);
                s.ctx.request_repaint();
            }
            TEvent::Exit => {
                s.exited.store(true, Ordering::Relaxed);
                s.ctx.request_repaint();
            }
            _ => {}
        }
    }
}

pub struct TermSession {
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub shared: Arc<Shared>,
    tx: EventLoopSender,
    pub pid: Option<u32>,
    size: Size,
    /// Last painted frame and what it depended on (see `PaintKey`).
    pub paint_cache: Option<(PaintKey, egui::Shape)>,
}

pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

impl TermSession {
    pub fn spawn(spec: SpawnSpec, cols: usize, rows: usize, scrollback: usize, ctx: egui::Context) -> std::io::Result<Self> {
        let size = Size { cols: cols.max(20), rows: rows.max(5) };
        let ws = window_size(size, 8.0, 16.0);
        let mut env = spec.env;
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        // Identify as ourselves, not whatever terminal launched Kennel.
        env.insert("TERM_PROGRAM".into(), "Kennel".into());
        env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into());
        let opts = tty::Options {
            shell: Some(tty::Shell::new(spec.program.to_string_lossy().into_owned(), spec.args)),
            working_directory: spec.cwd,
            drain_on_exit: true,
            env,
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&opts, ws, 0)?;
        #[cfg(windows)]
        let pid = pty.child_watcher().pid().map(|p| p.get());
        #[cfg(not(windows))]
        let pid = Some(pty.child().id());

        let shared = Arc::new(Shared {
            ctx,
            start: Instant::now(),
            last_output_ms: AtomicU64::new(0),
            activity: Mutex::new(VecDeque::new()),
            exited: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            bell: AtomicBool::new(false),
            title: Mutex::new(String::new()),
            writer: OnceLock::new(),
            window_size: Mutex::new(ws),
            generation: AtomicU64::new(0),
            visible: AtomicBool::new(true),
        });
        let listener = Listener(shared.clone());
        let config = Config { scrolling_history: scrollback, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let tx = event_loop.channel();
        let _ = shared.writer.set(tx.clone());
        event_loop.spawn();
        Ok(TermSession { term, shared, tx, pid, size, paint_cache: None })
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    pub fn set_visible(&self, v: bool) {
        self.shared.visible.store(v, Ordering::Relaxed);
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.tx.send(Msg::Input(Cow::Owned(bytes.into())));
    }

    /// Paste text the way a terminal does (bracketed if the app asked for it).
    pub fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            let clean = text.replace('\x1b', "");
            self.write(format!("\x1b[200~{clean}\x1b[201~"));
        } else {
            self.write(text);
        }
    }

    #[cfg(test)] // used by the live tests
    /// Paste `text` and press Enter a moment later, so TUIs see a separate keypress.
    pub fn submit(&self, text: &str) {
        self.paste(text);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            let _ = tx.send(Msg::Input(Cow::Borrowed(b"\r")));
        });
    }

    /// Send bytes after a short delay (lets a TUI react to a previous key).
    pub fn write_after(&self, bytes: &'static [u8], delay: Duration) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(Msg::Input(Cow::Borrowed(bytes)));
        });
    }

    pub fn scroll_to_bottom(&self) {
        let mut t = self.term.lock();
        if t.grid().display_offset() != 0 {
            t.scroll_display(Scroll::Bottom);
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: f32, cell_h: f32) {
        let cols = cols.max(20);
        let rows = rows.max(5);
        if cols == self.size.cols && rows == self.size.rows {
            return;
        }
        self.size = Size { cols, rows };
        let ws = window_size(self.size, cell_w, cell_h);
        *self.shared.window_size.lock().unwrap() = ws;
        self.term.lock().resize(self.size);
        let _ = self.tx.send(Msg::Resize(ws));
    }

    pub fn exited(&self) -> Option<i32> {
        self.shared
            .exited
            .load(Ordering::Relaxed)
            .then(|| self.shared.exit_code.load(Ordering::Relaxed))
    }

    /// Time since the last output, or None if there was none yet.
    pub fn quiet_for(&self) -> Option<Duration> {
        let ms = self.shared.last_output_ms.load(Ordering::Relaxed);
        if ms == 0 {
            return None;
        }
        let now = self.shared.start.elapsed().as_millis() as u64;
        Some(Duration::from_millis(now.saturating_sub(ms)))
    }

    /// Busy = sustained output (a working TUI redraws its spinner many times a
    /// second). Idle TUIs that blink a cursor or rotate a hint every couple of
    /// seconds don't count.
    pub fn busy(&self) -> bool {
        let now = self.shared.start.elapsed().as_millis() as u64 / SLOT_MS;
        is_busy(&self.shared.activity.lock().unwrap(), now)
    }

    /// Running count of lines this terminal has produced (scrollback plus the
    /// cursor's row). Differences between two readings = new output lines.
    pub fn line_mark(&self) -> usize {
        let t = self.term.lock();
        let g = t.grid();
        g.history_size() + g.cursor.point.line.0.max(0) as usize
    }

    pub fn take_bell(&self) -> bool {
        self.shared.bell.swap(false, Ordering::Relaxed)
    }

    /// Plain text of the bottom `n` lines of the live screen (where TUIs put
    /// their prompts). Cheaper than `screen_text` for frequent checks.
    pub fn tail_text(&self, n: usize) -> String {
        let t = self.term.lock();
        let g = t.grid();
        let rows = g.screen_lines();
        let cols = g.columns();
        let mut out = String::with_capacity(n.min(rows) * (cols + 1));
        for r in rows.saturating_sub(n)..rows {
            let row = &g[Line(r as i32)];
            out.extend((0..cols).map(|c| row[Column(c)].c));
            out.push('\n');
        }
        out
    }

    /// Plain text of the visible screen.
    pub fn screen_text(&self) -> String {
        let t = self.term.lock();
        let content = t.renderable_content();
        let mut out = String::new();
        let mut line = None;
        for cell in content.display_iter {
            if line != Some(cell.point.line) {
                if line.is_some() {
                    out.push('\n');
                }
                line = Some(cell.point.line);
            }
            out.push(cell.c);
        }
        out
    }

    pub fn kill(&self) {
        let already_exited = self.exited().is_some();
        let _ = self.tx.send(Msg::Shutdown);
        // Only kill while the child is known alive, so a recycled PID is never hit.
        if let (Some(pid), false) = (self.pid, already_exited) {
            kill_tree(pid);
        }
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        self.kill();
    }
}

fn window_size(s: Size, cw: f32, ch: f32) -> WindowSize {
    WindowSize {
        num_lines: s.rows as u16,
        num_cols: s.cols as u16,
        cell_width: cw.round().max(1.0) as u16,
        cell_height: ch.round().max(1.0) as u16,
    }
}

/// If Kennel itself was started from inside a Claude Code session, these
/// variables would make every agent think it is a nested child session (which,
/// among other things, turns off transcript saving and breaks resume).
/// Children inherit our environment, so remove them from it once at startup.
pub fn scrub_parent_session_env() {
    const MARKERS: &[&str] = &[
        "CLAUDECODE",
        "CLAUDE_PID",
        "CLAUDE_EFFORT",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_CHILD_NOTES_DIR",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        // Kennel draws its own colors; a NO_COLOR meant for the parent shell
        // would make every agent TUI monochrome.
        "NO_COLOR",
    ];
    for k in MARKERS {
        std::env::remove_var(k);
    }
}

/// Kill a process and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

/// How soon a hidden terminal's output is reflected (badges, status).
const HIDDEN_REPAINT: Duration = Duration::from_millis(400);

/// Activity is tracked in slots of this many ms.
const SLOT_MS: u64 = 250;
/// Window looked at, in slots (2 s).
const WINDOW_SLOTS: u64 = 8;
/// Active slots within the window needed to count as busy (1 s of output).
const BUSY_SLOTS: usize = 4;

fn record_activity(slots: &mut VecDeque<u64>, slot: u64) {
    if slots.back() != Some(&slot) {
        slots.push_back(slot);
    }
    while slots.front().is_some_and(|&s| s + WINDOW_SLOTS < slot) {
        slots.pop_front();
    }
}

fn is_busy(slots: &VecDeque<u64>, now: u64) -> bool {
    slots.iter().filter(|&&s| s + WINDOW_SLOTS > now).count() >= BUSY_SLOTS
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    fn slots(list: &[u64]) -> VecDeque<u64> {
        let mut d = VecDeque::new();
        for &s in list {
            record_activity(&mut d, s);
        }
        d
    }

    #[test]
    fn spinner_is_busy() {
        // Output in every slot for the last second.
        assert!(is_busy(&slots(&[10, 11, 12, 13, 14]), 14));
    }

    #[test]
    fn occasional_blips_are_idle() {
        // A cursor blink / hint rotation every ~2.5 s.
        assert!(!is_busy(&slots(&[0, 10, 20, 30]), 30));
        // A one-off redraw burst (e.g. after a resize).
        assert!(!is_busy(&slots(&[29, 30]), 30));
    }

    #[test]
    fn goes_idle_after_output_stops() {
        let s = slots(&[10, 11, 12, 13, 14, 15]);
        assert!(is_busy(&s, 15));
        assert!(!is_busy(&s, 22));
    }

    #[test]
    fn repeated_output_in_one_slot_counts_once() {
        assert_eq!(slots(&[5, 5, 5, 6]).len(), 2);
    }
}
