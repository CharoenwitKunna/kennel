//! Token usage for each provider: Claude's transcript and status line, and
//! OpenCode's session export.

use crate::agent::Usage;
use crate::runner::live_file;
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
    fn opencode_export() {
        let json = br#"{"info":{"cost":0.25,"tokens":{"input":10,"output":2,"reasoning":3,"cache":{"read":4,"write":1}}},"messages":[]}"#;
        let u = parse_opencode_export(json).unwrap();
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (10, 5, 4, 1));
        assert_eq!(u.cost_usd, 0.25);
    }
}
