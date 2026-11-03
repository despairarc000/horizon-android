//! Offline Rain World-style population on a finite room graph.
//! Logic model only; future server adapter must validate Minecraft actions.
use std::collections::{BTreeMap,BTreeSet,VecDeque};
use crate::{BotView,Intent,LocalObservation,Needs,Personality,Resolution,choose_intent};

#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Region {
    pub exits:Vec<usize>,
    pub food:u32,
    pub wood:u32,
    pub shelter:bool,
}
impl Region {
    pub fn new(exits:Vec<usize>,food:u32,wood:u32)->Self{
        Self{exits,food,wood,shelter:false}
    }
}
#[derive(Clone,Debug,PartialEq)]
pub struct Individual {
    pub id:u32,
    pub region:usize,
    pub needs:Needs,
    pub personality:Personality,
    pub food:u32,
    pub wood:u32,
    pub trust:BTreeMap<u32,f32>,
    pub known_food:BTreeMap<usize,u32>,
    pub visited:BTreeSet<usize>,
    pub alive:bool,
}
impl Individual {
    pub fn new(id:u32,region:usize,personality:Personality)->Self{
        Self{id,region,personality,needs:Needs::new(85,85,100).unwrap(),
            food:0,wood:0,trust:BTreeMap::new(),known_food:BTreeMap::new(),
            visited:BTreeSet::new(),alive:true}
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Event {
    pub tick:u64,pub bot_id:u32,pub intent:Intent,
    pub from:usize,pub to:usize,
}
#[derive(Clone,Debug,PartialEq)]
pub struct Population {
    pub regions:Vec<Region>,
    pub bots:Vec<Individual>,
    pub observer:usize,
    pub tick:u64,
    pub events:Vec<Event>,
    pub food_eaten:u64,
    initial_wood:u64,
    initial_food:u64,
    random_state:u64,
}
impl Population {
    pub fn new(mut regions:Vec<Region>,mut bots:Vec<Individual>,observer:usize,seed:u64)->Result<Self,String>{
        if regions.is_empty()||observer>=regions.len(){return Err("invalid observer".into());}
        for region in &mut regions{region.exits.sort_unstable();region.exits.dedup();}
        let size=regions.len();
        for (index,region) in regions.iter().enumerate(){
            if region.exits.iter().any(|&other|other==index||other>=size||!regions[other].exits.contains(&index)){
                return Err("region edges must be in bounds and symmetric".into());
            }
        }
        bots.sort_by_key(|b|b.id);
        if bots.iter().any(|b|b.region>=size||b.trust.values().any(|&v|!v.is_finite()||!(-1.0..=1.0).contains(&v))) ||
           bots.windows(2).any(|w|w[0].id==w[1].id){return Err("invalid bot state".into());}
        let mut p=Self{regions,bots,observer,tick:0,events:Vec::new(),food_eaten:0,initial_wood:0,initial_food:0,random_state:seed.max(1)};
        p.initial_wood=p.wood_total();p.initial_food=p.food_total();Ok(p)
    }
    pub fn wood_total(&self)->u64{
        self.regions.iter().map(|r|u64::from(r.wood)+if r.shelter{3}else{0}).sum::<u64>()
        +self.bots.iter().map(|b|u64::from(b.wood)).sum::<u64>()
    }
    pub fn food_total(&self)->u64{
        self.regions.iter().map(|r|u64::from(r.food)).sum::<u64>()
        +self.bots.iter().map(|b|u64::from(b.food)).sum::<u64>()
    }
    pub fn check(&self)->Result<(),String>{
        if self.wood_total()!=self.initial_wood{return Err("wood conservation failed".into());}
        if self.food_total()+self.food_eaten!=self.initial_food{return Err("food conservation failed".into());}
        if self.bots.iter().any(|b|b.region>=self.regions.len()||b.trust.values().any(|&x|!x.is_finite()||!(-1.0..=1.0).contains(&x))){
            return Err("invalid bot state".into());
        }
        Ok(())
    }
    fn rand(&mut self,count:usize)->usize{
        let mut x=self.random_state;x^=x.wrapping_shl(13);x^=x>>7;x^=x.wrapping_shl(17);self.random_state=x;
        (x%(count as u64)) as usize
    }
    fn distances(&self)->Vec<usize>{
        let mut d=vec![usize::MAX;self.regions.len()];
        let mut queue=VecDeque::from([self.observer]);d[self.observer]=0;
        while let Some(at)=queue.pop_front(){
            for &n in &self.regions[at].exits{
                if d[n]==usize::MAX{d[n]=d[at]+1;queue.push_back(n);}
            }
        }
        d
    }
    pub fn tier(&self,index:usize)->Resolution{
        let d=self.distances()[self.bots[index].region];
        Resolution::from_room_distance(u32::try_from(d).unwrap_or(u32::MAX))
    }
    fn next_step(&self,start:usize,targets:&BTreeSet<usize>)->Option<usize>{
        if targets.contains(&start){return Some(start);}
        let mut first=vec![None;self.regions.len()];
        let mut queue=VecDeque::from([start]);first[start]=Some(start);
        while let Some(at)=queue.pop_front(){
            for &next in &self.regions[at].exits{
                if first[next].is_some(){continue;}
                let step=if at==start{next}else{first[at]?};
                first[next]=Some(step);
                if targets.contains(&next){return Some(step);}
                queue.push_back(next);
            }
        }
        None
    }
    fn see(&mut self,i:usize)->(LocalObservation,Option<usize>,Option<usize>){
        let at=self.bots[i].region;
        for node in std::iter::once(at).chain(self.regions[at].exits.iter().copied()){
            self.bots[i].known_food.insert(node,self.regions[node].food);
        }
        self.bots[i].visited.insert(at);
        let mut enemy=None;let mut companion=None;
        for (j,other) in self.bots.iter().enumerate(){
            if i==j||!other.alive||other.region!=at{continue;}
            if self.bots[i].trust.get(&other.id).copied().unwrap_or(0.)<=-0.65&&enemy.is_none(){enemy=Some(j);}
            if other.needs.hunger<55&&companion.is_none(){companion=Some(j);}
        }
        let trust_enemy=enemy.map(|j|self.bots[i].trust.get(&self.bots[j].id).copied().unwrap_or(0.)).unwrap_or(0.);
        let trust_friend=companion.map(|j|self.bots[i].trust.get(&self.bots[j].id).copied().unwrap_or(0.)).unwrap_or(0.);
        (LocalObservation{
            enemy_at_position:enemy.is_some(),hungry_companion_at_position:companion.is_some(),
            relationship_to_enemy:trust_enemy,relationship_to_companion:trust_friend,
            local_food:self.regions[at].food.min(u32::from(u16::MAX)) as u16,
            local_wood:self.regions[at].wood.min(u32::from(u16::MAX)) as u16,
            known_food_reachable:self.bots[i].known_food.values().any(|&n|n>0),
            shelter_here:self.regions[at].shelter,
        },enemy,companion)
    }
    fn trust_delta(&mut self,i:usize,j:usize,delta:f32){
        let id=self.bots[j].id;
        let prev=self.bots[i].trust.get(&id).copied().unwrap_or(0.);
        self.bots[i].trust.insert(id,(prev+delta).clamp(-1.,1.));
    }
    fn act(&mut self,i:usize){
        let from=self.bots[i].region;
        let (observation,enemy,companion)=self.see(i);
        let b=&self.bots[i];
        let intent=choose_intent(&BotView{
            needs:b.needs,personality:b.personality,
            food_inventory:b.food.min(u32::from(u16::MAX)) as u16,
            wood_inventory:b.wood.min(u32::from(u16::MAX)) as u16,
        },&observation);
        match intent {
            Intent::Flee=>{
                let exits=self.regions[from].exits.clone();
                if !exits.is_empty(){let n=self.rand(exits.len());self.bots[i].region=exits[n];}
            }
            Intent::Defend=>{
                if let Some(j)=enemy{
                    self.bots[j].needs.health=self.bots[j].needs.health.saturating_sub(8);
                    self.trust_delta(j,i,-0.2);
                    if self.bots[j].needs.health==0{self.bots[j].alive=false;}
                }
            }
            Intent::Eat=>{
                if self.bots[i].food>0{
                    self.bots[i].food-=1;self.food_eaten+=1;
                    self.bots[i].needs.hunger=self.bots[i].needs.hunger.saturating_add(37).min(100);
                }
            }
            Intent::Rest=>{
                let gain=if self.regions[from].shelter{25}else{15};
                self.bots[i].needs.energy=self.bots[i].needs.energy.saturating_add(gain).min(100);
            }
            Intent::ShareFood=>{
                if let Some(j)=companion{
                    if self.bots[i].food>=2{
                        self.bots[i].food-=1;self.bots[j].food+=1;
                        self.trust_delta(j,i,0.25);self.trust_delta(i,j,0.08);
                    }
                }
            }
            Intent::Forage=>{
                if self.regions[from].food>0{
                    self.regions[from].food-=1;self.bots[i].food+=1;
                    self.bots[i].known_food.insert(from,self.regions[from].food);
                }
            }
            Intent::SeekKnownFood=>{
                let wanted=self.bots[i].known_food.iter().filter_map(|(&p,&n)|(n>0).then_some(p)).collect();
                if let Some(dest)=self.next_step(from,&wanted){self.bots[i].region=dest;}
            }
            Intent::BuildShelter=>{
                if self.bots[i].wood>=3&&!self.regions[from].shelter{
                    self.bots[i].wood-=3;self.regions[from].shelter=true;
                }
            }
            Intent::GatherWood=>{
                if self.regions[from].wood>0{self.regions[from].wood-=1;self.bots[i].wood+=1;}
            }
            Intent::Explore=>{
                let exits=self.regions[from].exits.clone();
                let fresh=exits.iter().copied().filter(|p|!self.bots[i].visited.contains(p)).collect::<Vec<_>>();
                let choices=if fresh.is_empty(){&exits}else{&fresh};
                if !choices.is_empty(){let pick=self.rand(choices.len());self.bots[i].region=choices[pick];}
            }
        }
        self.events.push(Event{tick:self.tick,bot_id:self.bots[i].id,intent,
            from,to:self.bots[i].region});
    }
    pub fn step(&mut self,n:u64)->Result<(),String>{
        for _ in 0..n{
            self.tick+=1;
            let distance=self.distances();
            for i in 0..self.bots.len(){
                if !self.bots[i].alive{continue;}
                let needs=&mut self.bots[i].needs;
                if self.tick%6==0{needs.hunger=needs.hunger.saturating_sub(1);}
                if self.tick%8==0{needs.energy=needs.energy.saturating_sub(1);}
                if needs.hunger==0&&self.tick%12==0{needs.health=needs.health.saturating_sub(1);}
                if needs.health==0{self.bots[i].alive=false;continue;}
                let pos=self.bots[i].region;
                let detail=Resolution::from_room_distance(u32::try_from(distance[pos]).unwrap_or(u32::MAX));
                if detail.should_decide(self.tick,self.bots[i].id){self.act(i);}
            }
            self.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn demo()->Population{
        let traits=Personality::new(0.9,0.2,0.9).unwrap();
        let bots=vec![Individual::new(0,0,traits),Individual::new(1,2,traits),Individual::new(2,4,traits)];
        Population::new(vec![
            Region::new(vec![1],3,4),Region::new(vec![0,2],2,4),
            Region::new(vec![1,3],3,4),Region::new(vec![2,4],1,4),
            Region::new(vec![3],2,4),
        ],bots,0,1234).unwrap()
    }
    #[test]fn deterministic_replay(){
        let mut a=demo();let mut b=demo();a.step(400).unwrap();b.step(400).unwrap();assert_eq!(a,b);
    }
    #[test]fn resource_conservation(){
        let mut s=demo();s.step(1200).unwrap();
        assert!(s.events.len()>40);s.check().unwrap();
        assert_eq!(s.food_total()+s.food_eaten,s.initial_food);
    }
    #[test]fn perception_not_omniscient(){
        let mut s=demo();s.see(0);
        assert!(s.bots[0].known_food.contains_key(&1));
        assert!(!s.bots[0].known_food.contains_key(&4));
    }
    #[test]fn rejects_invalid_graph(){
        assert!(Population::new(vec![Region::new(vec![1],0,0)],vec![],0,1).is_err());
        assert!(Population::new(vec![Region::new(vec![1],0,0),Region::new(vec![],0,0)],vec![],0,1).is_err());
    }
    #[test]fn social_sharing_changes_trust(){
        let mut s=demo();
        s.bots[0].food=3;s.initial_food+=3;
        s.bots[1].region=0;s.bots[1].needs.hunger=40;
        s.bots[0].trust.insert(1,0.9);
        s.act(0);
        assert_eq!(s.events.last().unwrap().intent,Intent::ShareFood);
        assert_eq!(s.bots[1].food,1);
        assert!(s.bots[1].trust.get(&0).copied().unwrap_or(0.)>0.);
        s.check().unwrap();
    }
    #[test]fn three_resolution_tiers(){
        let s=demo();
        assert_eq!(s.tier(0),Resolution::Realized);
        assert_eq!(s.tier(1),Resolution::Abstract);
        assert_eq!(s.tier(2),Resolution::Coarse);
    }
}
