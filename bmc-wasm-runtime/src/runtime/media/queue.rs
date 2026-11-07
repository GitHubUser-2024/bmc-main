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

//! The play queue: which track comes next under shuffle and repeat.
//!
//! Pure bookkeeping, no I/O: the player asks it for the current track and
//! moves it on, the guest's commands reorder or reposition it.

/// What happens at the end of a track and of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Repeat {
    /// Stop after the last track.
    #[default]
    Off,
    /// Start over after the last track.
    All,
    /// Play the current track again; skipping still moves on.
    One,
}

impl Repeat {
    pub(crate) fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::All),
            2 => Some(Self::One),
            _ => None,
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "all" => Some(Self::All),
            "one" => Some(Self::One),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

/// One entry the guest queued.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Track {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// From the guest (the stream itself carries no length).
    pub duration_ms: Option<u64>,
    /// Guest-owned metadata (title, artist, cover URL, …), echoed back in
    /// the status as-is so a widget can redraw after a sleep.
    pub meta: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Queue {
    tracks: Vec<Track>,
    /// Play order as indices into `tracks`; identity unless shuffled.
    order: Vec<usize>,
    /// Position in `order`.
    cursor: usize,
    shuffle: bool,
    repeat: Repeat,
    rng: u64,
}

impl Queue {
    /// `start` indexes `tracks`; with `shuffle` that track plays first.
    pub(crate) fn new(
        tracks: Vec<Track>,
        start: usize,
        shuffle: bool,
        repeat: Repeat,
        seed: u64,
    ) -> Self {
        let start = start.min(tracks.len().saturating_sub(1));
        let mut queue = Self {
            order: (0..tracks.len()).collect(),
            tracks,
            cursor: start,
            shuffle: false,
            repeat,
            // xorshift has a fixed point at zero.
            rng: seed | 1,
        };
        queue.set_shuffle(shuffle);
        queue
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Index into the queued tracks (the guest's numbering), not play order.
    pub(crate) fn current_index(&self) -> Option<usize> {
        self.order.get(self.cursor).copied()
    }

    pub(crate) fn current(&self) -> Option<&Track> {
        self.current_index().and_then(|i| self.tracks.get(i))
    }

    pub(crate) fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub(crate) fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub(crate) fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// The current track stays current: turning shuffle on puts it first and
    /// shuffles the rest behind it, turning it off returns to queue order.
    pub(crate) fn set_shuffle(&mut self, shuffle: bool) {
        let current = self.current_index();
        self.shuffle = shuffle;
        self.order = (0..self.tracks.len()).collect();
        match (shuffle, current) {
            (true, Some(current)) => {
                self.order.swap(0, current);
                self.shuffle_from(1);
                self.cursor = 0;
            }
            (false, Some(current)) => self.cursor = current,
            (_, None) => self.cursor = 0,
        }
    }

    /// A track played out: repeat-one plays it again, otherwise as [`Self::next`].
    pub(crate) fn advance_after_end(&mut self) -> bool {
        if self.repeat == Repeat::One {
            return !self.is_empty();
        }
        self.next()
    }

    /// Skip forward. `false` when the queue ran out (repeat off).
    pub(crate) fn next(&mut self) -> bool {
        if self.cursor + 1 < self.order.len() {
            self.cursor += 1;
            return true;
        }
        if self.repeat == Repeat::Off || self.is_empty() {
            return false;
        }
        if self.shuffle {
            // A fresh round, but not the track that just ended first again.
            let last = self.current_index();
            self.shuffle_from(0);
            if self.order.len() > 1 && self.order.first().copied() == last {
                self.order.swap(0, 1);
            }
        }
        self.cursor = 0;
        true
    }

    /// Skip back. At the first track it wraps under repeat-all and stays put
    /// otherwise (the player restarts it either way).
    pub(crate) fn previous(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            self.cursor = self.order.len() - 1;
        }
    }

    /// Make the queued track `index` current. `false` when out of range.
    pub(crate) fn jump(&mut self, index: usize) -> bool {
        match self.order.iter().position(|&i| i == index) {
            Some(cursor) => {
                self.cursor = cursor;
                true
            }
            None => false,
        }
    }

    fn shuffle_from(&mut self, from: usize) {
        // Fisher–Yates over `order[from..]`.
        let n = self.order.len();
        for i in (from + 1..n).rev() {
            let span = u64::try_from(i - from + 1).expect("BUG: queue length fits u64");
            let pick =
                from + usize::try_from(self.next_random() % span).expect("BUG: below queue length");
            self.order.swap(i, pick);
        }
    }

    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::{Queue, Repeat, Track};

    fn tracks(n: usize) -> Vec<Track> {
        (0..n)
            .map(|i| Track {
                url: format!("http://music/{i}.mp3"),
                headers: Vec::new(),
                duration_ms: None,
                meta: serde_json::Value::Null,
            })
            .collect()
    }

    fn play_order(queue: &mut Queue) -> Vec<usize> {
        let mut seen = vec![queue.current_index().expect("BUG: a current track")];
        while queue.next() {
            seen.push(queue.current_index().expect("BUG: a current track"));
        }
        seen
    }

    #[test]
    fn in_order_without_shuffle_and_stops_at_the_end() {
        let mut queue = Queue::new(tracks(4), 1, false, Repeat::Off, 7);
        assert_eq!(play_order(&mut queue), [1, 2, 3]);
        assert_eq!(queue.current_index(), Some(3));
    }

    #[test]
    fn repeat_all_wraps_to_the_first_track() {
        let mut queue = Queue::new(tracks(3), 2, false, Repeat::All, 7);
        assert!(queue.next());
        assert_eq!(queue.current_index(), Some(0));
    }

    #[test]
    fn repeat_one_replays_at_the_end_but_skipping_moves_on() {
        let mut queue = Queue::new(tracks(3), 0, false, Repeat::One, 7);
        assert!(queue.advance_after_end());
        assert_eq!(queue.current_index(), Some(0));
        assert!(queue.next());
        assert_eq!(queue.current_index(), Some(1));
    }

    #[test]
    fn shuffle_plays_every_track_once_starting_with_the_chosen_one() {
        let mut queue = Queue::new(tracks(20), 5, true, Repeat::Off, 42);
        let order = play_order(&mut queue);
        assert_eq!(order[0], 5);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_ne!(
            order,
            (0..20).collect::<Vec<_>>(),
            "seed 42 should not leave the order unchanged"
        );
    }

    #[test]
    fn a_new_shuffled_round_never_repeats_the_last_track_first() {
        for seed in 1..50 {
            let mut queue = Queue::new(tracks(3), 0, true, Repeat::All, seed);
            queue.next();
            queue.next();
            let last = queue.current_index();
            assert!(queue.next());
            assert_ne!(queue.current_index(), last, "seed {seed}");
        }
    }

    #[test]
    fn toggling_shuffle_keeps_the_current_track() {
        let mut queue = Queue::new(tracks(10), 0, false, Repeat::Off, 9);
        queue.jump(6);
        queue.set_shuffle(true);
        assert_eq!(queue.current_index(), Some(6));
        queue.next();
        let now = queue.current_index();
        queue.set_shuffle(false);
        assert_eq!(queue.current_index(), now);
    }

    #[test]
    fn previous_stays_at_the_first_track_unless_repeating_all() {
        let mut queue = Queue::new(tracks(3), 0, false, Repeat::Off, 1);
        queue.previous();
        assert_eq!(queue.current_index(), Some(0));
        queue.set_repeat(Repeat::All);
        queue.previous();
        assert_eq!(queue.current_index(), Some(2));
    }

    #[test]
    fn jump_refuses_an_index_outside_the_queue() {
        let mut queue = Queue::new(tracks(3), 0, true, Repeat::Off, 3);
        assert!(queue.jump(2));
        assert_eq!(queue.current_index(), Some(2));
        assert!(!queue.jump(3));
        assert_eq!(queue.current_index(), Some(2));
    }

    #[test]
    fn an_empty_queue_has_no_current_track() {
        let mut queue = Queue::new(Vec::new(), 0, true, Repeat::All, 3);
        assert!(queue.current().is_none());
        assert!(!queue.next());
        assert!(!queue.advance_after_end());
    }

    #[test]
    fn repeat_names_and_wire_values_round_trip() {
        for repeat in [Repeat::Off, Repeat::All, Repeat::One] {
            assert_eq!(Repeat::from_name(repeat.name()), Some(repeat));
        }
        assert_eq!(Repeat::from_wire(2), Some(Repeat::One));
        assert_eq!(Repeat::from_wire(3), None);
    }
}
