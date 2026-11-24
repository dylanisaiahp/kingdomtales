//! In-game screenshots.
//!
//! F12 captures the rendered window and writes a PNG into a `screenshots`
//! directory beside the game. Capture is asynchronous, so the log records the
//! destination and Bevy saves the image when the frame is ready.

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::log::{info, warn};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};

use crate::notifications::{NotificationFeed, NotificationTone};
use crate::settings::InputBindings;
use crate::ui_manager::UiManager;

const SCREENSHOT_DIR: &str = "screenshots";

/// Build a timestamped PNG path so repeated captures never overwrite each
/// other. A missing clock still produces a usable name.
fn screenshot_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    PathBuf::from(SCREENSHOT_DIR).join(format!("kingdom-{millis}.png"))
}

fn take_screenshot(
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<InputBindings>,
    ui: Res<UiManager>,
    mut commands: Commands,
    mut feed: ResMut<NotificationFeed>,
) {
    if ui.any_open() || !keys.just_pressed(bindings.screenshot) {
        return;
    }

    if let Err(error) = fs::create_dir_all(SCREENSHOT_DIR) {
        warn!("Cannot create {SCREENSHOT_DIR}: {error}");
        return;
    }

    let path = screenshot_path();
    info!("Saving screenshot to {}", path.display());
    feed.push_status("Screenshot requested", NotificationTone::Info);
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}

pub struct ScreenshotPlugin;

impl Plugin for ScreenshotPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, take_screenshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_path_is_inside_the_screenshot_directory() {
        let path = screenshot_path();
        assert_eq!(path.parent(), Some(std::path::Path::new(SCREENSHOT_DIR)));
        assert_eq!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("png")
        );
        assert!(
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap()
                .starts_with("kingdom-")
        );
    }
}
