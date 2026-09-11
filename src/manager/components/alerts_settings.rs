use eframe::egui;

use crate::common::ipc::{GamelogDiagnostic, GamelogStatus};
use crate::config::profile::{GlobalSettings, Profile};

/// Render alert settings. Returns true when persistent configuration changed.
pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    global: &mut GlobalSettings,
    status: Option<&GamelogStatus>,
) -> bool {
    let mut changed = false;

    ui.heading("Combat and decloak alerts");
    ui.add_space(8.0);
    changed |= ui
        .checkbox(&mut profile.alert_damage_flash, "Damage flash")
        .changed();
    ui.label("Pulses the preview border red when this character takes damage.");
    changed |= ui
        .checkbox(&mut profile.alert_decloak, "Decloak alert")
        .changed();
    ui.label("Shows a yellow border and DECLOAKED banner when a cloak is forced off.");
    ui.add_enabled_ui(profile.alert_decloak, |ui| {
        changed |= ui
            .checkbox(&mut profile.alert_decloak_audio, "Decloak audio cue")
            .changed();
    });

    ui.add_space(16.0);
    ui.separator();
    ui.heading("EVE Gamelogs directory");
    ui.add_space(6.0);

    let shown_path = global
        .gamelogs_directory
        .as_ref()
        .or_else(|| status.and_then(|s| s.directory.as_ref()))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "No EVE Gamelogs directory detected".to_string());
    ui.monospace(shown_path);

    ui.horizontal(|ui| {
        if ui.button("Browse…").clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            global.gamelogs_directory = Some(path);
            changed = true;
        }
        if ui
            .add_enabled(
                global.gamelogs_directory.is_some(),
                egui::Button::new("Auto"),
            )
            .clicked()
        {
            global.gamelogs_directory = None;
            changed = true;
        }
    });

    if let Some(status) = status {
        let message = match &status.diagnostic {
            GamelogDiagnostic::MissingDirectory => "Gamelogs directory is missing".to_string(),
            GamelogDiagnostic::InaccessibleDirectory(error) => {
                format!("Gamelogs directory is inaccessible: {error}")
            }
            GamelogDiagnostic::NoLogFiles => "No game log files found".to_string(),
            GamelogDiagnostic::NoMatchingCharacters => {
                "No log files match currently previewed characters".to_string()
            }
            GamelogDiagnostic::Monitoring => {
                format!("Monitoring {} active log file(s)", status.active_files)
            }
        };
        ui.label(message);
    } else {
        ui.label("Waiting for Gamelog monitor status…");
    }

    ui.add_space(12.0);
    ui.small(
        "Requires EVE Settings → Game Logs → Log game events to file. English EVE clients only.",
    );
    changed
}
