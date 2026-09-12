use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockIdentity {
    pub solar_system_id: u64,
    pub station_id: Option<u64>,
    pub structure_id: Option<u64>,
}

impl DockIdentity {
    pub fn new(
        solar_system_id: u64,
        station_id: Option<u64>,
        structure_id: Option<u64>,
    ) -> Option<Self> {
        (station_id.is_some() || structure_id.is_some()).then_some(Self {
            solar_system_id,
            station_id,
            structure_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Fire,
    Hold,
    Primed,
    Clear,
    Suppressed,
}

#[derive(Debug)]
struct CharacterLatch {
    latched: bool,
    attempts: u8,
    dock: Option<DockIdentity>,
    seen: Instant,
}

#[derive(Debug, Default)]
pub struct ReminderLatches {
    characters: HashMap<u64, CharacterLatch>,
    snoozed: HashSet<u64>,
}

impl ReminderLatches {
    pub const BLIND_GAP: Duration = Duration::from_secs(60);
    pub const MAX_IMPLANT_ATTEMPTS: u8 = 3;

    pub fn observe(
        &mut self,
        character_id: u64,
        dock: Option<DockIdentity>,
        disabled: bool,
        now: Instant,
    ) -> Observation {
        let first = !self.characters.contains_key(&character_id);
        let latch = self
            .characters
            .entry(character_id)
            .or_insert(CharacterLatch {
                latched: false,
                attempts: 0,
                dock: None,
                seen: now,
            });
        let gap = now.saturating_duration_since(latch.seen);
        latch.seen = now;

        let Some(dock) = dock else {
            latch.latched = false;
            latch.attempts = 0;
            latch.dock = None;
            return Observation::Clear;
        };
        if disabled || self.snoozed.contains(&character_id) {
            latch.latched = true;
            latch.dock = Some(dock);
            return Observation::Suppressed;
        }
        if first {
            latch.latched = true;
            latch.dock = Some(dock);
            return Observation::Primed;
        }
        if latch.latched {
            let moved = latch.dock.is_some_and(|previous| previous != dock);
            if !moved && gap <= Self::BLIND_GAP {
                return Observation::Hold;
            }
            latch.attempts = 0;
        }
        latch.latched = true;
        latch.dock = Some(dock);
        latch.attempts = latch.attempts.saturating_add(1);
        Observation::Fire
    }

    pub fn implant_failed(&mut self, character_id: u64) -> bool {
        let Some(latch) = self.characters.get_mut(&character_id) else {
            return false;
        };
        if latch.attempts >= Self::MAX_IMPLANT_ATTEMPTS {
            return false;
        }
        latch.latched = false;
        true
    }

    pub fn forget(&mut self, character_id: u64) {
        self.characters.remove(&character_id);
    }

    pub fn prime_all(&mut self) {
        self.characters.clear();
    }

    pub fn snooze(&mut self, character_id: u64) {
        self.snoozed.insert(character_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(id: u64) -> Option<DockIdentity> {
        DockIdentity::new(30_000_142, Some(id), None)
    }
    fn structure(id: u64) -> Option<DockIdentity> {
        DockIdentity::new(30_000_142, None, Some(id))
    }

    #[test]
    fn primes_then_rearms_on_undock_different_dock_and_gap() {
        let start = Instant::now();
        let mut state = ReminderLatches::default();
        assert_eq!(
            state.observe(1, station(10), false, start),
            Observation::Primed
        );
        assert_eq!(
            state.observe(1, station(10), false, start + Duration::from_secs(10)),
            Observation::Hold
        );
        assert_eq!(
            state.observe(1, None, false, start + Duration::from_secs(20)),
            Observation::Clear
        );
        assert_eq!(
            state.observe(1, structure(20), false, start + Duration::from_secs(30)),
            Observation::Fire
        );
        assert_eq!(
            state.observe(1, structure(21), false, start + Duration::from_secs(40)),
            Observation::Fire
        );
        assert_eq!(
            state.observe(1, structure(21), false, start + Duration::from_secs(101)),
            Observation::Fire
        );
    }

    #[test]
    fn unknown_location_does_not_count_as_undock() {
        let start = Instant::now();
        let mut state = ReminderLatches::default();
        state.observe(1, station(10), false, start);
        // Absence/failure is represented by not calling observe at all.
        assert_eq!(
            state.observe(1, station(10), false, start + Duration::from_secs(20)),
            Observation::Hold
        );
    }

    #[test]
    fn retry_budget_is_three_per_dock() {
        let start = Instant::now();
        let mut state = ReminderLatches::default();
        state.observe(1, None, false, start);
        for attempt in 0..3 {
            assert_eq!(
                state.observe(
                    1,
                    station(10),
                    false,
                    start + Duration::from_secs(attempt + 1)
                ),
                Observation::Fire
            );
            assert_eq!(state.implant_failed(1), attempt < 2);
        }
        assert_eq!(
            state.observe(1, station(10), false, start + Duration::from_secs(10)),
            Observation::Hold
        );
    }

    #[test]
    fn opt_out_and_snooze_prime_without_retroactive_fire() {
        let start = Instant::now();
        let mut state = ReminderLatches::default();
        assert_eq!(
            state.observe(1, station(10), true, start),
            Observation::Suppressed
        );
        assert_eq!(
            state.observe(1, station(10), false, start + Duration::from_secs(1)),
            Observation::Hold
        );
        state.snooze(2);
        assert_eq!(
            state.observe(2, station(10), false, start),
            Observation::Suppressed
        );
    }
}
