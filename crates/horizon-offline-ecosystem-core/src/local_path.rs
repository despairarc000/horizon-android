//! Deterministic bounded ground routing using ONLY caller-observed safe cells.
//! No world access, omniscient terrain, jumps, creative flight, or internet.
//! Horizon's terrain navigation is much richer; this is an initial player-safe
//! route step for small level-ground neighborhoods until full voxel A* works.
use std::collections::{BTreeMap,BTreeSet,VecDeque};
const MAX_CELLS:usize=225;

/// First traversable cardinal step toward any known reachable goal.
/// Start and goals are world block-grid X/Z coordinates. A cell is passable
/// only if the game server has confirmed two-block headroom, a solid floor,
/// and no water/lava. The planner never treats unobserved cells as safe.
pub fn first_ground_step(
    start:[i32;2],goals:&[[i32;2]],observed_safe:&[[i32;2]]
)->Option<[i32;2]>{
    if observed_safe.is_empty()||observed_safe.len()>MAX_CELLS{return None;}
    let walk:BTreeSet<_>=observed_safe.iter().copied().collect();
    if !walk.contains(&start){return None;}
    let goals:BTreeSet<_>=goals.iter().copied().filter(|g|walk.contains(g)).collect();
    if goals.is_empty(){return None;}
    if goals.contains(&start){return Some(start);}
    let mut queue=VecDeque::from([start]);
    let mut first=BTreeMap::from([(start,start)]);
    // North, east, south, west: no diagonal corner clipping.
    while let Some(current)=queue.pop_front(){
        for (dx,dz) in [(0,-1),(1,0),(0,1),(-1,0)]{
            let (Some(x),Some(z))=(current[0].checked_add(dx),current[1].checked_add(dz))
                else {continue;};
            let next=[x,z];
            if !walk.contains(&next)||first.contains_key(&next){continue;}
            let step=if current==start {next} else {first[&current]};
            first.insert(next,step);
            if goals.contains(&next){return Some(step);}
            queue.push_back(next);
        }
    }
    None
}

/// Reachable standing tiles around a resource. The caller still validates
/// horizontal/vertical block reach before issuing a Minecraft mining packet.
pub fn neighboring_resource_goals(
    resource:[i32;3],observed_safe:&[[i32;2]]
)->Vec<[i32;2]>{
    observed_safe.iter().copied().filter(|[x,z]|{
        let dx=i64::from(*x)-i64::from(resource[0]);
        let dz=i64::from(*z)-i64::from(resource[2]);
        dx*dx+dz*dz<=4
    }).collect()
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn routes_around_blocked_wall_without_diagonal_shortcuts(){
        let safe:Vec<_>=(0..=4).flat_map(|x|(0..=4)
            .filter(move |z|!(x==2 && *z<4) && !(x==1 && *z==0)).map(move |z|[x,z])
        ).collect();
        // The only pass around the x=2 wall is at z=4.
        assert_eq!(first_ground_step([0,0],&[[4,0]],&safe),Some([0,1]));
    }
    #[test]fn never_moves_into_unknown_or_blocked_cells(){
        assert_eq!(first_ground_step([0,0],&[[3,0]],&[[0,0],[1,0],[3,0]]),None);
        assert_eq!(first_ground_step([0,0],&[[0,1]],&[[0,1]]),None);
    }
    #[test]fn picks_reachable_goal_without_magic_line_of_sight(){
        let safe=[[0,0],[1,0],[2,0],[0,1],[1,1],[2,1]];
        assert_eq!(first_ground_step([0,0],&[[2,0],[0,1]],&safe),Some([0,1]));
    }
    #[test]fn bounded_input_and_extreme_coordinates_cannot_overflow(){
        let huge=vec![[0,0];MAX_CELLS+1];
        assert_eq!(first_ground_step([0,0],&[[0,0]],&huge),None);
        assert_eq!(first_ground_step([i32::MAX,0],&[[i32::MAX,1]],
            &[[i32::MAX,0],[i32::MAX,1]]),Some([i32::MAX,1]));
    }
    #[test]fn resource_goals_require_real_safe_ground(){
        let safe=[[0,0],[1,0],[3,0],[0,3]];
        assert_eq!(neighboring_resource_goals([1,64,0],&safe),vec![[0,0],[1,0],[3,0]]);
    }
}
