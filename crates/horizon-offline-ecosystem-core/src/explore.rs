//! Memory-driven exploration across locally observed, server-approved ground.
//! No waypoints supplied by the user, no global map, no fake player movement.
//! Cells learned beyond the current 7x7 world survey cannot be traversed.
use std::collections::BTreeMap;
use crate::local_path::first_ground_step;

pub const MAX_VISITED_TILES:usize=256;
#[derive(Debug,Clone,Default,PartialEq,Eq)]
pub struct ExplorationMemory {
    visited:BTreeMap<[i32;2],u64>,
    target:Option<[i32;2]>,
}
impl ExplorationMemory {
    pub fn new()->Self{Self::default()}
    pub fn visited_count(&self)->usize{self.visited.len()}
    pub fn selected_target(&self)->Option<[i32;2]>{self.target}
    fn visited(&mut self,pos:[i32;2],tick:u64){
        if !self.visited.contains_key(&pos) && self.visited.len()>=MAX_VISITED_TILES{
            if let Some(oldest)=self.visited.iter()
                .min_by_key(|(cell,last)|(**last,**cell))
                .map(|(key,_)|*key)
            {
                self.visited.remove(&oldest);
            }
        }
        self.visited.insert(pos,tick);
    }
    /// Choose an adjacent safe step toward an unvisited, actually reachable
    /// ground tile. Individual salt breaks tie patterns, not by inventing
    /// information: all candidates come from the real server survey.
    pub fn choose_next(
        &mut self,start:[i32;2],safe:&[[i32;2]],tick:u64,individual_salt:u32
    )->Option<[i32;2]>{
        if !safe.contains(&start){
            self.target=None;
            return None;
        }
        self.visited(start,tick);
        if self.target==Some(start){self.target=None;}
        if let Some(target)=self.target{
            if let Some(step)=first_ground_step(start,&[target],safe){
                return Some(step);
            }
            self.target=None;
        }
        let mut best=None;
        for &tile in safe{
            if tile==start{continue;}
            let Some(first)=first_ground_step(start,&[tile],safe) else{continue;};
            let previous=self.visited.get(&tile);
            let unseen=u8::from(previous.is_none());
            let old=previous.map_or(u64::MAX,|last|tick.saturating_sub(*last));
            let dx=i64::from(tile[0])-i64::from(start[0]);
            let dz=i64::from(tile[1])-i64::from(start[1]);
            let distance=dx.unsigned_abs()+dz.unsigned_abs();
            let tie=(i64::from(tile[0])*37+i64::from(tile[1])*17
                +i64::from(individual_salt)).rem_euclid(1024);
            let score=(unseen,old,distance,tie);
            if best.as_ref().is_none_or(|(b,_,_)|score>*b){
                best=Some((score,tile,first));
            }
        }
        if let Some((_,destination,first))=best{
            self.target=Some(destination);
            return Some(first);
        }
        None
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn explores_beyond_initial_spawn_neighborhood(){
        let mut e=ExplorationMemory::new();
        let mut at=[0,0];
        for tick in 1..=35 {
            let safe:Vec<_>=(-3..=3).flat_map(|x|(-3..=3).map(move|z|[at[0]+x,at[1]+z])).collect();
            if let Some(next)=e.choose_next(at,&safe,tick,2){at=next;}
        }
        assert!(at[0].abs()+at[1].abs()>=5,"explorer never left spawn");
        assert!(e.visited_count()>6,"explorer never discovered new cells");
    }
    #[test]fn never_treats_unobserved_terrain_as_safe(){
        let mut e=ExplorationMemory::new();
        assert_eq!(e.choose_next([0,0],&[[0,0],[2,0]],1,0),None);
        assert_eq!(e.choose_next([0,0],&[[0,0],[1,0],[2,0]],2,0),Some([1,0]));
    }
    #[test]fn avoids_infinite_revisits_with_a_memory_cap(){
        let mut e=ExplorationMemory::new();
        for i in 0..330{
            e.choose_next([i,0],&[[i,0]],i as u64,0);
        }
        assert_eq!(e.visited_count(),MAX_VISITED_TILES);
    }
    #[test]fn distinct_salts_do_not_change_observed_safety(){
        for salt in 0..8{
            let mut e=ExplorationMemory::new();
            let safe=[[0,0],[1,0],[0,1]];
            assert!(safe.contains(&e.choose_next([0,0],&safe,1,salt).unwrap()));
        }
    }
}
