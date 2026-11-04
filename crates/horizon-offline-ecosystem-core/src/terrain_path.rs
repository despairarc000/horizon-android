//! Conservative local 3D standing-surface routing, independent of Minecraft.
//!
//! A candidate is supplied ONLY after the authoritative server checked its
//! support block, body clearance, fluid, and loaded chunk status.
//! This planner does not create a jump or teleport packet. Vertical edges
//! are annotated so an eventual movement controller can execute genuine
//! survival jump/fall physics and validate its progress with server surveys.
use std::collections::{BTreeMap,BTreeSet,VecDeque};

pub const MAX_KNOWN_STANDS:usize=343;
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
