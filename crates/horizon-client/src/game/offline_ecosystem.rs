//! Explicitly opt-in offline local-player experiment.
//!
//! All observations are drawn from authoritative Horizon server state;
//! no internet, cloud model, renderer or imaginary resource generation.
//! This is not complete terrain navigation, mining or multi-bot survival.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use ecosystem_core::{
    BotView, Intent, LocalObservation, Needs, Personality, choose_intent,
};
use horizon_client::lan::{guest::Guest, hub::Hub};
use horizon_offline_ecosystem_client::LocalPlayer;

/// Only the server observes actual Minecraft state. A bounded local survey is
/// sent to a worker; no world/guest references cross thread boundaries.
#[derive(Clone, Debug, PartialEq)]
struct Survey {
    tick: u64,
    position: [f64; 3],
    health: f32,
    food: i32,
    nearby_log: Option<[i32; 3]>,
    /// At most 32 nearby real server blocks; unrecognized/absent blocks
    /// are never treated as resources. Not yet an occlusion-aware view.
    logs: Vec<[i32; 3]>,
}
struct SurveySink {
    name: String,
    tx: SyncSender<Survey>,
}

/// Temporary one-thread-per-nearby-player structure, bounded to 3.
/// Realization/abstraction and lifecycle-aware join are future work.
pub(super) struct ExperimentalBots {
    stop: Arc<AtomicBool>,
    _workers: Vec<JoinHandle<()>>,
    senders: Vec<SurveySink>,
    survey_ticks: u64,
}
impl Drop for ExperimentalBots {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Never join a login thread on the server tick thread; a blocked
        // login may need that thread to finish. Hub shutdown closes its pipe.
    }
}
fn configured_count(value: Option<&str>) -> usize {
    value.and_then(|v| v.parse().ok())
        .filter(|n| (1..=3).contains(n))
        .unwrap_or(0)
}
fn profile(index: usize) -> Personality {
    const TRAITS: [[f32;3];3] = [
        [0.90,0.15,0.45],
        [0.50,0.80,0.20],
        [0.75,0.10,0.95],
    ];
    let p = TRAITS[index % TRAITS.len()];
    Personality::new(p[0],p[1],p[2]).expect("static traits are valid")
}
/// No omniscient inventories, food, threats, memories or paths are fabricated.
/// Those fields remain unknown until their server-side observation bridges
/// exist. Minecraft has no energy meter, so don't invent one for gameplay.
fn decide(survey: &Survey, index: usize) -> Intent {
    let needs = Needs::new(
        (survey.food.clamp(0,20)*5) as u8,
        100,
        (survey.health.clamp(0.,20.)*5.) as u8,
    ).expect("clamped Minecraft health and food");
    let bot=BotView {
        needs,
        personality:profile(index),
        food_inventory:0,
        wood_inventory:0,
    };
    choose_intent(&bot,&LocalObservation {
        local_wood:u16::from(survey.nearby_log.is_some()),
        ..Default::default()
    })
}

/// Deterministic, memory-bounded proximity scan. All predicate answers must
/// come from loaded, authoritative server blocks; never infer distant terrain.
fn log_candidates<F:FnMut([i32;3])->bool>(
    center:[i32;3],mut is_log:F
)->Vec<[i32;3]>{
    let mut found=Vec::new();
    for dx in -3i32..=3 {
        for dy in -1i32..=2 {
            for dz in -3i32..=3 {
                let p=[center[0]+dx,center[1]+dy,center[2]+dz];
                if is_log(p){
                    let d=dx.abs()+dy.abs()+dz.abs();
                    found.push((d,p));
                }
            }
        }
    }
    found.sort_by_key(|(distance,p)|(*distance,p[0],p[1],p[2]));
    found.truncate(32);
    found.into_iter().map(|(_,p)|p).collect()
}

impl ExperimentalBots {
    pub(super) fn start(hub:&Hub)->Option<Self>{
        let count=configured_count(
            std::env::var("HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS").ok().as_deref()
        );
        if count==0{return None;}
        let stop=Arc::new(AtomicBool::new(false));
        let mut workers=Vec::new();
        let mut senders=Vec::new();
        for i in 0..count {
            let name=format!("EcoAI{i}");
            let pipe=match hub.lan.connect_local(){
                Ok(pipe)=>pipe,
                Err(error)=>{
                    log::warn!("Offline ecosystem: local player {name}: {error}");
                    continue;
                }
            };
            let (tx,rx)=mpsc::sync_channel(1);
            let flag=stop.clone();
            let worker_name=name.clone();
            match thread::Builder::new().name(format!("eco-local-{i}"))
                .spawn(move||{
                    if let Err(error)=run_local(pipe,&worker_name,i,flag,rx){
                        log::warn!("Offline ecosystem {worker_name}: {error}");
                    }
                })
            {
                Ok(worker)=>{
                    workers.push(worker);
                    senders.push(SurveySink{name,tx});
                },
                Err(error)=>log::warn!("Offline ecosystem: worker spawn failed: {error}"),
            }
        }
        if workers.is_empty(){return None;}
        log::info!("Offline ecosystem: started {} opt-in local bots",workers.len());
        Some(Self{stop,_workers:workers,senders,survey_ticks:0})
    }

    /// Scan a *small, loaded neighborhood* every eight server ticks. This
    /// probes nearby logs, but is NOT visual line-of-sight perception yet;
    /// occluded blocks in that radius may be discovered. Never scan an
    /// unloaded chunk, never synthesize a block or resource.
    pub(super) fn survey<F>(&mut self, guests:&[Guest],dimension:&str,mut is_log:F)
    where F: FnMut([i32;3])->bool
    {
        self.survey_ticks+=1;
        if self.survey_ticks%8!=0{return;}
        for sink in &self.senders{
            let Some(guest)=guests.iter().find(|g|
                g.name==sink.name && g.dimension==dimension && g.placed &&
                g.loaded && !g.loading
            ) else {continue;};
            let pos=guest.position;
            if !pos.iter().all(|x|x.is_finite()){continue;}
            let center=[pos[0].floor() as i32,pos[1].floor() as i32,pos[2].floor() as i32];
            let logs=log_candidates(center,&mut is_log);
            let nearest=logs.first().copied();
            // One outstanding observation maximum per player. If a worker
            // is behind, discard instead of blocking the game tick.
            let _=sink.tx.try_send(Survey{
                tick:self.survey_ticks,
                position:pos,
                health:guest.health,
                food:guest.food,
                nearby_log:nearest,
                logs,
            });
        }
    }
}
fn run_local(
    pipe:horizon_protocol::connection::Pipe,
    name:&str,
    index:usize,
    stop:Arc<AtomicBool>,
    rx:Receiver<Survey>,
)->Result<(),String>{
    let mut player=LocalPlayer::join(pipe,name,2)?;
    let mut origin:Option<[f64;2]>=None;
    let mut waypoint=0usize;
    let mut next_tick=Instant::now();
    let mut last_survey=None;
    // The server enforces reach, progress and drops. The bot only records
    // attempted transactions; no resources are credited speculatively.
    let mut mining:Option<([i32;3],u64)>=None;
    let mut attempts:Option<([i32;3],u8)>=None;
    let mut mining_sequence=100i32;
    let mut mine_cooldown=0u64;
    const OFFSETS:[[f64;2];4]=[[0.8,0.0],[0.8,0.8],[0.0,0.8],[0.0,0.0]];
    while !stop.load(Ordering::Acquire){
        player.poll_one_timeout(Duration::from_millis(25))?;
        for received in rx.try_iter(){
            // This is the server-authoritative position and health; use it
            // to correct optimistic positions after collision/rejection.
            player.state.position=Some(received.position);
            last_survey=Some((received,Instant::now()));
        }
        if !player.state.ready||Instant::now()<next_tick{continue;}
        next_tick=Instant::now()+Duration::from_millis(50);
        let Some((survey,observed_at))=last_survey.as_ref() else {continue;};
        if observed_at.elapsed()>Duration::from_secs(2){continue;}
        let spawn=*origin.get_or_insert([survey.position[0],survey.position[2]]);
        // A START is a request, not proof of mining success. A block can
        // disappear due to another player or a world change; the observation
        // only establishes that it is no longer present.
        if let Some((target,started))=mining {
            if !survey.logs.contains(&target){
                player.abort_mining(target,1,mining_sequence)?;
                mining_sequence+=1;
                mining=None;
                attempts=None;
                log::debug!("Offline ecosystem {name}: observed mining target absent");
            } else if survey.tick.saturating_sub(started)>=85{
                player.finish_mining(target,1,mining_sequence)?;
                mining_sequence+=1;
                mining=None;
                mine_cooldown=survey.tick+32;
            } else {
                player.client_tick_end()?;
                continue;
            }
        }
        let mut exploring=false;
        match decide(survey,index){
            Intent::GatherWood=>{
                if let Some(log)=survey.nearby_log {
                    let height_diff=(survey.position[1]-f64::from(log[1])).abs();
                    let log_center=[f64::from(log[0])+0.5,f64::from(log[2])+0.5];
                    let dx=survey.position[0]-log_center[0];
                    let dz=survey.position[2]-log_center[1];
                    let distance=dx.hypot(dz);
                    if height_diff>=1.5{
                        exploring=true;
                    } else if distance>2.3 {
                        let target=[
                            log_center[0]+dx/distance*2.0,
                            log_center[1]+dz/distance*2.0,
                        ];
                        let _=player.steer_xz(target);
                    } else if mining.is_none() && survey.tick>=mine_cooldown{
                        let used=attempts.filter(|(p,_)|*p==log).map_or(0,|(_,n)|n);
                        if used<2{
                            player.begin_mining(log,1,mining_sequence)?;
                            mining_sequence+=1;
                            attempts=Some((log,used+1));
                            mining=Some((log,survey.tick));
                        } else {
                            exploring=true;
                        }
                    }
                } else {
                    exploring=true;
                }
            },
            Intent::Explore=>exploring=true,
            _=>{},
        }
        if exploring {
            let offset=OFFSETS[(waypoint+index)%OFFSETS.len()];
            if player.steer_xz([spawn[0]+offset[0],spawn[1]+offset[1]])==Ok(true){
                waypoint=(waypoint+1)%OFFSETS.len();
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
    #[test]fn decision_uses_only_observed_food_health_and_wood(){
        let mut survey=Survey{
            tick:0,
            position:[0.5,64.,0.5],food:20,health:20.,
            nearby_log:None, logs:vec![],
        };
        assert_eq!(decide(&survey,0),Intent::Explore);
        survey.nearby_log=Some([1,64,0]);
        assert_eq!(decide(&survey,0),Intent::GatherWood);
        survey.food=4;
        assert_eq!(decide(&survey,0),Intent::Explore);

    #[test]fn local_log_survey_is_bounded_and_nearest_first(){
        let origin=[0,64,0];
        let all=log_candidates(origin,|_|true);
        assert_eq!(all.len(),32);
        assert_eq!(all[0],origin);
        assert!(all.iter().all(|p|
            (p[0]-origin[0]).abs()<=3 &&
            (p[1]-origin[1]).abs()<=2 &&
            (p[2]-origin[2]).abs()<=3
        ));
        assert_eq!(log_candidates(origin,|p|p==[3,64,0]),vec![[3,64,0]]);
        assert!(log_candidates(origin,|_|false).is_empty());
    }
    }
}
