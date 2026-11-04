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

## Server inventory and actual food use

The opt-in survey now includes server-authoritative counts from the 36-slot inventory, distinguishing usable edible **hotbar** items from inaccessible main-inventory food, and counts actual log/stem/plank stacks. `ecosystem_core::choose_intent` consumes these counts. For hunger, the headless player can now send normal selected-hotbar and item-use packets with a 40-server-tick retry cooldown; Horizon handles and verifies any consumption. No resource is fabricated, and no inventory change is assumed before a later server survey. Moving food from the main inventory into the hotbar and actual shelter construction remain unimplemented.

## Research application: individual last-known targets and stuck recovery

Based on Rain World devlogs about target pursuit and bounded abstract knowledge (not a code port), `horizon-offline-ecosystem-core/src/navigation.rs` now maintains per-bot last-seen resource targets, TTL-based avoidance, and a StuckTracker-style detector. The detector counts **only** genuine server-observed movement failures after actual movement requests, then temporarily avoids the blocked target and selects another. Known targets have finite lifetime, disappear on negative local evidence, and never spawn or credit resources. The real-game worker filters logs with excessive height difference and won't mine a remembered-but-not-currently-observed block.

This is still a local steering prototype: no full voxel pathfinder, line-of-sight occlusion, jumping, or persisted individual memory yet. Unit tests cover stalled motion, real progress, bounded avoidance, last-seen expiration, duplicate survey rejection, and negative evidence.

## Integrated-server whitelist and headless acceptance correction

The first two-bot **actual headless world** run rejected both bot names with `multiplayer.disconnect.not_whitelisted`, while the owner loaded normally. Horizon correctly keeps the private integrated server locked except for the owner and registered local seats. Experimental identities now opt into this existing **memory-only** seat registry; external-network clients spoofing their names remain denied, and no operator/creative privileges are granted. Added an identity-and-admission unit test.

The same run timed out because `--world-frames` only advances while the graphical client has reached a specific playing state. Headless acceptance now uses a bounded `--frames` counter, records the console log in a private scratch directory, and checks that **both** named bots actually logged in (not merely that their threads started). This still requires a passing resource-backed CI run before being declared working.

## Exact-target mining evidence / actual item pickup

Mining requests now have a per-bot watched target: the server performs a **direct block lookup** for the exact active mining coordinate, returning `None` for an unloaded or unobserved block rather than pretending it disappeared. A bounded evidence record distinguishes a mining packet, confirmed disappearance, and a **real increase** in that guest's own wood inventory. The bot moves toward the removed block to attempt ordinary item pickup, because Horizon only picks up nearby dropped entities. A correlated result is logged as such; it does not fabricate inventory or prove causality if another entity broke the same block.

This is still preliminary: the bot needs line-of-sight, 3D navigation and an actual successful inventory gain observed in a resource-backed test. The tracker now expires after 400 server ticks without confirmation; this is a bounded follow-up to observed server-delayed breaks that could occur after the former 200-tick timeout.

## Bounded obstacle-aware ground routing

The worker now receives a limited 7x7 set of standable tiles from server-loaded blocks. A tile must have a full-collision floor, two blocks of clearance and no water/lava. A bounded BFS in `ecosystem_core::local_path` chooses the first cardinal step toward a reachable log or dropped item. Unknown cells and impossible routes are rejected rather than crossed blindly. Horizon's full mob A* remains a useful future reference, but its mob-specific evaluators cannot be directly substituted for survival player actions without changing their collision assumptions. **Current pathing is ground-only**; jumps, stairs, slabs, swimming and genuine 3D terrain navigation remain unimplemented.

## Memory-driven exploration replacing the fixed spawn loop

Previous 40-second in-game testing confirmed zero mining attempts despite both bots being loaded. Their old exploration only followed four fixed offsets within ~one block of spawn, preventing meaningful discovery. The Rust `ExplorationMemory` now chooses reachable, previously unvisited, locally surveyed ground tiles; per-bot visited memory is bounded to 256 and tie-breaking is individual. Every movement still uses ordinary player packets, the authoritative block grid, and Horizon's server collision checks. The next CI run logs ground-tile and log counts, and periodic genuine positions, allowing measurable tests for movement beyond spawn. This still cannot climb cliffs or jump yet.

## 3D local standing-surface diagnosis (2026-11-04)

The natural-world seed 42 run (commit dd236520) accepted both bots and their server surveys, but both remained at their initial spawn positions for the full observation period: each survey reported only 2 or 3 same-height traversable ground cells and zero logs. The root cause has not yet been proven. The server now probes the same 7x7 region across y-1, y and y+1; its logs report validated lower/higher standing cells and whether each bot's current column is accepted as supported ground. Unknown chunks remain blocked, and the worker is **not yet allowed to execute vertical jumps or drops**. Rust core adds a six-test conservative 3D route planner that distinguishes walking, one-block descent and jump-up, preserving server collision authority.

## Real survival block destruction confirmed (2026-11-04)

[Resource-backed CI run #41761961158](https://github.com/despairarc000/horizon-android/actions/runs/41761961158) joined two opt-in headless survival players. Each made measurable server-observed progress (20.13 and 24.32 blocks of sampled path). The server recorded eight mining STARTs, eight STOPs (six `Break`, two `Delay`) and **eight actual `guest_destroy` calls that returned OK and changed the watched block to air**. Thus *real server mutation is demonstrated*, not merely a successful packet exchange. However the bot-side correlation reported **zero recorded exact removals and zero inventory gains**, so harvesting remains incomplete. The source of the discrepancy was not fully isolated by this run.

Two observed evidence risks motivated focused fixes:

- `run_local` previously resumed exploring as soon as it sent STOP, even though `HarvestEvidence` could only observe exact blocks within a locally loaded survey window; the bot now stays near the original target while awaiting removal or a bounded timeout.
- The 200-tick evidence window was too short for some delayed server mining. The window is now 400 ticks (not infinite) and Rust tests cover late removal and subsequent inventory gain. A new one-time bot-side removal log fires on actual `None→Some` removal evidence, including when inventory gain happens during that same survey, instead of only while the bot still has an active mining START.

The next resource-backed run must distinguish **server block mutation**, **bot-side exact block observation**, **physical item collection**, and **personal inventory gain**. None of these can be inferred from another. Keep natural world rules and prevent artificial items or creative privileges in acceptance fixtures.

## Bounded natural-resource scouting and physical pickup limit (2026-11-04)

On [live run #41767851594](https://github.com/despairarc000/horizon-android/actions/runs/41767851594), two opt-in normal survival actors joined. The 13x13 loaded-block wood sensor found eight natural logs around EcoAI0. Its bounded goal-directed frontier approach led to one valid dark oak mining attempt, a server-confirmed world mutation, a newly spawned `minecraft:dark_oak_log` item, and a subsequent exact block-removal observation. All foundation and resource-backed CI checks passed. **Zero wood was credited to the bot**: server item pickup telemetry showed `max_horizontally_near=0` and `max_touchable=0`. EcoAI1 was locally trapped on same-height ground, showing no meaningful displacement. Passing login and movement acceptance must not be confused with completing the harvesting task.

The item began near Y=82.33 and settled at Y=82.00, while EcoAI0's authoritative feet stayed at Y=83.00. The ordinary guest pickup condition requires `drop.y > player.feet_y - 0.6`; the settled item violates that threshold. The bot's item-position steering correctly refuses a pickup route if the item is below that range, rather than pretending the player reached it. Horizontal packets alone cannot solve such cases. Some dropped items also remain horizontally outside the reachable same-height cells after mining destroys a support block.

The next engineering work is a **real physics-aware vertical motion controller**, not a larger packet or an item grant. Horizon's `horizon-world/src/living_motion.rs` contains normal gravity/jump calculations; `horizon-client/src/lan/hub.rs::position_change` performs collision and movement checks and `lan_actions.rs::guest_server_tick` applies jumping and fall state. The existing core `terrain_path.rs` already produces verified one-block descent and jump-up *route annotations*, but the headless protocol client has **no action to execute them yet**. A legitimate actuator should:

1. Derive only adjacent safe standing surfaces from loaded authoritative blocks, considering player-body width and clearance, not just floor center coordinates.
2. Run bounded falling/jumping movement over multiple actual 20-Hz ticks, using appropriate airborne/ground flags and respecting Horizon's physics, collision checks, and server corrections. Never teleport the player to the target elevation or override inventory.
3. Measure authoritative Y changes and touchable dropped entities before claiming that a vertical route worked. Time out and backtrack if the ground changes or movement is refused.
4. Only after a correlated log removal, spawned drop, **real pickup and actual inventory increment** pass together should the ecosystem's harvest action be marked complete.

The longer-range wood sensor remains bounded to 32 retained log coordinates and a 13x13 scan every eight game ticks for at most three experimental bots. For larger Android populations, most offscreen individuals must run through a separate bounded abstract/coarse simulator rather than each owning an always-on full protocol client.

## Executable vertical traversal and natural log harvesting (2026-11-04)

[Real-world CI #41786727599](https://github.com/despairarc000/horizon-android/actions/runs/41786727599) is the strongest completed physical-navigation checkpoint in this series. Two opt-in ordinary survival bots both joined, moved in authoritative server surveys, and mined one natural dark-oak log each. Both destructions returned OK and changed their actual world blocks to air; each spawned one real dropped item. Importantly, the **four attempted one-block downhill transitions produced four server-confirmed lower landings, with no reported packet rejection**. The first verified vertical movement is no longer an untested conceptual plan.

The remaining limitation is not resource spawning: **neither bot collected wood**. The items settled at Y=82, and the pickup diagnostic reported no drop horizontally near either bot. One bot descended to Y=82 but ended at an inconvenient disconnected surface; the other descended to Y=80, below the item's elevation. In all these steps, a world mutation or landing is **not** synonymous with harvesting. The older run #41761961158 established eight server block changes but was missing reliable bot correlation; later tests repaired that confirmation.

Work on the experimental branch now includes a wider 11x11x3 collision-verified standing-surface scan, bounded 13x13 log sensing, a one-pass walk/down connected-frontier planner, lower-trunk approach routes, and a **new one-block jump-up controller**. The jump-up controller uses finite, bounded normal player movement packets, gradual survival-style vertical motion, and an 80-tick bounded authoritative-landing check. Rust unit tests can establish the generated motion arc, but **they cannot prove that a real Horizon collision-checked survival player successfully climbed a block**. That requires the newer resource-backed run and an explicit server-authoritative uphill-landing trace.

Next technical milestones should be validated separately: (1) actual server-acknowledged uphill landing, (2) a drop entering horizontal and vertical pickup range, (3) a real item inventory increase correlated with the removed block, and only then (4) multi-task bot survival, tool use, crafting, and the Rain World-like population realization/abstraction layer. Off-screen bots should remain lightweight state machines rather than each consuming a full protocol client or graphics renderer, especially on low-memory Android.

## Item accessibility, recovery, and scavenging (2026-11-04)

[Resource-backed test #41793961086](https://github.com/despairarc000/horizon-android/actions/runs/41793961086) established that eight consecutive authoritative surveys without any known pickup standing location can terminate an inaccessible drop pursuit early, with an explicit **no pickup or inventory credit** outcome. One natural log destruction, one real item spawn, and one lower/upper survival landing were confirmed; **no inventory gain** occurred. This recovered the bot from a previously 400-tick stall, not from the physical collision problem itself.

[Follow-up real-world test #41795128447](https://github.com/despairarc000/horizon-android/actions/runs/41795128447) confirmed the same bounded recovery under the new **multi-bot scavenging** implementation. Both opt-in bots moved; EcoAI1 destroyed one natural dark-oak log, one dropped item entity spawned, one server-confirmed descent and one server-confirmed ascent occurred. The result was still `max_touchable=0`, `max_horizontally_near=0`, `correlated_wood_pickups=0`; the blocked-drop release counted once. One bot encountered temporarily unloaded destination chunks during exploration but continued walking. This run did not contain a safely reachable drop for the new scavenger, so it **does not establish a successful scavenging pickup**. Foundation checks for the same commit, including Rust, Python and Horizon LAN/game integration, passed in [run #41795134185](https://github.com/despairarc000/horizon-android/actions/runs/41795134185).

The current worker now orders real locally observed logs by a conservative **potential pickup standing surface** before mining, abandons newly spawned drops only after repeated server-verified absence of any suitable pickup stand, and can independently seek **other bots' actual dropped wood items** through a bounded, connected 3D standing-surface route. It never grants inventory or infers an item from a mined block. Mining one solid trunk log within reach is not equivalent to having a supported body position adjacent to the resulting drop. In dense dark-oak trunks, ordinary logs can be removed while the remaining wood and terrain prevent the player from occupying any position inside the item's pickup box.

**Next blocker:** make the resource task *access-aware*: choose an exposed and safely collectible trunk, or deliberately open a collision-safe route by mining obstructing logs and verifying the newly available standing surfaces. Route and pickup must be checked again on the authoritative world after each break. A synthetic pickup or teleport would hide the exact constraint being investigated. Once a real inventory increment is observed in a normal integrated server, build the reusable need-driven resource task and then connect the population state machine, relationships and coarse offscreen ecosystem simulation.

