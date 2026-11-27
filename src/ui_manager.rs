//! Centralised overlay state.
//!
//! Every modal panel (pause menu, settings, chat, map, hammer shape menu,
//! backpack and `/give` picker) goes through one [`UiManager`]. Systems ask the
//! manager whether an overlay is open instead of stitching together a chain of
//! `menu.open || chat.open || ...` checks. Only one overlay can be active at a
//! time, which also makes Escape and cursor handling deterministic.

use bevy::prelude::*;

/// The modal panels that can take over input and release the mouse cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    Menu,
    /// Tabbed settings screen opened from the pause menu.
    Settings,
    Chat,
    ShapeMenu,
    Backpack,
    /// Creative item picker opened by `/give` with no args.
    GiveMenu,
    /// Overhead map with Live 3D and Explore 2D modes; explored cells persist
    /// via save records.
    Map,
}

/// Authoritative overlay state. Opening an overlay replaces the current one;
/// closing only clears the overlay if it is still the active one.
#[derive(Resource, Default, Debug)]
pub struct UiManager {
    active: Option<Overlay>,
}

impl UiManager {
    pub fn open(&mut self, overlay: Overlay) {
        self.active = Some(overlay);
    }

    pub fn close(&mut self, overlay: Overlay) {
        if self.active == Some(overlay) {
            self.active = None;
        }
    }

    pub fn is_open(&self, overlay: Overlay) -> bool {
        self.active == Some(overlay)
    }

    pub fn any_open(&self) -> bool {
        self.active.is_some()
    }

    pub fn active(&self) -> Option<Overlay> {
        self.active
    }
}

pub struct UiManagerPlugin;

impl Plugin for UiManagerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiManager>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_an_overlay_replaces_the_active_one() {
        let mut ui = UiManager::default();
        assert!(!ui.any_open());

        ui.open(Overlay::Menu);
        assert!(ui.is_open(Overlay::Menu));
        assert!(ui.any_open());

        ui.open(Overlay::Chat);
        assert!(!ui.is_open(Overlay::Menu));
        assert!(ui.is_open(Overlay::Chat));

        ui.open(Overlay::Settings);
        assert!(!ui.is_open(Overlay::Chat));
        assert!(ui.is_open(Overlay::Settings));
    }

    #[test]
    fn closing_a_non_active_overlay_is_a_no_op() {
        let mut ui = UiManager::default();
        ui.open(Overlay::Backpack);
        ui.close(Overlay::Menu);
        assert!(ui.is_open(Overlay::Backpack));

        ui.close(Overlay::Backpack);
        assert!(!ui.any_open());
    }
}
