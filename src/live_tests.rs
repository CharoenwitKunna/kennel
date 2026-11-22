//! Live end-to-end tests: real CLIs in a real PTY, no window.
//! Run with `cargo test --release -- --ignored --nocapture --test-threads=1`.

use crate::agent::{Agent, Provider, Settings};
use crate::runner::{self, ClaudeTracker};
use crate::term::TermSession;
use eframe::egui;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn app_exe() -> PathBuf {
    // The app binary doubles as Claude's status-line helper. Use the debug build
    // (`cargo build && cargo live`), falling back to a release build.
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let p = ["debug", "release"]
        .iter()
        .map(|d| target.join(d).join("kennel.exe"))
        .find(|p| p.is_file())
        .expect("run `cargo build` before the live tests");
    p
}

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("kennel-live");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for(sess: &TermSession, what: &[&str], secs: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let s = sess.screen_text();
        if let Some(hit) = what.iter().find(|w| s.contains(*w)) {
            return Some(hit.to_string());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn dump(label: &str, sess: &TermSession) {
    println!("----- {label} -----");
    for line in sess.screen_text().lines() {
        let l = line.trim_end();
        if !l.is_empty() {
            println!("|{l}");
        }
    }
}

/// Type a prompt and press Enter, retrying until the TUI shows it was received
/// (an input box that is still mounting silently drops keystrokes).
fn submit_checked(sess: &TermSession, text: &str) {
    let probe: String = text.chars().take(24).collect();
    for _ in 0..4 {
        sess.submit(text);
        if wait_for(sess, &[probe.as_str()], 4).is_some() {
            return;
        }
    }
    panic!("TUI never accepted the prompt");
}

fn wait_quiet(sess: &TermSession, quiet_ms: u64, max_secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(max_secs);
    while Instant::now() < deadline {
        if sess.quiet_for().is_some_and(|d| d > Duration::from_millis(quiet_ms)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore]
fn live_claude_terminal() {
    let mut agent = Agent::new(9001, "live-claude".into(), Provider::Claude);
    agent.model = "haiku".into();
    agent.cwd = work_dir().to_string_lossy().into();
    let settings = Settings::default();
    let ctx = egui::Context::default();
    crate::term::scrub_parent_session_env();

    let launch = runner::launch(&agent, &settings, &app_exe()).unwrap();
    println!("args: {:?}", launch.spec.args);
    assert!(!launch.resumed);
    let mut launch = launch;
    let dbg = work_dir().join("claude-debug.log");
    let _ = std::fs::remove_file(&dbg);
    launch.spec.args.extend(["--debug-file".into(), dbg.to_string_lossy().into()]);
    let sid = launch.session_id.clone();
    let sess = TermSession::spawn(launch.spec, 110, 36, ctx.clone()).unwrap();
    let mut tracker = ClaudeTracker::new(agent.id, &sid);

    let hit = wait_for(&sess, &["trust", "Trust", "? for shortcuts", "for shortcuts", ">"], 40);
    dump("startup", &sess);
    if matches!(hit.as_deref(), Some("trust") | Some("Trust")) {
        // Default is "No, exit": move down to "Yes, I trust this folder".
        wait_quiet(&sess, 1000, 10);
        sess.write(b"\x1b[B".to_vec());
        std::thread::sleep(Duration::from_millis(400));
        sess.write(b"\r".to_vec());
        std::thread::sleep(Duration::from_secs(3));
        dump("after trust", &sess);
    }
    wait_quiet(&sess, 1000, 10);
    if sess.screen_text().contains("MCP server") {
        // Default choice: continue without the project's MCP server.
        sess.write(b"\r".to_vec());
        wait_for(&sess, &["for shortcuts", "Try \""], 20);
        wait_quiet(&sess, 1000, 10);
    }
    submit_checked(&sess, "Reply with the word banana in uppercase letters, nothing else.");
    let got = wait_for(&sess, &["BANANA", "rror"], 60);
    wait_quiet(&sess, 1000, 10);
    dump("after prompt", &sess);
    assert_eq!(got.as_deref(), Some("BANANA"), "no reply on screen");

    std::thread::sleep(Duration::from_secs(2));
    let u = tracker.poll();
    println!("usage: {u:?}");
    let live = std::fs::read_to_string(runner::live_file(agent.id)).unwrap_or_default();
    println!("statusline json: {}", &live[..live.len().min(1500)]);
    assert!(u.output > 0, "no tokens read from transcript");
    assert!(u.cost_usd > 0.0, "no cost from status line");
    sess.kill();
    drop(sess);

    // Resume: same session, `--resume` this time.
    agent.session_id = Some(sid.clone());
    agent.session_usage = u;
    let launch = runner::launch(&agent, &settings, &app_exe()).unwrap();
    assert!(launch.resumed, "expected --resume");
    let sess = TermSession::spawn(launch.spec, 110, 36, ctx).unwrap();
    let mut tracker = ClaudeTracker::new(agent.id, &sid);
    wait_for(&sess, &["\u{276f}"], 20); // Claude's "❯" prompt
    wait_quiet(&sess, 1000, 10);
    let live0 = std::fs::read_to_string(runner::live_file(agent.id)).unwrap_or_default();
    let v0: serde_json::Value = serde_json::from_str(&live0).unwrap_or_default();
    println!("status-line cost right after resume (before any prompt): {:?}", v0["cost"]["total_cost_usd"]);
    submit_checked(&sess, "Now reply with the word cherry in uppercase letters, nothing else.");
    let got = wait_for(&sess, &["CHERRY", "rror"], 60);
    wait_quiet(&sess, 1000, 10);
    dump("after resume prompt", &sess);
    std::thread::sleep(Duration::from_secs(2));
    let u2 = tracker.poll();
    println!("usage after resume: {u2:?}");
    let live = std::fs::read_to_string(runner::live_file(agent.id)).unwrap_or_default();
    println!("statusline json after resume: {}", &live[..live.len().min(600)]);
    assert_eq!(got.as_deref(), Some("CHERRY"));
    assert!(u2.output > u.output, "resumed tokens should accumulate");
    assert!(u2.cost_usd > u.cost_usd, "resumed cost should accumulate");
    sess.kill();
}

#[test]
#[ignore]
fn live_opencode_terminal() {
    let mut agent = Agent::new(9002, "live-opencode".into(), Provider::OpenCode);
    agent.cwd = work_dir().to_string_lossy().into();
    let settings = Settings::default();
    let ctx = egui::Context::default();

    let launch = runner::launch(&agent, &settings, &app_exe()).unwrap();
    println!("args: {:?} env: {:?}", launch.spec.args, launch.spec.env);
    let sid = launch.session_id.clone();
    let exe = launch.spec.program.clone();
    let sess = TermSession::spawn(launch.spec, 110, 36, ctx).unwrap();
    wait_for(&sess, &["Ask anything"], 20);
    wait_quiet(&sess, 1000, 10);
    dump("startup", &sess);
    submit_checked(&sess, "Reply with the word banana in uppercase letters, nothing else.");
    let got = wait_for(&sess, &["BANANA", "free messages", "rror", "Credits"], 60);
    wait_quiet(&sess, 1000, 10);
    dump("after prompt", &sess);
    println!("reply check: {got:?}");
    assert!(got.is_some(), "prompt got no visible response at all");

    // Our pre-chosen session id must be the one OpenCode used.
    let u = runner::opencode_session_usage(&exe, &sid, Some(&work_dir()));
    println!("session {sid} usage: {u:?}");
    let u = u.expect("export of our chosen session id failed");
    if got.as_deref() == Some("BANANA") {
        assert!(u.total_tokens() > 0);
    }
    let quiet = sess.quiet_for();
    println!("quiet for (should keep growing when idle): {quiet:?}");
    std::thread::sleep(Duration::from_secs(3));
    println!("quiet for after 3s: {:?}", sess.quiet_for());
    sess.kill();
    agent.session_id = Some(sid);
}

#[test]
#[ignore]
fn live_key_probe() {
    use crate::term::SpawnSpec;
    let script = "$i=0; while($i -lt 4){ $k=[Console]::ReadKey($true); Write-Host (\"KEY=\" + $k.Key + \" CH=\" + [int]$k.KeyChar); $i++ }; Start-Sleep 2";
    let spec = SpawnSpec {
        program: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
        args: vec!["-NoProfile".into(), "-Command".into(), script.into()],
        cwd: None,
        env: Default::default(),
    };
    let sess = TermSession::spawn(spec, 100, 20, egui::Context::default()).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    sess.write(b"\x1b[B".to_vec());
    std::thread::sleep(Duration::from_millis(500));
    sess.write(b"\x1b[A".to_vec());
    std::thread::sleep(Duration::from_millis(500));
    sess.write(b"\r".to_vec());
    std::thread::sleep(Duration::from_millis(500));
    sess.write(b"x".to_vec());
    std::thread::sleep(Duration::from_secs(2));
    dump("keys", &sess);
}

/// Distinct foreground colors currently on screen (to diagnose color output).
fn color_census(sess: &TermSession) -> Vec<String> {
    let t = sess.term.lock();
    let mut seen = std::collections::BTreeSet::new();
    for c in t.renderable_content().display_iter {
        if c.c != ' ' {
            seen.insert(format!("{:?}", c.fg));
        }
    }
    seen.into_iter().collect()
}

#[test]
#[ignore]
fn live_color_probe() {
    // Reproduce a parent shell with NO_COLOR set; Kennel must still give agents color.
    std::env::set_var("NO_COLOR", "1");
    crate::term::scrub_parent_session_env();
    let agent = {
        let mut a = Agent::new(9003, "color".into(), Provider::Claude);
        a.cwd = work_dir().to_string_lossy().into();
        a.model = "haiku".into();
        a
    };
    for force in [false, true] {
        let mut launch = runner::launch(&agent, &Settings::default(), &app_exe()).unwrap();
        if force {
            launch.spec.env.insert("FORCE_COLOR".into(), "3".into());
        }
        let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
        wait_for(&sess, &["\u{276f}"], 20);
        wait_quiet(&sess, 1000, 10);
        println!("FORCE_COLOR={force}: {:?}", color_census(&sess));
        if !force {
            let t = sess.term.lock();
            let mut line = -1;
            let mut cur = String::new();
            let mut out = String::new();
            for c in t.renderable_content().display_iter {
                if c.point.line.0 != line {
                    line = c.point.line.0;
                    out.push('\n');
                    cur.clear();
                }
                let tag = match c.fg { alacritty_terminal::vte::ansi::Color::Named(_) => String::new(), other => format!("{other:?}") };
                if tag != cur && c.c != ' ' { out.push_str(&format!("[{}]", if tag.is_empty() { "fg".into() } else { tag.replace("Spec(Rgb { ", "").replace(" })", "") })); cur = tag; }
                out.push(c.c);
            }
            for l in out.lines().filter(|l| !l.trim().is_empty()).take(14) { println!("|{}", l.trim_end()); }
        }
        sess.kill();
    }
}

#[test]
#[ignore]
fn live_idle_probe() {
    // How does an idle TUI (waiting for input) look to our activity detector?
    crate::term::scrub_parent_session_env();
    let mut a = Agent::new(9004, "idle".into(), Provider::Claude);
    a.cwd = work_dir().to_string_lossy().into();
    a.model = "haiku".into();
    let launch = runner::launch(&a, &Settings::default(), &app_exe()).unwrap();
    let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
    wait_for(&sess, &["trust", "\u{276f}"], 20);
    std::thread::sleep(Duration::from_secs(3));
    let mut last = sess.screen_text();
    for i in 0..12 {
        std::thread::sleep(Duration::from_millis(500));
        let now = sess.screen_text();
        println!("t+{:.1}s quiet_for={:?} screen_changed={}", i as f32 * 0.5, sess.quiet_for(), now != last);
        last = now;
    }
    sess.kill();
}

/// Number of differing cells between two screen snapshots.
fn cells_changed(a: &str, b: &str) -> usize {
    a.lines().zip(b.lines()).map(|(x, y)| x.chars().zip(y.chars()).filter(|(p, q)| p != q).count()).sum()
}

#[test]
#[ignore]
fn live_idle_probe_opencode() {
    crate::term::scrub_parent_session_env();
    let a = Agent::new(9005, "idle-oc".into(), Provider::OpenCode);
    let launch = runner::launch(&a, &Settings::default(), &app_exe()).unwrap();
    let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
    wait_for(&sess, &["Ask anything"], 20);
    std::thread::sleep(Duration::from_secs(3));
    let mut last = sess.screen_text();
    for i in 0..10 {
        std::thread::sleep(Duration::from_millis(500));
        let now = sess.screen_text();
        println!("t+{:.1}s quiet_for={:?} cells_changed={}", i as f32 * 0.5, sess.quiet_for(), cells_changed(&last, &now));
        last = now;
    }
    sess.kill();
}

#[test]
#[ignore]
fn live_busy_probe() {
    // busy() must be true while Claude works on a prompt and false once it's done.
    crate::term::scrub_parent_session_env();
    let mut a = Agent::new(9006, "busy".into(), Provider::Claude);
    a.cwd = std::env::temp_dir().join("agent-hub-live").to_string_lossy().into(); // already trusted
    a.model = "haiku".into();
    let launch = runner::launch(&a, &Settings::default(), &app_exe()).unwrap();
    let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
    wait_for(&sess, &["\u{276f}"], 20);
    wait_quiet(&sess, 1500, 10);
    println!("idle before prompt: busy={}", sess.busy());
    submit_checked(&sess, "Count from 1 to 40, one number per line, slowly explaining nothing.");
    let mut samples = Vec::new();
    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(250));
        samples.push(if sess.busy() { 'B' } else { '.' });
    }
    println!("while answering (250ms steps): {}", samples.iter().collect::<String>());
    wait_quiet(&sess, 2500, 40);
    std::thread::sleep(Duration::from_secs(1));
    println!("after answer: busy={}", sess.busy());
    sess.kill();
}

#[test]
#[ignore]
fn live_resume_home_probe() {
    // What does resuming a Claude session in the HOME folder look like?
    crate::term::scrub_parent_session_env();
    let mut a = Agent::new(9007, "resume-home".into(), Provider::Claude);
    a.cwd = std::env::var("USERPROFILE").unwrap();
    a.model = "haiku".into();
    let accept_trust = |sess: &TermSession| {
        if wait_for(sess, &["trust this folder", "\u{276f} Try", "for shortcuts"], 20).as_deref() == Some("trust this folder") {
            wait_quiet(sess, 1000, 10);
            sess.write(b"\x1b[B".to_vec());
            std::thread::sleep(Duration::from_millis(300));
            sess.write(b"\r".to_vec());
        }
        wait_quiet(sess, 1500, 15);
    };
    let launch = runner::launch(&a, &Settings::default(), &app_exe()).unwrap();
    a.session_id = Some(launch.session_id.clone());
    let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
    accept_trust(&sess);
    submit_checked(&sess, "Remember the word MANGO. Reply only with the word READY.");
    let got = wait_for(&sess, &["{25cf} READY", "{25cf}"], 60);
    println!("first run reply: {got:?}");
    wait_quiet(&sess, 2000, 20);
    sess.kill();
    drop(sess);
    std::thread::sleep(Duration::from_secs(1));

    let launch = runner::launch(&a, &Settings::default(), &app_exe()).unwrap();
    println!("second launch resumed={} args={:?}", launch.resumed, &launch.spec.args[..2]);
    let sess = TermSession::spawn(launch.spec, 100, 30, egui::Context::default()).unwrap();
    std::thread::sleep(Duration::from_secs(4));
    dump("resume: first screen", &sess);
    accept_trust(&sess);
    dump("resume: after trusting", &sess);
    sess.kill();
}
