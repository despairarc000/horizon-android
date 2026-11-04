//! Bounded evidence record for actual Minecraft resource harvesting.
//! ACKs or vanished blocks cannot independently prove an item was collected.
//! Correlation between block removal and inventory increase is not causal proof.
pub const HARVEST_TIMEOUT_TICKS: u64 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarvestStatus {
    Pending,
    BlockRemovedWithoutPickup,
    ObservedWoodGain { gained: u16 },
    UnverifiedTimeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HarvestEvidence {
    pub target: [i32; 3],
    pub started_at: u64,
    pub initial_wood: u16,
    removed_at: Option<u64>,
    last_tick: u64,
    final_status: Option<HarvestStatus>,
}
impl HarvestEvidence {
    pub fn new(target: [i32;3], tick:u64, initial_wood:u16)->Self {
        Self {
            target, started_at:tick, initial_wood,
            removed_at:None,last_tick:tick,final_status:None
        }
    }
    /// block_is_log: Some(true) only for an authoritative loaded log,
    /// Some(false) only when that exact block was read and was NOT a log,
    /// None for unloaded/out-of-range/unknown. Never turn None into false.
    pub fn block_removed(&self)->bool {self.removed_at.is_some()}
    pub fn observe(
        &mut self, tick:u64, block_is_log:Option<bool>,
        real_wood_inventory:u16
    )->HarvestStatus {
        if let Some(done)=self.final_status{return done;}
        if tick < self.last_tick {return HarvestStatus::Pending;}
        self.last_tick=tick;
        if block_is_log==Some(false) && self.removed_at.is_none() {
            self.removed_at=Some(tick);
        }
        if self.removed_at.is_some() && real_wood_inventory>self.initial_wood {
            let status=HarvestStatus::ObservedWoodGain {
                gained:real_wood_inventory-self.initial_wood,
            };
            self.final_status=Some(status);
            return status;
        }
        if tick.saturating_sub(self.started_at)>=HARVEST_TIMEOUT_TICKS {
            self.final_status=Some(HarvestStatus::UnverifiedTimeout);
            return HarvestStatus::UnverifiedTimeout;
        }
        if self.removed_at.is_some() {
            HarvestStatus::BlockRemovedWithoutPickup
        }else{
            HarvestStatus::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mining_packets_and_acks_do_not_prove_harvesting(){
        let mut e=HarvestEvidence::new([1,64,1],0,0);
        assert_eq!(e.observe(85,Some(true),0),HarvestStatus::Pending);
        assert_eq!(e.observe(86,None,0),HarvestStatus::Pending);
    }
    #[test]
    fn removed_block_without_inventory_is_not_a_harvest(){
        let mut e=HarvestEvidence::new([1,64,1],0,0);
        assert_eq!(e.observe(80,Some(false),0),HarvestStatus::BlockRemovedWithoutPickup);
        assert_eq!(e.observe(100,None,0),HarvestStatus::BlockRemovedWithoutPickup);
        assert_eq!(e.observe(200,None,0),HarvestStatus::UnverifiedTimeout);
    }
    #[test]
    fn wood_gain_alone_does_not_prove_target_was_removed(){
        let mut e=HarvestEvidence::new([1,64,1],0,2);
        assert_eq!(e.observe(30,Some(true),5),HarvestStatus::Pending);
        assert_eq!(e.observe(45,None,5),HarvestStatus::Pending);
        assert_eq!(e.observe(50,Some(false),5),HarvestStatus::ObservedWoodGain{gained:3});
    }
    #[test]
    fn removal_and_personal_inventory_gain_are_correlated(){
        let mut e=HarvestEvidence::new([1,64,1],10,1);
        assert_eq!(e.observe(30,Some(false),1),HarvestStatus::BlockRemovedWithoutPickup);
        assert_eq!(e.observe(34,Some(false),2),HarvestStatus::ObservedWoodGain{gained:1});
        assert_eq!(e.observe(180,Some(true),1),HarvestStatus::ObservedWoodGain{gained:1});
    }
    #[test]
    fn stale_observations_do_not_rewind_evidence(){
        let mut e=HarvestEvidence::new([1,64,1],100,0);
        assert_eq!(e.observe(110,Some(true),0),HarvestStatus::Pending);
        assert_eq!(e.observe(104,Some(false),12),HarvestStatus::Pending);
        assert_eq!(e.observe(111,Some(false),0),HarvestStatus::BlockRemovedWithoutPickup);
    }
}
