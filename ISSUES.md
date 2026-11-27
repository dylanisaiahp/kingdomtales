# KingdomTales — Active Issues

**Date:** 2026-11-26\
**Scope:** active, evidence-backed issues only. Resolved/stale items removed.\
**Authority:** `ISSUES.md` is authoritative for tracked work; cross-reference code/tests before marking resolved.

## Verification (latest)
- `cargo fmt --all --check`: pass
- `cargo check --all-targets`: pass
- `cargo clippy --all-targets -- -D warnings`: pass
- `cargo test --all-targets`: pass (**351 passed, 0 failed, 4 ignored**)
- `cargo run --bin shader_check`: pass (10 shader permutations; validates syntax/types only, not visuals)
- `cargo build`: pass
- No in-game/runtime smoke verification was performed in this pass.

## Explicitly Deferred
- Tree/bush/limb/root placement and root-size/selection-box alignment.
- Chunk fade-in refinement (sorting/artifacts/material strategy; initial alpha fade implemented; see `ROADMAP.md`).
- Runtime verification needed (not confirmed bugs): map Live/Explore modes and controls, hand crafting and primitive tools, placeable workbench/furnace, sky/cloud shaders and light-shaft modes, water depth tint, E4 pending-edit replay across save/load/teleport/streaming/unloaded flows, and input gating in modals/chat.

## Active Issues

### P1 / Maintainability (no confirmed correctness bug)

1. **Large monolithic modules / ordering ambiguity (maintainability risk)**
   - **Evidence:** `src/ui/mod.rs`, `src/world/mod.rs`, `src/interaction.rs`, `src/chat.rs`, `src/settings.rs` remain large; ordering relies on chains/registration; no Bevy app integration tests.
   - **Direction:** Opportunistic, behavior-preserving extraction only when touching confirmed issues; no broad refactor.

2. **Reserved placeholders/scaffolding (intentional, decision-deferred)**
   - **Evidence:** Audio tab placeholder; crafting natural-surface/station
     scaffolding unused by shipped recipes; furnace smelting unavailable;
     `held_item` generic tool mesh/attachment wall-response TODO-like behavior;
     `ShadowQuality::Off` has meaningless preset values; debug perf log path
     hard-coded `/tmp/opencode/kt-perf.log`. The map is not a placeholder:
     `Overlay::Map`, bindings and Live/Explore systems are implemented.
   - **Direction:** Leave reserved placeholders/scaffolding alone unless
     demonstrably broken or unsafe. No removal/deletion without product decision.

### P2 / Performance (measure before changing)

3. **Unmeasured hot paths / allocations (NEEDS REVIEW)**
   - **Evidence:** Cave daylight DDA per frame (`src/settings.rs`), pending-edit/chunk/edit replay costs, synchronous startup/teleport generation, per-frame UI recompute while open, drop scans, meshing/generation costs. Mitigations landed: crosshair cache, UI gating, E4 conservative gating.
   - **Direction:** No broad optimization without baseline/profile. Use ignored benches and in-game profiling before behavior changes.

### P2 / Rendering and map policy (needs decision)

4. **SSAO UI/wiring mismatch (NEEDS REVIEW)**
   - **Evidence:** `src/ui/settings_menu.rs` shows an `SSAO` row with a note that
     SSAO is unavailable due an unresolved rendering regression, but the row's
     click action still cycles `settings.ssao`. In `src/settings.rs`,
     `sync_player_camera_render_requirements` inserts/removes SSAO, depth and
     normal prepass components and forces MSAA Off when SSAO is non-Off.
   - **Direction:** Decide whether to expose SSAO or disable the row cleanly;
     runtime verification is needed before marking either state correct.

5. **Water Depth Tint can be stale after runtime seabed edits (NEEDS REVIEW)**
   - **Evidence:** `ChunkData::world_heights` is filled during generation in
     `src/world/generation.rs` and read by `water_depth_at_position` in
     `src/world/mesher.rs`. No runtime edit path refreshes that cache, so water
     tint can describe the original seabed after a player changes it.
   - **Direction:** Decide how and when to refresh the cached height for edited
     seabed columns; this is a visual-accuracy issue, not a build failure.

6. **Detailed light shafts + MSAA policy undecided (NEEDS REVIEW)**
   - **Evidence:** `src/settings.rs` forces MSAA Off when Detailed light shafts
     are active and restores `preferred_msaa_samples` otherwise, while
     `src/bin/shader_check.rs` validates a `DETAILED_SHAFTS` +
     `MULTISAMPLED_DEPTH` permutation. The runtime path therefore avoids a
     permutation the validator accepts.
   - **Direction:** Decide whether Detailed shafts should remain MSAA Off or
     support a validated multisampled-depth path.

7. **Prepass validation coverage is partial (NEEDS REVIEW)**
   - **Evidence:** `cargo run --bin shader_check` validates individual WESL
     permutations (10 currently), including light-shaft and foliage-prepass
     variants, but it does not exercise combined camera state (SSAO + Detailed
     shafts + MSAA) or visual output.
   - **Direction:** Decide which combined permutations must be covered before
     changing SSAO/DepthPrepass ownership; keep visual checks separate.

8. **Map camera parity/transitions undecided (NEEDS REVIEW)**
   - **Evidence:** Live mode uses a fixed north-up `MapCamera` into an offscreen
     texture; Explore mode uses CPU-composited cached snapshots. `Tab` switches
     modes, but rotation/free-camera behavior and transition behavior are not
     decided.
   - **Direction:** Product decision before adding camera controls; do not treat
     the current difference as a confirmed bug.

## Rules
- Remove resolved items once verified by code/tests.
- Keep `NEEDS REVIEW` only with concrete evidence and an explicit question/risk.
- Do not add new cleanup tasks during audit unless they are genuine blockers/correctness risks.
