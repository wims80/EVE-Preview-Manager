//! Non-blocking playback of the decloak cue.
//!
//! The daemon must never wait on an audio device: some desktop sessions do not
//! have a usable PulseAudio/PipeWire endpoint.  This module therefore owns a
//! small worker thread and treats every device/decoder error as advisory.

use std::{
    io::Cursor,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink};

const DECLOAK_COOLDOWN: Duration = Duration::from_secs(2);
const DECLOAK_AUDIO: &[u8] = include_bytes!("../../assets/decloaked.mp3");

/// Sends decloak playback requests to the daemon's dedicated audio thread.
///
/// Requests are deliberately lossy.  At most one request may be waiting while
/// the worker is opening or recovering an output device, so bursts of game-log
/// events can never delay the X11/Tokio loop.
#[derive(Clone)]
pub struct AudioWorker {
    trigger_sender: Option<SyncSender<()>>,
}

impl AudioWorker {
    /// Starts the audio worker.  No audio device is opened until the first
    /// request, which keeps headless daemon startup quiet and cheap.
    pub fn new() -> Self {
        let (trigger_sender, receiver) = mpsc::sync_channel(1);
        let trigger_sender = match thread::Builder::new()
            .name("eve-preview-audio".to_owned())
            .spawn(move || run_audio_worker(receiver))
        {
            Ok(_) => Some(trigger_sender),
            Err(error) => {
                tracing::warn!(%error, "unable to start decloak audio worker");
                None
            }
        };

        Self { trigger_sender }
    }

    /// Queue one decloak cue without blocking.
    ///
    /// The caller is responsible for honoring the profile's audio toggle.
    pub fn trigger_decloak(&self) {
        let Some(trigger_sender) = &self.trigger_sender else {
            return;
        };
        match trigger_sender.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            // The worker only disconnects during daemon shutdown; a lost cue
            // is preferable to making alert handling fallible.
            Err(TrySendError::Disconnected(())) => {}
        }
    }
}

impl Default for AudioWorker {
    fn default() -> Self {
        Self::new()
    }
}

fn run_audio_worker(receiver: mpsc::Receiver<()>) {
    let mut output_stream = None;
    let mut cooldown = Cooldown::new(DECLOAK_COOLDOWN);

    while receiver.recv().is_ok() {
        let now = Instant::now();
        if !cooldown.is_ready(now) {
            continue;
        }

        // The cue is shorter than the cooldown, so the previous playback has
        // completed. Reopening here makes every permitted edge recover from a
        // PipeWire/PulseAudio server restart or output-device replacement.
        drop(output_stream.take());
        match play_decloak(&mut output_stream) {
            Ok(()) => cooldown.record(now),
            Err(error) => {
                // Drop the stream too: the next request obtains a fresh device
                // after a server restart, unplugged headset, or other failure.
                tracing::warn!(%error, "unable to play decloak audio cue");
            }
        }
    }
}

fn play_decloak(output_stream: &mut Option<OutputStream>) -> Result<(), String> {
    if output_stream.is_none() {
        *output_stream = Some(
            OutputStreamBuilder::open_default_stream()
                .map_err(|error| format!("could not open default audio output: {error}"))?,
        );
    }

    let decoder = Decoder::try_from(Cursor::new(DECLOAK_AUDIO))
        .map_err(|error| format!("could not decode embedded decloak cue: {error}"))?;
    let sink = Sink::connect_new(
        output_stream
            .as_ref()
            .expect("audio output stream was just initialized")
            .mixer(),
    );
    sink.append(decoder);
    // The mixer owns playback after detaching; dropping the sink here would
    // otherwise stop the sound immediately.
    sink.detach();
    Ok(())
}

/// Deterministic, global edge-trigger cooldown used by the worker.
#[derive(Debug)]
struct Cooldown {
    duration: Duration,
    last_played: Option<Instant>,
}

impl Cooldown {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_played: None,
        }
    }

    fn is_ready(&self, now: Instant) -> bool {
        match self.last_played {
            None => true,
            Some(last_played) => now.saturating_duration_since(last_played) >= self.duration,
        }
    }

    fn record(&mut self, now: Instant) {
        self.last_played = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_decloak_asset_decodes_as_mp3() {
        Decoder::try_from(Cursor::new(DECLOAK_AUDIO))
            .expect("the embedded decloak audio must remain a decodable MP3");
    }

    #[test]
    fn cooldown_allows_only_one_trigger_per_two_seconds() {
        let start = Instant::now();
        let mut cooldown = Cooldown::new(DECLOAK_COOLDOWN);

        assert!(cooldown.is_ready(start));
        cooldown.record(start);
        assert!(!cooldown.is_ready(start + Duration::from_millis(1_999)));
        assert!(cooldown.is_ready(start + DECLOAK_COOLDOWN));
    }
}
