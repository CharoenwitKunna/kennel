//! Token usage for each provider: Claude's transcript and status line, and
//! OpenCode's session export.

use crate::agent::Usage;
use crate::runner::{live_file, session_file};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
    /// Model, thinking and context details from the status line.
    pub info: StatusInfo,
    last_live_mtime: Option<SystemTime>,
    last_hook_mtime: Option<SystemTime>,
    /// Session the SessionStart hook last reported; it outranks the status line.
    hook_session: Option<String>,
    /// When this run began: older subagent transcripts belong to earlier runs.
    started: SystemTime,
    /// Subagent ids already seen finished (their meta isn't re-read).
    finished_subagents: std::collections::HashSet<String>,
    /// Subagents this session is running now, oldest first.
    pub subagents: Vec<Subagent>,
}

/// What the status line says about the running session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusInfo {
    /// e.g. "Opus 5.5".
    pub model: Option<String>,
    pub thinking: Option<bool>,
    /// Effort level, e.g. "medium".
    pub effort: Option<String>,
    /// Tokens in the context window now.
    pub context: Option<u64>,
    /// Size of the context window, when the harness reports it.
    pub context_size: Option<u64>,
}

impl StatusInfo {
    pub fn from_status(v: &Value) -> Self {
        let model = v["model"]["display_name"].as_str().or(v["model"]["id"].as_str()).map(str::to_string);
        let cw = &v["context_window"];
        let cu = &cw["current_usage"];
        let used: u64 = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
            .iter()
            .map(|k| cu[*k].as_u64().unwrap_or(0))
            .sum();
        StatusInfo {
            model,
            thinking: v["thinking"]["enabled"].as_bool(),
            effort: v["effort"]["level"].as_str().map(str::to_string),
            context: cu.is_object().then_some(used),
            context_size: cw["context_window_size"].as_u64(),
        }
    }

    /// From `opencode session export`: the session's model and variant, and the
    /// last reply's prompt size (what the context holds now).
    pub fn from_opencode_export(v: &Value) -> Self {
        let m = &v["info"]["model"];
        let model = m["id"].as_str().map(|id| match m["providerID"].as_str() {
            Some(p) if !p.is_empty() => format!("{p}/{id}"),
            _ => id.to_string(),
        });
        let last = v["messages"]
            .as_array()
            .and_then(|ms| ms.iter().rev().find(|x| x["type"] == "assistant" && x["tokens"].is_object()));
        let context = last.map(|x| {
            let t = &x["tokens"];
            t["input"].as_u64().unwrap_or(0)
                + t["cache"]["read"].as_u64().unwrap_or(0)
                + t["cache"]["write"].as_u64().unwrap_or(0)
        });
        let effort = m["variant"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let reasoned = last.is_some_and(|x| x["tokens"]["reasoning"].as_u64().unwrap_or(0) > 0);
        StatusInfo {
            model,
            thinking: if reasoned { Some(true) } else { None },
            effort,
            context: context.filter(|c| *c > 0),
            context_size: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    pub description: String,
    pub kind: String,
}

fn subagent_from_meta(meta: &Value) -> Subagent {
    let description = meta["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).unwrap_or("Subagent");
    Subagent { description: description.to_string(), kind: meta["agentType"].as_str().unwrap_or("").to_string() }
}

impl ClaudeTracker {
    pub fn new(agent_id: u64, session_id: &str) -> Self {
        let _ = std::fs::remove_file(live_file(agent_id));
        let _ = std::fs::remove_file(session_file(agent_id));
        ClaudeTracker {
            agent_id,
            session_id: session_id.to_string(),
            path: None,
            offset: 0,
            per_message: HashMap::new(),
            state: Usage::default(),
            status_cost: 0.0,
            context_pct: None,
            info: StatusInfo::default(),
            last_live_mtime: None,
            last_hook_mtime: None,
            hook_session: None,
            started: SystemTime::now(),
            finished_subagents: Default::default(),
            subagents: Vec::new(),
        }
    }

    /// Refresh `subagents` from `<session>/subagents/agent-*.{jsonl,meta.json}`.
    /// Running = not marked finished, and its transcript was written during this
    /// run and recently (a killed run leaves unfinished metas behind).
    pub fn scan_subagents(&mut self) {
        if self.path.is_none() {
            self.path = claude_transcript(&self.session_id);
        }
        let Some(path) = &self.path else { return };
        let dir = path.with_extension("").join("subagents");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            self.subagents.clear();
            return;
        };
        let mut running = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".jsonl") else { continue };
            if self.finished_subagents.contains(id) {
                continue;
            }
            let Some(mtime) = e.metadata().ok().and_then(|m| m.modified().ok()) else { continue };
            let recent = mtime.elapsed().map_or(true, |d| d < Duration::from_secs(30 * 60));
            if mtime < self.started || !recent {
                continue;
            }
            let meta: Value = std::fs::read_to_string(dir.join(format!("{id}.meta.json")))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            if meta["finished"].as_bool() == Some(true) {
                self.finished_subagents.insert(id.to_string());
                continue;
            }
            running.push((mtime, subagent_from_meta(&meta)));
        }
        running.sort_by_key(|(t, _)| *t);
        self.subagents = running.into_iter().map(|(_, s)| s).collect();
    }

    /// Returns the session totals so far.
    pub fn poll(&mut self) -> Usage {
        self.read_session_hook();
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
        // `/clear` and `/resume` switch sessions in the same process: follow,
        // or subagents, context and cost go stale. Once the SessionStart hook
        // has spoken, a status line naming another session is from before the
        // switch.
        if let Some(sid) = v["session_id"].as_str().filter(|s| *s != self.session_id) {
            if self.hook_session.is_some() {
                return;
            }
            self.switch_session(sid, v["transcript_path"].as_str());
        }
        if let Some(p) = v["context_window"]["used_percentage"].as_f64() {
            self.context_pct = Some(p as f32);
        }
        if let Some(c) = v["cost"]["total_cost_usd"].as_f64() {
            self.status_cost = c;
        }
        self.info = StatusInfo::from_status(&v);
    }

    /// What the SessionStart hook last saved (startup, /resume, /clear, compact).
    fn read_session_hook(&mut self) {
        let path = session_file(self.agent_id);
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let mtime = meta.modified().ok();
        if mtime.is_some() && mtime == self.last_hook_mtime {
            return;
        }
        self.last_hook_mtime = mtime;
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        let Some(sid) = v["session_id"].as_str() else { return };
        self.hook_session = Some(sid.to_string());
        if sid != self.session_id {
            self.switch_session(sid, v["transcript_path"].as_str());
        }
    }

    /// The session Claude is writing to now (changes after `/clear`).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn switch_session(&mut self, sid: &str, transcript: Option<&str>) {
        self.session_id = sid.to_string();
        self.path = transcript.map(PathBuf::from).filter(|p| p.is_file());
        self.offset = 0;
        self.per_message.clear();
        self.state = Usage::default();
        self.status_cost = 0.0;
        self.context_pct = None;
        self.finished_subagents.clear();
        self.subagents.clear();
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

/// `opencode session export <id>` -> session totals and live info. Blocking; run off the UI thread.
pub fn opencode_session_usage(exe: &Path, session_id: &str, cwd: Option<&Path>) -> Option<(Usage, StatusInfo)> {
    let mut cmd = Command::new(exe);
    cmd.args(["session", "export", session_id]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
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

pub fn parse_opencode_export(bytes: &[u8]) -> Option<(Usage, StatusInfo)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let info = &v["info"];
    let t = &info["tokens"];
    if t.is_null() {
        return None;
    }
    let usage = Usage {
        input: t["input"].as_u64().unwrap_or(0),
        output: t["output"].as_u64().unwrap_or(0) + t["reasoning"].as_u64().unwrap_or(0),
        cache_read: t["cache"]["read"].as_u64().unwrap_or(0),
        cache_write: t["cache"]["write"].as_u64().unwrap_or(0),
        cost_usd: info["cost"].as_f64().unwrap_or(0.0),
    };
    Some((usage, StatusInfo::from_opencode_export(&v)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn subagent_meta() {
        let m: Value =
            serde_json::from_str(r#"{"agentType":"web-fetch","description":"Read repo","finished":false}"#).unwrap();
        assert_eq!(subagent_from_meta(&m), Subagent { description: "Read repo".into(), kind: "web-fetch".into() });
        assert_eq!(subagent_from_meta(&Value::Null).description, "Subagent");
    }

    #[test]
    fn opencode_export() {
        let json = br#"{"info":{"cost":0.25,"tokens":{"input":10,"output":2,"reasoning":3,"cache":{"read":4,"write":1}}},"messages":[]}"#;
        let (u, _) = parse_opencode_export(json).unwrap();
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (10, 5, 4, 1));
        assert_eq!(u.cost_usd, 0.25);
    }

    #[test]
    fn opencode_export_live_info() {
        // Shape of OpenCode 2.3 `session export`.
        let json = br#"{"info":{"model":{"id":"gemini-4-neon","providerID":"aistudio","variant":"high"},"cost":0,"tokens":{"input":9,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}},
            "messages":[
              {"type":"assistant","model":{"id":"gemini-4-neon"},"tokens":{"input":30000,"output":10,"reasoning":5,"cache":{"read":0,"write":0}}},
              {"type":"assistant","model":{"id":"gemini-4-neon"},"tokens":{"input":34382,"output":144,"reasoning":48,"cache":{"read":100,"write":0}}},
              {"type":"user","text":"hi"},
              {"type":"idle"}]}"#;
        let (_, i) = parse_opencode_export(json).unwrap();
        assert_eq!(i.model.as_deref(), Some("aistudio/gemini-4-neon"));
        assert_eq!(i.effort.as_deref(), Some("high"));
        assert_eq!(i.thinking, Some(true));
        assert_eq!((i.context, i.context_size), (Some(34482), None));
    }

    #[test]
    fn status_info_from_status_line() {
        let v: Value = serde_json::from_str(r#"{"model":{"id":"claude-opus-5-5","display_name":"Opus 5.5"},"effort":{"level":"medium"},"thinking":{"enabled":true},"context_window":{"context_window_size":1000000,"current_usage":{"input_tokens":2,"output_tokens":55,"cache_creation_input_tokens":442,"cache_read_input_tokens":62697}}}"#).unwrap();
        let i = StatusInfo::from_status(&v);
        assert_eq!(i.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(i.thinking, Some(true));
        assert_eq!(i.effort.as_deref(), Some("medium"));
        assert_eq!((i.context, i.context_size), (Some(63141), Some(1_000_000)));
        assert_eq!(StatusInfo::from_status(&Value::Null), StatusInfo::default());
    }

    #[test]
    fn follows_new_session_after_clear() {
        let id = 9100;
        let mut t = ClaudeTracker::new(id, "old-session");
        let live = live_file(id);
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(
            &live,
            r#"{"session_id":"new-session","context_window":{"used_percentage":12},"cost":{"total_cost_usd":0.5}}"#,
        )
        .unwrap();
        let u = t.poll();
        let _ = std::fs::remove_file(&live);
        assert_eq!(t.session_id(), "new-session");
        assert_eq!(t.context_pct, Some(12.0));
        assert_eq!(u.cost_usd, 0.5);
    }

    #[test]
    fn session_hook_wins_over_stale_status_line() {
        let id = 9101;
        let mut t = ClaudeTracker::new(id, "first");
        std::fs::create_dir_all(session_file(id).parent().unwrap()).unwrap();
        // /resume picked "second"; a status line from before the switch is late.
        std::fs::write(session_file(id), r#"{"session_id":"second","source":"resume"}"#).unwrap();
        std::fs::write(live_file(id), r#"{"session_id":"first"}"#).unwrap();
        t.poll();
        let _ = std::fs::remove_file(session_file(id));
        let _ = std::fs::remove_file(live_file(id));
        assert_eq!(t.session_id(), "second");
    }
}
