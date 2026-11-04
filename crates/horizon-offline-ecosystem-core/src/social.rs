//! Individual, bounded social memory for offline survival actors.
//!
//! Sightings must come from nearby server-authoritative player observations,
//! not global population knowledge. Trust changes only after the caller
//! confirms a real interaction; mere proximity does not create friendship.
use std::collections::{BTreeSet,VecDeque};
use serde::{Serialize,Deserialize};

pub const MAX_SOCIAL_PEERS:usize=16;
pub const SIGHT_RADIUS_BLOCKS:f64=8.0;
pub const SIGHTING_TTL_TICKS:u64=240;
pub const REMEMBER_PEER_TICKS:u64=12_000;

#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
pub struct PeerSighting {
    pub id:u128,
    pub position:[f64;3],
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ConfirmedInteraction {
    ReceivedHelp,
    SharedItem,
    WasAttacked,
    WasThreatened,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SocialPreference { Approach, Avoid, Neutral }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
struct Acquaintance {
    sighting:PeerSighting,
    last_seen:u64,
    trust:f32,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
pub struct SocialMemory {
    peers:VecDeque<Acquaintance>,
    last_survey:Option<u64>,
}
impl SocialMemory {
    pub fn new()->Self{Self::default()}
    pub fn known_count(&self)->usize{self.peers.len()}
    pub fn trust(&self,id:u128)->Option<f32>{
        self.peers.iter().find(|p|p.sighting.id==id).map(|p|p.trust)
    }
    /// Accept a new authoritative local survey. No observation outside
    /// eight blocks, false self-sighting, duplicate or invalid coordinate
    /// may enter an individual's private memory. All memory is bounded.
    pub fn observe(&mut self,tick:u64,self_id:u128,at:[f64;3],sightings:&[PeerSighting]){
        if !at.iter().all(|v|v.is_finite())
            || self.last_survey.is_some_and(|previous|tick<=previous)
        {return;}
        self.last_survey=Some(tick);
        self.peers.retain(|p|tick.saturating_sub(p.last_seen)<=REMEMBER_PEER_TICKS);
        let mut ordered=sightings.to_vec();
        ordered.sort_by_key(|p|p.id);
        let mut dedup=BTreeSet::new();
        for peer in ordered{
            if peer.id==self_id || !dedup.insert(peer.id)
                || !peer.position.iter().all(|x|x.is_finite())
            {continue;}
            let distance_sq=(0..3)
                .map(|i|(peer.position[i]-at[i]).powi(2)).sum::<f64>();
            if !distance_sq.is_finite()
                || distance_sq>SIGHT_RADIUS_BLOCKS*SIGHT_RADIUS_BLOCKS
            {continue;}
            if let Some(known)=self.peers.iter_mut().find(|p|p.sighting.id==peer.id){
                known.sighting=peer;
                known.last_seen=tick;
            }else{
                if self.peers.len()>=MAX_SOCIAL_PEERS{
                    let oldest=self.peers.iter().enumerate()
                        .min_by_key(|(_,p)|(p.last_seen,p.sighting.id))
                        .map(|(i,_)|i).expect("nonempty at cap");
                    self.peers.remove(oldest);
                }
                self.peers.push_back(Acquaintance{
                    sighting:peer,last_seen:tick,trust:0.0,
                });
            }
        }
    }
    /// Return only recently seen peers. Expired sightings cannot drive
    /// real-world movement, although bounded long-term trust remains.
    pub fn visible(&self,tick:u64)->Vec<PeerSighting>{
        self.peers.iter()
            .filter(|p|tick>=p.last_seen
                && tick-p.last_seen<=SIGHTING_TTL_TICKS)
            .map(|p|p.sighting).collect()
    }
    /// Called only after the game server verifies that help, sharing, or
    /// harm occurred. An attempted action or emitted packet is not enough.
    pub fn record_confirmed(
        &mut self,id:u128,interaction:ConfirmedInteraction
    )->bool{
        let Some(peer)=self.peers.iter_mut().find(|p|p.sighting.id==id)
            else{return false;};
        let delta=match interaction{
            ConfirmedInteraction::ReceivedHelp=>0.15,
            ConfirmedInteraction::SharedItem=>0.25,
            ConfirmedInteraction::WasAttacked=>-0.55,
            ConfirmedInteraction::WasThreatened=>-0.20,
        };
        peer.trust=(peer.trust+delta).clamp(-1.0,1.0);
        true
    }
    /// An individual trait biases an opinion, not a permanent scripted role.
    /// Real target selection still requires a CURRENT, local peer sighting.
    pub fn preference(&self,id:u128,sociability:f32)->SocialPreference{
        let Some(trust)=self.trust(id) else{return SocialPreference::Neutral;};
        if trust<=-0.40{SocialPreference::Avoid}
        else if sociability.is_finite() && sociability>=0.55 && trust>=0.20{
            SocialPreference::Approach
        }else{SocialPreference::Neutral}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn seen(id:u128,x:f64)->PeerSighting{
        PeerSighting{id,position:[x,64.0,0.5]}
    }
    #[test]
    fn sightings_are_local_finite_and_not_self(){
        let mut m=SocialMemory::new();
        m.observe(8,1,[0.5,64.0,0.5],&[
            seen(1,1.0),seen(2,3.0),seen(3,10.0),
            seen(4,f64::NAN),seen(2,3.0),
        ]);
        assert_eq!(m.known_count(),1);
        assert_eq!(m.visible(9),vec![seen(2,3.0)]);
        assert_eq!(m.trust(2),Some(0.0));
    }
    #[test]
    fn sightings_expire_but_social_trust_persists_temporarily(){
        let mut m=SocialMemory::new();
        m.observe(8,1,[0.5,64.0,0.5],&[seen(2,3.0)]);
        assert!(m.record_confirmed(2,ConfirmedInteraction::SharedItem));
        assert!(m.visible(249).is_empty());
        assert_eq!(m.trust(2),Some(0.25));
        m.observe(12_009,1,[0.5,64.0,0.5],&[]);
        assert_eq!(m.trust(2),None);
    }
    #[test]
    fn only_confirmed_events_change_trust_and_preference(){
        let mut m=SocialMemory::new();
        assert!(!m.record_confirmed(2,ConfirmedInteraction::SharedItem));
        m.observe(8,1,[0.5,64.0,0.5],&[seen(2,1.0)]);
        assert_eq!(m.preference(2,0.9),SocialPreference::Neutral);
        m.record_confirmed(2,ConfirmedInteraction::SharedItem);
        assert_eq!(m.preference(2,0.9),SocialPreference::Approach);
        assert_eq!(m.preference(2,0.2),SocialPreference::Neutral);
        m.record_confirmed(2,ConfirmedInteraction::WasAttacked);
        m.record_confirmed(2,ConfirmedInteraction::WasThreatened);
        assert_eq!(m.preference(2,0.9),SocialPreference::Avoid);
    }
    #[test]
    fn memory_is_bounded_and_duplicate_or_stale_surveys_do_not_overwrite(){
        let mut m=SocialMemory::new();
        for id in 2..50{
            m.observe(id as u64,1,[0.5,64.0,0.5],&[seen(id,1.0)]);
        }
        assert_eq!(m.known_count(),MAX_SOCIAL_PEERS);
        m.observe(10,1,[0.5,64.0,0.5],&[seen(2,1.0)]);
        assert_eq!(m.trust(2),None);
        assert_eq!(m.visible(50).len(),MAX_SOCIAL_PEERS);
    }
}
