use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};

use crate::config::DaemonConfig;
use crate::config::profile::EsiAccountMetadata;

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

/// Storage mode reported to the UI. This deliberately contains no credential
/// identifier or token material.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EsiCredentialStorage {
    Secure,
    SessionOnly,
}

/// Typed, non-secret ESI worker health. Keep this small enough for IPC and do
/// not add HTTP response bodies, JWTs, authorization codes, or tokens here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EsiHealthStatus {
    Idle,
    AwaitingAuthorization,
    Authorizing,
    CredentialStoredSecure,
    Authorized { storage: EsiCredentialStorage },
    SsoMetadataUnavailable,
    MissingRequiredScopes,
    AuthorizationRevoked,
    SecureStorageUnavailable,
    BackingOff { retry_after_seconds: u64 },
    NoMatchingLiveClient,
    RateLimited { retry_after_seconds: u64 },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EsiAccountHealth {
    pub character_id: Option<u64>,
    pub status: EsiHealthStatus,
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
    /// Begin one PKCE authorization attempt. `session_only` is true only after
    /// the user explicitly accepts non-durable authorization.
    EsiAuthorize { session_only: bool },
    /// Stop the pending browser/loopback authorization attempt.
    EsiCancelAuthorization,
    /// Replace an existing character authorization.
    EsiReauthorize {
        character_id: u64,
        session_only: bool,
    },
    /// Delete the character's keyring credential and clear in-memory tokens.
    EsiDisconnect { character_id: u64 },
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
    /// Browser URL for a pending PKCE authorization. The authorization code is
    /// delivered directly to the daemon loopback listener and never crosses IPC.
    EsiAuthorizationUrl {
        url: String,
    },
    /// The pending authorization attempt reached a terminal state. This event
    /// contains no OAuth response data and exists only to reset Manager UI.
    EsiAuthorizationFinished,
    /// Persistable identity metadata only; refresh/access tokens stay daemon-side.
    EsiAccountUpdated(EsiAccountMetadata),
    /// The daemon has removed the character credential and in-memory token.
    EsiAccountDisconnected {
        character_id: u64,
    },
    EsiHealth(EsiAccountHealth),
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
            esi_accounts: vec![EsiAccountMetadata {
                character_id: 42,
                character_name: "Capsuleer".to_owned(),
            }],
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
        assert_eq!(received.esi_accounts[0].character_id, 42);
    }

    #[test]
    fn esi_ipc_round_trip_never_needs_credentials() {
        let command = ConfigMessage::EsiReauthorize {
            character_id: 42,
            session_only: true,
        };
        let command_bytes = bincode::serialize(&command).unwrap();
        assert!(matches!(
            bincode::deserialize::<ConfigMessage>(&command_bytes).unwrap(),
            ConfigMessage::EsiReauthorize {
                character_id: 42,
                session_only: true
            }
        ));

        let status = DaemonMessage::EsiHealth(EsiAccountHealth {
            character_id: Some(42),
            status: EsiHealthStatus::Authorized {
                storage: EsiCredentialStorage::Secure,
            },
        });
        let status_bytes = bincode::serialize(&status).unwrap();
        assert!(matches!(
            bincode::deserialize::<DaemonMessage>(&status_bytes).unwrap(),
            DaemonMessage::EsiHealth(EsiAccountHealth {
                character_id: Some(42),
                ..
            })
        ));

        let finished = bincode::serialize(&DaemonMessage::EsiAuthorizationFinished).unwrap();
        assert!(matches!(
            bincode::deserialize::<DaemonMessage>(&finished).unwrap(),
            DaemonMessage::EsiAuthorizationFinished
        ));
    }
}
