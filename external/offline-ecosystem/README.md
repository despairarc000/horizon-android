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
