//! Experimental, explicitly opt-in local Minecraft bot players.
//!
//! This is a milestone-1 plumbing probe, NOT a full Rain World-style AI.
//! By default it allocates no worker, player or additional game state.
//! Set HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS=1..3 before starting the game.
//! No TCP, cloud service, LLM, additional renderer or external Minecraft jar.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool,Ordering},
    },
    thread::{self,JoinHandle},
    time::{Duration,Instant},
};
use horizon_client::lan::hub::Hub;
use horizon_offline_ecosystem_client::LocalPlayer;

/// Each experimental player has one headless thread. This is intentionally
/// capped at three until memory cost and the lifecycle are tested on a phone.
/// Future ecosystem implementation must multiplex nearby actors and abstract
/// all distant bots rather than allocating a full connection per individual.
pub(super) struct ExperimentalBots {
    stop: Arc<AtomicBool>,
    _workers: Vec<JoinHandle<()>>,
}
impl Drop for ExperimentalBots {
    fn drop(&mut self){
        self.stop.store(true,Ordering::Release);
        // Do NOT join while LanHost still owns Hub: an in-progress login
        // could be waiting for the server to tick and deadlock the shutdown.
        // Dropping the hub's pipes closes blocked reads after this returns.
    }
}
fn configured_count(value:Option<&str>)->usize{
    value.and_then(|text|text.parse::<usize>().ok())
        .filter(|n|(1..=3).contains(n)).unwrap_or(0)
}
impl ExperimentalBots {
    pub(super) fn start(hub:&Hub)->Option<Self>{
        let setting=std::env::var("HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS").ok();
        let count=configured_count(setting.as_deref());
        if count==0{return None;}
        let stop=Arc::new(AtomicBool::new(false));
        let mut workers=Vec::new();
        for i in 0..count{
            let name=format!("EcoAI{i}");
            let pipe=match hub.lan.connect_local(){
                Ok(pipe)=>pipe,
                Err(error)=>{
                    log::warn!("Offline ecosystem: cannot create local player {name}: {error}");
                    continue;
                }
            };
            let flag=stop.clone();
            let result=thread::Builder::new()
                .name(format!("eco-local-{i}"))
                .spawn(move||{
                    if let Err(error)=run_local(pipe,&name,i,flag){
                        log::warn!("Offline ecosystem {name}: {error}");
                    }
                });
            match result{
                Ok(worker)=>workers.push(worker),
                Err(error)=>log::warn!("Offline ecosystem: could not spawn worker: {error}"),
            }
        }
        if workers.is_empty(){return None;}
        log::info!("Offline ecosystem: started {} experimental local bots",workers.len());
        Some(Self{stop,_workers:workers})
    }
}
fn run_local(
    pipe:horizon_protocol::connection::Pipe,
    name:&str,
    index:usize,
    stop:Arc<AtomicBool>
)->Result<(),String>{
    let mut player=LocalPlayer::join(pipe,name,2)?;
    let mut origin=None;
    let mut waypoint=0usize;
    let mut next_tick=Instant::now();
    const OFFSETS:[[f64;2];4]=[[0.8,0.0],[0.8,0.8],[0.0,0.8],[0.0,0.0]];
    while !stop.load(Ordering::Acquire){
        player.poll_one_timeout(Duration::from_millis(25))?;
        if !player.state.ready {continue;}
        if Instant::now()<next_tick{continue;}
        next_tick=Instant::now()+Duration::from_millis(50);
        let spawn=*origin.get_or_insert_with(||{
            let p=player.state.position.expect("ready bot must have a position");
            [p[0],p[2]]
        });
        let desired=OFFSETS[(waypoint+index)%OFFSETS.len()];
        let target=[spawn[0]+desired[0],spawn[1]+desired[1]];
        match player.steer_xz(target){
            Ok(true)=>waypoint=(waypoint+1)%OFFSETS.len(),
            Ok(false)=>{},
            Err(error)=>{
                log::debug!("Offline ecosystem {name}: limited steering: {error}");
            }
        }
        player.client_tick_end()?;
    }
    Ok(())
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn opt_in_must_be_bounded(){
        assert_eq!(configured_count(None),0);
        assert_eq!(configured_count(Some("0")),0);
        assert_eq!(configured_count(Some("3")),3);
        assert_eq!(configured_count(Some("4")),0);
        assert_eq!(configured_count(Some("wrong")),0);
    }
}
