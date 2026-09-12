use anyhow::{Context, Result};
use ipc_channel::ipc::IpcOneShotServer;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};

use super::core::SaveMode;
use crate::manager::utils::spawn_daemon;

use super::DaemonStatus;
use super::SharedState;

impl SharedState {
    fn finish_esi_authorization_ui(&mut self) {
        let was_cancelling = self.esi_authorization_state.cancelling;
        let had_action = self.esi_authorization_state.action.take().is_some();
        self.esi_authorization_state.cancelling = false;
        let previous = self.esi_authorization_state.previous_health.take();

        if had_action
            && let Some((key, previous_status)) = previous
            && (was_cancelling
                || matches!(
                    self.esi_health.get(&key),
                    Some(
                        crate::common::ipc::EsiHealthStatus::AwaitingAuthorization
                            | crate::common::ipc::EsiHealthStatus::Authorizing
                    )
                ))
        {
            if let Some(status) = previous_status {
                self.esi_health.insert(key, status);
            } else {
                self.esi_health.remove(&key);
            }
        }
    }

    pub fn start_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        // 1. Create IPC OneShot Server
        let (server, server_name) =
            IpcOneShotServer::<BootstrapMessage>::new().context("Failed to create IPC server")?;

        // 2. Spawn Daemon with server name
        let child = spawn_daemon(&server_name, self.debug_mode)?;
        let pid = child.id();
        debug!(pid, server_name = %server_name, "Started daemon process");

        // 3. Spawn thread to wait for connection (avoid blocking Manager)
        let (tx, rx) = mpsc::channel();
        self.bootstrap_rx = Some(rx);

        std::thread::spawn(move || {
            debug!("Waiting for daemon IPC connection...");
            match server.accept() {
                Ok((_, bootstrap_msg)) => {
                    info!("Daemon connected via IPC");
                    let _ = tx.send(bootstrap_msg);
                }
                Err(e) => {
                    error!(error = %e, "Failed to accept IPC connection");
                }
            }
        });

        self.daemon = Some(child);
        self.daemon_status = DaemonStatus::Starting;
        Ok(())
    }

    pub fn stop_daemon(&mut self) -> Result<()> {
        if let Some(mut child) = self.daemon.take() {
            info!(pid = child.id(), "Stopping daemon process");

            if let Err(e) = child.kill() {
                error!(pid = child.id(), error = %e, "Failed to send SIGKILL to daemon");
            } else {
                debug!(pid = child.id(), "SIGKILL sent successfully");
            }

            match child.wait() {
                Ok(status) => {
                    info!(pid = child.id(), status = ?status, "Daemon exited");
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                }
                Err(e) => {
                    error!(pid = child.id(), error = %e, "Failed to wait for daemon exit");
                    self.daemon_status = DaemonStatus::Crashed(None);
                }
            }
            // Clear IPC channels immediately to prevent "Broken pipe" errors if save_config is called (e.g. on exit)
            self.ipc_config_tx = None;
            self.ipc_status_rx = None;
            self.daemon_status_rx = None;
            self.esi_authorization_url = None;
            self.finish_esi_authorization_ui();
        }
        Ok(())
    }

    pub fn restart_daemon(&mut self) {
        info!("Restart requested");
        if let Err(err) = self.stop_daemon().and_then(|_| self.start_daemon()) {
            error!(error = ?err, "Failed to restart daemon");
            self.status_message = Some(super::types::StatusMessage {
                text: format!("Restart failed: {err}"),
                color: STATUS_STOPPED,
            });
        }
    }

    pub fn reload_daemon_config(&mut self) {
        info!("Config reload requested - restarting daemon");
        self.restart_daemon();
    }

    pub fn poll_daemon(&mut self) {
        // 1. Check for Bootstrap handshake
        if let Some(ref rx) = self.bootstrap_rx
            && let Ok(msg) = rx.try_recv()
        {
            debug!("Received IPC channels from daemon");
            let (config_tx, status_rx) = msg;
            self.ipc_config_tx = Some(config_tx);

            // Bridge status_rx to Manager thread
            let (manager_tx, manager_rx) = mpsc::channel();
            self.daemon_status_rx = Some(manager_rx);

            std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    if manager_tx.send(msg).is_err() {
                        break; // Manager dropped
                    }
                }
            });

            // Sync config to daemon
            let _ = self.sync_to_daemon();

            self.bootstrap_rx = None; // Done
            self.daemon_status = DaemonStatus::Running;

            // initialize heartbeats
            self.ipc_healthy = true;
            self.last_heartbeat = Instant::now();
            self.missed_heartbeats = 0;
        }

        // 2. Poll Status Messages
        let mut profile_switch_request = None;

        // Collect messages first to avoid holding an immutable borrow on self while calling mutable methods (save_config)
        let messages: Vec<DaemonMessage> = if let Some(ref rx) = self.daemon_status_rx {
            let mut msgs = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
            msgs
        } else {
            Vec::new()
        };

        for msg in messages {
            match msg {
                DaemonMessage::Log { level, message } => {
                    info!(level = %level, "Daemon: {}", message);
                }
                DaemonMessage::Error(e) => {
                    error!("Daemon Error: {}", e);
                }
                DaemonMessage::Status(msg) => {
                    info!("Daemon Status: {}", msg);
                    self.status_message = Some(crate::manager::state::StatusMessage {
                        text: msg,
                        color: crate::common::constants::manager_ui::STATUS_RUNNING,
                    });
                }
                DaemonMessage::GamelogStatus(status) => {
                    self.gamelog_status = Some(status);
                }
                DaemonMessage::EsiAuthorizationUrl { url } => {
                    match Command::new("xdg-open").arg(&url).spawn() {
                        Ok(mut child) => {
                            std::thread::spawn(move || match child.wait() {
                                Ok(status) if !status.success() => {
                                    warn!(
                                        ?status,
                                        "Browser launcher could not open ESI authorization page"
                                    );
                                }
                                Err(error) => {
                                    warn!(%error, "Failed to reap ESI browser launcher");
                                }
                                Ok(_) => {}
                            });
                        }
                        Err(error) => warn!(%error, "Failed to open ESI authorization page"),
                    }
                    self.esi_authorization_url = Some(url);
                }
                DaemonMessage::EsiAuthorizationFinished => {
                    self.esi_authorization_url = None;
                    self.finish_esi_authorization_ui();
                }
                DaemonMessage::EsiAccountUpdated(account) => {
                    self.esi_authorization_url = None;
                    if let Some(existing) = self
                        .config
                        .global
                        .esi_accounts
                        .iter_mut()
                        .find(|existing| existing.character_id == account.character_id)
                    {
                        if *existing != account {
                            *existing = account;
                            self.settings_changed = true;
                        }
                    } else {
                        self.config.global.esi_accounts.push(account);
                        self.settings_changed = true;
                    }
                    if let Err(error) = self.config.save().and_then(|_| self.sync_to_daemon()) {
                        warn!(%error, "Failed to persist ESI account metadata");
                    }
                }
                DaemonMessage::EsiAccountDisconnected { character_id } => {
                    let before = self.config.global.esi_accounts.len();
                    self.config
                        .global
                        .esi_accounts
                        .retain(|account| account.character_id != character_id);
                    self.esi_health.remove(&Some(character_id));
                    if self.config.global.esi_accounts.len() != before {
                        self.settings_changed = true;
                        if let Err(error) = self.config.save().and_then(|_| self.sync_to_daemon()) {
                            warn!(%error, "Failed to persist ESI account disconnection");
                        }
                    }
                }
                DaemonMessage::EsiHealth(health) => {
                    if matches!(health.status, crate::common::ipc::EsiHealthStatus::Idle) {
                        self.esi_authorization_url = None;
                    }
                    self.esi_health.insert(health.character_id, health.status);
                }
                DaemonMessage::PositionChanged {
                    name,
                    x,
                    y,
                    width,
                    height,
                    is_custom,
                } => {
                    let mut changed = false;
                    if let Some(profile) = self.config.get_active_profile_mut() {
                        changed = profile
                            .update_thumbnail_position(&name, x, y, width, height, is_custom);
                    }

                    if !changed {
                        continue;
                    }

                    let auto_save = self
                        .config
                        .get_active_profile()
                        .map(|p| p.thumbnail_auto_save_position)
                        .unwrap_or(false);

                    debug!("Position changed: auto_save={}", auto_save);

                    if auto_save {
                        // Debounce save: only write to disk if it's been at least 1 second since last attempt
                        if self.last_save_attempt.elapsed()
                            > Duration::from_millis(AUTO_SAVE_DELAY_MS)
                        {
                            // Save to disk only (Daemon already has the correct position)
                            let _ = self.save_config_no_sync(SaveMode::Explicit);

                            // Send lightweight delta to confirm the position
                            // Daemon will perform idempotency check and skip redundant X11 operations
                            if let Some(ref tx) = self.ipc_config_tx {
                                let _ = tx.send(ConfigMessage::ThumbnailMove {
                                    name: name.clone(),
                                    is_custom,
                                    x,
                                    y,
                                    width,
                                    height,
                                });
                            }

                            self.last_save_attempt = Instant::now();
                            debug!("Debounced auto-save triggered with ThumbnailMove delta");
                        } else {
                            self.settings_changed = true; // Mark as dirty for final save
                        }
                    }
                }
                DaemonMessage::CharacterDetected { name, is_custom } => {
                    if is_custom {
                        info!("Daemon detected custom source: {}", name);
                    } else {
                        info!("Daemon detected character: {}", name);
                    }
                }
                DaemonMessage::RequestProfileSwitch(name) => {
                    info!("Daemon requested profile switch: {}", name);
                    profile_switch_request = Some(name);
                }
                DaemonMessage::Heartbeat => {
                    self.ipc_healthy = true;
                    self.last_heartbeat = Instant::now();
                    self.missed_heartbeats = 0;
                }
            }
        }

        if let Some(name) = profile_switch_request {
            if let Some(idx) = self
                .config
                .profiles
                .iter()
                .position(|p| p.profile_name == name)
            {
                self.switch_profile(idx);
            } else {
                warn!("Requested profile '{}' not found", name);
            }
        }

        // IPC Health Check
        // If connected but no heartbeat for 15s (5s grace * 3), assume hung process
        if self.daemon.is_some()
            && self.ipc_healthy
            && self.last_heartbeat.elapsed() > Duration::from_secs(5)
        {
            // Only count missed beats if we are expecting them
            if self.daemon_status == DaemonStatus::Running {
                self.missed_heartbeats += 1;

                // We poll roughly every DAEMON_CHECK_INTERVAL_MS (500ms).
                // So wait 30 ticks (15s) or just use time elapsed.
                // Actually, simpler to just check total elapsed time since last beat.
                if self.last_heartbeat.elapsed() > Duration::from_secs(15) {
                    warn!("IPC appears unhealthy (no heartbeat for 15s), restarting daemon");
                    self.ipc_healthy = false;
                    self.restart_daemon();
                    return; // Restart will reset everything
                }
            }
        }

        if self.last_health_check.elapsed() < Duration::from_millis(DAEMON_CHECK_INTERVAL_MS) {
            return;
        }
        self.last_health_check = Instant::now();

        if let Some(child) = self.daemon.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(pid = child.id(), exit = ?status.code(), "Daemon exited unexpectedly");
                    self.daemon = None;
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                    self.ipc_config_tx = None;
                    self.ipc_status_rx = None;
                    self.daemon_status_rx = None;
                    self.esi_authorization_url = None;
                    self.finish_esi_authorization_ui();
                }
                Ok(None) => {}
                Err(err) => {
                    error!(error = ?err, "Failed to query daemon status");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ipc::{EsiCredentialStorage, EsiHealthStatus};
    use crate::config::profile::Config;
    use crate::manager::state::EsiAuthorizationAction;

    #[test]
    fn authorization_finish_restores_cancelled_row_status() {
        let mut state = SharedState::new(Config::default(), false);
        let previous = EsiHealthStatus::Authorized {
            storage: EsiCredentialStorage::Secure,
        };
        state
            .esi_health
            .insert(Some(42), EsiHealthStatus::AwaitingAuthorization);
        state.esi_authorization_state.action = Some(EsiAuthorizationAction::Reauthorize(42));
        state.esi_authorization_state.cancelling = true;
        state.esi_authorization_state.previous_health = Some((Some(42), Some(previous.clone())));

        state.finish_esi_authorization_ui();

        assert_eq!(state.esi_health.get(&Some(42)), Some(&previous));
        assert!(state.esi_authorization_state.action.is_none());
        assert!(!state.esi_authorization_state.cancelling);
    }

    #[test]
    fn authorization_finish_preserves_terminal_error() {
        let mut state = SharedState::new(Config::default(), false);
        let error = EsiHealthStatus::Error("sanitized".into());
        state.esi_health.insert(None, error.clone());
        state.esi_authorization_state.action = Some(EsiAuthorizationAction::Connect);
        state.esi_authorization_state.previous_health = Some((None, None));

        state.finish_esi_authorization_ui();

        assert_eq!(state.esi_health.get(&None), Some(&error));
    }
}
