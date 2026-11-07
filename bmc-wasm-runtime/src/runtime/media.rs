// Copyright (C) 2026  Jan Novák
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Streamed music playback on the device's own speaker.
//!
//! One player for the whole host process, as there is one speaker: a guest
//! hands it a queue of track URLs (with its own metadata per track) and then
//! sends commands; the player streams, decodes and moves on through the
//! queue by itself, so music keeps playing while the widget that started it
//! is off screen or asleep. Any guest can read the status and send commands,
//! which is how a woken widget picks the state up again.
//!
//! Tracks must be MP3 (servers like Subsonic transcode on request); see
//! [`pipeline`] for the decoding. Sample audio (`host_audio_*`) is separate.

mod pipeline;
mod queue;

use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::background::{FetchAgent, open_stream};
pub(crate) use queue::Repeat;
use queue::{Queue, Track};

/// Connect and response-head limit for each track's stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(15);
/// A pause this long hands the sound device back (the device can't mix, so
/// a held pause would silence every other sound); play resumes from the
/// paused position with a new stream.
const RELEASE_AFTER: Duration = Duration::from_mins(1);
/// Failed tracks in a row before the player gives up instead of skipping on.
const MAX_FAILURES: u32 = 3;
/// "Previous" this far into a track restarts it instead.
const RESTART_THRESHOLD_MS: u64 = 3_000;
pub(crate) const MAX_TRACKS: usize = 2_000;
pub(crate) const MAX_LOAD_BYTES: usize = 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Playback {
    Stopped,
    /// Opening the stream / starting the decoder.
    Loading,
    Playing,
    Paused,
}

impl Playback {
    fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Loading => "loading",
            Self::Playing => "playing",
            Self::Paused => "paused",
        }
    }
}

/// A guest command; wire values in [`Command::from_wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    Seek(u64),
    Volume(u8),
    Shuffle(bool),
    SetRepeat(Repeat),
    Jump(usize),
}

impl Command {
    pub(crate) fn from_wire(command: u32, arg: u32) -> Option<Self> {
        Some(match command {
            0 => Self::Play,
            1 => Self::Pause,
            2 => Self::Toggle,
            3 => Self::Stop,
            4 => Self::Next,
            5 => Self::Previous,
            6 => Self::Seek(u64::from(arg)),
            7 => Self::Volume(u8::try_from(arg.min(100)).expect("BUG: bounded to 100")),
            8 => Self::Shuffle(arg != 0),
            9 => Self::SetRepeat(Repeat::from_wire(arg)?),
            10 => Self::Jump(usize::try_from(arg).ok()?),
            _ => return None,
        })
    }
}

/// Why a queue was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadError {
    /// Not JSON of the expected shape.
    Malformed = 1,
    /// No tracks, too many, a URL that isn't http(s) or an unknown repeat.
    Invalid = 2,
}

#[derive(Deserialize)]
struct LoadRequest {
    tracks: Vec<TrackIn>,
    #[serde(default)]
    start: usize,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    repeat: Option<String>,
    #[serde(default = "yes")]
    autoplay: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct TrackIn {
    url: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    meta: serde_json::Value,
}

struct State {
    queue: Queue,
    playback: Playback,
    volume: u8,
    /// Into the current track, by the samples handed to the output.
    position_ms: u64,
    /// Bumped on every change a widget redraws for; the position alone isn't one.
    version: u64,
    /// Bumped to make the worker drop what it plays and start over at `start_ms`.
    session: u64,
    start_ms: u64,
    /// The worker let go of the device during a long pause.
    released: bool,
    error: Option<String>,
    failures: u32,
}

impl State {
    fn changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// (Re)start the current track at `at_ms`.
    fn restart(&mut self, at_ms: u64) {
        self.session = self.session.wrapping_add(1);
        self.start_ms = at_ms;
        self.position_ms = at_ms;
        self.released = false;
        self.playback = if self.queue.is_empty() {
            Playback::Stopped
        } else {
            Playback::Loading
        };
        self.changed();
    }

    fn stop(&mut self) {
        self.session = self.session.wrapping_add(1);
        self.playback = Playback::Stopped;
        self.position_ms = 0;
        self.released = false;
        self.changed();
    }

    fn resume(&mut self) {
        match self.playback {
            Playback::Paused if self.released => self.restart(self.position_ms),
            Playback::Paused => {
                self.playback = Playback::Playing;
                self.changed();
            }
            Playback::Stopped => {
                self.error = None;
                self.failures = 0;
                self.restart(0);
            }
            Playback::Loading | Playback::Playing => {}
        }
    }

    fn pause(&mut self) {
        if matches!(self.playback, Playback::Playing | Playback::Loading) {
            self.playback = Playback::Paused;
            self.changed();
        }
    }

    fn apply(&mut self, command: Command) -> bool {
        match command {
            Command::Play => self.resume(),
            Command::Pause => self.pause(),
            Command::Toggle => {
                if matches!(self.playback, Playback::Playing | Playback::Loading) {
                    self.pause();
                } else {
                    self.resume();
                }
            }
            Command::Stop => self.stop(),
            Command::Next => {
                if self.queue.next() {
                    self.restart(0);
                } else {
                    self.stop();
                }
            }
            Command::Previous => {
                if self.position_ms <= RESTART_THRESHOLD_MS {
                    self.queue.previous();
                }
                self.restart(0);
            }
            Command::Seek(ms) => {
                let end = self
                    .queue
                    .current()
                    .and_then(|t| t.duration_ms)
                    .unwrap_or(u64::MAX);
                if self.playback == Playback::Stopped || self.queue.is_empty() {
                    return false;
                }
                let paused = self.playback == Playback::Paused;
                self.restart(ms.min(end));
                if paused {
                    // Seeking while paused stays paused; the worker parks right
                    // after opening the new stream.
                    self.playback = Playback::Paused;
                }
            }
            Command::Volume(volume) => {
                self.volume = volume;
                self.changed();
            }
            Command::Shuffle(on) => {
                self.queue.set_shuffle(on);
                self.changed();
            }
            Command::SetRepeat(repeat) => {
                self.queue.set_repeat(repeat);
                self.changed();
            }
            Command::Jump(index) => {
                if !self.queue.jump(index) {
                    return false;
                }
                self.restart(0);
            }
        }
        true
    }

    fn status_json(&self) -> String {
        let current = self.queue.current();
        serde_json::json!({
            "version": self.version,
            "state": self.playback.name(),
            "index": self.queue.current_index(),
            "count": self.queue.len(),
            "position_ms": self.position_ms,
            "duration_ms": current.and_then(|t| t.duration_ms),
            "volume": self.volume,
            "shuffle": self.queue.shuffle(),
            "repeat": self.queue.repeat().name(),
            "error": self.error,
            "meta": current.map_or(serde_json::Value::Null, |t| t.meta.clone()),
        })
        .to_string()
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves plain data behind; keep going.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The process-wide player.
pub(crate) struct Player {
    shared: Arc<Shared>,
}

static PLAYER: OnceLock<Player> = OnceLock::new();

/// The player, its worker started on first use.
pub(crate) fn player() -> &'static Player {
    PLAYER.get_or_init(|| {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: Queue::new(Vec::new(), 0, false, Repeat::Off, 1),
                playback: Playback::Stopped,
                volume: 100,
                position_ms: 0,
                version: 0,
                session: 0,
                start_ms: 0,
                released: false,
                error: None,
                failures: 0,
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("media-player".into())
            .spawn(move || run(&worker))
            .expect("BUG: spawning the media player thread");
        Player { shared }
    })
}

impl Player {
    /// Replace the queue from a guest's JSON (see the SDK's `media::load`).
    pub(crate) fn load(&self, json: &[u8]) -> Result<(), LoadError> {
        let request: LoadRequest =
            serde_json::from_slice(json).map_err(|_| LoadError::Malformed)?;
        let queue = parse_queue(request)?;
        let mut state = self.shared.lock();
        let autoplay = queue.1;
        state.queue = queue.0;
        state.error = None;
        state.failures = 0;
        if autoplay {
            state.restart(0);
        } else {
            state.stop();
        }
        drop(state);
        self.shared.wake.notify_all();
        Ok(())
    }

    /// `false` for a command that doesn't apply (a seek while stopped, a
    /// jump outside the queue).
    pub(crate) fn command(&self, command: Command) -> bool {
        let applied = self.shared.lock().apply(command);
        self.shared.wake.notify_all();
        applied
    }

    pub(crate) fn version(&self) -> u64 {
        self.shared.lock().version
    }

    pub(crate) fn status_json(&self) -> String {
        self.shared.lock().status_json()
    }
}

fn parse_queue(request: LoadRequest) -> Result<(Queue, bool), LoadError> {
    if request.tracks.is_empty() || request.tracks.len() > MAX_TRACKS {
        return Err(LoadError::Invalid);
    }
    let repeat = match request.repeat.as_deref() {
        None => Repeat::Off,
        Some(name) => Repeat::from_name(name).ok_or(LoadError::Invalid)?,
    };
    let tracks = request
        .tracks
        .into_iter()
        .map(|t| {
            if !(t.url.starts_with("http://") || t.url.starts_with("https://")) {
                return Err(LoadError::Invalid);
            }
            Ok(Track {
                url: t.url,
                headers: t.headers,
                duration_ms: t.duration_ms,
                meta: t.meta,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| u64::from(d.subsec_nanos()) ^ d.as_secs());
    Ok((
        Queue::new(tracks, request.start, request.shuffle, repeat, seed),
        request.autoplay,
    ))
}

/// How one track's playback ended.
enum Ended {
    /// Played to the end of the stream.
    Finished,
    /// A command moved on (new track, seek, stop) while it played.
    Superseded,
    /// Paused past [`RELEASE_AFTER`]; the device is free again.
    Released,
    Failed(String),
}

fn run(shared: &Shared) {
    let agent = FetchAgent::default();
    let mut handled = 0_u64;
    loop {
        let (session, track, start_ms) = {
            let mut state = shared.lock();
            loop {
                let wants_audio = matches!(state.playback, Playback::Loading | Playback::Paused);
                if wants_audio && state.session != handled && !state.released {
                    break;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            handled = state.session;
            let Some(track) = state.queue.current().cloned() else {
                state.stop();
                continue;
            };
            (state.session, track, state.start_ms)
        };
        let ended = play_track(shared, &agent, session, &track, start_ms);
        let mut state = shared.lock();
        if state.session != session {
            continue;
        }
        match ended {
            Ended::Finished => {
                state.failures = 0;
                if state.queue.advance_after_end() {
                    state.restart(0);
                } else {
                    state.stop();
                }
            }
            Ended::Superseded => {}
            Ended::Released => {
                state.released = true;
                state.changed();
            }
            Ended::Failed(reason) => {
                tracing::warn!(url = %track.url, %reason, "media: track failed");
                state.error = Some(reason);
                state.failures += 1;
                if state.failures < MAX_FAILURES && state.queue.next() {
                    state.restart(0);
                } else {
                    state.stop();
                }
            }
        }
    }
}

#[expect(
    clippy::integer_division,
    reason = "the position in whole milliseconds"
)]
fn play_track(
    shared: &Shared,
    agent: &FetchAgent,
    session: u64,
    track: &Track,
    start_ms: u64,
) -> Ended {
    let superseded = || shared.lock().session != session;
    let mut stream = match open_stream(agent, &track.url, &track.headers, STREAM_TIMEOUT) {
        Ok(stream) => stream,
        Err(reason) => return Ended::Failed(reason),
    };
    if superseded() {
        return Ended::Superseded;
    }
    let mut children = pipeline::Children::default();
    let (decoder, mut decoder_in, mut decoder_out) = match pipeline::spawn_decoder(start_ms) {
        Ok(parts) => parts,
        Err(e) => return Ended::Failed(format!("decoder: {e}")),
    };
    children.push(decoder);
    // The feeder ends on its own when the decoder goes away (broken pipe)
    // or the stream does.
    thread::spawn(move || {
        let _ = std::io::copy(&mut stream, &mut decoder_in);
    });
    let format = match pipeline::read_wav_header(&mut decoder_out) {
        Ok(format) => format,
        Err(_) if superseded() => return Ended::Superseded,
        Err(e) => return Ended::Failed(format!("not playable as MP3 ({e})")),
    };
    let (output, mut output_in) = match pipeline::spawn_output(format) {
        Ok(parts) => parts,
        Err(e) => return Ended::Failed(format!("output: {e}")),
    };
    children.push(output);
    {
        let mut state = shared.lock();
        if state.session != session {
            return Ended::Superseded;
        }
        if state.playback == Playback::Loading {
            state.playback = Playback::Playing;
        }
        state.changed();
    }

    let mut buf = vec![0_u8; READ_CHUNK];
    let mut carried = 0_usize;
    let mut written = 0_u64;
    loop {
        let gain = {
            let mut state = shared.lock();
            let mut paused_at = None;
            loop {
                if state.session != session {
                    return Ended::Superseded;
                }
                if state.playback != Playback::Paused {
                    break;
                }
                let since = *paused_at.get_or_insert_with(Instant::now);
                let Some(left) = RELEASE_AFTER.checked_sub(since.elapsed()) else {
                    return Ended::Released;
                };
                state = shared
                    .wake
                    .wait_timeout(state, left)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
            pipeline::gain_q15(state.volume)
        };
        let n = match decoder_out.read(&mut buf[carried..]) {
            Ok(0) => {
                children.drain_output(output_in);
                return Ended::Finished;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Ended::Failed(format!("decoder: {e}")),
        };
        // Whole samples only; an odd byte waits for the next read.
        let have = carried + n;
        let even = have & !1;
        pipeline::apply_gain(&mut buf[..even], gain);
        if let Err(e) = std::io::Write::write_all(&mut output_in, &buf[..even]) {
            return Ended::Failed(format!("output: {e}"));
        }
        carried = have - even;
        if carried == 1 {
            buf[0] = buf[even];
        }
        written += u64::try_from(even).expect("BUG: chunk fits u64");
        let mut state = shared.lock();
        if state.session == session {
            state.position_ms = start_ms + written * 1000 / format.bytes_per_second();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, LoadError, LoadRequest, Playback, Repeat, State, parse_queue};
    use crate::runtime::media::queue::Queue;

    fn state_with(json: &str) -> State {
        let request: LoadRequest = serde_json::from_str(json).expect("BUG: test queue JSON");
        let (queue, _) = parse_queue(request).expect("BUG: a valid test queue");
        State {
            queue,
            playback: Playback::Stopped,
            volume: 100,
            position_ms: 0,
            version: 0,
            session: 0,
            start_ms: 0,
            released: false,
            error: None,
            failures: 0,
        }
    }

    const THREE: &str = r#"{"tracks":[
        {"url":"http://m/1.mp3","duration_ms":200000,"meta":{"title":"One"}},
        {"url":"http://m/2.mp3","meta":{"title":"Two"}},
        {"url":"https://m/3.mp3"}],"repeat":"all"}"#;

    fn parse(json: &str) -> Result<(Queue, bool), LoadError> {
        parse_queue(serde_json::from_str(json).map_err(|_| LoadError::Malformed)?)
    }

    #[test]
    fn a_queue_needs_tracks_with_http_urls_and_a_known_repeat() {
        parse(THREE).expect("BUG: the test queue loads");
        assert_eq!(
            parse(r#"{"tracks":[]}"#).expect_err("BUG: refused"),
            LoadError::Invalid
        );
        assert_eq!(
            parse(r#"{"tracks":[{"url":"file:///etc/passwd"}]}"#).expect_err("BUG: refused"),
            LoadError::Invalid
        );
        assert_eq!(
            parse(r#"{"tracks":[{"url":"http://a"}],"repeat":"twice"}"#).expect_err("BUG: refused"),
            LoadError::Invalid
        );
        assert_eq!(
            parse(r#"{"tracks":"x"}"#).expect_err("BUG: refused"),
            LoadError::Malformed
        );
    }

    #[test]
    fn play_starts_loading_and_toggle_pauses_and_resumes() {
        let mut state = state_with(THREE);
        assert!(state.apply(Command::Play));
        assert_eq!(state.playback, Playback::Loading);
        let session = state.session;
        state.playback = Playback::Playing;
        state.apply(Command::Toggle);
        assert_eq!(state.playback, Playback::Paused);
        state.apply(Command::Toggle);
        assert_eq!(state.playback, Playback::Playing);
        assert_eq!(
            state.session, session,
            "a held pause resumes the same stream"
        );
    }

    #[test]
    fn resuming_a_released_pause_restarts_at_the_paused_position() {
        let mut state = state_with(THREE);
        state.apply(Command::Play);
        state.playback = Playback::Paused;
        state.position_ms = 42_000;
        state.released = true;
        let session = state.session;
        state.apply(Command::Play);
        assert_eq!(state.playback, Playback::Loading);
        assert_eq!(state.start_ms, 42_000);
        assert_ne!(state.session, session);
    }

    #[test]
    fn previous_restarts_a_track_that_has_been_playing_a_while() {
        let mut state = state_with(THREE);
        state.apply(Command::Next);
        state.position_ms = 10_000;
        state.apply(Command::Previous);
        assert_eq!(state.queue.current_index(), Some(1));
        state.position_ms = 1_000;
        state.apply(Command::Previous);
        assert_eq!(state.queue.current_index(), Some(0));
    }

    #[test]
    fn seeking_is_clamped_to_the_track_and_keeps_a_pause() {
        let mut state = state_with(THREE);
        assert!(
            !state.apply(Command::Seek(5_000)),
            "nothing to seek while stopped"
        );
        state.apply(Command::Play);
        state.playback = Playback::Paused;
        assert!(state.apply(Command::Seek(900_000)));
        assert_eq!(state.start_ms, 200_000);
        assert_eq!(state.playback, Playback::Paused);
    }

    #[test]
    fn next_past_the_end_stops_without_repeat() {
        let mut state = state_with(r#"{"tracks":[{"url":"http://m/1.mp3"}]}"#);
        state.apply(Command::Play);
        state.apply(Command::Next);
        assert_eq!(state.playback, Playback::Stopped);
    }

    #[test]
    fn wire_commands_decode_and_unknown_ones_are_refused() {
        assert_eq!(Command::from_wire(7, 250), Some(Command::Volume(100)));
        assert_eq!(
            Command::from_wire(9, 1),
            Some(Command::SetRepeat(Repeat::All))
        );
        assert_eq!(Command::from_wire(9, 7), None);
        assert_eq!(Command::from_wire(99, 0), None);
    }

    #[test]
    fn the_status_carries_the_current_tracks_metadata() {
        let mut state = state_with(THREE);
        state.apply(Command::Play);
        let status: serde_json::Value =
            serde_json::from_str(&state.status_json()).expect("BUG: status is JSON");
        assert_eq!(status["state"], "loading");
        assert_eq!(status["index"], 0);
        assert_eq!(status["count"], 3);
        assert_eq!(status["duration_ms"], 200_000);
        assert_eq!(status["repeat"], "all");
        assert_eq!(status["meta"]["title"], "One");
        let version = status["version"].as_u64().expect("BUG: a version");
        state.apply(Command::Volume(40));
        let status: serde_json::Value =
            serde_json::from_str(&state.status_json()).expect("BUG: status is JSON");
        assert_eq!(status["volume"], 40);
        assert!(status["version"].as_u64().expect("BUG: a version") > version);
    }
}
