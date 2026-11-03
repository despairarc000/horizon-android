import json
import unittest

from ecosim.engine import Bot, Personality, World, Zone, Simulation, distance
from ecosim.demo import make_demo

class EcosystemTests(unittest.TestCase):
    def test_accounting_survives_long_run(self):
        sim = make_demo()
        sim.step(1200)
        sim.invariant_check()
        self.assertGreater(sum(z.shelter for z in sim.world.zones.values()), 1)
        self.assertTrue(any(b.visited for b in sim.bots.values()))

    def test_one_room_per_action_no_teleport(self):
        sim = make_demo()
        for _ in range(400):
            before = {i:b.pos for i,b in sim.bots.items()}
            sim.step()
            for i,b in sim.bots.items():
                self.assertLessEqual(distance(before[i], b.pos),1)

    def test_perception_excludes_remote_resources(self):
        world = World({(0,0):Zone(food=0), (1,0):Zone(food=1), (2,0):Zone(food=99)})
        sim = Simulation(world,[Bot(0,"A",(0,0))],(0,0))
        sim.observe(sim.bots[0])
        self.assertNotIn((2,0),sim.bots[0].known)
        self.assertEqual(sim.bots[0].known[(1,0)][0],1)

    def test_food_sharing_changes_relationship_and_uses_inventory(self):
        a = Bot(0,"A",(0,0),Personality(sociability=1),food=3,hunger=90)
        b = Bot(1,"B",(0,0),hunger=40)
        sim = Simulation(World({(0,0):Zone()}),[a,b],(0,0))
        sim.step()
        self.assertEqual(a.food,2)
        self.assertGreater(b.relations[0],0)
        self.assertTrue(any('share B' in e for e in sim.events))
        self.assertEqual(sim.food_accounted()+sim.consumed_food, 3)

    def test_hostile_encounter_combat_and_flight(self):
        a=Bot(0,"A",(0,0),Personality(aggression=0.9),relations={1:-0.9})
        b=Bot(1,"B",(0,0),Personality(aggression=0.2),health=55,relations={0:-0.9})
        sim=Simulation(World({(0,0):Zone(),(1,0):Zone()}),[a,b],(0,0))
        sim.step()
        self.assertEqual(b.health,47)
        self.assertEqual(b.pos,(1,0))
        self.assertEqual(b.last_action,"flee")

    def test_building_consumes_three_wood(self):
        a=Bot(0,"A",(0,0),wood=3)
        sim=Simulation(World({(0,0):Zone()}),[a],(0,0))
        sim.step()
        self.assertTrue(sim.world.zones[(0,0)].shelter)
        self.assertEqual(a.wood,0)
        sim.invariant_check()

    def test_abstract_and_coarse_frequency(self):
        bot=Bot(0,"Far",(6,6))
        sim=Simulation(World({(0,0):Zone(),(6,6):Zone()}),[bot],(0,0))
        self.assertEqual(sim.tier(bot),"coarse")
        sim.step(36)
        self.assertEqual(sum('Far idle' in e for e in sim.events),3)
        self.assertLess(bot.hunger,85)

    def test_snapshot_roundtrip_and_future_replay(self):
        sim=make_demo(seed=221)
        sim.step(110)
        frozen=json.loads(sim.dumps())
        restored=Simulation.restore(frozen)
        self.assertEqual(sim.dumps(),restored.dumps())
        sim.step(160)
        restored.step(160)
        self.assertEqual(sim.dumps(),restored.dumps())

    def test_deterministic_seed(self):
        a,b=make_demo(seed=123),make_demo(seed=123)
        a.step(400);b.step(400)
        self.assertEqual(a.dumps(),b.dumps())
        c=make_demo(seed=124)
        c.step(400)
        self.assertNotEqual(a.dumps(),c.dumps())

    def test_rejects_invalid_state(self):
        with self.assertRaises(ValueError):
            Personality(aggression=1.1)
        with self.assertRaises(ValueError):
            Zone(food=-1)
        with self.assertRaises(ValueError):
            Simulation(World({(0,0):Zone()}),[Bot(0,"A",(0,0)),Bot(0,"B",(0,0))],(0,0))
        with self.assertRaises(ValueError):
            Simulation(World({(0,0):Zone()}),[Bot(0,"A",(8,8))],(0,0))

if __name__ == "__main__":
    unittest.main()
