# Offline ecosystem foundation (experimental real-world integration)

Goal: a fully **offline, multi-bot, emergent Minecraft population** inspired by Rain World's design principles. Not an LLM agent. Each individual has needs, personality, local observations, social memory, and independent decisions. Jobs and relationships emerge from experiences rather than fixed roles.

## Implemented

- Engine-independent Rust decision core, three simulation-resolution tiers, deterministic bounded planning, per-bot exploration memory, stuck recovery, and a conservative 3D standing-surface route planner.
- Python reference model (not shipped with the game): six autonomous bots demonstrating exploration, foraging, resource accounting, shelter building, food sharing, combat, fleeing, limited local perception, and deterministic serialization/replay. Ten Python tests.
- An opt-in local multiplayer experiment in Horizon's actual integrated-world server: headless survival players join via in-memory protocol, receive authoritative local surveys, attempt normal walking/mining/eating, and validate all world changes on the server. See RESEARCH.md.

The Python reference simulator is a small **logical room graph**, not Minecraft voxel terrain; it is not shipped. The Rust decision core is now called by the opt-in real-world bot coordinator, but the reference simulator is independent. The live players are **experimental protocol-backed actors**, not complete intelligent survival players; neither a full GOAP system nor background population abstraction is implemented.

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

Two experimental headless players have joined and moved in a **real integrated Minecraft world**. A natural-world run has now verified a **first successful three-log survival harvest** by one bot (details below). Repeatability, crafting, real-world food acquisition, a production-ready bot, an Android build and Android performance remain unverified. Keep vanilla rules and game resources intact.

## Persistence and memory bounds

The Rust `Population` now supports a versioned, deterministic JSON snapshot with validation on restore. It conserves finite resources and caps remembered event history at 4096 events. This covers **logical ecosystem state only**; saving and restoring genuine Minecraft player and chunk state remains a later integration task.

## Experimental in-game bot (not a completed ecosystem)

The fork now has a temporary, **opt-in only** host-side local-player probe:

    HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS=2

Set it in the process environment **before launching a locally built Horizon game** using a scratch world. Allowed values are 1–3; all other values, including unset, create zero bots. It attaches headless in-process protocol players to the real LAN hub. The players now use the Rust decision selector and server-sensed resource targets, safe level-ground exploration with blocked-step recovery, basic survival inventory/food observations, and real mining packets. Server-verified one-block ascents and descents are now executed experimentally; wider 3D navigation is incomplete. No crafting, construction, social behavior, sustained survival, or abstract offscreen population is implemented.

Normal public CI runs resource-free core, network, and reference tests. Separately, `.github/workflows/offline-ecosystem-real-world.yml` retrieves the pinned official game resources into a private ephemeral CI workspace and runs a true two-bot loaded-world test. **Only the real-world log's server positions and inventory changes count as gameplay proof.**

The current configuration is a desktop/developer environment variable; the Android launcher does not yet expose a toggle. Real Android build and device testing remain unverified.

## Harvesting diagnostic after live acceptance

The real-world CI harness now continues for 70 seconds after both bots are loaded. It reports the actual number of mining starts, exact server-confirmed target removals, correlated survival inventory gains, and unverified attempts. It **does not count a mining attempt as success**, and a run can pass admission/sensor acceptance with zero wood collected. No test world items are spawned. A first successful natural survival gathering test is recorded below, but broader repeatability and full survival automation remain open.

## Measured real-world checkpoint — 2026-11-04

[Real-world workflow #41758884742](https://github.com/despairarc000/horizon-android/actions/runs/41758884742) passed both local-player login and authoritative loaded-world observation. The **server-observed** path length was 26.05 blocks for EcoAI0 (19.10 net displacement), and 2.68 blocks for EcoAI1 (which then stalled). The former started four mining attempts; there were **zero confirmed block removals and zero correlated wood pickups**. Both bots stayed at their initial sampled Y levels. This is proof of some real movement, not proof of general navigation or survival.

Follow-up fixes are on this feature branch: a bounded per-bot blocked-step memory only quarantines an edge after repeated **unchanged authoritative positions** following actual submitted movement packets; mining diagnostics distinguish server reach/spawn/border gates and START/STOP outcomes. See the current GitHub Actions runs for validation. Do not assume an in-progress or cancelled workflow has passed.

### Next acceptance criteria

1. **Achieved in experimental runs:** two real bots moved via server-confirmed position updates, with successful one-block uphill/downhill landings.
2. **First success:** three natural logs broken and three real wood inventory increments by EcoAI1 in a loaded survival world; repeat across different seeds and conditions.
3. **Next:** independently verify the exact server item-to-inventory transfer, make pickup and exploration reliable across terrain, and keep task-specific failures bounded.
4. **Later:** real crafting, food acquisition, individual social behavior, offscreen population abstraction, Android performance profiling.

## First confirmed natural-world wood harvest — 2026-11-04

[GitHub Actions run #41826770398](https://github.com/despairarc000/horizon-android/actions/runs/41826770398) tested two opt-in survival bots using a real Minecraft world and the game's normal server action rules. EcoAI1 located naturally generated dark-oak wood and achieved **three** server-confirmed block-to-air mutations, **three** real dropped log entities, **three** correlated **+1** wood inventory increments, and a final server-surveyed wood count of **3**. Both bots also showed meaningful server-authoritative movement. No creative inventory grant, synthetic item or teleported pickup was used. EcoAI0 did not collect wood in this run. This establishes one valid autonomous harvesting sequence, not sustained survival or all-seed reliability.

Follow-up improvements include prioritizing connected pickup positions in the bounded terrain survey, preventing inaccessible tree mining, suppressing two-ledge exploration loops, and instrumenting Horizon's actual guest inventory transfer. **The latter instrumented implementation requires its own subsequent successful run before calling its new telemetry verified.**
