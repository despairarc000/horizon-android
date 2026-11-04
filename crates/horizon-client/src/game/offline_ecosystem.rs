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
        Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use ecosystem_core::{
    BotView, Intent, LocalObservation, Needs, Personality, choose_intent,
    navigation::TargetMemory,
    harvest::{HarvestEvidence,HarvestStatus},
    local_path::{first_ground_step,neighboring_resource_goals},
    explore::ExplorationMemory,
};
use horizon_client::{items::ItemStack, lan::{guest::Guest, hub::Hub}};
use horizon_offline_ecosystem_client::LocalPlayer;

/// Only the server observes actual Minecraft state. A bounded local survey is
/// sent to a worker; no world/guest references cross thread boundaries.
#[derive(Clone, Debug, PartialEq)]
struct Survey {
    tick: u64,
    position: [f64; 3],
    health: f32,
    food: i32,
    food_inventory: u16,
    wood_inventory: u16,
    hotbar_food_slot: Option<u8>,
    nearby_log: Option<[i32; 3]>,
    /// The exact requested mining target, only if its loaded block could be
    /// inspected by the authoritative server in this survey.
    watched_log: Option<([i32;3],bool)>,
    /// Loaded, approximately level ground tiles with full support and
    /// two-block clearance. Never an omniscient traversability map.
    ground: Vec<[i32;2]>,
    /// At most 32 nearby real server blocks; unrecognized/absent blocks
    /// are never treated as resources. Not yet an occlusion-aware view.
    logs: Vec<[i32; 3]>,
}
struct SurveySink {
    name: String,
    tx: SyncSender<Survey>,
    first_observation_logged: bool,
    watched_target: Arc<Mutex<Option<[i32;3]>>>,
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
pub(super) fn configured_count(value: Option<&str>) -> usize {
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
        food_inventory:survey.food_inventory,
        wood_inventory:survey.wood_inventory,
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

/// Inventory is the server's 36-slot survival inventory, hotbar first.
/// Only food already in the hotbar is currently actionable. No imaginary
/// supplies are added to the bot's decision state.
fn inventory_supply<F:FnMut(&str)->bool>(
    slots:&[Option<ItemStack>], mut is_edible:F
)->(u16,u16,Option<u8>){
    let mut food=0u32;
    let mut wood=0u32;
    let mut hotbar_food=None;
    for (i,stack) in slots.iter().enumerate(){
        let Some(stack)=stack.as_ref() else {continue;};
        if stack.count==0{continue;}
        let name=stack.item.as_ref();
        if name.ends_with("_log") || name.ends_with("_stem") || name.ends_with("_planks"){
            wood=wood.saturating_add(stack.count);
        }
        if i<9 && is_edible(name){
            food=food.saturating_add(stack.count);
            if hotbar_food.is_none(){hotbar_food=Some(i as u8);}
        }
    }
    (food.min(u32::from(u16::MAX)) as u16,
     wood.min(u32::from(u16::MAX)) as u16,
     hotbar_food)
}

/// Ground-only, bounded 7x7 sampling; the caller checks real loaded
/// blocks, physical floor, airspace, and fluid for every candidate.
#[cfg(test)]
fn ground_candidates<F:FnMut([i32;3])->bool>(
    center:[i32;3],mut is_standable:F
)->Vec<[i32;2]>{
    let mut out=Vec::with_capacity(49);
    for dx in -3..=3{
        for dz in -3..=3{
            let p=[center[0]+dx,center[1],center[2]+dz];
            if is_standable(p){out.push([p[0],p[2]]);}
        }
    }
    out
}

/// Probe nearby elevations without guessing walkability from a heightmap.
/// The caller validates support, clearance, fluids and loaded chunks for
/// every returned [x,y,z]. Vertical routes are not executed yet.
fn terrain_candidates<F:FnMut([i32;3])->bool>(
    center:[i32;3],mut is_standable:F
)->Vec<[i32;3]>{
    let mut out=Vec::with_capacity(147);
    for dx in -3..=3 {
        for dz in -3..=3 {
            for dy in [-1,0,1] {
                if let Some(y)=center[1].checked_add(dy){
                    let p=[center[0]+dx,y,center[2]+dz];
                    if is_standable(p){out.push(p);}
                }
            }
        }
    }
    out
}

/// The next ground tile on a real obstacle-aware, local route to a log.
/// No jump/swim/drop ability is inferred when the local grid has no route.
fn ground_step_to_log(survey:&Survey,log:[i32;3])->Option<[f64;2]>{
    let start=[survey.position[0].floor() as i32,survey.position[2].floor() as i32];
    let goals=neighboring_resource_goals(log,&survey.ground);
    let step=first_ground_step(start,&goals,&survey.ground)?;
    Some([f64::from(step[0])+0.5,f64::from(step[1])+0.5])
}
fn ground_step_to_pickup(survey:&Survey,log:[i32;3])->Option<[f64;2]>{
    let start=[survey.position[0].floor() as i32,survey.position[2].floor() as i32];
    let goals:Vec<_>=survey.ground.iter().copied().filter(|[x,z]|{
        let dx=i64::from(*x)-i64::from(log[0]);
        let dz=i64::from(*z)-i64::from(log[2]);
        dx*dx+dz*dz<=1
    }).collect();
    let step=first_ground_step(start,&goals,&survey.ground)?;
    Some([f64::from(step[0])+0.5,f64::from(step[1])+0.5])
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
            let watched_target=Arc::new(Mutex::new(None));
            let worker_target=watched_target.clone();
            let flag=stop.clone();
            let worker_name=name.clone();
            match thread::Builder::new().name(format!("eco-local-{i}"))
                .spawn(move||{
                    if let Err(error)=run_local(pipe,&worker_name,i,flag,rx,worker_target){
                        log::warn!("Offline ecosystem {worker_name}: {error}");
                    }
                })
            {
                Ok(worker)=>{
                    workers.push(worker);
                    senders.push(SurveySink{name,tx,first_observation_logged:false,watched_target});
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
    pub(super) fn survey<F,G,H>(&mut self, guests:&[Guest],dimension:&str,mut is_log:F,mut is_edible:G,mut is_standable:H)
    where F: FnMut([i32;3])->Option<bool>, G: FnMut(&str)->bool,H:FnMut([i32;3])->bool
    {
        self.survey_ticks+=1;
        if self.survey_ticks%8!=0{return;}
        for sink in &mut self.senders{
            let Some(guest)=guests.iter().find(|g|
                g.name==sink.name && g.dimension==dimension && g.placed &&
                g.loaded && !g.loading
            ) else {continue;};
            let pos=guest.position;
            if !pos.iter().all(|x|x.is_finite()){continue;}
            let center=[pos[0].floor() as i32,pos[1].floor() as i32,pos[2].floor() as i32];
            let logs=log_candidates(center,|here|is_log(here)==Some(true));
            let terrain=terrain_candidates(center,&mut is_standable);
            let ground:Vec<[i32;2]>=terrain.iter()
                .filter(|p|p[1]==center[1])
                .map(|p|[p[0],p[2]]).collect();
            let lower_count=terrain.iter().filter(|p|p[1]<center[1]).count();
            let higher_count=terrain.iter().filter(|p|p[1]>center[1]).count();
            let center_is_ground=ground.contains(&[center[0],center[2]]);
            let logs_count=logs.len();
            let ground_count=ground.len();
            let nearest=logs.first().copied();
            let watched_log=sink.watched_target.try_lock().ok()
                .and_then(|guard|*guard)
                .filter(|target|{
                    (target[0]-center[0]).abs()<=3
                        && (-1..=2).contains(&(target[1]-center[1]))
                        && (target[2]-center[2]).abs()<=3
                })
                .and_then(|target|is_log(target).map(|is_log|(target,is_log)));
            let (food_inventory,wood_inventory,hotbar_food_slot)=
                inventory_supply(&guest.inventory,&mut is_edible);
            // One outstanding observation maximum per player. If a worker
            // is behind, discard instead of blocking the game tick.
            let sent=sink.tx.try_send(Survey{
                tick:self.survey_ticks,
                position:pos,
                health:guest.health,
                food:guest.food,
                food_inventory,
                wood_inventory,
                hotbar_food_slot,
                nearby_log:nearest,
                watched_log,
                ground,
                logs,
            });
            if sent.is_ok() && !sink.first_observation_logged {
                // This occurs only after Horizon has accepted the player
                // and supplied a valid server-authoritative loaded position.
                // It is stronger evidence than the worker simply spawning.
                log::info!(
                    "Offline ecosystem {}: first authoritative survey at {:.2},{:.2},{:.2} logs={} ground_tiles={} terrain_down={} terrain_up={} current_ground={}",
                    sink.name,pos[0],pos[1],pos[2],logs_count,ground_count,
                    lower_count,higher_count,center_is_ground
                );
                sink.first_observation_logged=true;
            }
            if sent.is_ok() && self.survey_ticks%160==0 {
                log::info!(
                    "Offline ecosystem {}: exploration survey position {:.2},{:.2},{:.2} logs={} ground_tiles={} terrain_down={} terrain_up={} current_ground={} wood_inventory={}",
                    sink.name,pos[0],pos[1],pos[2],logs_count,ground_count,
                    lower_count,higher_count,center_is_ground,wood_inventory
                );
            }
        }
    }
}
fn run_local(
    pipe:horizon_protocol::connection::Pipe,
    name:&str,
    index:usize,
    stop:Arc<AtomicBool>,
    rx:Receiver<Survey>,
    watched_target:Arc<Mutex<Option<[i32;3]>>>,
)->Result<(),String>{
    let mut player=LocalPlayer::join(pipe,name,2)?;
    let mut exploration=ExplorationMemory::new();
    let mut next_tick=Instant::now();
    let mut last_survey=None;
    // Last-known resource locations and failed-approach history belong to
    // this individual, not a world-wide omniscient shared list.
    let mut target_memory=TargetMemory::new();
    let mut harvest:Option<HarvestEvidence>=None;
    // The server enforces reach, progress and drops. The bot only records
    // attempted transactions; no resources are credited speculatively.
    let mut mining:Option<([i32;3],u64)>=None;
    let mut attempts:Option<([i32;3],u8)>=None;
    let mut mining_sequence=100i32;
    let mut mine_cooldown=0u64;
    let mut food_cooldown=0u64;
    while !stop.load(Ordering::Acquire){
        player.poll_one_timeout(Duration::from_millis(25))?;
        for mut received in rx.try_iter(){
            if let Some(probe)=harvest.as_mut(){
                let exists=received.watched_log.and_then(|(pos,is_log)|
                    (pos==probe.target).then_some(is_log));
                match probe.observe(received.tick,exists,received.wood_inventory){
                    HarvestStatus::ObservedWoodGain{gained}=>{
                        log::info!(
                            "Offline ecosystem {name}: correlated log removal and real inventory gain +{gained} at {:?}",
                            probe.target
                        );
                        harvest=None;
                        attempts=None;
                        if let Ok(mut target)=watched_target.lock(){*target=None;}
                    },
                    HarvestStatus::UnverifiedTimeout=>{
                        log::info!(
                            "Offline ecosystem {name}: mining unverified (no correlated drop pickup)"
                        );
                        target_memory.avoid(probe.target,received.tick);
                        harvest=None;
                        if let Ok(mut target)=watched_target.lock(){*target=None;}
                    },
                    _=>{},
                }
            }
            // Only consider logs on approximately the same level while
            // vertical navigation and obstacle handling remain unavailable.
            // This also prevents getting stuck forever targeting tall trees.
            let walkable_height:Vec<[i32;3]>=received.logs.iter().copied()
                .filter(|p|(received.position[1]-f64::from(p[1])).abs()<1.5)
                .collect();
            // Only real, server-observed positions count as movement progress.
            // A remembered target can be investigated, but not treated as
            // confirmed still-existing or credited with any item rewards.
            received.nearby_log=target_memory.observe(
                received.tick,received.position,&walkable_height
            );
            player.state.position=Some(received.position);
            last_survey=Some((received,Instant::now()));
        }
        if !player.state.ready||Instant::now()<next_tick{continue;}
        next_tick=Instant::now()+Duration::from_millis(50);
        let Some((survey,observed_at))=last_survey.as_ref() else {continue;};
        if observed_at.elapsed()>Duration::from_secs(2){continue;}
        // A START is a request, not proof of mining success. A block can
        // disappear due to another player or a world change; the observation
        // only establishes that it is no longer present.
        if let Some((target,started))=mining {
            if survey.watched_log==Some((target,false)){
                player.abort_mining(target,1,mining_sequence)?;
                mining_sequence+=1;
                mining=None;
                attempts=None;
                log::info!("Offline ecosystem {name}: exact server block observation confirmed target no longer log");
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
        if let Some(probe)=harvest.as_ref().filter(|h|h.block_removed()){
            // Mining drops fall into the world rather than appearing in the
            // player's inventory. Move toward the former log to pick them up.
            let x=f64::from(probe.target[0])+0.5;
            let z=f64::from(probe.target[2])+0.5;
            let delta=(survey.position[0]-x).hypot(survey.position[2]-z);
            if delta>0.55 && (survey.position[1]-f64::from(probe.target[1])).abs()<1.5{
                if let Some(waypoint)=ground_step_to_pickup(survey,probe.target){
                    let _=player.steer_xz(waypoint);
                }
            }
            player.client_tick_end()?;
            continue;
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
                        if let Some(waypoint)=ground_step_to_log(survey,log) {
                            if matches!(player.steer_xz(waypoint),Ok(false)){
                                target_memory.attempted_step(log);
                            }
                        } else {
                            // Unknown or unsafe cells are never traversed.
                            // This is still only level-ground navigation.
                            exploring=true;
                        }
                    } else if !survey.logs.contains(&log) {
                        // Last-known location only: investigate, never mine
                        // an unobserved or already-removed block.
                        exploring=true;
                    } else if mining.is_none() && harvest.is_none() && survey.tick>=mine_cooldown{
                        let used=attempts.filter(|(p,_)|*p==log).map_or(0,|(_,n)|n);
                        if used<2{
                            player.begin_mining(log,1,mining_sequence)?;
                            mining_sequence+=1;
                            attempts=Some((log,used+1));
                            mining=Some((log,survey.tick));
                            harvest=Some(HarvestEvidence::new(
                                log,survey.tick,survey.wood_inventory
                            ));
                            if let Ok(mut target)=watched_target.lock(){*target=Some(log);}
                            log::info!("Offline ecosystem {name}: mining attempt started at {:?}",log);
                        } else {
                            // Tried twice without a confirmed block removal.
                            // Consider a different observed log for a while.
                            target_memory.avoid(log,survey.tick);
                            exploring=true;
                        }
                    }
                } else {
                    exploring=true;
                }
            },
            Intent::Eat=>{
                if let Some(slot)=survey.hotbar_food_slot{
                    if survey.tick>=food_cooldown{
                        player.select_hotbar(slot)?;
                        player.use_selected_item(mining_sequence)?;
                        mining_sequence+=1;
                        food_cooldown=survey.tick+40;
                    }
                }
            },
            Intent::Explore|Intent::BuildShelter=>exploring=true,
            _=>{},
        }
        if exploring {
            let at=[
                survey.position[0].floor() as i32,
                survey.position[2].floor() as i32,
            ];
            if let Some(step)=exploration.choose_next(
                at,&survey.ground,survey.tick,index as u32
            ){
                // This can lead well beyond the initial spawn, but only
                // through the level-ground cells currently observed.
                let target=[f64::from(step[0])+0.5,f64::from(step[1])+0.5];
                let _=player.steer_xz(target);
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
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,ground:vec![[0,0]], logs:vec![],
        };
        assert_eq!(decide(&survey,0),Intent::Explore);
        survey.nearby_log=Some([1,64,0]);
        assert_eq!(decide(&survey,0),Intent::GatherWood);
        survey.food=4;
        assert_eq!(decide(&survey,0),Intent::Explore);
        survey.food_inventory=1;
        survey.hotbar_food_slot=Some(2);
        assert_eq!(decide(&survey,0),Intent::Eat);
    }
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
    #[test]fn inventory_uses_only_real_item_stacks_and_hotbar_food(){
        use std::sync::Arc;
        let mut slots=vec![None;36];
        slots[2]=Some(ItemStack{item:Arc::from("minecraft:apple"),count:3,damage:0,extra:None});
        slots[16]=Some(ItemStack{item:Arc::from("minecraft:oak_log"),count:7,damage:0,extra:None});
        slots[23]=Some(ItemStack{item:Arc::from("minecraft:bread"),count:5,damage:0,extra:None});
        let (food,wood,slot)=inventory_supply(&slots,|name|
            name=="minecraft:apple" || name=="minecraft:bread"
        );
        assert_eq!((food,wood,slot),(3,7,Some(2)));
        slots[2]=None;
        let (food,wood,slot)=inventory_supply(&slots,|name|
            name=="minecraft:apple" || name=="minecraft:bread"
        );
        assert_eq!((food,wood,slot),(0,7,None));
    }
    #[test]fn server_ground_survey_is_local_and_not_omniscient(){
        let ground=ground_candidates([10,64,20],|p|p[0]==10 && p[2]>=20);
        assert_eq!(ground.len(),4);
        assert_eq!(ground[0],[10,20]);
        assert_eq!(ground[3],[10,23]);
    }
    #[test]fn terrain_probe_find_stands_at_nearby_different_heights(){
        let all=terrain_candidates([10,64,20],|[x,y,z]|
            (x==10&&z==20&&y==64) ||
            (x==11&&z==20&&y==65) ||
            (x==9&&z==20&&y==63)
        );
        assert!(all.contains(&[10,64,20]));
        assert!(all.contains(&[11,65,20]));
        assert!(all.contains(&[9,63,20]));
        assert_eq!(all.len(),3);
        let none=terrain_candidates([10,64,20],|_|false);
        assert!(none.is_empty());
    }
    #[test]fn local_route_avoids_unobserved_wall(){
        let mut s=Survey{
            tick:0,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([3,64,0]),watched_log:None,logs:vec![[3,64,0]],
            ground:vec![[0,0],[0,1],[1,1],[2,1],[3,1]],
        };
        assert_eq!(ground_step_to_log(&s,[3,64,0]),Some([0.5,1.5]));
        s.ground=vec![[0,0],[3,1]];
        assert_eq!(ground_step_to_log(&s,[3,64,0]),None);
    }

}
