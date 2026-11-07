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

//! Subsonic API client (Navidrome, Gonic, Airsonic, LMS, Ampache, …): picks
//! songs and turns them into a queue for the device's own player.
//!
//! Auth is the API's salted token (`t` = md5(password + salt), `s` = salt)
//! in every query, so stream and cover URLs work as plain GETs. Streams ask
//! the server for MP3 (`format=mp3`), the one format the player decodes.

use bmc_wasm_sdk::{JsonDoc, fmt, json, media, ufmt};
use md5::{Digest, Md5};

/// API version sent as `v`; 1.16.1 is what current servers implement.
const API_VERSION: &str = "1.16.1";
const CLIENT: &str = "braiins-deck";
/// Songs per random/favourites queue.
pub const QUEUE_SIZE: u32 = 200;

/// Which songs to queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Random,
    Starred,
    Playlist(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// Without a trailing slash.
    base: String,
    user: String,
    password: String,
    max_bitrate: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    pub cover_art: Option<String>,
}

impl Server {
    #[must_use]
    pub fn new(base: &str, user: &str, password: &str, max_bitrate: u32) -> Self {
        Self {
            base: base.trim().trim_end_matches('/').into(),
            user: user.into(),
            password: password.into(),
            max_bitrate,
        }
    }

    /// `rest/<method>` with `extra` and fresh auth.
    #[must_use]
    pub fn api_url(&self, method: &str, extra: &[(&str, &str)]) -> String {
        let salt = salt();
        self.api_url_salted(method, extra, &salt)
    }

    fn api_url_salted(&self, method: &str, extra: &[(&str, &str)], salt: &str) -> String {
        self.api_url_with(&self.auth_query(salt), method, extra)
    }

    /// The query's auth part (user, token, salt, version, client, format).
    /// A queue signs all its URLs with one: an MD5 per URL costs a callback
    /// its fuel when the queue is long.
    fn auth_query(&self, salt: &str) -> String {
        form_urlencoded::Serializer::new(String::new())
            .append_pair("u", &self.user)
            .append_pair("t", &token(&self.password, salt))
            .append_pair("s", salt)
            .append_pair("v", API_VERSION)
            .append_pair("c", CLIENT)
            .append_pair("f", "json")
            .finish()
    }

    fn api_url_with(&self, auth: &str, method: &str, extra: &[(&str, &str)]) -> String {
        let mut query = form_urlencoded::Serializer::for_suffix(String::from(auth), 0);
        for (k, v) in extra {
            query.append_pair(k, v);
        }
        fmt!(
            "{}/rest/{}?{}",
            self.base.as_str(),
            method,
            query.finish().as_str()
        )
    }

    #[must_use]
    pub fn songs_url(&self, source: &Source) -> String {
        let size = fmt!("{}", QUEUE_SIZE);
        match source {
            Source::Random => self.api_url("getRandomSongs", &[("size", &size)]),
            Source::Starred => self.api_url("getStarred2", &[]),
            Source::Playlist(_) => self.api_url("getPlaylists", &[]),
        }
    }

    #[must_use]
    pub fn playlist_url(&self, id: &str) -> String {
        self.api_url("getPlaylist", &[("id", id)])
    }

    fn stream_url_with(&self, auth: &str, id: &str) -> String {
        let bitrate = fmt!("{}", self.max_bitrate);
        self.api_url_with(
            auth,
            "stream",
            &[("id", id), ("format", "mp3"), ("maxBitRate", &bitrate)],
        )
    }

    #[must_use]
    pub fn cover_url(&self, cover_art: &str) -> String {
        self.api_url("getCoverArt", &[("id", cover_art), ("size", "300")])
    }
}

/// A song list on its way to becoming the player's queue, a slice at a
/// time: a long list costs more fuel than one callback has. Each track's
/// metadata carries the song's title, artist, album and cover-art id for
/// display; the cover's URL is made when the track shows, which halves the
/// signed URLs a queue costs.
/// Songs a [`QueueBuild::step`] takes on: about 3 M fuel on the device, a
/// third of the 10 M frame that 26.10 firmware gives.
pub const SONGS_PER_STEP: usize = 16;

#[derive(Debug)]
pub struct QueueBuild {
    doc: JsonDoc,
    base: String,
    count: usize,
    next: usize,
    /// One signature for every URL in the queue.
    auth: String,
    queue: media::Queue,
    queued: usize,
}

impl QueueBuild {
    /// The songs under `list` in `doc` (`randomSongs/song`,
    /// `starred2/song`, `playlist/entry`).
    #[must_use]
    pub fn new(server: &Server, doc: JsonDoc, list: &str) -> Self {
        let base = fmt!("/subsonic-response/{}", list);
        let count = doc.len(&base).unwrap_or(0);
        Self {
            doc,
            base,
            count,
            next: 0,
            auth: server.auth_query(&salt()),
            queue: media::Queue::new(),
            queued: 0,
        }
    }

    /// Queue up to `max` more songs; `true` once the list is used up.
    pub fn step(&mut self, server: &Server, max: usize) -> bool {
        let end = self.count.min(self.next.saturating_add(max));
        for i in self.next..end {
            let Some(song) = parse_song(&self.doc, &self.base, i) else {
                continue;
            };
            let url = server.stream_url_with(&self.auth, &song.id);
            let meta = json!({
                "title": #s(&song.title),
                "artist": #s(song.artist.as_deref().unwrap_or("")),
                "album": #s(song.album.as_deref().unwrap_or("")),
                "cover_art": #s(song.cover_art.as_deref().unwrap_or(""))
            });
            self.queue.push(&media::Track {
                url: &url,
                headers: &[],
                duration_ms: song.duration_secs.map(|s| u64::from(s) * 1_000),
                meta_json: &meta,
            });
            self.queued += 1;
        }
        self.next = end;
        self.next == self.count
    }

    /// Songs queued so far.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued == 0
    }

    /// The queue for [`media::load`]; `paused` loads it without starting.
    #[must_use]
    pub fn into_json(mut self, shuffle: bool, repeat: media::Repeat, paused: bool) -> String {
        self.queue
            .shuffle(shuffle)
            .repeat(repeat)
            .paused(paused)
            .into_json()
    }
}

/// The server's refusal, if the response is one (wrong password, unknown
/// playlist, …); `None` for an `ok` response.
#[must_use]
pub fn response_error(doc: &JsonDoc) -> Option<String> {
    match doc.str("/subsonic-response/status").as_deref() {
        Some("ok") => None,
        Some(_) => Some(
            doc.str("/subsonic-response/error/message")
                .unwrap_or_else(|| "request failed".into()),
        ),
        None => Some("not a Subsonic server".into()),
    }
}

/// Song `i` of the list at `base`; `None` for a video or podcast (they
/// share the list, the player plays music only) or an entry without an id.
fn parse_song(doc: &JsonDoc, base: &str, i: usize) -> Option<Song> {
    let at = |field: &str| doc.str(&fmt!("{}/{}/{}", base, i, field));
    if doc.bool(&fmt!("{}/{}/isVideo", base, i)) == Some(true) {
        return None;
    }
    Some(Song {
        id: at("id")?,
        title: at("title").unwrap_or_else(|| "Unknown title".into()),
        artist: at("artist"),
        album: at("album"),
        duration_secs: doc
            .i64(&fmt!("{}/{}/duration", base, i))
            .and_then(|d| u32::try_from(d).ok()),
        cover_art: at("coverArt"),
    })
}

/// The id of the playlist called `name` (case-insensitive) in a
/// `getPlaylists` response.
#[must_use]
pub fn find_playlist(doc: &JsonDoc, name: &str) -> Option<String> {
    let base = "/subsonic-response/playlists/playlist";
    let wanted = name.trim().to_lowercase();
    (0..doc.len(base).unwrap_or(0)).find_map(|i| {
        let found = doc.str(&fmt!("{}/{}/name", base, i))?;
        (found.trim().to_lowercase() == wanted).then(|| doc.str(&fmt!("{}/{}/id", base, i)))?
    })
}

fn token(password: &str, salt: &str) -> String {
    let digest = Md5::new_with_prefix(password.as_bytes())
        .chain_update(salt.as_bytes())
        .finalize();
    hex(&digest)
}

fn salt() -> String {
    let mut bytes = [0_u8; 8];
    bytes[..4].copy_from_slice(&bmc_wasm_sdk::host::random_u32().to_le_bytes());
    bytes[4..].copy_from_slice(&bmc_wasm_sdk::host::random_u32().to_le_bytes());
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use bmc_wasm_sdk::{JsonDoc, media, wasm_test};

    use super::{
        QueueBuild, SONGS_PER_STEP, Server, Song, find_playlist, parse_song, response_error, token,
    };

    fn parse_songs(doc: &JsonDoc, list: &str) -> Vec<Song> {
        let base = format!("/subsonic-response/{list}");
        (0..doc.len(&base).unwrap_or(0))
            .filter_map(|i| parse_song(doc, &base, i))
            .collect()
    }

    #[wasm_test]
    fn the_token_matches_the_subsonic_spec_example() {
        // http://www.subsonic.org/pages/api.jsp, "Authentication".
        assert_eq!(
            token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[wasm_test]
    fn urls_carry_encoded_auth_and_the_method_arguments() {
        let server = Server::new("https://music.example/ ", "jan nov", "x", 192);
        let url = server.api_url_salted("stream", &[("id", "a&b"), ("format", "mp3")], "c19b2d");
        assert!(
            url.starts_with("https://music.example/rest/stream?u=jan+nov&t="),
            "{url}"
        );
        assert!(
            url.contains("&s=c19b2d&v=1.16.1&c=braiins-deck&f=json&id=a%26b&format=mp3"),
            "{url}"
        );
    }

    const SONGS: &[u8] = br#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[
        {"id":"s1","title":"Was That What It Was","artist":"Pet Shop Boys","album":"Alternative",
         "duration":281,"coverArt":"al-7"},
        {"id":"v1","title":"Clip","isVideo":true},
        {"id":"s2"}]}}}"#;

    #[wasm_test]
    fn songs_are_parsed_and_videos_left_out() {
        let songs = parse_songs(&JsonDoc::parse(SONGS), "randomSongs/song");
        assert_eq!(songs.len(), 2);
        assert_eq!(
            songs[0],
            Song {
                id: "s1".into(),
                title: "Was That What It Was".into(),
                artist: Some("Pet Shop Boys".into()),
                album: Some("Alternative".into()),
                duration_secs: Some(281),
                cover_art: Some("al-7".into()),
            }
        );
        assert_eq!(songs[1].title, "Unknown title");
        assert_eq!(songs[1].duration_secs, None);
    }

    #[wasm_test]
    fn a_refusal_carries_the_servers_message() {
        let wrong = JsonDoc::parse(
            br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}"#,
        );
        assert_eq!(
            response_error(&wrong).as_deref(),
            Some("Wrong username or password")
        );
        assert_eq!(response_error(&JsonDoc::parse(SONGS)), None);
        assert_eq!(
            response_error(&JsonDoc::parse(b"<html>")).as_deref(),
            Some("not a Subsonic server")
        );
    }

    #[wasm_test]
    fn a_playlist_is_found_by_name_ignoring_case() {
        let doc = JsonDoc::parse(
            r#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[
                {"id":"p1","name":"Jazz"},{"id":"p2","name":"Kuchyně "}]}}}"#
                .as_bytes(),
        );
        assert_eq!(find_playlist(&doc, "kuchyně").as_deref(), Some("p2"));
        assert_eq!(find_playlist(&doc, "Rock"), None);
    }

    #[wasm_test]
    fn the_queue_streams_mp3_and_carries_the_display_data() {
        let server = Server::new("http://m", "u", "p", 128);
        let mut build = QueueBuild::new(&server, JsonDoc::parse(SONGS), "randomSongs/song");
        assert!(build.step(&server, SONGS_PER_STEP));
        assert_eq!(build.len(), 2);
        let json = build.into_json(true, media::Repeat::All, false);
        let doc = JsonDoc::parse(json.as_bytes());
        let url = doc.str("/tracks/0/url").expect("BUG: a stream URL");
        assert!(url.starts_with("http://m/rest/stream?"), "{url}");
        assert!(url.contains("id=s1&format=mp3&maxBitRate=128"), "{url}");
        assert_eq!(doc.i64("/tracks/0/duration_ms"), Some(281_000));
        assert_eq!(
            doc.str("/tracks/0/meta/artist").as_deref(),
            Some("Pet Shop Boys")
        );
        assert_eq!(doc.str("/tracks/0/meta/cover_art").as_deref(), Some("al-7"));
        assert_eq!(doc.str("/tracks/1/meta/cover_art").as_deref(), Some(""));
        let second = doc.str("/tracks/1/url").expect("BUG: a stream URL");
        let auth = |url: &str| url.split("&id=").next().map(str::to_owned);
        assert_eq!(auth(&url), auth(&second), "one signature per queue");
        assert_eq!(doc.bool("/shuffle"), Some(true));
        assert_eq!(doc.str("/repeat").as_deref(), Some("all"));
    }

    /// `count` songs as Navidrome sends them, with all the fields we skip.
    fn full_list(count: usize) -> Vec<u8> {
        let song = r#"{"id":"0a1b2c3d4e5f60718293a4b5c6d7e8f9","parent":"9f8e7d6c5b4a39281706f5e4d3c2b1a0",
            "isDir":false,"title":"Was That What It Was (Extended Remix)","album":"Alternative (Disc 2)",
            "artist":"Pet Shop Boys","track":7,"year":1995,"genre":"Synthpop",
            "coverArt":"mf-0a1b2c3d4e5f60718293a4b5c6d7e8f9_65f1e2d3","size":11234567,
            "contentType":"audio/flac","suffix":"flac","duration":281,"bitRate":1011,
            "path":"Pet Shop Boys/Alternative/2-07 Was That What It Was.flac","playCount":3,
            "discNumber":2,"created":"2026-11-05T09:12:44.123456789Z",
            "albumId":"9f8e7d6c5b4a39281706f5e4d3c2b1a0","artistId":"1234567890abcdef1234567890abcdef",
            "type":"music","isVideo":false,"bpm":0,"comment":"","sortName":"was that what it was",
            "mediaType":"song","musicBrainzId":"","genres":[{"name":"Synthpop"}],
            "replayGain":{"trackGain":-7.1,"albumGain":-6.4,"trackPeak":1,"albumPeak":1},
            "channelCount":2,"samplingRate":44100}"#;
        format!(
            r#"{{"subsonic-response":{{"status":"ok","randomSongs":{{"song":[{}]}}}}}}"#,
            vec![song; count].join(",")
        )
        .into_bytes()
    }

    // Each step runs in a frame of its own; running out of fuel there
    // tears the widget down.
    #[wasm_test(
        fuel = 5_000_000,
        reason = "half a frame on 26.10 firmware (10 M), which is less than master's"
    )]
    fn a_step_fits_well_within_a_frame() {
        let server = Server::new("https://music.example.com", "deck", "secret", 192);
        let doc = JsonDoc::parse(&full_list(SONGS_PER_STEP));
        let mut build = QueueBuild::new(&server, doc, "randomSongs/song");
        assert!(build.step(&server, SONGS_PER_STEP));
        assert_eq!(build.len(), SONGS_PER_STEP);
    }

    #[wasm_test(
        fuel = 100_000_000,
        reason = "builds a whole 200-song queue in one test"
    )]
    fn a_long_list_is_queued_over_several_steps() {
        let server = Server::new("https://music.example.com", "deck", "secret", 192);
        let doc = JsonDoc::parse(&full_list(200));
        let mut build = QueueBuild::new(&server, doc, "randomSongs/song");
        let mut steps = 1;
        while !build.step(&server, SONGS_PER_STEP) {
            steps += 1;
        }
        assert_eq!((steps, build.len()), (13, 200));
        let json = build.into_json(false, media::Repeat::Off, false);
        assert_eq!(JsonDoc::parse(json.as_bytes()).len("/tracks"), Some(200));
    }
}
