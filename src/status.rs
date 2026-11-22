//! Agent status, and the small state machine that turns status changes into
//! events (turn finished, new question, exited) for badges and usage polling.

use crate::theme::{ACCENT, AMBER, GREEN, GREY, RED};
use eframe::egui::Color32;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Stopped,
    Starting,
    Working,
    Waiting,
    /// Idle on a question, permission prompt or confirmation: blocked on you.
    Asking,
    Exited(i32),
}

impl Status {
    pub fn label(self) -> String {
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

    pub fn color(self) -> Color32 {
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

    pub fn alive(self) -> bool {
        matches!(self, Status::Starting | Status::Working | Status::Waiting | Status::Asking)
    }
}

/// Status of a running terminal from its raw signals.
pub fn classify(exited: Option<i32>, had_output: bool, busy: bool, asking: bool) -> Status {
    match exited {
        Some(code) => Status::Exited(code),
        None if !had_output => Status::Starting,
        None if busy => Status::Working,
        None if asking => Status::Asking,
        None => Status::Waiting,
    }
}

/// What changed in one update.
#[derive(Default, Debug, PartialEq)]
pub struct Events {
    /// Was working, now idle (ready or asking).
    pub finished_turn: bool,
    /// Just started waiting on an answer.
    pub newly_asking: bool,
    /// Just exited (reported once).
    pub exited: bool,
}

impl Events {
    /// Worth the amber attention dot? A question or exit always is; a finished
    /// turn only if you gave the agent something to do.
    pub fn wants_attention(&self, interacted: bool, bell: bool) -> bool {
        (self.finished_turn && interacted) || self.newly_asking || self.exited || bell
    }
}

pub struct Tracker {
    /// Current status and since when (for "Working for 2m").
    pub shown: Status,
    pub since: Instant,
    was_working: bool,
    was_asking: bool,
    exit_reported: bool,
}

impl Tracker {
    pub fn new(now: Instant) -> Self {
        Tracker { shown: Status::Starting, since: now, was_working: false, was_asking: false, exit_reported: false }
    }

    pub fn update(&mut self, s: Status, now: Instant) -> Events {
        if s != self.shown {
            self.shown = s;
            self.since = now;
        }
        let working = s == Status::Working;
        let asking = s == Status::Asking;
        let ev = Events {
            finished_turn: self.was_working && matches!(s, Status::Waiting | Status::Asking),
            newly_asking: asking && !self.was_asking,
            exited: matches!(s, Status::Exited(_)) && !self.exit_reported,
        };
        self.was_working = working;
        self.was_asking = asking;
        self.exit_reported |= ev.exited;
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run(seq: &[Status]) -> Vec<Events> {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        seq.iter().enumerate().map(|(i, s)| t.update(*s, t0 + Duration::from_secs(i as u64))).collect()
    }

    #[test]
    fn classify_order() {
        assert_eq!(classify(Some(1), true, true, true), Status::Exited(1));
        assert_eq!(classify(None, false, true, true), Status::Starting);
        assert_eq!(classify(None, true, true, true), Status::Working);
        assert_eq!(classify(None, true, false, true), Status::Asking);
        assert_eq!(classify(None, true, false, false), Status::Waiting);
    }

    #[test]
    fn finished_turn_fires_once() {
        use Status::*;
        let ev = run(&[Starting, Working, Working, Waiting, Waiting]);
        let fired: Vec<bool> = ev.iter().map(|e| e.finished_turn).collect();
        assert_eq!(fired, [false, false, false, true, false]);
    }

    #[test]
    fn question_after_work_is_both_events() {
        use Status::*;
        let ev = run(&[Working, Asking, Asking]);
        assert!(ev[1].finished_turn && ev[1].newly_asking);
        assert_eq!(ev[2], Events::default());
    }

    #[test]
    fn exit_reported_once() {
        use Status::*;
        let ev = run(&[Waiting, Exited(0), Exited(0)]);
        assert!(ev[1].exited && !ev[2].exited);
    }

    #[test]
    fn since_moves_only_on_change() {
        let t0 = Instant::now();
        let mut t = Tracker::new(t0);
        t.update(Status::Waiting, t0 + Duration::from_secs(1));
        t.update(Status::Waiting, t0 + Duration::from_secs(5));
        assert_eq!(t.since, t0 + Duration::from_secs(1));
    }

    #[test]
    fn attention_rules() {
        let turn = Events { finished_turn: true, ..Default::default() };
        assert!(!turn.wants_attention(false, false));
        assert!(turn.wants_attention(true, false));
        assert!(Events { newly_asking: true, ..Default::default() }.wants_attention(false, false));
        assert!(Events::default().wants_attention(false, true));
    }
}
