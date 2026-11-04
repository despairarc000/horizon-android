//! Bounded, offline resource-target memory and observed movement failure.
//!
//! Inspired by Rain World's last-known-target and stuck-tracker ideas, not a
//! port of its code. Only the caller's actual observations can create targets.
//! Predicted position never counts as progress: observe() must receive the
//! authoritative position published by the Horizon server.

use std::collections::VecDeque;

/// Four failed server observations after a movement request: give up on the
/// attempted approach instead of sending the same movement forever.
pub const STUCK_SAMPLES: u8 = 4;
/// How long a last-seen target remains worth investigating, in server ticks.
pub const MEMORY_TICKS: u64 = 160;
/// How long a failed approach is avoided before it may be reconsidered.
pub const AVOID_TICKS: u64 = 240;
pub const MAX_AVOIDED: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LastSeen {
    pos: [i32; 3],
    tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Avoided {
    pos: [i32; 3],
    until_tick: u64,
}

/// One individual bot's short-lived knowledge, not shared global knowledge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetMemory {
    last_seen: Option<LastSeen>,
    avoided: VecDeque<Avoided>,
    tracking: Option<[i32; 3]>,
    previous_position: Option<[f64; 3]>,
    previous_tick: Option<u64>,
    movement_requested: bool,
    stagnant_samples: u8,
}

impl TargetMemory {
    pub fn new() -> Self { Self::default() }

    /// Called once for each *new* authoritative server survey. Visible blocks
    /// must have been observed in loaded terrain, ordered by caller's priority.
    /// Unlike an omniscient search, a missing target can only be remembered
    /// for a bounded time and can be disproved by another survey.
    pub fn observe(
        &mut self, tick: u64, position: [f64; 3], visible: &[[i32; 3]],
    ) -> Option<[i32; 3]> {
        if !position.iter().all(|v| v.is_finite()) {
            return None;
        }
        if self.previous_tick.is_some_and(|old| tick <= old) {
            return self.tracking;
        }

        self.avoided.retain(|entry| entry.until_tick > tick);
        if let Some(previous) = self.previous_position {
            if self.movement_requested && self.tracking.is_some() {
                let dx = position[0] - previous[0];
                let dz = position[2] - previous[2];
                if dx * dx + dz * dz < 0.10f64.powi(2) {
                    self.stagnant_samples = self.stagnant_samples.saturating_add(1);
                } else {
                    self.stagnant_samples = 0;
                }
                if self.stagnant_samples >= STUCK_SAMPLES {
                    if let Some(target) = self.tracking {
                        self.avoid(target, tick);
                    }
                }
            } else {
                self.stagnant_samples = 0;
            }
        }

        // When last-known location is inside this survey's full window but
        // absent from the observed blocks, do not investigate a ghost target.
        if let Some(remembered) = self.last_seen {
            let center = [
                position[0].floor() as i32,
                position[1].floor() as i32,
                position[2].floor() as i32,
            ];
            let p = remembered.pos;
            let in_survey = (p[0] - center[0]).abs() <= 3
                && (p[2] - center[2]).abs() <= 3
                && (-1..=2).contains(&(p[1] - center[1]));
            if tick.saturating_sub(remembered.tick) > MEMORY_TICKS
                || (in_survey && !visible.contains(&p))
            {
                self.last_seen = None;
            }
        }

        let selected = visible
            .iter()
            .copied()
            .find(|p| !self.is_avoided(*p))
            .or_else(|| self.last_seen
                .map(|r| r.pos)
                .filter(|p| !self.is_avoided(*p)));

        if let Some(p) = selected {
            if visible.contains(&p) {
                self.last_seen = Some(LastSeen { pos: p, tick });
            }
        }
        if selected != self.tracking {
            self.stagnant_samples = 0;
        }
        self.tracking = selected;
        self.previous_tick = Some(tick);
        self.previous_position = Some(position);
        self.movement_requested = false;
        selected
    }

    /// The caller actually submitted a short movement packet toward target.
    /// A plan or a failed action does not count as an attempted move.
    pub fn attempted_step(&mut self, target: [i32; 3]) {
        if self.tracking == Some(target) {
            self.movement_requested = true;
        }
    }

    /// A goal has proved repeatedly impossible, regardless of the movement
    /// tracker (e.g. mining was rejected). Remember the failure for a while.
    pub fn avoid(&mut self, target: [i32; 3], tick: u64) {
        self.avoided.retain(|entry| entry.pos != target && entry.until_tick > tick);
        if self.avoided.len() == MAX_AVOIDED {
            self.avoided.pop_front();
        }
        self.avoided.push_back(Avoided {
            pos: target, until_tick: tick.saturating_add(AVOID_TICKS),
        });
        if self.last_seen.is_some_and(|r| r.pos == target) {
            self.last_seen = None;
        }
        if self.tracking == Some(target) {
            self.tracking = None;
            self.stagnant_samples = 0;
            self.movement_requested = false;
        }
    }

    pub fn is_avoided(&self, target: [i32; 3]) -> bool {
        self.avoided.iter().any(|entry| entry.pos == target)
    }

    pub fn avoided_count(&self) -> usize { self.avoided.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stationary_server_position_abandons_target_and_chooses_alternative() {
        let mut memory = TargetMemory::new();
        let first = [2, 64, 0];
        let second = [3, 64, 0];
        assert_eq!(memory.observe(1, [0.5,64.0,0.5], &[first,second]), Some(first));
        for tick in [9,17,25,33] {
            memory.attempted_step(first);
            memory.observe(tick, [0.5,64.0,0.5], &[first,second]);
        }
        assert!(memory.is_avoided(first));
        assert_eq!(memory.observe(41, [0.5,64.0,0.5], &[first,second]), Some(second));
    }

    #[test]
    fn actual_authoritative_progress_resets_stuck_count() {
        let first = [2,64,0];
        let mut m = TargetMemory::new();
        m.observe(1, [0.,64.,0.], &[first]);
        for n in 1..=10 {
            m.attempted_step(first);
            assert_eq!(
                m.observe(n * 8 + 1, [(n as f64) * 0.15,64.,0.], &[first]),
                Some(first),
            );
        }
        assert!(!m.is_avoided(first));
    }

    #[test]
    fn remembers_last_seen_only_when_now_outside_observation_window() {
        let first=[2,64,0];
        let mut m=TargetMemory::new();
        m.observe(1, [0.5,64.,0.5], &[first]);
        // The target is in a currently surveyed area and absent: negative
        // evidence is authoritative, so memory must be cleared.
        assert_eq!(m.observe(9, [0.5,64.,0.5], &[]), None);
        m.observe(17, [0.5,64.,0.5], &[first]);
        // Target is outside the new scan; investigate where it was last seen.
        assert_eq!(m.observe(25, [6.5,64.,0.5], &[]), Some(first));
        assert_eq!(m.observe(178, [6.5,64.,0.5], &[]), None);
    }

    #[test]
    fn avoidance_is_bounded_expires_and_never_creates_targets() {
        let mut m=TargetMemory::new();
        for i in 0..32 {
            m.avoid([i,64,0],1);
        }
        assert_eq!(m.avoided_count(), MAX_AVOIDED);
        assert!(!m.is_avoided([0,64,0]));
        assert!(m.is_avoided([31,64,0]));
        assert_eq!(m.observe(1,[0.,64.,0.],&[]),None);
        assert_eq!(m.observe(242,[0.,64.,0.],&[[31,64,0]]),Some([31,64,0]));
        assert_eq!(m.avoided_count(),0);
    }

    #[test]
    fn rejected_or_duplicate_server_samples_do_not_increment_stall() {
        let first=[2,64,0];
        let mut m=TargetMemory::new();
        assert_eq!(m.observe(1,[0.,64.,0.],&[first]),Some(first));
        m.attempted_step(first);
        assert_eq!(m.observe(1,[0.,64.,0.],&[first]),Some(first));
        assert_eq!(m.observe(0,[0.,64.,0.],&[first]),Some(first));
        assert_eq!(m.observe(9,[0.,64.,0.],&[first]),Some(first));
        assert!(!m.is_avoided(first));
        assert_eq!(m.observe(17,[f64::NAN,64.,0.],&[first]),None);
        assert_eq!(m.observe(17,[0.,64.,0.],&[first]),Some(first));
    }
}
