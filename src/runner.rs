//! Building launch commands for each provider's interactive TUI, and reading
//! token/cost usage back from the files each CLI writes.

use crate::agent::{data_dir, Agent, Permission, Provider, Settings, Usage};
use crate::term::SpawnSpec;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------- executable lookup ----------

/// Find the real executable for a CLI. Handles npm `.cmd` shims on Windows by
/// reading the `.exe` path out of the shim so we never go through cmd.exe.
pub fn resolve_exe(configured: &str, default_name: &str) -> Option<PathBuf> {
    let name = if configured.trim().is_empty() { default_name } else { configured.trim() };
    let p = Path::new(name);
    if p.components().count() > 1 || p.is_absolute() {
        return check_candidate(p);
    }
    let path_var = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    // Common install spots in case the app was launched with a thin PATH.
    if let Some(home) = std::env::var_os("USERPROFILE") {
        dirs.push(PathBuf::from(&home).join(".local").join("bin"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd"] } else { &[""] };
    for dir in dirs {
        for ext in exts {
            let file = if ext.is_empty() { name.to_string() } else { format!("{name}.{ext}") };
            if let Some(found) = check_candidate(&dir.join(file)) {
                return Some(found);
            }
        }
    }
    None
}

fn check_candidate(p: &Path) -> Option<PathBuf> {
    if !p.is_file() {
        return None;
    }
    let is_cmd = p
        .extension()
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);
    if is_cmd {
        parse_cmd_shim(p)
    } else {
        Some(p.to_path_buf())
    }
}

fn parse_cmd_shim(p: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(p).ok()?;
    let dir = p.parent()?.to_string_lossy().to_string();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let Some(end) = lower.find(".exe\"") else { continue };
        let start = line[..end].rfind('"')? + 1;
        let raw = &line[start..end + 4];
        let resolved = raw.replace("%dp0%", &dir).replace("%~dp0", &format!("{dir}\\"));
        let candidate = PathBuf::from(resolved);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn exe_for(provider: Provider, settings: &Settings) -> Option<PathBuf> {
    match provider {
        Provider::Claude => resolve_exe(&settings.claude_path, "claude"),
        Provider::OpenCode => resolve_exe(&settings.opencode_path, "opencode"),
    }
}

// ---------- ids ----------

fn rand_u64() -> u64 {
    std::collections::hash_map::RandomState::new().hash_one(SystemTime::now())
}

pub fn new_uuid() -> String {
    let a = rand_u64();
    let b = rand_u64();
    let a = (a & 0xFFFF_FFFF_FFFF_0FFF) | 0x0000_0000_0000_4000; // version 4
    let b = (b & 0x3FFF_FFFF_FFFF_FFFF) | 0x8000_0000_0000_0000; // variant
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xFFFF,
        a & 0xFFFF,
        b >> 48,
        b & 0xFFFF_FFFF_FFFF
    )
}

/// OpenCode-style session id: `ses_` + 12 hex (time, descending) + 14 base62.
pub fn new_opencode_session_id() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let time = !(ms.wrapping_mul(0x1000)) & 0xFFFF_FFFF_FFFF;
    const B62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut r = rand_u64() ^ rand_u64().rotate_left(17);
    let mut tail = String::new();
    for i in 0..14 {
        if i == 10 {
            r = rand_u64();
        }
        tail.push(B62[(r % 62) as usize] as char);
        r /= 62;
    }
    format!("ses_{time:012x}{tail}")
}

// ---------- launching ----------

pub struct Launch {
    pub spec: SpawnSpec,
    /// Session id this launch uses (store it on the agent).
    pub session_id: String,
    pub resumed: bool,
}

/// Build the command line that opens `agent` in its CLI's interactive TUI.
/// `self_exe` is this app's exe, used as Claude's status-line command.
pub fn launch(agent: &Agent, settings: &Settings, self_exe: &Path) -> Result<Launch, String> {
    let exe = exe_for(agent.provider, settings).ok_or_else(|| {
        format!("Could not find the {} CLI. Install it or set its path in Settings.", agent.provider.label())
    })?;
    let cwd = agent.cwd.trim();
    let cwd = if cwd.is_empty() {
        None
    } else if Path::new(cwd).is_dir() {
        Some(PathBuf::from(cwd))
    } else {
        return Err(format!("Folder does not exist: {cwd}"));
    };
    let model = agent.model.trim();
    let mut args = Vec::new();
    let env = HashMap::new();
    let (session_id, resumed);

    match agent.provider {
        Provider::Claude => {
            // Only resume if Claude actually wrote the conversation (it doesn't
            // until the first message is sent).
            match &agent.session_id {
                Some(sid) if claude_transcript(sid).is_some() => {
                    args.extend(["--resume".into(), sid.clone()]);
                    session_id = sid.clone();
                    resumed = true;
                }
                other => {
                    let sid = other.clone().unwrap_or_else(new_uuid);
                    args.extend(["--session-id".into(), sid.clone()]);
                    session_id = sid;
                    resumed = false;
                }
            }
            let settings_path = write_claude_settings(agent.id, self_exe).map_err(|e| format!("settings file: {e}"))?;
            args.extend(["--settings".into(), settings_path.to_string_lossy().into_owned()]);
            if !model.is_empty() {
                args.extend(["--model".into(), model.into()]);
            }
            match agent.permission {
                Permission::Safe => {}
                Permission::AcceptEdits => args.extend(["--permission-mode".into(), "acceptEdits".into()]),
                Permission::FullAuto => args.extend(["--permission-mode".into(), "bypassPermissions".into()]),
            }
            if !agent.system_prompt.trim().is_empty() {
                args.extend(["--append-system-prompt".into(), agent.system_prompt.trim().into()]);
            }
        }
        Provider::OpenCode => {
            resumed = agent.session_id.is_some();
            session_id = agent.session_id.clone().unwrap_or_else(new_opencode_session_id);
            args.extend(["--session".into(), session_id.clone()]);
            if agent.permission != Permission::Safe {
                args.push("--auto".into());
            }
            if !resumed && !agent.system_prompt.trim().is_empty() {
                args.extend(["--prompt".into(), agent.system_prompt.trim().into()]);
            }
            // No model flag: the OpenCode TUI uses the model last picked with /models.
            // (Config overrides only reach a --standalone server, which has no login.)
        }
    }
    Ok(Launch { spec: SpawnSpec { program: exe, args, cwd, env }, session_id, resumed })
}

fn write_claude_settings(agent_id: u64, self_exe: &Path) -> std::io::Result<PathBuf> {
    let dir = data_dir().join("claude");
    std::fs::create_dir_all(&dir)?;
    let exe = shell_neutral_path(self_exe);
    let settings = serde_json::json!({
        "statusLine": {
            "type": "command",
            // Unquoted on purpose: Claude may run this through PowerShell, bash or
            // cmd, and a bare space-free path is the only form all three accept.
            "command": format!("{exe} --statusline {agent_id}"),
            "padding": 0
        }
    });
    let path = dir.join(format!("settings-{agent_id}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(path)
}

/// A path usable unquoted in any shell: forward slashes, and the 8.3 short
/// form if the real path contains spaces.
fn shell_neutral_path(p: &Path) -> String {
    let mut s = p.to_string_lossy().into_owned();
    #[cfg(windows)]
    if s.contains(' ') {
        if let Some(short) = short_path(p) {
            s = short;
        }
    }
    s.replace('\\', "/")
}

#[cfg(windows)]
fn short_path(p: &Path) -> Option<String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated; `buf` is writable for `buf.len()` u16s.
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let s = std::ffi::OsString::from_wide(&buf[..n]).to_string_lossy().into_owned();
    (!s.contains(' ')).then_some(s)
}

// ---------- "waiting on you" detection ----------

/// Footers/labels that only appear while a TUI waits for the user to answer:
/// Claude's question menus and permission/trust prompts ("… · Esc to cancel"),
/// and OpenCode's permission prompt.
const ASK_MARKERS: &[&str] = &["Esc to cancel", "Do you want to proceed", "Permission required", "Allow once"];

/// Does this screen show a question or permission prompt?
pub fn screen_asks_user(screen: &str) -> bool {
    ASK_MARKERS.iter().any(|m| screen.contains(m))
}

// ---------- Claude status line helper ----------

pub fn live_file(agent_id: u64) -> PathBuf {
    data_dir().join("live").join(format!("{agent_id}.json"))
}

/// `kennel --statusline <id>`: Claude pipes its status JSON to us; we save it
/// for the app and print a short status line back for Claude to show.
pub fn statusline_main(agent_id: &str) {
    use std::io::Write;
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Ok(id) = agent_id.parse::<u64>() {
        let path = live_file(id);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &input).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let model = v["model"]["display_name"].as_str().unwrap_or("");
    let _ = write!(std::io::stdout(), "\x1b[2m{model} · Kennel\x1b[0m");
}

// ---------- usage tracking ----------

pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

pub fn claude_transcript(session_id: &str) -> Option<PathBuf> {
    let dir = claude_projects_dir()?;
    let file = format!("{session_id}.jsonl");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path().join(&file);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Tracks one Claude session's cumulative totals.
///
/// - The status line (while the TUI runs) reports `cost.total_cost_usd`.
/// - The transcript has per-message `usage`, plus `cost-state` records with the
///   session's running total cost and per-model token counts. Claude restores
///   that state on `--resume`, so both numbers are cumulative per session.
pub struct ClaudeTracker {
    agent_id: u64,
    session_id: String,
    path: Option<PathBuf>,
    offset: u64,
    per_message: HashMap<String, Usage>,
    /// Latest `cost-state` record (cost + token totals).
    state: Usage,
    status_cost: f64,
    /// How full the conversation's context window is (0-100), from the status line.
    pub context_pct: Option<f32>,
    last_live_mtime: Option<SystemTime>,
}

impl ClaudeTracker {
    pub fn new(agent_id: u64, session_id: &str) -> Self {
        let _ = std::fs::remove_file(live_file(agent_id));
        ClaudeTracker {
            agent_id,
            session_id: session_id.to_string(),
            path: None,
            offset: 0,
            per_message: HashMap::new(),
            state: Usage::default(),
            status_cost: 0.0,
            context_pct: None,
            last_live_mtime: None,
        }
    }

    /// Returns the session totals so far.
    pub fn poll(&mut self) -> Usage {
        self.read_transcript();
        self.read_status();
        let mut m = Usage::default();
        for u in self.per_message.values() {
            m.add(u);
        }
        let s = self.state;
        Usage {
            input: m.input.max(s.input),
            output: m.output.max(s.output),
            cache_read: m.cache_read.max(s.cache_read),
            cache_write: m.cache_write.max(s.cache_write),
            cost_usd: self.status_cost.max(s.cost_usd),
        }
    }

    fn read_transcript(&mut self) {
        if self.path.is_none() {
            self.path = claude_transcript(&self.session_id);
        }
        let Some(path) = &self.path else { return };
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.per_message.clear();
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        // Only consume complete lines.
        let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else { return };
        self.offset += last_nl as u64 + 1;
        for line in buf[..last_nl].split(|&b| b == b'\n') {
            match parse_claude_transcript_line(line) {
                Some(TranscriptUsage::Message(id, u)) => {
                    self.per_message.insert(id, u);
                }
                Some(TranscriptUsage::CostState(u)) => self.state = u,
                None => {}
            }
        }
    }

    fn read_status(&mut self) {
        let path = live_file(self.agent_id);
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let mtime = meta.modified().ok();
        if mtime.is_some() && mtime == self.last_live_mtime {
            return;
        }
        self.last_live_mtime = mtime;
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        if v["session_id"].as_str().is_some_and(|s| s != self.session_id) {
            return;
        }
        if let Some(p) = v["context_window"]["used_percentage"].as_f64() {
            self.context_pct = Some(p as f32);
        }
        if let Some(c) = v["cost"]["total_cost_usd"].as_f64() {
            self.status_cost = c;
        }
    }
}

pub enum TranscriptUsage {
    /// An assistant message's usage (lines repeat per content block; key by id).
    Message(String, Usage),
    /// A `cost-state` record: session totals so far.
    CostState(Usage),
}

pub fn parse_claude_transcript_line(line: &[u8]) -> Option<TranscriptUsage> {
    let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
    if !has(b"\"usage\"") && !has(b"cost-state") {
        return None;
    }
    let v: Value = serde_json::from_slice(line).ok()?;
    match v["type"].as_str()? {
        "assistant" => {
            let msg = &v["message"];
            let id = msg["id"].as_str()?.to_string();
            let u = &msg["usage"];
            Some(TranscriptUsage::Message(
                id,
                Usage {
                    input: u["input_tokens"].as_u64().unwrap_or(0),
                    output: u["output_tokens"].as_u64().unwrap_or(0),
                    cache_read: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                    cache_write: u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                    cost_usd: 0.0,
                },
            ))
        }
        "cost-state" => {
            let mut u = Usage { cost_usd: v["totalCostUSD"].as_f64().unwrap_or(0.0), ..Default::default() };
            for m in v["modelUsage"].as_object().into_iter().flat_map(|o| o.values()) {
                u.input += m["inputTokens"].as_u64().unwrap_or(0);
                u.output += m["outputTokens"].as_u64().unwrap_or(0);
                u.cache_read += m["cacheReadInputTokens"].as_u64().unwrap_or(0);
                u.cache_write += m["cacheCreationInputTokens"].as_u64().unwrap_or(0);
            }
            Some(TranscriptUsage::CostState(u))
        }
        _ => None,
    }
}

/// `opencode session export <id>` -> session totals. Blocking; run off the UI thread.
pub fn opencode_session_usage(exe: &Path, session_id: &str, cwd: Option<&Path>) -> Option<Usage> {
    let mut cmd = Command::new(exe);
    cmd.args(["session", "export", session_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let bytes = reader.join().ok()?;
    parse_opencode_export(&bytes)
}

pub fn parse_opencode_export(bytes: &[u8]) -> Option<Usage> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let info = &v["info"];
    let t = &info["tokens"];
    if t.is_null() {
        return None;
    }
    Some(Usage {
        input: t["input"].as_u64().unwrap_or(0),
        output: t["output"].as_u64().unwrap_or(0) + t["reasoning"].as_u64().unwrap_or(0),
        cache_read: t["cache"]["read"].as_u64().unwrap_or(0),
        cache_write: t["cache"]["write"].as_u64().unwrap_or(0),
        cost_usd: info["cost"].as_f64().unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_shape() {
        let u = new_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(new_uuid(), new_uuid());
    }

    #[test]
    fn opencode_id_shape() {
        let s = new_opencode_session_id();
        assert!(s.starts_with("ses_"));
        assert_eq!(s.len(), 4 + 12 + 14);
    }

    #[test]
    fn transcript_line() {
        let line = br#"{"type":"assistant","message":{"id":"msg_1","usage":{"input_tokens":3,"output_tokens":5,"cache_read_input_tokens":7,"cache_creation_input_tokens":11}}}"#;
        let Some(TranscriptUsage::Message(id, u)) = parse_claude_transcript_line(line) else { panic!() };
        assert_eq!(id, "msg_1");
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (3, 5, 7, 11));
        let cs = br#"{"type":"cost-state","totalCostUSD":0.5,"modelUsage":{"a":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4},"b":{"inputTokens":10,"outputTokens":0}}}"#;
        let Some(TranscriptUsage::CostState(u)) = parse_claude_transcript_line(cs) else { panic!() };
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write, u.cost_usd), (11, 2, 3, 4, 0.5));
        assert!(parse_claude_transcript_line(br#"{"type":"user","message":{}}"#).is_none());
    }

    #[test]
    fn detects_waiting_prompts() {
        // Captured from Claude Code 2.1.330 (AskUserQuestion menu).
        let question = "☐ Drink\nDo you prefer tea or coffee?\n❯ 1. Tea\n  2. Coffee\nEnter to select · ↑/↓ to navigate · Esc to cancel";
        assert!(screen_asks_user(question));
        let trust = "❯ No, exit\n  Yes, I trust this folder\nEnter to confirm · Esc to cancel";
        assert!(screen_asks_user(trust));
        // Normal idle prompt and a working screen are not questions.
        let idle = "● The command ran and printed:\n  kennel-probe\n────\n❯\n────\n  Haiku 5.5 · $0.004 · Kennel\n  ⏵⏵ auto mode on (shift+tab to cycle)";
        assert!(!screen_asks_user(idle));
        let working = "✻ Thinking… (3s · esc to interrupt)";
        assert!(!screen_asks_user(working));
    }

    #[test]
    fn opencode_export() {
        let json = br#"{"info":{"cost":0.25,"tokens":{"input":10,"output":2,"reasoning":3,"cache":{"read":4,"write":1}}},"messages":[]}"#;
        let u = parse_opencode_export(json).unwrap();
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (10, 5, 4, 1));
        assert_eq!(u.cost_usd, 0.25);
    }
}
