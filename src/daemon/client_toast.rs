use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const WIDTH: u16 = 430;
const HEIGHT: u16 = 92;
const HOLD: Duration = Duration::from_secs(12);
const FADE: Duration = Duration::from_millis(500);
const OPACITY_94: u32 = (u32::MAX as u64 * 94 / 100) as u32;

#[derive(Clone, Copy)]
pub enum ToastClick {
    Dismiss,
    Snooze(u64),
}

pub struct ClientToast {
    window: Window,
    gc: Gcontext,
    font: Font,
    opacity_atom: Atom,
    shown_at: Instant,
    character_id: u64,
}

impl ClientToast {
    pub fn show(
        conn: &RustConnection,
        screen: &Screen,
        target: Window,
        character_id: u64,
        character_name: &str,
        implant_names: &[String],
    ) -> Result<Self> {
        conn.get_geometry(target)?
            .reply()
            .context("EVE window unavailable")?;
        let translated = conn
            .translate_coordinates(target, screen.root, 0, 0)?
            .reply()?;
        let root_geometry = conn.get_geometry(screen.root)?.reply()?;
        let x = (translated.dst_x + 18).clamp(0, root_geometry.width.saturating_sub(WIDTH) as i16);
        let y =
            (translated.dst_y + 38).clamp(0, root_geometry.height.saturating_sub(HEIGHT) as i16);
        let window = conn.generate_id()?;
        conn.create_window(
            screen.root_depth,
            window,
            screen.root,
            x,
            y,
            WIDTH,
            HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x202124)
                .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
        )?;
        let opacity_atom = conn
            .intern_atom(false, b"_NET_WM_WINDOW_OPACITY")?
            .reply()?
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            opacity_atom,
            AtomEnum::CARDINAL,
            &[OPACITY_94],
        )?;
        let font = conn.generate_id()?;
        conn.open_font(font, b"fixed")?;
        let gc = conn.generate_id()?;
        conn.create_gc(
            gc,
            window,
            &CreateGCAux::new()
                .foreground(0xE8EAED)
                .background(0x202124)
                .font(font),
        )?;
        conn.map_window(window)?;
        // Override-redirect windows are not managed by the desktop's normal
        // always-on-top policy. Keep the toast at the top of the X11 stack so
        // the visible toast is also the pointer event target over the client.
        conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        let toast = Self {
            window,
            gc,
            font,
            opacity_atom,
            shown_at: Instant::now(),
            character_id,
        };
        toast.draw(conn, character_name, implant_names)?;
        conn.flush()?;
        Ok(toast)
    }

    fn draw(&self, conn: &RustConnection, character_name: &str, implants: &[String]) -> Result<()> {
        let accent_gc = conn.generate_id()?;
        conn.create_gc(
            accent_gc,
            self.window,
            &CreateGCAux::new().foreground(0xF2C94C),
        )?;
        conn.poly_fill_rectangle(
            self.window,
            accent_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: HEIGHT,
            }],
        )?;
        conn.free_gc(accent_gc)?;
        conn.image_text8(self.window, self.gc, 14, 23, b"Implants still plugged in")?;
        let summary = compact_summary(implants);
        let body = format!("{}: {}", character_name, summary);
        let body = truncate_ascii(&body, 66);
        conn.image_text8(self.window, self.gc, 14, 49, body.as_bytes())?;
        conn.image_text8(
            self.window,
            self.gc,
            14,
            74,
            b"Left click: dismiss   Right click: snooze until restart",
        )?;
        Ok(())
    }

    pub fn window(&self) -> Window {
        self.window
    }

    /// Button events selected on the root window identify the top-level window
    /// under the pointer in `child`, while events delivered directly to this
    /// window identify it in `event`. Accept both forms so the toast remains
    /// clickable when the daemon's root event selection receives the event.
    pub fn is_button_target(&self, event: Window, child: Window) -> bool {
        button_targets_window(self.window, event, child)
    }

    pub fn click(&self, button: u8) -> Option<ToastClick> {
        match button {
            1 => Some(ToastClick::Dismiss),
            3 => Some(ToastClick::Snooze(self.character_id)),
            _ => None,
        }
    }

    pub fn tick(&self, conn: &RustConnection, now: Instant) -> Result<bool> {
        let age = now.saturating_duration_since(self.shown_at);
        if age >= HOLD + FADE {
            return Ok(false);
        }
        if age > HOLD {
            let remaining = (HOLD + FADE - age).as_secs_f64() / FADE.as_secs_f64();
            let opacity = (OPACITY_94 as f64 * remaining) as u32;
            conn.change_property32(
                PropMode::REPLACE,
                self.window,
                self.opacity_atom,
                AtomEnum::CARDINAL,
                &[opacity],
            )?;
        }
        Ok(true)
    }

    pub fn destroy(self, conn: &RustConnection) {
        let _ = conn.destroy_window(self.window);
        let _ = conn.free_gc(self.gc);
        let _ = conn.close_font(self.font);
    }
}

fn button_targets_window(window: Window, event: Window, child: Window) -> bool {
    event == window || child == window
}

fn compact_summary(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    if names.len() == 1 {
        return names[0].clone();
    }
    let set_pattern = regex::Regex::new(
        r"(?i)^(high|mid|low)-grade\s+(.+?)\s+(alpha|beta|gamma|delta|epsilon|omega)$",
    )
    .expect("constant regex");
    let mut sets: HashMap<(String, String), (usize, usize)> = HashMap::new();
    for (index, name) in names.iter().enumerate() {
        if let Some(captures) = set_pattern.captures(name) {
            let key = (capitalize(&captures[1]), captures[2].to_owned());
            let entry = sets.entry(key).or_insert((0, index));
            entry.0 += 1;
        }
    }
    if let Some(((grade, family), (count, _))) =
        sets.into_iter().max_by_key(|((grade, _), (count, first))| {
            let grade_rank = match grade.as_str() {
                "High" => 3,
                "Mid" => 2,
                _ => 1,
            };
            (*count, grade_rank, usize::MAX - *first)
        })
    {
        let plural = if family.ends_with('s') {
            family
        } else {
            format!("{family}s")
        };
        let extra = names.len() - count;
        return if extra == 0 {
            format!("{grade}-grade {plural}")
        } else {
            format!("{grade}-grade {plural} +{extra} more")
        };
    }
    let mindlinks = names
        .iter()
        .filter(|name| name.to_ascii_lowercase().contains("mindlink"))
        .count();
    let attributes = names
        .iter()
        .filter(|name| name.to_ascii_lowercase().ends_with("- improved"))
        .count();
    let code = regex::Regex::new(r"\b[A-Z]{2,4}-?\d{2,4}$").expect("constant regex");
    let hardwirings = names.iter().filter(|name| code.is_match(name)).count();
    let other = names
        .len()
        .saturating_sub(mindlinks + attributes + hardwirings);
    let (label, count) = [
        ("Mindlinks", mindlinks),
        ("attribute implants", attributes),
        ("hardwirings", hardwirings),
        ("implants", other),
    ]
    .into_iter()
    .enumerate()
    .max_by_key(|(index, (_, count))| (*count, usize::MAX - *index))
    .map(|(_, value)| value)
    .unwrap();
    let extra = names.len().saturating_sub(count);
    if extra == 0 {
        format!("{count} {label}")
    } else {
        format!("{count} {label} +{extra} more")
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.to_ascii_lowercase().chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        first.make_ascii_uppercase();
    }
    chars.into_iter().collect()
}

fn truncate_ascii(value: &str, max: usize) -> String {
    let mut value: String = value
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .take(max)
        .collect();
    if value.len() == max {
        value.replace_range(max.saturating_sub(3).., "...");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_is_compact_and_replacement_safe() {
        assert_eq!(compact_summary(&["A".into()]), "A");
        assert_eq!(
            compact_summary(&["A".into(), "B".into(), "C".into()]),
            "3 implants"
        );
        assert_eq!(
            compact_summary(&[
                "Mid-grade Amulet Alpha".into(),
                "Mid-grade Amulet Beta".into(),
                "Mid-grade Amulet Omega".into(),
                "Zainou SM-705".into(),
            ]),
            "Mid-grade Amulets +1 more"
        );
        assert!(truncate_ascii(&"x".repeat(100), 20).ends_with("..."));
    }

    #[test]
    fn button_target_accepts_direct_and_root_propagated_events() {
        let toast = 41;
        let root = 1;
        assert!(button_targets_window(toast, toast, 0));
        assert!(button_targets_window(toast, root, toast));
        assert!(!button_targets_window(toast, root, 99));
    }
}
