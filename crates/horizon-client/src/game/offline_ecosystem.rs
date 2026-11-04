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
    explore::{ExplorationMemory,VerticalTransitionMemory},
    terrain_path::{first_walk_down_step,first_terrain_step,best_walk_down_frontier,RouteMotion,RouteStep,StepDownController,StepUpController},
};
use horizon_client::{items::ItemStack, lan::{guest::Guest, hub::Hub}};
use horizon_offline_ecosystem_client::LocalPlayer;

/// Only the server observes actual Minecraft state. A bounded local survey is
/// sent to a worker; no world/guest references cross thread boundaries.
#[derive(Clone, Debug, PartialEq)]
struct Survey {
    tick: u64,
    /// The server accepted the actual ground-contact state. A matching Y
    /// coordinate during a jump arc does not prove a completed landing.
    on_ground: bool,
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
    /// Up to 363 server-verified 11x11x3 standing surfaces for routing.
    terrain: Vec<[i32;3]>,
    /// Real log cells whose verified support and airspace would become a
    /// valid standing surface AFTER this exact log is removed. This is only
    /// a provisional route goal, never a currently walkable block.
    clearable_stands: Vec<[i32;3]>,
    /// At most 32 nearby real server blocks within a 13x13 search.
    /// Only loaded cells; not yet an occlusion-aware view.
    logs: Vec<[i32; 3]>,
    /// Real nearby wood item entities, not invented loot or predicted positions.
    wood_drops: Vec<[f64;3]>,
}
/// Transport-level steering evidence. Packet emission is NEVER credited as
/// server-acknowledged movement: the next authoritative survey decides that.
#[derive(Default)]
struct SteeringEvidence {
    packets_sent: u64,
    waypoints_reached: u64,
    rejected: u64,
    last_error: Option<String>,
}
impl SteeringEvidence {
    fn steer(&mut self, player: &mut LocalPlayer, target: [f64; 2]) -> bool {
        match player.steer_xz(target) {
            Ok(false) => {
                self.packets_sent = self.packets_sent.saturating_add(1);
                true
            }
            Ok(true) => {
                self.waypoints_reached = self.waypoints_reached.saturating_add(1);
                false
            }
            Err(err) => {
                self.rejected = self.rejected.saturating_add(1);
                self.last_error = Some(err);
                false
            }
        }
    }
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

/// Deterministic, memory-bounded scan. Query only loaded authoritative
/// blocks. Rank potential accessibility BEFORE taking the nearest 32;
/// otherwise dense inaccessible trunks can hide nearby collectible wood.
fn ranked_log_candidates<F,G>(
    center:[i32;3],mut is_log:F,mut access_rank:G
)->Vec<[i32;3]>
where F:FnMut([i32;3])->bool,G:FnMut([i32;3])->u8 {
    let mut found=Vec::new();
    for dx in -6i32..=6 {
        for dy in -1i32..=2 {
            for dz in -6i32..=6 {
                let (Some(x),Some(y),Some(z))=(
                    center[0].checked_add(dx),
                    center[1].checked_add(dy),
                    center[2].checked_add(dz),
                )else{continue;};
                let p=[x,y,z];
                if is_log(p){
                    let distance=dx.abs()+dy.abs()+dz.abs();
                    found.push((access_rank(p),distance,p));
                }
            }
        }
    }
    found.sort_by_key(|(rank,distance,p)|(*rank,*distance,p[0],p[1],p[2]));
    found.truncate(32);
    found.into_iter().map(|(_,_,p)|p).collect()
}
/// Preserve deterministic nearest-first scanning in legacy isolated tests.
#[cfg(test)]
fn log_candidates<F:FnMut([i32;3])->bool>(
    center:[i32;3],is_log:F
)->Vec<[i32;3]>{
    ranked_log_candidates(center,is_log,|_|0)
}

/// A *potential* settled log drop is one or two blocks above a supported
/// player stance, or at the same level. Only already graph-connected
/// server-verified terrain counts. This is a cheap filter, NOT movement,
/// actual item spawn geometry, or proof that its pickup will succeed.
fn has_connected_pickup_stand(
    connected:&std::collections::BTreeSet<[i32;3]>,log:[i32;3]
)->bool{
    for down in 0..=2{
        for dx in -1..=1{
            for dz in -1..=1{
                if let (Some(x),Some(y),Some(z))=(
                    log[0].checked_add(dx),
                    log[1].checked_sub(down),
                    log[2].checked_add(dz)
                ){
                    if connected.contains(&[x,y,z]){return true;}
                }
            }
        }
    }
    false
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

/// Probe an 11x11x3 bounded neighborhood without guessing walkability.
/// Every returned surface has server-verified support, body clearance,
/// absence of blocking fluids, and a loaded chunk. Walking and one-block
/// descending are supported; jumping is still not implemented.
fn terrain_candidates<F:FnMut([i32;3])->bool>(
    center:[i32;3],mut is_standable:F
)->Vec<[i32;3]>{
    let mut out=Vec::with_capacity(363);
    for dx in -5i32..=5 {
        for dz in -5i32..=5 {
            for dy in [-1,0,1] {
                if let (Some(x),Some(y),Some(z))=(
                    center[0].checked_add(dx),
                    center[1].checked_add(dy),
                    center[2].checked_add(dz),
                ) {
                    let p=[x,y,z];
                    if is_standable(p){out.push(p);}
                }
            }
        }
    }
    out
}

/// Prefer logs whose drops may fall into the standing player's real
/// pickup box. Breaking support blocks a level below the feet is valid
/// survival mining, but their drops can settle below pickup height without
/// a proper descent controller. This affects only target order, not sensing,
/// block access permissions, or inventory credits.
fn prefer_pickup_height(logs:&mut [[i32;3]], feet:[f64;3]){
    let center=[feet[0].floor() as i32,feet[1].floor() as i32,feet[2].floor() as i32];
    logs.sort_by_key(|p|{
        let dx=(i64::from(p[0])-i64::from(center[0])).abs();
        let dz=(i64::from(p[2])-i64::from(center[2])).abs();
        let dy=(i64::from(p[1])-i64::from(center[1])).abs();
        (p[1]<center[1],dx+dz,dy,p[0],p[1],p[2])
    });
}

/// Candidate pre-harvest standing cells come ONLY from Horizon's verified
/// loaded-world terrain scan. An item is not guaranteed to spawn at the
/// block's center after destruction, so these are conservative *prospects*,
/// never evidence of a spawned entity or credited inventory.
fn prospective_pickup_stands(survey:&Survey,log:[i32;3])->Vec<[i32;3]>{
    survey.terrain.iter().copied().filter(|stand|{
        let dx=(i64::from(stand[0])-i64::from(log[0])).abs();
        let dz=(i64::from(stand[2])-i64::from(log[2])).abs();
        let feet=f64::from(stand[1]);
        let drop_floor=f64::from(log[1]);
        dx<=1 && dz<=1
            && drop_floor>feet-0.6 && drop_floor<feet+2.4
    }).collect()
}

/// A geometrically nearby stand is useless if the bot cannot walk, descend
/// or jump there through *connected* server-verified ground. All routes are
/// tentative movement plans; the server must independently accept the steps.
fn pickup_route_to_log(survey:&Survey,log:[i32;3])->Option<RouteStep>{
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let goals=prospective_pickup_stands(survey,log);
    first_walk_down_step(start,&goals,&survey.terrain)
        .or_else(||first_terrain_step(start,&goals,&survey.terrain))
}

/// Number of potentially useful, already-verified pickup stands *only if*
/// at least one is connected to this bot's current authoritative position.
/// This never infers a real future drop or a successful inventory change.
fn predicted_pickup_stands(survey:&Survey,log:[i32;3])->usize{
    let goals=prospective_pickup_stands(survey,log);
    if goals.is_empty() {return 0;}
    if pickup_route_to_log(survey,log).is_some() {goals.len()} else {0}
}
/// Only a real server-observed log with solid floor support and clear body
/// space above may become a safe new standing position after removal.
fn cleared_log_route(survey:&Survey,log:[i32;3])->Option<RouteStep>{
    if !survey.clearable_stands.contains(&log){return None;}
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    // A hypothetical stand can be used as a GOAL after a successful break,
    // never as a currently executable route or an item pickup credit.
    let mut after=survey.terrain.clone();
    if after.len()>=ecosystem_core::terrain_path::MAX_KNOWN_STANDS {return None;}
    after.push(log);
    first_walk_down_step(start,&[log],&after)
        .or_else(||first_terrain_step(start,&[log],&after))
}
/// Zero: currently connected pickup stand, one: opening a new supported stand
/// by breaking this observed log, two: no currently evidenced pickup access.
/// The lowest rank wins; all blocks are still subject to server permissions.
fn harvest_access_rank(survey:&Survey,log:[i32;3])->u8{
    if predicted_pickup_stands(survey,log)>0{0}
    else if cleared_log_route(survey,log).is_some(){1}
    else{2}
}
fn prioritize_collectible_logs(logs:&mut [[i32;3]], survey:&Survey){
    // One cached key per bounded candidate; stable ties retain the original
    // nearest/highest ordering and avoid pathfinding in each comparison.
    logs.sort_by_cached_key(|log|harvest_access_rank(survey,*log));
}

/// The next ground tile on a real obstacle-aware, local route to a log.
/// No jump/swim/drop ability is inferred when the local grid has no route.
fn ground_step_to_log(survey:&Survey,log:[i32;3])->Option<[f64;2]>{
    let start=[survey.position[0].floor() as i32,survey.position[2].floor() as i32];
    let goals=neighboring_resource_goals(log,&survey.ground);
    let first=first_ground_step(start,&goals,&survey.ground).or_else(||{
        // A visible tree can lie OUTSIDE the 7x7 motion survey. Don't
        // discard it or magically cross unknown cells: route to the nearest
        // actually reachable frontier cell that gets closer to the tree.
        let distance=|p:[i32;2]|{
            (i64::from(p[0])-i64::from(log[0])).abs()
                +(i64::from(p[1])-i64::from(log[2])).abs()
        };
        let here=distance(start);
        let mut frontiers:Vec<_>=survey.ground.iter().copied()
            .filter(|&p|p!=start && distance(p)<here).collect();
        frontiers.sort_by_key(|&p|(distance(p),p[0],p[1]));
        frontiers.into_iter().find_map(|goal|
            first_ground_step(start,&[goal],&survey.ground)
        )
    })?;
    Some([f64::from(first[0])+0.5,f64::from(first[1])+0.5])
}
/// A connected, physically executable local route to a real observed
/// trunk. A target above the player's mining reach is never silently
/// upgraded into a jump, flight or fabricated resource.
fn terrain_step_to_log(survey:&Survey,log:[i32;3])->Option<RouteStep>{
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let mineable=|p:[i32;3]|{
        let vertical=f64::from(log[1])-f64::from(p[1]);
        let dx=f64::from(p[0])-f64::from(log[0]);
        let dz=f64::from(p[2])-f64::from(log[2]);
        (-1.5..=2.5).contains(&vertical)
            && dx*dx+dz*dz <= 2.3f64.powi(2)
    };
    let goals:Vec<_>=survey.terrain.iter().copied()
        .filter(|&p|mineable(p)).collect();
    if let Some(route)=first_walk_down_step(start,&goals,&survey.terrain){
        return Some(route);
    }
    // Only when walk/down-only paths cannot reach the real trunk do we
    // consider a server-verified one-block uphill jump.
    if let Some(route)=first_terrain_step(start,&goals,&survey.terrain){
        return Some(route);
    }
    // If the trunk is beyond this bounded 3D survey, follow a connected
    // frontier on real ground, permitting a detour and one-block descents.
    // All movement still requires independent server collision approval.
    best_walk_down_frontier(start,&survey.terrain,|p|{
        let dx=f64::from(p[0])-f64::from(log[0]);
        let dz=f64::from(p[2])-f64::from(log[2]);
        let vertical=f64::from(log[1])-f64::from(p[1]);
        let outside=if vertical < -1.5 {-1.5-vertical}
            else if vertical>2.5 {vertical-2.5}
            else {0.0};
        dx*dx+dz*dz+16.0*outside*outside
    })
}

/// Prefer observed wood that can actually be approached from the
/// bot's current server-known ground. Otherwise the first visible tree can
/// monopolize every decision even when it is behind a cliff or solid wall.
fn can_approach_log(survey:&Survey,log:[i32;3])->bool{
    // Survival players normally mine using an eye-height reach. A log two
    // blocks above the feet can be legitimate and its drop may fall into
    // the player's pickup box. Avoid mining two blocks BELOW unsupported
    // feet, where the loot would be even less reachable.
    let dy=f64::from(log[1])-survey.position[1];
    if !(-1.5..=2.5).contains(&dy) {
        return false;
    }
    let dx=survey.position[0]-f64::from(log[0])-0.5;
    let dz=survey.position[2]-f64::from(log[2])-0.5;
    dx.hypot(dz)<=2.3 || ground_step_to_log(survey,log).is_some()
        || terrain_step_to_log(survey,log).is_some()
}

/// When ordinary exploration is trapped on a tiny plateau, follow only
/// connected server-verified standing surfaces to a one-block lower level.
/// No jump-only or unknown transitions are allowed.
fn exploratory_downhill_step(
    survey:&Survey,avoid:impl IntoIterator<Item=[i32;3]>
)->Option<RouteStep>{
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let avoid:Vec<_>=avoid.into_iter().collect();
    let safe:Vec<_>=survey.terrain.iter().copied()
        .filter(|p|*p==start || !avoid.contains(p)).collect();
    let lower:Vec<_>=safe.iter().copied()
        .filter(|p|p[1]<start[1]).collect();
    first_walk_down_step(start,&lower,&safe)
}
/// No descending connection? A verified one-block jump can open a new
/// plateau and prevent being trapped at an isolated lower elevation.
/// This only chooses collision-checked standing surfaces; all jump packets
/// still need normal server approval and later authoritative landing.
fn exploratory_uphill_step(
    survey:&Survey,avoid:impl IntoIterator<Item=[i32;3]>
)->Option<RouteStep>{
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let avoid:Vec<_>=avoid.into_iter().collect();
    let safe:Vec<_>=survey.terrain.iter().copied()
        .filter(|p|*p==start || !avoid.contains(p)).collect();
    let higher:Vec<_>=safe.iter().copied()
        .filter(|p|p[1]>start[1]).collect();
    first_terrain_step(start,&higher,&safe)
}

/// Vertical transitions can reveal new wood even if NO tree is currently
/// visible. Once all connected, presently observed flat cells have genuinely
/// been visited, seeking another loaded standing elevation is preferable to
/// looping forever through old ground. Missing chunks remain impassable.
fn should_seek_vertical(flat_exhausted:bool,has_flat_step:bool)->bool{
    flat_exhausted || !has_flat_step
}

/// A server-published player location is only a *landing* if its
/// ground-contact flag is set and the current collision survey still proves
/// the intended destination has physical support and body clearance.
fn server_confirmed_landing(survey:&Survey,target:[i32;3])->bool{
    survey.on_ground
        && survey.terrain.contains(&target)
        && (survey.position[1]-f64::from(target[1])).abs()<0.12
        && (survey.position[0]-f64::from(target[0])-0.5).abs()<0.65
        && (survey.position[2]-f64::from(target[2])-0.5).abs()<0.65
}

/// Locate a real nearby wood drop whose spawn or subsequent physics place
/// it close to the watched break. This is spatial evidence, not ownership.
fn watched_wood_drop(survey:&Survey,log:[i32;3])->Option<[f64;3]>{
    let center=[f64::from(log[0])+0.5,f64::from(log[1])+0.5,f64::from(log[2])+0.5];
    survey.wood_drops.iter().copied()
        .filter(|p|p.iter().all(|v|v.is_finite()))
        .filter(|p|(p[0]-center[0]).abs()<2.0
            && (p[1]-center[1]).abs()<2.5
            && (p[2]-center[2]).abs()<2.0)
        .min_by(|a,b|{
            let score=|p:[f64;3]|(p[0]-center[0]).powi(2)
                +(p[1]-center[1]).powi(2)+(p[2]-center[2]).powi(2);
            score(*a).total_cmp(&score(*b))
        })
}
/// The integrated server already verified support and collision at each
/// surveyed standing surface. A newly spawned item can be *observed* even
/// when no known surface is within the server's actual pickup box.
/// Do not equate observed loot or a theoretical path with inventory gain.
fn has_surveyed_pickup_stand(survey:&Survey,drop:[f64;3])->bool{
    drop.iter().all(|v|v.is_finite()) && survey.terrain.iter().any(|p|{
        let feet=f64::from(p[1]);
        (f64::from(p[0])+0.5-drop[0]).abs()<1.5
            && (f64::from(p[2])+0.5-drop[2]).abs()<1.5
            && drop[1]>feet-0.6 && drop[1]<feet+2.4
    })
}

/// Safe standing cells are supplied by Horizon's loaded-world collision
/// survey. The actual item height must fit the game's pickup bounding box.
fn ground_step_to_drop(survey:&Survey, drop:[f64;3])->Option<[f64;2]>{
    if drop[1]<=survey.position[1]-0.6 || drop[1]>=survey.position[1]+2.4 {
        return None;
    }
    let start=[survey.position[0].floor() as i32,survey.position[2].floor() as i32];
    let goals:Vec<_>=survey.ground.iter().copied().filter(|[x,z]|{
        (f64::from(*x)+0.5-drop[0]).abs()<1.5
            && (f64::from(*z)+0.5-drop[2]).abs()<1.5
    }).collect();
    let step=first_ground_step(start,&goals,&survey.ground)?;
    Some([f64::from(step[0])+0.5,f64::from(step[1])+0.5])
}
/// Identify actual safe-level changes toward a drop that cannot be collected
/// from the current feet elevation. These are PLANS, not movement packets.
/// Return the next SERVER-CONFIRMED standing surface toward an actual
/// dropped item. If its pickup area is outside this local 7x7x3 window,
/// approach along connected, safe frontier cells rather than pretending
/// the distant item is reachable or moving through unobserved blocks.
fn terrain_step_to_drop(survey:&Survey,drop:[f64;3])->Option<RouteStep>{
    if !drop.iter().all(|x|x.is_finite()){return None;}
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let pickup_goal=|p:[i32;3]|{
        let feet=f64::from(p[1]);
        drop[1]>feet-0.6 && drop[1]<feet+2.4
            && (f64::from(p[0])+0.5-drop[0]).abs()<1.5
            && (f64::from(p[2])+0.5-drop[2]).abs()<1.5
    };
    let goals:Vec<[i32;3]>=survey.terrain.iter().copied()
        .filter(|&p|pickup_goal(p)).collect();
    if let Some(route)=first_walk_down_step(start,&goals,&survey.terrain){
        return Some(route);
    }
    // A verified one-block jump may reach the item when downhill-only
    // routes cannot. The controller still needs server-confirmed landing.
    if let Some(route)=first_terrain_step(start,&goals,&survey.terrain){
        return Some(route);
    }
    // Prefer reaching the item's *height* first; a small horizontal
    // shortcut on the high ledge cannot pick up a drop on lower ground.
    // One bounded BFS finds the best reachable frontier without repeated
    // graph searches. It may first walk AWAY from the item to go around
    // a physical wall, but never invents a jump or unexplored cell.
    best_walk_down_frontier(start,&survey.terrain,|p|{
        let x=f64::from(p[0])+0.5-drop[0];
        let z=f64::from(p[2])+0.5-drop[2];
        let feet=f64::from(p[1]);
        let vertical=if drop[1]<feet-0.6{
            feet-0.6-drop[1]
        }else if drop[1]>feet+2.4{
            drop[1]-feet-2.4
        }else{0.0};
        x*x+z*z+256.0*vertical*vertical
    })
}

/// Scavenge only actual nearby item entities published by the server.
/// Unlike mining correlation, these can belong to *any* bot or be left
/// behind by a previous attempt. Require a CONNECTED path to a verified
/// pickup-position stand, not just a plausible-looking frontier.
fn scavengable_wood_route(survey:&Survey)->Option<([f64;3],RouteStep)>{
    let start=[
        survey.position[0].floor() as i32,
        survey.position[1].floor() as i32,
        survey.position[2].floor() as i32,
    ];
    let mut drops:Vec<_>=survey.wood_drops.iter().copied()
        .filter(|d|d.iter().all(|v|v.is_finite()))
        .collect();
    let distance=|d:[f64;3]| {
        (d[0]-survey.position[0]).powi(2)
            +(d[1]-survey.position[1]).powi(2)
            +(d[2]-survey.position[2]).powi(2)
    };
    drops.sort_by(|a,b|distance(*a).total_cmp(&distance(*b)));
    for drop in drops {
        let goals:Vec<_>=survey.terrain.iter().copied().filter(|p|{
            let feet=f64::from(p[1]);
            drop[1]>feet-0.6 && drop[1]<feet+2.4
                && (f64::from(p[0])+0.5-drop[0]).abs()<1.5
                && (f64::from(p[2])+0.5-drop[2]).abs()<1.5
        }).collect();
        let route=first_walk_down_step(start,&goals,&survey.terrain)
            .or_else(||first_terrain_step(start,&goals,&survey.terrain));
        if let Some(route)=route{return Some((drop,route));}
    }
    None
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
    pub(super) fn survey<F,G,H,J,K>(&mut self, guests:&[Guest],dimension:&str,mut is_log:F,mut is_edible:G,mut is_standable:H,mut item_positions:J,mut clearable_log:K)
    where F: FnMut([i32;3])->Option<bool>, G: FnMut(&str)->bool,H:FnMut([i32;3])->bool,
        J:FnMut([f64;3])->Vec<[f64;3]>,K:FnMut([i32;3])->bool
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
            let terrain=terrain_candidates(center,&mut is_standable);
            let connected:std::collections::BTreeSet<_>=
                ecosystem_core::terrain_path::connected_terrain_stands(
                    center,&terrain
                ).into_iter().collect();
            // Favor genuinely connected, loaded-world pickup stances over
            // provisional post-break clearances, then inaccessible logs.
            // The worker repeats full route checks before taking any action.
            let logs=ranked_log_candidates(
                center,|here|is_log(here)==Some(true),|here|{
                    if has_connected_pickup_stand(&connected,here){0}
                    else if clearable_log(here){1}
                    else{2}
                }
            );
            let clearable_stands:Vec<_>=logs.iter().copied()
                .filter(|&pos|clearable_log(pos)).collect();
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
            let mut wood_drops=item_positions(pos);
            // Redundant input boundary: no arbitrary world-wide item list.
            wood_drops.retain(|p|p.iter().all(|v|v.is_finite())
                && (p[0]-pos[0]).abs()<=6.0
                && (p[1]-pos[1]).abs()<=4.0
                && (p[2]-pos[2]).abs()<=6.0);
            wood_drops.truncate(16);
            // One outstanding observation maximum per player. If a worker
            // is behind, discard instead of blocking the game tick.
            let sent=sink.tx.try_send(Survey{
                tick:self.survey_ticks,
                on_ground:guest.on_ground,
                position:pos,
                health:guest.health,
                food:guest.food,
                food_inventory,
                wood_inventory,
                hotbar_food_slot,
                nearby_log:nearest,
                watched_log,
                ground,
                terrain,
                clearable_stands,
                logs,
                wood_drops,
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
    let mut resource_confined=false;
    let mut vertical_memory=VerticalTransitionMemory::new();
    let mut steering=SteeringEvidence::default();
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
    // A real one-block falling request may span multiple client ticks.
    // Only server-authoritative positions can confirm the final landing.
    let mut descent:Option<(StepDownController,u64)>=None;
    let mut descent_cooldown=0u64;
    let mut ascent:Option<(StepUpController,u64)>=None;
    let mut ascent_cooldown=0u64;
    let mut last_route_report_tick=None;
    // Count authoritative observations, not speculative client ticks.
    // Inaccessible loot can be left behind; it is never credited as wood.
    let mut blocked_pickup_surveys=0u8;
    while !stop.load(Ordering::Acquire){
        player.poll_one_timeout(Duration::from_millis(25))?;
        for mut received in rx.try_iter(){
            if let Some(probe)=harvest.as_mut(){
                let exists=received.watched_log.and_then(|(pos,is_log)|
                    (pos==probe.target).then_some(is_log));
                let already_removed=probe.block_removed();
                let result=probe.observe(received.tick,exists,received.wood_inventory);
                if !already_removed && probe.block_removed() {
                    // One authoritative loaded-block observation per target.
                    // This also records same-survey drop pickup outcomes.
                    log::info!(
                        "Offline ecosystem {name}: exact server block observation confirmed target no longer log at {:?}",
                        probe.target
                    );
                }
                match result {
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
            // If a REAL spawned item repeatedly has no server-verified
            // standing surface in pickup range, stop waiting for an
            // impossible *currently known* route. Resume ordinary resource
            // search rather than wasting all 400 ticks on a sealed trunk.
            let inaccessible=harvest.as_ref()
                .filter(|probe|probe.block_removed())
                .and_then(|probe|watched_wood_drop(&received,probe.target))
                .is_some_and(|drop|!has_surveyed_pickup_stand(&received,drop));
            blocked_pickup_surveys=if inaccessible {
                blocked_pickup_surveys.saturating_add(1)
            } else {0};
            if blocked_pickup_surveys>=8 {
                if let Some(unverified)=harvest.take(){
                    log::info!(
                        "Offline ecosystem {name}: observed drop still inaccessible after 8 server surveys; no pickup or inventory credit for {:?}",
                        unverified.target
                    );
                    target_memory.avoid(unverified.target,received.tick);
                    attempts=None;
                    if let Ok(mut target)=watched_target.lock(){*target=None;}
                }
                blocked_pickup_surveys=0;
            }
            // Retain an active verified descent even when exploring without
            // a harvested block; it has its own bounded timeout.
            // Only consider logs on approximately the same level while
            // full vertical navigation remains unavailable.
            // This also prevents getting stuck forever targeting tall trees.
            let mut walkable_height:Vec<[i32;3]>=received.logs.iter().copied()
                .filter(|p|{
                    let dy=f64::from(p[1])-received.position[1];
                    (-1.5..=2.5).contains(&dy)
                })
                .collect();
            prefer_pickup_height(&mut walkable_height,received.position);
            walkable_height.retain(|&log|can_approach_log(&received,log));
            prioritize_collectible_logs(&mut walkable_height,&received);
            // Don't sacrifice additional trunks merely because they are
            // within mining reach. Only a connected pickup stand or a
            // server-verified post-break clearance can justify this task.
            // Otherwise explore until another real resource is approachable.
            walkable_height.retain(|&log|harvest_access_rank(&received,log)<2);
            // Only real, server-observed positions count as movement progress.
            // A remembered target can be investigated, but not treated as
            // confirmed still-existing or credited with any item rewards.
            received.nearby_log=target_memory.observe(
                received.tick,received.position,&walkable_height
            );
            let usable_wood=received.nearby_log
                .is_some_and(|p|walkable_height.contains(&p));
            let now_confined=exploration.resource_search_confined(
                received.tick,
                [received.position[0],received.position[2]],
                usable_wood
            );
            if now_confined && !resource_confined {
                log::info!(
                    "Offline ecosystem {name}: repeated server-observed logless exploration within five blocks; seeking verified vertical escape"
                );
            }
            resource_confined=now_confined;
            if let Some(blocked) = exploration.observe_server_position(
                received.tick, [received.position[0], received.position[2]]
            ) {
                log::info!(
                    "Offline ecosystem {name}: exploration stalled; temporarily avoiding verified step {:?}",
                    blocked
                );
            }
            player.state.position=Some(received.position);
            // This reports only outgoing packet attempts and local errors.
            // Physical progress is measured separately from server positions.
            if received.tick%160==0 {
                log::info!(
                    "Offline ecosystem {name}: steering packets_sent={} waypoints_reached={} rejected={} last_error={}",
                    steering.packets_sent,steering.waypoints_reached,steering.rejected,
                    steering.last_error.as_deref().unwrap_or("none")
                );
            }
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
                log::info!("Offline ecosystem {name}: aborting mining because the server already observed the target gone");
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
        if harvest.as_ref().is_some_and(|e| !e.block_removed()){
            // The STOP is only a request. Keep the watched block inside the
            // authoritative survey window until an actual server lookup
            // confirms its disappearance or the bounded evidence timer
            // expires. Exploring immediately after STOP loses that proof.
            // This is not a claim that the block was successfully mined.
            player.client_tick_end()?;
            continue;
        }
        if let Some((controller,started))=ascent.as_mut(){
            let target=controller.destination;
            let reached=server_confirmed_landing(survey,target);
            if reached {
                vertical_memory.observed_landing(controller.origin,target,survey.tick);
                log::info!(
                    "Offline ecosystem {name}: server-authoritative uphill landing at {target:?}"
                );
                ascent=None;
            } else if survey.tick.saturating_sub(*started)>80 {
                log::info!(
                    "Offline ecosystem {name}: uphill motion unconfirmed; backing off from {target:?}"
                );
                ascent=None;
                ascent_cooldown=survey.tick.saturating_add(160);
            } else {
                let estimated=player.state.position.unwrap_or(survey.position);
                match controller.next(estimated) {
                    Some(motion)=>{
                        if let Err(error)=player.jump_delta(
                            motion.dx,motion.dy,motion.dz,motion.on_ground
                        ){
                            log::info!(
                                "Offline ecosystem {name}: uphill packet rejected: {error}"
                            );
                            ascent=None;
                            ascent_cooldown=survey.tick.saturating_add(160);
                        }
                    },
                    None=>{
                        // A simulated arc that misses or exceeds its bounds
                        // cannot be promoted to a server-confirmed landing.
                        // Stop immediately rather than waiting 80 ticks
                        // while generating no physical movement packets.
                        log::info!(
                            "Offline ecosystem {name}: uphill arc invalid before verified landing at {target:?}"
                        );
                        ascent=None;
                        ascent_cooldown=survey.tick.saturating_add(160);
                    },
                }
                player.client_tick_end()?;
                continue;
            }
        }
        if let Some((controller,started))=descent.as_mut(){
            let target=controller.destination;
            let reached=server_confirmed_landing(survey,target);
            if reached {
                vertical_memory.observed_landing(controller.origin,target,survey.tick);
                log::info!(
                    "Offline ecosystem {name}: server-authoritative downhill landing at {target:?}"
                );
                descent=None;
            } else if survey.tick.saturating_sub(*started)>80 {
                log::info!(
                    "Offline ecosystem {name}: downhill motion unconfirmed; backing off from {target:?}"
                );
                descent=None;
                descent_cooldown=survey.tick.saturating_add(160);
            } else {
                // Never mistake local optimistic packet position for a real
                // landing. The next server survey overwrites local position.
                let estimated=player.state.position.unwrap_or(survey.position);
                if let Some(movement)=controller.next(estimated) {
                    let result=if movement.dy<0.0 {
                        player.descend_delta(
                            movement.dx,movement.dy,movement.dz,movement.on_ground
                        )
                    } else {
                        player.walk_delta(movement.dx,movement.dz)
                    };
                    if let Err(error)=result {
                        log::info!(
                            "Offline ecosystem {name}: downhill packet rejected: {error}"
                        );
                        descent=None;
                        descent_cooldown=survey.tick.saturating_add(160);
                    }
                }
                player.client_tick_end()?;
                continue;
            }
        }
        if let Some(probe)=harvest.as_ref().filter(|h|h.block_removed()){
            // Travel toward the actual local dropped entity, never a guessed
            // block center. No flight, teleport, or extra inventory allowed.
            if let Some(drop)=watched_wood_drop(survey,probe.target) {
                let already_near=(survey.position[0]-drop[0]).abs()<1.25
                    && (survey.position[2]-drop[2]).abs()<1.25;
                let same_pickup_height=drop[1]>survey.position[1]-0.6
                    && drop[1]<survey.position[1]+2.4;
                let flat_route=if same_pickup_height && !already_near {
                    ground_step_to_drop(survey,drop)
                } else {None};
                if let Some(waypoint)=flat_route {
                    steering.steer(&mut player,waypoint);
                } else if !already_near || !same_pickup_height {
                    // Fall back to a verified 3D walk/down route when
                    // the same-level path is disconnected, not just when
                    // the item is below the player's current pickup box.
                    let candidate=terrain_step_to_drop(survey,drop);
                    if survey.tick%160==0 && last_route_report_tick!=Some(survey.tick) {
                        last_route_report_tick=Some(survey.tick);
                        let origin=[
                            survey.position[0].floor() as i32,
                            survey.position[1].floor() as i32,
                            survey.position[2].floor() as i32,
                        ];
                        let nearby_lower=survey.terrain.iter().filter(|p|
                            i64::from(p[1])==i64::from(origin[1])-1
                            && (i64::from(p[0])-i64::from(origin[0])).abs()
                              +(i64::from(p[2])-i64::from(origin[2])).abs()==1
                        ).count();
                        let pickup_stands=survey.terrain.iter().filter(|p|{
                            let feet=f64::from(p[1]);
                            drop[1]>feet-0.6 && drop[1]<feet+2.4
                                && (f64::from(p[0])+0.5-drop[0]).abs()<1.5
                                && (f64::from(p[2])+0.5-drop[2]).abs()<1.5
                        }).count();
                        log::info!(
                            "Offline ecosystem {name}: drop route observed item={drop:?} at={:?} safe_stands={} pickup_stands={} adjacent_lower={} next={candidate:?}",
                            survey.position,survey.terrain.len(),
                            pickup_stands,nearby_lower
                        );
                    }
                    if let Some(route)=candidate {
                        match route.motion {
                            RouteMotion::Walk=>{
                                let target=[f64::from(route.at[0])+0.5,
                                            f64::from(route.at[2])+0.5];
                                steering.steer(&mut player,target);
                            },
                            RouteMotion::StepDown=>{
                                if survey.tick>=descent_cooldown {
                                    if let Some(controller)=
                                        StepDownController::begin(survey.position,route.at)
                                    {
                                        log::info!(
                                            "Offline ecosystem {name}: attempting normal downhill movement to verified surface {:?}",
                                            route.at
                                        );
                                        descent=Some((controller,survey.tick));
                                    }
                                }
                            },
                            RouteMotion::JumpUp=>{
                                if survey.tick>=ascent_cooldown {
                                    if let Some(controller)=StepUpController::begin(
                                        survey.position,route.at
                                    ){
                                        log::info!(
                                            "Offline ecosystem {name}: attempting normal uphill jump to verified surface {:?}",
                                            route.at
                                        );
                                        ascent=Some((controller,survey.tick));
                                        descent=None;
                                    }
                                }
                            },
                        }
                    }
                }
            }
            player.client_tick_end()?;
            continue;
        }
        // A true item entity is an opportunistic resource whether it came
        // from this bot, another bot, or an earlier abandoned harvest.
        // Scavenging does not grant wood: only Horizon's regular pickup
        // rules and the next authoritative inventory survey do that.
        let intent=decide(survey,index);
        if harvest.is_none()
            && survey.wood_inventory<3
            && matches!(intent,Intent::GatherWood|Intent::Explore|Intent::BuildShelter)
        {
            if let Some((drop,route))=scavengable_wood_route(survey){
                let currently_touchable=(survey.position[0]-drop[0]).abs()<1.5
                    && (survey.position[2]-drop[2]).abs()<1.5
                    && drop[1]>survey.position[1]-0.6
                    && drop[1]<survey.position[1]+2.4;
                if !currently_touchable {
                    if survey.tick%160==0 {
                        log::info!(
                            "Offline ecosystem {name}: scavenging observed wood drop {drop:?} via verified {route:?}"
                        );
                    }
                    match route.motion {
                        RouteMotion::Walk=>{
                            let target=[
                                f64::from(route.at[0])+0.5,
                                f64::from(route.at[2])+0.5,
                            ];
                            steering.steer(&mut player,target);
                        },
                        RouteMotion::StepDown=>{
                            if survey.tick>=descent_cooldown {
                                if let Some(controller)=StepDownController::begin(
                                    survey.position,route.at
                                ){
                                    descent=Some((controller,survey.tick));
                                }
                            }
                        },
                        RouteMotion::JumpUp=>{
                            if survey.tick>=ascent_cooldown {
                                if let Some(controller)=StepUpController::begin(
                                    survey.position,route.at
                                ){
                                    ascent=Some((controller,survey.tick));
                                    descent=None;
                                }
                            }
                        },
                    }
                    player.client_tick_end()?;
                    continue;
                }
            }
        }
        let mut exploring=false;
        match intent{
            Intent::GatherWood=>{
                if let Some(log)=survey.nearby_log {
                    let vertical_delta=f64::from(log[1])-survey.position[1];
                    let log_center=[f64::from(log[0])+0.5,f64::from(log[2])+0.5];
                    let dx=survey.position[0]-log_center[0];
                    let dz=survey.position[2]-log_center[1];
                    let distance=dx.hypot(dz);
                    let standing_now=[
                        survey.position[0].floor() as i32,
                        survey.position[1].floor() as i32,
                        survey.position[2].floor() as i32,
                    ];
                    // If a *connected* prospective pickup stand exists,
                    // approach it BEFORE breaking wood. The old behavior
                    // mined from anywhere within reach and frequently
                    // left genuine drops outside every reachable pickup box.
                    let access_route=pickup_route_to_log(survey,log)
                        .filter(|route|route.at!=standing_now);
                    if !(-1.5..=2.5).contains(&vertical_delta){
                        exploring=true;
                    } else if distance>2.3 || access_route.is_some() {
                        if let Some(waypoint)=ground_step_to_log(survey,log)
                            .filter(|_|access_route.is_none())
                        {
                            if steering.steer(&mut player,waypoint){
                                target_memory.attempted_step(log);
                            }
                        } else if let Some(route)=access_route
                            .or_else(||terrain_step_to_log(survey,log))
                        {
                            match route.motion {
                                RouteMotion::Walk=>{
                                    let target=[
                                        f64::from(route.at[0])+0.5,
                                        f64::from(route.at[2])+0.5,
                                    ];
                                    if steering.steer(&mut player,target){
                                        target_memory.attempted_step(log);
                                    }
                                },
                                RouteMotion::StepDown=>{
                                    if survey.tick>=descent_cooldown{
                                        if let Some(controller)=
                                            StepDownController::begin(
                                                survey.position,route.at
                                            )
                                        {
                                            log::info!(
                                                "Offline ecosystem {name}: approaching real trunk downhill toward verified surface {:?}",
                                                route.at
                                            );
                                            descent=Some((controller,survey.tick));
                                        }
                                    }
                                },
                                RouteMotion::JumpUp=>{
                                    if survey.tick>=ascent_cooldown {
                                        if let Some(controller)=StepUpController::begin(
                                            survey.position,route.at
                                        ){
                                            log::info!(
                                                "Offline ecosystem {name}: approaching real trunk uphill toward verified surface {:?}",
                                                route.at
                                            );
                                            ascent=Some((controller,survey.tick));
                                            descent=None;
                                        }
                                    }
                                },
                            }
                        } else {
                            // No server-known safe approach. Choose another
                            // observed resource or explore verified terrain.
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
                            blocked_pickup_surveys=0;
                            if let Ok(mut target)=watched_target.lock(){*target=Some(log);}
                            log::info!(
                                "Offline ecosystem {name}: mining attempt started at {:?} access_rank={} connected_pickup_stands={} cleared_log_route={:?} pickup_route={:?}",
                                log,harvest_access_rank(survey,log),
                                predicted_pickup_stands(survey,log),
                                cleared_log_route(survey,log),pickup_route_to_log(survey,log)
                            );
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
            let flat_step=exploration.choose_next(
                at,&survey.ground,survey.tick,index as u32
            );
            // A real visible but uncollectible trunk is a reason to
            // LOOK FOR access, not a reason to jump up and down forever.
            // Prefer unvisited *connected* flat ground before searching for
            // a vertical route. Disconnected or unknown cells do not block
            // this local exhaustion test.
            let flat_exhausted=exploration.locally_exhausted(
                at,&survey.ground,survey.tick
            );
            // The previous rule never escaped large, treeless plateaus:
            // visible-but-unreachable wood was its only trigger if more
            // than three flat cells existed. A forest may lie downhill
            // outside the bounded sensor even while logs.is_empty().
            let seek_vertical=should_seek_vertical(
                flat_exhausted,flat_step.is_some()
            ) || resource_confined;
            let current=[
                survey.position[0].floor() as i32,
                survey.position[1].floor() as i32,
                survey.position[2].floor() as i32,
            ];
            // A single reversed edge is not enough: the real-world bot
            // repeatedly cycled across a four-surface downhill/uphill loop.
            // Remember bounded server-confirmed landings, but only for
            // unguided exploration, never for active resources or pickups.
            let mut avoid_vertical=vertical_memory.recent_stands_to_avoid(
                current,survey.tick
            );
            if let Some(back)=vertical_memory.avoid_immediate_return(
                current,survey.tick
            ){
                if !avoid_vertical.contains(&back){avoid_vertical.push(back);}
            }
            let downhill=if seek_vertical && survey.tick>=descent_cooldown {
                exploratory_downhill_step(survey,avoid_vertical.clone())
            } else {None};
            let vertical=downhill.or_else(||{
                if seek_vertical && survey.tick>=ascent_cooldown {
                    exploratory_uphill_step(survey,avoid_vertical)
                } else {None}
            });
            if let Some(route)=vertical {
                match route.motion{
                    RouteMotion::Walk=>{
                        let target=[f64::from(route.at[0])+0.5,
                            f64::from(route.at[2])+0.5];
                        steering.steer(&mut player,target);
                    },
                    RouteMotion::StepDown=>{
                        if let Some(controller)=
                            StepDownController::begin(survey.position,route.at)
                        {
                            log::info!(
                                "Offline ecosystem {name}: exploring downhill toward verified surface {:?}",
                                route.at
                            );
                            descent=Some((controller,survey.tick));
                        }
                    },
                    RouteMotion::JumpUp=>{
                        if let Some(controller)=
                            StepUpController::begin(survey.position,route.at)
                        {
                            log::info!(
                                "Offline ecosystem {name}: exploring uphill toward verified surface {:?}",
                                route.at
                            );
                            ascent=Some((controller,survey.tick));
                            descent=None;
                        }
                    },
                }
            } else if let Some(step)=flat_step{
                // Never cross unknown ground. All physical movement remains
                // subject to Horizon's ordinary player collision handling.
                let target=[f64::from(step[0])+0.5,f64::from(step[1])+0.5];
                if steering.steer(&mut player,target){
                    exploration.attempted_step(step);
                }
            }
        }
        player.client_tick_end()?;
    }
    Ok(())
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn ground_route_to_drop_requires_observed_item_and_real_pickup_height(){
        let mut survey=Survey {
            tick:0,on_ground:true,position:[0.5,64.0,0.5],food:20,health:20.0,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],
            wood_drops:vec![[2.6,64.1,0.5]],ground:vec![[0,0],[1,0],[2,0]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,64,0],[2,64,0]],
        };
        assert_eq!(watched_wood_drop(&survey,[2,64,0]),Some([2.6,64.1,0.5]));
        assert_eq!(ground_step_to_drop(&survey,[2.6,64.1,0.5]),Some([1.5,0.5]));
        assert_eq!(ground_step_to_drop(&survey,[2.6,63.0,0.5]),None);
        survey.ground=vec![[0,0],[2,0]];
        assert_eq!(ground_step_to_drop(&survey,[2.6,64.1,0.5]),None);
        survey.wood_drops=vec![[7.5,64.0,0.5]];
        assert_eq!(watched_wood_drop(&survey,[2,64,0]),None);
    }
    #[test]
    fn lower_real_item_routes_as_descent_without_making_the_player_fly(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.0,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],
            wood_drops:vec![[2.5,64.15,0.5]],
            ground:vec![[0,0]],clearable_stands:vec![],terrain:vec![[0,65,0],[1,64,0],[2,64,0]],
        };
        assert_eq!(terrain_step_to_drop(&survey,[2.5,64.15,0.5]),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::StepDown}));
        survey.terrain=vec![[0,65,0],[1,65,0],[2,64,0]];
        assert_eq!(terrain_step_to_drop(&survey,[2.5,64.15,0.5]),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::Walk}));
        survey.terrain=vec![[0,65,0],[2,64,0]];
        assert_eq!(terrain_step_to_drop(&survey,[2.5,64.15,0.5]),None);
    }
    #[test]
    fn lower_drop_detours_around_solid_high_ground_without_jumping(){
        let safe=vec![
            [0,65,0],[0,65,1],[0,65,2],[1,65,2],
            [2,65,2],[2,64,1],[2,64,0],
        ];
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],wood_drops:vec![],
            ground:vec![[0,0],[0,1],[0,2],[1,2],[2,2]],clearable_stands:vec![],terrain:safe,
        };
        assert_eq!(terrain_step_to_drop(&survey,[2.5,64.15,0.5]),
            Some(RouteStep{at:[0,65,1],motion:RouteMotion::Walk}));
    }
    #[test]
    fn lower_drop_outside_local_pickup_area_uses_verified_3d_frontier(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.0,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],
            wood_drops:vec![[5.5,64.15,0.5]],
            ground:vec![[0,0],[1,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,65,0],[2,64,0]],
        };
        assert_eq!(terrain_step_to_drop(&survey,[5.5,64.15,0.5]),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::Walk}));
        survey.position=[1.5,65.0,0.5];
        assert_eq!(terrain_step_to_drop(&survey,[5.5,64.15,0.5]),
            Some(RouteStep{at:[2,64,0],motion:RouteMotion::StepDown}));
        survey.position=[0.5,65.0,0.5];
        survey.terrain=vec![[0,65,0],[3,64,0]];
        assert_eq!(terrain_step_to_drop(&survey,[5.5,64.15,0.5]),None);
    }
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
            on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,ground:vec![[0,0]], logs:vec![],
            wood_drops:vec![],clearable_stands:vec![],terrain:vec![[0,64,0]],
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
    #[test]
    fn rejects_unreachable_visible_trees_but_keeps_mineable_alternative(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],
            wood_drops:vec![],ground:vec![[0,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,64,0]],
        };
        assert!(!can_approach_log(&survey,[5,65,0]));
        assert!(can_approach_log(&survey,[1,65,0]));
        assert!(can_approach_log(&survey,[1,67,0]),
            "normal eye-level reach should allow upper trunk harvesting");
        assert!(!can_approach_log(&survey,[1,68,0]));
        assert!(!can_approach_log(&survey,[1,63,0]));
        assert_eq!(exploratory_downhill_step(&survey,None),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::StepDown}));
        survey.terrain=vec![[0,65,0],[2,64,0]];
        assert_eq!(exploratory_downhill_step(&survey,None),None);
    }
    #[test]
    fn higher_trunk_routes_only_when_server_surface_supports_real_jump(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([3,65,0]),watched_log:None,
            logs:vec![[3,65,0]],wood_drops:vec![],
            ground:vec![[0,0]],clearable_stands:vec![],terrain:vec![[0,64,0],[1,65,0],[2,65,0]],
        };
        assert_eq!(terrain_step_to_log(&survey,[3,65,0]),Some(
            RouteStep{at:[1,65,0],motion:RouteMotion::JumpUp}
        ));
        survey.terrain=vec![[0,64,0],[2,65,0]];
        assert_eq!(terrain_step_to_log(&survey,[3,65,0]),None);
    }
    #[test]
    fn gather_wood_approaches_real_lower_trunk_via_verified_descent(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([4,64,0]),watched_log:None,
            logs:vec![[4,64,0]],wood_drops:vec![],
            ground:vec![[0,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,64,0],[2,64,0],[3,64,0]],
        };
        assert_eq!(ground_step_to_log(&survey,[4,64,0]),None);
        assert_eq!(terrain_step_to_log(&survey,[4,64,0]),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::StepDown}));
        assert!(can_approach_log(&survey,[4,64,0]));
        // A disconnected lower log never becomes a teleport destination.
        survey.terrain=vec![[0,65,0],[3,64,0]];
        assert_eq!(terrain_step_to_log(&survey,[4,64,0]),None);
        assert!(!can_approach_log(&survey,[4,64,0]));
    }
    #[test]
    fn gather_wood_can_walk_to_downhill_transition_first(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([5,64,0]),watched_log:None,
            logs:vec![[5,64,0]],wood_drops:vec![],
            ground:vec![[0,0],[1,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,65,0],[2,64,0],[3,64,0]],
        };
        assert_eq!(terrain_step_to_log(&survey,[5,64,0]),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::Walk}));
    }
    #[test]
    fn visible_but_unreachable_wood_triggers_downhill_exploration(){
        let survey=Survey{
            tick:10,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![[4,65,0]],
            wood_drops:vec![],ground:vec![[0,0],[1,0],[2,0],[3,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,65,0],[2,65,0],
                         [3,65,0],[3,64,1]],
        };
        assert!(!survey.logs.is_empty() && survey.nearby_log.is_none());
        // The abstract safe downhill route is only a plan; server position
        // still decides whether a physical landing happened.
        assert_eq!(exploratory_downhill_step(&survey,None),Some(
            RouteStep{at:[1,65,0],motion:RouteMotion::Walk}
        ));
    }
    #[test]
    fn matching_height_without_server_ground_contact_is_not_landing(){
        let mut survey=Survey{
            tick:8,on_ground:false,position:[1.5,65.0,0.5],
            food:20,health:20.,food_inventory:0,wood_inventory:0,
            hotbar_food_slot:None,nearby_log:None,watched_log:None,
            logs:vec![],wood_drops:vec![],ground:vec![[1,0]],
            clearable_stands:vec![],terrain:vec![[1,65,0]],
        };
        assert!(!server_confirmed_landing(&survey,[1,65,0]));
        survey.on_ground=true;
        assert!(server_confirmed_landing(&survey,[1,65,0]));
        survey.terrain.clear();
        assert!(!server_confirmed_landing(&survey,[1,65,0]));
        survey.terrain.push([1,65,0]);
        survey.position=[2.7,65.0,0.5];
        assert!(!server_confirmed_landing(&survey,[1,65,0]));
    }
    #[test]
    fn isolated_lower_player_can_choose_safe_one_block_uphill_route(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],wood_drops:vec![],
            ground:vec![[0,0]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,65,0]],
        };
        assert_eq!(exploratory_downhill_step(&survey,None),None);
        assert_eq!(exploratory_uphill_step(&survey,None),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::JumpUp}));
        survey.terrain=vec![[0,64,0],[2,65,0]];
        assert_eq!(exploratory_uphill_step(&survey,None),None);
    }
    #[test]
    fn uphill_exploration_can_use_flat_approach_first(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],wood_drops:vec![],
            ground:vec![[0,0],[1,0]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,64,0],[2,65,0]],
        };
        assert_eq!(exploratory_uphill_step(&survey,None),
            Some(RouteStep{at:[1,64,0],motion:RouteMotion::Walk}));
    }
    #[test]
    fn confirmed_vertical_landing_memory_blocks_immediate_exploration_reversal(){
        let mut m=VerticalTransitionMemory::new();
        let a=[0,64,0];
        let b=[1,65,0];
        m.observed_landing(a,b,100);
        let survey=Survey{
            tick:101,on_ground:true,position:[1.5,65.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],wood_drops:vec![],
            ground:vec![[1,0]],clearable_stands:vec![],terrain:vec![a,b],
        };
        assert_eq!(exploratory_downhill_step(&survey,None),
            Some(RouteStep{at:a,motion:RouteMotion::StepDown}));
        let excluded=m.avoid_immediate_return(b,101);
        assert_eq!(excluded,Some(a));
        assert_eq!(exploratory_downhill_step(&survey,excluded),None);
        assert_eq!(exploratory_downhill_step(&survey,
            m.avoid_immediate_return(b,220)),
            Some(RouteStep{at:a,motion:RouteMotion::StepDown}));
    }
    #[test]
    fn exhausted_connected_ground_seeks_a_new_elevation_even_without_logs(){
        assert!(!should_seek_vertical(false,true),
            "unvisited connected flat ground has priority");
        assert!(should_seek_vertical(true,true),
            "never loop on a large treeless but fully visited plateau");
        assert!(should_seek_vertical(true,false));
        assert!(should_seek_vertical(false,false),
            "with no safe flat step, consider verified vertical escape");
    }
    #[test]
    fn repeated_vertical_landings_cannot_trap_explorer_in_short_loop(){
        let mut memory=VerticalTransitionMemory::new();
        let a=[0,65,0];
        let b=[1,64,0];
        let c=[1,64,1];
        let d=[2,65,1];
        memory.observed_landing(a,b,100);
        memory.observed_landing(c,d,120);
        let survey=Survey{
            tick:121,on_ground:true,position:[2.5,65.,1.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],wood_drops:vec![],
            ground:vec![[2,1]],clearable_stands:vec![],
            terrain:vec![a,b,c,d],
        };
        // Without history, this is an immediate new downhill attempt toward
        // a recently traversed surface. With bounded history it refuses.
        assert!(exploratory_downhill_step(&survey,None).is_some());
        assert_eq!(exploratory_downhill_step(&survey,
            memory.recent_stands_to_avoid(d,121)),None);
        // All routes become available again after the short memory expires.
        assert!(exploratory_downhill_step(&survey,
            memory.recent_stands_to_avoid(d,601)).is_some());
    }
    #[test]
    fn connected_downhill_escape_can_first_walk_across_plateau(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.0,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,logs:vec![],
            wood_drops:vec![],ground:vec![[0,0],[1,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,65,0],[2,64,0]],
        };
        assert_eq!(exploratory_downhill_step(&survey,None),
            Some(RouteStep{at:[1,65,0],motion:RouteMotion::Walk}));
    }
    #[test]
    fn scavenging_prefers_reachable_real_drops_and_skips_disconnected_items(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.0,0.5],
            food:20,health:20.,food_inventory:0,wood_inventory:0,
            hotbar_food_slot:None,nearby_log:None,watched_log:None,
            ground:vec![[0,0],[1,0]],logs:vec![],
            wood_drops:vec![[3.5,64.0,0.5]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,64,0],[3,64,0]],
        };
        // Physically observed but disconnected items do not create paths.
        assert_eq!(scavengable_wood_route(&survey),None);
        survey.wood_drops.push([2.6,64.0,0.5]);
        assert_eq!(scavengable_wood_route(&survey),Some((
            [2.6,64.0,0.5],
            RouteStep{at:[1,64,0],motion:RouteMotion::Walk},
        )));
        // An invalid or missing server item is never invented as a goal.
        survey.wood_drops=vec![[f64::NAN,64.0,0.5]];
        assert_eq!(scavengable_wood_route(&survey),None);
    }
    #[test]
    fn observed_drop_without_safe_pickup_stand_cannot_count_as_collected(){
        let mut survey=Survey{
            tick:0,on_ground:true,position:[0.5,81.,0.5],
            food:20,health:20.,food_inventory:0,wood_inventory:0,
            hotbar_food_slot:None,nearby_log:None,watched_log:None,
            ground:vec![[0,0]],logs:vec![],wood_drops:vec![[1.5,80.,0.5]],
            clearable_stands:vec![],terrain:vec![[0,81,0]],
        };
        assert!(!has_surveyed_pickup_stand(&survey,[1.5,80.,0.5]));
        let mut evidence=HarvestEvidence::new([1,80,0],0,0);
        assert_eq!(evidence.observe(8,Some(false),0),
            HarvestStatus::BlockRemovedWithoutPickup);
        for tick in 1..=8{
            survey.tick=tick*8;
            let drop=watched_wood_drop(&survey,[1,80,0]).unwrap();
            assert!(!has_surveyed_pickup_stand(&survey,drop));
        }
        assert!(evidence.block_removed());
        // If genuine safe standing terrain becomes visible, pickup is
        // physically plausible again; still no inventory without a gain.
        survey.terrain.push([1,80,0]);
        assert!(has_surveyed_pickup_stand(&survey,[1.5,80.,0.5]));
        assert_eq!(evidence.observe(72,None,0),
            HarvestStatus::BlockRemovedWithoutPickup);
    }
    #[test]
    fn known_pickup_stands_prioritize_accessible_drops_over_buried_wood(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,81.,0.5],
            food:20,health:20.,food_inventory:0,wood_inventory:0,
            hotbar_food_slot:None,nearby_log:None,watched_log:None,
            ground:vec![[0,0]],logs:vec![[1,80,0],[2,81,0]],
            clearable_stands:vec![],terrain:vec![[0,81,0],[1,81,0],[1,81,1]],wood_drops:vec![],
        };
        // Below-feet logs can be mined, but a settled drop at Y=80 is
        // lower than the pickup box of both current standing surfaces.
        assert_eq!(predicted_pickup_stands(&survey,[1,80,0]),0);
        assert!(predicted_pickup_stands(&survey,[2,81,0])>0);
        let mut candidates=vec![[1,80,0],[2,81,0]];
        prioritize_collectible_logs(&mut candidates,&survey);
        assert_eq!(candidates[0],[2,81,0]);
        // No observed pickup cells? Keep real resources as fallback, not
        // fabricated stand positions or invisible inventory credits.
        let mut no_support=survey.clone();
        no_support.terrain=vec![[0,83,0]];
        let mut candidates=vec![[1,80,0],[2,81,0]];
        prioritize_collectible_logs(&mut candidates,&no_support);
        assert_eq!(candidates,vec![[1,80,0],[2,81,0]]);
    }
    #[test]
    fn conservative_cleared_log_route_requires_supported_body_space(){
        let mut s=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],health:20.,food:20,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,
            ground:vec![[0,0],[1,0]],logs:vec![[2,63,0]],
            wood_drops:vec![],terrain:vec![[0,64,0],[1,64,0]],
            clearable_stands:vec![[2,63,0]],
        };
        // Same-height cells cannot pick up a drop that settles one block
        // lower. Removing a verified log would open a supported descent.
        assert_eq!(predicted_pickup_stands(&s,[2,63,0]),0);
        assert_eq!(cleared_log_route(&s,[2,63,0]),Some(
            RouteStep{at:[1,64,0],motion:RouteMotion::Walk}
        ));
        assert_eq!(harvest_access_rank(&s,[2,63,0]),1);
        s.clearable_stands.clear();
        assert_eq!(harvest_access_rank(&s,[2,63,0]),2);
    }
    #[test]
    fn access_rank_prefers_collectible_then_clearable_then_inaccessible(){
        let s=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],health:20.,food:20,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,
            ground:vec![[0,0],[1,0]],
            logs:vec![[3,64,0],[2,63,0],[1,64,1]],
            wood_drops:vec![],terrain:vec![[0,64,0],[1,64,0]],
            clearable_stands:vec![[2,63,0]],
        };
        assert_eq!(harvest_access_rank(&s,[3,64,0]),2);
        assert_eq!(harvest_access_rank(&s,[2,63,0]),1);
        assert_eq!(harvest_access_rank(&s,[1,64,1]),0);
        let mut logs=s.logs.clone();
        prioritize_collectible_logs(&mut logs,&s);
        assert_eq!(logs,vec![[1,64,1],[2,63,0],[3,64,0]]);
    }
    #[test]
    fn preharvest_position_is_reached_before_mining_from_the_edge_of_reach(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([2,64,0]),watched_log:None,logs:vec![[2,64,0]],
            wood_drops:vec![],ground:vec![[0,0],[1,0]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,64,0]],
        };
        // This log is already within mining reach, but breaking it here
        // may leave its drop out of pickup range. First walk to the
        // connected stand beside the trunk, subject to server approval.
        assert!(2.0f64<=2.3);
        assert_eq!(pickup_route_to_log(&survey,[2,64,0]),Some(
            RouteStep{at:[1,64,0],motion:RouteMotion::Walk}
        ));
        assert_eq!(predicted_pickup_stands(&survey,[2,64,0]),1);
    }
    #[test]
    fn disconnected_pickup_stands_do_not_outrank_connected_real_wood(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:None,watched_log:None,
            logs:vec![[3,64,0],[1,64,0]],wood_drops:vec![],
            ground:vec![[0,0]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[3,64,1]],
        };
        assert_eq!(prospective_pickup_stands(&survey,[3,64,0]),vec![[3,64,1]]);
        assert_eq!(pickup_route_to_log(&survey,[3,64,0]),None);
        assert_eq!(predicted_pickup_stands(&survey,[3,64,0]),0);
        assert!(predicted_pickup_stands(&survey,[1,64,0])>0);
        let mut order=vec![[3,64,0],[1,64,0]];
        prioritize_collectible_logs(&mut order,&survey);
        assert_eq!(order,vec![[1,64,0],[3,64,0]]);
    }
    #[test]
    fn preharvest_pickup_stand_can_use_server_verified_downhill_path(){
        let survey=Survey{
            tick:0,on_ground:true,position:[0.5,65.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([2,64,0]),watched_log:None,
            logs:vec![[2,64,0]],wood_drops:vec![],
            ground:vec![[0,0]],
            clearable_stands:vec![],terrain:vec![[0,65,0],[1,64,0]],
        };
        assert_eq!(pickup_route_to_log(&survey,[2,64,0]),Some(
            RouteStep{at:[1,64,0],motion:RouteMotion::StepDown}
        ));
    }
    #[test]
    fn pickup_aware_target_priority_prefers_collectible_log_elevations(){
        let mut logs=vec![[0,63,0],[2,65,0],[1,64,0],[0,65,1]];
        prefer_pickup_height(&mut logs,[0.5,64.0,0.5]);
        assert_eq!(logs[0],[1,64,0]);
        assert_eq!(logs[1],[0,65,1]);
        assert_eq!(logs[2],[2,65,0]);
        assert_eq!(logs[3],[0,63,0]);
        // With no level or upper logs, lower blocks remain visible
        // rather than inventing a new resource or silently hiding all wood.
        let mut low=[[0,63,0]];
        prefer_pickup_height(&mut low,[0.5,64.0,0.5]);
        assert_eq!(low,[[0,63,0]]);
    }
    #[test]
    fn dense_inaccessible_log_cluster_cannot_hide_accessible_log(){
        let origin=[0,64,0];
        let accessible=[6,64,6];
        let all=ranked_log_candidates(origin,|p|
            (p[1]==64 && p[0].abs()<=4 && p[2].abs()<=4)
            || p==accessible,
            |p|if p==accessible{0}else{2}
        );
        assert_eq!(all.len(),32);
        assert_eq!(all[0],accessible);
        assert!(all.contains(&accessible));
        assert!(all.iter().all(|p|
            p[0].abs()<=6 && p[2].abs()<=6 && (63..=66).contains(&p[1])
        ));
    }
    #[test]
    fn candidate_pickup_rank_requires_actual_connected_standing_cell(){
        let connected:std::collections::BTreeSet<_>=
            [[0,64,0],[1,64,0]].into_iter().collect();
        assert!(has_connected_pickup_stand(&connected,[2,64,0]));
        assert!(has_connected_pickup_stand(&connected,[2,66,0]));
        assert!(!has_connected_pickup_stand(&connected,[2,67,0]));
        assert!(!has_connected_pickup_stand(&connected,[6,64,0]));
        assert!(!has_connected_pickup_stand(&connected,[i32::MAX,64,0]));
    }
    #[test]fn local_log_survey_is_bounded_and_nearest_first(){
        let origin=[0,64,0];
        let all=log_candidates(origin,|_|true);
        assert_eq!(all.len(),32);
        assert_eq!(all[0],origin);
        assert!(all.iter().all(|p|
            (p[0]-origin[0]).abs()<=6 &&
            (p[1]-origin[1]).abs()<=2 &&
            (p[2]-origin[2]).abs()<=6
        ));
        assert_eq!(log_candidates(origin,|p|p==[3,64,0]),vec![[3,64,0]]);
        assert!(log_candidates(origin,|_|false).is_empty());
    }
    #[test]
    fn distant_observed_tree_routes_to_nearest_safe_ground_frontier(){
        let mut s=Survey{
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([6,64,0]),watched_log:None,
            ground:vec![[0,0],[1,0],[2,0],[3,0]],
            logs:vec![[6,64,0]],wood_drops:vec![],
            clearable_stands:vec![],terrain:vec![[0,64,0],[1,64,0],[2,64,0],[3,64,0]],
        };
        assert_eq!(ground_step_to_log(&s,[6,64,0]),Some([1.5,0.5]));
        // A disconnected tile near a tree does not become a magical path.
        s.ground=vec![[0,0],[3,0]];
        assert_eq!(ground_step_to_log(&s,[6,64,0]),None);
        // A target with no forward progress remains unrouteable.
        s.ground=vec![[0,0],[-1,0]];
        assert_eq!(ground_step_to_log(&s,[6,64,0]),None);
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
    #[test]
    fn expanded_terrain_survey_finds_connected_detour_five_blocks_away(){
        let p=terrain_candidates([0,65,0],|[x,y,z]|{
            y==65 && z==2 && (-5..=5).contains(&x)
                || y==65 && x==0 && (0..=2).contains(&z)
                || y==65 && x==4 && (0..=2).contains(&z)
                || y==64 && x==4 && z==0
        });
        assert!(p.contains(&[4,64,0]));
        assert!(p.contains(&[0,65,2]));
        assert!(p.len()<=363);
        assert_eq!(first_walk_down_step([0,65,0],&[[4,64,0]],&p),
            Some(RouteStep{at:[0,65,1],motion:RouteMotion::Walk}));
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
            tick:0,on_ground:true,position:[0.5,64.,0.5],food:20,health:20.,
            food_inventory:0,wood_inventory:0,hotbar_food_slot:None,
            nearby_log:Some([3,64,0]),watched_log:None,logs:vec![[3,64,0]],
            wood_drops:vec![],ground:vec![[0,0],[0,1],[1,1],[2,1],[3,1]],
            clearable_stands:vec![],terrain:vec![[0,64,0],[0,64,1],[1,64,1],[2,64,1],[3,64,1]],
        };
        assert_eq!(ground_step_to_log(&s,[3,64,0]),Some([0.5,1.5]));
        s.ground=vec![[0,0],[3,1]];
        assert_eq!(ground_step_to_log(&s,[3,64,0]),None);
    }

}
