//! Memory-driven exploration across locally observed, server-approved ground.
//! A transport packet is only an attempted move; authoritative position samples
//! determine whether an attempted step made physical progress.
//! No waypoints supplied by the user, no global map, no fake player movement.
use std::collections::{BTreeMap,VecDeque};
use crate::local_path::first_ground_step;

pub const MAX_VISITED_TILES: usize = 256;
pub const MAX_BLOCKED_STEPS: usize = 16;
pub const BLOCKED_STEP_TICKS: u64 = 240;
pub const STALLED_SURVEYS: u8 = 4;
const PROGRESS_SQUARED: f64 = 0.10 * 0.10;
/// If no usable wood is observed and the *server-reported* position has
/// stayed within a small neighborhood this long, try a safe vertical exit
/// rather than wandering forever among the same flat cells.
pub const CONFINED_SEARCH_TICKS:u64=240;
const CONFINED_SEARCH_RADIUS_SQ:f64=25.0;

/// Avoid immediately undoing a real, server-confirmed one-block
/// transition during unguided exploration. Explicit resource/pickup goals
/// can override this preference. The single-edge memory is bounded.
pub const VERTICAL_RETURN_TICKS:u64=120;
/// A longer, bounded memory prevents alternating among two or more ledges,
/// not just immediately reversing the last single transition.
pub const VERTICAL_REVISIT_TICKS:u64=480;
pub const MAX_RECENT_VERTICAL_STANDS:usize=12;
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
struct ConfirmedVerticalTransition {
    from:[i32;3],
    to:[i32;3],
    until_tick:u64,
}
#[derive(Debug,Clone,Default,PartialEq,Eq)]
pub struct VerticalTransitionMemory {
    recent:Option<ConfirmedVerticalTransition>,
    recently_visited:VecDeque<([i32;3],u64)>,
}
impl VerticalTransitionMemory {
    pub fn new()->Self{Self::default()}
    /// Only call this after an authoritative server survey verified a
    /// supported landing, never after emitting a jump/fall packet.
    pub fn observed_landing(&mut self,from:[i32;3],to:[i32;3],tick:u64){
        let horizontal=(i64::from(from[0])-i64::from(to[0])).abs()
            +(i64::from(from[2])-i64::from(to[2])).abs();
        let vertical=(i64::from(from[1])-i64::from(to[1])).abs();
        if horizontal==1 && vertical==1
            && self.recent.is_none_or(|prev|tick>=prev.until_tick.saturating_sub(VERTICAL_RETURN_TICKS))
        {
            self.recent=Some(ConfirmedVerticalTransition{
                from,to,until_tick:tick.saturating_add(VERTICAL_RETURN_TICKS)
            });
            self.recently_visited.retain(|(_,until)|*until>tick);
            for stand in [from,to]{
                self.recently_visited.retain(|(p,_)|*p!=stand);
                if self.recently_visited.len()>=MAX_RECENT_VERTICAL_STANDS{
                    self.recently_visited.pop_front();
                }
                self.recently_visited.push_back((
                    stand,tick.saturating_add(VERTICAL_REVISIT_TICKS)
                ));
            }
        }
    }
    /// Other verified recent landing/launch surfaces, excluding the current
    /// position. Only unguided exploration avoids them: a real resource,
    /// pickup or emergency action may still legitimately backtrack.
    pub fn recent_stands_to_avoid(&self,current:[i32;3],tick:u64)->Vec<[i32;3]>{
        self.recently_visited.iter()
            .filter(|(p,until)|*until>tick && *p!=current)
            .map(|(p,_)|*p).collect()
    }
    /// The physical surface to avoid *only during exploration*, when the
    /// current authoritative standing cell is the last transition's goal.
    /// This expires; it cannot permanently block backtracking or survival.
    pub fn avoid_immediate_return(
        &self,current:[i32;3],tick:u64
    )->Option<[i32;3]> {
        self.recent.filter(|e|e.to==current && tick<e.until_tick)
            .map(|e|e.from)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExplorationMemory {
    visited: BTreeMap<[i32; 2], u64>,
    target: Option<[i32; 2]>,
    blocked_steps: BTreeMap<[i32; 2], u64>,
    last_sample: Option<(u64, [f64; 2])>,
    attempted_step: Option<[i32; 2]>,
    stagnant_surveys: u8,
    resource_search_anchor:Option<(u64,[f64;2])>,
}

impl ExplorationMemory {
    pub fn new() -> Self { Self::default() }
    pub fn visited_count(&self) -> usize { self.visited.len() }
    pub fn selected_target(&self) -> Option<[i32; 2]> { self.target }
    pub fn blocked_count(&self) -> usize { self.blocked_steps.len() }

    fn visited(&mut self, pos: [i32; 2], tick: u64) {
        if !self.visited.contains_key(&pos) && self.visited.len() >= MAX_VISITED_TILES {
            if let Some(oldest) = self.visited.iter()
                .min_by_key(|(cell, last)| (**last, **cell))
                .map(|(key, _)| *key)
            {
                self.visited.remove(&oldest);
            }
        }
        self.visited.insert(pos, tick);
    }

    /// This must be called only after a REAL movement packet was submitted.
    /// A path proposal, rejected packet, or unconfirmed local prediction is
    /// not evidence that a movement attempt happened.
    pub fn attempted_step(&mut self, waypoint: [i32; 2]) {
        self.attempted_step = Some(waypoint);
    }

    /// Observe a *new* authoritative server position, not the bot's locally
    /// predicted position. Four stagnant surveys after submitted packets
    /// quarantine the attempted next tile temporarily. No movement at all
    /// does not cause a false obstacle. Returns the newly quarantined tile.
    pub fn observe_server_position(
        &mut self, tick: u64, position: [f64; 2],
    ) -> Option<[i32; 2]> {
        if !position.iter().all(|v| v.is_finite()) {
            return None;
        }
        if self.last_sample.is_some_and(|(previous_tick, _)| tick <= previous_tick) {
            return None;
        }

        self.blocked_steps.retain(|_, until| *until > tick);
        let mut blocked = None;
        if let Some((_, previous)) = self.last_sample {
            let dx = position[0] - previous[0];
            let dz = position[1] - previous[1];
            if dx * dx + dz * dz >= PROGRESS_SQUARED {
                self.stagnant_surveys = 0;
            } else if self.attempted_step.is_some() {
                self.stagnant_surveys = self.stagnant_surveys.saturating_add(1);
                if self.stagnant_surveys >= STALLED_SURVEYS {
                    let step = self.attempted_step.expect("attempt was checked");
                    if self.blocked_steps.len() >= MAX_BLOCKED_STEPS {
                        if let Some(oldest) = self.blocked_steps.iter()
                            .min_by_key(|(pos, until)| (**until, **pos))
                            .map(|(pos, _)| *pos)
                        {
                            self.blocked_steps.remove(&oldest);
                        }
                    }
                    self.blocked_steps.insert(step, tick.saturating_add(BLOCKED_STEP_TICKS));
                    self.target = None;
                    self.stagnant_surveys = 0;
                    blocked = Some(step);
                }
            } else {
                self.stagnant_surveys = 0;
            }
        }

        self.last_sample = Some((tick, position));
        self.attempted_step = None;
        blocked
    }

    /// Observe whether this individual is confined while it seeks a usable
    /// natural resource. Only authoritative positions and *new* surveys are
    /// admitted. This is not a claim that a higher/lower route exists; the
    /// existing collision-verified planner must still choose each step.
    pub fn resource_search_confined(
        &mut self,tick:u64,position:[f64;2],usable_wood_observed:bool
    )->bool{
        if !position.iter().all(|x|x.is_finite()){return false;}
        if usable_wood_observed{
            self.resource_search_anchor=Some((tick,position));
            return false;
        }
        let Some((start,anchor))=self.resource_search_anchor else {
            self.resource_search_anchor=Some((tick,position));
            return false;
        };
        if tick<start{return false;}
        let dx=position[0]-anchor[0];
        let dz=position[1]-anchor[1];
        if dx*dx+dz*dz>=CONFINED_SEARCH_RADIUS_SQ{
            self.resource_search_anchor=Some((tick,position));
            return false;
        }
        tick.saturating_sub(start)>=CONFINED_SEARCH_TICKS
    }

    /// Were all CURRENTLY REACHABLE, observed level-ground cells actually
    /// visited? Disconnected terrain is not a reason to keep circling a
    /// smaller accessible plateau. Never infer cells beyond the server scan.
    pub fn locally_exhausted(
        &self,start:[i32;2],safe:&[[i32;2]],tick:u64
    )->bool{
        if !safe.contains(&start){return true;}
        let available:Vec<_>=safe.iter().copied().filter(|p|
            *p==start || self.blocked_steps.get(p).is_none_or(|until|*until<=tick)
        ).collect();
        available.iter().all(|&p|{
            if p==start{return self.visited.contains_key(&p);}
            first_ground_step(start,&[p],&available).is_none()
                || self.visited.contains_key(&p)
        })
    }

    /// Choose a first cardinal step toward an observed reachable tile.
    /// Temporarily blocked steps cannot be used as transit cells, not just
    /// final destinations. This prevents retrying the same failed edge.
    pub fn choose_next(
        &mut self, start: [i32; 2], safe: &[[i32; 2]],
        tick: u64, individual_salt: u32,
    ) -> Option<[i32; 2]> {
        self.blocked_steps.retain(|_, until| *until > tick);
        if !safe.contains(&start) {
            self.target = None;
            return None;
        }
        self.visited(start, tick);
        if self.target == Some(start) { self.target = None; }
        let unblocked: Vec<[i32; 2]> = safe.iter().copied()
            .filter(|p| *p == start || !self.blocked_steps.contains_key(p))
            .collect();

        if let Some(target) = self.target {
            if let Some(step) = first_ground_step(start, &[target], &unblocked) {
                return Some(step);
            }
            self.target = None;
        }
        let mut best = None;
        for &tile in &unblocked {
            if tile == start { continue; }
            let Some(first) = first_ground_step(start, &[tile], &unblocked) else { continue; };
            let previous = self.visited.get(&tile);
            let unseen = u8::from(previous.is_none());
            let old = previous.map_or(u64::MAX, |last| tick.saturating_sub(*last));
            let dx = i64::from(tile[0]) - i64::from(start[0]);
            let dz = i64::from(tile[1]) - i64::from(start[1]);
            let distance = dx.unsigned_abs() + dz.unsigned_abs();
            let tie = (i64::from(tile[0]) * 37 + i64::from(tile[1]) * 17
                + i64::from(individual_salt)).rem_euclid(1024);
            let score = (unseen, old, distance, tie);
            if best.as_ref().is_none_or(|(b, _, _)| score > *b) {
                best = Some((score, tile, first));
            }
        }
        if let Some((_, destination, first)) = best {
            self.target = Some(destination);
            return Some(first);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ping_pong_between_newly_reached_vertical_surfaces(){
        let a=[0,65,0];
        let b=[1,66,0];
        let mut m=VerticalTransitionMemory::new();
        assert_eq!(m.avoid_immediate_return(b,101),None);
        m.observed_landing(a,b,100);
        assert_eq!(m.avoid_immediate_return(b,101),Some(a));
        // The rule is scoped to the actual landing surface.
        assert_eq!(m.avoid_immediate_return([2,66,0],101),None);
        // Expiration permits returning along a narrow, legitimate path.
        assert_eq!(m.avoid_immediate_return(b,220),None);
        m.observed_landing(b,a,221);
        assert_eq!(m.avoid_immediate_return(a,222),Some(b));
    }
    #[test]
    fn multi_edge_vertical_cycles_are_temporarily_avoided(){
        let a=[0,65,0];
        let b=[1,64,0];
        let c=[1,64,1];
        let d=[2,65,1];
        let mut memory=VerticalTransitionMemory::new();
        memory.observed_landing(a,b,100);
        memory.observed_landing(c,d,120);
        let avoided=memory.recent_stands_to_avoid(d,121);
        assert!(avoided.contains(&a));
        assert!(avoided.contains(&b));
        assert!(avoided.contains(&c));
        assert!(!avoided.contains(&d));
        assert!(memory.recent_stands_to_avoid(d,601).is_empty());
    }
    #[test]
    fn vertical_history_is_bounded_to_recent_confirmed_physical_steps(){
        let mut memory=VerticalTransitionMemory::new();
        for i in 0..20{
            let from=[i*3,64,0];
            let to=[i*3+1,65,0];
            memory.observed_landing(from,to,i as u64*10+1);
        }
        assert!(memory.recently_visited.len()<=MAX_RECENT_VERTICAL_STANDS);
        // Invalid or out-of-order transitions never poison the history.
        let before=memory.recent_stands_to_avoid([500,65,0],210);
        memory.observed_landing([50,64,0],[52,65,0],211);
        memory.observed_landing([500,64,0],[501,65,0],1);
        assert_eq!(memory.recent_stands_to_avoid([500,65,0],210),before);
    }
    #[test]
    fn only_verified_cardinal_one_block_changes_create_reverse_avoidance(){
        let mut m=VerticalTransitionMemory::new();
        m.observed_landing([0,64,0],[2,65,0],1);
        m.observed_landing([0,64,0],[0,66,0],2);
        m.observed_landing([0,64,0],[1,64,0],3);
        assert_eq!(m.avoid_immediate_return([1,65,0],4),None);
        m.observed_landing([0,64,0],[1,65,0],5);
        assert_eq!(m.avoid_immediate_return([1,65,0],6),Some([0,64,0]));
        // A stale survey must not overwrite a newer confirmed transition.
        m.observed_landing([2,65,0],[3,66,0],4);
        assert_eq!(m.avoid_immediate_return([1,65,0],6),Some([0,64,0]));
    }
    #[test]
    fn confined_wood_search_uses_real_displacement_and_observed_resources(){
        let mut memory=ExplorationMemory::new();
        assert!(!memory.resource_search_confined(1,[0.5,0.5],false));
        assert!(!memory.resource_search_confined(240,[1.5,0.5],false));
        assert!(memory.resource_search_confined(241,[1.5,0.5],false));
        // A real excursion, unlike attempted movement packets, restarts the
        // bounded search interval and allows normal flat exploration again.
        assert!(!memory.resource_search_confined(249,[5.5,0.5],false));
        assert!(!memory.resource_search_confined(490,[5.5,0.5],true));
        assert!(!memory.resource_search_confined(491,[5.5,0.5],false));
        assert!(!memory.resource_search_confined(300,[5.5,0.5],false));
        assert!(!memory.resource_search_confined(800,[f64::NAN,0.5],false));
    }
    #[test]
    fn flat_exhaustion_requires_real_visits_not_proposed_waypoints(){
        let mut m=ExplorationMemory::new();
        let safe=[[0,0],[1,0],[2,0]];
        assert!(!m.locally_exhausted([0,0],&safe,1));
        m.choose_next([0,0],&safe,1,0);
        assert!(!m.locally_exhausted([0,0],&safe,1));
        m.choose_next([1,0],&safe,9,0);
        assert!(!m.locally_exhausted([1,0],&safe,9));
        m.choose_next([2,0],&safe,17,0);
        assert!(m.locally_exhausted([2,0],&safe,17));
    }

    #[test]
    fn disconnected_unknown_side_does_not_block_flat_exhaustion(){
        let mut m=ExplorationMemory::new();
        let safe=[[0,0],[1,0],[8,0]];
        m.choose_next([0,0],&safe,1,0);
        m.choose_next([1,0],&safe,9,0);
        assert!(m.locally_exhausted([1,0],&safe,17));
        assert!(m.locally_exhausted([12,0],&safe,17));
    }

    #[test]
    fn explores_beyond_initial_spawn_neighborhood() {
        let mut e = ExplorationMemory::new();
        let mut at = [0, 0];
        for tick in 1..=35 {
            let safe: Vec<_> = (-3..=3).flat_map(|x|
                (-3..=3).map(move |z| [at[0] + x, at[1] + z])).collect();
            if let Some(next) = e.choose_next(at, &safe, tick, 2) { at = next; }
        }
        assert!(at[0].abs() + at[1].abs() >= 5, "explorer never left spawn");
        assert!(e.visited_count() > 6, "explorer never discovered new cells");
    }

    #[test]
    fn never_treats_unobserved_terrain_as_safe() {
        let mut e = ExplorationMemory::new();
        assert_eq!(e.choose_next([0, 0], &[[0, 0], [2, 0]], 1, 0), None);
        assert_eq!(e.choose_next([0, 0], &[[0, 0], [1, 0], [2, 0]], 2, 0), Some([1, 0]));
    }

    #[test]
    fn avoids_infinite_revisits_with_a_memory_cap() {
        let mut e = ExplorationMemory::new();
        for i in 0..330 {
            e.choose_next([i, 0], &[[i, 0]], i as u64, 0);
        }
        assert_eq!(e.visited_count(), MAX_VISITED_TILES);
    }

    #[test]
    fn distinct_salts_do_not_change_observed_safety() {
        for salt in 0..8 {
            let mut e = ExplorationMemory::new();
            let safe = [[0, 0], [1, 0], [0, 1]];
            assert!(safe.contains(&e.choose_next([0, 0], &safe, 1, salt).unwrap()));
        }
    }

    #[test]
    fn stationary_server_position_quarantines_only_attempted_step() {
        let mut e = ExplorationMemory::new();
        let safe = [[0, 0], [1, 0], [0, 1]];
        e.observe_server_position(1, [0.5, 0.5]);
        for tick in [9, 17, 25, 33] {
            e.attempted_step([1, 0]);
            let quarantined = e.observe_server_position(tick, [0.5, 0.5]);
            assert_eq!(quarantined, (tick == 33).then_some([1, 0]));
        }
        assert_eq!(e.blocked_count(), 1);
        assert_eq!(e.choose_next([0, 0], &safe, 34, 0), Some([0, 1]));
        assert_eq!(e.blocked_count(), 1);
        assert_eq!(e.choose_next([0, 0], &safe, 274, 0).is_some(), true);
        assert_eq!(e.blocked_count(), 0);
    }

    #[test]
    fn legitimate_progress_resets_stall_and_idle_never_blocks() {
        let mut e = ExplorationMemory::new();
        e.observe_server_position(1, [0.5, 0.5]);
        for tick in [9, 17, 25] {
            e.attempted_step([1, 0]);
            assert_eq!(e.observe_server_position(tick, [0.5, 0.5]), None);
        }
        e.attempted_step([1, 0]);
        assert_eq!(e.observe_server_position(33, [0.7, 0.5]), None);
        for tick in [41, 49, 57, 65] {
            assert_eq!(e.observe_server_position(tick, [0.7, 0.5]), None);
        }
        assert_eq!(e.blocked_count(), 0);
    }

    #[test]
    fn rejects_duplicate_and_invalid_surveys_without_false_failure() {
        let mut e = ExplorationMemory::new();
        e.observe_server_position(1, [0.5, 0.5]);
        e.attempted_step([1, 0]);
        assert_eq!(e.observe_server_position(1, [0.5, 0.5]), None);
        assert_eq!(e.observe_server_position(2, [f64::NAN, 0.5]), None);
        assert_eq!(e.observe_server_position(9, [0.5, 0.5]), None);
        assert_eq!(e.blocked_count(), 0);
    }

    #[test]
    fn does_not_route_through_quarantined_tile() {
        let mut e = ExplorationMemory::new();
        let safe = [[0, 0], [1, 0], [2, 0]];
        e.observe_server_position(1, [0.5, 0.5]);
        for tick in [9, 17, 25, 33] {
            e.attempted_step([1, 0]);
            e.observe_server_position(tick, [0.5, 0.5]);
        }
        assert_eq!(e.choose_next([0, 0], &safe, 34, 0), None);
    }

    #[test]
    fn blocked_step_memory_is_bounded() {
        let mut e = ExplorationMemory::new();
        for i in 0..30 {
            e.observe_server_position((i * 48 + 1) as u64, [0.5, 0.5]);
            for j in 1..=4 {
                e.attempted_step([i + 1, 0]);
                e.observe_server_position((i * 48 + 1 + j * 8) as u64, [0.5, 0.5]);
            }
        }
        assert!(e.blocked_count() <= MAX_BLOCKED_STEPS);
    }
}
