//! Turns the raw edges of a push button (pressed, released, time passing) into the events of Matter's Switch cluster.
//!
//! No hardware and no clock in here: callers pass timestamps in milliseconds, so every sequence can be tested exactly.
//!
//! The sequences follow the Matter Core spec for a momentary switch with the release, long-press and multi-press features:
//!
//! | what you do | events |
//! |---|---|
//! | tap | `InitialPress`, `ShortRelease`, then `MultiPressComplete(1)` once the multi-press time has passed |
//! | hold | `InitialPress`, `LongPress` (once, after the long-press time), `LongRelease` |
//! | double tap | `InitialPress`, `ShortRelease`, `InitialPress`, `MultiPressOngoing(2)`, `ShortRelease`, `MultiPressComplete(2)` |
//! | more taps than the maximum | `MultiPressComplete(0)`, then nothing until the button has been idle |
//!
//! There is no `LongPress` inside a multi-press sequence, and none of the `MultiPress*` events when multi-press is off
//! (then every tap ends with its `ShortRelease`, and Home Assistant sees the press at once instead of after the wait).

/// One event of the Switch cluster, for one button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchEvent {
    InitialPress,
    ShortRelease,
    LongPress,
    LongRelease,
    /// The press count so far, sent right after the second and later `InitialPress`.
    MultiPressOngoing(u8),
    /// The final press count; 0 means the sequence went past the maximum and was abandoned.
    MultiPressComplete(u8),
}

/// Timing and features of the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Holding at least this long is a long press.
    pub long_press_ms: u64,
    /// A tap sequence ends when the button has been up this long.
    pub multi_press_ms: u64,
    /// Report double taps (and `MultiPressComplete`); off reports each tap on its own.
    pub multi_press: bool,
    /// Most presses in one sequence (the cluster's `MultiPressMax`, 2 to 255).
    pub max_presses: u8,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { long_press_ms: 500, multi_press_ms: 300, multi_press: true, max_presses: 2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Held down since `at`; `count` is the press number in this sequence; `long` is set once `LongPress` was sent.
    Down { at: u64, count: u8, long: bool },
    /// Released at `at`, waiting to see whether another tap follows.
    Up { at: u64, count: u8 },
    /// A sequence that went past the maximum: all edges are ignored until the button has been idle.
    AbandonedDown,
    AbandonedUp { at: u64 },
}

#[derive(Debug, Clone)]
pub struct SwitchLogic {
    timing: Timing,
    state: State,
}

impl SwitchLogic {
    pub fn new(timing: Timing) -> Self {
        SwitchLogic { timing, state: State::Idle }
    }

    /// Is the button held down now (the cluster's `CurrentPosition` is 1)?
    pub fn is_down(&self) -> bool {
        matches!(self.state, State::Down { .. } | State::AbandonedDown)
    }

    /// The button went down (`pressed`) or up at `now` (ms). Call [`tick`](Self::tick) with the same time first, so a
    /// sequence whose multi-press time has run out is closed before this edge starts a new one.
    pub fn edge(&mut self, pressed: bool, now: u64) -> Vec<SwitchEvent> {
        use SwitchEvent::*;
        let mut out = Vec::new();
        match (self.state, pressed) {
            (State::Idle, true) => {
                out.push(InitialPress);
                self.state = State::Down { at: now, count: 1, long: false };
            }
            (State::Up { count, .. }, true) => {
                if count >= self.timing.max_presses {
                    out.push(MultiPressComplete(0));
                    self.state = State::AbandonedDown;
                } else {
                    out.push(InitialPress);
                    out.push(MultiPressOngoing(count + 1));
                    self.state = State::Down { at: now, count: count + 1, long: false };
                }
            }
            (State::Down { count, long, .. }, false) => {
                if long {
                    out.push(LongRelease);
                    self.state = State::Idle;
                } else {
                    out.push(ShortRelease);
                    self.state = if self.timing.multi_press { State::Up { at: now, count } } else { State::Idle };
                }
            }
            (State::AbandonedDown, false) => self.state = State::AbandonedUp { at: now },
            (State::AbandonedUp { .. }, true) => self.state = State::AbandonedDown,
            // A press while already down, or a release while already up: nothing changed.
            _ => {}
        }
        out
    }

    /// Time has passed: send what is due (`LongPress`, the end of a tap sequence).
    pub fn tick(&mut self, now: u64) -> Vec<SwitchEvent> {
        let mut out = Vec::new();
        match self.state {
            State::Down { at, count, long: false } if count == 1 && now.saturating_sub(at) >= self.timing.long_press_ms => {
                out.push(SwitchEvent::LongPress);
                self.state = State::Down { at, count, long: true };
            }
            State::Up { at, count } if now.saturating_sub(at) >= self.timing.multi_press_ms => {
                out.push(SwitchEvent::MultiPressComplete(count));
                self.state = State::Idle;
            }
            State::AbandonedUp { at } if now.saturating_sub(at) >= self.timing.multi_press_ms => self.state = State::Idle,
            _ => {}
        }
        out
    }

    /// When `tick` next has something to do (ms), or `None` while nothing is pending.
    pub fn next_deadline(&self) -> Option<u64> {
        match self.state {
            State::Down { at, count: 1, long: false } => Some(at + self.timing.long_press_ms),
            State::Up { at, .. } | State::AbandonedUp { at } => Some(at + self.timing.multi_press_ms),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SwitchEvent::*;
    use super::*;

    /// One scripted step: an edge at a time, or just the clock moving on.
    enum Step {
        Press(u64),
        Release(u64),
        Tick(u64),
    }
    use Step::*;

    fn run(timing: Timing, steps: &[Step]) -> Vec<SwitchEvent> {
        let mut l = SwitchLogic::new(timing);
        let mut out = Vec::new();
        for s in steps {
            out.extend(match *s {
                Press(t) => l.edge(true, t),
                Release(t) => l.edge(false, t),
                Tick(t) => l.tick(t),
            });
        }
        out
    }

    #[test]
    fn a_tap_ends_with_multi_press_complete_one() {
        let ev = run(Timing::default(), &[Press(0), Release(80), Tick(200), Tick(380)]);
        assert_eq!(ev, [InitialPress, ShortRelease, MultiPressComplete(1)]);
    }

    #[test]
    fn a_hold_is_a_long_press_with_no_multi_press_events() {
        let ev = run(Timing::default(), &[Press(0), Tick(499), Tick(500), Tick(900), Release(1200), Tick(2000)]);
        assert_eq!(ev, [InitialPress, LongPress, LongRelease]);
    }

    #[test]
    fn a_long_press_is_sent_once() {
        let ev = run(Timing::default(), &[Press(0), Tick(500), Tick(600), Tick(10_000)]);
        assert_eq!(ev, [InitialPress, LongPress]);
    }

    #[test]
    fn a_double_tap() {
        let ev = run(Timing::default(), &[Press(0), Release(60), Press(200), Release(260), Tick(400), Tick(560)]);
        assert_eq!(ev, [InitialPress, ShortRelease, InitialPress, MultiPressOngoing(2), ShortRelease, MultiPressComplete(2)]);
    }

    #[test]
    fn the_second_press_of_a_double_tap_can_be_held_without_a_long_press() {
        let ev = run(Timing::default(), &[Press(0), Release(60), Press(200), Tick(900), Release(1500), Tick(1800)]);
        assert_eq!(ev, [InitialPress, ShortRelease, InitialPress, MultiPressOngoing(2), ShortRelease, MultiPressComplete(2)]);
    }

    #[test]
    fn a_third_tap_abandons_the_sequence_and_stays_quiet_until_idle() {
        let t = Timing::default();
        let ev = run(t, &[Press(0), Release(50), Press(150), Release(200), Press(300), Release(350), Press(450), Release(500), Tick(900)]);
        assert_eq!(ev, [InitialPress, ShortRelease, InitialPress, MultiPressOngoing(2), ShortRelease, MultiPressComplete(0)]);
        // Idle again: the next tap is a fresh sequence.
        let ev2 = run(t, &[Press(0), Release(50), Press(150), Release(200), Press(300), Release(350), Tick(700), Press(800), Release(850), Tick(1200)]);
        assert_eq!(&ev2[ev2.len() - 3..], [InitialPress, ShortRelease, MultiPressComplete(1)]);
    }

    #[test]
    fn with_multi_press_off_every_tap_stands_alone() {
        let t = Timing { multi_press: false, ..Timing::default() };
        let ev = run(t, &[Press(0), Release(60), Press(200), Release(260), Tick(1000)]);
        assert_eq!(ev, [InitialPress, ShortRelease, InitialPress, ShortRelease]);
        let hold = run(t, &[Press(0), Tick(500), Release(900)]);
        assert_eq!(hold, [InitialPress, LongPress, LongRelease]);
    }

    #[test]
    fn a_bigger_maximum_counts_more_taps() {
        let t = Timing { max_presses: 3, ..Timing::default() };
        let ev = run(t, &[Press(0), Release(50), Press(150), Release(200), Press(300), Release(350), Tick(700)]);
        assert_eq!(*ev.last().unwrap(), MultiPressComplete(3));
        assert!(ev.contains(&MultiPressOngoing(3)));
    }

    #[test]
    fn stray_edges_change_nothing() {
        let ev = run(Timing::default(), &[Release(5), Release(6), Press(10), Press(11), Release(70)]);
        assert_eq!(ev, [InitialPress, ShortRelease]);
    }

    #[test]
    fn position_and_deadlines_follow_the_state() {
        let mut l = SwitchLogic::new(Timing::default());
        assert!(!l.is_down() && l.next_deadline().is_none());
        l.edge(true, 1000);
        assert!(l.is_down());
        assert_eq!(l.next_deadline(), Some(1500));
        l.edge(false, 1100);
        assert!(!l.is_down());
        assert_eq!(l.next_deadline(), Some(1400));
        l.tick(1400);
        assert!(l.next_deadline().is_none());
    }

    #[test]
    fn a_button_that_is_never_released_sends_one_long_press_and_waits() {
        let mut l = SwitchLogic::new(Timing::default());
        l.edge(true, 0);
        assert_eq!(l.tick(500), [LongPress]);
        assert!(l.is_down());
        assert!(l.next_deadline().is_none(), "no timer is pending while it is held");
    }
}
