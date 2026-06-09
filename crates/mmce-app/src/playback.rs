//! Slideshow / playback driver. egui is pulled in a frame a time; we
//! compute elapsed time ourselves and advance the cursor when each interval
//! crosses.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Playback {
    pub state: State,
    /// Time between automatic page advances.
    pub interval: Duration,
    /// What to do when we hit the last (or first) page.
    pub loop_mode: LoopMode,
    /// Marks the last time the cursor auto-advanced.
    last_tick: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Playing { forward: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Off` is constructed from the settings UI via #update
pub enum LoopMode {
    /// Stop at the last page.
    Off,
    /// Wrap around to the first page.
    Wrap,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            state: State::Idle,
            interval: Duration::from_millis(3500),
            loop_mode: LoopMode::Wrap,
            last_tick: Instant::now(),
        }
    }
}

impl Playback {
    pub fn is_playing(&self) -> bool {
        matches!(self.state, State::Playing { .. })
    }

    pub fn start(&mut self, forward: bool) {
        self.state = State::Playing { forward };
        self.last_tick = Instant::now();
    }

    pub fn toggle(&mut self) {
        match self.state {
            State::Playing { .. } => self.state = State::Idle,
            State::Idle => self.start(true),
        }
    }

    pub fn pause(&mut self) {
        self.state = State::Idle;
    }

    /// Called every frame. Returns `Some(signed_step)` when it's time to
    /// advance (positive → forward, negative → backward). `cursor` and
    /// `len` drive loop-wrap logic.
    pub fn tick(&mut self, cursor: usize, len: usize) -> Option<isize> {
        let State::Playing { forward } = self.state else {
            return None;
        };
        if len == 0 {
            return None;
        }
        let now = Instant::now();
        if now.duration_since(self.last_tick) < self.interval {
            return None;
        }
        self.last_tick = now;

        let at_end = if forward {
            cursor + 1 >= len
        } else {
            cursor == 0
        };
        if at_end {
            match self.loop_mode {
                LoopMode::Off => {
                    self.pause();
                    return None;
                }
                LoopMode::Wrap => {
                    return Some(if forward {
                        -(len as isize - 1)
                    } else {
                        (len - 1) as isize
                    });
                }
            }
        }
        Some(if forward { 1 } else { -1 })
    }

    /// Millisecond delay until the next tick — useful for
    /// `ctx.request_repaint_after` so egui doesn't spin at 60 Hz while
    /// the slideshow idles between advances.
    pub fn repaint_after(&self) -> Option<Duration> {
        if !self.is_playing() {
            return None;
        }
        let since = Instant::now().saturating_duration_since(self.last_tick);
        Some(
            self.interval
                .saturating_sub(since)
                .max(Duration::from_millis(50)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_returns_none_when_idle() {
        let mut p = Playback::default();
        assert!(p.tick(0, 10).is_none());
    }

    #[test]
    fn tick_advances_after_interval() {
        let mut p = Playback {
            interval: Duration::from_millis(0),
            ..Default::default()
        };
        p.start(true);
        assert_eq!(p.tick(0, 10), Some(1));
    }

    #[test]
    fn wrap_jumps_back_to_zero_at_end() {
        let mut p = Playback {
            interval: Duration::from_millis(0),
            loop_mode: LoopMode::Wrap,
            ..Default::default()
        };
        p.start(true);
        // At the last page, wrap returns a delta that sends cursor to 0.
        let delta = p.tick(9, 10).unwrap();
        assert_eq!(9_isize + delta, 0);
    }

    #[test]
    fn loop_off_pauses_at_end() {
        let mut p = Playback {
            interval: Duration::from_millis(0),
            loop_mode: LoopMode::Off,
            ..Default::default()
        };
        p.start(true);
        assert!(p.tick(9, 10).is_none());
        assert!(!p.is_playing());
    }
}
