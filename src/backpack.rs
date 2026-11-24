//! Backpack storage: bulk slots, category sections, capacity upgrades, and
//! physical attachment slots.
//!
//! The backpack is data only. The TAB key opens its storage panel, but there is
//! no follower entity and no weight simulation. Attachment slots hold real
//! items that stay with the backpack and can provide passive effects.

use bevy::prelude::*;

use crate::items::{ItemCategory, ItemId, ItemStack};

pub const BASE_CAPACITY: usize = 18;
pub const MAX_CAPACITY: usize = 40;
pub const CATEGORY_SLOTS_PER_UNLOCK: usize = 4;
pub const ATTACHMENT_SLOTS: usize = 4;

/// A coloured, category-specific section of backpack storage.
#[derive(Clone, Debug)]
pub struct CategoryStorage {
    pub category: ItemCategory,
    pub entries: Vec<Option<ItemStack>>,
}

impl CategoryStorage {
    pub fn new(category: ItemCategory, slots: usize) -> Self {
        Self {
            category,
            entries: vec![None; slots],
        }
    }

    pub fn total_items(&self) -> u32 {
        self.entries.iter().flatten().map(|stack| stack.count).sum()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackpackSnapshot {
    pub capacity: usize,
    pub slots: Vec<Option<ItemStack>>,
    pub categories: Vec<(ItemCategory, Vec<Option<ItemStack>>)>,
    pub attachments: Vec<Option<ItemStack>>,
}

#[derive(Resource, Debug)]
pub struct Backpack {
    slots: Vec<Option<ItemStack>>,
    pub category_slots: Vec<CategoryStorage>,
    attachments: [Option<ItemStack>; ATTACHMENT_SLOTS],
}

impl Default for Backpack {
    fn default() -> Self {
        Self {
            slots: vec![None; BASE_CAPACITY],
            category_slots: Vec::new(),
            attachments: [None; ATTACHMENT_SLOTS],
        }
    }
}

impl Backpack {
    #[inline]
    pub fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Remove the stack from one general storage slot.
    pub fn take_slot(&mut self, index: usize) -> Option<ItemStack> {
        self.slots.get_mut(index)?.take()
    }

    /// Replace a general storage slot and return its previous stack.
    pub fn replace_slot(&mut self, index: usize, stack: Option<ItemStack>) -> Option<ItemStack> {
        let slot = self.slots.get_mut(index)?;
        std::mem::replace(slot, stack.filter(|stack| stack.count > 0))
    }

    /// Remove the stack from one unlocked category slot.
    pub fn take_category_slot(
        &mut self,
        category: ItemCategory,
        index: usize,
    ) -> Option<ItemStack> {
        self.category_slots
            .iter_mut()
            .find(|storage| storage.category == category)?
            .entries
            .get_mut(index)?
            .take()
    }

    /// Replace an unlocked category slot and return its previous stack.
    pub fn replace_category_slot(
        &mut self,
        category: ItemCategory,
        index: usize,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let entry = self
            .category_slots
            .iter_mut()
            .find(|storage| storage.category == category)?
            .entries
            .get_mut(index)?;
        std::mem::replace(entry, stack.filter(|stack| stack.count > 0))
    }

    #[inline]
    pub fn attachments(&self) -> &[Option<ItemStack>; ATTACHMENT_SLOTS] {
        &self.attachments
    }

    /// Remove the stack from one backpack attachment slot.
    pub fn take_attachment(&mut self, index: usize) -> Option<ItemStack> {
        self.attachments.get_mut(index)?.take()
    }

    /// Replace a backpack attachment slot and return its previous stack.
    pub fn replace_attachment(
        &mut self,
        index: usize,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let slot = self.attachments.get_mut(index)?;
        std::mem::replace(slot, stack.filter(|stack| stack.count > 0))
    }

    /// Returns `true` if the item is attached to the backpack.
    pub fn has_attachment_item(&self, item: ItemId) -> bool {
        self.attachments
            .iter()
            .flatten()
            .any(|stack| stack.item == item)
    }

    pub fn upgrade_capacity(&mut self) -> bool {
        if self.slots.len() >= MAX_CAPACITY {
            return false;
        }
        let extra = 4.min(MAX_CAPACITY - self.slots.len());
        self.slots.extend(std::iter::repeat_n(None, extra));
        true
    }

    #[inline]
    pub fn has_category(&self, category: ItemCategory) -> bool {
        self.category_slots
            .iter()
            .any(|storage| storage.category == category)
    }

    /// Unlock a category-specific section and immediately reorganise existing
    /// items into it.
    pub fn unlock_category(&mut self, category: ItemCategory) -> bool {
        if self.has_category(category) {
            return false;
        }
        self.category_slots
            .push(CategoryStorage::new(category, CATEGORY_SLOTS_PER_UNLOCK));
        self.reorganize();
        true
    }

    /// Move matching items from generic slots into category slots. Called when
    /// a new category is unlocked.
    pub fn reorganize(&mut self) {
        let mut items: Vec<ItemStack> = self
            .slots
            .iter()
            .flatten()
            .copied()
            .chain(
                self.category_slots
                    .iter()
                    .flat_map(|storage| storage.entries.iter().flatten().copied()),
            )
            .collect();

        self.slots.fill(None);
        for storage in &mut self.category_slots {
            storage.entries.fill(None);
        }

        // Stable ordering makes the resulting layout deterministic.
        items.sort_by_key(|stack| stack.item.index());
        for stack in items {
            let leftover = self.add(stack.item, stack.count);
            if leftover > 0 {
                // Preserve data integrity: inventories that already fit should not
                // lose items during reorganization. Force-store leftovers into any
                // available slot space as a defensive fallback.
                self.force_add(stack.item, leftover);
            }
        }
    }

    /// Sort backpack contents: merge stacks, then order by category/item for a
    /// stable, readable layout. Category-section items stay in their sections;
    /// unlocked categories are filled first as usual.
    pub fn sort(&mut self) {
        self.reorganize();
    }

    /// Add items to the matching category section first, then to generic
    /// storage. Returns leftovers.
    pub fn add(&mut self, item: ItemId, mut count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let category = item.def().category;
        if let Some(storage) = self
            .category_slots
            .iter_mut()
            .find(|storage| storage.category == category)
        {
            count = add_to_entries(&mut storage.entries, item, count);
        }
        if count > 0 {
            count = add_to_entries(&mut self.slots, item, count);
        }
        count
    }

    /// Remove `count` items from category storage first, then generic storage.
    /// Items may be drained across multiple stacks/sections as long as the
    /// backpack contains enough in total.
    pub fn consume(&mut self, item: ItemId, mut count: u32) -> bool {
        if self.count(item) < count {
            return false;
        }
        for storage in &mut self.category_slots {
            if count == 0 {
                break;
            }
            count = drain_from_entries(&mut storage.entries, item, count);
        }
        if count > 0 {
            count = drain_from_entries(&mut self.slots, item, count);
        }
        debug_assert_eq!(count, 0);
        true
    }

    /// Count one item across generic storage and category sections.
    /// Attachments are separate physical slots and are not included.
    pub fn count(&self, item: ItemId) -> u32 {
        let generic: u32 = self
            .slots
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum();
        let category: u32 = self
            .category_slots
            .iter()
            .flat_map(|storage| storage.entries.iter().flatten())
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum();
        generic + category
    }

    pub fn total_items(&self) -> u32 {
        let generic: u32 = self.slots.iter().flatten().map(|stack| stack.count).sum();
        generic
            + self
                .category_slots
                .iter()
                .map(CategoryStorage::total_items)
                .sum::<u32>()
            + self
                .attachments
                .iter()
                .flatten()
                .map(|stack| stack.count)
                .sum::<u32>()
    }

    pub fn clear(&mut self) {
        self.slots.fill(None);
        for storage in &mut self.category_slots {
            storage.entries.fill(None);
        }
        self.attachments = [None; ATTACHMENT_SLOTS];
    }

    /// Complete storage layout, including capacity, category sections,
    /// inventory contents and backpack attachments. Save files use this so a
    /// reload does not lose upgrades, reorganised items or attachments.
    pub fn snapshot(&self) -> BackpackSnapshot {
        BackpackSnapshot {
            capacity: self.slots.len(),
            slots: self.slots.clone(),
            categories: self
                .category_slots
                .iter()
                .map(|storage| (storage.category, storage.entries.clone()))
                .collect(),
            attachments: self.attachments.to_vec(),
        }
    }

    /// Validate and restore a snapshot. Invalid or impossible inventory states
    /// are rejected so malformed saves cannot silently reshape or discard data.
    pub fn restore(&mut self, snapshot: &BackpackSnapshot) -> Result<(), String> {
        Self::validate_restore(snapshot)?;
        let capacity = snapshot.capacity;
        self.slots = vec![None; capacity];
        for (index, stack) in snapshot.slots.iter().take(capacity).enumerate() {
            self.slots[index] = *stack;
        }

        self.category_slots.clear();
        for (category, entries) in &snapshot.categories {
            let mut storage = CategoryStorage::new(*category, CATEGORY_SLOTS_PER_UNLOCK);
            for (index, stack) in entries.iter().take(CATEGORY_SLOTS_PER_UNLOCK).enumerate() {
                storage.entries[index] = *stack;
            }
            self.category_slots.push(storage);
        }

        self.attachments = [None; ATTACHMENT_SLOTS];
        for (index, stack) in snapshot
            .attachments
            .iter()
            .take(ATTACHMENT_SLOTS)
            .enumerate()
        {
            self.attachments[index] = *stack;
        }
        Ok(())
    }

    pub fn validate_restore(snapshot: &BackpackSnapshot) -> Result<(), String> {
        let capacity = snapshot.capacity;
        if !(BASE_CAPACITY..=MAX_CAPACITY).contains(&capacity) {
            return Err(format!(
                "invalid backpack capacity {capacity} (expected {BASE_CAPACITY}..={MAX_CAPACITY})"
            ));
        }
        if snapshot.slots.len() != capacity {
            return Err(format!(
                "backpack slots {} do not match declared capacity {capacity}",
                snapshot.slots.len()
            ));
        }
        for (index, stack) in snapshot.slots.iter().enumerate() {
            if let Some(stack) = stack {
                validate_backpack_stack(stack, &format!("backpack slot {index}"))?;
            }
        }

        let mut seen_categories = std::collections::HashSet::new();
        for (category, entries) in &snapshot.categories {
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
                let Some(stack) = stack else { continue };
                validate_backpack_stack(
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

        if snapshot.attachments.len() != ATTACHMENT_SLOTS {
            return Err(format!(
                "invalid attachment slot count: {} (expected {ATTACHMENT_SLOTS})",
                snapshot.attachments.len()
            ));
        }
        for (index, stack) in snapshot.attachments.iter().enumerate() {
            if let Some(stack) = stack {
                validate_backpack_stack(stack, &format!("attachment slot {index}"))?;
            }
        }
        Ok(())
    }
}

fn drain_from_entries(entries: &mut [Option<ItemStack>], item: ItemId, mut count: u32) -> u32 {
    for entry in entries.iter_mut() {
        if count == 0 {
            break;
        }
        if let Some(stack) = entry.as_mut()
            && stack.item == item
        {
            let taken = stack.count.min(count);
            stack.count -= taken;
            count -= taken;
            if stack.count == 0 {
                *entry = None;
            }
        }
    }
    count
}

fn validate_backpack_stack(stack: &ItemStack, context: &str) -> Result<(), String> {
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

fn add_to_entries(entries: &mut [Option<ItemStack>], item: ItemId, mut count: u32) -> u32 {
    let max_stack = item.def().max_stack.max(1);

    for entry in entries.iter_mut() {
        if count == 0 {
            break;
        }
        if let Some(stack) = entry
            && stack.item == item
            && stack.count < max_stack
        {
            let moved = (max_stack - stack.count).min(count);
            stack.count += moved;
            count -= moved;
        }
    }

    for entry in entries.iter_mut() {
        if count == 0 {
            break;
        }
        if entry.is_none() {
            let moved = max_stack.min(count);
            *entry = Some(ItemStack { item, count: moved });
            count -= moved;
        }
    }

    count
}

impl Backpack {
    /// Defensive fallback used by `reorganize` to avoid silently discarding
    /// items when normal placement unexpectedly leaves leftovers. This should be
    /// unreachable for inventories that already fit, but it preserves data
    /// integrity if assumptions are violated.
    fn force_add(&mut self, item: ItemId, mut count: u32) {
        let max_stack = item.def().max_stack.max(1);
        for storage in &mut self.category_slots {
            count = add_to_entries(&mut storage.entries, item, count);
            if count == 0 {
                return;
            }
        }
        count = add_to_entries(&mut self.slots, item, count);
        if count == 0 {
            return;
        }

        // Absolute last resort: expand generic slots rather than dropping items.
        while count > 0 && self.slots.len() < MAX_CAPACITY {
            let moved = max_stack.min(count);
            self.slots.push(Some(ItemStack { item, count: moved }));
            count -= moved;
        }

        debug_assert_eq!(
            count, 0,
            "reorganize force_add could not preserve all items"
        );
    }
}

pub struct BackpackPlugin;

impl Plugin for BackpackPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Backpack>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::store_item;
    use crate::pockets::Pockets;

    #[test]
    fn backpack_starts_with_basic_capacity() {
        let backpack = Backpack::default();
        assert_eq!(BASE_CAPACITY, 18);
        assert_eq!(backpack.capacity(), BASE_CAPACITY);
        assert!(backpack.category_slots.is_empty());
    }

    #[test]
    fn capacity_upgrades_are_bounded() {
        let mut backpack = Backpack::default();
        while backpack.upgrade_capacity() {}
        assert_eq!(backpack.capacity(), MAX_CAPACITY);
        assert!(!backpack.upgrade_capacity());
    }

    #[test]
    fn category_slots_auto_sort_by_metadata() {
        let mut backpack = Backpack::default();
        assert!(backpack.unlock_category(ItemCategory::Food));
        assert_eq!(backpack.add(ItemId::Berry, 3), 0);
        let food = backpack
            .category_slots
            .iter()
            .find(|storage| storage.category == ItemCategory::Food)
            .unwrap();
        assert_eq!(food.entries.iter().flatten().count(), 1);
        assert_eq!(food.total_items(), 3);
        assert!(backpack.slots().iter().all(Option::is_none));
    }

    #[test]
    fn unlocking_a_category_reorganises_existing_items() {
        let mut backpack = Backpack::default();
        assert_eq!(backpack.add(ItemId::Granite, 5), 0);
        assert!(
            backpack
                .slots()
                .iter()
                .flatten()
                .any(|s| s.item == ItemId::Granite)
        );

        assert!(backpack.unlock_category(ItemCategory::Materials));
        assert!(!backpack.unlock_category(ItemCategory::Materials));
        assert!(backpack.slots().iter().all(Option::is_none));

        let materials = backpack
            .category_slots
            .iter()
            .find(|storage| storage.category == ItemCategory::Materials)
            .unwrap();
        assert_eq!(materials.total_items(), 5);
    }

    #[test]
    fn pickups_use_backpack_before_pockets() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        assert_eq!(
            store_item(&mut pockets, &mut backpack, ItemId::Granite, 3),
            0
        );
        assert_eq!(backpack.total_items(), 3);
        assert_eq!(pockets.count(ItemId::Granite), 0);
    }

    #[test]
    fn pickups_spill_into_pockets_when_backpack_is_full() {
        let mut pockets = Pockets::default();
        let mut backpack = Backpack::default();
        let max_stack = ItemId::Granite.def().max_stack;
        for _ in 0..backpack.capacity() {
            assert_eq!(backpack.add(ItemId::Granite, max_stack), 0);
        }

        assert_eq!(
            store_item(&mut pockets, &mut backpack, ItemId::Granite, 2),
            0
        );
        assert_eq!(pockets.count(ItemId::Granite), 2);
        assert_eq!(
            backpack.total_items(),
            backpack.capacity() as u32 * max_stack
        );
    }

    #[test]
    fn reorganize_preserves_items_when_generic_space_is_tight() {
        let mut backpack = Backpack::default();
        let max_stack = ItemId::Granite.def().max_stack;
        // Fill all generic slots.
        for index in 0..backpack.capacity() {
            backpack.replace_slot(
                index,
                Some(ItemStack {
                    item: ItemId::Granite,
                    count: max_stack,
                }),
            );
        }
        let total_before = backpack.total_items();

        backpack.unlock_category(ItemCategory::Materials);
        // Reorganize moves materials into category slots; no items should be lost.
        assert_eq!(backpack.total_items(), total_before);
        assert_eq!(backpack.count(ItemId::Granite), total_before);
    }
}
