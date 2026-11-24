# Roadmap: unimplemented work only

This file lists features that are **not implemented yet**. Current behavior,
settled decisions, controls and gotchas are documented in `FEATURES.md`; do not
duplicate them here.

## 1. World generation and terrain

- Promote the declarative Rust biome/graph tables into external versioned data
  files so they can be tuned without a rebuild.
- Expose the existing ordered geology strata, cave thresholds and surface-opening
  density as configurable data instead of compiled constants.
- Add ore deposits as separate 3D bodies associated with host voxels. Deposits
  should support multi-voxel veins, independent geometry and yields such as
  granite plus iron ore.
- Add the remaining geological and processed building materials discussed in
  the original design (for example basalt, diorite, marble, gneiss, loam, clay,
  silt, peat, granite bricks, cobblestone and polished granite). Humus and red
  sandstone are already implemented.
- Explore a visual editor for terrain and biome parameters once the tables are
  external data files.

## 2. Rendering and streaming

Distant-terrain LOD is intentionally the last rendering milestone, after
biomes/data and caves. Do not start it as part of unrelated UI or performance
work.

- Add hardware-aware view-distance recommendations and preset tiers beyond the
  current manual slider and RAM hint.
- Keep generation distance, render distance and unload distance logically
  separate; the current streamer still couples them to one asymmetric stream
  volume plus an unload margin.
- Add occlusion culling where practical. Exposed-face culling and Bevy frustum
  culling are already implemented.
- Add distant terrain LOD without changing underlying voxel data:
  - Full detail near the player, reduced detail farther out.
  - Preserve silhouettes and avoid seams between detail levels.
  - Add hysteresis so LOD does not thrash at chunk boundaries.
  - Decide final thresholds with profiling rather than guesses.
- Refine chunk fade-in: current implementation uses per-chunk true-alpha fade materials during a short fade, then restores shared materials. Evaluate sorting/artifact tradeoffs, remesh behavior, and whether a dither/clip or per-chunk material strategy is better.
- Add explicit time-based and work-item budgets to the existing distance/urgent stream priority.
- Benchmark RAM/VRAM, CPU, GPU and generation cost to derive final budgets.

## 3. Map system

The map is implemented with Live 3D and Explore 2D modes (see `FEATURES.md`). Remaining work:

- **3D map:** an overhead live view of loaded chunks with a dedicated map
  camera, player position/orientation, and optional zoom, rotation and camera
  movement.
- **2D map:** a persistent top-down view of explored areas with fog of war,
  saved between sessions even after chunks unload.

Open work:

- Pressing M opens/closes the map and switches modes without losing position or
  zoom where practical.
- Keep exploration data independent of loaded chunk data and map rendering.
- Choose three visibility states: unexplored, explored and currently visible.
- Decide discovery radius and update rules.
- Add dynamic map resolution/LOD with cached lower-resolution levels; zooming
  must never reveal unexplored terrain.
- Decide map visual style, biome labels, landmarks, player markers and routes.
- Decide persistence format and save cadence for exploration data.

## 4. Inventory, tools and progression

- Decide the cost curve for backpack upgrades. The starting capacity (18) and
  maximum (40) are already set.
- Load item, block and tool tables from data files instead of compiled tables.
- Add crafting and recycling rules; item descriptions and `pieces_per_item`
  metadata already anticipate this but no recipes exist.
- Add tool quality tiers, crafting costs and tool-specific efficiency upgrades.
- Add more off-hand utility trinkets beyond the shipped Lantern and Magnet.
  Lantern is a non-combat carried light source; do not add another light toggle
  or unrelated lighting work here.
- Expand the shipped XP/coin/perk progression with farming, exploration rewards
  and long-term goals. No combat progression.
- Expand the bottom-right biome card into a broader status readout with a game
  date, day count and time of day.

## 5. Building work

- Add blueprints and saved structures.
- Add snapping and precise placement controls.
- Add more architectural shapes: walls, corners, roofs, doors, windows and
  decorative pieces.
- Add construction efficiency upgrades without reintroducing a universal
  building currency.
- Keep the current one-voxel placement, physical item cost, hammer shape menu
  and wrench rotation as the foundation for these features.

## 7. Primitive fire system (future, after furnace)

This is roadmap direction only. Do **not** implement it as part of the current
workbench/furnace work. After furnace, decide timing and break it into small
verified increments.

### Primitive torch

- Add a craftable primitive torch as a portable light source.
- Likely early ingredients: wooden stick, plant/bark fiber or cordage, and dry
  grass or another suitable tinder material; resin may be added later if/when
  introduced.
- Torch should eventually be usable both handheld and placeable.
- Initial direction: reusable torches that can be extinguished and relit; no
  immediate consumption/durability system unless explicitly decided later.
- Reuse existing lighting, held-item, attachment/offhand, and placement systems
  where practical.

### Campfire

- Add a small placeable primitive campfire: simple stones/sticks/kindling
  arrangement with visible flame and light when burning.
- Campfire should eventually provide: torch ignition, future warmth, a
  foundation for cooking, and a foundation for later fire-related crafting/progression.
- Keep it compatible with existing placement/collision/selection/save patterns.

### Starting fires

- Add a primitive fire starter (e.g. bow drill from wood + cordage) as a later
  tool/item.
- Intended interaction: interact with an unlit campfire while carrying required
  fire-starting tool/materials, perform a short fire-starting action; no
  complicated minigame.
- Once burning, player should be able to light an unlit torch from the campfire.

### Scope constraints

- Do not add fuel management, temperature simulation, or complicated fire physics
  in the initial system.
- Do not make fire mandatory for basic crafting progression.
- Keep the initial fire system simple and data/system-reuse oriented.

## 6. Open design decisions

These remain undecided and should not be treated as requirements:

- Final biome distribution and border blending beyond the current cradle and
  Voronoi cell layout.
- Terrain-generation graph structure and noise-layer count beyond the current
  evaluator.
- Further cave tuning and geological distribution.
- Final voxel storage and meshing implementations.
- Ore vein representation and interaction.
- Hardware benchmarking and view-distance calculations.
- LOD thresholds and transition techniques.
- Exact chunk streaming and processing budgets.
- Map zoom levels, resolution tiers and hierarchical LOD structure.
- Map discovery radius, exploration visibility and persistence rules.
- 3D-map camera behavior and transitions between map modes.
- Map visual style, labels and points of interest.
- Exact backpack upgrade costs.
- Rules and stacking for additional off-hand trinkets beyond Lantern and
  Magnet.
- Exact tool-quality tiers and crafting costs.
- Building-piece rules beyond cube, half block, slope, stairs and panel.
- Blueprint and snapping workflows.
- Crafting, farming, exploration rewards and long-term progression.
