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
//
// Braiins Systems s.r.o. and Braiins Forge s.r.o. each reserve the right
// to grant any party a license to this program, or any part thereof,
// under any terms, and such a grant shall be considered distinct from
// the grant above.

//! Fills the host's player with songs from the Subsonic server the params
//! name.
//!
//! The host's player ([`media`]) does the playing and keeps going while the
//! widget is off screen; this module only hands it a queue. Everything else
//! (transport, position, metadata) is read back from the player's status, so
//! a restarted widget shows a queue it didn't load itself.

use std::cell::RefCell;

use bmc_wasm_sdk::{FetchResponse, JsonDoc, fetch, fmt, log_warn, media, request_frame, ufmt};

use crate::manifest_params::{Params, SubsonicSource};
use crate::subsonic::{self, QueueBuild, SONGS_PER_STEP, Server, Source};

/// A queue request in flight.
struct Pending {
    server: Server,
    source: Source,
    shuffle: bool,
    repeat: media::Repeat,
    paused: bool,
    /// Set once the songs have arrived; [`step`] finishes it.
    build: Option<QueueBuild>,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    /// Why the last queue request failed, shown until the next one.
    static ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The server and source the params name, if a server is set.
#[must_use]
pub fn configured() -> Option<(Server, Source)> {
    let params = Params::current();
    let url = params.subsonic_url.filter(|u| !u.trim().is_empty())?;
    let server = Server::new(
        &url,
        params.subsonic_user.as_deref().unwrap_or(""),
        params.subsonic_password.as_deref().unwrap_or(""),
        u32::try_from(params.max_bitrate.as_manifest_value()).unwrap_or(192),
    );
    let source = match params.subsonic_source {
        SubsonicSource::Random => Source::Random,
        SubsonicSource::Starred => Source::Starred,
        SubsonicSource::Playlist => Source::Playlist(params.subsonic_playlist.unwrap_or_default()),
    };
    Some((server, source))
}

#[must_use]
pub fn is_loading() -> bool {
    PENDING.with(|p| p.borrow().is_some())
}

#[must_use]
pub fn error() -> Option<String> {
    ERROR.with(|e| e.borrow().clone())
}

/// Ask the server for songs and replace the player's queue with them.
/// Shuffle and repeat carry over from the queue it replaces; `paused`
/// loads the new queue without starting it.
pub fn load_from_server(status: &media::Status, paused: bool) {
    if is_loading() {
        return;
    }
    let Some((server, source)) = configured() else {
        set_error("No music server set");
        return;
    };
    if matches!(&source, Source::Playlist(name) if name.trim().is_empty()) {
        set_error("No playlist name set");
        return;
    }
    let url = server.songs_url(&source);
    // A random pick is already shuffled; shuffling it again changes nothing.
    let shuffle = status.shuffle() && source != Source::Random;
    PENDING.with(|p| {
        *p.borrow_mut() = Some(Pending {
            server,
            source,
            shuffle,
            repeat: status.repeat(),
            paused,
            build: None,
        });
    });
    ERROR.with(|e| *e.borrow_mut() = None);
    if fetch(&url, None, on_songs).is_none() {
        fail("Song request rejected");
    }
    request_frame();
}

fn on_songs(response: &FetchResponse) {
    let Some(doc) = checked(response) else { return };
    let source = PENDING.with(|p| p.borrow().as_ref().map(|p| p.source.clone()));
    match source {
        Some(Source::Random) => start_build(doc, "randomSongs/song"),
        Some(Source::Starred) => start_build(doc, "starred2/song"),
        Some(Source::Playlist(name)) => {
            let Some(id) = subsonic::find_playlist(&doc, &name) else {
                fail(&fmt!("Playlist \u{201c}{}\u{201d} not found", name.trim()));
                return;
            };
            let url = PENDING.with(|p| p.borrow().as_ref().map(|p| p.server.playlist_url(&id)));
            if url.is_none_or(|url| fetch(&url, None, on_playlist).is_none()) {
                fail("Playlist request rejected");
            }
        }
        None => {}
    }
}

fn on_playlist(response: &FetchResponse) {
    if let Some(doc) = checked(response) {
        start_build(doc, "playlist/entry");
    }
}

/// The response as a Subsonic document, or `None` after recording why not.
fn checked(response: &FetchResponse) -> Option<JsonDoc> {
    if !response.ok() {
        fail(&fmt!("Music server unreachable ({})", response.status));
        return None;
    }
    let doc = JsonDoc::parse(response.body());
    if let Some(reason) = subsonic::response_error(&doc) {
        fail(&reason);
        return None;
    }
    Some(doc)
}

/// Queue the songs over the next frames: a long list costs more fuel than
/// this callback has.
fn start_build(doc: JsonDoc, list: &str) {
    PENDING.with(|p| {
        if let Some(pending) = p.borrow_mut().as_mut() {
            pending.build = Some(QueueBuild::new(&pending.server, doc, list));
        }
    });
    request_frame();
}

/// Advance a queue build by one slice; called every frame. Hands the queue
/// to the player once the list is used up.
pub fn step() {
    let done = PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let Some(Pending {
            server,
            build: Some(build),
            ..
        }) = pending.as_mut()
        else {
            return false;
        };
        build.step(server, SONGS_PER_STEP)
    });
    if !done {
        if is_loading() {
            request_frame();
        }
        return;
    }
    let Some(Pending {
        shuffle,
        repeat,
        paused,
        build: Some(build),
        ..
    }) = PENDING.with(|p| p.borrow_mut().take())
    else {
        return;
    };
    if build.is_empty() {
        set_error("No songs to play");
        return;
    }
    if let Err(e) = media::load(&build.into_json(shuffle, repeat, paused)) {
        let why = match e {
            media::LoadError::Malformed => "malformed",
            media::LoadError::Invalid => "invalid",
            media::LoadError::TooLarge => "too large",
        };
        log_warn!("deck-music: queue refused ({})", why);
        set_error("The player refused the queue");
    }
    request_frame();
}

fn fail(reason: &str) {
    PENDING.with(|p| *p.borrow_mut() = None);
    set_error(reason);
}

fn set_error(reason: &str) {
    log_warn!("deck-music: {}", reason);
    ERROR.with(|e| *e.borrow_mut() = Some(reason.into()));
    request_frame();
}
