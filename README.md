# KingdomTales

KingdomTales is a peaceful voxel survival and building game. Explore a procedural world, gather resources, shape terrain, and build one block at a time - with no combat.

## Current status

Early in-development project (v0.0.3). Features and systems are still evolving and may change.

## Technology

- **Rust** (edition 2024)
- **Bevy** 0.20.1

## Requirements

- Rust toolchain compatible with the project (see `Cargo.toml`)
- A GPU/driver capable of running Bevy 3D
- Platform dependencies required by Bevy for your OS (windowing, audio/input, graphics backend)

## Build and run

```bash
# Debug/dev run
cargo run

# Release build
cargo build --release
./target/release/KingdomTales
```

## Controls

| Action | Default |
|---|---|
| Move | `W A S D` |
| Jump | `Space` (hold to keep hopping) |
| Sprint | `Shift` (uses stamina) |
| Crouch | `Ctrl` |
| Hotbar slots | `1-8` or mouse wheel |
| Mine / use tool | `LMB` |
| Gather / pick up drops | `RMB` (hold with hammer to open the shape menu) |
| Place / use placeable item / interact | `RMB` |
| Rotate placeable preview | `R` |
| Backpack / hand crafting | `Tab` |
| Map | `M` (while open: drag pan, scroll zoom, `C`/`Home` recenter, `Tab` toggles Live/Explore, `M`/`Esc` closes) |
| Chat / commands | `Enter` |
| Pause menu | `Esc` |
| Help | `H` |
| Toggle HUD | `F1` |
| Debug overlay | `F3` |
| Save | `F9` |
| Load | `F10` |
| Screenshot | `F12` |

## Development notes

KingdomTales is actively being built in small milestones. Current 0.0.3 work includes sky, clouds and day/night, the Live/Explore map, survival HUD, hand crafting and primitive tools. Expect rough edges, balancing changes, and incomplete systems while development continues.

Before calling a change complete, run the checks in `FEATURES.md`. Those checks include `cargo run --bin shader_check`, which validates WESL/Naga permutations only; it does not verify in-game visuals.

## Runtime assets

The game loads checked-in files from `assets/` at runtime: hand-authored WESL shaders (`assets/shaders/`), UI PNG icons (`assets/ui/icons/`), item and drop GLB models (`assets/models/items/`), and `assets/textures/water_detail.png`. Retained source art includes `assets/textures/items/*.png`. The generated runtime assets and source art are checked in and must stay in the repository. Their original Python generation scripts have been removed; no generation tooling is included.
