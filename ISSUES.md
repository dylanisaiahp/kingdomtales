# KingdomTales — Active Issues

**Date:** 2026-11-21  
**Scope:** active, evidence-backed issues only. Resolved/stale items removed.  
**Authority:** `ISSUES.md` is authoritative for tracked work; cross-reference code/tests before marking resolved.

## Verification (latest)
- `cargo fmt --all --check`: pass
- `cargo check --all-targets`: pass
- `cargo clippy --all-targets`: pass (pre-existing `clippy::too_many_arguments` warnings only)
- `cargo test --all-targets`: pass (**322 passed, 0 failed, 4 ignored**)

## Explicitly Deferred
- Tree/bush/limb/root placement and root-size/selection-box alignment.
- Chunk fade-in refinement (sorting/artifacts/material strategy; initial alpha fade implemented; see `ROADMAP.md`).
- Runtime verification needed (not confirmed bugs): E4 pending-edit replay across save/load/teleport/streaming/unloaded flows; F5 input gating in modals/chat; recent changes (chunk fade-in, lantern glow/slot behavior, magnet auto-pickup, fly/noclip stamina, drop visuals, leaf stick/fiber, bush felling, held-item/preview anim).

## Active Issues

### P1 / Maintainability (no confirmed correctness bug)

1. **Large monolithic modules / ordering ambiguity (maintainability risk)**
   - **Evidence:** `src/ui/mod.rs`, `src/world/mod.rs`, `src/interaction.rs`, `src/chat.rs`, `src/settings.rs` remain large; ordering relies on chains/registration; no Bevy app integration tests.
   - **Direction:** Opportunistic, behavior-preserving extraction only when touching confirmed issues; no broad refactor.

2. **Reserved placeholders/scaffolding (intentional, decision-deferred)**
   - **Evidence:** `Overlay::Map`/map bindings/audio placeholders intentional; crafting natural-surface/station scaffolding unused by shipped recipes; `held_item` generic tool mesh/attachment wall-response TODO-like behavior; `ShadowQuality::Off` has meaningless preset values; debug perf log path hard-coded `/tmp/opencode/kt-perf.log`.
   - **Direction:** Leave reserved placeholders/scaffolding alone unless demonstrably broken or unsafe. No removal/deletion without product decision.

### P2 / Performance (measure before changing)

3. **Unmeasured hot paths / allocations (NEEDS REVIEW)**
   - **Evidence:** Cave daylight DDA per frame (`src/settings.rs`), pending-edit/chunk/edit replay costs, synchronous startup/teleport generation, per-frame UI recompute while open, drop scans, meshing/generation costs. Mitigations landed: crosshair cache, UI gating, E4 conservative gating.
   - **Direction:** No broad optimization without baseline/profile. Use ignored benches and in-game profiling before behavior changes.

## Rules
- Remove resolved items once verified by code/tests.
- Keep `NEEDS REVIEW` only with concrete evidence and an explicit question/risk.
- Do not add new cleanup tasks during audit unless they are genuine blockers/correctness risks.
