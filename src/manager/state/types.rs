use crate::common::constants::manager_ui::*;
use crate::common::ipc::EsiHealthStatus;
use eframe::egui;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ManagerTab {
    Behavior,
    Appearance,
    Hotkeys,
    Characters,
    Sources,
    Alerts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    Starting,
    Running,
    Stopped,
    Crashed(Option<i32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsiAuthorizationAction {
    Connect,
    Reauthorize(u64),
}

#[derive(Debug, Default)]
pub struct EsiAuthorizationUiState {
    pub action: Option<EsiAuthorizationAction>,
    pub cancelling: bool,
    pub previous_health: Option<(Option<u64>, Option<EsiHealthStatus>)>,
}

impl DaemonStatus {
    pub fn color(&self) -> egui::Color32 {
        match self {
            DaemonStatus::Running => STATUS_RUNNING,
            DaemonStatus::Starting => STATUS_STARTING,
            _ => STATUS_STOPPED,
        }
    }

    pub fn label(&self) -> String {
        match self {
            DaemonStatus::Running => "Daemon running".to_string(),
            DaemonStatus::Starting => "Daemon starting...".to_string(),
            DaemonStatus::Stopped => "Daemon stopped".to_string(),
            DaemonStatus::Crashed(code) => match code {
                Some(code) => format!("Daemon crashed (exit {code})"),
                None => "Daemon crashed".to_string(),
            },
        }
    }
}

pub struct StatusMessage {
    pub text: String,
    pub color: egui::Color32,
}
