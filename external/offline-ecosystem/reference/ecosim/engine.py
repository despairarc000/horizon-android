"""Executable offline ecosystem reference. Does not use Minecraft APIs."""
from __future__ import annotations
from dataclasses import dataclass, field
import json
from typing import Any

Point = tuple[int, int]


@dataclass
class Zone:
    food: int = 0
    wood: int = 0
    hazard: int = 0
    shelter: bool = False

    def __post_init__(self) -> None:
        if min(self.food, self.wood, self.hazard) < 0:
            raise ValueError("negative resources/hazard")


@dataclass
class Personality:
    curiosity: float = 0.5
    aggression: float = 0.2
    sociability: float = 0.5

    def __post_init__(self) -> None:
        if not all(0 <= v <= 1 for v in vars(self).values()):
            raise ValueError("personality values must be in [0,1]")


@dataclass
class Bot:
    bot_id: int
    name: str
    pos: Point
    personality: Personality = field(default_factory=Personality)
    hunger: int = 85
    energy: int = 85
    health: int = 100
    food: int = 0
    wood: int = 0
    relations: dict[int, float] = field(default_factory=dict)
    known: dict[Point, tuple[int, int]] = field(default_factory=dict)
    visited: set[Point] = field(default_factory=set)
    last_action: str = "spawn"
    alive: bool = True


@dataclass
class World:
    zones: dict[Point, Zone]

    def neighbors(self, pos: Point) -> list[Point]:
        x, y = pos
        return sorted(p for p in ((x-1, y), (x+1, y), (x, y-1), (x, y+1)) if p in self.zones)


def distance(a: Point, b: Point) -> int:
    return abs(a[0] - b[0]) + abs(a[1] - b[1])


class Simulation:
    INTERVALS = {"realized": 1, "abstract": 4, "coarse": 12}

    def __init__(self, world: World, bots: list[Bot], observer: Point, seed: int = 1234):
        if len({b.bot_id for b in bots}) != len(bots):
            raise ValueError("bot IDs must be unique")
        if any(b.pos not in world.zones for b in bots):
            raise ValueError("bot starts outside world")
        if observer not in world.zones:
            raise ValueError("observer outside world")
        self.world = world
        self.bots = {b.bot_id: b for b in bots}
        self.observer = observer
        self.clock = 0
        self.rng_state = seed & ((1 << 64) - 1) or 1
        self.events: list[str] = []
        self.initial_wood = self.wood_accounted()
        self.initial_food = self.food_accounted()
        self.consumed_food = 0

    def random_index(self, n: int) -> int:
        if n <= 0:
            raise ValueError("n must be positive")
        x = self.rng_state
        x ^= (x << 13) & ((1 << 64) - 1)
        x ^= x >> 7
        x ^= (x << 17) & ((1 << 64) - 1)
        self.rng_state = x & ((1 << 64) - 1)
        return self.rng_state % n

    def tier(self, bot: Bot) -> str:
        d = distance(bot.pos, self.observer)
        return "realized" if d <= 1 else ("abstract" if d <= 3 else "coarse")

    def wood_accounted(self) -> int:
        return sum(z.wood + (3 if z.shelter else 0) for z in self.world.zones.values()) + sum(b.wood for b in self.bots.values())

    def food_accounted(self) -> int:
        return sum(z.food for z in self.world.zones.values()) + sum(b.food for b in self.bots.values())

    def invariant_check(self) -> None:
        assert self.wood_accounted() == self.initial_wood, "wood created or destroyed"
        assert self.food_accounted() + self.consumed_food == self.initial_food, "food created or destroyed"
        assert all(b.pos in self.world.zones for b in self.bots.values()), "invalid position"
        assert all(z.wood >= 0 and z.food >= 0 for z in self.world.zones.values()), "negative zone resources"
        assert all(b.food >= 0 and b.wood >= 0 for b in self.bots.values()), "negative inventory"
        assert all(-1 <= trust <= 1 for b in self.bots.values() for trust in b.relations.values()), "invalid relationship"

    def observe(self, bot: Bot) -> list[Bot]:
        # Observe current logical room and adjacent rooms, never the whole world.
        for p in [bot.pos, *self.world.neighbors(bot.pos)]:
            z = self.world.zones[p]
            bot.known[p] = (z.food, z.wood)
        bot.visited.add(bot.pos)
        return [b for b in self.bots.values() if b.bot_id != bot.bot_id and b.alive and distance(b.pos, bot.pos) <= 1]

    def route(self, start: Point, goals: set[Point]) -> Point | None:
        """BFS over room graph; returns one adjacent step, never a teleport."""
        if start in goals:
            return start
        queue = [start]
        first = {start: start}
        for current in queue:
            for neighbor in self.world.neighbors(current):
                if neighbor in first:
                    continue
                first[neighbor] = neighbor if current == start else first[current]
                if neighbor in goals:
                    return first[neighbor]
                queue.append(neighbor)
        return None

    def change_relation(self, a: Bot, b: Bot, delta: float) -> None:
        a.relations[b.bot_id] = max(-1.0, min(1.0, a.relations.get(b.bot_id, 0.0) + delta))

    def do(self, bot: Bot, action: str, detail: str = "") -> None:
        bot.last_action = action
        if len(self.events) < 100_000:
            self.events.append(f"t={self.clock} {bot.name} {action}" + (f" {detail}" if detail else ""))

    def act(self, bot: Bot) -> None:
        if not bot.alive:
            return
        visible = self.observe(bot)
        same = [b for b in visible if b.pos == bot.pos]
        z = self.world.zones[bot.pos]
        enemies = [b for b in same if bot.relations.get(b.bot_id, 0) <= -0.65]
        if enemies and (bot.health < 65 or bot.personality.aggression < 0.55):
            options = [p for p in self.world.neighbors(bot.pos) if all(distance(p, b.pos) > 0 for b in enemies)]
            if options:
                bot.pos = options[self.random_index(len(options))]
                bot.energy = max(0, bot.energy-3)
                self.do(bot, "flee")
                return
        if enemies and bot.personality.aggression >= 0.7 and bot.energy > 20:
            enemy = enemies[0]
            enemy.health = max(0, enemy.health - 8)
            bot.energy = max(0, bot.energy-5)
            self.change_relation(enemy, bot, -0.2)
            self.do(bot, "attack", enemy.name)
            if enemy.health == 0:
                enemy.alive = False
                self.do(enemy, "died")
            return
        if bot.hunger <= 60 and bot.food:
            bot.food -= 1
            self.consumed_food += 1
            bot.hunger = min(100, bot.hunger + 37)
            self.do(bot, "eat")
            return
        if bot.energy <= 25:
            bot.energy = min(100, bot.energy + (25 if z.shelter else 15))
            self.do(bot, "rest")
            return
        recipients = [b for b in same if b.hunger < 55 and bot.food >= 2 and
                      (bot.personality.sociability >= 0.65 or bot.relations.get(b.bot_id, 0) >= 0.4)]
        if recipients and bot.hunger >= 45:
            other = min(recipients, key=lambda b: (b.hunger, b.bot_id))
            bot.food -= 1
            other.food += 1
            self.change_relation(other, bot, 0.25)
            self.change_relation(bot, other, 0.08)
            self.do(bot, "share", other.name)
            return
        if bot.hunger < 70:
            if z.food > 0:
                z.food -= 1
                bot.food += 1
                bot.energy = max(0, bot.energy-2)
                bot.known[bot.pos] = (z.food, z.wood)
                self.do(bot, "forage")
                return
            food_places = {p for p, (food, _) in bot.known.items() if food > 0}
            step = self.route(bot.pos, food_places)
            if step is not None and step != bot.pos:
                bot.pos = step
                bot.energy = max(0, bot.energy-2)
                self.do(bot, "seek_food")
                return
        if bot.wood >= 3 and not z.shelter and bot.hunger > 40:
            bot.wood -= 3
            z.shelter = True
            self.do(bot, "build_shelter")
            return
        if bot.wood < 3 and z.wood > 0 and bot.hunger > 45:
            z.wood -= 1
            bot.wood += 1
            bot.energy = max(0, bot.energy-3)
            bot.known[bot.pos] = (z.food, z.wood)
            self.do(bot, "gather_wood")
            return
        unseen = [p for p in self.world.neighbors(bot.pos) if p not in bot.visited]
        options = unseen or self.world.neighbors(bot.pos)
        if options:
            if bot.personality.curiosity < 0.3 and z.shelter and bot.energy < 70:
                bot.energy = min(100, bot.energy + 10)
                self.do(bot, "stay_home")
            else:
                bot.pos = options[self.random_index(len(options))]
                bot.energy = max(0, bot.energy-2)
                self.do(bot, "explore")
        else:
            self.do(bot, "idle")

    def step(self, n: int = 1) -> None:
        for _ in range(n):
            self.clock += 1
            for bot in sorted(self.bots.values(), key=lambda x: x.bot_id):
                if not bot.alive:
                    continue
                # Metabolism proceeds regardless of how often decisions run.
                if self.clock % 6 == 0:
                    bot.hunger = max(0, bot.hunger-1)
                if self.clock % 8 == 0:
                    bot.energy = max(0, bot.energy-1)
                if bot.hunger == 0 and self.clock % 12 == 0:
                    bot.health = max(0, bot.health-1)
                if bot.health == 0:
                    bot.alive = False
                    self.do(bot, "starved")
                    continue
                interval = self.INTERVALS[self.tier(bot)]
                if (self.clock + bot.bot_id) % interval == 0:
                    self.act(bot)
            self.invariant_check()

    def snapshot(self) -> dict[str, Any]:
        def point(p: Point) -> str:
            return f"{p[0]},{p[1]}"
        return {
            "clock": self.clock, "observer": list(self.observer), "rng_state": self.rng_state,
            "initial_wood": self.initial_wood, "initial_food": self.initial_food,
            "consumed_food": self.consumed_food,
            "zones": {point(p): vars(z).copy() for p,z in sorted(self.world.zones.items())},
            "bots": [dict(bot_id=b.bot_id, name=b.name, pos=list(b.pos),
                          personality=vars(b.personality).copy(), hunger=b.hunger,
                          energy=b.energy, health=b.health, food=b.food, wood=b.wood,
                          relations={str(k):v for k,v in sorted(b.relations.items())},
                          known={point(p):list(v) for p,v in sorted(b.known.items())},
                          visited=[list(p) for p in sorted(b.visited)],
                          last_action=b.last_action, alive=b.alive)
                     for b in sorted(self.bots.values(), key=lambda x: x.bot_id)]
        }

    @classmethod
    def restore(cls, value: dict[str, Any]) -> "Simulation":
        def point(s: str) -> Point:
            a,b = s.split(",")
            return (int(a), int(b))
        zones = {point(k): Zone(**v) for k,v in value["zones"].items()}
        bots = []
        for d in value["bots"]:
            bots.append(Bot(d["bot_id"], d["name"], tuple(d["pos"]),
                            Personality(**d["personality"]), d["hunger"], d["energy"],
                            d["health"], d["food"], d["wood"],
                            {int(k):v for k,v in d["relations"].items()},
                            {point(k):tuple(v) for k,v in d["known"].items()},
                            {tuple(p) for p in d["visited"]},
                            d["last_action"], d["alive"]))
        sim = cls(World(zones), bots, tuple(value["observer"]), seed=value["rng_state"])
        sim.clock = value["clock"]
        sim.rng_state = value["rng_state"]
        sim.initial_wood = value["initial_wood"]
        sim.initial_food = value["initial_food"]
        sim.consumed_food = value["consumed_food"]
        sim.invariant_check()
        return sim

    def dumps(self) -> str:
        return json.dumps(self.snapshot(), sort_keys=True, separators=(",", ":"))
