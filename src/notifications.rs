//! Transient HUD notifications.
//!
//! Gameplay systems push events here; the HUD only renders what it finds. The
//! feed merges rapid repeats, caps its length and owns all timing so the UI
//! layer never needs timers of its own.

use bevy::prelude::*;

use crate::items::ItemId;
use crate::rewards::pickup_reward;

const LIFETIME: f32 = 4.2;
const FADE_IN: f32 = 0.18;
const FADE_OUT: f32 = 0.55;
const COMBINE_WINDOW: f32 = 1.6;
const MAX_NOTIFICATIONS: usize = 5;
/// Anything worth at least this much XP or coin gets a brighter frame.
const RARE_XP: u32 = 5;
const RARE_COINS: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotificationTone {
    Normal,
    Rare,
    Info,
    Success,
    Error,
}

impl NotificationTone {
    pub const ALL: [NotificationTone; 5] = [
        NotificationTone::Normal,
        NotificationTone::Rare,
        NotificationTone::Info,
        NotificationTone::Success,
        NotificationTone::Error,
    ];

    #[inline]
    pub fn index(self) -> usize {
        match self {
            NotificationTone::Normal => 0,
            NotificationTone::Rare => 1,
            NotificationTone::Info => 2,
            NotificationTone::Success => 3,
            NotificationTone::Error => 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub title: String,
    pub detail: String,
    pub item: Option<ItemId>,
    pub tone: NotificationTone,
    age: f32,
}

impl Notification {
    pub fn alpha(&self) -> f32 {
        if self.age < FADE_IN {
            return (self.age / FADE_IN).clamp(0.0, 1.0);
        }
        let remaining = LIFETIME - self.age;
        if remaining < FADE_OUT {
            return (remaining / FADE_OUT).clamp(0.0, 1.0);
        }
        1.0
    }

    /// Small horizontal slide-in and drift-out, in logical pixels.
    pub fn offset_x(&self) -> f32 {
        if self.age < FADE_IN {
            14.0 * (1.0 - self.age / FADE_IN)
        } else {
            -2.0 * (self.age / LIFETIME).min(1.0)
        }
    }

    pub fn expired(&self) -> bool {
        self.age >= LIFETIME
    }
}

#[derive(Resource, Default)]
pub struct NotificationFeed {
    /// Newest first, so the first row is drawn above older ones.
    notifications: Vec<Notification>,
}

impl NotificationFeed {
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Push an item pickup, merging it into the newest matching row when the
    /// pickup happens soon enough to feel like one action.
    pub fn push_item(&mut self, item: ItemId, count: u32) {
        if count == 0 {
            return;
        }

        if let Some(newest) = self.notifications.first_mut()
            && newest.item == Some(item)
            && newest.age <= COMBINE_WINDOW
        {
            let current = newest
                .detail
                .trim_start_matches('+')
                .parse::<u32>()
                .unwrap_or(0);
            let total = current.saturating_add(count);
            newest.detail = format!("+{total}");
            newest.tone = item_tone(item, total);
            newest.age = 0.0;
            return;
        }

        self.insert(Notification {
            title: item.display_name().to_string(),
            detail: format!("+{count}"),
            item: Some(item),
            tone: item_tone(item, count),
            age: 0.0,
        });
    }

    pub fn push_status(&mut self, text: impl Into<String>, tone: NotificationTone) {
        self.insert(Notification {
            title: text.into(),
            detail: String::new(),
            item: None,
            tone,
            age: 0.0,
        });
    }

    pub fn update(&mut self, dt: f32) {
        for notification in &mut self.notifications {
            notification.age += dt;
        }
        self.notifications
            .retain(|notification| !notification.expired());
    }

    fn insert(&mut self, notification: Notification) {
        self.notifications.insert(0, notification);
        self.notifications.truncate(MAX_NOTIFICATIONS);
    }
}

fn item_tone(item: ItemId, count: u32) -> NotificationTone {
    let reward = pickup_reward(item, count);
    if reward.xp >= RARE_XP || reward.coins >= RARE_COINS {
        NotificationTone::Rare
    } else {
        NotificationTone::Normal
    }
}

pub struct NotificationPlugin;

impl Plugin for NotificationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NotificationFeed>()
            .add_systems(Update, tick_notifications);
    }
}

fn tick_notifications(time: Res<Time>, mut feed: ResMut<NotificationFeed>) {
    feed.update(time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_item_pickups_combine_into_one_row() {
        let mut feed = NotificationFeed::default();
        feed.push_item(ItemId::Wood, 2);
        feed.push_item(ItemId::Wood, 3);

        assert_eq!(feed.notifications().len(), 1);
        assert_eq!(feed.notifications()[0].detail, "+5");
    }

    #[test]
    fn a_different_item_creates_a_new_row_above_the_old_one() {
        let mut feed = NotificationFeed::default();
        feed.push_item(ItemId::Wood, 2);
        feed.push_item(ItemId::Berry, 1);

        assert_eq!(feed.notifications().len(), 2);
        assert_eq!(feed.notifications()[0].title, "Berry");
        assert_eq!(feed.notifications()[1].title, "Wood");
    }

    #[test]
    fn rare_pickups_get_a_distinct_tone() {
        let mut feed = NotificationFeed::default();
        feed.push_item(ItemId::Turf, 1);
        feed.push_item(ItemId::IronOre, 1);

        assert_eq!(feed.notifications()[1].tone, NotificationTone::Normal);
        assert_eq!(feed.notifications()[0].tone, NotificationTone::Rare);
    }

    #[test]
    fn notifications_fade_and_expire() {
        let mut feed = NotificationFeed::default();
        feed.push_item(ItemId::Wood, 1);
        assert!(feed.notifications()[0].alpha() < 1.0);

        feed.update(1.0);
        assert_eq!(feed.notifications()[0].alpha(), 1.0);

        feed.update(LIFETIME);
        assert!(feed.notifications().is_empty());
    }

    #[test]
    fn feed_is_capped() {
        let mut feed = NotificationFeed::default();
        for _ in 0..12 {
            feed.push_status("save", NotificationTone::Info);
        }
        assert_eq!(feed.notifications().len(), MAX_NOTIFICATIONS);
    }
}
