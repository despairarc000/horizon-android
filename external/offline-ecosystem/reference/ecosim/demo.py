"""Run: PYTHONPATH=external/offline-ecosystem/reference python -m ecosim.demo"""
from collections import Counter
from .engine import Bot, Personality, World, Zone, Simulation

def make_demo(seed: int = 2026) -> Simulation:
    zones = {(x,y):Zone(food=3 if (x+y)%3 == 0 else 1,
                        wood=4 if (x*2+y)%4 == 0 else 1,
                        shelter=(x,y)==(0,0))
             for x in range(7) for y in range(7)}
    bots = [
        Bot(0,"Ash",(1,0),Personality(.95,.15,.8)),
        Bot(1,"Bran",(3,2),Personality(.2,.80,.12)),
        Bot(2,"Cove",(1,1),Personality(.6,.2,.95)),
        Bot(3,"Dusk",(6,6),Personality(.9,.25,.5)),
        Bot(4,"Ember",(4,5),Personality(.35,.7,.2)),
        Bot(5,"Fern",(0,5),Personality(.8,.1,.9)),
    ]
    bots[0].relations[2] = .5
    bots[2].relations[0] = .5
    return Simulation(World(zones),bots,(0,0),seed)

def main() -> None:
    sim = make_demo()
    print("Offline ecosystem reference — no Minecraft required")
    sim.step(240)
    for b in sorted(sim.bots.values(),key=lambda x:x.bot_id):
        print(f"{b.name:5} pos={str(b.pos):7} tier={sim.tier(b):8} hp={b.health:3} "
              f"food={b.food} wood={b.wood} last={b.last_action}")
    print("actions:",dict(Counter(event.split()[2] for event in sim.events)))
    print("wood conserved:",sim.wood_accounted()==sim.initial_wood)
    print("food conserved with consumption:",sim.food_accounted()+sim.consumed_food==sim.initial_food)
    print("save/reload stable:",sim.dumps()==Simulation.restore(sim.snapshot()).dumps())
    friendly = [Bot(0,"Donor",(0,0),Personality(sociability=0.95),food=3,hunger=90),
                Bot(1,"Hungry",(0,0),hunger=40)]
    social = Simulation(World({(0,0):Zone()}),friendly,(0,0))
    social.step()
    print("social interaction:",social.events[0],"recipient trust:",round(friendly[1].relations[0],2))
    rivals = [Bot(0,"Rival",(0,0),Personality(aggression=0.9),relations={1:-0.9}),
              Bot(1,"Target",(0,0),Personality(aggression=0.1),health=55,relations={0:-0.9})]
    conflict = Simulation(World({(0,0):Zone(),(1,0):Zone()}),rivals,(0,0))
    conflict.step()
    print("conflict interaction:",conflict.events[:2])

if __name__ == "__main__":
    main()
