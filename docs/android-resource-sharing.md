# Android nearby resource sharing (MLBB-style)

**Status:** Foundation only on `feat/android-resource-sharing`. The Android launcher does **not** yet expose Send/Receive, discover peers, or transfer bytes over Wi-Fi. No APK has been built or tested from this branch.

## Goal

Download and prepare Minecraft resources once, then let other Android phones with the same Horizon build retrieve what they are missing **directly from a nearby device**, without mobile data. Model the two-button flow on Mobile Legends: Bang Bang's Resource Transfer (2024): **Send**, **Receive**, nearby peer selection, missing-resource scan, progress, and verification.

This is **not** the ordinary resource-pack/mod sharing feature, and should not require ZIP creation, a file manager, root, or third-party apps.

## Intended flow

1. On the phone with a complete installation, open **Resource Sharing → Send**. Show a nearby device name and temporary pairing/approval UI. Display the source Minecraft version and install size.
2. On the other phone, open **Resource Sharing → Receive**. Discover nearby Horizon senders by Android Wi-Fi Direct or, as a compatibility fallback, local Wi-Fi/hotspot. Select one and confirm.
3. Compare the sender's versioned content manifest against the receiver's verified staged files. Display needed bytes and file count, then stream only missing files.
4. Hash-check each file (SHA-256). Keep verified files when interrupted and resume from them on reconnect.
5. Verify the entire final tree, then atomically activate it in the versioned game resource directory. The existing online Mojang installer remains the fallback.

Wi-Fi Direct peer discovery/pairing and Android permissions must be handled explicitly; UDP broadcast alone is **not** equivalent to Wi-Fi Direct. Local transfer bytes must not require an Internet connection. An enabled mobile hotspot may present a practical fallback even where Wi-Fi Direct support is unavailable.

## Existing data model

- Source path: `<app-private-home>/.local/horizon-client/<horizon_core::VANILLA_VERSION>/`.
- Must be fully installed: `official_install::ready(home)`, including `horizon-resources.json` and `android-downloads-v2.json`.
- Receiving staging path: `<app-private-home>/.local/horizon-client/resource-share.partial/`.
- Do **not** share `<app-private-home>/profiles/`, worlds, accounts, tokens, saved passwords, server lists, logs, mods, shader packs, screenshots or downloaded cache.
- Engine build/version must be compatible; Minecraft version by itself may not be enough. Add a stable resource-format/engine compatibility identity to the network handshake before enabling the UI.

## Foundation already added

`crates/horizon-client/src/resource_share_manifest.rs` implements:

- Deterministic inventory of the prepared versioned resource directory, with per-file SHA-256.
- Strict manifest protocol/version, path, file-count, size and duplicate checks.
- Receiver-side missing-file determination from verified staged content.
- Safe per-file streaming sink with checksum/size verification, temporary files and a resumable staging directory.
- Final whole-tree verification and atomic promotion, refusing to overwrite an existing installed version.
- Unit tests for successful staged copy, rejected corrupted input and traversal paths.

This module has **not** been compiled or run in this session. The code and tests require CI or an environment with Rust and the Horizon dependencies. Do not treat it as a complete working feature.

## Next engineering tasks

1. Add an authenticated, bounded local sender transport and receiver client (no global file server). Use short-lived random pairing credentials and explicit sender approval.
2. Add Android Wi-Fi Direct discovery, connection lifecycle and permissions. Retain same-Wi-Fi/hotspot fallback; detect and explain unsupported peer-to-peer devices.
3. Integrate Send/Receive views into the existing Rust launcher in `crates/horizon-client/src/android/launcher_pages.rs`, using background workers so rendering never blocks.
4. Show the progress, version mismatch, connection lost/retry and cancel states; permit explicit staging reset if an older manifest is incompatible.
5. Validate no traffic over mobile data; check permission denials, both phones with Wi-Fi on but no Internet, Android 10 through 15+, and a Samsung Galaxy A06 5G.
6. Run Rust unit tests, `tools/pr_ready.py --quick`, Android build/CI and two-real-device test. An APK or user-ready feature should only be reported after these pass.

## Security principles

- No publicly accessible upload API; receiver never names an arbitrary host path and sender serves only entries in its generated, fixed-scope manifest.
- Reject unexpected file paths, symlinks, duplicates, large files, oversized manifests, incompatible versions, stale staged files and hash mismatches.
- Consent and pairing expire. Bind transfer only while the sender's screen/session permits it, limit peers and use I/O timeouts.
- Keep files already verified when a transfer is interrupted. Do not overwrite valid installed resources.
- **Never commit or bundle Mojang resource files**; sharing is between users' devices after resource acquisition.

## References

- MLBB Resource Transfer overview: https://gamingonphone.com/guides/how-to-use-resource-transfer-feature-in-mobile-legends-mlbb/
- Android source: `crates/horizon-client/src/android/launcher.rs` and `launcher_pages.rs`
- Official installer: `crates/horizon-client/src/official_install.rs`
- Safe extraction and version path: `crates/horizon-client/src/resource_install.rs`
