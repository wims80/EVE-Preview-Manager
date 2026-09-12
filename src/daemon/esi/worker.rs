use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::jwk::JwkSet;
use keyring::Entry;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, IF_NONE_MATCH, USER_AGENT};
use serde::Deserialize;
use url::Url;

use super::auth::{self, AuthError, AuthorizationAttempt, Discovery, VerifiedCharacter};
use super::cache::ResponseCache;
use super::classifier::ImplantClassifier;
use super::latch::{DockIdentity, Observation, ReminderLatches};
use super::limits::RateLimits;

const LOCATION: &str = "location";
const IMPLANTS: &str = "implants";
const KEYRING_SERVICE: &str = "com.evepreview.manager.esi";
const ESI_BASE: &str = "https://esi.evetech.net/latest";

#[derive(Clone, Debug)]
pub struct PublicAccount {
    pub character_id: u64,
    pub character_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerConfig {
    pub enabled: bool,
    pub disabled_characters: HashSet<u64>,
    pub accounts: Vec<PublicAccount>,
    pub live_characters: Vec<String>,
    pub generation: u64,
}

#[derive(Debug)]
pub enum WorkerCommand {
    Configure(WorkerConfig),
    Authorize {
        session_only: bool,
        expected_character_id: Option<u64>,
    },
    CancelAuthorization,
    Disconnect(u64),
    Snooze(u64),
}

#[derive(Debug)]
pub enum WorkerEvent {
    AuthorizationUrl {
        epoch: u64,
        url: String,
    },
    AuthorizationFinished,
    AccountConnected {
        account: PublicAccount,
        secure: bool,
    },
    AccountDisconnected {
        character_id: u64,
    },
    Health {
        character_id: Option<u64>,
        health: Health,
    },
    Reminder {
        epoch: u64,
        generation: u64,
        character_id: u64,
        character_name: String,
        implant_names: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Idle,
    AwaitingAuthorization,
    Authorizing,
    CredentialStoredSecure,
    AuthorizedSecure,
    AuthorizedSession,
    MetadataUnavailable,
    MissingScopes,
    Revoked,
    SecureStorageUnavailable,
    Backoff(u64),
    NoMatchingClient,
    RateLimited(u64),
    Error,
}

pub struct EsiWorker {
    wake: SyncSender<()>,
    pub events: Receiver<WorkerEvent>,
    handle: Option<JoinHandle<()>>,
    epoch: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    config_generation: Arc<AtomicU64>,
    pending_commands: Arc<Mutex<VecDeque<WorkerCommand>>>,
}

impl EsiWorker {
    pub fn spawn() -> Self {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (event_tx, event_rx) = mpsc::sync_channel(32);
        let epoch = Arc::new(AtomicU64::new(1));
        let shutdown = Arc::new(AtomicBool::new(false));
        let config_generation = Arc::new(AtomicU64::new(0));
        let pending_commands = Arc::new(Mutex::new(VecDeque::new()));
        let worker_epoch = epoch.clone();
        let worker_shutdown = shutdown.clone();
        let worker_config_generation = config_generation.clone();
        let worker_pending_commands = pending_commands.clone();
        let handle = std::thread::Builder::new()
            .name("epm-esi".into())
            .spawn(move || {
                Worker::new(
                    wake_rx,
                    event_tx,
                    worker_epoch,
                    worker_shutdown,
                    worker_config_generation,
                    worker_pending_commands,
                )
                .run()
            })
            .ok();
        Self {
            wake: wake_tx,
            events: event_rx,
            handle,
            epoch,
            shutdown,
            config_generation,
            pending_commands,
        }
    }
    pub fn send(&self, command: WorkerCommand) -> bool {
        match &command {
            WorkerCommand::Configure(config) => {
                self.config_generation
                    .store(config.generation, Ordering::Release);
            }
            WorkerCommand::CancelAuthorization | WorkerCommand::Disconnect(_) => {
                self.epoch.fetch_add(1, Ordering::AcqRel);
            }
            WorkerCommand::Authorize { .. } | WorkerCommand::Snooze(_) => {}
        }
        let Ok(mut pending) = self.pending_commands.lock() else {
            return false;
        };
        if matches!(command, WorkerCommand::Configure(_))
            && let Some(WorkerCommand::Configure(existing)) = pending.back_mut()
            && let WorkerCommand::Configure(replacement) = command
        {
            *existing = replacement;
        } else {
            if pending.len() >= 64 {
                return false;
            }
            pending.push_back(command);
        }
        drop(pending);
        !matches!(
            self.wake.try_send(()),
            Err(mpsc::TrySendError::Disconnected(_))
        )
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

impl Drop for EsiWorker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct AccessToken(String);
impl AccessToken {
    fn bearer(&self) -> Result<HeaderValue, ()> {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", self.0)).map_err(|_| ())?;
        value.set_sensitive(true);
        Ok(value)
    }
}
struct RefreshToken(String);

struct Account {
    verified: PublicAccount,
    access: Option<AccessToken>,
    expires_at: u64,
    refresh: Option<RefreshToken>,
    secure: bool,
    next_location: Instant,
    failures: u32,
}

struct Pending {
    attempt: AuthorizationAttempt,
    listener: TcpListener,
    session_only: bool,
    config_generation: u64,
}

struct Worker {
    wake: Receiver<()>,
    events: SyncSender<WorkerEvent>,
    client: Client,
    config: WorkerConfig,
    accounts: HashMap<u64, Account>,
    pending: Option<Pending>,
    authorization_active: AtomicBool,
    discovery: Option<(Discovery, Instant)>,
    jwks: Option<(JwkSet, Instant)>,
    locations: ResponseCache<Location>,
    implants: ResponseCache<Vec<u32>>,
    limits: RateLimits,
    latches: ReminderLatches,
    classifier: Option<ImplantClassifier>,
    next_account: usize,
    sso_pause_until: Option<Instant>,
    epoch: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    pending_events: RefCell<VecDeque<WorkerEvent>>,
    disconnected: HashSet<u64>,
    newly_connected: HashSet<u64>,
    config_generation: Arc<AtomicU64>,
    pending_commands: Arc<Mutex<VecDeque<WorkerCommand>>>,
}

impl Worker {
    fn new(
        wake: Receiver<()>,
        events: SyncSender<WorkerEvent>,
        epoch: Arc<AtomicU64>,
        shutdown: Arc<AtomicBool>,
        config_generation: Arc<AtomicU64>,
        pending_commands: Arc<Mutex<VecDeque<WorkerCommand>>>,
    ) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP client");
        Self {
            wake,
            events,
            client,
            config: WorkerConfig::default(),
            accounts: HashMap::new(),
            pending: None,
            authorization_active: AtomicBool::new(false),
            discovery: None,
            jwks: None,
            locations: ResponseCache::default(),
            implants: ResponseCache::default(),
            limits: RateLimits::default(),
            latches: ReminderLatches::default(),
            classifier: ImplantClassifier::bundled().ok(),
            next_account: 0,
            sso_pause_until: None,
            epoch,
            shutdown,
            pending_events: RefCell::new(VecDeque::new()),
            disconnected: HashSet::new(),
            newly_connected: HashSet::new(),
            config_generation,
            pending_commands,
        }
    }

    fn run(&mut self) {
        while !self.shutdown.load(Ordering::Acquire) {
            self.flush_events();
            match self.wake.recv_timeout(Duration::from_millis(100)) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            loop {
                let command = self
                    .pending_commands
                    .lock()
                    .ok()
                    .and_then(|mut queue| queue.pop_front());
                let Some(command) = command else { break };
                self.command(command);
            }
            self.poll_callback();
            self.poll_one();
        }
    }

    fn command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Configure(config) => {
                let changed = config.generation != self.config.generation;
                self.config = config;
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.config_generation != self.config.generation)
                {
                    self.pending = None;
                    self.finish_authorization();
                }
                let configured: HashSet<u64> = self
                    .config
                    .accounts
                    .iter()
                    .map(|account| account.character_id)
                    .collect();
                self.newly_connected.retain(|id| !configured.contains(id));
                self.disconnected.retain(|id| configured.contains(id));
                self.accounts.retain(|id, _| {
                    (configured.contains(id) || self.newly_connected.contains(id))
                        && !self.disconnected.contains(id)
                });
                if changed {
                    self.latches.prime_all();
                }
                for (index, metadata) in self.config.accounts.clone().into_iter().enumerate() {
                    if self.disconnected.contains(&metadata.character_id) {
                        continue;
                    }
                    if self.accounts.contains_key(&metadata.character_id) {
                        continue;
                    }
                    let character_id = metadata.character_id;
                    let refresh = match read_refresh(character_id) {
                        Ok(refresh) => refresh,
                        Err(()) => {
                            self.emit(WorkerEvent::Health {
                                character_id: Some(character_id),
                                health: Health::SecureStorageUnavailable,
                            });
                            continue;
                        }
                    };
                    let secure = refresh.is_some();
                    self.accounts.insert(
                        character_id,
                        Account {
                            verified: metadata,
                            access: None,
                            expires_at: 0,
                            secure,
                            refresh,
                            next_location: Instant::now()
                                + Duration::from_millis(index as u64 * 500),
                            failures: 0,
                        },
                    );
                    self.emit(WorkerEvent::Health {
                        character_id: Some(character_id),
                        health: if secure {
                            Health::CredentialStoredSecure
                        } else {
                            Health::Revoked
                        },
                    });
                }
            }
            WorkerCommand::Authorize {
                session_only,
                expected_character_id,
            } => self.authorize(session_only, expected_character_id),
            WorkerCommand::CancelAuthorization => {
                self.pending = None;
                self.emit(WorkerEvent::Health {
                    character_id: None,
                    health: Health::Idle,
                });
                self.finish_authorization();
            }
            WorkerCommand::Disconnect(id) => {
                self.disconnected.insert(id);
                self.newly_connected.remove(&id);
                self.accounts.remove(&id);
                self.latches.forget(id);
                self.locations.remove_character(id);
                self.implants.remove_character(id);
                let deleted = match Entry::new(KEYRING_SERVICE, &id.to_string())
                    .and_then(|entry| entry.delete_credential())
                {
                    Ok(()) | Err(keyring::Error::NoEntry) => true,
                    Err(_) => false,
                };
                if deleted {
                    self.emit(WorkerEvent::AccountDisconnected { character_id: id });
                } else {
                    self.emit(WorkerEvent::Health {
                        character_id: Some(id),
                        health: Health::SecureStorageUnavailable,
                    });
                }
                self.finish_authorization();
            }
            WorkerCommand::Snooze(id) => self.latches.snooze(id),
        }
    }

    fn authorize(&mut self, session_only: bool, expected_character_id: Option<u64>) {
        if self.pending.is_some() {
            self.emit(WorkerEvent::Health {
                character_id: expected_character_id,
                health: Health::AwaitingAuthorization,
            });
            return;
        }
        if !session_only && !keyring_available() {
            self.emit(WorkerEvent::Health {
                character_id: expected_character_id,
                health: Health::SecureStorageUnavailable,
            });
            self.emit(WorkerEvent::AuthorizationFinished);
            return;
        }
        if self
            .sso_pause_until
            .is_some_and(|until| until > Instant::now())
        {
            let seconds = self
                .sso_pause_until
                .unwrap()
                .saturating_duration_since(Instant::now())
                .as_secs();
            self.emit(WorkerEvent::Health {
                character_id: expected_character_id,
                health: Health::Backoff(seconds),
            });
            self.emit(WorkerEvent::AuthorizationFinished);
            return;
        }
        let Some((client_id, callback, _, _)) = release_config() else {
            self.emit(WorkerEvent::Health {
                character_id: expected_character_id,
                health: Health::MetadataUnavailable,
            });
            self.emit(WorkerEvent::AuthorizationFinished);
            return;
        };
        let discovery = match self.discovery() {
            Ok(value) => value,
            Err(_) => {
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::MetadataUnavailable,
                });
                self.emit(WorkerEvent::AuthorizationFinished);
                return;
            }
        };
        let attempt = match auth::begin_authorization(
            &discovery,
            client_id,
            callback,
            expected_character_id,
            self.epoch.load(Ordering::Acquire),
        ) {
            Ok(value) => value,
            Err(_) => {
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::Error,
                });
                self.emit(WorkerEvent::AuthorizationFinished);
                return;
            }
        };
        let callback_url = match Url::parse(callback) {
            Ok(url) => url,
            Err(_) => {
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::Error,
                });
                self.emit(WorkerEvent::AuthorizationFinished);
                return;
            }
        };
        let ip: IpAddr = match callback_url
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse().ok())
        {
            Some(ip) => ip,
            None => {
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::Error,
                });
                self.emit(WorkerEvent::AuthorizationFinished);
                return;
            }
        };
        let address = SocketAddr::new(ip, callback_url.port().unwrap_or(0));
        match TcpListener::bind(address).and_then(|listener| {
            listener.set_nonblocking(true)?;
            Ok(listener)
        }) {
            Ok(listener) => {
                let url = attempt.url.to_string();
                self.pending = Some(Pending {
                    attempt,
                    listener,
                    session_only,
                    config_generation: self.config.generation,
                });
                self.authorization_active.store(true, Ordering::Release);
                self.emit(WorkerEvent::AuthorizationUrl {
                    epoch: self.epoch.load(Ordering::Acquire),
                    url,
                });
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::AwaitingAuthorization,
                });
            }
            Err(_) => {
                self.emit(WorkerEvent::Health {
                    character_id: expected_character_id,
                    health: Health::Error,
                });
                self.emit(WorkerEvent::AuthorizationFinished);
            }
        }
    }

    fn poll_callback(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        if Instant::now() >= pending.attempt.expires_at {
            self.pending = None;
            self.emit(WorkerEvent::Health {
                character_id: None,
                health: Health::Idle,
            });
            self.finish_authorization();
            return;
        }
        let accepted = match pending.listener.accept() {
            Ok(pair) => Some(pair.0),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(_) => None,
        };
        let Some(mut stream) = accepted else { return };
        let callback_path = release_config()
            .and_then(|(_, cb, _, _)| Url::parse(cb).ok())
            .map(|url| url.path().to_owned())
            .unwrap_or_default();
        let Some((code, state)) = parse_callback(&mut stream, &callback_path) else {
            respond(&mut stream, 400, "Invalid authorization callback");
            return;
        };
        if state != pending.attempt.state {
            respond(&mut stream, 400, "Authorization state mismatch");
            return;
        }
        let pending = self.pending.take().expect("pending exists");
        let epoch_changed = pending.attempt.generation != self.epoch.load(Ordering::Acquire);
        let config_changed =
            pending.config_generation != self.config_generation.load(Ordering::Acquire);
        if epoch_changed || config_changed {
            respond(
                &mut stream,
                400,
                "Authorization was superseded by a configuration change.",
            );
            self.finish_authorization();
            return;
        }
        self.emit(WorkerEvent::Health {
            character_id: pending.attempt.expected_character_id,
            health: Health::Authorizing,
        });
        match self.exchange_code(&code, &pending) {
            Ok((verified, access, refresh)) => {
                let epoch_changed =
                    pending.attempt.generation != self.epoch.load(Ordering::Acquire);
                let config_changed =
                    pending.config_generation != self.config_generation.load(Ordering::Acquire);
                if epoch_changed || config_changed {
                    respond(
                        &mut stream,
                        400,
                        "Authorization was cancelled or superseded.",
                    );
                    self.finish_authorization();
                    return;
                }
                let metadata = PublicAccount {
                    character_id: verified.character_id,
                    character_name: verified.name,
                };
                let mut secure = !pending.session_only;
                let mut storage_failed = false;
                let mut refresh = refresh.map(RefreshToken);
                let previous_refresh = if secure {
                    match read_refresh(metadata.character_id) {
                        Ok(value) => value,
                        Err(()) => {
                            self.emit(WorkerEvent::Health {
                                character_id: Some(metadata.character_id),
                                health: Health::SecureStorageUnavailable,
                            });
                            respond(&mut stream, 500, "Secure credential storage failed.");
                            self.finish_authorization();
                            return;
                        }
                    }
                } else {
                    None
                };
                let replacement_written = secure
                    && refresh
                        .as_ref()
                        .is_some_and(|token| store_refresh(metadata.character_id, &token.0));
                if secure && !replacement_written {
                    secure = false;
                    refresh = None;
                    storage_failed = true;
                }
                let epoch_changed =
                    pending.attempt.generation != self.epoch.load(Ordering::Acquire);
                let config_changed =
                    pending.config_generation != self.config_generation.load(Ordering::Acquire);
                if epoch_changed || config_changed {
                    let restored = if replacement_written {
                        previous_refresh.as_ref().map_or_else(
                            || delete_refresh(metadata.character_id),
                            |token| store_refresh(metadata.character_id, &token.0),
                        )
                    } else {
                        true
                    };
                    if !restored {
                        self.emit(WorkerEvent::Health {
                            character_id: Some(metadata.character_id),
                            health: Health::SecureStorageUnavailable,
                        });
                    }
                    respond(
                        &mut stream,
                        400,
                        "Authorization was cancelled or superseded.",
                    );
                    self.finish_authorization();
                    return;
                }
                self.disconnected.remove(&metadata.character_id);
                self.newly_connected.insert(metadata.character_id);
                self.accounts.insert(
                    metadata.character_id,
                    Account {
                        verified: metadata.clone(),
                        access: Some(AccessToken(access)),
                        expires_at: verified.expires_at_unix,
                        refresh,
                        secure,
                        next_location: Instant::now(),
                        failures: 0,
                    },
                );
                respond(
                    &mut stream,
                    200,
                    "EVE Preview Manager authorization complete. You may close this tab.",
                );
                self.emit(WorkerEvent::AccountConnected {
                    account: metadata.clone(),
                    secure,
                });
                self.emit(WorkerEvent::Health {
                    character_id: Some(metadata.character_id),
                    health: if storage_failed {
                        Health::SecureStorageUnavailable
                    } else if secure {
                        Health::AuthorizedSecure
                    } else {
                        Health::AuthorizedSession
                    },
                });
                self.finish_authorization();
            }
            Err(AuthError::MissingScopes) => {
                respond(&mut stream, 400, "Required ESI scopes were not granted.");
                self.emit(WorkerEvent::Health {
                    character_id: pending.attempt.expected_character_id,
                    health: Health::MissingScopes,
                });
                self.finish_authorization();
            }
            Err(_) => {
                respond(&mut stream, 400, "Authorization failed.");
                self.emit(WorkerEvent::Health {
                    character_id: pending.attempt.expected_character_id,
                    health: Health::Error,
                });
                self.finish_authorization();
            }
        }
    }

    fn exchange_code(
        &mut self,
        code: &str,
        pending: &Pending,
    ) -> Result<(VerifiedCharacter, String, Option<String>), AuthError> {
        if self
            .sso_pause_until
            .is_some_and(|until| until > Instant::now())
        {
            return Err(AuthError::Unavailable);
        }
        let (client_id, callback, _, _) = release_config().ok_or(AuthError::Unavailable)?;
        let discovery = self.discovery()?;
        let response = self
            .client
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("redirect_uri", callback),
                ("code_verifier", pending.attempt.verifier.as_str()),
            ])
            .send()
            .map_err(|_| AuthError::Unavailable)?;
        if response.status().as_u16() == 429 {
            self.pause_sso(&response);
            return Err(AuthError::Unavailable);
        }
        let tokens: TokenResponse = bounded_json(response)?;
        let verified = self.verify(&tokens.access_token, pending.attempt.expected_character_id)?;
        Ok((verified, tokens.access_token, tokens.refresh_token))
    }

    fn discovery(&mut self) -> Result<Discovery, AuthError> {
        if let Some((value, until)) = &self.discovery
            && Instant::now() < *until
        {
            return Ok(value.clone());
        }
        if self
            .sso_pause_until
            .is_some_and(|until| until > Instant::now())
        {
            return Err(AuthError::Unavailable);
        }
        let response = self
            .client
            .get(auth::DISCOVERY_URL)
            .send()
            .map_err(|_| AuthError::Unavailable)?;
        if response.status().as_u16() == 429 {
            self.pause_sso(&response);
            return Err(AuthError::Unavailable);
        }
        let headers = response.headers().clone();
        let value: Discovery = bounded_json(response)?;
        value.validate()?;
        let until = super::cache::freshness(&headers, Instant::now(), SystemTime::now());
        self.discovery = Some((value.clone(), until));
        Ok(value)
    }

    fn verify(
        &mut self,
        token: &str,
        expected: Option<u64>,
    ) -> Result<VerifiedCharacter, AuthError> {
        let (client_id, _, _, _) = release_config().ok_or(AuthError::Unavailable)?;
        let discovery = self.discovery()?;
        for refresh in [false, true] {
            if refresh
                || self
                    .jwks
                    .as_ref()
                    .is_none_or(|(_, until)| Instant::now() >= *until)
            {
                let response = self
                    .client
                    .get(&discovery.jwks_uri)
                    .send()
                    .map_err(|_| AuthError::Unavailable)?;
                if response.status().as_u16() == 429 {
                    self.pause_sso(&response);
                    return Err(AuthError::Unavailable);
                }
                let headers = response.headers().clone();
                let keys: JwkSet = bounded_json(response)?;
                self.jwks = Some((
                    keys,
                    super::cache::freshness(&headers, Instant::now(), SystemTime::now()),
                ));
            }
            let result = auth::validate_access_token(
                token,
                &self.jwks.as_ref().expect("set").0,
                &discovery.issuer,
                client_id,
                expected,
            );
            if !matches!(result, Err(AuthError::UnknownKey)) || refresh {
                return result;
            }
            self.jwks = None;
        }
        Err(AuthError::UnknownKey)
    }

    fn poll_one(&mut self) {
        if !self.config.enabled || self.config.accounts.is_empty() {
            return;
        }
        let lifecycle_epoch = self.epoch.load(Ordering::Acquire);
        let generation = self.config.generation;
        if generation != self.config_generation.load(Ordering::Acquire) {
            return;
        }
        let live: HashSet<String> = self
            .config
            .live_characters
            .iter()
            .map(|name| normalize(name))
            .collect();
        let ids: Vec<u64> = self
            .accounts
            .values()
            .filter(|account| {
                live.contains(&normalize(&account.verified.character_name))
                    && !self
                        .config
                        .disabled_characters
                        .contains(&account.verified.character_id)
            })
            .map(|account| account.verified.character_id)
            .collect();
        if ids.is_empty() {
            self.emit(WorkerEvent::Health {
                character_id: None,
                health: Health::NoMatchingClient,
            });
            return;
        }
        let id = ids[self.next_account % ids.len()];
        self.next_account = self.next_account.wrapping_add(1);
        let now = Instant::now();
        if self
            .accounts
            .get(&id)
            .is_some_and(|a| now < a.next_location)
            || now < self.limits.allowed_at(id, now)
        {
            return;
        }
        if !self.ensure_access(id) {
            return;
        }
        if lifecycle_epoch != self.epoch.load(Ordering::Acquire)
            || generation != self.config.generation
            || generation != self.config_generation.load(Ordering::Acquire)
        {
            return;
        }
        let epoch = lifecycle_epoch;
        match self.fetch_location(id) {
            Ok(Some(location)) => {
                if epoch != self.epoch.load(Ordering::Acquire)
                    || generation != self.config.generation
                    || generation != self.config_generation.load(Ordering::Acquire)
                {
                    return;
                }
                if let Some(account) = self.accounts.get_mut(&id) {
                    account.failures = 0;
                    account.next_location = self
                        .locations
                        .fresh_until(id, LOCATION)
                        .unwrap_or(now)
                        .max(now + Duration::from_secs(10));
                }
                let dock = DockIdentity::new(
                    location.solar_system_id,
                    location.station_id,
                    location.structure_id,
                );
                let limit_now = Instant::now();
                if limit_now < self.limits.allowed_at(id, limit_now) {
                    return;
                }
                if self.latches.observe(
                    id,
                    dock,
                    self.config.disabled_characters.contains(&id),
                    now,
                ) == Observation::Fire
                {
                    if epoch != self.epoch.load(Ordering::Acquire)
                        || generation != self.config.generation
                        || generation != self.config_generation.load(Ordering::Acquire)
                    {
                        return;
                    }
                    match self.fetch_implants(id) {
                        Ok(Some(ids)) => {
                            let names = self
                                .classifier
                                .as_ref()
                                .map(|classifier| classifier.classify(&ids))
                                .unwrap_or_default();
                            if !names.is_empty()
                                && generation == self.config.generation
                                && epoch == self.epoch.load(Ordering::Acquire)
                                && generation == self.config_generation.load(Ordering::Acquire)
                            {
                                let name = self
                                    .accounts
                                    .get(&id)
                                    .map(|a| a.verified.character_name.clone())
                                    .unwrap_or_default();
                                self.emit(WorkerEvent::Reminder {
                                    epoch,
                                    generation,
                                    character_id: id,
                                    character_name: name,
                                    implant_names: names,
                                });
                            }
                        }
                        Ok(None) => {}
                        Err(_) => {
                            self.latches.implant_failed(id);
                            self.backoff(id);
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(_) => self.backoff(id),
        }
    }

    fn ensure_access(&mut self, id: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if self
            .accounts
            .get(&id)
            .is_some_and(|account| account.access.is_some() && account.expires_at > now + 30)
        {
            return true;
        }
        let Some(refresh) = self
            .accounts
            .get_mut(&id)
            .and_then(|account| account.refresh.take())
        else {
            self.emit(WorkerEvent::Health {
                character_id: Some(id),
                health: Health::Revoked,
            });
            return false;
        };
        let result = self.refresh(id, &refresh.0);
        match result {
            Ok((verified, access, rotated)) => {
                let account = self.accounts.get_mut(&id).expect("account exists");
                account.verified = PublicAccount {
                    character_id: verified.character_id,
                    character_name: verified.name,
                };
                account.access = Some(AccessToken(access));
                account.expires_at = verified.expires_at_unix;
                let token = rotated.unwrap_or(refresh.0);
                let health = if account.secure && !store_refresh(id, &token) {
                    account.secure = false;
                    account.refresh = None;
                    Health::SecureStorageUnavailable
                } else {
                    account.refresh = Some(RefreshToken(token));
                    if account.secure {
                        Health::AuthorizedSecure
                    } else {
                        Health::AuthorizedSession
                    }
                };
                self.emit(WorkerEvent::Health {
                    character_id: Some(id),
                    health,
                });
                true
            }
            Err(RefreshError::Revoked) => {
                self.emit(WorkerEvent::Health {
                    character_id: Some(id),
                    health: Health::Revoked,
                });
                false
            }
            Err(RefreshError::Transient(rotated)) => {
                let replacement = rotated.unwrap_or(refresh.0);
                if let Some(account) = self.accounts.get_mut(&id) {
                    if account.secure && !store_refresh(id, &replacement) {
                        account.secure = false;
                        account.refresh = None;
                        self.emit(WorkerEvent::Health {
                            character_id: Some(id),
                            health: Health::SecureStorageUnavailable,
                        });
                    } else {
                        account.refresh = Some(RefreshToken(replacement));
                    }
                }
                self.backoff(id);
                false
            }
        }
    }

    fn refresh(
        &mut self,
        id: u64,
        refresh: &str,
    ) -> Result<(VerifiedCharacter, String, Option<String>), RefreshError> {
        if self
            .sso_pause_until
            .is_some_and(|until| until > Instant::now())
        {
            return Err(RefreshError::Transient(None));
        }
        let (client_id, _, _, _) = release_config().ok_or(RefreshError::Transient(None))?;
        let discovery = self
            .discovery()
            .map_err(|_| RefreshError::Transient(None))?;
        let response = self
            .client
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", client_id),
            ])
            .send()
            .map_err(|_| RefreshError::Transient(None))?;
        if response.status().as_u16() == 429 {
            self.pause_sso(&response);
            return Err(RefreshError::Transient(None));
        }
        if response.status().as_u16() == 400 || response.status().as_u16() == 401 {
            return Err(RefreshError::Revoked);
        }
        if !response.status().is_success() {
            return Err(RefreshError::Transient(None));
        }
        let tokens: TokenResponse =
            bounded_json(response).map_err(|_| RefreshError::Transient(None))?;
        let verified = match self.verify(&tokens.access_token, Some(id)) {
            Ok(verified) => verified,
            Err(AuthError::Unavailable | AuthError::UnknownKey) => {
                return Err(RefreshError::Transient(tokens.refresh_token));
            }
            Err(_) => return Err(RefreshError::Revoked),
        };
        Ok((verified, tokens.access_token, tokens.refresh_token))
    }

    fn fetch_location(&mut self, id: u64) -> Result<Option<Location>, ()> {
        self.fetch(
            id,
            LOCATION,
            format!("{ESI_BASE}/characters/{id}/location/"),
        )
    }
    fn fetch_implants(&mut self, id: u64) -> Result<Option<Vec<u32>>, ()> {
        self.fetch(
            id,
            IMPLANTS,
            format!("{ESI_BASE}/characters/{id}/implants/"),
        )
    }
    fn fetch<T>(&mut self, id: u64, endpoint: &'static str, url: String) -> Result<Option<T>, ()>
    where
        T: Clone + for<'de> Deserialize<'de> + CacheType,
    {
        let limit_now = Instant::now();
        if limit_now < self.limits.allowed_at(id, limit_now) {
            return Err(());
        }
        if let Some(body) = T::cache(self).get_fresh(id, endpoint, Instant::now()) {
            return Ok(Some(body));
        }
        let (_, _, contact, compatibility) = release_config().ok_or(())?;
        let access = self
            .accounts
            .get(&id)
            .and_then(|a| a.access.as_ref())
            .ok_or(())?;
        let headers = esi_headers(access, contact, compatibility)?;
        let mut request = self.client.get(url).headers(headers);
        if let Some(etag) = T::cache(self).etag_after_expiry(id, endpoint, Instant::now()) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().map_err(|_| ())?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        self.limits.observe(id, status, &headers, Instant::now());
        if status == 429 {
            let seconds = headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .unwrap_or(30);
            self.emit(WorkerEvent::Health {
                character_id: Some(id),
                health: Health::RateLimited(seconds),
            });
        }
        if status == 304 {
            return T::cache_mut(self)
                .revalidate_304(id, endpoint, &headers, Instant::now(), SystemTime::now())
                .map(Some)
                .ok_or(());
        }
        if !response.status().is_success() {
            return Err(());
        }
        let body: T = bounded_json(response).map_err(|_| ())?;
        T::cache_mut(self).store(
            id,
            endpoint,
            body.clone(),
            &headers,
            Instant::now(),
            SystemTime::now(),
        );
        Ok(Some(body))
    }

    fn backoff(&mut self, id: u64) {
        if let Some(account) = self.accounts.get_mut(&id) {
            account.failures = account.failures.saturating_add(1);
            let ceiling = 2_u64.saturating_pow(account.failures.min(6));
            let jitter = u64::from(rand::random::<u8>()) % 3;
            let delay = Duration::from_secs((ceiling + jitter).min(60));
            account.next_location =
                (Instant::now() + delay).max(self.sso_pause_until.unwrap_or(Instant::now()));
            self.emit(WorkerEvent::Health {
                character_id: Some(id),
                health: Health::Backoff(delay.as_secs()),
            });
        }
    }

    fn pause_sso(&mut self, response: &Response) {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        self.sso_pause_until = Some(Instant::now() + Duration::from_secs(seconds));
    }
    fn emit(&self, event: WorkerEvent) {
        if !self.pending_events.borrow().is_empty() {
            self.queue_event(event);
            return;
        }
        match self.events.try_send(event) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {}
            Err(mpsc::TrySendError::Full(event)) => self.queue_event(event),
        }
    }

    fn finish_authorization(&self) {
        if self.authorization_active.swap(false, Ordering::AcqRel) {
            self.emit(WorkerEvent::AuthorizationFinished);
        }
    }

    fn queue_event(&self, event: WorkerEvent) {
        let mut queue = self.pending_events.borrow_mut();
        if queue.len() >= 64 {
            if let Some(position) = queue
                .iter()
                .position(|queued| matches!(queued, WorkerEvent::Health { .. }))
            {
                queue.remove(position);
            } else {
                tracing::warn!("ESI event queue exhausted; terminal event could not be delivered");
                return;
            }
        }
        queue.push_back(event);
    }

    fn flush_events(&self) {
        loop {
            let Some(event) = self.pending_events.borrow_mut().pop_front() else {
                break;
            };
            match self.events.try_send(event) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => break,
                Err(mpsc::TrySendError::Full(event)) => {
                    self.pending_events.borrow_mut().push_front(event);
                    break;
                }
            }
        }
    }
}

trait CacheType: Sized {
    fn cache(worker: &Worker) -> &ResponseCache<Self>;
    fn cache_mut(worker: &mut Worker) -> &mut ResponseCache<Self>;
}
impl CacheType for Location {
    fn cache(worker: &Worker) -> &ResponseCache<Self> {
        &worker.locations
    }
    fn cache_mut(worker: &mut Worker) -> &mut ResponseCache<Self> {
        &mut worker.locations
    }
}
impl CacheType for Vec<u32> {
    fn cache(worker: &Worker) -> &ResponseCache<Self> {
        &worker.implants
    }
    fn cache_mut(worker: &mut Worker) -> &mut ResponseCache<Self> {
        &mut worker.implants
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Location {
    solar_system_id: u64,
    station_id: Option<u64>,
    structure_id: Option<u64>,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}
enum RefreshError {
    Revoked,
    Transient(Option<String>),
}

fn bounded_json<T: for<'de> Deserialize<'de>>(response: Response) -> Result<T, AuthError> {
    if !response.status().is_success() {
        return Err(AuthError::Unavailable);
    }
    let bytes = response.bytes().map_err(|_| AuthError::Unavailable)?;
    if bytes.len() > 1_048_576 {
        return Err(AuthError::Unavailable);
    }
    serde_json::from_slice(&bytes).map_err(|_| AuthError::Unavailable)
}
fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}
fn release_config() -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    Some((
        option_env!("EPM_ESI_CLIENT_ID")?,
        option_env!("EPM_ESI_CALLBACK")?,
        option_env!("EPM_MAINTAINER_CONTACT")?,
        option_env!("EPM_ESI_COMPATIBILITY_DATE")?,
    ))
}
fn keyring_available() -> bool {
    matches!(
        Entry::new(KEYRING_SERVICE, "availability-probe").and_then(|entry| entry.get_password()),
        Ok(_) | Err(keyring::Error::NoEntry)
    )
}
fn read_refresh(id: u64) -> Result<Option<RefreshToken>, ()> {
    match Entry::new(KEYRING_SERVICE, &id.to_string()).and_then(|entry| entry.get_password()) {
        Ok(token) => Ok(Some(RefreshToken(token))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(()),
    }
}
fn store_refresh(id: u64, token: &str) -> bool {
    Entry::new(KEYRING_SERVICE, &id.to_string())
        .and_then(|entry| entry.set_password(token))
        .is_ok()
}
fn delete_refresh(id: u64) -> bool {
    match Entry::new(KEYRING_SERVICE, &id.to_string()).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => true,
        Err(_) => false,
    }
}

fn esi_headers(access: &AccessToken, contact: &str, compatibility: &str) -> Result<HeaderMap, ()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&format!(
            "EvePreviewManager/{} ({}; +https://github.com/h0lylag/EVE-Preview-Manager)",
            env!("CARGO_PKG_VERSION"),
            contact
        ))
        .map_err(|_| ())?,
    );
    headers.insert(
        "X-Compatibility-Date",
        HeaderValue::from_str(compatibility).map_err(|_| ())?,
    );
    headers.insert(AUTHORIZATION, access.bearer()?);
    Ok(headers)
}

fn parse_callback(stream: &mut TcpStream, expected_path: &str) -> Option<(String, String)> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut buffer = [0_u8; 8192];
    let size = stream.read(&mut buffer).ok()?;
    if size == buffer.len() {
        return None;
    }
    parse_callback_request(&buffer[..size], expected_path)
}

fn parse_callback_request(request: &[u8], expected_path: &str) -> Option<(String, String)> {
    let request = std::str::from_utf8(request).ok()?;
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = Url::parse(&format!("http://127.0.0.1{}", parts.next()?)).ok()?;
    if target.path() != expected_path {
        return None;
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in target.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if state.is_none() => state = Some(value.into_owned()),
            "code" | "state" => return None,
            _ => {}
        }
    }
    Some((code?, state?))
}
fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!("<html><body>{message}</body></html>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_rejects_duplicate_state_and_wrong_method() {
        fn parse(raw: &str) -> Option<(String, String)> {
            parse_callback_request(raw.as_bytes(), "/callback")
        }
        assert_eq!(
            parse("GET /callback?code=a&state=b HTTP/1.1\r\n\r\n"),
            Some(("a".into(), "b".into()))
        );
        assert!(parse("GET /callback?code=a&state=b&state=c HTTP/1.1\r\n\r\n").is_none());
        assert!(parse("POST /callback?code=a&state=b HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn esi_headers_are_exact_and_authorization_is_redacted() {
        let headers = esi_headers(
            &AccessToken("token-sentinel".into()),
            "maintainer@example.test",
            "2026-09-01",
        )
        .unwrap();
        assert_eq!(headers["X-Compatibility-Date"], "2026-09-01");
        assert_eq!(
            headers[USER_AGENT],
            format!(
                "EvePreviewManager/{} (maintainer@example.test; +https://github.com/h0lylag/EVE-Preview-Manager)",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(!format!("{headers:?}").contains("token-sentinel"));
    }

    #[test]
    fn overflow_commands_preserve_fifo_order() {
        let (wake, _wake_rx) = mpsc::sync_channel(1);
        let (event_tx, events) = mpsc::sync_channel(1);
        let pending_commands = Arc::new(Mutex::new(VecDeque::new()));
        let worker = EsiWorker {
            wake,
            events,
            handle: None,
            epoch: Arc::new(AtomicU64::new(1)),
            shutdown: Arc::new(AtomicBool::new(false)),
            config_generation: Arc::new(AtomicU64::new(0)),
            pending_commands: pending_commands.clone(),
        };
        let authorize = || WorkerCommand::Authorize {
            expected_character_id: None,
            session_only: true,
        };

        assert!(worker.send(authorize()));
        assert!(worker.send(authorize()));
        assert!(worker.send(WorkerCommand::CancelAuthorization));

        let mut pending = pending_commands.lock().unwrap();
        assert!(matches!(
            pending.pop_front(),
            Some(WorkerCommand::Authorize { .. })
        ));
        assert!(matches!(
            pending.pop_front(),
            Some(WorkerCommand::Authorize { .. })
        ));
        assert!(matches!(
            pending.pop_front(),
            Some(WorkerCommand::CancelAuthorization)
        ));
        drop(pending);
        drop(event_tx);
    }

    #[test]
    fn authorization_completion_is_emitted_once() {
        let (_wake_tx, wake_rx) = mpsc::sync_channel(1);
        let (events, event_rx) = mpsc::sync_channel(4);
        let worker = Worker::new(
            wake_rx,
            events,
            Arc::new(AtomicU64::new(1)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(1)),
            Arc::new(Mutex::new(VecDeque::new())),
        );
        worker.authorization_active.store(true, Ordering::Release);

        worker.finish_authorization();
        worker.finish_authorization();

        assert!(matches!(
            event_rx.try_recv(),
            Ok(WorkerEvent::AuthorizationFinished)
        ));
        assert!(event_rx.try_recv().is_err());
    }
}
