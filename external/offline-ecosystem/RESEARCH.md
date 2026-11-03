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
