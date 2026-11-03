# Offline ecosystem foundation (pre-integration)

Goal: a fully **offline, multi-bot, emergent Minecraft population** inspired by Rain World's design principles. Not an LLM agent, not a Minecraft mob reskin. Each individual has needs, personality, local observations, social memory and independent decisions. Jobs and relationships emerge from experiences rather than fixed roles.

## Status

- **Available:** independent, dependency-free Rust selector, three resolution tiers, bounded goal priority, unit tests; research into Horizon's player state and integrated server.
- **Not yet available:** actual spawned player actors, real Minecraft movement/mining/crafting/building, save format, chunk transitions, Android APK or measured performance.
- **Prior reference simulation:** a Python model with finite resource accounting, local knowledge, social encounters and save/replay was tested outside Horizon (10 Python tests passing). It is not equivalent to full Minecraft gameplay.

## Run the Rust core

From the repository root: 

    cargo test -p horizon-offline-ecosystem-core

## Development direction

1. Model bot identity and per-individual state, with deterministic, bounded scheduling.
2. Prove one connection-free player-like actor can perform ordinary server-validated movement and block actions.
3. Add multi-bot simulation and actor-to-actor perception/relationships.
4. Add world-region abstraction and coherent re-realization, with persistence and resource conservation.
5. Benchmark on Android; never assume a distant simulated event can bypass world or inventory rules.

See the companion source map in RESEARCH.md. No Mojang files, gameplay binaries or resources are stored here.
