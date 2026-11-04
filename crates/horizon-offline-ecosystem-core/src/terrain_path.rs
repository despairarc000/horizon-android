//! Conservative local 3D standing-surface routing, independent of Minecraft.
//!
//! A candidate is supplied ONLY after the authoritative server checked its
//! support block, body clearance, fluid, and loaded chunk status.
//! This planner does not create a jump or teleport packet. Vertical edges
//! are annotated so an eventual movement controller can execute genuine
//! survival jump/fall physics and validate its progress with server surveys.
use std::collections::{BTreeMap,BTreeSet,VecDeque};

pub const MAX_KNOWN_STANDS:usize=512;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteMotion { Walk, StepDown, JumpUp }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteStep {
    pub at:[i32;3],
    pub motion:RouteMotion,
}

/// Return the first *cardinal* movement step toward any observed destination.
/// Never traverse unknown cells, changes greater than one block in height,
/// or diagonal gaps. Both endpoints must be confirmed safe by the server.
/// This is an abstract route, not proof that physical movement is possible.
pub fn first_terrain_step(
    start:[i32;3], goals:&[[i32;3]], confirmed_stands:&[[i32;3]]
)->Option<RouteStep>{
    route_step(start,goals,confirmed_stands,true)
}

/// A route that this experimental client can currently EXECUTE: level
/// walking and one-block descents only. Don't select a shortcut that needs
/// unimplemented jump physics and then stall on its first step.
pub fn first_walk_down_step(
    start:[i32;3], goals:&[[i32;3]], confirmed_stands:&[[i32;3]]
)->Option<RouteStep>{
    route_step(start,goals,confirmed_stands,false)
}
fn route_step(
    start:[i32;3], goals:&[[i32;3]], confirmed_stands:&[[i32;3]],
    allow_up:bool
)->Option<RouteStep>{
    if confirmed_stands.is_empty() || confirmed_stands.len()>MAX_KNOWN_STANDS{
        return None;
    }
    let safe:BTreeSet<_>=confirmed_stands.iter().copied().collect();
    if !safe.contains(&start) {return None;}
    let destinations:BTreeSet<_>=goals.iter().copied().filter(|p|safe.contains(p)).collect();
    if destinations.is_empty() {return None;}
    if destinations.contains(&start) {
        return Some(RouteStep {at:start,motion:RouteMotion::Walk});
    }
    let mut frontier=VecDeque::from([start]);
    let mut seen=BTreeMap::from([(start,start)]);
    while let Some(p)=frontier.pop_front(){
        for (dx,dz) in [(0,-1),(1,0),(0,1),(-1,0)]{
            let (Some(x),Some(z))=(p[0].checked_add(dx),p[2].checked_add(dz)) else {
                continue;
            };
            // A player can jump only one block and can step or fall down
            // one block. No implicit multi-block climbing or jumping gaps.
            for dy in [0,-1,1]{
                if dy>0 && !allow_up {continue;}
                let Some(y)=p[1].checked_add(dy) else{continue;};
                let to=[x,y,z];
                if !safe.contains(&to)||seen.contains_key(&to){continue;}
                let first=if p==start{to}else{seen[&p]};
                seen.insert(to,first);
                if destinations.contains(&to){
                    let motion=match first[1].cmp(&start[1]){
                        std::cmp::Ordering::Less=>RouteMotion::StepDown,
                        std::cmp::Ordering::Equal=>RouteMotion::Walk,
                        std::cmp::Ordering::Greater=>RouteMotion::JumpUp,
                    };
                    return Some(RouteStep{at:first,motion});
                }
                frontier.push_back(to);
            }
        }
    }
    None
}

/// In one bounded flood fill, find the best EXECUTABLE reachable frontier
/// with a meaningful score improvement over the current standing cell.
/// The chosen first step can initially move AWAY from the goal, allowing
/// detours. Unknown cells and upward jump edges remain forbidden.
/// This is local path planning, not authoritative physical movement.
pub fn best_walk_down_frontier<F>(
    start:[i32;3], confirmed_stands:&[[i32;3]],
    score:F
)->Option<RouteStep>
where F:Fn([i32;3])->f64 {
    if confirmed_stands.is_empty() || confirmed_stands.len()>MAX_KNOWN_STANDS {
        return None;
    }
    let safe:BTreeSet<_>=confirmed_stands.iter().copied().collect();
    if !safe.contains(&start){return None;}
    let starting_score=score(start);
    if !starting_score.is_finite(){return None;}
    let mut queue=VecDeque::from([start]);
    // First executable step and the complete walk/down route length.
    let mut seen=BTreeMap::from([(start,(RouteStep{
        at:start,motion:RouteMotion::Walk,
    },0usize))]);
    let mut best:Option<(f64,usize,[i32;3],RouteStep)>=None;
    while let Some(p)=queue.pop_front(){
        let (first,depth)=seen[&p];
        if p!=start {
            let value=score(p);
            if value.is_finite() && value+0.01<starting_score {
                let candidate=(value,depth,p,first);
                if best.as_ref().is_none_or(|old|
                    value.total_cmp(&old.0).then_with(||depth.cmp(&old.1))
                        .then_with(||p.cmp(&old.2)).is_lt()
                ){
                    best=Some(candidate);
                }
            }
        }
        for (dx,dz) in [(0,-1),(1,0),(0,1),(-1,0)] {
            let (Some(x),Some(z))=(p[0].checked_add(dx),p[2].checked_add(dz))
                else {continue;};
            for dy in [0,-1] {
                let Some(y)=p[1].checked_add(dy) else {continue;};
                let q=[x,y,z];
                if !safe.contains(&q) || seen.contains_key(&q){continue;}
                let route=if p==start {
                    RouteStep{at:q,motion:if dy<0{
                        RouteMotion::StepDown
                    } else {RouteMotion::Walk}}
                } else {first};
                seen.insert(q,(route,depth+1));
                queue.push_back(q);
            }
        }
    }
    best.map(|(_,_,_,first)|first)
}

/// One conservative player motion packet requested by a one-block descent.
/// These are local *requests*; only a later server survey proves movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DescentMotion {
    pub dx: f64,
    pub dy: f64,
    pub dz: f64,
    pub on_ground: bool,
}

/// Bounded falling controller for an adjacent, server-verified standing
/// surface exactly one block lower. It does NOT bypass Horizon's movement
/// collision, chunk loading, or survival health/fall-damage simulation.
/// Horizontal movement first reaches the lip, then falling uses a small
/// acceleration and a final exactly-on-ground step to the safe lower tile.
#[derive(Clone, Debug)]
pub struct StepDownController {
    pub destination: [i32;3],
    vertical_speed: f64,
}
impl StepDownController {
    pub fn begin(at:[f64;3],destination:[i32;3])->Option<Self>{
        if !at.iter().all(|v|v.is_finite()){return None;}
        let from=[at[0].floor() as i32,at[1].floor() as i32,at[2].floor() as i32];
        let dx=(i64::from(destination[0])-i64::from(from[0])).abs();
        let dz=(i64::from(destination[2])-i64::from(from[2])).abs();
        if dx+dz!=1 || i64::from(from[1])-i64::from(destination[1])!=1 {
            return None;
        }
        Some(Self {destination,vertical_speed:0.0})
    }
    /// None means local predicted completion or a rejected/invalid position;
    /// neither counts as a verified safe landing. The caller must wait for
    /// authoritative location or time out and replan.
    pub fn next(&mut self,at:[f64;3])->Option<DescentMotion>{
        if !at.iter().all(|v|v.is_finite()) {return None;}
        let floor=f64::from(self.destination[1]);
        if at[1] < floor-0.05 || at[1]>floor+1.20 || at[1]<=floor+0.01 {
            return None;
        }
        let tx=f64::from(self.destination[0])+0.5;
        let tz=f64::from(self.destination[2])+0.5;
        let dx=tx-at[0];
        let dz=tz-at[2];
        let distance=dx.hypot(dz);
        if !distance.is_finite() || distance>1.8 {return None;}
        // Walk toward the ledge with normal ground-claim packets. At a
        // horizontal separation of <=0.22, most of a 0.6-wide body is clear
        // of the higher support, so the downward packet can be collision
        // tested normally by Horizon's server.
        if distance>0.22 {
            let step=distance.min(0.20);
            return Some(DescentMotion{
                dx:dx/distance*step,dy:0.0,dz:dz/distance*step,on_ground:true
            });
        }
        // A bounded approximation of survival gravity, not teleportation.
        // A standard downward acceleration is applied once each client tick.
        self.vertical_speed=(self.vertical_speed-0.08)*0.98;
        let descend=(-self.vertical_speed).clamp(0.01,0.30).min(at[1]-floor);
        let landed=at[1]-descend<=floor+1e-8;
        let drift=distance.min(0.15);
        Some(DescentMotion{
            dx:if distance>1e-8{dx/distance*drift}else{0.0},
            dy:-descend,
            dz:if distance>1e-8{dz/distance*drift}else{0.0},
            on_ground:landed,
        })
    }
}

/// A bounded physical jump packet. It is only a client request; the
/// server remains responsible for collision, jumping, and fall damage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JumpMotion {
    pub dx:f64,
    pub dy:f64,
    pub dz:f64,
    pub on_ground:bool,
}

/// Execute one adjacent one-block uphill route over normal 20-Hz ticks.
/// A jump begins with vertical velocity ~0.42; subsequent ticks use
/// vanilla-like gravity and drag. Horizontal progress waits until the
/// player's feet clear the adjacent higher block.
#[derive(Clone, Debug)]
pub struct StepUpController {
    pub destination:[i32;3],
    vertical_speed:f64,
    began:bool,
    start_y:i32,
}
impl StepUpController {
    pub fn begin(at:[f64;3],destination:[i32;3])->Option<Self>{
        if !at.iter().all(|x|x.is_finite()){return None;}
        let from=[at[0].floor() as i32,at[1].floor() as i32,at[2].floor() as i32];
        let dx=(i64::from(destination[0])-i64::from(from[0])).abs();
        let dz=(i64::from(destination[2])-i64::from(from[2])).abs();
        if dx+dz!=1 || i64::from(destination[1])-i64::from(from[1])!=1 {
            return None;
        }
        Some(Self{destination,vertical_speed:0.0,began:false,start_y:from[1]})
    }
    /// None means the attempted arc was invalid or did not land, NOT a
    /// successful jump. The authoritative server survey decides success.
    pub fn next(&mut self,at:[f64;3])->Option<JumpMotion>{
        if !at.iter().all(|x|x.is_finite()){return None;}
        let landing_y=f64::from(self.destination[1]);
        if at[1]<f64::from(self.start_y)-0.10 || at[1]>landing_y+0.75{
            return None;
        }
        let tx=f64::from(self.destination[0])+0.5;
        let tz=f64::from(self.destination[2])+0.5;
        let x=tx-at[0];
        let z=tz-at[2];
        let distance=x.hypot(z);
        if !distance.is_finite()||distance>1.8{return None;}
        if !self.began{
            self.began=true;
            self.vertical_speed=0.42;
        }else{
            self.vertical_speed=(self.vertical_speed-0.08)*0.98;
        }
        let predicted=at[1]+self.vertical_speed;
        let clear=predicted>=landing_y+0.001;
        // No horizontal clipping into the higher support block before
        // the feet have gained a full block of altitude.
        let step=if clear{distance.min(0.24)}else{0.0};
        let dx=if distance>1e-8{x/distance*step}else{0.0};
        let dz=if distance>1e-8{z/distance*step}else{0.0};
        let close=distance<=0.26;
        if self.vertical_speed<0.0 && predicted<=landing_y+0.03 && close{
            return Some(JumpMotion{
                dx,dz,dy:landing_y-at[1],on_ground:true,
            });
        }
        // Don't fake a landing if the arc missed the top of the higher
        // block. A real server position still determines what happened.
        if self.vertical_speed<0.0 && predicted<landing_y-0.03 {
            return None;
        }
        Some(JumpMotion{
            dx,dz,dy:self.vertical_speed.clamp(-0.30,0.42),
            on_ground:false,
        })
    }
}

/// Confirmed safe standing cells close enough to attempt harvesting a log.
/// The action itself must independently pass Horizon's raycast and reach tests.
pub fn adjacent_terrain_goals(log:[i32;3],stands:&[[i32;3]])->Vec<[i32;3]>{
    stands.iter().copied().filter(|p|{
        let dx=i64::from(p[0])-i64::from(log[0]);
        let dz=i64::from(p[2])-i64::from(log[2]);
        let dy=(i64::from(p[1])-i64::from(log[1])).abs();
        dx*dx+dz*dz<=4 && dy<=2
    }).collect()
}

#[cfg(test)]
mod tests{
    use super::*;

    #[test]
    fn uphill_jump_is_only_one_cardinal_block(){
        assert!(StepUpController::begin([0.5,64.0,0.5],[1,65,0]).is_some());
        assert!(StepUpController::begin([0.5,64.0,0.5],[1,66,0]).is_none());
        assert!(StepUpController::begin([0.5,64.0,0.5],[1,65,1]).is_none());
        assert!(StepUpController::begin([0.5,64.0,0.5],[0,63,1]).is_none());
        assert!(StepUpController::begin([f64::NAN,64.0,0.5],[1,65,0]).is_none());
    }
    #[test]
    fn uphill_arc_clears_support_then_claims_ground(){
        let mut jump=StepUpController::begin([0.5,64.0,0.5],[1,65,0]).unwrap();
        let mut at=[0.5,64.0,0.5];
        let mut peak=at[1];
        let mut landed=false;
        for _ in 0..18{
            let Some(m)=jump.next(at) else{break;};
            assert!(m.dx.hypot(m.dz)<=0.240001);
            assert!((-0.300001..=0.420001).contains(&m.dy));
            if at[1]+m.dy<65.0-1e-6{
                assert_eq!(m.dx,0.0,"can't clip horizontally through a wall");
            }
            at=[at[0]+m.dx,at[1]+m.dy,at[2]+m.dz];
            peak=peak.max(at[1]);
            if m.on_ground{
                landed=true;
                break;
            }
        }
        assert!(peak>65.1);
        assert!(landed);
        assert!((at[0]-1.5).abs()<0.27);
        assert!((at[1]-65.0).abs()<1e-8);
    }
    #[test]
    fn impossible_uphill_jump_does_not_claim_a_success(){
        let mut j=StepUpController::begin([0.5,64.0,0.5],[1,65,0]).unwrap();
        assert!(j.next([f64::INFINITY,64.0,0.5]).is_none());
        assert!(j.next([10.5,64.0,0.5]).is_none());
        assert!(j.next([0.5,60.0,0.5]).is_none());
    }
    #[test]
    fn descent_is_cardinal_and_exactly_one_block(){
        assert!(StepDownController::begin([0.5,65.,0.5],[1,64,0]).is_some());
        assert!(StepDownController::begin([0.5,65.,0.5],[1,63,0]).is_none());
        assert!(StepDownController::begin([0.5,65.,0.5],[1,64,1]).is_none());
        assert!(StepDownController::begin([0.5,65.,0.5],[0,65,1]).is_none());
        assert!(StepDownController::begin([f64::NAN,65.,0.5],[1,64,0]).is_none());
    }
    #[test]
    fn descent_requests_only_small_downward_steps_then_ground_contact(){
        let mut controller=StepDownController::begin([0.5,65.,0.5],[1,64,0]).unwrap();
        let mut at=[0.5,65.,0.5];
        let mut falling=0;
        let mut landed=false;
        for _ in 0..32 {
            let Some(m)=controller.next(at) else {break;};
            assert!(m.dx.hypot(m.dz)<=0.200001);
            assert!((-0.300001..=0.0).contains(&m.dy));
            assert!(m.dy<=0.0);
            if m.dy<0.0 { falling+=1; }
            if m.on_ground && m.dy<0.0 { landed=true; }
            at=[at[0]+m.dx,at[1]+m.dy,at[2]+m.dz];
        }
        assert!(falling>=3,"instant one-block drop would bypass physics");
        assert!(landed,"must finish with server-verifiable ground claim");
        assert!((at[1]-64.0).abs()<1e-8);
        assert!((at[0]-1.5).abs()<0.02);
    }
    #[test]
    fn descent_rejects_far_disconnected_and_invalid_positions(){
        let mut controller=StepDownController::begin([0.5,65.,0.5],[1,64,0]).unwrap();
        assert_eq!(controller.next([10.0,65.0,0.5]),None);
        assert_eq!(controller.next([0.5,62.0,0.5]),None);
        assert_eq!(controller.next([0.5,65.0,f64::INFINITY]),None);
    }
    #[test]
    fn walks_on_flat_verified_ground(){
        let known=[[0,64,0],[1,64,0],[2,64,0]];
        assert_eq!(
            first_terrain_step([0,64,0],&[[2,64,0]],&known),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::Walk})
        );
    }
    #[test]
    fn distinguishes_one_block_downhill_and_uphill(){
        let down=[[0,65,0],[1,64,0],[2,64,0]];
        assert_eq!(
            first_terrain_step([0,65,0],&[[2,64,0]],&down),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::StepDown})
        );
        assert_eq!(
            first_terrain_step([2,64,0],&[[0,65,0]],&down),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::Walk})
        );
        assert_eq!(
            first_terrain_step([1,64,0],&[[0,65,0]],&down),
            Some(RouteStep{at:[0,65,0],motion:RouteMotion::JumpUp})
        );
    }
    #[test]
    fn refuses_two_block_steep_cliffs(){
        let cliff=[[0,64,0],[1,66,0]];
        assert_eq!(first_terrain_step([0,64,0],&[[1,66,0]],&cliff),None);
        assert_eq!(first_terrain_step([1,66,0],&[[0,64,0]],&cliff),None);
    }
    #[test]
    fn no_diagonal_corner_jump_or_unknown_cells(){
        let only_diagonal=[[0,64,0],[1,65,1]];
        assert_eq!(first_terrain_step([0,64,0],&[[1,65,1]],&only_diagonal),None);
        let missing_middle=[[0,64,0],[2,64,0]];
        assert_eq!(first_terrain_step([0,64,0],&[[2,64,0]],&missing_middle),None);
    }
    #[test]
    fn executable_route_detours_around_jump_shortcut(){
        // An abstract route might select a climb; the bot cannot execute
        // JumpUp yet, but it can walk around on a longer known flat route.
        let stands=[
            [0,65,0],[1,66,0],[0,65,1],
            [1,65,1],[2,64,1],[2,64,0],
        ];
        let goal=[[2,64,1]];
        let walkable=first_walk_down_step([0,65,0],&goal,&stands);
        assert_eq!(walkable,Some(RouteStep{
            at:[0,65,1],motion:RouteMotion::Walk,
        }));
        let impossible=[[0,65,0],[1,66,0],[2,66,0]];
        assert_eq!(first_walk_down_step([0,65,0],&[[2,66,0]],&impossible),None);
        assert_eq!(first_terrain_step([0,65,0],&[[2,66,0]],&impossible),
            Some(RouteStep{at:[1,66,0],motion:RouteMotion::JumpUp}));
    }
    #[test]
    fn one_pass_frontier_search_uses_a_genuine_detour(){
        let stands=[
            [0,65,0],[0,65,1],[0,65,2],
            [1,65,2],[2,65,2],[2,64,1],[2,64,0],
        ];
        let score=|p:[i32;3]|{
            let dx=f64::from(2-p[0]);
            let dz=f64::from(0-p[2]);
            dx*dx+dz*dz+256.0*f64::from(p[1]-64).powi(2)
        };
        assert_eq!(best_walk_down_frontier([0,65,0],&stands,score),
            Some(RouteStep{at:[0,65,1],motion:RouteMotion::Walk}));
        // A disconnected lower cell never becomes an invented path.
        let disconnected=[[0,65,0],[2,64,0]];
        assert_eq!(best_walk_down_frontier([0,65,0],&disconnected,score),None);
    }
    #[test]
    fn one_pass_frontier_never_uses_unimplemented_jump(){
        let stands=[[0,65,0],[1,66,0],[2,66,0]];
        let score=|p:[i32;3]|f64::from(2-p[0]).powi(2);
        assert_eq!(best_walk_down_frontier([0,65,0],&stands,score),None);
    }
    #[test]
    fn expanded_bounded_survey_is_supported(){
        let mut stands=Vec::new();
        for x in -5..=5 {
            for z in -5..=5 {
                for y in 64..=66 {stands.push([x,y,z]);}
            }
        }
        assert_eq!(stands.len(),363);
        assert_eq!(first_walk_down_step([0,65,0],&[[5,65,5]],&stands),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::Walk}));
    }
    #[test]
    fn bounded_search_and_integer_extremes(){
        let huge=vec![[0,64,0];MAX_KNOWN_STANDS+1];
        assert_eq!(first_terrain_step([0,64,0],&[[0,64,0]],&huge),None);
        let cells=[[i32::MAX,64,0],[i32::MAX,64,1]];
        assert_eq!(
            first_terrain_step(cells[0],&[cells[1]],&cells),
            Some(RouteStep{at:cells[1],motion:RouteMotion::Walk})
        );
    }
    #[test]
    fn does_not_infer_unseen_resource_reach(){
        let safe=[[0,64,0],[1,64,0],[3,64,0]];
        assert_eq!(adjacent_terrain_goals([1,64,0],&safe),vec![[0,64,0],[1,64,0],[3,64,0]]);
        assert!(adjacent_terrain_goals([10,64,10],&safe).is_empty());
    }
}
