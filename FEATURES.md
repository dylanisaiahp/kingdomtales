# Features: current game reference

This file is the source of truth for what KingdomTales does today. It replaces
the old `HANDOFF.md`: completed design decisions, shipped behavior, controls,
engineering conventions and gotchas all live here. Planned or unimplemented work
lives in `ROADMAP.md` and should not be duplicated here.

## 0. Project state

- Version: `0.0.2` in `Cargo.toml`.
- Bevy `0.20.1`, features `3d` + `ui`.
- Rust source: ~40k lines across `src/` (see `wc -l src/*.rs src/**/*.rs`).
- Last verified baseline: `cargo fmt --all --check`,
  `cargo check --all-targets`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets` (**329 passed, 0 failed, 4 ignored**), and
  `cargo build`. No in-game/runtime smoke verification was performed in this
  pass.

## 1. Game identity and settled decisions

KingdomTales is a peaceful, non-combat voxel survival and building game. The
player explores a generated world, gathers physical resources, stores them in a
hotbar and backpack, mines and shapes terrain, and builds one voxel at a time.

These decisions are settled and should not be reintroduced:

1. **No construction units.** No universal building currency, no orbs, no unit
   balance, no unit rewards and no saved balance.
2. **Placement consumes inventory.** Placing a block consumes one matching item
   from the selected pocket, then the backpack. Invalid placement costs nothing.
3. **Hammer = geometry only.** It never places blocks and never changes material.
   Holding RMB opens the shape menu; releasing applies the hovered shape; LMB
   applies the selected shape to an existing block.
4. **Wrench = orientation only.** LMB rotates the targeted block, preserving its
   material and shape. Rotation stays extensible per shape.
5. **Tools are items.** `ItemId` and `ITEM_DEFS` own the tool items; `ToolKind`
   is metadata plus mining rules. `Toolbelt` is gone.
6. **No tool wheel.** There is no `src/ui/radial.rs`, `Overlay::ToolRadial` or
   hold-Q tool selector.
7. **Backpack is storage/UI only.** No follower, legs, movement, weight,
   sinking or equipment attachments. The Magnet and Lantern are real off-hand
   trinkets, not backpack attachments. Keep slots, categories, details and
   capacity/category upgrades.
8. **Hotbar has eight slots.** One stack per slot; stack limits come from item
   metadata.
9. **Drops are physical.** No spin, no bob, gravity applies until supported,
   support is rechecked while resting, small velocities are clamped, and drops
   below the world despawn. Stick drops use a stick mesh.
10. **Felling is batched but leaves are individual.** Wood drops are immediate
    batched items. Each leaf becomes its own full 0.5 m falling detail block,
    with a deterministic ~5% chance to yield one stick on decay.
11. **UI source stays ASCII-only.** Docs and tests stay in sync with behavior.
12. **Unrelated systems keep working:** mining, natural surface layers, geology,
    settings, off-hand trinkets and voxel persistence.
13. **The hammer shape menu is vertical.** It is a compact presentation panel,
    not a radial tool wheel; tool selection still happens through hotbar slots.
14. **No health or injury mechanics.** Survival is stamina, hunger and hydration
    only. Empty hunger/hydration bars do not damage the player or gate gameplay.
15. **Basic peaceful progression is live.** XP and coins come from successful
    item pickups. Every level grants +1 maximum stamina, hunger and hydration
    plus one perk point. Perk points buy larger scaling bonuses in level bands.
16. **Economy and notification values are data-driven.** Pickup XP/coins live in
    `PICKUP_REWARDS`; transient HUD rows come from `NotificationFeed`.
17. **The map has live 3D and Explore 2D modes.** `M` toggles the map overlay,
    which renders a north-up 3D live view or a 2D Explore view with pan/zoom,
    recenter, player marker, fog-of-war visibility, and persisted explored cells.
    Exploration accumulates while the map is closed; Explore rendering/snapshot
    work is gated to the map being open.
18. **LODs are deliberately last.** Occlusion culling remains unimplemented;
    performance work so far is exposed-face culling, frustum culling, stream
    priority and foliage shadow handling.
19. **The compass is a sliding cardinal strip.** Only `N`, `E`, `S` and `W`
    appear; labels move with the player's heading under a fixed centre mark.
    There are no `<` / `>` caps and no numeric bearing.
20. **The Tab overlay is a bottom dock.** The character sheet sits directly left
    of the centre stack, the backpack above the hotbar in the centre, and a
    click-to-inspect details panel directly right. All three panels use the same
    height and text sizes, and backpack/hotbar slots share the same 58 px style
    with blank empty slots. The whole dock sits 24 px above the screen bottom.
21. **Inventory stacks are drag-and-drop.** While Tab is open, dragging between
    any hotbar, general storage or unlocked category slot moves the stack and
    swaps it with the destination stack when one is present. Dragging never
    changes the clicked details selection.
22. **Survival stays visible with Tab open.** Hunger, Insight and thirst keep
    three equal-width slots across the hotbar with 8 px insets from its edges
    and 8 px gaps between them. Hunger and thirst are 20 px tall, down from the
    earlier 22 px. The Insight slot still flexes like the other two, but its
    34 px panel is absolutely positioned so it can grow out of the middle slot,
    overlap the hotbar's top edge by 8 px and square its bottom corners. This
    makes it read as a tab attached to the hotbar without shifting the bars.
    With Tab open, the row overlays the storage panel, which stays flush with
    the hotbar and squares the hotbar's top corners; the dock uses one 8 px
    corner radius and joined corners are square. A new backpack starts with 18
    general slots.
23. **Stamina is a thin bar.** A 6 px capsule is centered 160 px wide, 32 px
    below the crosshair, with its current/max value centered just above it in
    10 px text. This keeps the bar slim without making the number unreadable.
    The fill follows the same fraction and low-value colour rules as the other
    survival bars. The whole bar fades after a delay when full; it is not
    segmented. XP, hunger, thirst and character bars share one compact capsule
    with the value on the left and the name on the right, and all their text is
    centered vertically.

## 2. World and voxels

### Resolution and storage

- The game uses two aligned voxel grids:
  - **World voxels:** 1 m per side. Terrain, grass, water, bedrock and geology
    live here; terrain renders as true 1 m blocks.
  - **Detail voxels:** 0.5 m per side. Collision, construction, trees, bushes
    and natural clutter live here.
  - One world voxel contains 2 x 2 x 2 = 8 detail voxels.
- A detail chunk is a 32 x 32 x 32 voxel cube (16 m per side). A world-layer
  chunk is a 16 x 16 x 16 world-voxel cube covering the same 16 m cube.
- Chunks are keyed by `IVec3`. One block of padding is stored on every axis of
  both grids, so padded storage is 34 x 34 x 34 detail voxels and 18 x 18 x 18
  world voxels. Padding lets the mesher cull faces and compute ambient occlusion
  across chunk borders; edits are copied into neighbour padding too.
- `ChunkData` stores both layers, including enclosed voxels. Generation and
  meshing are separate from storage; per-chunk meshes are generated only for
  exposed faces and rendered independently.
- Edits are logged per detail cell, world cell, turf flag and snow flag so they
  survive chunk streaming and save/load.

### World layout

- Sea level is world Y = 0 and detail Y = 0. Dry plains topsoil at Y >= 0 carries
  continuous turf; forest humus carries turf in patches; desert sand and mesa red
  sandstone stay bare; alpine granite carries patchy snow as a raised surface
  layer, not a buried cube. Submerged basins keep bare soil under shallow water
  and a narrow coastal strip is sand. Deeper negative Y is ordinary geology with
  large deterministic cave networks.
- The generated world spans world Y = -4000..+4000, giving 4 km below and 4 km
  above sea level. Below the bottom is bedrock; above the top is air.
- The nominal land footprint is a square about 4 km x 4 km in X/Z, from -2000 m
  to +2000 m on each axis. It does not end in a wall: an 8 m beach drops into
  open ocean. The ocean has its own rolling underwater terrain around Y = -18,
  clamped above the absolute floor at Y = -64, with water filling up to Y = 0.
- Terrain rules are declarative data in `src/world/generation_data.rs` and are
  evaluated through a small deterministic graph interpreter supporting noise,
  add, subtract, multiply, min, max, clamp, smoothstep and lerp. There are five
  biomes: Plains (low topsoil with continuous turf and a little inland water),
  Forest (humus with patchy turf, dense tall trees and many bushes), Desert
  (bare sand with no vegetation), Mesa (elevated red sandstone and sedimentary
  layers) and Alpine (high exposed granite with patchy snow slabs on dry,
  exposed, inland columns). Snow flags are only set on granite columns above sea
  level, never on soil or submerged terrain.
  Biomes are chosen by jittered Voronoi cells instead of latitude-like climate
  bands. Each 64 m cell centre gets one biome from a stable weighted hash, so
  adjacent cells may resolve to the same biome. The nearest centre owns every
  discrete rule for a column, so no chunk stores or selects multiple biomes;
  nearby centres soften density, clutter and height values near a patch border,
  so a Mesa next to Plains ramps over several metres instead of forming a hard
  wall. The nearest centre still owns every discrete rule. The spawn area
  remains plains inside a 32 m cradle, fading into cell terrain by 96 m.
- Biome tables control height range, surface material, turf/snow coverage,
  ordered geology strata, tree density/height, bush density and loose-stick/pebble
  rates. Sand is still forced on the short coastal strip and seabed. Ore
  deposits are not implemented yet. `Topsoil` remains a legacy save/build
  material; generated terrain uses the specific real-world soil and rock names.
- Terrain generation is global-coordinate based and seed-driven, so chunk Y
  boundaries do not reset surface height or material rules.
- Underground stone is arranged in ordered, biome-specific strata rather than
  random material rolls. Below a three-voxel surface cap, each biome has a fixed
  top-to-bottom sequence of limestone, shale, sandstone, slate and granite (or
  red sandstone in mesa); wide low-amplitude noise makes boundaries undulate by
  a couple of metres. Geology follows the dominant biome cell, so changing
  biomes changes the layers you see underground.
- Caves are carved as `Block::Air` from the authoritative 1 m world layer, then
  expanded into the detail layer like any other terrain. Two independently warped
  2D noise fields form winding worm tunnels roughly 6 m across on stacked,
  drifting layers. Each voxel uses only its nearest layer, so the tunnels stay
  thin instead of merging into cavern slabs. Individually placed spherical rooms
  (8-18 m across) open up junctions on those worms, and rare narrow capsules join
  neighbouring layers into connected cave systems. Fine 3D noise roughens the
  walls. Caves fade out around 320 m below the surface, so networks are large but
  do not hollow out the whole crust. Caves keep the surrounding biome geology and
  need no new save records or special meshing: exposed rock faces cull and shade
  normally, and player edits still overlay generated blocks in the existing v2
  logs.
- Surface entrances are short, narrow tunnels rather than vertical pits. A coarse
  2D mask selects rare sites on dry inland terrain; each site is accepted only if
  a first-layer worm lies within 16 m. The tunnel then slopes sideways into that
  worm over about 24 m, with a 5 m mouth and no deep vertical continuation. The
  spawn area gets a stronger entrance and worm bias so a new player finds a real
  cave nearby. Coasts, seabed and bedrock stay untouched. Entrances remove the
  surface voxel and suppress flora, turf, snow and loose clutter.

### Materials and shapes

- Materials are specific rather than generic. Implemented `Block` variants are:
  `Air`, `Turf`, `Topsoil`, `Granite`, `Sand`, `Snow`, `Water`, `Wood`, `Leaves`,
  `Bedrock`, `Slate`, `Shale`, `Sandstone`, `Limestone`, `Humus` and
  `RedSandstone`. The last two are appended after `Limestone` because the packed
  voxel has only four material bits; all original save indices are unchanged.
- Buildable materials exclude `Air`, `Water` and `Bedrock`.
- Materials are separate from architectural shapes. Buildable shapes are
  `Cube`, `HalfBlock`, `Slope`, `Stairs` and `Panel`. Generated shapes are
  `Stick`, `Pebble`, `Foliage`, `Trunk` and `Stem`; they are not offered by the
  hammer but reuse the same voxel storage, meshing, raycast, mining and save
  pipeline. `Trunk` is solid; the others are non-blocking natural detail.
- A packed `Voxel` stores material, shape and one of 24 proper 3D rotations in
  two bytes. Rotation supports cardinal facing, inversion and sideways mounting.
  The shape field uses four bits so appended shapes 8 and 9 survive round trips;
  saved indices for the original shapes are unchanged.
- `PANEL_HEIGHT = 0.2` detail units, i.e. a 0.1 m mat-like panel.

### Natural surface layers and detail

- Natural turf and snow use the same surface-layer model. Each is a flag on a
  1 m world voxel, rendered as one raised 1 m x 0.1 m x 1 m slab above the voxel
  top, with its own colour and flag storage. Each is a single slab, not four
  detail panels, and the hidden top face and side lips are intentional. Plains
  `Topsoil` is always turfed, forest `Humus` is turfed on deterministic patches,
  and exposed alpine `Granite` carries patchy snow. The granite voxel stays
  intact underneath snow; natural snow is never a full cube buried in stone.
  Player-placed `ItemId::Snow` blocks remain separate full cubes.
- Mining the top face of a natural surface harvests only that flag and drops one
  matching turf or snow item; mining the voxel from another side removes the
  terrain and any surface flag on it. A top-face hit with both flags harvests
  snow first.
- Raised natural surfaces suppress the underlying world top face, emit a thin
  side band, and lift loose detail so sticks, pebbles and drops rest on the
  visible layer instead of sinking into it. Buried surface flags are not drawn.
- Natural sticks, pebbles, foliage, trunks and stems are ordinary detail voxels.
  Sticks, pebbles and foliage are decorative; trunks and stems are solid wood
  that blocks movement. All of them are raycastable and mineable.
- Loose sticks and pebbles are scattered on suitable dry surfaces, at most one
  object per square metre, never floating and never inside bushes or trees. One
  voxel stores the whole object in the lower-left detail cell, while its
  geometry and selection outline are centred in the 1 m block. Pebbles are
  larger, flattened stone shapes; sticks remain slim. Mining or gathering that
  one voxel removes one object and yields exactly one item.
- Loose stones use the biome's characteristic rock: limestone in plains, slate
  in forests, sandstone in deserts and mesa, and granite in alpine. They drop
  matching `Limestone Pebble`, `Slate Pebble`, `Sandstone Pebble` or `Granite
  Pebble` items. The old `rubble` names still work in `/give` commands.
- Generated trees grow 6 to 13 m tall and use one full 0.5 x 0.5 x 0.5 m
  `Trunk` detail cube per level. Each trunk level is stored in the 1 m world
  voxel's lower-left detail cell but its geometry, collision, selection and
  mining align to the centre of that 1 m block. Two short roots lie flat on the
  ground and grow out from the centred trunk. Tree centres keep a deterministic
  two-metre gap so crowded columns cannot grow adjacent trees. The three
  silhouettes keep their varied heights and canopies, but limbs are horizontal
  `Stem` cells instead of upright sticks or flat panels. Flat stems are
  bottom-aligned so roots and branches rest on the ground and block movement;
  a two-cell vertical `Stem` is 0.25 m on X/Z and 1 m tall. Each foliage cell is
  a full 0.5 m box in the separate non-shadow-casting foliage mesh; limb tufts
  attach at limb/tip stem cells, and interior faces between adjacent `Shape::Foliage`
  voxels are culled so canopies merge into continuous foliage. Foliage chunk
  meshes render with an extended foliage material that applies time-driven,
  base-anchored per-vertex wind sway in a foliage-only vertex shader.
- Bushes use a two-cell vertical `Stem` as a single detail-cell column,
  surrounded by a dense foliage clump with a few deterministic gaps. The top
  layer drops its corners, giving a rounded outline instead of a cube lid, and
  the stem sits inside the leaves rather than on top of them.

### Streaming and rendering

- The world generator is pure and seed-driven. The default seed is
  `0x4B54_414C`; `KT_SEED` overrides it.
- Chunk loading streams a normal cylinder: horizontal radius is the view
  distance and vertical radius is capped at three chunks. When the player is
  underground, falling, or looking down, a narrow tail adds 17 layers from
  `dy = -4` through `dy = -20` (about 320 m) with a horizontal radius of three
  chunks. That tail gives deep caves real geometry without multiplying the full
  horizontal cylinder. It is enabled underground or while falling, and the
  look-down test has hysteresis so it does not toggle at the horizon. The tail
  follows the player because the stream volume is rebuilt around the current
  chunk. Its depth, radius and activation speeds are compiled constants near the
  top of `src/world/mod.rs`.
- An unload margin surrounds whichever volume is active. Generation and meshing
  run in Bevy async compute tasks. The bulk candidate list is ranked by squared
  distance from the player chunk center (deterministic tie-breakers on chunk
  coordinates), cached by chunk center, radii, and tail state, and rebuilt when
  those inputs change. It is intentionally not camera-facing biased; the
  separate urgent list handles immediate/predictive needs. A small 3 x 3 x 3
  patch is seeded at startup.
- Newly spawned chunks get a short true-alpha fade-in (per-chunk fade materials
  during fade, then restored to shared materials). Remeshes do not refade.
- A separate short urgent list is recomputed from live player position and
  velocity. Its tiers are immediate 3 x 3 x 3 surroundings, sampled points along
  the predicted velocity path over the next 0.75 seconds, and a short camera
  look-ahead. Tiers are ordered first, then time-to-arrival, so speed extends the
  prediction while distant chunks cannot outrank nearby ones. Four of the twelve
  pending slots are reserved for urgent work. If urgent work arrives while the
  queue is full, the furthest normal tasks are preempted and suppressed briefly
  so priorities cannot cancel and restart the same task every frame. Polling is
  explicit (urgent first, then nearest normal) instead of relying on `HashMap`
  iteration order.
- Distance culling uses cubic chunk extents and a cull distance that accounts
  for the active volume, including the deep tail. Surface fog remains anchored to
  the normal band, so changing pitch while looking into a cave does not move it;
  chunks are fully fogged before the cull distance. Bevy frustum-culls each chunk
  mesh.
- The first-person camera carries a procedurally generated cubemap sky with six
  256x256 faces, a blue horizon, deeper zenith, dark ground half and a soft round
  sun disc. It is generated in memory, so there are no external sky assets, and
  its brightness compensates for Bevy's HDR skybox exposure. The skybox is not
  fogged, which keeps cave openings visibly bright while underground fog stays
  dark inside the cave.
- Underground state combines the camera's depth below the local 1 m surface with
  upward terrain raycasts. A visible opening to the sky keeps the daylight
  presets, so jumping into a cave mouth does not snap to dark. Fully enclosed
  space fades over about a second into dim ambient light and a 60 m fog end;
  returning to an opening restores daylight. No extra collision geometry or
  material flag is needed.
- Water renders in its own translucent mesh. Upward water faces are recessed by
  0.1 m so the surface sits just below the voxel's top edge; side faces stay full
  height to keep the shoreline closed.
- Natural foliage is meshed into a separate child entity marked `NotShadowCaster`.
  It uses an opaque PBR material extended with a time-driven vertex wind-sway
  shader so leaves sway from their base while remaining visible in the main pass
  without adding shadow-pass cost.
- View distance is adjustable in settings; the UI shows an approximate RAM hint
  for the padded cubic grids. The estimate uses the worst-case volume, including
  the downward cave tail, rather than assuming the normal band is always active.
  At the default view distance of 8, the normal band is 1,379 chunks (~119 MB);
  the tail adds 493 chunks (~42 MB), for a worst case of 1,872 chunks (~161 MB).
- Current release microbaselines from ignored tests: generation
  (`world::generation::tests::bench_generate_chunk`) produces 1,211 non-air
  world voxels in about 459 us per chunk; meshing
  (`world::mesher::tests::bench_generated_chunk`) produces 1,020 opaque plus
  11,976 foliage triangles in about 2.06 ms per chunk. The mesh breakdown test
  confirms that terrain-only geometry is about 648 triangles and 1.2 ms, with
  nearly all remaining geometry coming from foliage. Foliage is a separate
  `NotShadowCaster` mesh, so it does not add shadow-pass cost. In a local
  windowed smoke run at view distance 24, this raised steady frame rate from
  roughly 100 FPS to roughly 320 FPS at about 12.5k chunks and 13.7M rendered
  triangles. These are Step 2 measurements, not final budgets.
- The F3 overlay shows rolling wall-clock timings for the world poll, stream and
  cull systems alongside FPS, loaded/pending chunks and triangle count. These
  are CPU-side timings on the system thread, not GPU timings.
- When `perf_log` is enabled in settings, the game appends a perf snapshot once
  per second to `/tmp/opencode/kt-perf.log` with the same FPS, view distance,
  chunk, triangle and world timing values, plus a Unix timestamp so separate runs
  can be told apart. This is an append-only diagnostic file and is not a save
  file; the toggle is off by default.
- Shadow cost scales with the shadow-quality setting rather than the full view
  distance. Low uses one cascade, 70 m, 1024x1024 maps and cheap 2x2 filtering;
  Medium uses two cascades, 110 m, 1024x1024 maps and Gaussian filtering; High
  restores three cascades, 140 m, 2048x2048 maps and Gaussian filtering. The sun
  keeps its fixed direction and colour in every preset. The player camera uses
  2x MSAA instead of 4x to reduce fill cost on dense voxel silhouettes.

## 3. Map system

Press `M` to toggle the map overlay (`Overlay::Map`). The map has two modes:

- **Live 3D:** north-up overhead view rendered by a dedicated `MapCamera` into an
  offscreen texture shown in the map UI. It uses loaded world/chunk rendering and
  focuses on the player's current world position/height when opened or recentered.
- **Explore 2D:** CPU-composited north-up top-down image of explored terrain,
  with pan, zoom, recenter (`C`/`Home`), player marker/facing, and mode tabs.

Fog-of-war in Explore is binary: unexplored cells are transparent/hidden;
explored cells show terrain. Exploration is tracked in 4 m grid cells, updated
from player position (discover/visible radii), accumulated while the map is
closed, and persisted in save v2 `map_explored x z` records. Save validation
rejects invalid map exploration data before mutating live state.

Explore snapshots use budgeted chunk builds, terrain/flora cache entries keyed
by chunk, and dirty invalidation hints from world edits. Snapshot/render/build
work is gated to the map being open.

## 4. Player and world interaction

- Gameplay is first-person and non-combat. WASD moves, Space jumps (holding
  continues hopping), Shift sprints with stamina, and Ctrl crouches. In shallow
  water a buffered jump from the seabed works; deep water still swims upward
  while the head is submerged.
- Targeting and collision use each voxel's actual shape. The camera can see over
  thin player-placed panels, and half blocks, stairs and slopes collide as their
  visible geometry.
- A white outline marks the targeted voxel. It is slightly proud of the block,
  matches the full shape and orientation, and uses thin edge bars rather than
  face diagonals.
- Holding LMB with a mining tool shows a mining preview. Releasing early
  restores the original material, shape and orientation; breaking completes the
  action and spawns drops.
- Blocks require the correct tool: an axe for wood/leaves, a shovel for
  soil/sand/snow, a pickaxe for stone including red sandstone, and a hoe as an
  alternative for turf, topsoil and humus. An empty hand cannot harvest
  blocks, and changing tools cancels the swing. Bedrock and water cannot be
  mined. No tools are granted automatically;
  use `/give` to obtain them.
- Mining a 1 m world voxel clears its eight detail cells and removes any natural
  turf or snow flag, rewarding one physical resource item. Mining a 0.5 m detail
  voxel rewards one matching item.
- RMB instantly gathers loose pebbles and ground sticks without a mining preview
  and picks up drops continuously while held. These natural pickups stay
  hand-gatherable; they are not block harvesting. The hammer reserves RMB for
  its shape menu.

## 5. Items, inventory and storage

- All carryable items are defined in one compiled table, `ItemId` / `ITEM_DEFS`.
  A test keeps the enum and table aligned. `Lantern` and `Magnet` are real
  single-stack items in the `Miscellaneous` category, spawnable with `/give`.
- Categories are `Food`, `Materials`, `Ores`, `Miscellaneous` and `Tools`.
  Weight is metadata only; there is no physical weight simulation.
  `/unlock tools` is a valid command.
- The hotbar (`Pockets`) has eight slots, one stack per slot. Pickups merge into
  an existing stack of the same item anywhere in the hotbar before claiming an
  empty slot, then spill from the selected slot outward up to the metadata stack
  limit. Number keys 1-8 select a slot and the mouse wheel cycles slots.
- Pickups route into backpack storage first and only fill hotbar pockets when
  the backpack is full. A successful pickup awards its data-driven XP and coin
  reward and pushes a combining notification into the HUD feed.
- Pressing Tab dims the screen and reveals three panels in the bottom dock. The
  left panel sits immediately beside the centre stack and shows
  level/XP/coins/perk points, `+` buttons and the equipped off-hand trinket. The
  centre stack has 18 general slots at start, unlocked category sections, and
  a hotbar; three equal, stretched survival readouts for hunger, the reserved
  Insight slot and thirst overlay the lower part of the storage panel. The right
  panel appears only after clicking an occupied
  hotbar, storage or category slot, and shows its name, category, description,
  weight and stack limit. All three panels share the same height and typography;
  opening Tab does not hide hunger or thirst. The storage panel stays flush with
  the hotbar, with square joining corners; all other corners use the shared
  8 px radius.
- Hotbar and backpack slots use the same 58 px tile and item presentation. The
  off-hand slot stays reserved and blank until a trinket is equipped, and it
  shows only the generated item icon and count with no name or description text.
  Empty slots contain only the tile; occupied slots show a distinct unframed
  procedural icon centered in the tile and a count when the stack has more than
  one item. Pebble icons use the pebble's real item color. Dragging from one slot to
  another swaps stacks while the Tab overlay is open, and the click emitted at
  the end of a drag is ignored by the details panel.
- Backpack capacity upgrades add general slots up to a maximum. Category
  upgrades unlock colored sections; items enter matching sections automatically
  and existing items are reorganized when a section unlocks.
- Chat commands include `/help`, `/give <item> [count]`, `/clear`,
  `/upgrade capacity`, `/unlock <category>`, `/fly`, `/noclip`, `/tp`,
  `/spawn`, `/seed` and `/pos`.
- `/tp` and `/spawn` synchronously load the destination's local chunk patch and
  clear all motion state before the next movement tick. Chunk streaming takes
  its center from the player's `Transform` rather than the camera's stale
  `GlobalTransform`, so the new patch is not unloaded before collision sees it.
  This prevents unloaded chunks from being treated as solid and pushing the
  player upward.

## 6. Progression, survival and HUD

- `Progression` is the authoritative resource for level, XP, coins, perk points,
  perk bonuses and hunger/hydration. The HUD only reads it; it does not keep
  display-only copies of those values.
- XP required for the next level is `100 + 160 * (level - 1)`. A single award can
  complete several levels.
- Every level automatically grants +1 maximum stamina, hunger and hydration plus
  one unspent perk point. The new hunger/hydration capacity is filled. Stamina
  sync adds only the newly gained slice and never refills stamina already spent.
- Base maxima are 100 for all three attributes. One perk point adds +2 at levels
  1-12, +4 at 13-24, +6 at 25-36, +8 at 37-48, +10 at 49-60 and +12 at 61+.
  Hunger and hydration perk bonuses also fill the amount they add.
- Hunger drains 0.18 per second and hydration 0.26 per second. Sprinting
  multiplies both by 1.7. Drain pauses while any overlay is open. Empty bars do
  not damage the player or block any action; there is no health or injury system.
- Pickup XP and coins come from the `PICKUP_REWARDS` table in `src/rewards.rs`.
  Item values are per collected unit, and tools are intentionally worth nothing.
- `NotificationFeed` owns transient pickup, save/load and perk-spend rows. Rapid
  pickups of the same item combine into one row, the feed keeps at most five
  rows, and each row lives 4.2 seconds with alpha/slide entrance and exit.
- `InsightInfo` supplies a simplified contextual tab in the permanently reserved
  slot between hunger and thirst. The tab is 34 px tall, joins the hotbar's top
  edge with square bottom corners, and stacks two centered lines: the target
  name in 12 px text and a smaller 9 px muted grey action hint, for example
  `Stone`, `Slate Pebble` or `Wood` over `Pickaxe`, `RMB pickup`, `RMB gather`,
  `Need Axe` or `LMB mine`. Separate fixed-height rows stop the text from
  overlapping. It only appears while gameplay has the cursor captured and no
  overlay is open.
- `BiomeInfo` is sampled from the world generator at the player's position in
  metres and drives a compact bottom-right card showing `Biome: <name>` for the
  five current cell biomes: Plains, Forest, Desert, Mesa and Alpine.

### Placeable structures (verified in-game by user)

- A save-compatible placeable-structure foundation exists for anchored
  multi-cell structures. Structure save records are optional in v2 saves;
  old saves without structure records remain loadable, and invalid structure
  records are rejected before mutating live world/player state.
- **Workbench**: placeable primitive wooden table, approximately 2m wide x 1m
  deep, with four legs, tabletop, supporting braces, and visible open space
  underneath (not a solid block). It occupies a 2-cell anchored footprint;
  both cells behave as one structure for placement, collision, selection,
  interaction, dismantling, and persistence.
- Placement uses the placeable preview/validity flow and aligns to the 1m grid
  with center-origin/footprint parity handling. RMB interacts with the placed
  workbench; LMB uses the selected tool.
- Dismantling requires an axe (existing axe tool-kind/category), uses timed
  mining animation, and shrinks/refunds/removes the structure as one piece.
  Ordinary hand/unrelated tools do not dismantle it.
- Interacting with the workbench opens a dedicated workbench UI shell above or
  alongside the backpack while the backpack remains accessible. The workbench
  UI shows `WORKBENCH`, category tabs (`Components`, `Equipment`,
  `Construction`, `Stations`), and a proper empty state when no workbench
  recipes are available. No placeholder workbench recipes were added; existing
  recipes remain hand-crafted for now.

The HUD layout is fixed and shared through `src/ui/theme.rs`:

- top left: level and XP bar, with unspent perk points
- top centre: clipped compass strip with static N/E/S/W labels that slide under
  a fixed centre mark as the player turns. Each cardinal glyph is center-aligned
  in its band so the mark lines up with the letter; no caps and no bearing number
- top right: coin count, with the F3 debug panel underneath when enabled
- centre: a small white dot crosshair and a thin capsule stamina bar 32 px below
  it, with its `current/max` value centered just above it; the bar and value
  fade when full
- bottom dock: character immediately left, then a centre stack with the
  backpack, a 20 px hunger/thirst row with a reserved middle Insight slot,
  stretched across the hotbar width with 8 px edge insets and 8 px gaps, and the
  hotbar, then click-to-inspect details immediately right. The whole dock sits
  24 px above the screen bottom. All three slots flex equally, the Insight tab
  stays reserved even when empty, and its square bottom edge overlaps the
  hotbar's top edge while the row remains visible and in place with Tab open.
  The hotbar has a reserved blank off-hand slot and eight pockets that show an
  item only when one is carried
- bottom left: key hints; the chat panel sits just above them
- bottom right: a small current-biome card in the corner, with stacked
  pickup/status notifications immediately above it

All UI text is ASCII-only. Every surface reads from the single `UiTheme`
resource installed by `UiPlugin`: HUD cards, the level/XP panel, compass, coins,
F3 debug, aim reticle, stamina, Insight, pickup prompt, key hints, help, biome
card, notifications, Tab backdrop, character, backpack storage, details, chat,
hammer shape menu, pause menu and every settings tab. The Hytale-inspired
palette uses deep slate/navy translucent panels, bright blue `#4D88FF`-style
accents and restrained warm status colors, with one 8 px corner radius, subtle
panel bevels and shadowed text. XP, hunger, thirst and character bars use the
same compact capsule with the value on the left and the name on the right; all
bar text is vertically centered, and the bars turn amber and then red as they
empty. Storage and hotbar slots are visually identical and show distinct item
art instead of letters; the storage panel docks flush to the hotbar with square
joining corners, and drag/inspection targets are highlighted. The survival row
uses three equal flex readouts instead of fixed-width bars, so it always spans
from one 8 px inset to the other. There is no light orb or on/off status. Item
and status icons are 12x12 pixel patterns generated in `src/ui/icons.rs` as
nearest-filtered RGBA textures and drawn with `ImageNode`; no external art
assets are loaded. The settings screen is a fixed full-window translucent panel,
so changing tabs never resizes the menu.

## 7. Tools, building and off-hand

### Tools

- Tools are ordinary hotbar items with stack limit 1: Pickaxe, Axe, Hammer,
  Shovel, Hoe, Scythe and Wrench.
- The selected stack decides the LMB action. There is no separate tool
  inventory and no tool wheel.
- Mining roles live in `ToolKind` / `TOOL_DEFS`: pickaxe for stone and ore
  (including red sandstone), axe for wood and leaves, shovel for soil, sand and
  snow, hoe for turf, topsoil and humus, scythe for leaves, and hammer/wrench for
  non-mining work. `required_tools()`
  is the inverse table used by Insight hints, with a test keeping it aligned
  with the mining roles.
- This is adventure-style tool use: selecting the wrong tool, or nothing, does
  not break a block. Loose sticks and pebbles are RMB pickups rather than block
  harvesting and remain hand-gatherable.

### Building

- Selecting a block item and LMB-clicking places one full cube against the hit
  face, oriented toward the player.
- Placement consumes one matching item from the selected pocket, then the
  backpack. Occupied cells, cells overlapping the player and unloaded chunks
  reject placement without spending anything.
- The hammer applies an architectural shape to an existing block. Material is
  preserved; orientation is snapped when the new shape has fewer resting faces.
- The hammer shape menu only lists buildable shapes and remembers recent
  choices. Hold RMB to open, release to apply the hovered shape, or click an
  option to apply it immediately.
- The wrench advances the targeted block through the rotation cycle valid for
  that shape, preserving material and shape. Cubes do not rotate.
- There are no volume previews, abstract building currency or construction
  units.

### Off-hand trinkets

- The off-hand is attachment slot 0 at the left end of the hotbar (a real
  `SlotSource::Attachment(0)` slot). It starts empty and is not a weapon or
  universal tool selector; mining and building stay with the selected hotbar
  stack.
- Give a trinket with `/give lantern` or `/give magnet`, open Tab and drag the
  single stack into attachment slots (offhand slot 0 or backpack attachment
  slots 1-3). There is no `/offhand` command. Both trinkets are real
  single-stack `ItemId` entries.
- **Magnet:** while attached (any attachment slot), it pulls nearby ground
  drops horizontally toward the player and auto-picks them up when close enough,
  awarding progression/notifications as normal pickup.
- **Lantern:** a non-combat utility trinket. While in any attachment slot it
  emits a warm `PointLight`. A 3D offhand lantern model is shown only when the
  lantern is in attachment slot 0 (offhand); attachment slots 1-3 provide light
  but no hand model. The lantern core/glow uses unlit/emissive materials so it
  remains visibly bright. There is no separate on/off state; equipping it is the
  switch.
- Neither trinket consumes a hotbar slot, and neither changes mining, building
  or combat rules (the game has no combat).

## 8. Drops and felling

### Item drops

- Drops use item-specific cuboid silhouettes/colors: sticks, leaves, tools, fiber/cordage, pebbles, and lantern/magnet have distinct sizes/colors. They are not multi-part 3D drop models.
- Drops fall under gravity, snap to the surface on support and settle. Settled
  drops do not bob or spin.
- Support is rechecked on a timer. If the supporting voxel is removed, the drop
  un-settles and falls again.
- Support checks include raised natural turf and snow slabs, including the
  fast-fall case where a drop ends a frame inside the voxel below. Small items
  rest on top of grass or snow instead of sinking through it.
- Very small velocities are clamped to zero, and drops below the world floor
  despawn. Pickup radius logic is unchanged.
- While the Magnet trinket is attached, drops within its radius are nudged
  toward the player horizontally. Very close drops are auto-picked up (if space
  allows) and award progression/notifications; otherwise they still settle and
  support-check normally.

### Tree felling

- Breaking a trunk fells the connected tree instead of removing single cubes.
  Bush stem-only plans (no trunk, but connected stem/leaves) are also felled as
  small bushes; isolated placed beams/sticks with no leaves are not felled.
- Planning follows connected wood only, then gathers nearby leaves. Touching
  canopies cannot merge two trees into a forest-sized plan. Limb and bush
  `Stem` cells are batched as stick drops; trunk cells are batched as logs.
- Every wood/log cell becomes a normal physical drop, batched into a small
  number of entities.
- Each leaf falls as its own full 0.5 m detail block, straight down, with an
  individual fall delay and decay lifetime. Falling leaves settle on the raised
  natural surface, then fade and decay independently.
- Each leaf cell has a deterministic ~5% chance to yield either a stick or a
  fiber when it decays (split evenly). The tree voxel cap bounds the number of
  pieces created.
- Releasing a mining swing before the trunk breaks leaves the whole tree intact.

## 9. Persistence and settings

- Saves use line-based ASCII with a `KT_SAVE 2` header.
- The save stores the hotbar, backpack contents, capacity, category unlocks,
  selected slot, progression level/XP/coins/perk points/bonuses, current
  hunger/hydration and every changed detail voxel, world voxel, turf flag and
  snow flag. Positions are global and signed, so edits below Y = 0 survive.
  Edits are reapplied lazily as chunks regenerate.
- Progression and snow records are optional inside a v2 save. An older v2 file
  without them loads as the default level-1 character with no snow edits; v1
  files are still rejected.
- F9 saves; F10 loads. The result is pushed into the notification feed as a
  success or error row.
- `KT_SAVE 1` files are rejected because Y = 0 used to mean the world bottom.
  Legacy `units` and `fragment` records are accepted and ignored. Legacy
  `magnet` / `lantern` attachment records are reconciled into the matching
  single-stack trinket items, while old `light_active` / `light_enabled` flags
  are accepted and ignored. The save header stays `KT_SAVE 2`; the off-hand slot
  itself is runtime state and is not saved.
- Esc opens a vertical pause menu with Resume, Settings and Quit. Choosing
  Settings opens a separate tabbed screen with centered Video, Audio, Mouse,
  Controls and Map tabs. Audio and Map are placeholder UI only; perks are no
  longer shown in settings. Esc on the Settings screen returns to the pause
  menu.
- Video holds field of view (90-120 degrees), view distance (4-128 chunks, with
  a RAM hint), shadow quality, VSync, fullscreen and the off-by-default perf log
  toggle. Mouse holds mouse sensitivity. Controls lists the current bindings.
  Character level/XP/coins/perk points and `+` buttons live in the
  backpack panel, not in settings.
- All settings apply immediately. Shadow quality cycles Off / Low / Medium /
  High; the default Medium uses two sun cascades out to 110 m, while High
  restores the original three cascades out to 140 m. Fullscreen uses borderless
  fullscreen rather than an exclusive video-mode change, because exclusive
  fullscreen is unsupported on Wayland and did not uncap the frame rate in local
  testing. The present mode is reapplied after a mode change so VSync Off is not
  lost when the window surface is recreated. Wayland compositors pace every
  window to the display refresh, so VSync Off cannot uncap the frame rate there;
  the settings menu shows a note on Wayland. A windowed session can exceed the
  refresh rate.

## 10. Controls

Implemented defaults (gameplay keys live in `InputBindings`):

- **WASD:** Move.
- **Space (hold):** Jump. Jumping does not consume stamina. This also jumps out
  of 1 m deep water; deeper water uses the same key to swim upward.
- **Shift:** Sprint, consuming stamina. A full bar lasts about 12.5 seconds;
  recovery starts after a 0.6 second delay. In `/fly` or `/noclip` mode,
  sprinting does not consume stamina.
- **Ctrl:** Crouch. With `/fly` active, Ctrl descends and Space ascends.
- **1-8:** Select a hotbar slot.
- **Mouse wheel:** Cycle hotbar slots.
- **Left click:** Mine with a tool, place a block from inventory, apply the
  hammer shape, or rotate with the wrench.
- **Right click:** Gather loose pebbles and ground sticks, pick up drops
  continuously, or hold with the hammer to open the shape menu.
- **F9:** Save the world and inventories.
- **F10:** Load the save file.
- **F12:** Save a timestamped PNG of the current frame into `screenshots/`.
- **Tab:** Open or close the docked overlay: character immediately left,
  backpack with a stretched hunger/Insight/thirst row at its base and the hotbar
  in the centre, click-to-inspect details immediately right. Drag stacks between
  slots while it is open.
- **M:** Reserved for the future map; it currently has no game effect.
- **Enter:** Open chat; Enter again sends without closing. Esc closes. The chat
  panel is 424 px wide.
- **`/fly`:** Toggle flight; WASD is camera-relative (W follows view pitch),
  Space ascends, Ctrl descends and Shift flies faster. Enabling flight also
  disables collision; disabling it restores collision.
- **`/noclip`:** Toggle voxel collision independently. Neither mode is saved.
- **Esc:** Vertical pause menu with Resume, Settings and Quit; Esc on the
  Settings screen returns to the pause menu.
- **F3:** Debug overlay, including world system timings.
- **H:** Controls help. Opening any modal overlay, including the pause menu,
  closes it.

## 11. Engineering conventions

- Prefer owned data and `Copy` IDs: `ItemId`, `Block`, `Shape`, `Orientation`,
  `Entity` and integer indices. Avoid reference fields and explicit lifetimes.
- Use arenas (`Vec<T>` / `HashMap<Id, T>`) instead of references between systems.
- One system does one action. Use `Commands` and events instead of long chains
  of mutable parameters; use `ParamSet` when queries conflict.
- Put game rules in pure free functions that take plain data and return plain
  data (`plan_felling`, `mining_time`, `step_drop`). Bevy systems read input,
  call rules and write components/resources.
- Prefer static data tables over trait hierarchies, concrete types over
  premature generics, and `Option`/`Result` with `?` over custom error enums.
- Keep module APIs small: UI never owns game state, and game state never
  imports UI.

## 12. Gotchas

- `BLOCK_SIZE = 0.5` detail cell; world voxels are 1 m; 8 detail cells per
  world voxel. `PANEL_HEIGHT = 0.2` detail units, i.e. 0.1 m.
- Chunks are keyed by `IVec3`; Y uses global signed bounds, not a per-chunk
  `0..256` range. `SEA_LEVEL_*` is 0, below-world is bedrock, and above-world is
  air. Do not reintroduce a `y < 0` bedrock shortcut.
- Turf and snow are raised 1 m x 0.1 m x 1 m slabs above a world voxel, not full
  cubes. Do not regress their hidden top faces or side lips. Natural snow only
  flags exposed granite; the rock voxel underneath stays intact. Player-placed
  `ItemId::Snow` is still a buildable full cube.
- `ChunkManager::is_solid` checks detail voxels. `world_terrain_target` maps
  untouched full-cube detail cells back to 1 m world voxels.
- `Pockets::consume_selected` and `Pocket::consume_first` are available in
  normal builds; placement uses them.
- `Orientation::next_for_shape` already defines rotation cycles per shape.
- `Voxel::with_shape` preserves material and orientation; use it for hammer
  changes.
- Save format is line-based ASCII with a `KT_SAVE 2` header. Keep unknown
  records ignorable, but do not silently accept v1 files: its Y values have
  different semantics. Progression records may be absent from old v2 files.
- `Progression` is authoritative for level/XP/coins/perks and survival bars. Do
  not add a second copy in the UI, add health/injury, or make empty bars damage
  or block the player.
- The map binding and `Overlay::Map` are display placeholders only. Do not add a
  map screen, discovery save records or M-key handling as part of unrelated UI
  work.
- `Overlay::ShapeMenu` is a vertical presentation panel, not a radial tool menu.
  There is no toolbelt, no hold-Q switching and no active-tool HUD element.
- Perf logging writes `/tmp/opencode/kt-perf.log` only when the settings toggle
  is on; the F3 overlay itself never writes the file.
- Default seed is `0x4B54_414C`; `KT_SEED` overrides.
- Falling leaves are deliberately one piece per leaf cell. Do not regress them
  into grouped clumps, but keep the tree voxel cap.
- Felling follows wood only; do not traverse leaf-to-leaf connections.
- Drop and leaf support must go through the surface-aware check so small objects
  stay visible and pickable on grass and snow.

## 13. Verification

Run these before calling a change complete:

```sh
cargo fmt --all --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
timeout 8 ./target/debug/KingdomTales
```

The smoke run should create a window without a panic; timeout exit code 124 is
expected.
