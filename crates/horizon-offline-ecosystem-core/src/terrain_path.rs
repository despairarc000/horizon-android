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
