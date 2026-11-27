//! Simple ASCII save file for player progress and edited voxels.
//!
//! The project deliberately avoids a serialization dependency. One record per
//! line keeps the format inspectable and makes it easy to test round trips. The
//! save stores both inventories, backpack capacity/category unlocks and every detail/world voxel
//! the player has changed. Unloaded regions are restored lazily as their chunks
//! stream in.

use std::fs;
use std::io::Write;
use std::path::Path;

use bevy::prelude::*;

use crate::backpack::{
    ATTACHMENT_SLOTS, BASE_CAPACITY, Backpack, BackpackSnapshot, CATEGORY_SLOTS_PER_UNLOCK,
    MAX_CAPACITY,
};
use crate::items::{ItemCategory, ItemId, ItemStack};
use crate::notifications::{NotificationFeed, NotificationTone};
use crate::pockets::{POCKET_COUNT, Pockets};
use crate::progression::{Progression, ProgressionSnapshot};
use crate::settings::InputBindings;
use crate::structures::{
    StructureEntities, StructureKind, Structures, despawn_structure_visual, spawn_structure_visual,
};
use crate::ui_manager::UiManager;
use crate::world::ChunkManager;
use crate::world::block::{Block, Facing, Orientation, Shape, Voxel};
use crate::world::chunk::{
    DETAILS_PER_WORLD_AXIS, LAND_HALF_EXTENT, SurfaceLayer, detail_y_in_range, world_y_in_range,
};
use crate::world::remesh_chunk;

pub const SAVE_PATH: &str = "savegame.kt";
/// Supported save format versions. Older supported versions are migrated
/// explicitly to the current in-memory format; unsupported versions are rejected
/// with a clear error. Legacy B11 records inside supported formats remain accepted.
const SAVE_VERSION: u32 = 2;
const HEADER: &str = "KT_SAVE 2";
const HEADER_PREFIX: &str = "KT_SAVE ";
/// Generous X/Z bounds for persisted edits. The nominal land footprint is much
/// smaller, but saves may include edits near blended shores; this rejects only
/// clearly impossible coordinates that could keep pending edits around forever.
const SAVE_MAX_WORLD_XZ: i32 = LAND_HALF_EXTENT + 512;
const SAVE_MAX_DETAIL_XZ: i32 = SAVE_MAX_WORLD_XZ * DETAILS_PER_WORLD_AXIS;
const SAVE_MAX_EDIT_RECORDS: usize = 1_000_000;
/// Reasonable hard limits for manual/malformed save files. They are large enough
/// for legitimate saves, but prevent unbounded parsing/allocation.
const SAVE_MAX_BYTES: usize = 16 * 1024 * 1024;
const SAVE_MAX_LINES: usize = 1_100_000;
const SAVE_MAX_LINE_BYTES: usize = 4096;

/// Everything the save file carries. Tests compare this value directly.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SaveData {
    pub selected: usize,
    pub pockets: Vec<Option<ItemStack>>,
    pub backpack: BackpackSnapshot,
    /// Added to v2 as optional records; older v2 saves keep the level-1 default.
    pub progression: ProgressionSnapshot,
    pub detail_edits: Vec<(IVec3, Voxel)>,
    pub world_edits: Vec<(IVec3, Block)>,
    pub turf_edits: Vec<(IVec3, bool)>,
    pub snow_edits: Vec<(IVec3, bool)>,
    /// Placed multi-cell structures. Optional for older v2 saves.
    pub structures: Vec<(u32, u8, IVec3, u8)>,
    /// Explored map cells (world XZ cell coords). Optional for older v2 saves.
    pub map_explored: Vec<IVec2>,
}

impl SaveData {
    /// Snapshot the live resources and edit logs.
    pub fn capture(
        manager: &ChunkManager,
        pockets: &Pockets,
        backpack: &Backpack,
        progression: &Progression,
        structures: &Structures,
        map_explored: Vec<IVec2>,
    ) -> Self {
        let (selected, pocket_entries) = pockets.snapshot();

        let mut detail_edits: Vec<_> = manager
            .detail_edits
            .iter()
            .map(|(pos, voxel)| (*pos, *voxel))
            .collect();
        detail_edits.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

        let mut world_edits: Vec<_> = manager
            .world_edits
            .iter()
            .map(|(pos, block)| (*pos, *block))
            .collect();
        world_edits.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

        let mut turf_edits: Vec<_> = manager
            .turf_edits
            .iter()
            .map(|(pos, turf)| (*pos, *turf))
            .collect();
        turf_edits.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

        let mut snow_edits: Vec<_> = manager
            .snow_edits
            .iter()
            .map(|(pos, snow)| (*pos, *snow))
            .collect();
        snow_edits.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));

        let mut structure_records: Vec<_> = structures
            .iter()
            .map(|s| (s.id.0, s.kind.index(), s.anchor, facing_index(s.facing)))
            .collect();
        structure_records.sort_by_key(|(id, _, _, _)| *id);

        Self {
            selected,
            pockets: pocket_entries,
            backpack: backpack.snapshot(),
            progression: progression.snapshot(),
            detail_edits,
            world_edits,
            turf_edits,
            snow_edits,
            structures: structure_records,
            map_explored,
        }
    }

    /// Replace live player state from the save. World edits are queued
    /// separately because their chunks may not be loaded yet.
    ///
    /// Validation happens before any live state is mutated so a failed restore
    /// cannot partially change the running game.
    pub fn restore_player(
        &self,
        pockets: &mut Pockets,
        backpack: &mut Backpack,
        progression: &mut Progression,
    ) -> Result<(), String> {
        validate_save_data(self)?;
        Pockets::validate_restore(self.selected, &self.pockets)?;
        Backpack::validate_restore(&self.backpack)?;
        pockets.restore(self.selected, &self.pockets)?;
        backpack.restore(&self.backpack)?;
        *progression = Progression::from_snapshot(self.progression.clone());
        Ok(())
    }

    /// Replace the live edit logs with the edits from this save.
    ///
    /// The logs are what later saves capture and what streaming uses to
    /// re-apply edits when a chunk regenerates, so a load must make them match
    /// the loaded save rather than only writing directly into loaded chunks.
    /// Replacing avoids duplicate accumulation across repeated loads.
    pub fn seed_edit_logs(&self, manager: &mut ChunkManager, structures: &mut Structures) {
        manager.detail_edits.clear();
        manager
            .detail_edits
            .extend(self.detail_edits.iter().copied());
        manager.world_edits.clear();
        manager.world_edits.extend(self.world_edits.iter().copied());
        manager.turf_edits.clear();
        manager.turf_edits.extend(self.turf_edits.iter().copied());
        manager.snow_edits.clear();
        manager.snow_edits.extend(self.snow_edits.iter().copied());
        structures.placed.clear();
        structures.by_cell.clear();
        structures.next_id = 0;
        for (id, kind_idx, anchor, facing_idx) in &self.structures {
            let Some(kind) = StructureKind::from_index(*kind_idx) else {
                continue;
            };
            let facing = facing_from_index(*facing_idx);
            let sid = crate::structures::StructureId(*id);
            let placed = crate::structures::PlacedStructure {
                id: sid,
                kind,
                anchor: *anchor,
                facing,
            };
            for cell in placed.occupied_cells() {
                structures.by_cell.insert(cell, sid);
            }
            structures.placed.insert(sid, placed);
            structures.next_id = structures.next_id.max(id.saturating_add(1));
        }
    }

    /// Serialize to the line-oriented ASCII format.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(HEADER);
        out.push('\n');
        out.push_str(&format!("selected {}\n", self.selected));
        out.push_str(&format!("backpack_capacity {}\n", self.backpack.capacity));
        out.push_str(&format!("level {}\n", self.progression.level));
        out.push_str(&format!("xp {}\n", self.progression.xp));
        out.push_str(&format!("coins {}\n", self.progression.coins));
        out.push_str(&format!("perks {}\n", self.progression.perk_points));
        out.push_str(&format!(
            "stamina_bonus {}\n",
            self.progression.stamina_bonus
        ));
        out.push_str(&format!("hunger_bonus {}\n", self.progression.hunger_bonus));
        out.push_str(&format!(
            "hydration_bonus {}\n",
            self.progression.hydration_bonus
        ));
        out.push_str(&format!("hunger {:.2}\n", self.progression.hunger));
        out.push_str(&format!("hydration {:.2}\n", self.progression.hydration));

        for (slot, stack) in self.pockets.iter().enumerate() {
            if let Some(stack) = stack {
                out.push_str(&format!(
                    "pocket {} {} {}\n",
                    slot,
                    stack.item.index(),
                    stack.count
                ));
            }
        }
        for (slot, stack) in self.backpack.slots.iter().enumerate() {
            if let Some(stack) = stack {
                out.push_str(&format!(
                    "backpack {} {} {}\n",
                    slot,
                    stack.item.index(),
                    stack.count
                ));
            }
        }
        for (category, entries) in &self.backpack.categories {
            for (slot, stack) in entries.iter().enumerate() {
                if let Some(stack) = stack {
                    out.push_str(&format!(
                        "category {} {} {} {}\n",
                        category_index(*category),
                        slot,
                        stack.item.index(),
                        stack.count
                    ));
                }
            }
        }
        for (slot, stack) in self.backpack.attachments.iter().enumerate() {
            if let Some(stack) = stack {
                out.push_str(&format!(
                    "attachment {} {} {}\n",
                    slot,
                    stack.item.index(),
                    stack.count
                ));
            }
        }
        for (pos, voxel) in &self.detail_edits {
            out.push_str(&format!(
                "detail {} {} {} {} {} {}\n",
                pos.x,
                pos.y,
                pos.z,
                voxel.block().index(),
                voxel.shape().index(),
                voxel.orientation().index()
            ));
        }
        for (pos, block) in &self.world_edits {
            out.push_str(&format!(
                "world {} {} {} {}\n",
                pos.x,
                pos.y,
                pos.z,
                block.index()
            ));
        }
        for (layer, edits) in [
            (SurfaceLayer::Turf, &self.turf_edits),
            (SurfaceLayer::Snow, &self.snow_edits),
        ] {
            for (pos, present) in edits {
                out.push_str(&format!(
                    "{} {} {} {} {}\n",
                    layer.record_name(),
                    pos.x,
                    pos.y,
                    pos.z,
                    u8::from(*present)
                ));
            }
        }
        for (id, kind, anchor, facing) in &self.structures {
            out.push_str(&format!(
                "structure {} {} {} {} {} {}\n",
                id, kind, anchor.x, anchor.y, anchor.z, facing
            ));
        }
        for cell in &self.map_explored {
            out.push_str(&format!("map_explored {} {}\n", cell.x, cell.y));
        }
        out
    }

    /// Parse a save produced by [`SaveData::to_text`]. Unknown record names are
    /// rejected so a corrupted or incompatible file cannot load silently.
    pub fn from_text(text: &str) -> Result<Self, String> {
        let version = validate_save_text(text)?;
        let mut data = parse_save_v2(text)?;
        migrate_to_current(version, &mut data)?;
        validate_save_data(&data)?;
        Ok(data)
    }
}

fn validate_save_text(text: &str) -> Result<u32, String> {
    if text.len() > SAVE_MAX_BYTES {
        return Err(format!(
            "save file too large ({} bytes, max {SAVE_MAX_BYTES})",
            text.len()
        ));
    }
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(|| "empty save file".to_string())?;
    let mut line_count = 1usize;
    if header.len() > SAVE_MAX_LINE_BYTES {
        return Err(format!(
            "line 1: too long ({} bytes, max {SAVE_MAX_LINE_BYTES})",
            header.len()
        ));
    }
    for line in lines {
        line_count += 1;
        if line_count > SAVE_MAX_LINES {
            return Err(format!(
                "save file has too many lines (max {SAVE_MAX_LINES})"
            ));
        }
        if line.len() > SAVE_MAX_LINE_BYTES {
            return Err(format!(
                "line {line_count}: too long ({} bytes, max {SAVE_MAX_LINE_BYTES})",
                line.len()
            ));
        }
    }
    parse_save_version(header.trim())
}

fn parse_save_version(header: &str) -> Result<u32, String> {
    let version_text = header
        .strip_prefix(HEADER_PREFIX)
        .ok_or_else(|| format!("unsupported save header: {header}"))?;
    let version: u32 = version_text
        .parse()
        .map_err(|_| format!("unsupported save header: {header}"))?;
    match version {
        2 => Ok(2),
        1 => Err(
            "unsupported save version KT_SAVE 1: Y=0 was redefined as sea level in KT_SAVE 2;              no safe automatic migration is available"
                .to_string(),
        ),
        _ => Err(format!(
            "unsupported save version KT_SAVE {version}; supported versions: KT_SAVE {SAVE_VERSION}"
        )),
    }
}

fn migrate_to_current(version: u32, _data: &mut SaveData) -> Result<(), String> {
    match version {
        SAVE_VERSION => Ok(()),
        _ => Err(format!(
            "unsupported save version KT_SAVE {version}; supported versions: KT_SAVE {SAVE_VERSION}"
        )),
    }
}

fn parse_save_v2(text: &str) -> Result<SaveData, String> {
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(|| "empty save file".to_string())?;
    if header.trim() != HEADER {
        return Err(format!("unsupported save header: {header}"));
    }

    let mut data = SaveData {
        backpack: BackpackSnapshot {
            capacity: BASE_CAPACITY,
            slots: vec![None; BASE_CAPACITY],
            ..default()
        },
        ..default()
    };
    let mut pockets = vec![None; POCKET_COUNT];
    data.backpack.attachments = vec![None; ATTACHMENT_SLOTS];
    // Legacy attachment/trinket records are reconciled after explicit records
    // so a real saved slot always wins over an old derived item.
    let mut legacy_trinkets = Vec::new();

    for (line_number, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let record = parts.next().unwrap_or_default();
        let number = line_number + 2;
        match record {
            "units" => {
                // Old saves stored a universal building balance. It no
                // longer exists, so accept the record and ignore its value.
                let _value: Option<String> = parts.next().map(str::to_string);
                ensure_no_extra(&mut parts, number)?;
            }
            "selected" => {
                data.selected = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "backpack_capacity" => {
                let capacity: usize = parse(&mut parts, number)?;
                if !(BASE_CAPACITY..=MAX_CAPACITY).contains(&capacity) {
                    return Err(format!(
                        "line {number}: invalid backpack capacity {capacity} (expected {BASE_CAPACITY}..={MAX_CAPACITY})"
                    ));
                }
                data.backpack.capacity = capacity;
                data.backpack.slots = vec![None; capacity];
                ensure_no_extra(&mut parts, number)?;
            }
            "level" => {
                data.progression.level = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "xp" => {
                data.progression.xp = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "coins" => {
                data.progression.coins = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "perks" => {
                data.progression.perk_points = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "stamina_bonus" => {
                data.progression.stamina_bonus = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "hunger_bonus" => {
                data.progression.hunger_bonus = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "hydration_bonus" => {
                data.progression.hydration_bonus = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "hunger" => {
                let value: f32 = parse(&mut parts, number)?;
                if !value.is_finite() {
                    return Err(format!("line {number}: invalid hunger `{value}`"));
                }
                data.progression.hunger = value;
                ensure_no_extra(&mut parts, number)?;
            }
            "hydration" => {
                let value: f32 = parse(&mut parts, number)?;
                if !value.is_finite() {
                    return Err(format!("line {number}: invalid hydration `{value}`"));
                }
                data.progression.hydration = value;
                ensure_no_extra(&mut parts, number)?;
            }
            "fragment" => {
                // Legacy saves from before the construction-units-only
                // hammer stored partial material pieces. They no longer
                // affect placement, so consume the record and ignore it.
                let _item = parse_item(&mut parts, number)?;
                let _count: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "pocket" => {
                let slot: usize = parse(&mut parts, number)?;
                let item = parse_item(&mut parts, number)?;
                let count: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if count == 0 {
                    return Err(format!("line {number}: invalid pocket count {count}"));
                }
                if slot < POCKET_COUNT {
                    pockets[slot] = Some(ItemStack { item, count });
                }
            }
            "backpack" => {
                let slot: usize = parse(&mut parts, number)?;
                let item = parse_item(&mut parts, number)?;
                let count: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if count == 0 {
                    return Err(format!("line {number}: invalid backpack count {count}"));
                }
                if slot < data.backpack.slots.len() {
                    data.backpack.slots[slot] = Some(ItemStack { item, count });
                }
            }
            "category" => {
                let category = parse_category(&mut parts, number)?;
                let slot: usize = parse(&mut parts, number)?;
                let item = parse_item(&mut parts, number)?;
                let count: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if count == 0 {
                    return Err(format!("line {number}: invalid category count {count}"));
                }
                if slot >= CATEGORY_SLOTS_PER_UNLOCK {
                    continue;
                }
                let existing = data
                    .backpack
                    .categories
                    .iter()
                    .position(|(existing, _)| *existing == category);
                let index = match existing {
                    Some(index) => index,
                    None => {
                        data.backpack
                            .categories
                            .push((category, vec![None; CATEGORY_SLOTS_PER_UNLOCK]));
                        data.backpack.categories.len() - 1
                    }
                };
                data.backpack.categories[index].1[slot] = Some(ItemStack { item, count });
            }
            "attachment" => {
                let slot: usize = parse(&mut parts, number)?;
                let item = parse_item(&mut parts, number)?;
                let count: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if count == 0 {
                    return Err(format!("line {number}: invalid attachment count {count}"));
                }
                if slot < ATTACHMENT_SLOTS {
                    data.backpack.attachments[slot] = Some(ItemStack { item, count });
                }
            }
            "magnet" | "lantern" => {
                // Old saves stored attachment levels. Magnet and Lantern are
                // now real backpack attachments: a positive level grants one
                // matching item to attachments, and level 0 grants nothing.
                let level: u32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if level > 0 {
                    legacy_trinkets.push(if record == "magnet" {
                        ItemId::Magnet
                    } else {
                        ItemId::Lantern
                    });
                }
            }
            "light_active" | "light_enabled" => {
                // Legacy staff-light flags. Trinkets have no toggleable
                // light, so consume the value and ignore it.
                let _flag: u8 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
            }
            "detail" => {
                ensure_edit_record_budget(&data, number)?;
                let x: i32 = parse(&mut parts, number)?;
                let y: i32 = parse(&mut parts, number)?;
                let z: i32 = parse(&mut parts, number)?;
                if !detail_y_in_range(y) || !detail_xz_in_range(x, z) {
                    return Err(format!("line {number}: detail edit out of range"));
                }
                let block: u8 = parse(&mut parts, number)?;
                let shape: u8 = parse(&mut parts, number)?;
                let orientation: u8 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                let voxel = Voxel::from_oriented_parts(
                    Block::from_index(block),
                    Shape::from_index(shape),
                    Orientation::from_index(orientation),
                );
                data.detail_edits.push((IVec3::new(x, y, z), voxel));
            }
            "world" => {
                ensure_edit_record_budget(&data, number)?;
                let x: i32 = parse(&mut parts, number)?;
                let y: i32 = parse(&mut parts, number)?;
                let z: i32 = parse(&mut parts, number)?;
                if !world_y_in_range(y) || !world_xz_in_range(x, z) {
                    return Err(format!("line {number}: world edit out of range"));
                }
                let block: u8 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                data.world_edits
                    .push((IVec3::new(x, y, z), Block::from_index(block)));
            }
            "turf" | "snow" => {
                ensure_edit_record_budget(&data, number)?;
                let layer = if record == "turf" {
                    SurfaceLayer::Turf
                } else {
                    SurfaceLayer::Snow
                };
                let x: i32 = parse(&mut parts, number)?;
                let y: i32 = parse(&mut parts, number)?;
                let z: i32 = parse(&mut parts, number)?;
                if !world_y_in_range(y) || !world_xz_in_range(x, z) {
                    return Err(format!("line {number}: {record} edit out of range"));
                }
                let flag: u8 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if flag > 1 {
                    return Err(format!("line {number}: invalid {record} flag `{flag}`"));
                }
                let edits = match layer {
                    SurfaceLayer::Turf => &mut data.turf_edits,
                    SurfaceLayer::Snow => &mut data.snow_edits,
                };
                edits.push((IVec3::new(x, y, z), flag != 0));
            }
            "structure" => {
                let id: u32 = parse(&mut parts, number)?;
                let kind: u8 = parse(&mut parts, number)?;
                let x: i32 = parse(&mut parts, number)?;
                let y: i32 = parse(&mut parts, number)?;
                let z: i32 = parse(&mut parts, number)?;
                let facing: u8 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if StructureKind::from_index(kind).is_none() {
                    return Err(format!("line {number}: unknown structure kind {kind}"));
                }
                if facing > 3 {
                    return Err(format!("line {number}: invalid structure facing {facing}"));
                }
                if !detail_y_in_range(y) || !detail_xz_in_range(x, z) {
                    return Err(format!("line {number}: structure out of range"));
                }
                data.structures
                    .push((id, kind, IVec3::new(x, y, z), facing));
            }
            "map_explored" => {
                let x: i32 = parse(&mut parts, number)?;
                let z: i32 = parse(&mut parts, number)?;
                ensure_no_extra(&mut parts, number)?;
                if x.abs() > SAVE_MAX_DETAIL_XZ || z.abs() > SAVE_MAX_DETAIL_XZ {
                    return Err(format!("line {number}: map_explored out of range"));
                }
                data.map_explored.push(IVec2::new(x, z));
            }
            other => return Err(format!("line {number}: unknown record `{other}`")),
        }
    }

    for item in legacy_trinkets {
        grant_legacy_attachment(&mut data.backpack.attachments, &mut pockets, item);
    }
    data.pockets = pockets;
    Ok(data)
}

fn parse<'a, T: std::str::FromStr>(
    parts: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<T, String> {
    let value = parts
        .next()
        .ok_or_else(|| format!("line {line}: missing value"))?;
    value
        .parse()
        .map_err(|_| format!("line {line}: invalid value `{value}`"))
}

fn ensure_no_extra<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<(), String> {
    if parts.next().is_some() {
        Err(format!("line {line}: unexpected extra tokens"))
    } else {
        Ok(())
    }
}

fn parse_item<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<ItemId, String> {
    let index: usize = parse(parts, line)?;
    ItemId::ALL
        .get(index)
        .copied()
        .ok_or_else(|| format!("line {line}: unknown item index {index}"))
}

/// Reconcile a legacy attachment/trinket record into the backpack attachment
/// model. Attachments are single-stack physical items; if all attachment slots
/// are occupied the item falls back to the first free pocket.
fn grant_legacy_attachment(
    attachments: &mut [Option<ItemStack>],
    pockets: &mut [Option<ItemStack>],
    item: ItemId,
) {
    if let Some(slot) = attachments.iter_mut().find(|stack| stack.is_none()) {
        *slot = Some(ItemStack { item, count: 1 });
        return;
    }
    if let Some(slot) = pockets.iter_mut().find(|stack| stack.is_none()) {
        *slot = Some(ItemStack { item, count: 1 });
    }
}

fn parse_category<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<ItemCategory, String> {
    let index: usize = parse(parts, line)?;
    ItemCategory::ALL
        .get(index)
        .copied()
        .ok_or_else(|| format!("line {line}: unknown category index {index}"))
}

fn world_xz_in_range(x: i32, z: i32) -> bool {
    x.abs() <= SAVE_MAX_WORLD_XZ && z.abs() <= SAVE_MAX_WORLD_XZ
}

fn facing_index(facing: Facing) -> u8 {
    match facing {
        Facing::North => 0,
        Facing::East => 1,
        Facing::South => 2,
        Facing::West => 3,
    }
}

fn facing_from_index(index: u8) -> Facing {
    match index {
        1 => Facing::East,
        2 => Facing::South,
        3 => Facing::West,
        _ => Facing::North,
    }
}

fn validate_map_explored(data: &SaveData) -> Result<(), String> {
    if data.map_explored.len() > 500_000 {
        return Err("too many map_explored records".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for cell in &data.map_explored {
        if cell.x.abs() > SAVE_MAX_DETAIL_XZ || cell.y.abs() > SAVE_MAX_DETAIL_XZ {
            return Err("map_explored out of range".to_string());
        }
        if !seen.insert(*cell) {
            return Err(format!("duplicate map_explored cell {:?}", cell));
        }
    }
    Ok(())
}

fn validate_structures(data: &SaveData) -> Result<(), String> {
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_cells = std::collections::HashSet::new();
    for (id, kind_idx, anchor, facing_idx) in &data.structures {
        if !seen_ids.insert(*id) {
            return Err(format!("duplicate structure id {id}"));
        }
        let Some(kind) = StructureKind::from_index(*kind_idx) else {
            return Err(format!("unknown structure kind {kind_idx}"));
        };
        if *facing_idx > 3 {
            return Err(format!("invalid structure facing {facing_idx}"));
        }
        if !detail_y_in_range(anchor.y) || !detail_xz_in_range(anchor.x, anchor.z) {
            return Err("structure anchor out of range".to_string());
        }
        let facing = facing_from_index(*facing_idx);
        for cell in Structures::occupied_cells(kind, *anchor, facing) {
            if !detail_y_in_range(cell.y) || !detail_xz_in_range(cell.x, cell.z) {
                return Err("structure footprint out of range".to_string());
            }
            if !seen_cells.insert(cell) {
                return Err(format!("overlapping structure footprint at {cell:?}"));
            }
        }
    }
    Ok(())
}

fn detail_xz_in_range(x: i32, z: i32) -> bool {
    x.abs() <= SAVE_MAX_DETAIL_XZ && z.abs() <= SAVE_MAX_DETAIL_XZ
}

fn ensure_edit_record_budget(data: &SaveData, line: usize) -> Result<(), String> {
    let total = data
        .detail_edits
        .len()
        .saturating_add(data.world_edits.len())
        .saturating_add(data.turf_edits.len())
        .saturating_add(data.snow_edits.len());
    if total >= SAVE_MAX_EDIT_RECORDS {
        return Err(format!("line {line}: too many edit records"));
    }
    Ok(())
}

fn category_index(category: ItemCategory) -> usize {
    ItemCategory::ALL
        .iter()
        .position(|entry| *entry == category)
        .unwrap_or(0)
}

/// World edits waiting for their chunks to stream in after a load.
#[derive(Resource, Default)]
pub struct PendingEdits {
    pub detail: Vec<(IVec3, Voxel)>,
    pub world: Vec<(IVec3, Block)>,
    pub turf: Vec<(IVec3, bool)>,
    pub snow: Vec<(IVec3, bool)>,
}

impl PendingEdits {
    pub fn queue(&mut self, data: &SaveData) {
        self.detail = data.detail_edits.clone();
        self.world = data.world_edits.clone();
        self.turf = data.turf_edits.clone();
        self.snow = data.snow_edits.clone();
    }

    pub fn is_empty(&self) -> bool {
        self.detail.is_empty()
            && self.world.is_empty()
            && self.turf.is_empty()
            && self.snow.is_empty()
    }
}

/// Apply surface edits whose chunks are loaded, leaving the rest queued.
fn retain_ready_surface_edits(
    manager: &mut ChunkManager,
    dirty: &mut Vec<IVec3>,
    layer: SurfaceLayer,
    edits: &mut Vec<(IVec3, bool)>,
) {
    edits.retain(|(pos, present)| {
        if !world_y_in_range(pos.y) {
            return false;
        }
        if manager.get_world_surface(layer, *pos).is_none() {
            return true;
        }
        dirty.extend(manager.set_world_surface_untracked(layer, *pos, *present));
        false
    });
}

/// Write a save file atomically: write to a sibling temp file, flush/sync,
/// then rename into place so an interrupted save cannot leave a truncated file.
pub fn write_save_atomic(path: impl AsRef<Path>, text: &str) -> Result<(), String> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .ok_or_else(|| "save path has no file name".to_string())?
        .to_string_lossy();
    let tmp_path = path.with_file_name(format!("{file_name}.tmp"));
    let mut file = fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&tmp_path, path).map_err(|e| e.to_string())
}

/// Parse and validate a save text: header/version, per-line structure, record
/// coordinates/counts, and absence of extra tokens. The returned data is fully
/// validated before any live game state is mutated by callers.
pub fn load_save_text(text: &str) -> Result<SaveData, String> {
    SaveData::from_text(text)
}

/// Validate a parsed save against current format and gameplay constraints.
/// This catches malformed/inconsistent data that may be representable in text
/// records but is not a legal runtime state.
pub fn validate_save_data(data: &SaveData) -> Result<(), String> {
    validate_inventory(data)?;
    validate_structures(data)?;
    validate_map_explored(data)?;
    Ok(())
}

fn validate_stack(stack: &ItemStack, context: &str) -> Result<(), String> {
    let max_stack = stack.item.def().max_stack.max(1);
    if stack.count == 0 {
        return Err(format!("{context}: item count must be >= 1"));
    }
    if stack.count > max_stack {
        return Err(format!(
            "{context}: {} count {} exceeds max stack {max_stack}",
            stack.item.display_name(),
            stack.count
        ));
    }
    Ok(())
}

fn validate_inventory(data: &SaveData) -> Result<(), String> {
    if data.selected >= POCKET_COUNT {
        return Err(format!(
            "selected pocket {} out of range (0..{POCKET_COUNT})",
            data.selected
        ));
    }
    if data.pockets.len() != POCKET_COUNT {
        return Err(format!(
            "invalid pocket slot count: {} (expected {POCKET_COUNT})",
            data.pockets.len()
        ));
    }
    for (index, stack) in data.pockets.iter().enumerate() {
        if let Some(stack) = stack {
            validate_stack(stack, &format!("pocket {index}"))?;
        }
    }

    let capacity = data.backpack.capacity;
    if !(BASE_CAPACITY..=MAX_CAPACITY).contains(&capacity) {
        return Err(format!(
            "invalid backpack capacity {capacity} (expected {BASE_CAPACITY}..={MAX_CAPACITY})"
        ));
    }
    if data.backpack.slots.len() != capacity {
        return Err(format!(
            "backpack slots {} do not match declared capacity {capacity}",
            data.backpack.slots.len()
        ));
    }
    for (index, stack) in data.backpack.slots.iter().enumerate() {
        if let Some(stack) = stack {
            validate_stack(stack, &format!("backpack slot {index}"))?;
        }
    }

    let mut seen_categories = std::collections::HashSet::new();
    for (category, entries) in &data.backpack.categories {
        if !seen_categories.insert(*category) {
            return Err(format!(
                "duplicate backpack category section: {}",
                category.display_name()
            ));
        }
        if entries.len() != CATEGORY_SLOTS_PER_UNLOCK {
            return Err(format!(
                "category {} has {} slots (expected {CATEGORY_SLOTS_PER_UNLOCK})",
                category.display_name(),
                entries.len()
            ));
        }
        for (index, stack) in entries.iter().enumerate() {
            if let Some(stack) = stack {
                validate_stack(
                    stack,
                    &format!("category {} slot {index}", category.display_name()),
                )?;
                if stack.item.def().category != *category {
                    return Err(format!(
                        "category {} slot {index} contains {}, which belongs to {}",
                        category.display_name(),
                        stack.item.display_name(),
                        stack.item.def().category.display_name()
                    ));
                }
            }
        }
    }

    if data.backpack.attachments.len() != ATTACHMENT_SLOTS {
        return Err(format!(
            "invalid attachment slot count: {} (expected {ATTACHMENT_SLOTS})",
            data.backpack.attachments.len()
        ));
    }
    for (index, stack) in data.backpack.attachments.iter().enumerate() {
        if let Some(stack) = stack {
            validate_stack(stack, &format!("attachment slot {index}"))?;
        }
    }

    Ok(())
}

/// Latest save/load status. The message is mirrored into the notification feed;
/// this resource mainly carries change detection between save/load operations.
#[derive(Resource, Default, Debug, Clone)]
pub struct SaveStatus {
    pub message: String,
}

/// Pick the HUD tone for a save/load status message.
fn status_tone(message: &str) -> NotificationTone {
    let lower = message.to_ascii_lowercase();
    if lower.contains("failed") {
        NotificationTone::Error
    } else if lower.starts_with("saved") || lower.starts_with("loaded") {
        NotificationTone::Success
    } else {
        NotificationTone::Info
    }
}

pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingEdits>()
            .init_resource::<SaveStatus>()
            .add_systems(
                Update,
                (handle_save_keys, publish_save_status, apply_pending_edits).chain(),
            );
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_save_keys(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<InputBindings>,
    ui: Res<UiManager>,
    mut manager: ResMut<ChunkManager>,
    mut pockets: ResMut<Pockets>,
    mut backpack: ResMut<Backpack>,
    mut progression: ResMut<Progression>,
    mut structures: ResMut<Structures>,
    mut structure_entities: ResMut<StructureEntities>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut pending: ResMut<PendingEdits>,
    mut status: ResMut<SaveStatus>,
    mut map_exploration: Option<ResMut<crate::map::MapExploration>>,
) {
    if ui.any_open() {
        return;
    }
    if keys.just_pressed(bindings.save) {
        let mut explored: Vec<_> = map_exploration
            .as_ref()
            .map(|m| m.explored.iter().copied().collect())
            .unwrap_or_default();
        explored.sort_by_key(|c| (c.x, c.y));
        let data = SaveData::capture(
            &manager,
            &pockets,
            &backpack,
            &progression,
            &structures,
            explored,
        );
        let text = data.to_text();
        match write_save_atomic(SAVE_PATH, &text) {
            Ok(()) => {
                status.message = format!(
                    "Saved {} detail, {} world, {} turf and {} snow edits",
                    data.detail_edits.len(),
                    data.world_edits.len(),
                    data.turf_edits.len(),
                    data.snow_edits.len()
                );
            }
            Err(error) => status.message = format!("Save failed: {error}"),
        }
    }

    if keys.just_pressed(bindings.load) {
        match fs::metadata(SAVE_PATH)
            .map_err(|error| error.to_string())
            .and_then(|meta| {
                if meta.len() as usize > SAVE_MAX_BYTES {
                    Err(format!(
                        "save file too large ({} bytes, max {SAVE_MAX_BYTES})",
                        meta.len()
                    ))
                } else {
                    fs::read_to_string(SAVE_PATH).map_err(|error| error.to_string())
                }
            })
            .and_then(|text| load_save_text(&text))
        {
            Ok(data) => match data.restore_player(&mut pockets, &mut backpack, &mut progression) {
                Ok(()) => {
                    for id in structure_entities.0.keys().copied().collect::<Vec<_>>() {
                        despawn_structure_visual(&mut commands, &mut structure_entities, id);
                    }
                    data.seed_edit_logs(&mut manager, &mut structures);
                    for placed in structures.placed.values().cloned().collect::<Vec<_>>() {
                        spawn_structure_visual(
                            &mut commands,
                            &mut meshes,
                            &mut materials,
                            &mut structure_entities,
                            &placed,
                        );
                    }
                    pending.queue(&data);
                    if let Some(map_exploration) = map_exploration.as_mut() {
                        map_exploration.explored = data.map_explored.iter().copied().collect();
                        map_exploration.visible.clear();
                    }
                    status.message = format!(
                        "Loaded {} detail, {} world, {} turf and {} snow edits",
                        data.detail_edits.len(),
                        data.world_edits.len(),
                        data.turf_edits.len(),
                        data.snow_edits.len()
                    );
                }
                Err(error) => status.message = format!("Load failed: {error}"),
            },
            Err(error) => status.message = format!("Load failed: {error}"),
        }
    }
}

/// Mirror a fresh save/load status into the notification feed. The status text
/// itself stays in place for the debug UI.
fn publish_save_status(status: Res<SaveStatus>, mut feed: ResMut<NotificationFeed>) {
    if !status.is_changed() {
        return;
    }
    feed.push_status(status.message.clone(), status_tone(&status.message));
}

/// Apply queued edits to any chunk that is currently loaded, then remesh it.
/// Edits in unloaded regions stay queued until streaming reaches them.
///
/// Conservative gating (E4): when queued edits exist, rescan only if the pending
/// queue or chunk manager changed since last run. Chunk loads/spawns/remeshes
/// mutate `ChunkManager`, so loaded-target edits can be applied when chunks
/// become available without rescanning every steady-state frame. Behavior is
/// preserved for load/save/streaming flows, but runtime confirmation is still
/// recommended for edge cases.
fn apply_pending_edits(
    mut commands: Commands,
    mut manager: ResMut<ChunkManager>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut pending: ResMut<PendingEdits>,
) {
    if pending.is_empty() {
        return;
    }
    if !pending.is_changed() && !manager.is_changed() {
        return;
    }

    let mut dirty = Vec::new();

    pending.detail.retain(|(pos, voxel)| {
        if !detail_y_in_range(pos.y) {
            return false;
        }
        if manager.get_voxel_if_loaded(*pos).is_none() {
            return true;
        }
        dirty.extend(manager.set_voxel_untracked(*pos, *voxel));
        false
    });

    pending.world.retain(|(pos, block)| {
        if !world_y_in_range(pos.y) {
            return false;
        }
        if manager.get_world_block(*pos).is_none() {
            return true;
        }
        dirty.extend(manager.set_world_block_untracked(*pos, *block));
        false
    });

    retain_ready_surface_edits(
        &mut manager,
        &mut dirty,
        SurfaceLayer::Turf,
        &mut pending.turf,
    );
    retain_ready_surface_edits(
        &mut manager,
        &mut dirty,
        SurfaceLayer::Snow,
        &mut pending.snow,
    );

    if dirty.is_empty() {
        return;
    }

    dirty.sort_unstable_by_key(|coord| (coord.x, coord.y, coord.z));
    dirty.dedup();
    for coord in dirty {
        remesh_chunk(&mut commands, &mut manager, &mut meshes, coord);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_backpack(backpack: &Backpack, item: ItemId) -> u32 {
        let snapshot = backpack.snapshot();
        let generic: u32 = snapshot
            .slots
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum();
        generic
            + snapshot
                .categories
                .iter()
                .flat_map(|(_, entries)| entries.iter().flatten())
                .filter(|stack| stack.item == item)
                .map(|stack| stack.count)
                .sum::<u32>()
            + snapshot
                .attachments
                .iter()
                .flatten()
                .filter(|stack| stack.item == item)
                .map(|stack| stack.count)
                .sum::<u32>()
    }

    fn attachment_count(backpack: &Backpack, item: ItemId) -> u32 {
        backpack
            .snapshot()
            .attachments
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum()
    }

    #[test]
    fn status_tone_follows_the_message() {
        assert_eq!(
            status_tone("Save failed: disk full"),
            NotificationTone::Error
        );
        assert_eq!(
            status_tone("Load failed: bad header"),
            NotificationTone::Error
        );
        assert_eq!(
            status_tone("Saved 2 detail, 1 world, 0 turf and 0 snow edits"),
            NotificationTone::Success
        );
        assert_eq!(
            status_tone("Loaded 2 detail, 1 world, 0 turf and 0 snow edits"),
            NotificationTone::Success
        );
        assert_eq!(status_tone("Working"), NotificationTone::Info);
    }

    fn sample() -> SaveData {
        let mut backpack = BackpackSnapshot {
            capacity: BASE_CAPACITY,
            slots: vec![None; BASE_CAPACITY],
            attachments: vec![None; ATTACHMENT_SLOTS],
            ..default()
        };
        backpack.slots[0] = Some(ItemStack {
            item: ItemId::Granite,
            count: 120,
        });
        backpack.slots[7] = Some(ItemStack {
            item: ItemId::IronOre,
            count: 12,
        });
        backpack.categories.push((
            ItemCategory::Ores,
            vec![
                Some(ItemStack {
                    item: ItemId::IronOre,
                    count: 8,
                }),
                None,
                None,
                None,
            ],
        ));
        backpack.attachments[0] = Some(ItemStack {
            item: ItemId::Lantern,
            count: 1,
        });

        let mut pockets = vec![None; POCKET_COUNT];
        pockets[0] = Some(ItemStack {
            item: ItemId::Granite,
            count: 3,
        });
        pockets[5] = Some(ItemStack {
            item: ItemId::Wood,
            count: 1,
        });

        SaveData {
            selected: 5,
            pockets,
            backpack,
            progression: ProgressionSnapshot {
                level: 4,
                xp: 123,
                coins: 45,
                perk_points: 2,
                stamina_bonus: 6,
                hunger_bonus: 4,
                hydration_bonus: 2,
                hunger: 87.5,
                hydration: 91.25,
            },
            detail_edits: vec![
                (
                    IVec3::new(4, 8, 4),
                    Voxel::from_oriented_parts(
                        Block::Granite,
                        Shape::Stairs,
                        Orientation::from_facing(crate::world::block::Facing::East),
                    ),
                ),
                (IVec3::new(-3, 2, 9), Voxel::new(Block::Granite)),
                (IVec3::new(1, -4, 2), Voxel::new(Block::Granite)),
            ],
            world_edits: vec![
                (IVec3::new(2, 4, 2), Block::Air),
                (IVec3::new(-1, 5, 8), Block::Sand),
                (IVec3::new(3, -2, 3), Block::Granite),
            ],
            turf_edits: vec![
                (IVec3::new(2, 4, 2), false),
                (IVec3::new(6, 9, 6), true),
                (IVec3::new(3, -2, 3), true),
            ],
            snow_edits: vec![(IVec3::new(7, 9, 7), false)],
            structures: Vec::new(),
            map_explored: vec![IVec2::new(0, 0), IVec2::new(1, -2)],
        }
    }

    #[test]
    fn save_round_trips_every_field() {
        let original = sample();
        let text = original.to_text();
        assert!(text.starts_with(HEADER));
        assert!(text.is_ascii(), "save format stays ASCII");
        assert!(text.contains("map_explored 0 0"));
        assert!(text.contains("map_explored 1 -2"));

        let parsed = SaveData::from_text(&text).expect("save should parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn old_save_without_map_explored_loads() {
        let mut original = sample();
        original.map_explored.clear();
        let mut text = original.to_text();
        text = text
            .lines()
            .filter(|l| !l.starts_with("map_explored "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let parsed = SaveData::from_text(&text).expect("old save should parse");
        assert!(parsed.map_explored.is_empty());
    }

    #[test]
    fn invalid_map_explored_is_rejected() {
        let mut original = sample();
        original.map_explored.push(IVec2::new(0, 0));
        let err = SaveData::from_text(&original.to_text()).unwrap_err();
        assert!(err.contains("duplicate map_explored"));
    }

    #[test]
    fn player_state_is_restored_exactly() {
        let original = sample();
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();

        let mut progression = Progression::default();
        original
            .restore_player(&mut pockets, &mut backpack, &mut progression)
            .expect("sample save should restore");

        assert_eq!(pockets.selected(), 5);
        assert_eq!(pockets.count(ItemId::Granite), 3);
        assert_eq!(pockets.count(ItemId::Wood), 1);
        assert_eq!(count_backpack(&backpack, ItemId::Granite), 120);
        assert_eq!(count_backpack(&backpack, ItemId::IronOre), 20);
        assert_eq!(attachment_count(&backpack, ItemId::Lantern), 1);
        assert_eq!(progression.level(), 4);
        assert_eq!(progression.xp(), 123);
        assert_eq!(progression.coins(), 45);
        assert_eq!(progression.perk_points(), 2);
        assert_eq!(progression.hunger(), 87.5);
        assert_eq!(progression.hydration(), 91.25);
    }

    #[test]
    fn loaded_edits_survive_a_later_save() {
        let original = sample();
        let mut manager = ChunkManager::default();
        let mut structures = Structures::default();
        original.seed_edit_logs(&mut manager, &mut structures);

        let captured = SaveData::capture(
            &manager,
            &Pockets::default(),
            &Backpack::default(),
            &Progression::default(),
            &structures,
            Vec::new(),
        );

        let mut expected_detail = original.detail_edits.clone();
        let mut captured_detail = captured.detail_edits.clone();
        expected_detail.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        captured_detail.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        assert_eq!(captured_detail, expected_detail);

        let mut expected_world = original.world_edits.clone();
        let mut captured_world = captured.world_edits.clone();
        expected_world.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        captured_world.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        assert_eq!(captured_world, expected_world);

        let mut expected_turf = original.turf_edits.clone();
        let mut captured_turf = captured.turf_edits.clone();
        expected_turf.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        captured_turf.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        assert_eq!(captured_turf, expected_turf);

        let mut expected_snow = original.snow_edits.clone();
        let mut captured_snow = captured.snow_edits.clone();
        expected_snow.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        captured_snow.sort_by_key(|(pos, _)| (pos.x, pos.y, pos.z));
        assert_eq!(captured_snow, expected_snow);
    }

    #[test]
    fn legacy_fragment_records_are_accepted_and_ignored() {
        let original = sample();
        let mut text = original.to_text();
        // Old saves wrote partial material pieces. The hammer no longer consumes
        // them, but loading such a save must still succeed.
        text.insert_str(HEADER.len() + 1, "fragment 11 5\nfragment 5 7\n");
        assert!(text.is_ascii());

        let parsed = SaveData::from_text(&text).expect("legacy save should parse");
        assert_eq!(parsed, original);
    }

    fn pocket_count(pockets: &[Option<ItemStack>], item: ItemId) -> u32 {
        pockets
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum()
    }

    #[test]
    fn legacy_attachment_records_attach_to_backpack() {
        let mut original = sample();
        original.backpack.attachments = vec![None; ATTACHMENT_SLOTS];
        let mut text = original.to_text();
        // Old saves wrote magnet and lantern attachment levels. Positive levels
        // are reconciled into backpack attachment slots.
        text.insert_str(HEADER.len() + 1, "magnet 2\nlantern 1\nlantern 0\n");
        assert!(text.is_ascii());

        let parsed = SaveData::from_text(&text).expect("legacy save should parse");
        assert_eq!(pocket_count(&parsed.pockets, ItemId::Magnet), 0);
        assert_eq!(pocket_count(&parsed.pockets, ItemId::Lantern), 0);
        let mut expected = original.clone();
        expected.backpack.attachments[0] = Some(ItemStack {
            item: ItemId::Magnet,
            count: 1,
        });
        expected.backpack.attachments[1] = Some(ItemStack {
            item: ItemId::Lantern,
            count: 1,
        });
        assert_eq!(parsed, expected);
    }

    #[test]
    fn legacy_attachment_records_fall_back_to_pockets_when_attachments_full() {
        let mut original = sample();
        original.backpack.attachments = vec![
            Some(ItemStack {
                item: ItemId::Lantern,
                count: 1,
            });
            ATTACHMENT_SLOTS
        ];
        let mut text = original.to_text();
        text.insert_str(HEADER.len() + 1, "magnet 1\n");
        assert!(text.is_ascii());

        let parsed = SaveData::from_text(&text).expect("legacy save should parse");
        assert_eq!(pocket_count(&parsed.pockets, ItemId::Magnet), 1);
        let mut expected = original.clone();
        expected.pockets[1] = Some(ItemStack {
            item: ItemId::Magnet,
            count: 1,
        });
        assert_eq!(parsed, expected);
    }

    #[test]
    fn legacy_light_flags_are_accepted_and_ignored() {
        let original = sample();
        let mut text = original.to_text();
        // Old staff-light state has no trinket equivalent and must not block
        // loading a v2 save.
        text.insert_str(HEADER.len() + 1, "light_active 1\nlight_enabled 0\n");
        assert!(text.is_ascii());

        let parsed = SaveData::from_text(&text).expect("legacy save should parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn malformed_files_are_rejected() {
        assert!(SaveData::from_text("").is_err());
        assert!(SaveData::from_text("NOT_A_SAVE 1\n").is_err());
        assert!(SaveData::from_text(&format!("{HEADER}\nselected nope\n")).is_err());
        assert!(SaveData::from_text(&format!("{HEADER}\nmystery 1\n")).is_err());
        let v1 = SaveData::from_text("KT_SAVE 1\n").unwrap_err();
        assert!(v1.contains("unsupported save version KT_SAVE 1"));
        assert!(v1.contains("no safe automatic migration"));
        let future = SaveData::from_text("KT_SAVE 999\n").unwrap_err();
        assert!(future.contains("unsupported save version KT_SAVE 999"));
        assert!(
            SaveData::from_text(&format!("{HEADER}\nselected 0 extra\n")).is_err(),
            "extra tokens must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\nhunger NaN\n")).is_err(),
            "non-finite hunger must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\nhydration inf\n")).is_err(),
            "non-finite hydration must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\npocket 0 0 0\n")).is_err(),
            "zero item counts must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\nworld 0 99999 0 1\n")).is_err(),
            "out-of-range world edit coordinates must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\nworld 99999 0 0 1\n")).is_err(),
            "out-of-range world XZ edit coordinates must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\ndetail 999999 0 0 1 0 0\n")).is_err(),
            "out-of-range detail XZ edit coordinates must be rejected"
        );
        assert!(
            SaveData::from_text(&format!("{HEADER}\nturf 99999 0 0 1\n")).is_err(),
            "out-of-range turf XZ edit coordinates must be rejected"
        );
        let mut too_many_edits = format!("{HEADER}\n");
        for i in 0..=SAVE_MAX_EDIT_RECORDS {
            too_many_edits.push_str(&format!("world {i} 0 0 1\n"));
        }
        assert!(
            SaveData::from_text(&too_many_edits).is_err(),
            "excessive edit record counts must be rejected"
        );
        let long_line = format!("{HEADER}\n#{}\n", "a".repeat(SAVE_MAX_LINE_BYTES + 1));
        let err = SaveData::from_text(&long_line).unwrap_err();
        assert!(err.contains("too long"));
        let oversized = "x".repeat(SAVE_MAX_BYTES + 1);
        let err = SaveData::from_text(&oversized).unwrap_err();
        assert!(err.contains("save file too large"));
    }

    #[test]
    fn invalid_inventory_states_are_rejected() {
        let max = ItemId::Granite.def().max_stack;
        let granite = ItemId::Granite.index();
        let txt = format!("{HEADER}\nselected 0\npocket 0 {granite} {}\n", max + 1);
        let err = SaveData::from_text(&txt).unwrap_err();
        assert!(err.contains("exceeds max stack"), "{err}");

        let err = SaveData::from_text(&format!("{HEADER}\nbackpack_capacity 17\n")).unwrap_err();
        assert!(err.contains("invalid backpack capacity"), "{err}");

        let mut bad_category = sample();
        bad_category.backpack.categories[0].0 = ItemCategory::Materials;
        let err = SaveData::from_text(&bad_category.to_text()).unwrap_err();
        assert!(err.contains("belongs to"));

        // Structural mismatch not representable through normalized text: validate directly.
        let mut bad_slots = sample();
        bad_slots.backpack.slots.pop();
        let err = validate_save_data(&bad_slots).unwrap_err();
        assert!(err.contains("do not match declared capacity"), "{err}");

        let mut dup_cat = sample();
        dup_cat
            .backpack
            .categories
            .push((ItemCategory::Ores, vec![None; CATEGORY_SLOTS_PER_UNLOCK]));
        let err = validate_save_data(&dup_cat).unwrap_err();
        assert!(err.contains("duplicate backpack category section"), "{err}");
    }

    #[test]
    fn failed_restore_does_not_mutate_live_state() {
        let mut pockets = Pockets::default();
        pockets.replace(
            0,
            Some(ItemStack {
                item: ItemId::Wood,
                count: 2,
            }),
        );
        pockets.select(0);
        let mut backpack = Backpack::default();
        backpack.add(ItemId::Granite, 3);
        let mut progression = Progression::from_snapshot(ProgressionSnapshot {
            level: 1,
            xp: 7,
            coins: 42,
            perk_points: 0,
            stamina_bonus: 0,
            hunger_bonus: 0,
            hydration_bonus: 0,
            hunger: 0.0,
            hydration: 0.0,
        });

        let pockets_before = pockets.snapshot();
        let backpack_before = backpack.snapshot();
        let progression_before = progression.snapshot();

        let mut bad = sample();
        bad.backpack.slots[0] = Some(ItemStack {
            item: ItemId::Granite,
            count: ItemId::Granite.def().max_stack + 1,
        });
        let err = bad
            .restore_player(&mut pockets, &mut backpack, &mut progression)
            .unwrap_err();
        assert!(err.contains("exceeds max stack"));

        assert_eq!(pockets.snapshot(), pockets_before);
        assert_eq!(backpack.snapshot(), backpack_before);
        assert_eq!(progression.snapshot(), progression_before);
    }

    #[test]
    fn atomic_save_writes_file_and_removes_temp() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-save-test-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("save.kt");
        let tmp = dir.join("save.kt.tmp");

        write_save_atomic(&path, "KT_SAVE 2\nselected 0\n").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "KT_SAVE 2\nselected 0\n"
        );
        assert!(
            !tmp.exists(),
            "temporary save file must be removed after rename"
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn pending_edits_only_apply_to_loaded_chunks() {
        use crate::world::chunk::{Chunk, ChunkData};
        use bevy::prelude::Entity;

        let mut manager = ChunkManager::default();
        manager.chunks.insert(
            IVec3::ZERO,
            Chunk {
                data: ChunkData::new(IVec3::ZERO),
                entity: Entity::PLACEHOLDER,
                water_entity: None,
                foliage_entity: None,
                triangles: 0,
                water_triangles: 0,
                foliage_triangles: 0,
            },
        );

        let loaded = IVec3::new(2, 8, 2);
        let unloaded = IVec3::new(1000, 8, 1000);
        let mut pending = PendingEdits {
            detail: vec![
                (loaded, Voxel::new(Block::Granite)),
                (unloaded, Voxel::new(Block::Granite)),
            ],
            world: vec![(IVec3::new(1, 4, 1), Block::Air)],
            turf: vec![(IVec3::new(1, 4, 1), false)],
            snow: vec![(IVec3::new(2, 4, 2), false)],
        };

        let mut dirty = Vec::new();
        pending.detail.retain(|(pos, voxel)| {
            if manager.get_voxel_if_loaded(*pos).is_none() {
                return true;
            }
            dirty.extend(manager.set_voxel_untracked(*pos, *voxel));
            false
        });
        pending.world.retain(|(pos, block)| {
            if manager.get_world_block(*pos).is_none() {
                return true;
            }
            dirty.extend(manager.set_world_block_untracked(*pos, *block));
            false
        });
        retain_ready_surface_edits(
            &mut manager,
            &mut dirty,
            SurfaceLayer::Turf,
            &mut pending.turf,
        );
        retain_ready_surface_edits(
            &mut manager,
            &mut dirty,
            SurfaceLayer::Snow,
            &mut pending.snow,
        );

        assert_eq!(
            manager.get_voxel_if_loaded(loaded),
            Some(Voxel::new(Block::Granite))
        );
        assert_eq!(
            manager.get_world_block(IVec3::new(1, 4, 1)),
            Some(Block::Air)
        );
        assert_eq!(pending.detail.len(), 1);
        assert_eq!(pending.detail[0].0, unloaded);
        assert!(pending.world.is_empty());
        assert!(pending.turf.is_empty());
        assert!(pending.snow.is_empty());
        assert_eq!(manager.get_world_turf(IVec3::new(1, 4, 1)), Some(false));
        assert_eq!(
            manager.get_world_surface(SurfaceLayer::Snow, IVec3::new(2, 4, 2)),
            Some(false)
        );
        assert!(!dirty.is_empty());
    }

    #[test]
    fn edits_are_tracked_for_saving() {
        use crate::world::chunk::{Chunk, ChunkData};
        use bevy::prelude::Entity;

        let mut manager = ChunkManager::default();
        manager.chunks.insert(
            IVec3::ZERO,
            Chunk {
                data: ChunkData::new(IVec3::ZERO),
                entity: Entity::PLACEHOLDER,
                water_entity: None,
                foliage_entity: None,
                triangles: 0,
                water_triangles: 0,
                foliage_triangles: 0,
            },
        );

        let pos = IVec3::new(3, 7, 3);
        manager.set_voxel(pos, Voxel::new(Block::Slate));
        manager.set_world_block(IVec3::new(1, 3, 1), Block::Air);
        manager.set_world_turf(IVec3::new(2, 5, 2), false);
        manager.set_world_surface(SurfaceLayer::Snow, IVec3::new(3, 5, 3), false);

        assert_eq!(
            manager.detail_edits.get(&pos),
            Some(&Voxel::new(Block::Slate))
        );
        assert_eq!(
            manager.world_edits.get(&IVec3::new(1, 3, 1)),
            Some(&Block::Air)
        );
        assert_eq!(manager.turf_edits.get(&IVec3::new(2, 5, 2)), Some(&false));
        assert_eq!(manager.snow_edits.get(&IVec3::new(3, 5, 3)), Some(&false));
    }

    #[test]
    fn structure_records_round_trip() {
        let mut data = sample();
        data.structures = vec![
            (
                1,
                StructureKind::Workbench.index(),
                IVec3::new(10, 8, -4),
                1,
            ),
            (7, StructureKind::Furnace.index(), IVec3::new(-3, 2, 9), 3),
        ];
        let text = data.to_text();
        let parsed = SaveData::from_text(&text).expect("structure save should parse");
        assert_eq!(parsed.structures, data.structures);

        let mut manager = ChunkManager::default();
        let mut structures = Structures::default();
        parsed.seed_edit_logs(&mut manager, &mut structures);
        assert_eq!(structures.placed.len(), 2);
        let furnace = structures
            .placed
            .values()
            .find(|s| s.kind == StructureKind::Furnace)
            .expect("furnace should restore");
        assert_eq!(furnace.anchor, IVec3::new(-3, 2, 9));
        assert_eq!(furnace.occupied_cells().len(), 8);
    }

    #[test]
    fn invalid_structure_records_are_rejected_before_mutation() {
        let mut manager = ChunkManager::default();
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let mut progression = Progression::default();
        let mut structures = Structures::default();
        pockets.add(ItemId::Wood, 5);
        progression.award_coins(99);

        let bad = format!(
            "{HEADER}\nselected 0\nbackpack_capacity {BASE_CAPACITY}\nworld 1 2 3 1\nstructure 1 255 0 0 0 0\n"
        );
        let err = load_save_text(&bad).expect_err("invalid structure kind should fail");
        assert!(err.contains("structure kind"), "unexpected error: {err}");

        // Simulate the load path: parse/validate first, only mutate live state on success.
        let load_result = load_save_text(&bad)
            .and_then(|data| {
                data.restore_player(&mut pockets, &mut backpack, &mut progression)?;
                Ok(data)
            })
            .map(|data| {
                data.seed_edit_logs(&mut manager, &mut structures);
            });
        assert!(load_result.is_err());

        assert_eq!(pockets.count(ItemId::Wood), 5);
        assert_eq!(progression.coins(), 99);
        assert!(structures.placed.is_empty());
        assert!(manager.detail_edits.is_empty());
        assert!(manager.world_edits.is_empty());
    }

    #[test]
    fn old_save_without_structure_records_remains_loadable() {
        let mut data = sample();
        data.structures.clear();
        let mut text = data.to_text();
        text.retain(|c| c != '\r');
        let filtered: String = text
            .lines()
            .filter(|line| !line.starts_with("structure "))
            .map(|line| format!("{line}\n"))
            .collect();
        let parsed =
            SaveData::from_text(&filtered).expect("old save without structures should parse");
        assert!(parsed.structures.is_empty());

        let mut manager = ChunkManager::default();
        let mut structures = Structures::default();
        parsed.seed_edit_logs(&mut manager, &mut structures);
        assert!(structures.placed.is_empty());
    }
}
