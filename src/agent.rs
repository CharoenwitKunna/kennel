//! Agent definitions, usage totals and on-disk persistence.

use crate::theme;
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Claude,
    OpenCode,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::OpenCode];
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude Code",
            Provider::OpenCode => "OpenCode",
        }
    }
    pub fn color(self) -> Color32 {
        match self {
            Provider::Claude => theme::CLAUDE,
            Provider::OpenCode => theme::OPENCODE,
        }
    }
    /// Model pre-filled for new agents.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Claude => "",
            Provider::OpenCode => "",
        }
    }
    pub fn model_suggestions(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &["sonnet", "opus", "haiku"],
            Provider::OpenCode => &[],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Permission {
    /// The agent asks before risky actions (you answer in its terminal).
    Safe,
    /// File edits allowed automatically.
    AcceptEdits,
    /// Everything allowed, including shell commands. Use in trusted folders only.
    FullAuto,
}

impl Permission {
    pub const ALL: [Permission; 3] = [Permission::Safe, Permission::AcceptEdits, Permission::FullAuto];
    pub fn label(self) -> &'static str {
        match self {
            Permission::Safe => "Ask before acting",
            Permission::AcceptEdits => "Auto-accept edits",
            Permission::FullAuto => "Full auto",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Permission::Safe => "The agent asks you in its terminal before editing files or running commands.",
            Permission::AcceptEdits => "File edits go through automatically; commands still ask (Claude).",
            Permission::FullAuto => "Runs any command in its folder without asking. Trusted folders only.",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cost_usd += o.cost_usd;
    }
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Agent {
    pub id: u64,
    pub name: String,
    pub provider: Provider,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub system_prompt: String,
    pub permission: Permission,
    /// Set on first launch; later launches resume it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Totals of the current session.
    #[serde(default)]
    pub session_usage: Usage,
    /// Totals of earlier sessions of this agent.
    #[serde(default)]
    pub usage_archived: Usage,
    /// v1 stored one running total here; folded into `usage_archived` on load.
    #[serde(default, rename = "usage", skip_serializing)]
    legacy_usage: Usage,
    #[serde(default)]
    pub runs: u32,
    /// It was running when the app closed; start it again on next launch.
    #[serde(default)]
    pub resume_on_launch: bool,
}

impl Agent {
    pub fn new(id: u64, name: String, provider: Provider) -> Agent {
        Agent {
            id,
            name,
            provider,
            model: String::new(),
            cwd: String::new(),
            system_prompt: String::new(),
            permission: Permission::Safe,
            session_id: None,
            session_usage: Usage::default(),
            usage_archived: Usage::default(),
            legacy_usage: Usage::default(),
            runs: 0,
            resume_on_launch: false,
        }
    }

    /// Forget the session; the next start opens a fresh conversation.
    pub fn reset_session(&mut self) {
        let s = self.session_usage;
        self.usage_archived.add(&s);
        self.session_usage = Usage::default();
        self.session_id = None;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    /// Blank = auto-detect on PATH.
    #[serde(default)]
    pub claude_path: String,
    #[serde(default)]
    pub opencode_path: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Folder new agents open in (the last one used).
    #[serde(default)]
    pub last_folder: String,
    /// When resuming a Claude conversation, answer Claude's "trust this folder?"
    /// with Yes if it asks about the agent's own folder (already trusted when the
    /// conversation started; Claude never remembers trust for the home folder).
    #[serde(default = "yes")]
    pub auto_trust_on_resume: bool,
    /// Sidebar collapsed (Ctrl+Shift+B / the ☰ button).
    #[serde(default)]
    pub sidebar_hidden: bool,
    /// Lines of history each terminal keeps (applies to newly started agents).
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
}

fn yes() -> bool {
    true
}

fn default_scrollback() -> usize {
    crate::term::SCROLLBACK
}

fn default_font_size() -> f32 {
    13.0
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            claude_path: String::new(),
            opencode_path: String::new(),
            font_size: default_font_size(),
            last_folder: String::new(),
            auto_trust_on_resume: true,
            sidebar_hidden: false,
            scrollback: default_scrollback(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct State {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub settings: Settings,
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("Kennel");
    // One-time move from the app's previous name.
    let old = base.join("AgentHub");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

pub fn data_file() -> PathBuf {
    data_dir().join("state.json")
}

impl State {
    pub fn load() -> State {
        let path = data_file();
        let mut state = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    // Keep the unreadable file instead of overwriting it.
                    let bak = path.with_extension("json.broken");
                    let _ = std::fs::copy(&path, &bak);
                    eprintln!("state.json unreadable ({e}); copied to {}", bak.display());
                    State::default()
                }
            },
            Err(_) => State::default(),
        };
        for a in &mut state.agents {
            let legacy = std::mem::take(&mut a.legacy_usage);
            a.usage_archived.add(&legacy);
        }
        state
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = data_file();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn agent_mut(&mut self, id: u64) -> Option<&mut Agent> {
        self.agents.iter_mut().find(|a| a.id == id)
    }

    pub fn agent(&self, id: u64) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }
}
