//! Tail EVE's UTF-8 combat Gamelogs without replaying the existing archive.
//!
//! The monitor deliberately has no X11 dependency.  The daemon owns the
//! receiver end of the bounded Tokio channel and decides whether an event is
//! enabled/focused; this module only identifies which character's own log
//! produced it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RESCAN_INTERVAL: Duration = Duration::from_secs(60);
const DAMAGE_COALESCE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertKind {
    Damage,
    Decloak,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertEvent {
    pub character: String,
    pub kind: AlertKind,
    pub source_generation: u64,
}

/// A cheap, manager-safe description of what the monitor is currently seeing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamelogStatus {
    pub logs_dir: Option<PathBuf>,
    pub directory_exists: bool,
    pub scanning: bool,
    pub file_count: usize,
    pub active_file_count: usize,
    pub unmatched_characters: Vec<String>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Control {
    override_dir: Option<PathBuf>,
    /// None means accept every Listener (useful before the daemon knows tiles).
    tracked: Option<HashSet<String>>,
    generation: u64,
}

/// Thread-safe controls retained by the daemon after starting the monitor.
#[derive(Clone)]
pub struct GamelogMonitorHandle {
    control: Arc<Mutex<Control>>,
    status: Arc<Mutex<GamelogStatus>>,
}

impl GamelogMonitorHandle {
    pub fn set_directory_override(&self, directory: Option<PathBuf>) {
        let mut control = self.control.lock().expect("gamelog control poisoned");
        if control.override_dir != directory {
            control.override_dir = directory;
            control.generation = control.generation.wrapping_add(1);
        }
    }

    /// Restrict events to live EVE character previews.  `None` accepts all
    /// listeners; an empty iterator accepts none.
    pub fn set_tracked_characters<I, S>(&self, names: Option<I>)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let tracked = names.map(|names| {
            names
                .into_iter()
                .map(|name| name.as_ref().trim().to_lowercase())
                .filter(|name| !name.is_empty())
                .collect()
        });
        self.control
            .lock()
            .expect("gamelog control poisoned")
            .tracked = tracked;
    }

    pub fn status(&self) -> GamelogStatus {
        self.status.lock().expect("gamelog status poisoned").clone()
    }

    pub fn generation(&self) -> u64 {
        self.control
            .lock()
            .expect("gamelog control poisoned")
            .generation
    }
}

#[derive(Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

struct TailFile {
    offset: u64,
    identity: FileIdentity,
    listener: Option<String>,
    pending: Vec<u8>,
}

/// A pollable monitor. `start` runs it in a small OS thread so synchronous
/// filesystem calls cannot delay the daemon's current-thread Tokio/X11 loop.
pub struct GamelogMonitor {
    control: Arc<Mutex<Control>>,
    status: Arc<Mutex<GamelogStatus>>,
    stopped: Arc<AtomicBool>,
    events: Sender<AlertEvent>,
    files: HashMap<PathBuf, TailFile>,
    current_dir: Option<PathBuf>,
    applied_generation: u64,
    last_scan: Option<Instant>,
    last_dir_modified: Option<SystemTime>,
    last_damage: HashMap<String, Instant>,
}

impl GamelogMonitor {
    pub fn new(
        events: Sender<AlertEvent>,
        directory_override: Option<PathBuf>,
    ) -> (Self, GamelogMonitorHandle) {
        let control = Arc::new(Mutex::new(Control {
            override_dir: directory_override,
            ..Control::default()
        }));
        let status = Arc::new(Mutex::new(GamelogStatus::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = GamelogMonitorHandle {
            control: control.clone(),
            status: status.clone(),
        };
        (
            Self {
                control,
                status,
                stopped,
                events,
                files: HashMap::new(),
                current_dir: None,
                applied_generation: u64::MAX,
                last_scan: None,
                last_dir_modified: None,
                last_damage: HashMap::new(),
            },
            handle,
        )
    }

    pub fn start(mut self) -> GamelogMonitorHandle {
        let handle = GamelogMonitorHandle {
            control: self.control.clone(),
            status: self.status.clone(),
        };
        thread::spawn(move || {
            while !self.stopped.load(Ordering::Acquire) {
                self.poll_once();
                thread::sleep(POLL_INTERVAL);
            }
        });
        handle
    }

    /// One synchronous pass, exposed for deterministic daemon integration/tests.
    pub fn poll_once(&mut self) {
        let (override_dir, tracked, generation) = {
            let control = self.control.lock().expect("gamelog control poisoned");
            (
                control.override_dir.clone(),
                control.tracked.clone(),
                control.generation,
            )
        };
        let directory = override_dir.or_else(auto_detect_gamelogs_dir);
        if generation != self.applied_generation || directory != self.current_dir {
            self.files.clear();
            self.last_scan = None;
            self.last_dir_modified = None;
            self.current_dir = directory.clone();
            self.applied_generation = generation;
        }
        let Some(directory) = directory else {
            self.publish_status(None, false, tracked.as_ref(), None);
            return;
        };
        let dir_meta = match fs::metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) => {
                let exists = directory.exists();
                self.publish_status(
                    Some(directory),
                    exists,
                    tracked.as_ref(),
                    Some(error.to_string()),
                );
                return;
            }
        };
        if !dir_meta.is_dir() {
            self.publish_status(
                Some(directory),
                true,
                tracked.as_ref(),
                Some("path is not a directory".into()),
            );
            return;
        }
        let now = Instant::now();
        let dir_modified = dir_meta.modified().ok();
        let must_scan = self.last_scan.is_none()
            || self
                .last_scan
                .is_some_and(|at| now.duration_since(at) >= RESCAN_INTERVAL)
            || dir_modified != self.last_dir_modified;
        if must_scan {
            if let Err(error) = self.discover(&directory) {
                self.publish_status(
                    Some(directory),
                    true,
                    tracked.as_ref(),
                    Some(error.to_string()),
                );
                return;
            }
            self.last_scan = Some(now);
            self.last_dir_modified = dir_modified;
        }
        let paths: Vec<_> = self.files.keys().cloned().collect();
        for path in paths {
            self.tail_file(&path, tracked.as_ref());
        }
        self.publish_status(Some(directory), true, tracked.as_ref(), None);
    }

    fn discover(&mut self, directory: &Path) -> std::io::Result<()> {
        let initial_scan = self.last_scan.is_none();
        self.files.retain(|path, _| path.is_file());
        let entries = fs::read_dir(directory)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("txt"))
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let identity = identity(&meta);
            if let std::collections::hash_map::Entry::Vacant(entry) = self.files.entry(path.clone())
            {
                // Initial EOF seeding is intentional: old combat must never flash.
                let listener = read_listener(&path);
                entry.insert(TailFile {
                    // Existing files are historical on startup. A file created
                    // after monitoring began is read from its header so an event
                    // written between directory polls is not lost.
                    offset: if initial_scan { identity.len } else { 0 },
                    identity,
                    listener,
                    pending: Vec::new(),
                });
            }
        }

        // Only the newest session file per Listener can be live. This keeps the
        // one-second poll proportional to logged-in clients, not archive size.
        let mut newest: HashMap<String, (PathBuf, Option<SystemTime>)> = HashMap::new();
        for (path, tail) in &self.files {
            let Some(listener) = &tail.listener else {
                continue;
            };
            let key = listener.trim().to_lowercase();
            let replace = newest
                .get(&key)
                .is_none_or(|(_, modified)| tail.identity.modified > *modified);
            if replace {
                newest.insert(key, (path.clone(), tail.identity.modified));
            }
        }
        let live: HashSet<_> = newest.into_values().map(|(path, _)| path).collect();
        self.files
            .retain(|path, tail| tail.listener.is_none() || live.contains(path));
        Ok(())
    }

    fn tail_file(&mut self, path: &Path, tracked: Option<&HashSet<String>>) {
        let Some(tail) = self.files.get_mut(path) else {
            return;
        };
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let fresh_identity = identity(&meta);
        let rotated =
            !same_file(&tail.identity, &fresh_identity) || fresh_identity.len < tail.offset;
        if rotated {
            // A replacement's existing bytes are historical just like initial seed.
            tail.offset = fresh_identity.len;
            tail.identity = fresh_identity;
            tail.pending.clear();
            tail.listener = read_listener(path);
            return;
        }
        if fresh_identity.len == tail.offset {
            tail.identity = fresh_identity;
            return;
        }
        if tail.listener.is_none() {
            tail.listener = read_listener(path);
        }
        let Some(character) = tail.listener.clone() else {
            return;
        };
        // Do not consume bytes for a character that is not previewed yet. A
        // newly created preview can join on the next status refresh without
        // losing the first damage/decloak lines written in the meantime.
        if !is_tracked(&character, tracked) {
            return;
        }
        let Ok(mut file) = fs::File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(tail.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        tail.offset = tail.offset.saturating_add(bytes.len() as u64);
        tail.identity = fresh_identity;
        tail.pending.extend(bytes);
        let complete_at = tail.pending.iter().rposition(|byte| *byte == b'\n');
        let Some(complete_at) = complete_at else {
            return;
        };
        let complete: Vec<u8> = tail.pending.drain(..=complete_at).collect();
        for line in String::from_utf8_lossy(&complete).lines() {
            if !event_is_recent(line, chrono::Utc::now()) {
                continue;
            }
            if parse_damage_line(line) {
                let permitted = self
                    .last_damage
                    .get(&character)
                    .is_none_or(|last| last.elapsed() >= DAMAGE_COALESCE);
                if permitted {
                    self.last_damage.insert(character.clone(), Instant::now());
                    let _ = self.events.try_send(AlertEvent {
                        character: character.clone(),
                        kind: AlertKind::Damage,
                        source_generation: self.applied_generation,
                    });
                }
            }
            if parse_decloak_line(line) {
                let _ = self.events.try_send(AlertEvent {
                    character: character.clone(),
                    kind: AlertKind::Decloak,
                    source_generation: self.applied_generation,
                });
            }
        }
    }

    fn publish_status(
        &self,
        directory: Option<PathBuf>,
        exists: bool,
        tracked: Option<&HashSet<String>>,
        error: Option<String>,
    ) {
        let mut present = HashSet::new();
        let mut active = 0;
        for file in self.files.values() {
            if let Some(listener) = &file.listener {
                present.insert(listener.to_lowercase());
                if is_tracked(listener, tracked) {
                    active += 1;
                }
            }
        }
        let mut unmatched: Vec<_> = tracked
            .into_iter()
            .flatten()
            .filter(|name| !present.contains(*name))
            .cloned()
            .collect();
        unmatched.sort();
        *self.status.lock().expect("gamelog status poisoned") = GamelogStatus {
            logs_dir: directory,
            directory_exists: exists,
            scanning: exists,
            file_count: self.files.len(),
            active_file_count: active,
            unmatched_characters: unmatched,
            error,
        };
    }
}

fn identity(meta: &fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            len: meta.len(),
            modified: meta.modified().ok(),
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        FileIdentity {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

fn same_file(a: &FileIdentity, b: &FileIdentity) -> bool {
    #[cfg(unix)]
    {
        a.dev == b.dev && a.ino == b.ino
    }
    #[cfg(not(unix))]
    {
        a.modified == b.modified || a.len <= b.len
    }
}

fn is_tracked(character: &str, tracked: Option<&HashSet<String>>) -> bool {
    tracked.is_none_or(|names| names.contains(&character.trim().to_lowercase()))
}

fn read_listener(path: &Path) -> Option<String> {
    let mut bytes = Vec::with_capacity(4096);
    fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut bytes)
        .ok()?;
    let complete_end = bytes.iter().rposition(|byte| *byte == b'\n')? + 1;
    let text = std::str::from_utf8(&bytes[..complete_end]).ok()?;
    if !text.contains("Gamelog") {
        return None;
    }
    text.split_inclusive('\n').find_map(|line| {
        line.ends_with('\n')
            .then_some(line)?
            .trim_end_matches(['\r', '\n'])
            .trim_start_matches('\u{feff}')
            .trim_start()
            .strip_prefix("Listener:")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// True for a positive, incoming English `(combat)` hit.  Markup is stripped
/// first because EVE nests `<color>`/`<font>` tags differently between builds.
pub fn parse_damage_line(line: &str) -> bool {
    let plain = strip_markup(line);
    let lower = plain.to_ascii_lowercase();
    let Some(combat) = lower.find("(combat)") else {
        return false;
    };
    let after = &plain[combat + "(combat)".len()..];
    let lower_after = after.to_ascii_lowercase();
    let Some(from) = word_index(&lower_after, "from") else {
        return false;
    };
    // An outgoing line's `to` occurs before any incoming `from` clause.
    if word_index(&lower_after[..from], "to").is_some() {
        return false;
    }
    let amount = after[..from].trim();
    if amount.is_empty()
        || !amount
            .chars()
            .all(|character| character.is_ascii_digit() || character == ',')
    {
        return false;
    }
    amount
        .replace(',', "")
        .parse::<u64>()
        .ok()
        .is_some_and(|number| number > 0)
}

/// English-only exact notification; failed cloak activations intentionally do
/// not contain this wording.
pub fn parse_decloak_line(line: &str) -> bool {
    let lower = strip_markup(line).to_ascii_lowercase();
    lower.contains("(notify)") && lower.contains("your cloak deactivates due to")
}

fn strip_markup(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn word_index(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word).find_map(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + word.len()..].chars().next();
        (!before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric()))
        .then_some(index)
    })
}

fn event_is_recent(line: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(end) = line.find(']') else {
        return true;
    };
    let Some(timestamp) = line.get(1..end).map(str::trim) else {
        return true;
    };
    let Ok(naive) = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y.%m.%d %H:%M:%S") else {
        return true;
    };
    let age = now.signed_duration_since(naive.and_utc());
    age >= chrono::Duration::seconds(-10) && age <= chrono::Duration::minutes(2)
}

/// Candidate order covers native Linux, Steam's Proton prefix for app 8500,
/// and conventional Wine prefixes. Equivalent `.steam`/XDG Steam spellings are
/// canonicalized/deduplicated where the directory already exists.
pub fn gamelog_candidates(home: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![home.join("Documents/EVE/logs/Gamelogs")];
    let steam_roots = [
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ];
    for steam in steam_roots {
        candidates.push(steam.join(
            "steamapps/compatdata/8500/pfx/drive_c/users/steamuser/Documents/EVE/logs/Gamelogs",
        ));
        candidates.push(
            steam
                .join("steamapps/compatdata/8500/pfx/drive_c/users")
                .join(whoami())
                .join("Documents/EVE/logs/Gamelogs"),
        );
    }
    if let Some(prefix) = std::env::var_os("WINEPREFIX") {
        candidates.push(
            PathBuf::from(prefix)
                .join("drive_c/users")
                .join(whoami())
                .join("Documents/EVE/logs/Gamelogs"),
        );
    }
    candidates.push(
        home.join(".wine/drive_c/users")
            .join(whoami())
            .join("Documents/EVE/logs/Gamelogs"),
    );
    candidates.push(
        home.join("Games/eve-online/drive_c/users")
            .join(whoami())
            .join("Documents/EVE/logs/Gamelogs"),
    );
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| {
            let key = candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone());
            seen.insert(key)
        })
        .collect()
}

pub fn auto_detect_gamelogs_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    gamelog_candidates(&home)
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

fn whoami() -> String {
    std::env::var("USER")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "steamuser".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    const HEADER: &str = "\u{feff}Gamelog\n  Listener: Ada Lovelace\n";
    const INCOMING: &str = "[ 2026.09.11 12:00:00 ] (combat) <color=0xff><b>42</b></color> from <b><color=x>NPC</color></b>\n";

    #[test]
    fn parsers_accept_real_shapes_and_reject_near_misses() {
        assert!(parse_damage_line(INCOMING));
        assert!(!parse_damage_line("[x] (combat) <b>42</b> to Some NPC"));
        assert!(!parse_damage_line("[x] (combat) <b>0</b> from NPC"));
        assert!(!parse_damage_line("[x] (mining) <b>42</b> from Veldspar"));
        assert!(!parse_damage_line("[x] (combat) from NPC"));
        assert!(!parse_damage_line("[x] (combat) -7 from NPC"));
        assert!(!parse_damage_line("[x] (combat) bogus 7 from NPC"));
        assert!(parse_decloak_line(
            "[x] (notify) Your cloak deactivates due to proximity to a nearby Stargate."
        ));
        assert!(parse_decloak_line(
            "[x] (notify) Your cloak deactivates due to a pulse from a Mobile Observatory."
        ));
        assert!(!parse_decloak_line(
            "[x] (notify) Your cloaking systems are unable to activate due to proximity."
        ));
    }

    #[test]
    fn seeds_existing_content_and_tails_complete_utf8_lines() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("one.txt");
        fs::write(&path, format!("{HEADER}{INCOMING}")).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let (mut monitor, handle) = GamelogMonitor::new(tx, Some(temp.path().into()));
        handle.set_tracked_characters(Some(["Ada Lovelace"]));
        monitor.poll_once();
        assert!(rx.try_recv().is_err());
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "[x] (combat) <b>7</b> from caf").unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert!(rx.try_recv().is_err());
        writeln!(file, "é").unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert_eq!(rx.try_recv().unwrap().kind, AlertKind::Damage);
    }

    #[test]
    fn tracks_listener_and_handles_truncate_without_replay() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("one.txt");
        fs::write(&path, HEADER).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let (mut monitor, handle) = GamelogMonitor::new(tx, Some(temp.path().into()));
        handle.set_tracked_characters(Some(["Other"]));
        monitor.poll_once();
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{INCOMING}").unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert!(rx.try_recv().is_err());
        handle.set_tracked_characters(Some(["Ada Lovelace"]));
        writeln!(
            file,
            "[x] (notify) Your cloak deactivates due to a pulse from a Mobile Observatory."
        )
        .unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert_eq!(rx.try_recv().unwrap().kind, AlertKind::Decloak);
        fs::write(&path, format!("{HEADER}{INCOMING}")).unwrap();
        monitor.poll_once();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn completes_a_header_created_in_parts() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("new.txt");
        fs::write(&path, "Game").unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let (mut monitor, handle) = GamelogMonitor::new(tx, Some(temp.path().into()));
        handle.set_tracked_characters(Some(["Ada Lovelace"]));
        monitor.poll_once();

        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "log\n  Listener: Ada Lovelace\n[x] (notify) Your cloak deactivates due to proximity to an object.").unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert_eq!(rx.try_recv().unwrap().kind, AlertKind::Decloak);
    }

    #[test]
    fn does_not_cache_an_unterminated_listener_name() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("partial-listener.txt");
        fs::write(&path, "Gamelog\nListener: Ada Love").unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let (mut monitor, handle) = GamelogMonitor::new(tx, Some(temp.path().into()));
        handle.set_tracked_characters(Some(["Ada Lovelace"]));
        monitor.poll_once();

        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(
            file,
            "lace\n[x] (notify) Your cloak deactivates due to proximity to an object."
        )
        .unwrap();
        file.flush().unwrap();
        monitor.poll_once();
        assert_eq!(rx.try_recv().unwrap().kind, AlertKind::Decloak);
    }

    #[test]
    fn candidates_deduplicate_steam_aliases() {
        let temp = tempdir().unwrap();
        let home = temp.path();
        fs::create_dir_all(home.join(".local/share")).unwrap();
        std::os::unix::fs::symlink(home.join(".local/share"), home.join(".steam")).unwrap();
        let candidates = gamelog_candidates(home);
        let canonical: HashSet<_> = candidates
            .iter()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
            .collect();
        assert_eq!(candidates.len(), canonical.len());
    }

    #[test]
    fn stale_timestamps_are_rejected_and_unknown_shapes_fail_open() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-11T12:05:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(!event_is_recent(
            "[ 2026.09.11 12:00:00 ] (combat) 7 from NPC",
            now
        ));
        assert!(event_is_recent("[x] (combat) 7 from NPC", now));
    }
}
