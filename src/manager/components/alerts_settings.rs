use eframe::egui;
use std::collections::HashMap;

use ipc_channel::ipc::IpcSender;

use crate::common::ipc::{ConfigMessage, EsiHealthStatus, GamelogDiagnostic, GamelogStatus};
use crate::config::profile::{GlobalSettings, Profile};
use crate::manager::state::{EsiAuthorizationAction, EsiAuthorizationUiState};

#[derive(Default)]
pub struct AlertsSettingsState {
    /// This intentionally starts false each time the Manager opens. A user must
    /// explicitly confirm the less durable session-only authorization path.
    pub session_only_confirmed: bool,
}

fn esi_status_text(status: Option<&EsiHealthStatus>) -> String {
    match status {
        None | Some(EsiHealthStatus::Idle) => "Not authorized".to_owned(),
        Some(EsiHealthStatus::AwaitingAuthorization) => "Awaiting authorization".to_owned(),
        Some(EsiHealthStatus::Authorizing) => "Authorizing…".to_owned(),
        Some(EsiHealthStatus::CredentialStoredSecure) => "Credential stored securely".to_owned(),
        Some(EsiHealthStatus::Authorized { storage }) => match storage {
            crate::common::ipc::EsiCredentialStorage::Secure => {
                "Connected • stored securely".to_owned()
            }
            crate::common::ipc::EsiCredentialStorage::SessionOnly => {
                "Connected • session-only".to_owned()
            }
        },
        Some(EsiHealthStatus::SsoMetadataUnavailable) => "SSO metadata unavailable".to_owned(),
        Some(EsiHealthStatus::MissingRequiredScopes) => "Required scopes are missing".to_owned(),
        Some(EsiHealthStatus::AuthorizationRevoked) => "Authorization was revoked".to_owned(),
        Some(EsiHealthStatus::SecureStorageUnavailable) => {
            "Secure credential storage is unavailable".to_owned()
        }
        Some(EsiHealthStatus::BackingOff {
            retry_after_seconds,
        }) => {
            format!("ESI backoff ({retry_after_seconds}s)")
        }
        Some(EsiHealthStatus::NoMatchingLiveClient) => "No matching live EVE client".to_owned(),
        Some(EsiHealthStatus::RateLimited {
            retry_after_seconds,
        }) => {
            format!("ESI rate limited ({retry_after_seconds}s)")
        }
        Some(EsiHealthStatus::Error(message)) => format!("ESI error: {message}"),
    }
}

fn esi_status_compact(status: Option<&EsiHealthStatus>) -> String {
    match status {
        None | Some(EsiHealthStatus::Idle) => "Not authorized".to_owned(),
        Some(EsiHealthStatus::AwaitingAuthorization) => "Waiting…".to_owned(),
        Some(EsiHealthStatus::Authorizing) => "Authorizing…".to_owned(),
        Some(EsiHealthStatus::CredentialStoredSecure) => "Stored".to_owned(),
        Some(EsiHealthStatus::Authorized { storage }) => match storage {
            crate::common::ipc::EsiCredentialStorage::Secure => "Secure".to_owned(),
            crate::common::ipc::EsiCredentialStorage::SessionOnly => "Session".to_owned(),
        },
        Some(EsiHealthStatus::SsoMetadataUnavailable) => "SSO unavailable".to_owned(),
        Some(EsiHealthStatus::MissingRequiredScopes) => "Scopes missing".to_owned(),
        Some(EsiHealthStatus::AuthorizationRevoked) => "Revoked".to_owned(),
        Some(EsiHealthStatus::SecureStorageUnavailable) => "Storage error".to_owned(),
        Some(EsiHealthStatus::BackingOff {
            retry_after_seconds,
        }) => format!("Backoff {retry_after_seconds}s"),
        Some(EsiHealthStatus::NoMatchingLiveClient) => "No live client".to_owned(),
        Some(EsiHealthStatus::RateLimited {
            retry_after_seconds,
        }) => format!("Limited {retry_after_seconds}s"),
        Some(EsiHealthStatus::Error(_)) => "ESI error".to_owned(),
    }
}

/// Render alert settings. Returns true when persistent configuration changed.
#[allow(clippy::too_many_arguments)]
pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    global: &mut GlobalSettings,
    status: Option<&GamelogStatus>,
    esi_health: &HashMap<Option<u64>, EsiHealthStatus>,
    authorization_url: Option<&str>,
    authorization: &mut EsiAuthorizationUiState,
    ipc_tx: Option<&IpcSender<ConfigMessage>>,
    state: &mut AlertsSettingsState,
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
    ui.heading("Implant docking reminder");
    changed |= ui
        .checkbox(
            &mut profile.alert_implant_reminder,
            "Implant docking reminder",
        )
        .changed();
    ui.label(
        "Shows a client overlay when an authorized character docks with valuable implants still plugged in.",
    );
    ui.small("Uses only ESI location and active-clone implant scopes. No Gamelogs, client automation, audio, or desktop notifications are used.");

    if !global.esi_accounts.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("implant_reminder_accounts")
            .num_columns(5)
            .spacing([8.0, 2.0])
            .striped(true)
            .show(ui, |ui| {
                for account in &global.esi_accounts {
                    let mut enabled = !profile
                        .implant_reminder_disabled_characters
                        .contains(&account.character_id);
                    if ui
                        .checkbox(&mut enabled, "")
                        .on_hover_text(
                            "Enable implant reminders for this character in this profile",
                        )
                        .changed()
                    {
                        if enabled {
                            profile
                                .implant_reminder_disabled_characters
                                .retain(|id| *id != account.character_id);
                        } else {
                            profile
                                .implant_reminder_disabled_characters
                                .push(account.character_id);
                        }
                        changed = true;
                    }
                    ui.strong(&account.character_name)
                        .on_hover_text(format!("Character ID: {}", account.character_id));

                    let health = esi_health.get(&Some(account.character_id));
                    ui.small(esi_status_compact(health))
                        .on_hover_text(esi_status_text(health));

                    let reauthorizing = authorization.action
                        == Some(EsiAuthorizationAction::Reauthorize(account.character_id));
                    if reauthorizing {
                        if authorization.cancelling {
                            ui.add_enabled(
                                false,
                                egui::Button::new("Wait…")
                                    .small()
                                    .min_size(egui::vec2(52.0, 0.0)),
                            )
                            .on_hover_text("Cancelling reauthorization");
                        } else if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .small()
                                    .min_size(egui::vec2(52.0, 0.0)),
                            )
                            .on_hover_text(format!(
                                "Cancel reauthorization for {}",
                                account.character_name
                            ))
                            .clicked()
                        {
                            if let Some(tx) = ipc_tx {
                                let _ = tx.send(ConfigMessage::EsiCancelAuthorization);
                            }
                            authorization.cancelling = true;
                        }
                    } else if ui
                        .add_enabled(
                            authorization.action.is_none(),
                            egui::Button::new("Reauth")
                                .small()
                                .min_size(egui::vec2(52.0, 0.0)),
                        )
                        .clicked()
                        && let Some(tx) = ipc_tx
                        && tx
                            .send(ConfigMessage::EsiReauthorize {
                                character_id: account.character_id,
                                session_only: false,
                            })
                            .is_ok()
                    {
                        authorization.previous_health = Some((
                            Some(account.character_id),
                            esi_health.get(&Some(account.character_id)).cloned(),
                        ));
                        authorization.action =
                            Some(EsiAuthorizationAction::Reauthorize(account.character_id));
                        authorization.cancelling = false;
                    }
                    if ui
                        .add_enabled(
                            authorization.action.is_none(),
                            egui::Button::new("×").small(),
                        )
                        .on_hover_text("Disconnect this character")
                        .clicked()
                        && let Some(tx) = ipc_tx
                    {
                        let _ = tx.send(ConfigMessage::EsiDisconnect {
                            character_id: account.character_id,
                        });
                    }
                    ui.end_row();
                }
            });
    }
    if authorization.action == Some(EsiAuthorizationAction::Connect) {
        if authorization.cancelling {
            ui.add_enabled(false, egui::Button::new("Cancelling connection…"));
        } else if ui.button("Cancel character connection").clicked() {
            if let Some(tx) = ipc_tx {
                let _ = tx.send(ConfigMessage::EsiCancelAuthorization);
            }
            authorization.cancelling = true;
        }
    } else if ui
        .add_enabled(
            authorization.action.is_none(),
            egui::Button::new("Connect EVE character…"),
        )
        .clicked()
        && let Some(tx) = ipc_tx
        && tx
            .send(ConfigMessage::EsiAuthorize {
                session_only: false,
            })
            .is_ok()
    {
        authorization.previous_health = Some((None, esi_health.get(&None).cloned()));
        authorization.action = Some(EsiAuthorizationAction::Connect);
        authorization.cancelling = false;
    }

    if let Some(status) = esi_health.get(&None) {
        ui.label(esi_status_text(Some(status)));
    }

    if let Some(url) = authorization_url {
        ui.horizontal(|ui| {
            ui.label("Authorization is waiting in your browser:");
            ui.hyperlink_to("Open authorization page", url);
            if authorization.action.is_none()
                && ui.button("Cancel").clicked()
                && let Some(tx) = ipc_tx
            {
                let _ = tx.send(ConfigMessage::EsiCancelAuthorization);
                authorization.action = Some(EsiAuthorizationAction::Connect);
                authorization.cancelling = true;
            }
        });
    }

    let secure_storage_unavailable = esi_health
        .values()
        .any(|status| matches!(status, EsiHealthStatus::SecureStorageUnavailable));
    if secure_storage_unavailable {
        ui.checkbox(
            &mut state.session_only_confirmed,
            "I understand session-only authorization is lost when the daemon stops",
        );
        if ui
            .add_enabled(
                state.session_only_confirmed && authorization.action.is_none(),
                egui::Button::new("Authorize for this session only…"),
            )
            .clicked()
            && let Some(tx) = ipc_tx
            && tx
                .send(ConfigMessage::EsiAuthorize { session_only: true })
                .is_ok()
        {
            authorization.previous_health = Some((None, esi_health.get(&None).cloned()));
            authorization.action = Some(EsiAuthorizationAction::Connect);
            authorization.cancelling = false;
        }
    }

    ui.small("Manage or revoke access in EVE Online’s authorized applications page.");
    ui.hyperlink_to(
        "EVE Online authorized applications",
        "https://community.eveonline.com/support/third-party-applications/",
    );

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
