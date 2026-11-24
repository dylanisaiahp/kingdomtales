//! Physical pockets: the eight quick-access slots shown above the hotbar.
//!
//! Each pocket holds one item stack, up to that item's metadata stack limit.
//! Mouse-wheel selection moves between pockets; the backpack owns bulk storage.

use bevy::prelude::*;

use crate::items::{ItemId, ItemStack};

pub const POCKET_COUNT: usize = 8;
/// One item stack per pocket. The stack limit comes from item metadata.
pub const POCKET_CAPACITY: usize = 1;

/// One physical pocket.
#[derive(Clone, Debug)]
pub struct Pocket {
    entries: Vec<Option<ItemStack>>,
}

impl Default for Pocket {
    fn default() -> Self {
        Self::new()
    }
}

impl Pocket {
    pub fn new() -> Self {
        Self {
            entries: vec![None; POCKET_CAPACITY],
        }
    }

    #[inline]
    pub fn entries(&self) -> &[Option<ItemStack>] {
        &self.entries
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }

    /// Merge into non-full stacks that already hold `item` without claiming an
    /// empty slot. Returns the amount that still needs a slot.
    pub fn merge_existing(&mut self, item: ItemId, mut count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let max_stack = item.def().max_stack.max(1);
        for entry in &mut self.entries {
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
        count
    }

    /// Add items, merging into existing stacks first. Returns leftovers.
    pub fn add(&mut self, item: ItemId, mut count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        count = self.merge_existing(item, count);
        if count == 0 {
            return 0;
        }

        let max_stack = item.def().max_stack.max(1);
        for entry in &mut self.entries {
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

    /// Consume from the first occupied entry. Returns false when there are not
    /// enough items in that entry.
    pub fn consume_first(&mut self, count: u32) -> bool {
        let Some(index) = self.entries.iter().position(Option::is_some) else {
            return false;
        };
        let stack = self.entries[index].as_mut().expect("checked above");
        if stack.count < count {
            return false;
        }
        stack.count -= count;
        if stack.count == 0 {
            self.entries[index] = None;
        }
        true
    }

    #[inline]
    pub fn first_stack(&self) -> Option<ItemStack> {
        self.entries.iter().flatten().next().copied()
    }

    pub fn total_items(&self) -> u32 {
        self.entries.iter().flatten().map(|stack| stack.count).sum()
    }

    pub fn clear(&mut self) {
        self.entries.fill(None);
    }
}

/// The player's eight pockets and the currently selected pocket.
#[derive(Resource, Debug)]
pub struct Pockets {
    pockets: Vec<Pocket>,
    selected: usize,
}

impl Default for Pockets {
    fn default() -> Self {
        Self {
            pockets: (0..POCKET_COUNT).map(|_| Pocket::new()).collect(),
            selected: 0,
        }
    }
}

impl Pockets {
    #[inline]
    pub fn pockets(&self) -> &[Pocket] {
        &self.pockets
    }

    #[inline]
    pub fn pocket(&self, index: usize) -> Option<&Pocket> {
        self.pockets.get(index)
    }

    #[inline]
    pub fn selected(&self) -> usize {
        self.selected.min(POCKET_COUNT - 1)
    }

    #[inline]
    fn selected_index(&self) -> usize {
        self.selected()
    }

    #[inline]
    pub fn selected_pocket(&self) -> &Pocket {
        &self.pockets[self.selected_index()]
    }

    #[inline]
    pub fn selected_stack(&self) -> Option<ItemStack> {
        self.selected_pocket().first_stack()
    }

    /// Remove the stack from one pocket, leaving it empty.
    pub fn take(&mut self, index: usize) -> Option<ItemStack> {
        self.pockets.get_mut(index)?.entries.first_mut()?.take()
    }

    /// Replace a pocket's stack and return whatever was there before.
    pub fn replace(&mut self, index: usize, stack: Option<ItemStack>) -> Option<ItemStack> {
        let entry = self.pockets.get_mut(index)?.entries.first_mut()?;
        std::mem::replace(entry, stack.filter(|stack| stack.count > 0))
    }

    pub fn select(&mut self, index: usize) {
        if index < POCKET_COUNT {
            self.selected = index;
        } else {
            self.clamp_selected();
        }
    }

    pub fn cycle_selection(&mut self, delta: i32) {
        let selected = self.selected_index() as i32;
        self.selected = (selected + delta).rem_euclid(POCKET_COUNT as i32) as usize;
    }

    #[inline]
    fn clamp_selected(&mut self) {
        self.selected = self.selected.min(POCKET_COUNT - 1);
    }

    /// Add items, merging into any existing stack before claiming a new slot.
    /// Empty slots fill from the selected pocket outward. Returns any leftover
    /// that did not fit.
    pub fn add(&mut self, item: ItemId, mut count: u32) -> u32 {
        if count == 0 {
            return 0;
        }

        self.clamp_selected();
        let selected = self.selected_index();
        for offset in 0..POCKET_COUNT {
            let index = (selected + offset) % POCKET_COUNT;
            count = self.pockets[index].merge_existing(item, count);
            if count == 0 {
                return 0;
            }
        }

        for offset in 0..POCKET_COUNT {
            let index = (selected + offset) % POCKET_COUNT;
            count = self.pockets[index].add(item, count);
            if count == 0 {
                break;
            }
        }
        count
    }

    /// Consume one stack from the selected pocket. Placement uses this to pay
    /// for a single block.
    pub fn consume_selected(&mut self, count: u32) -> bool {
        self.clamp_selected();
        let selected = self.selected_index();
        self.pockets[selected].consume_first(count)
    }

    pub fn clear(&mut self) {
        for pocket in &mut self.pockets {
            pocket.clear();
        }
        self.selected = 0;
    }

    /// Selected slot plus every occupied pocket, in slot order. Used by the
    /// save file so a restored hotbar keeps its layout.
    pub fn snapshot(&self) -> (usize, Vec<Option<ItemStack>>) {
        (
            self.selected(),
            self.pockets
                .iter()
                .map(|pocket| pocket.first_stack())
                .collect(),
        )
    }

    /// Validate and replace the hotbar from a snapshot. Invalid or impossible
    /// inventory states are rejected so malformed saves cannot silently reshape
    /// player data.
    pub fn restore(
        &mut self,
        selected: usize,
        entries: &[Option<ItemStack>],
    ) -> Result<(), String> {
        Self::validate_restore(selected, entries)?;
        self.clear();
        self.selected = selected;
        for (index, stack) in entries.iter().take(POCKET_COUNT).enumerate() {
            if let Some(stack) = stack {
                let _ = self.pockets[index].add(stack.item, stack.count);
            }
        }
        Ok(())
    }

    pub fn validate_restore(selected: usize, entries: &[Option<ItemStack>]) -> Result<(), String> {
        if selected >= POCKET_COUNT {
            return Err(format!(
                "selected pocket {selected} out of range (0..{POCKET_COUNT})"
            ));
        }
        if entries.len() != POCKET_COUNT {
            return Err(format!(
                "invalid pocket slot count: {} (expected {POCKET_COUNT})",
                entries.len()
            ));
        }
        for (index, stack) in entries.iter().enumerate() {
            let Some(stack) = stack else { continue };
            let max_stack = stack.item.def().max_stack.max(1);
            if stack.count == 0 {
                return Err(format!("pocket {index}: item count must be >= 1"));
            }
            if stack.count > max_stack {
                return Err(format!(
                    "pocket {index}: {} count {} exceeds max stack {max_stack}",
                    stack.item.display_name(),
                    stack.count
                ));
            }
        }
        Ok(())
    }

    pub fn count(&self, item: ItemId) -> u32 {
        self.pockets
            .iter()
            .flat_map(|pocket| pocket.entries().iter().flatten())
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum()
    }

    pub fn total_items(&self) -> u32 {
        self.pockets.iter().map(Pocket::total_items).sum()
    }
}

pub struct PocketsPlugin;

impl Plugin for PocketsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Pockets>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_eight_empty_pockets() {
        let pockets = Pockets::default();
        assert_eq!(pockets.pockets().len(), POCKET_COUNT);
        assert!(pockets.pockets().iter().all(Pocket::is_empty));
        assert_eq!(pockets.selected(), 0);
    }

    #[test]
    fn each_pocket_holds_one_item_stack() {
        let mut pockets = Pockets::default();
        for item in [
            ItemId::Granite,
            ItemId::Topsoil,
            ItemId::Sand,
            ItemId::Wood,
            ItemId::Turf,
        ] {
            pockets.add(item, 1);
        }

        let first = &pockets.pockets()[0];
        assert_eq!(first.entries().iter().flatten().count(), POCKET_CAPACITY);
        assert!(first.entries().iter().all(Option::is_some));
        // The fifth distinct item spills into the next empty pocket.
        assert_eq!(
            pockets.pockets()[4].first_stack().unwrap().item,
            ItemId::Turf
        );
    }

    #[test]
    fn identical_items_merge_up_to_the_stack_limit_then_spill() {
        let mut pockets = Pockets::default();
        let stack = ItemId::Granite.def().max_stack;
        assert_eq!(pockets.add(ItemId::Granite, stack + 6), 0);
        assert_eq!(pockets.count(ItemId::Granite), stack + 6);
        assert_eq!(pockets.pockets()[0].entries()[0].unwrap().count, stack);
        assert_eq!(pockets.pockets()[1].entries()[0].unwrap().count, 6);
    }

    #[test]
    fn pickups_merge_into_existing_stacks_outside_the_selected_pocket() {
        let mut pockets = Pockets::default();
        pockets.replace(
            1,
            Some(ItemStack {
                item: ItemId::GranitePebble,
                count: 2,
            }),
        );
        pockets.select(2);

        assert_eq!(pockets.add(ItemId::GranitePebble, 3), 0);
        assert_eq!(pockets.pocket(1).unwrap().first_stack().unwrap().count, 5);
        assert!(pockets.pocket(2).unwrap().is_empty());
    }

    #[test]
    fn pickups_spill_into_the_selected_pocket_only_after_merging() {
        let mut pockets = Pockets::default();
        let max_stack = ItemId::GranitePebble.def().max_stack;
        pockets.replace(
            1,
            Some(ItemStack {
                item: ItemId::GranitePebble,
                count: max_stack - 1,
            }),
        );
        pockets.select(2);

        assert_eq!(pockets.add(ItemId::GranitePebble, 3), 0);
        assert_eq!(
            pockets.pocket(1).unwrap().first_stack().unwrap().count,
            max_stack
        );
        assert_eq!(pockets.pocket(2).unwrap().first_stack().unwrap().count, 2);
    }

    #[test]
    fn merging_never_claims_an_empty_slot() {
        let mut pocket = Pocket::new();
        assert_eq!(pocket.merge_existing(ItemId::GranitePebble, 1), 1);
        assert!(pocket.is_empty());
    }

    #[test]
    fn selection_wraps_and_consume_uses_the_selected_pocket() {
        let mut pockets = Pockets::default();
        pockets.select(POCKET_COUNT - 1);
        pockets.add(ItemId::Wood, 2);
        assert_eq!(pockets.selected_stack().unwrap().item, ItemId::Wood);
        assert!(pockets.consume_selected(1));
        assert_eq!(pockets.selected_stack().unwrap().count, 1);
        pockets.cycle_selection(1);
        assert_eq!(pockets.selected(), 0);
        pockets.cycle_selection(-1);
        assert_eq!(pockets.selected(), POCKET_COUNT - 1);
    }

    #[test]
    fn pockets_report_overflow() {
        let mut pockets = Pockets::default();
        let capacity = POCKET_COUNT * POCKET_CAPACITY * ItemId::Granite.def().max_stack as usize;
        assert_eq!(pockets.add(ItemId::Granite, capacity as u32 + 5), 5);
    }

    #[test]
    fn invalid_selected_index_is_clamped_on_use() {
        let mut pockets = Pockets::default();
        pockets.replace(
            7,
            Some(ItemStack {
                item: ItemId::Wood,
                count: 2,
            }),
        );
        // Simulate corrupted/internal invariant breakage.
        pockets.selected = 99;
        assert_eq!(pockets.selected(), POCKET_COUNT - 1);
        assert_eq!(pockets.selected_stack().unwrap().item, ItemId::Wood);
        assert!(pockets.consume_selected(1));
        assert_eq!(pockets.pocket(7).unwrap().first_stack().unwrap().count, 1);
        // Operations should keep selection valid.
        pockets.cycle_selection(1);
        assert_eq!(pockets.selected(), 0);
    }
}
