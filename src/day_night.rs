//! In-game day/night cycle.
//!
//! A single authoritative resource drives sun/moon motion, sky colors,
//! environmental lighting, and HUD time display. Timing is configurable
//! through [`DayNightCycle::day_length_seconds`]; no other system should
//! hardcode day duration.

use bevy::prelude::*;

pub const TICKS_PER_DAY: u32 = 24_000;
pub const DEFAULT_DAY_LENGTH_SECONDS: f32 = 20.0 * 60.0;

/// Normalized time: 0.0 = midnight, 0.25 = sunrise, 0.5 = noon, 0.75 = sunset.
/// Minecraft-style tick mapping: 0=sunrise, 6000=noon, 12000=sunset, 18000=midnight.
#[derive(Resource, Clone, Debug)]
pub struct DayNightCycle {
    /// Normalized time of day in `[0.0, 1.0)`.
    pub time_of_day: f32,
    /// Full day/night cycle length in real seconds at speed 1.0.
    pub day_length_seconds: f32,
    /// Number of completed in-game days.
    pub day_count: u32,
    /// Whether the day/night cycle advances automatically.
    pub cycle_enabled: bool,
    /// Multiplier for day/night progression only. 0 freezes time.
    pub time_scale: f32,
    /// Total game time elapsed in ticks (advances with day/night progression).
    pub gametime_ticks: u64,
}

impl Default for DayNightCycle {
    fn default() -> Self {
        Self {
            // Start shortly after sunrise for a pleasant first impression.
            time_of_day: 0.30,
            day_length_seconds: DEFAULT_DAY_LENGTH_SECONDS,
            day_count: 1,
            cycle_enabled: true,
            time_scale: 1.0,
            gametime_ticks: 0,
        }
    }
}

impl DayNightCycle {
    /// Advance time. Returns true when the day wrapped.
    pub fn advance(&mut self, delta_secs: f32) -> bool {
        let speed = self.effective_speed();
        if speed <= 0.0 || self.day_length_seconds <= 0.0 {
            return false;
        }
        let prev = self.time_of_day.rem_euclid(1.0);
        let delta_days = delta_secs * speed / self.day_length_seconds;
        self.time_of_day = (prev + delta_days).rem_euclid(1.0);
        let ticks_advanced = (delta_days * TICKS_PER_DAY as f32).round().max(0.0) as u64;
        self.gametime_ticks = self.gametime_ticks.saturating_add(ticks_advanced);
        if prev + delta_days >= 1.0 {
            let wraps = (prev + delta_days).floor() as u32;
            self.day_count = self.day_count.saturating_add(wraps.max(1));
            return true;
        }
        false
    }

    pub fn effective_speed(&self) -> f32 {
        if self.cycle_enabled {
            self.time_scale.max(0.0)
        } else {
            0.0
        }
    }

    /// Minecraft daytime ticks: 0=sunrise (06:00), 6000=noon, 12000=sunset,
    /// 18000=midnight. The internal clock starts at midnight, so sunrise is
    /// six hours into the normalized day rather than at zero.
    pub fn daytime_ticks(&self) -> u32 {
        let normalized = self.time_of_day.rem_euclid(1.0);
        ((normalized * TICKS_PER_DAY as f32).round() as u32 + 18_000) % TICKS_PER_DAY
    }

    pub fn set_daytime_ticks(&mut self, ticks: u32) {
        let ticks = ticks % TICKS_PER_DAY;
        // Convert Minecraft ticks (0=sunrise) to internal normalized (0=midnight).
        self.time_of_day =
            ((ticks as f32 + 6_000.0).rem_euclid(TICKS_PER_DAY as f32)) / TICKS_PER_DAY as f32;
    }

    pub fn add_ticks(&mut self, ticks: i64) {
        let day_delta = ticks as f64 / TICKS_PER_DAY as f64;
        let current = self.time_of_day.rem_euclid(1.0) as f64;
        let next = current + day_delta;
        self.time_of_day = next.rem_euclid(1.0) as f32;
        if ticks > 0 {
            self.gametime_ticks = self.gametime_ticks.saturating_add(ticks as u64);
        }
        if next >= 1.0 {
            let wraps = next.floor() as u32;
            self.day_count = self.day_count.saturating_add(wraps.max(1));
        } else if next < 0.0 {
            let wraps = (-next).ceil() as u32;
            self.day_count = self.day_count.saturating_sub(wraps);
        }
    }

    /// Sun angle in radians around world X axis, where 0 is at horizon during
    /// sunrise and pi/2 is overhead at noon.
    pub fn sun_angle(&self) -> f32 {
        self.time_of_day * std::f32::consts::TAU
    }

    /// Directional light points from the sun toward the world.
    ///
    /// Time convention (normalized `time_of_day`):
    /// - 0.00 = midnight, sun below horizon
    /// - 0.25 = sunrise, sun on eastern horizon
    /// - 0.50 = noon, sun high overhead
    /// - 0.75 = sunset, sun on western horizon
    pub fn sun_direction(&self) -> Vec3 {
        let angle = self.sun_angle();
        // Sun position: east(+X) at sunrise, up(+Y) at noon, west(-X) at sunset.
        let sun_pos = Vec3::new(angle.sin(), -angle.cos(), 0.18).normalize();
        -sun_pos
    }

    pub fn moon_direction(&self) -> Vec3 {
        -self.sun_direction()
    }

    /// True while the sun is at or above the horizon. A small tolerance keeps
    /// sunrise/sunset from flickering between icons due to floating-point drift.
    pub fn is_daytime(&self) -> bool {
        self.sun_height() >= -0.01
    }

    /// 0 at night, 1 at full day.
    pub fn daylight_factor(&self) -> f32 {
        smoothstep(-0.12, 0.10, self.sun_height())
    }

    /// 0 at day, 1 at full night. Uses same solar height as daylight.
    pub fn night_factor(&self) -> f32 {
        1.0 - smoothstep(-0.20, 0.02, self.sun_height())
    }

    /// Sun height in normalized direction Y component (-1..1).
    pub fn sun_height(&self) -> f32 {
        (-self.sun_direction()).y
    }

    pub fn moon_height(&self) -> f32 {
        (-self.moon_direction()).y
    }

    /// Warm sunrise/sunset amount.
    pub fn twilight_factor(&self) -> f32 {
        let h = self.sun_height();
        let dawn = 1.0 - ((h - 0.0).abs() / 0.22).clamp(0.0, 1.0);
        let low = smoothstep(-0.18, 0.08, h) * (1.0 - smoothstep(0.08, 0.35, h));
        dawn.max(low).clamp(0.0, 1.0)
    }

    /// Clock string for HUD, 24h-style game time.
    pub fn clock_string(&self) -> String {
        let total_minutes = (self.time_of_day.rem_euclid(1.0) * 24.0 * 60.0).floor() as u32;
        let hours = (total_minutes / 60) % 24;
        let minutes = total_minutes % 60;
        let (display_hour, am_pm) = if hours == 0 {
            (12, "AM")
        } else if hours < 12 {
            (hours, "AM")
        } else if hours == 12 {
            (12, "PM")
        } else {
            (hours - 12, "PM")
        };
        format!("{display_hour:02}:{minutes:02} {am_pm}")
    }

    pub fn day_label(&self) -> String {
        format!("Day {}", self.day_count)
    }
}

pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DayNightCycle>()
            .add_systems(Update, advance_day_night);
    }
}

fn advance_day_night(time: Res<Time>, mut cycle: ResMut<DayNightCycle>) {
    cycle.advance(time.delta_secs());
}

#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(0.0001)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn lerp_color(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minecraft_daytime_ticks_round_trip() {
        for (ticks, normalized) in [
            (0, 0.25),      // sunrise / 06:00
            (6_000, 0.5),   // noon
            (12_000, 0.75), // sunset / 18:00
            (18_000, 0.0),  // midnight
            (13_000, 19_000.0 / TICKS_PER_DAY as f32),
        ] {
            let mut cycle = DayNightCycle::default();
            cycle.set_daytime_ticks(ticks);
            assert!(
                (cycle.time_of_day - normalized).abs() < 1e-6,
                "ticks {ticks} mapped to {}, expected {normalized}",
                cycle.time_of_day
            );
            assert_eq!(
                cycle.daytime_ticks(),
                ticks,
                "ticks {ticks} did not survive a round trip"
            );
        }
    }

    #[test]
    fn named_time_presets_match_their_hours() {
        let mut cycle = DayNightCycle::default();

        cycle.set_daytime_ticks(0);
        assert_eq!(cycle.clock_string(), "06:00 AM");
        assert!(cycle.is_daytime());

        cycle.set_daytime_ticks(6_000);
        assert_eq!(cycle.clock_string(), "12:00 PM");
        assert!(cycle.is_daytime());

        cycle.set_daytime_ticks(12_000);
        assert_eq!(cycle.clock_string(), "06:00 PM");
        assert!(cycle.is_daytime());

        cycle.set_daytime_ticks(13_000);
        assert_eq!(cycle.clock_string(), "07:00 PM");
        assert!(!cycle.is_daytime());

        cycle.set_daytime_ticks(18_000);
        assert_eq!(cycle.clock_string(), "12:00 AM");
        assert!(!cycle.is_daytime());
    }

    #[test]
    fn day_night_advances_and_wraps() {
        let mut cycle = DayNightCycle {
            time_of_day: 0.99,
            day_length_seconds: 100.0,
            ..Default::default()
        };
        let wrapped = cycle.advance(2.0);
        assert!(wrapped);
        assert_eq!(cycle.day_count, 2);
        assert!(cycle.time_of_day >= 0.0 && cycle.time_of_day < 1.0);
    }

    #[test]
    fn clock_formats_reasonably() {
        let mut cycle = DayNightCycle {
            time_of_day: 0.0,
            ..Default::default()
        };
        assert_eq!(cycle.clock_string(), "12:00 AM");
        cycle.time_of_day = 0.5;
        assert_eq!(cycle.clock_string(), "12:00 PM");
        cycle.time_of_day = 0.75;
        assert_eq!(cycle.clock_string(), "06:00 PM");
    }

    #[test]
    fn is_daytime_follows_the_sun_horizon() {
        let mut cycle = DayNightCycle {
            time_of_day: 0.25, // sunrise
            ..Default::default()
        };
        assert!(cycle.is_daytime(), "sunrise counts as day");

        cycle.time_of_day = 0.5; // noon
        assert!(cycle.is_daytime());

        cycle.time_of_day = 0.75; // sunset
        assert!(cycle.is_daytime(), "sunset counts as day");

        cycle.time_of_day = 0.8; // after sunset
        assert!(!cycle.is_daytime());

        cycle.time_of_day = 0.0; // midnight
        assert!(!cycle.is_daytime());
    }

    #[test]
    fn celestial_positions_match_time_convention() {
        let mut cycle = DayNightCycle {
            time_of_day: 0.25, // sunrise
            ..Default::default()
        };
        let sun = -cycle.sun_direction();
        assert!(sun.x > 0.5, "sunrise should be in east (+X), got {sun:?}");
        assert!(
            sun.y.abs() < 0.2,
            "sunrise should be near horizon, got {sun:?}"
        );

        cycle.time_of_day = 0.5; // noon
        let sun = -cycle.sun_direction();
        assert!(sun.y > 0.8, "noon sun should be high, got {sun:?}");

        cycle.time_of_day = 0.75; // sunset
        let sun = -cycle.sun_direction();
        assert!(sun.x < -0.5, "sunset should be in west (-X), got {sun:?}");
        assert!(
            sun.y.abs() < 0.2,
            "sunset should be near horizon, got {sun:?}"
        );

        cycle.time_of_day = 0.0; // midnight
        let sun = -cycle.sun_direction();
        assert!(
            sun.y < -0.5,
            "midnight sun should be below horizon, got {sun:?}"
        );
        let moon = -cycle.moon_direction();
        assert!(
            moon.y > 0.5,
            "midnight moon should be above horizon, got {moon:?}"
        );
    }
}
