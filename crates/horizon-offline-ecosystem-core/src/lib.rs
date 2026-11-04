//! Offline population decision core. No cloud calls, LLM, graphics, or network.
//! Pure deterministic logic; all player actions must be validated by Horizon's server.

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Personality {
    pub curiosity: f32,
    pub aggression: f32,
    pub sociability: f32,
}
impl Personality {
    pub fn new(curiosity: f32, aggression: f32, sociability: f32) -> Option<Self> {
        let valid = [curiosity, aggression, sociability]
            .into_iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(&v));
        valid.then_some(Self { curiosity, aggression, sociability })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Needs {
    pub hunger: u8,
    pub energy: u8,
    pub health: u8,
}
impl Needs {
    pub fn new(hunger: u8, energy: u8, health: u8) -> Option<Self> {
        (hunger <= 100 && energy <= 100 && health <= 100)
            .then_some(Self { hunger, energy, health })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Realized,
    Abstract,
    Coarse,
}
impl Resolution {
    pub fn from_room_distance(distance: u32) -> Self {
        if distance <= 1 { Self::Realized }
        else if distance <= 3 { Self::Abstract }
        else { Self::Coarse }
    }
    pub fn decision_interval(self) -> u32 {
        match self { Self::Realized => 1, Self::Abstract => 4, Self::Coarse => 12 }
    }
    pub fn should_decide(self, tick: u64, bot_id: u32) -> bool {
        (tick + u64::from(bot_id)) % u64::from(self.decision_interval()) == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Intent {
    Flee,
    Defend,
    Eat,
    Rest,
    ShareFood,
    Forage,
    SeekKnownFood,
    BuildShelter,
    GatherWood,
    Explore,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalObservation {
    pub enemy_at_position: bool,
    pub hungry_companion_at_position: bool,
    pub relationship_to_enemy: f32,
    pub relationship_to_companion: f32,
    pub local_food: u16,
    pub local_wood: u16,
    pub known_food_reachable: bool,
    pub shelter_here: bool,
}
impl Default for LocalObservation {
    fn default() -> Self {
        Self {
            enemy_at_position: false,
            hungry_companion_at_position: false,
            relationship_to_enemy: 0.0,
            relationship_to_companion: 0.0,
            local_food: 0,
            local_wood: 0,
            known_food_reachable: false,
            shelter_here: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BotView {
    pub needs: Needs,
    pub personality: Personality,
    pub food_inventory: u16,
    pub wood_inventory: u16,
}

/// This is deliberately a bounded reference selector, not a complete GOAP
/// implementation. The server bridge must validate actions and resource costs.
pub fn choose_intent(bot: &BotView, seen: &LocalObservation) -> Intent {
    let Needs { hunger, energy, health } = bot.needs;
    if seen.enemy_at_position && seen.relationship_to_enemy <= -0.65 {
        if health < 65 || bot.personality.aggression < 0.55 { return Intent::Flee; }
        if bot.personality.aggression >= 0.7 && energy > 20 { return Intent::Defend; }
    }
    if hunger <= 60 && bot.food_inventory > 0 { return Intent::Eat; }
    if energy <= 25 { return Intent::Rest; }
    if seen.hungry_companion_at_position && bot.food_inventory >= 2 && hunger >= 45
        && (bot.personality.sociability >= 0.65 || seen.relationship_to_companion >= 0.4)
    { return Intent::ShareFood; }
    if hunger < 70 {
        if seen.local_food > 0 { return Intent::Forage; }
        if seen.known_food_reachable { return Intent::SeekKnownFood; }
    }
    if bot.wood_inventory >= 3 && !seen.shelter_here && hunger > 40 {
        return Intent::BuildShelter;
    }
    if bot.wood_inventory < 3 && seen.local_wood > 0 && hunger > 45 {
        return Intent::GatherWood;
    }
    Intent::Explore
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bot() -> BotView {
        BotView {
            needs: Needs::new(80, 80, 100).unwrap(),
            personality: Personality::new(0.8, 0.1, 0.9).unwrap(),
            food_inventory: 0,
            wood_inventory: 0,
        }
    }
    #[test]
    fn resolution_intervals() {
        assert_eq!(Resolution::from_room_distance(0), Resolution::Realized);
        assert_eq!(Resolution::from_room_distance(3), Resolution::Abstract);
        assert_eq!(Resolution::from_room_distance(4), Resolution::Coarse);
        assert!(Resolution::Coarse.should_decide(12, 0));
        assert!(!Resolution::Coarse.should_decide(11, 0));
    }
    #[test]
    fn starvation_needs_food() {
        let mut b = bot();
        b.needs.hunger = 45;
        b.food_inventory = 2;
        assert_eq!(choose_intent(&b, &LocalObservation::default()), Intent::Eat);
    }
    #[test]
    fn food_sharing_uses_inventory_and_sociality() {
        let mut b = bot();
        b.food_inventory = 3;
        let o = LocalObservation { hungry_companion_at_position: true, ..Default::default() };
        assert_eq!(choose_intent(&b, &o), Intent::ShareFood);
    }
    #[test]
    fn hostility_changes_priority() {
        let b = bot();
        let o = LocalObservation {
            enemy_at_position: true,
            relationship_to_enemy: -0.9,
            ..Default::default()
        };
        assert_eq!(choose_intent(&b, &o), Intent::Flee);
    }
    #[test]
    fn building_requires_materials() {
        let mut b = bot();
        b.wood_inventory = 3;
        assert_eq!(choose_intent(&b, &LocalObservation::default()), Intent::BuildShelter);
        b.wood_inventory = 2;
        assert_eq!(choose_intent(&b, &LocalObservation::default()), Intent::Explore);
    }
    #[test]
    fn rejects_invalid_character_traits() {
        assert!(Personality::new(f32::NAN, 0.5, 0.5).is_none());
        assert!(Personality::new(0.2, 1.2, 0.5).is_none());
        assert!(Needs::new(101, 0, 0).is_none());
    }
}

/// Deterministic room-graph prototype for multi-bot simulation.
pub mod population;
pub mod navigation;
pub mod harvest;
pub mod local_path;
pub mod explore;
pub mod terrain_path;
/// Per-individual proximity-based social memory; no omniscient relationships.
pub mod social;
