use std::time::{Duration, Instant};

pub const DAMAGE_HOLD: Duration = Duration::from_secs(5);
pub const DAMAGE_REARM: Duration = Duration::from_secs(3);
pub const DECLOAK_HOLD: Duration = Duration::from_secs(10);
pub const PULSE_PERIOD: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
pub struct AlertVisual {
    pub color: x11rb::protocol::render::Color,
    pub border_size: u16,
    pub decloak_banner: bool,
}

#[derive(Debug, Default, Clone)]
pub struct AlertState {
    damage_until: Option<Instant>,
    damage_since: Option<Instant>,
    decloak_until: Option<Instant>,
    decloak_since: Option<Instant>,
    last_damage_trigger: Option<Instant>,
}

impl AlertState {
    pub fn trigger_damage(&mut self, now: Instant) -> bool {
        // Every hit extends the rolling hold, even while the visual edge is
        // inside its re-arm cooldown.
        if !self.damage_active(now) {
            self.damage_since = Some(now);
        }
        self.damage_until = Some(now + DAMAGE_HOLD);
        if self
            .last_damage_trigger
            .is_some_and(|last| now.saturating_duration_since(last) < DAMAGE_REARM)
        {
            return false;
        }
        self.last_damage_trigger = Some(now);
        true
    }

    pub fn trigger_decloak(&mut self, now: Instant) {
        self.decloak_until = Some(now + DECLOAK_HOLD);
        self.decloak_since = Some(now);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn clear_damage(&mut self) {
        self.damage_until = None;
        self.damage_since = None;
        self.last_damage_trigger = None;
    }

    pub fn clear_decloak(&mut self) {
        self.decloak_until = None;
        self.decloak_since = None;
    }

    pub fn is_active(&self, now: Instant) -> bool {
        self.damage_active(now) || self.decloak_active(now)
    }

    pub fn visual(&self, now: Instant) -> Option<AlertVisual> {
        let damage = self.damage_active(now);
        let decloak = self.decloak_active(now);
        if !damage && !decloak {
            return None;
        }

        let started = if damage {
            self.damage_since.unwrap_or(now)
        } else {
            self.decloak_since.unwrap_or(now)
        };
        let intensity = 0.45 + 0.55 * pulse(now.saturating_duration_since(started));
        let scaled = |channel: u16| (channel as f32 * intensity).round() as u16;
        let color = if damage {
            // FCtool #ff3b30
            x11rb::protocol::render::Color {
                red: scaled(0xffff),
                green: scaled(0x3b3b),
                blue: scaled(0x3030),
                alpha: 0xffff,
            }
        } else {
            // FCtool #ffcc00
            x11rb::protocol::render::Color {
                red: scaled(0xffff),
                green: scaled(0xcccc),
                blue: 0,
                alpha: 0xffff,
            }
        };
        Some(AlertVisual {
            color,
            border_size: 2,
            decloak_banner: decloak,
        })
    }

    fn damage_active(&self, now: Instant) -> bool {
        self.damage_until.is_some_and(|until| now < until)
    }

    fn decloak_active(&self, now: Instant) -> bool {
        self.decloak_until.is_some_and(|until| now < until)
    }
}

fn pulse(elapsed: Duration) -> f32 {
    let phase = (elapsed.as_secs_f64() % PULSE_PERIOD.as_secs_f64()) / PULSE_PERIOD.as_secs_f64();
    (0.5 + 0.5 * (std::f64::consts::TAU * phase).cos()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_cooldown_and_precedence() {
        let start = Instant::now();
        let mut state = AlertState::default();
        assert!(state.trigger_damage(start));
        assert!(!state.trigger_damage(start + Duration::from_secs(2)));
        state.trigger_decloak(start);
        let both = state.visual(start + Duration::from_millis(100)).unwrap();
        assert!(both.color.red > both.color.green);
        assert_eq!(both.color.alpha, 0xffff);
        assert!(both.decloak_banner);
        assert!(state.trigger_damage(start + DAMAGE_REARM));
        assert!(state.is_active(start + Duration::from_secs(6)));
        let decloak_only = state.visual(start + Duration::from_secs(9)).unwrap();
        assert!(decloak_only.color.green > 0);
        assert_eq!(decloak_only.color.alpha, 0xffff);
        assert!(state.visual(start + DECLOAK_HOLD).is_none());
    }

    #[test]
    fn clear_removes_all_alerts_and_cooldown() {
        let now = Instant::now();
        let mut state = AlertState::default();
        state.trigger_damage(now);
        state.trigger_decloak(now);
        state.clear();
        assert!(!state.is_active(now));
        assert!(state.trigger_damage(now));
    }
}
