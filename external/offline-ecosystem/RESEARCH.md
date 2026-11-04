# Horizon research / integration checkpoints

Source inspected: https://github.com/horizon-rust/horizon and https://github.com/Vasysik/horizon-android (Android branch, 2026-11-04).

- crates/horizon-client/src/game/integrated.rs: single-player starts a separate server thread; the rendered client joins over an internal link.
- crates/horizon-client/src/game/levels.rs: Game::tick_server runs dimensions and authoritative tick phases; the population system belongs on this side, never inside the render loop.
- crates/horizon-client/src/game/lan_actions.rs: as_guest/enter_guest share mining, inventory and combat code through temporary per-player state swapping. Their connection-ID assumptions must be addressed before building a connection-free BotActor.
- crates/horizon-client/src/lan/guest.rs: Guest contains live network metadata as well as player position and inventory, so direct construction is not a safe shortcut.
- crates/horizon-client/src/mobs/ai.rs, goals.rs, path.rs: existing goals and A* terrain navigation are useful references, but lack human-style excavation/bridging/crafting.
- crates/horizon-client/src/game/native_lifecycle.rs: authoritative lifecycle hooks are available; the observed hooks alone do not imply mutation rights.
- crates/horizon-client/src/game/native_entities.rs: spawning normal entities is not a general API for creating independent survival players.
- crates/horizon-bots/src/main.rs: load-testing protocol bots; movement/combat test fixtures, not autonomous survival bots.
- crates/horizon-client/src/game/persist.rs and save.rs: investigate before choosing persistent format.
- Android docs/android-features.md: ordinary APK supports sandboxed hmod, but source-only native integration requires a custom build.

## Proposed seam (NOT implemented)

Authoritative server tick -> population scheduler -> bot-specific local observation -> decision core -> validated action executor -> existing world/gameplay code -> save + entity tracking.

Do not implement visual movement alone and label it a player. Verification must cover reach, timing, drops, inventory debits, combat outcomes, pause/resume, world reload and multi-bot synchronization.

## Policy

Keep experimental code isolated; use public-source-only tests. Keep core standalone until a real player-action bridge can be tested without interfering with Horizon vanilla parity.

## Native player protocol experiment (2026-11-04)

- Connection-free gameplay state is still the eventual target, but a temporary `Pipe`-backed `LocalPlayer` uses the **real** `Lan::connect_local` protocol. It avoids any window, JVM or TCP. It does not spawn an independent player unless the authoritative server handles the connection.
- The Android fork's `lan/conn.rs` enforces a `horizon:block_registry` challenge for Horizon-branded clients. The trusted same-process bot echoes that fingerprint; it is not safe for arbitrary remote clients because it doesn't prove independently loaded registry parity.
- The `lan_bot.rs` integration test uses the real LAN hub and requires prepared game resources. It tests joining, chunk readiness, a movement packet and a block-use event. Its fixture directly places a block on receipt and therefore **does not verify survival item consumption, reach or block placement**.
- Next: connect this client to the actual `Game::tick_lan` path, establish server-acknowledged survival actions, then schedule multiple individuals and profile CPU/memory cost.

## Resource-free real networking test

The Android fork has `SyncData::empty()` available during its own library unit tests. The new `offline_ecosystem_local_player_joins_real_lan_without_resources` test instantiates Horizon's **actual** `Lan::start_with`, calls `connect_local`, completes a full local login and configuration with the ecosystem client, then verifies the real LAN raises a joined event. It does not instantiate the Minecraft world. The separate `lan_bot.rs` hub/world test still needs prepared game assets.

## Authoritative sensor bridge (prototype)

An opt-in `ExperimentalBots` coordinator now scans a limited 7x4x7 **loaded-block neighborhood** of each real connected guest every eight server ticks. It sends only server position, actual health, actual hunger, and a nearby log coordinate over a capacity-one nonblocking channel to its corresponding headless bot. The bot uses `ecosystem_core::choose_intent` and tries short level-ground steering toward wood, otherwise exploring a small local waypoint path.

This is an intentionally incomplete prototype: it does not yet do raycast visibility, full voxel navigation, reach-certified mining, inventory observation, crafting, combat, shelter building, social contact or population realization. Core `Needs::energy` is set to 100 rather than inventing a Minecraft stamina mechanic. Background workers have no access to full world state. Never interpret a steering attempt as confirmed movement; the next authoritative survey reconciles position.

## Real, server-validated mining attempt (prototype)

Headless bots now use a capacity-one server survey with a capped list of 32 nearby log coordinates (not LOS-aware). When a log is near and approximately level, the bot emits `PlayerAction START`, waits at least 85 non-paused server survey ticks, and emits `STOP`. Horizon's own server checks reach, protection, tool mining progress, inventory and block drops. If a surveyed block disappears the bot stops the attempt; **disappearance is not credited as a successful harvest or inventory gain**. Retries are capped to two for the same block, with a cooldown.

No crafting, item pickup confirmation, vertical navigation or obstacle-aware pathfinding is implemented. The test still requires a full resource-backed world execution before we can claim a bot mined a log.
