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

//! The current track's cover, kept in step with the track.
//!
//! Each cover is keyed by its Subsonic cover-art id. When the track changes
//! the old picture goes at once (the placeholder shows until the new one
//! arrives), and a response is only taken for the request that is still
//! wanted: skipping quickly leaves earlier requests in flight, and taking
//! one of their answers would show the previous song's cover under the
//! current one.

use std::cell::RefCell;

#[cfg_attr(not(test), expect(clippy::wildcard_imports))]
use bmc_wasm_sdk::*;

use crate::queue;

/// One bitmap for the whole widget: `set` evicts the previous cover.
static COVER: BitmapSlot = BitmapSlot::new("cover");

thread_local! {
    static TRACKER: RefCell<Tracker<FetchRequestId>> = const { RefCell::new(Tracker::new()) };
    /// The shown cover's bitmap and width / height.
    static BITMAP: RefCell<Option<(BitmapId, f32)>> = const { RefCell::new(None) };
}

/// The current cover: its bitmap and width / height, if it has arrived.
#[must_use]
pub fn current() -> Option<(BitmapId, f32)> {
    BITMAP.with(|b| *b.borrow())
}

/// Follow the track whose cover-art id is `id` (empty for none); call every
/// frame with the player's current track.
pub fn follow(id: &str) {
    if TRACKER.with(|t| t.borrow_mut().follow(id)) {
        BITMAP.with(|b| *b.borrow_mut() = None);
        COVER.evict();
    }
    request_if_needed();
}

fn request_if_needed() {
    let Some(id) = TRACKER.with(|t| t.borrow().to_request()) else {
        return;
    };
    let Some((server, _)) = queue::configured() else {
        return;
    };
    match fetch(&server.cover_url(&id), None, on_cover) {
        Some(request) => TRACKER.with(|t| t.borrow_mut().requested(request, id)),
        None => log_warn!("deck-music: cover request rejected"),
    }
}

fn on_cover(response: &FetchResponse) {
    let ok = response.ok() && !response.body().is_empty();
    match TRACKER.with(|t| t.borrow_mut().answered(response.request_id, ok)) {
        Answer::Ignore => {}
        Answer::Superseded => {
            // The track moved on while this was in flight: ask for the
            // cover that shows now instead.
            request_if_needed();
        }
        Answer::Failed => log_warn!("deck-music: no cover ({})", response.status),
        Answer::Take => {
            let Some(bitmap) = COVER.set(response.body()) else {
                TRACKER.with(|t| t.borrow_mut().undecodable());
                return;
            };
            let aspect = host::image_dimensions(response.body())
                .map_or(1.0, |(w, h)| if h > 0 { w as f32 / h as f32 } else { 1.0 });
            BITMAP.with(|b| *b.borrow_mut() = Some((bitmap, aspect)));
            request_frame();
        }
    }
}

/// What to do with a cover response.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// Not the request in flight (an old one); drop it.
    Ignore,
    /// For a cover no longer wanted; drop it and ask for the wanted one.
    Superseded,
    /// The wanted cover didn't come; not asked for again for this track.
    Failed,
    /// The wanted cover: show it.
    Take,
}

/// Which cover is wanted, shown and asked for, keyed by cover-art id. `R`
/// is the request handle (the host's [`FetchRequestId`]).
#[derive(Debug)]
struct Tracker<R> {
    /// The current track's cover-art id; empty for none.
    wanted: String,
    /// The id whose picture is up.
    shown: Option<String>,
    /// The request in flight and the id it asked for.
    flight: Option<(R, String)>,
    /// An id the server failed to give; not asked for again until the
    /// track changes.
    failed: Option<String>,
}

impl<R: Copy + PartialEq> Tracker<R> {
    const fn new() -> Self {
        Self {
            wanted: String::new(),
            shown: None,
            flight: None,
            failed: None,
        }
    }

    /// The track's cover is now `id`; `true` when the picture up belongs to
    /// another cover and has to go.
    fn follow(&mut self, id: &str) -> bool {
        if self.wanted == id {
            return false;
        }
        id.clone_into(&mut self.wanted);
        self.failed = None;
        if self.shown.as_deref().is_some_and(|shown| shown != id) {
            self.shown = None;
            return true;
        }
        false
    }

    /// The cover-art id to fetch now, if any.
    fn to_request(&self) -> Option<String> {
        let wanted = &self.wanted;
        let idle = self.flight.is_none();
        let missing = !wanted.is_empty()
            && self.shown.as_ref() != Some(wanted)
            && self.failed.as_ref() != Some(wanted);
        (idle && missing).then(|| wanted.clone())
    }

    fn requested(&mut self, request: R, id: String) {
        self.flight = Some((request, id));
    }

    fn answered(&mut self, request: R, ok: bool) -> Answer {
        let Some((_, id)) = self.flight.take_if(|(r, _)| *r == request) else {
            return Answer::Ignore;
        };
        if id != self.wanted {
            return Answer::Superseded;
        }
        if ok {
            self.shown = Some(id);
            Answer::Take
        } else {
            self.failed = Some(id);
            Answer::Failed
        }
    }

    /// The taken cover would not decode: treat it as failed.
    fn undecodable(&mut self) {
        self.failed = self.shown.take();
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Tracker};

    /// Send the request the tracker wants, as `request`.
    fn ask(t: &mut Tracker<u32>, request: u32) {
        let id = t.to_request().expect("a cover to ask for");
        t.requested(request, id);
    }

    #[test]
    fn a_new_track_drops_the_old_cover_and_asks_for_its_own() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 1);
        assert_eq!(t.answered(1, true), Answer::Take);
        assert_eq!(t.to_request(), None);

        assert!(t.follow("al-2"), "the first album's picture must go");
        assert_eq!(t.to_request().as_deref(), Some("al-2"));
    }

    #[test]
    fn the_same_album_keeps_its_picture() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 1);
        t.answered(1, true);
        // Next song, same album: same cover-art id, nothing to do.
        assert!(!t.follow("al-1"));
        assert_eq!(t.to_request(), None);
    }

    #[test]
    fn a_late_answer_for_a_skipped_song_is_not_shown() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 1);
        // Skipped before the first cover came.
        t.follow("al-2");
        assert_eq!(t.to_request(), None, "one request at a time");
        assert_eq!(t.answered(1, true), Answer::Superseded);
        assert_eq!(t.to_request().as_deref(), Some("al-2"));
        t.requested(2, "al-2".into());
        assert_eq!(t.answered(2, true), Answer::Take);
    }

    #[test]
    fn an_answer_to_no_request_in_flight_is_ignored() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 7);
        assert_eq!(t.answered(6, true), Answer::Ignore);
        assert_eq!(t.answered(7, true), Answer::Take);
        assert_eq!(t.answered(7, true), Answer::Ignore);
    }

    #[test]
    fn a_failed_cover_is_not_asked_for_again_until_the_track_changes() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 1);
        assert_eq!(t.answered(1, false), Answer::Failed);
        assert_eq!(t.to_request(), None);
        t.follow("al-2");
        t.follow("al-1");
        assert_eq!(t.to_request().as_deref(), Some("al-1"));
    }

    #[test]
    fn a_track_without_a_cover_shows_none() {
        let mut t = Tracker::<u32>::new();
        t.follow("al-1");
        ask(&mut t, 1);
        t.answered(1, true);
        assert!(t.follow(""));
        assert_eq!(t.to_request(), None);
    }
}
