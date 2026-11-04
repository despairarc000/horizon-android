# Offline ecosystem foundation (pre-integration)

Goal: a fully **offline, multi-bot, emergent Minecraft population** inspired by Rain World's design principles. Not an LLM agent. Each individual has needs, personality, local observations, social memory, and independent decisions. Jobs and relationships emerge from experiences rather than fixed roles.

## Implemented

- Engine-independent Rust decision selector with six Rust unit tests, three simulation resolution tiers, and bounded action priorities.
- Python reference model (not shipped with the game): six autonomous bots demonstrating exploration, foraging, resource accounting, shelter building, food sharing, combat, fleeing, limited local perception, and deterministic serialization/replay. Ten Python tests.
- Source research and integration recommendations in RESEARCH.md.

The Python simulator is a small **logical room graph**, not actual Minecraft voxel terrain. The Rust core is not connected to it or to the game yet. Neither module implements a real player entity. Current bot behavior is a prototype, not a full utility AI/GOAP implementation.

## Test

    cargo test -p horizon-offline-ecosystem-core
    PYTHONPATH=external/offline-ecosystem/reference python -m unittest discover -s external/offline-ecosystem/reference/tests -v
    PYTHONPATH=external/offline-ecosystem/reference python -m ecosim.demo

## Development milestones

1. Add server-owned simulated-player state and a safe action bridge.
2. Prove a real survival block break/place, movement, combat, and inventory roundtrip.
3. Add individual perceptions, memory, needs, and relationships to actual server actors.
4. Add realized/abstract/coarse transitions with persistence and real conservation invariants.
5. Benchmark bot counts and game performance on the Android phone.

No actual Minecraft bot, generated APK, or Android performance result is claimed yet. Keep vanilla rules and game resources intact.

## Persistence and memory bounds

The Rust `Population` now supports a versioned, deterministic JSON snapshot with validation on restore. It conserves finite resources and caps remembered event history at 4096 events. This covers **logical ecosystem state only**; saving and restoring genuine Minecraft player and chunk state remains a later integration task.

## Experimental in-game probe (NOT a completed bot)

The fork now has a temporary, **opt-in only** host-side local-player probe:

    HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS=2

Set it in the process environment **before launching a locally built Horizon game** using a scratch world. Allowed values are 1–3; all other values, including unset, create zero bots. It attaches headless in-process protocol players to the real LAN hub. These players currently only attempt a small walking square at spawn and service the Minecraft protocol; they do **not** yet use the Rain World-style population brain, navigate terrain, mine, craft, fight or build. It is a plumbing experiment, not a playable AI ecosystem.

On normal public GitHub CI the Horizon LAN test is **compiled**, but runtime gameplay tests are skipped if private Minecraft resources are absent. Passing CI therefore does not establish that the experimental player successfully joins or moves in a real game world.

The current configuration is a desktop/developer environment variable; the Android launcher does not yet expose a toggle. Real Android build and device testing remain unverified.

## Harvesting diagnostic after live acceptance

The real-world CI harness now continues for 40 seconds after both bots are loaded. It reports the actual number of mining starts, exact server-confirmed target removals, correlated survival inventory gains, and unverified attempts. It **does not count a mining attempt as success**, and a run can pass admission/sensor acceptance with zero wood collected. No test world items are spawned. A separate survival automation test will be required before claiming successful wood gathering.
