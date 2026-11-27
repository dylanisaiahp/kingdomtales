//! Character progression, currency and survival attributes.
//!
//! This is the authoritative gameplay state behind the HUD. Level/XP, coins,
//! perk points and the current/maximum survival values live here; the UI only
//! reads them. All balance numbers are kept in small tables or named constants
//! so they can be retuned without touching gameplay systems.

use bevy::prelude::*;

use crate::items::ItemId;
use crate::player::Stamina;
use crate::rewards::pickup_reward;
use crate::ui_manager::UiManager;

/// Full sprint stamina at level 1. The existing stamina drain keeps a full bar
/// at roughly 12.5 seconds of sprinting.
pub const BASE_STAMINA: f32 = 100.0;
pub const BASE_HUNGER: f32 = 100.0;
pub const BASE_HYDRATION: f32 = 100.0;

/// XP required for level `level` is `100 + 160 * (level - 1)`. Level 6 therefore
/// needs a 900 XP bar, matching the design example.
const XP_BASE: u32 = 100;
const XP_STEP: u32 = 160;

/// Perk gain by level band. The last band continues at +12 beyond level 72 so
/// late-game points never silently become worthless.
static PERK_BANDS: [(u32, u32); 6] = [(12, 2), (24, 4), (36, 6), (48, 8), (60, 10), (u32::MAX, 12)];

/// Hunger/hydration loss per second while walking. Sprinting multiplies both.
const HUNGER_DRAIN_PER_SEC: f32 = 0.18;
const HYDRATION_DRAIN_PER_SEC: f32 = 0.26;
const SPRINT_DRAIN_MULTIPLIER: f32 = 1.7;
/// Ignore huge frame gaps (alt-tab, debugger breakpoints) so one stalled frame
/// cannot empty both bars.
const MAX_DRAIN_STEP: f32 = 0.10;

/// A survival attribute that can be improved with perk points.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Attribute {
    Stamina,
    Hunger,
    Hydration,
}

impl Attribute {
    pub const ALL: [Attribute; 3] = [Attribute::Stamina, Attribute::Hunger, Attribute::Hydration];

    pub fn label(self) -> &'static str {
        match self {
            Attribute::Stamina => "Stamina",
            Attribute::Hunger => "Hunger",
            Attribute::Hydration => "Hydration",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Attribute::Stamina => "STA",
            Attribute::Hunger => "FOD",
            Attribute::Hydration => "H2O",
        }
    }
}

/// XP needed to advance from `level` to the next one.
pub fn xp_to_next(level: u32) -> u32 {
    let level = level.max(1);
    XP_BASE.saturating_add(XP_STEP.saturating_mul(level - 1))
}

/// Attribute increase granted by one perk point at `level`.
pub fn perk_gain(level: u32) -> u32 {
    for (max_level, gain) in PERK_BANDS {
        if level <= max_level {
            return gain;
        }
    }
    // PERK_BANDS covers all u32 levels (final band is u32::MAX).
    PERK_BANDS
        .last()
        .map(|(_, gain)| *gain)
        .unwrap_or(PERK_BANDS[0].1)
}

/// Serializable subset of progression. Old v2 saves that lack these records
/// simply keep the default level-1 state.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressionSnapshot {
    pub level: u32,
    pub xp: u32,
    pub coins: u32,
    pub perk_points: u32,
    pub stamina_bonus: u32,
    pub hunger_bonus: u32,
    pub hydration_bonus: u32,
    pub hunger: f32,
    pub hydration: f32,
}

impl Default for ProgressionSnapshot {
    fn default() -> Self {
        Self {
            level: 1,
            xp: 0,
            coins: 0,
            perk_points: 0,
            stamina_bonus: 0,
            hunger_bonus: 0,
            hydration_bonus: 0,
            hunger: BASE_HUNGER,
            hydration: BASE_HYDRATION,
        }
    }
}

/// Authoritative character state. Kept as a resource so every HUD element reads
/// the same values and no display-only copies are needed.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Progression {
    level: u32,
    xp: u32,
    coins: u32,
    perk_points: u32,
    stamina_bonus: u32,
    hunger_bonus: u32,
    hydration_bonus: u32,
    hunger: f32,
    hydration: f32,
}

impl Default for Progression {
    fn default() -> Self {
        Self::from_snapshot(ProgressionSnapshot::default())
    }
}

impl Progression {
    pub fn level(&self) -> u32 {
        self.level
    }

    pub fn xp(&self) -> u32 {
        self.xp
    }

    pub fn xp_required(&self) -> u32 {
        xp_to_next(self.level)
    }

    pub fn xp_fraction(&self) -> f32 {
        let required = self.xp_required();
        if required == 0 {
            0.0
        } else {
            (self.xp as f32 / required as f32).clamp(0.0, 1.0)
        }
    }

    pub fn coins(&self) -> u32 {
        self.coins
    }

    pub fn perk_points(&self) -> u32 {
        self.perk_points
    }

    pub fn perk_gain(&self) -> u32 {
        perk_gain(self.level)
    }

    pub fn bonus(&self, attribute: Attribute) -> u32 {
        match attribute {
            Attribute::Stamina => self.stamina_bonus,
            Attribute::Hunger => self.hunger_bonus,
            Attribute::Hydration => self.hydration_bonus,
        }
    }

    pub fn max(&self, attribute: Attribute) -> f32 {
        let level_gain = self.level.saturating_sub(1) as f32;
        match attribute {
            Attribute::Stamina => BASE_STAMINA + level_gain + self.stamina_bonus as f32,
            Attribute::Hunger => BASE_HUNGER + level_gain + self.hunger_bonus as f32,
            Attribute::Hydration => BASE_HYDRATION + level_gain + self.hydration_bonus as f32,
        }
    }

    pub fn max_stamina(&self) -> f32 {
        self.max(Attribute::Stamina)
    }

    pub fn hunger(&self) -> f32 {
        self.hunger
    }

    pub fn hydration(&self) -> f32 {
        self.hydration
    }

    pub fn hunger_fraction(&self) -> f32 {
        fraction(self.hunger, self.max(Attribute::Hunger))
    }

    pub fn hydration_fraction(&self) -> f32 {
        fraction(self.hydration, self.max(Attribute::Hydration))
    }

    /// Grant XP, applying every earned level. Each level automatically adds one
    /// point to all three maxima and one unspent perk point.
    pub fn award_xp(&mut self, amount: u32) {
        self.xp = self.xp.saturating_add(amount);
        while self.xp >= self.xp_required() {
            self.xp -= self.xp_required();
            self.level = self.level.saturating_add(1);
            self.perk_points = self.perk_points.saturating_add(1);
            // Fill the newly added capacity so the bars do not look damaged
            // immediately after a level-up.
            self.hunger += 1.0;
            self.hydration += 1.0;
        }
        self.clamp_current();
    }

    pub fn award_coins(&mut self, amount: u32) {
        self.coins = self.coins.saturating_add(amount);
    }

    /// Award the data-driven XP and coin value of a collected item.
    pub fn award_pickup(&mut self, item: ItemId, count: u32) {
        let reward = pickup_reward(item, count);
        if reward.is_empty() {
            return;
        }
        self.award_xp(reward.xp);
        self.award_coins(reward.coins);
    }

    /// Spend one perk point on `attribute`. Returns false when no point is
    /// available, so the UI can disable the button without guessing.
    pub fn spend_perk(&mut self, attribute: Attribute) -> bool {
        if self.perk_points == 0 {
            return false;
        }
        let gain = self.perk_gain() as f32;
        self.perk_points -= 1;
        match attribute {
            Attribute::Stamina => self.stamina_bonus += gain as u32,
            Attribute::Hunger => {
                self.hunger_bonus += gain as u32;
                self.hunger += gain;
            }
            Attribute::Hydration => {
                self.hydration_bonus += gain as u32;
                self.hydration += gain;
            }
        }
        self.clamp_current();
        true
    }

    /// Drain survival values while gameplay is active. Overlays pause drain so
    /// reading the backpack or settings menu cannot cost resources.
    pub fn tick(&mut self, dt: f32, sprinting: bool, gameplay_active: bool) {
        if !gameplay_active {
            return;
        }
        let dt = dt.clamp(0.0, MAX_DRAIN_STEP);
        let multiplier = if sprinting {
            SPRINT_DRAIN_MULTIPLIER
        } else {
            1.0
        };
        self.hunger = (self.hunger - HUNGER_DRAIN_PER_SEC * multiplier * dt).max(0.0);
        self.hydration = (self.hydration - HYDRATION_DRAIN_PER_SEC * multiplier * dt).max(0.0);
    }

    pub fn snapshot(&self) -> ProgressionSnapshot {
        ProgressionSnapshot {
            level: self.level,
            xp: self.xp,
            coins: self.coins,
            perk_points: self.perk_points,
            stamina_bonus: self.stamina_bonus,
            hunger_bonus: self.hunger_bonus,
            hydration_bonus: self.hydration_bonus,
            hunger: self.hunger,
            hydration: self.hydration,
        }
    }

    pub fn from_snapshot(snapshot: ProgressionSnapshot) -> Self {
        let sanitize_u32 = |value: u32| value;
        let sanitize_f32 = |value: f32, fallback: f32| {
            if value.is_finite() {
                value.max(0.0)
            } else {
                fallback
            }
        };

        let level = snapshot.level.max(1);
        let mut result = Self {
            level,
            xp: sanitize_u32(snapshot.xp),
            coins: sanitize_u32(snapshot.coins),
            perk_points: sanitize_u32(snapshot.perk_points),
            stamina_bonus: sanitize_u32(snapshot.stamina_bonus),
            hunger_bonus: sanitize_u32(snapshot.hunger_bonus),
            hydration_bonus: sanitize_u32(snapshot.hydration_bonus),
            hunger: sanitize_f32(snapshot.hunger, 0.0),
            hydration: sanitize_f32(snapshot.hydration, 0.0),
        };
        // XP should not be at/above the current level requirement after restore.
        let required = xp_to_next(result.level);
        if required > 0 {
            result.xp = result.xp.min(required.saturating_sub(1));
        }
        result.clamp_current();
        result
    }

    fn clamp_current(&mut self) {
        self.hunger = self.hunger.clamp(0.0, self.max(Attribute::Hunger));
        self.hydration = self.hydration.clamp(0.0, self.max(Attribute::Hydration));
    }
}

#[inline]
fn fraction(current: f32, max: f32) -> f32 {
    if max <= 0.0 {
        0.0
    } else {
        (current / max).clamp(0.0, 1.0)
    }
}

pub struct ProgressionPlugin;

impl Plugin for ProgressionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Progression>()
            .add_systems(Update, (tick_survival, sync_stamina).chain());
    }
}

fn tick_survival(
    time: Res<Time>,
    ui: Res<UiManager>,
    stamina: Res<Stamina>,
    mut progression: ResMut<Progression>,
) {
    progression.tick(time.delta_secs(), stamina.sprinting, !ui.any_open());
}

/// Keep the movement stamina resource in step with progression maxima. A level
/// or stamina perk fills only the newly added slice, never refills spent stamina.
fn sync_stamina(progression: Res<Progression>, mut stamina: ResMut<Stamina>) {
    let desired = progression.max_stamina();
    let mut current = stamina.current;
    if desired > stamina.max {
        current += desired - stamina.max;
    }
    current = current.clamp(0.0, desired);
    if stamina.max != desired {
        stamina.max = desired;
    }
    if stamina.current != current {
        stamina.current = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_curve_matches_the_design_example() {
        assert_eq!(xp_to_next(1), 100);
        assert_eq!(xp_to_next(6), 900);
        assert!(xp_to_next(12) > xp_to_next(6));
    }

    #[test]
    fn perk_gain_grows_in_bands() {
        assert_eq!(perk_gain(1), 2);
        assert_eq!(perk_gain(12), 2);
        assert_eq!(perk_gain(13), 4);
        assert_eq!(perk_gain(37), 8);
        assert_eq!(perk_gain(61), 12);
        assert_eq!(perk_gain(90), 12);
    }

    #[test]
    fn level_up_awards_all_automatic_rewards() {
        let mut progression = Progression::default();
        let required = progression.xp_required();
        progression.award_xp(required);

        assert_eq!(progression.level(), 2);
        assert_eq!(progression.xp(), 0);
        assert_eq!(progression.perk_points(), 1);
        assert_eq!(progression.max_stamina(), BASE_STAMINA + 1.0);
        assert_eq!(progression.max(Attribute::Hunger), BASE_HUNGER + 1.0);
        assert_eq!(progression.max(Attribute::Hydration), BASE_HYDRATION + 1.0);
    }

    #[test]
    fn multiple_levels_can_complete_from_one_award() {
        let mut progression = Progression::default();
        let total = xp_to_next(1) + xp_to_next(2) + 5;
        progression.award_xp(total);

        assert_eq!(progression.level(), 3);
        assert_eq!(progression.xp(), 5);
        assert_eq!(progression.perk_points(), 2);
    }

    #[test]
    fn perk_points_are_required_and_scale_with_level() {
        let mut progression = Progression::default();
        assert!(!progression.spend_perk(Attribute::Stamina));

        progression.award_xp(xp_to_next(1));
        assert!(progression.spend_perk(Attribute::Stamina));
        assert_eq!(progression.perk_points(), 0);
        assert_eq!(progression.max_stamina(), BASE_STAMINA + 3.0);

        while progression.level() < 13 {
            progression.award_xp(progression.xp_required());
        }
        assert_eq!(progression.perk_gain(), 4);
        let before = progression.max(Attribute::Hydration);
        assert!(progression.spend_perk(Attribute::Hydration));
        assert_eq!(progression.max(Attribute::Hydration), before + 4.0);
    }

    #[test]
    fn pickups_award_data_driven_progress() {
        let mut progression = Progression::default();
        progression.award_pickup(ItemId::IronOre, 4);

        assert_eq!(progression.xp(), 20);
        assert_eq!(progression.coins(), 12);
    }

    #[test]
    fn survival_values_drain_only_during_gameplay() {
        let mut progression = Progression::default();
        progression.tick(1.0, false, false);
        assert_eq!(progression.hunger(), BASE_HUNGER);

        progression.tick(0.5, false, true);
        assert!(progression.hunger() < BASE_HUNGER);
        assert!(progression.hydration() < BASE_HYDRATION);

        let before = progression.hydration();
        progression.tick(0.5, true, true);
        assert!(progression.hydration() < before);
    }

    #[test]
    fn snapshots_round_trip_and_repair_old_saves() {
        let mut progression = Progression::default();
        progression.award_xp(xp_to_next(1) + 4);
        progression.award_coins(37);
        progression.spend_perk(Attribute::Hunger);
        progression.tick(0.5, false, true);

        let restored = Progression::from_snapshot(progression.snapshot());
        assert_eq!(restored, progression);

        let legacy = Progression::from_snapshot(ProgressionSnapshot {
            level: 0,
            hunger: -4.0,
            hydration: 9999.0,
            ..default()
        });
        assert_eq!(legacy.level(), 1);
        assert_eq!(legacy.hunger(), 0.0);
        assert_eq!(legacy.hydration(), BASE_HYDRATION);
    }

    #[test]
    fn from_snapshot_sanitizes_malformed_progression_values() {
        let restored = Progression::from_snapshot(ProgressionSnapshot {
            level: 3,
            xp: 9999,
            coins: u32::MAX,
            perk_points: 7,
            stamina_bonus: 5,
            hunger_bonus: 6,
            hydration_bonus: 7,
            hunger: f32::NAN,
            hydration: f32::NEG_INFINITY,
        });

        assert_eq!(restored.level(), 3);
        assert!(restored.xp() < restored.xp_required());
        assert_eq!(restored.coins(), u32::MAX);
        assert_eq!(restored.perk_points(), 7);
        assert_eq!(restored.bonus(Attribute::Stamina), 5);
        assert!(restored.hunger().is_finite());
        assert!(restored.hydration().is_finite());
        assert_eq!(restored.hunger(), 0.0);
        assert_eq!(restored.hydration(), 0.0);

        let finite_negative = Progression::from_snapshot(ProgressionSnapshot {
            level: 1,
            xp: 0,
            coins: 0,
            perk_points: 0,
            stamina_bonus: 0,
            hunger_bonus: 0,
            hydration_bonus: 0,
            hunger: -10.0,
            hydration: -1.0,
        });
        assert_eq!(finite_negative.hunger(), 0.0);
        assert_eq!(finite_negative.hydration(), 0.0);
    }
}
