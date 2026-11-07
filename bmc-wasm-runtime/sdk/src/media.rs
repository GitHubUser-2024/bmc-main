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

//! Streamed music on the device's own speaker.
//!
//! The host runs one player for the whole device. A widget [`load`]s a queue
//! of MP3 stream URLs (each with its own metadata) and steers it with
//! [`play`], [`pause`], [`next`], [`seek`], …; the host streams the tracks
//! and moves through the queue by itself, so music keeps playing while the
//! widget is off screen or asleep. [`status`] reads where it is, including
//! the current track's metadata, which is how a woken widget redraws.
//! [`version`] changes with every status change except the position, for
//! cheap polling.
//!
//! ```ignore
//! let mut queue = media::Queue::new();
//! queue.push(&media::Track {
//!     url: "https://music.example/stream?id=1&format=mp3",
//!     headers: &[],
//!     duration_ms: Some(215_000),
//!     meta_json: &json!({"title": #s(title), "artist": #s(artist)}),
//! });
//! media::load(&queue.shuffle(true).into_json())?;
//! ```
//!
//! For short sounds bundled with the widget use [`crate::host::audio_play`].

use crate::json::JsonDoc;
use crate::json_str::JsonStr;

/// What happens at the end of a track and of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off = 0,
    All = 1,
    /// Replays the current track; skipping still moves on.
    One = 2,
}

impl Repeat {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    /// Off → All → One → Off, the usual repeat button.
    #[must_use]
    pub fn cycled(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    /// Opening the stream; becomes `Playing` with the first sound.
    Loading,
    Playing,
    Paused,
}

/// One queue entry.
#[derive(Debug)]
pub struct Track<'a> {
    /// `http://` or `https://`, serving MP3.
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    /// The stream carries no length; without it the status has none either.
    pub duration_ms: Option<u64>,
    /// A JSON object (or `""` for none), handed back as the status's metadata
    /// while this track is current.
    pub meta_json: &'a str,
}

/// Builds the JSON [`load`] takes.
#[derive(Debug, Default)]
pub struct Queue {
    tracks: String,
    start: u32,
    shuffle: bool,
    repeat: Repeat,
    paused: bool,
}

impl Queue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, track: &Track<'_>) -> &mut Self {
        if !self.tracks.is_empty() {
            self.tracks.push(',');
        }
        let mut headers = String::new();
        for (i, (name, value)) in track.headers.iter().enumerate() {
            let sep = if i == 0 { "" } else { "," };
            headers.push_str(&crate::fmt!(
                "{sep}[\"{}\",\"{}\"]",
                JsonStr(name),
                JsonStr(value)
            ));
        }
        let duration = track
            .duration_ms
            .map_or_else(|| "null".into(), |ms| crate::fmt!("{ms}"));
        let meta = if track.meta_json.is_empty() {
            "null"
        } else {
            track.meta_json
        };
        self.tracks.push_str(&crate::fmt!(
            "{{\"url\":\"{}\",\"headers\":[{headers}],\"duration_ms\":{duration},\"meta\":{meta}}}",
            JsonStr(track.url)
        ));
        self
    }

    /// Queue index to play first (also first under shuffle).
    pub fn start(&mut self, index: u32) -> &mut Self {
        self.start = index;
        self
    }

    pub fn shuffle(&mut self, on: bool) -> &mut Self {
        self.shuffle = on;
        self
    }

    pub fn repeat(&mut self, repeat: Repeat) -> &mut Self {
        self.repeat = repeat;
        self
    }

    /// Load without starting; [`play`] starts it.
    pub fn paused(&mut self, paused: bool) -> &mut Self {
        self.paused = paused;
        self
    }

    #[must_use]
    pub fn into_json(&self) -> String {
        let (start, shuffle, repeat, autoplay) =
            (self.start, self.shuffle, self.repeat.name(), !self.paused);
        crate::fmt!(
            "{{\"tracks\":[{}],\"start\":{start},\"shuffle\":{shuffle},\"repeat\":\"{repeat}\",\"autoplay\":{autoplay}}}",
            self.tracks.as_str()
        )
    }
}

/// Why [`load`] refused a queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    Malformed,
    /// No tracks, more than the host takes (2000), a non-http(s) URL.
    Invalid,
    TooLarge,
}

/// Where the player is, parsed from the host's status JSON.
#[derive(Debug)]
pub struct Status(JsonDoc);

impl Status {
    #[must_use]
    pub fn from_json(json: &[u8]) -> Self {
        Self(JsonDoc::parse(json))
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.0
            .i64("/version")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn state(&self) -> PlayState {
        match self.0.str("/state").as_deref() {
            Some("loading") => PlayState::Loading,
            Some("playing") => PlayState::Playing,
            Some("paused") => PlayState::Paused,
            _ => PlayState::Stopped,
        }
    }

    /// The current track's queue index.
    #[must_use]
    pub fn index(&self) -> Option<u32> {
        self.0.i64("/index").and_then(|v| u32::try_from(v).ok())
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.0
            .i64("/count")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn position_ms(&self) -> u64 {
        self.0
            .i64("/position_ms")
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.0
            .i64("/duration_ms")
            .and_then(|v| u64::try_from(v).ok())
    }

    #[must_use]
    pub fn volume(&self) -> u8 {
        self.0
            .i64("/volume")
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(100)
    }

    #[must_use]
    pub fn shuffle(&self) -> bool {
        self.0.bool("/shuffle").unwrap_or(false)
    }

    #[must_use]
    pub fn repeat(&self) -> Repeat {
        match self.0.str("/repeat").as_deref() {
            Some("all") => Repeat::All,
            Some("one") => Repeat::One,
            _ => Repeat::Off,
        }
    }

    /// The last track failure (cleared by a new queue or a fresh start).
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.0.str("/error")
    }

    /// A string from the current track's metadata, by JSON pointer below
    /// `meta` (`"/title"`).
    #[must_use]
    pub fn meta_str(&self, pointer: &str) -> Option<String> {
        self.0.str(&crate::fmt!("/meta{pointer}"))
    }
}

/// Replace the queue (built with [`Queue`]); it starts playing unless built
/// [`Queue::paused`].
#[cfg(target_arch = "wasm32")]
pub fn load(queue_json: &str) -> Result<(), LoadError> {
    match Platform::load(queue_json.as_bytes()) {
        0 => Ok(()),
        2 => Err(LoadError::Invalid),
        3 => Err(LoadError::TooLarge),
        _ => Err(LoadError::Malformed),
    }
}

/// Start, or resume a pause. A command that doesn't apply (a seek while
/// stopped, a jump outside the queue) is ignored; [`status`] shows the result.
#[cfg(target_arch = "wasm32")]
pub fn play() {
    Platform::command(0, 0);
}

#[cfg(target_arch = "wasm32")]
pub fn pause() {
    Platform::command(1, 0);
}

#[cfg(target_arch = "wasm32")]
pub fn toggle() {
    Platform::command(2, 0);
}

#[cfg(target_arch = "wasm32")]
pub fn stop() {
    Platform::command(3, 0);
}

#[cfg(target_arch = "wasm32")]
pub fn next() {
    Platform::command(4, 0);
}

/// Back a track, or to the start of this one when it has played over 3 s.
#[cfg(target_arch = "wasm32")]
pub fn previous() {
    Platform::command(5, 0);
}

#[cfg(target_arch = "wasm32")]
pub fn seek(position_ms: u32) {
    Platform::command(6, position_ms);
}

/// 0–100, applied to the stream only; the device volume stays as it is.
#[cfg(target_arch = "wasm32")]
pub fn set_volume(volume: u8) {
    Platform::command(7, u32::from(volume));
}

#[cfg(target_arch = "wasm32")]
pub fn set_shuffle(on: bool) {
    Platform::command(8, u32::from(on));
}

#[cfg(target_arch = "wasm32")]
pub fn set_repeat(repeat: Repeat) {
    Platform::command(9, repeat as u32);
}

/// Play the queued track `index`.
#[cfg(target_arch = "wasm32")]
pub fn jump(index: u32) {
    Platform::command(10, index);
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn version() -> u64 {
    Platform::version()
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn status() -> Status {
    Status::from_json(&Platform::status())
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        fn host_media_load(json_ptr: *const u8, json_len: u32) -> u32;
        fn host_media_command(command: u32, arg: u32) -> u32;
        fn host_media_version() -> u64;
        fn host_media_status(out_ptr: *mut u8, out_cap: u32) -> u32;
    }

    pub(super) struct Host;

    impl Host {
        pub(super) fn load(json: &[u8]) -> u32 {
            let Ok(len) = u32::try_from(json.len()) else {
                return 3;
            };
            // SAFETY: `json` is a live borrow of `len` bytes.
            unsafe { host_media_load(json.as_ptr(), len) }
        }

        pub(super) fn command(command: u32, arg: u32) {
            // 0 = didn't apply; the status tells the guest what happened.
            unsafe { host_media_command(command, arg) };
        }

        pub(super) fn version() -> u64 {
            unsafe { host_media_version() }
        }

        pub(super) fn status() -> Vec<u8> {
            // The position moves between the probe and the read, so the
            // length can grow by a few digits: retry until it fits.
            let mut cap = 512_u32;
            loop {
                let mut out = vec![0_u8; usize::try_from(cap).expect("BUG: u32 fits usize")];
                // SAFETY: `out` is uniquely borrowed with length `cap`; the
                // host writes at most `cap` bytes.
                let needed = unsafe { host_media_status(out.as_mut_ptr(), cap) };
                if needed <= cap {
                    out.truncate(usize::try_from(needed).expect("BUG: u32 fits usize"));
                    return out;
                }
                cap = needed + 64;
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
use wasm::Host as Platform;

#[cfg(test)]
mod tests {
    use super::{PlayState, Queue, Repeat, Status, Track};
    use crate::json::JsonDoc;
    use crate::wasm_test;

    #[wasm_test]
    fn a_queue_serialises_tracks_with_escaped_strings_and_raw_metadata() {
        let mut queue = Queue::new();
        queue
            .push(&Track {
                url: "http://m/a?\"x\"",
                headers: &[("Authorization", "Bearer t")],
                duration_ms: Some(1_500),
                meta_json: r#"{"title":"A"}"#,
            })
            .push(&Track {
                url: "https://m/b",
                headers: &[],
                duration_ms: None,
                meta_json: "",
            });
        let json = queue.start(1).shuffle(true).repeat(Repeat::All).into_json();
        let doc = JsonDoc::parse(json.as_bytes());
        assert!(doc.is_valid(), "{json}");
        assert_eq!(
            doc.str("/tracks/0/url").as_deref(),
            Some("http://m/a?\"x\"")
        );
        assert_eq!(
            doc.str("/tracks/0/headers/0/1").as_deref(),
            Some("Bearer t")
        );
        assert_eq!(doc.i64("/tracks/0/duration_ms"), Some(1_500));
        assert_eq!(doc.str("/tracks/0/meta/title").as_deref(), Some("A"));
        assert_eq!(doc.len("/tracks"), Some(2));
        assert_eq!(doc.i64("/start"), Some(1));
        assert_eq!(doc.bool("/shuffle"), Some(true));
        assert_eq!(doc.str("/repeat").as_deref(), Some("all"));
        assert_eq!(doc.bool("/autoplay"), Some(true));
    }

    #[wasm_test]
    fn a_status_reads_every_field_and_the_metadata() {
        let status = Status::from_json(
            br#"{"version":7,"state":"paused","index":2,"count":9,"position_ms":61000,
                "duration_ms":null,"volume":40,"shuffle":true,"repeat":"one","error":null,
                "meta":{"title":"Song","album":{"name":"LP"}}}"#,
        );
        assert_eq!(status.version(), 7);
        assert_eq!(status.state(), PlayState::Paused);
        assert_eq!(status.index(), Some(2));
        assert_eq!(status.count(), 9);
        assert_eq!(status.position_ms(), 61_000);
        assert_eq!(status.duration_ms(), None);
        assert_eq!(status.volume(), 40);
        assert!(status.shuffle());
        assert_eq!(status.repeat(), Repeat::One);
        assert_eq!(status.error(), None);
        assert_eq!(status.meta_str("/title").as_deref(), Some("Song"));
        assert_eq!(status.meta_str("/album/name").as_deref(), Some("LP"));
    }

    #[wasm_test]
    fn an_unreadable_status_reads_as_stopped() {
        let status = Status::from_json(b"not json");
        assert_eq!(status.state(), PlayState::Stopped);
        assert_eq!(status.index(), None);
    }

    #[wasm_test]
    fn the_repeat_button_cycles_through_all_three() {
        assert_eq!(Repeat::Off.cycled(), Repeat::All);
        assert_eq!(Repeat::All.cycled(), Repeat::One);
        assert_eq!(Repeat::One.cycled(), Repeat::Off);
    }
}
