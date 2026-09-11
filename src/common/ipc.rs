use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};

use crate::config::DaemonConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GamelogDiagnostic {
    MissingDirectory,
    InaccessibleDirectory(String),
    NoLogFiles,
    NoMatchingCharacters,
    Monitoring,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GamelogStatus {
    pub directory: Option<std::path::PathBuf>,
    pub diagnostic: GamelogDiagnostic,
    pub active_files: usize,
}

/// Messages sent from Manager to Daemon
#[derive(Debug, Serialize, Deserialize)]
pub enum ConfigMessage {
    /// Full state synchronization.
    ///
    /// Used for low-frequency, heavy operations like initial startup, profile switching,
    /// or bulk GUI setting changes. The payload is Boxed to reduce the enum size,
    /// optimizing the memory footprint for the high-frequency `ThumbnailMove` variant.
    Full(Box<DaemonConfig>),

    /// Lightweight spatial delta for a single thumbnail.
    ///
    /// Used during high-frequency drag events to avoid the overhead of full state serialization.
    /// The Daemon applies this incrementally and enforces idempotency to prevent redundant
    /// X11 re-configurations during rapid movement.
    ThumbnailMove {
        name: String,
        is_custom: bool,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    },
}

/// Messages sent from Daemon to Manager
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonMessage {
    /// Log message from daemon
    Log {
        level: String,
        message: String,
    },
    /// New character window detected
    CharacterDetected {
        name: String,
        is_custom: bool,
    },
    /// Notification that a thumbnail's spatial state was detected or changed by the Daemon.
    ///
    /// Upon receipt, the Manager updates its local state, saves to disk, and acknowledges
    /// with a `ThumbnailMove` delta. This confirms the new position without triggering
    /// a full config sync cycle.
    PositionChanged {
        name: String,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        is_custom: bool,
    },
    /// Daemon encountered an error
    Error(String),
    /// Generic status update for the Manager UI
    Status(String),
    /// Current state of the local EVE Gamelog monitor.
    GamelogStatus(GamelogStatus),
    RequestProfileSwitch(String),
    /// Periodic heartbeat (optional)
    Heartbeat,
}

/// The bootstrap payload sent over the initial server channel.
/// Contains the channel for receiving config updates and the channel for sending status updates.
pub type BootstrapMessage = (IpcSender<ConfigMessage>, IpcReceiver<DaemonMessage>);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn daemon_config_binary_ipc_round_trip_preserves_alert_settings() {
        let profile = crate::config::profile::Profile {
            alert_damage_flash: false,
            alert_decloak_audio: true,
            ..Default::default()
        };
        let config = DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
            gamelogs_directory: Some("/tmp/EVE/Gamelogs".into()),
        };
        let bytes = bincode::serialize(&ConfigMessage::Full(Box::new(config))).unwrap();
        let decoded: ConfigMessage = bincode::deserialize(&bytes).unwrap();
        let ConfigMessage::Full(received) = decoded else {
            panic!("unexpected IPC variant");
        };
        assert!(!received.profile.alert_damage_flash);
        assert!(received.profile.alert_decloak_audio);
        assert_eq!(
            received.gamelogs_directory,
            Some("/tmp/EVE/Gamelogs".into())
        );
    }
}
