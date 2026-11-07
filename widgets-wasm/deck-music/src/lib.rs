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

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

//! Deck Music: music from a Subsonic server (Navidrome, Gonic, …) on the
//! Deck's own speaker.
//!
//! The host's player ([`media`]) streams the queue and keeps playing while
//! the widget is off screen; the widget fills the queue ([`queue`]) and
//! draws the player's status: the cover, title and artist, a seek bar,
//! one symmetric row of round buttons (shuffle, previous, play/pause, next,
//! repeat) and a volume row.

mod cover;
mod manifest_params;
mod queue;
mod subsonic;

use std::cell::Cell;

use bmc_wasm_sdk::*;

mod icons {
    use bmc_wasm_sdk::{Svg, include_svg};

    pub const PLAY: Svg = include_svg!("assets/icons/play.svg");
    pub const PAUSE: Svg = include_svg!("assets/icons/pause.svg");
    pub const PREVIOUS: Svg = include_svg!("assets/icons/skip-back.svg");
    pub const NEXT: Svg = include_svg!("assets/icons/skip-forward.svg");
    pub const SHUFFLE: Svg = include_svg!("assets/icons/shuffle.svg");
    pub const REPEAT: Svg = include_svg!("assets/icons/repeat.svg");
    pub const REPEAT_ONE: Svg = include_svg!("assets/icons/repeat-one.svg");
    pub const VOLUME_DOWN: Svg = include_svg!("assets/icons/volume-down.svg");
    pub const VOLUME_UP: Svg = include_svg!("assets/icons/volume-up.svg");
    pub const MUSIC: Svg = include_svg!("assets/icons/music.svg");
}

/// Volume buttons step by this much (of 100).
const VOLUME_STEP: u8 = 5;
/// Redraw this often while the position moves, and while idle.
const FRAME_PLAYING_MS: u32 = 500;
const FRAME_IDLE_MS: u32 = 2_000;

/// Colours: buttons are gray discs, the play button a white one; a mode
/// that is on (shuffle, repeat) gets the accent.
const BUTTON_FILL: Color = GRAY_80;
const BUTTON_PRESSED: Color = GRAY_60;
const ICON: Color = GRAY_10;
const ICON_DISABLED: Color = GRAY_60;
const ACCENT: Color = GREEN_50;
const PLAY_FILL: Color = WHITE;
const PLAY_PRESSED: Color = GRAY_30;
const PLAY_ICON: Color = GRAY_100;

thread_local! {
    /// Where the finger holds the seek / volume bar, shown until release.
    static SEEK_PREVIEW: Cell<Option<f32>> = const { Cell::new(None) };
    static VOLUME_PREVIEW: Cell<Option<f32>> = const { Cell::new(None) };
}

// ── Entry points ─────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    request_frame();
}

/// A new server, account or source replaces the queue: paused, unless
/// music was playing.
#[unsafe(no_mangle)]
pub extern "C" fn on_params_update() {
    const QUEUE_KEYS: &[&str] = &[
        "subsonic_url",
        "subsonic_user",
        "subsonic_password",
        "subsonic_source",
        "subsonic_playlist",
        "max_bitrate",
    ];
    let current = manifest_params::Params::current();
    let changed = manifest_params::Params::previous().is_some_and(|prev| {
        current
            .changed_keys(&prev)
            .iter()
            .any(|k| QUEUE_KEYS.contains(k))
    });
    let status = media::status();
    if changed && status.count() > 0 {
        let playing = matches!(
            status.state(),
            media::PlayState::Playing | media::PlayState::Loading
        );
        queue::load_from_server(&status, !playing);
    }
    request_frame();
}

/// Back on screen: the player went on without us, so redraw from its status.
#[unsafe(no_mangle)]
pub extern "C" fn on_wake() {
    request_frame();
}

/// The host never renders on touch by itself.
#[unsafe(no_mangle)]
pub extern "C" fn on_touch() {
    request_frame();
}

#[unsafe(no_mangle)]
pub extern "C" fn render(_delta_ms: u32) {
    queue::step();
    let status = media::status();
    let view = View::new(&status);
    cover::follow(view.cover_art.as_deref().unwrap_or(""));

    let size = widget_size();
    let result = render_ui(size.width, size.height, build(size, &view));
    handle(&result, &status, &view);

    if queue::is_loading() {
        request_frame();
    } else if view.state == media::PlayState::Playing || view.state == media::PlayState::Loading {
        request_frame_after(FRAME_PLAYING_MS);
    } else {
        request_frame_after(FRAME_IDLE_MS);
    }
}

// ── What to show ─────────────────────────────────────────────────

/// The player's status, as the screen shows it.
struct View {
    state: media::PlayState,
    has_track: bool,
    title: String,
    /// Artist and album, joined; empty when neither is known.
    subtitle: String,
    cover_art: Option<String>,
    position_ms: u64,
    duration_ms: Option<u64>,
    /// 0–100.
    volume: u8,
    shuffle: bool,
    repeat: media::Repeat,
}

impl View {
    fn new(status: &media::Status) -> Self {
        let has_track = status.index().is_some();
        let meta = |key: &str| {
            status
                .meta_str(&fmt!("/{}", key))
                .filter(|v| has_track && !v.is_empty())
        };
        let title = meta("title").unwrap_or_else(|| idle_line(status));
        let subtitle = match (meta("artist"), meta("album")) {
            (Some(artist), Some(album)) => fmt!("{} \u{2014} {}", artist, album),
            (Some(one), None) | (None, Some(one)) => one,
            (None, None) => String::new(),
        };
        Self {
            state: status.state(),
            has_track,
            title,
            subtitle,
            cover_art: meta("cover_art"),
            position_ms: status.position_ms(),
            duration_ms: status.duration_ms().filter(|d| *d > 0),
            volume: status.volume(),
            shuffle: status.shuffle(),
            repeat: status.repeat(),
        }
    }

    fn is_playing(&self) -> bool {
        matches!(
            self.state,
            media::PlayState::Playing | media::PlayState::Loading
        )
    }

    fn can_seek(&self) -> bool {
        self.has_track && self.duration_ms.is_some()
    }
}

/// The title line when no track is current: what's wrong, or what a tap
/// on play does.
fn idle_line(status: &media::Status) -> String {
    if queue::is_loading() {
        return "Loading songs\u{2026}".into();
    }
    if let Some(error) = queue::error().or_else(|| status.error()) {
        return error;
    }
    if queue::configured().is_none() {
        return "Set a music server in the widget settings".into();
    }
    "Tap play for music".into()
}

// ── Layout ───────────────────────────────────────────────────────

/// Sizes of one layout; everything is centred on the vertical axis.
struct Metrics {
    pad: f32,
    gap: f32,
    /// 0 = no cover (the 238 px tall sizes have no room for one).
    cover: f32,
    title: u32,
    subtitle: u32,
    time: u32,
    track_h: f32,
    button: f32,
    play: f32,
    button_gap: f32,
    /// Seek and volume rows don't grow past this.
    bar_width: f32,
}

impl Metrics {
    fn for_size(size: WidgetSize) -> Self {
        match size.variant {
            SizeVariant::Full | SizeVariant::Large => Self {
                pad: 14.0,
                gap: 6.0,
                cover: 168.0,
                title: 24,
                subtitle: 16,
                time: 16,
                track_h: 6.0,
                button: 64.0,
                play: 84.0,
                button_gap: 28.0,
                bar_width: 560.0,
            },
            SizeVariant::Medium => Self {
                pad: 10.0,
                gap: 4.0,
                cover: 0.0,
                title: 18,
                subtitle: 14,
                time: 13,
                track_h: 4.0,
                button: 52.0,
                play: 66.0,
                button_gap: 22.0,
                bar_width: 480.0,
            },
            SizeVariant::Small => Self {
                pad: 8.0,
                gap: 4.0,
                cover: 0.0,
                title: 16,
                subtitle: 12,
                time: 12,
                track_h: 4.0,
                button: 44.0,
                play: 56.0,
                button_gap: 10.0,
                bar_width: 300.0,
            },
        }
    }
}

fn build(size: WidgetSize, view: &View) -> Node {
    let m = Metrics::for_size(size);
    let mut children = Vec::new();
    if m.cover > 0.0 {
        children.push(centered_row([cover_node(m.cover)]));
    }
    children.push(text(
        &view.title,
        style!(size: m.title, color: GRAY_10, weight: FontWeight::BOLD,
               align: TextAlign::Center, text_overflow: TextOverflow::Ellipsis),
    ));
    children.push(text(
        if view.subtitle.is_empty() {
            "\u{2007}"
        } else {
            &view.subtitle
        },
        style!(size: m.subtitle, color: GRAY_40,
               align: TextAlign::Center, text_overflow: TextOverflow::Ellipsis),
    ));
    children.push(spacer(1.0));
    children.push(centered_row([seek_row(view, &m)]));
    children.push(spacer(1.0));
    children.push(row(
        props!(gap: m.button_gap, justify_content: Justify::Center,
               cross_align: CrossAlign::Center),
        button_row(view, &m),
    ));
    children.push(spacer(1.0));
    children.push(centered_row([volume_row(view, &m)]));
    col(
        props!(background: GRAY_100, padding: m.pad, gap: m.gap),
        children,
    )
}

/// `children` side by side in the middle of the width.
fn centered_row<const N: usize>(children: [Node; N]) -> Node {
    row(
        props!(justify_content: Justify::Center, cross_align: CrossAlign::Center),
        children,
    )
}

fn cover_node(side: f32) -> Node {
    let draws = if let Some((bitmap, aspect)) = cover::current() {
        // Contain: the whole picture, centred in the square.
        let (w, h) = if aspect >= 1.0 {
            (side, side / aspect)
        } else {
            (side * aspect, side)
        };
        vec![Draw::bitmap_id(
            (side - w) / 2.0,
            (side - h) / 2.0,
            w,
            h,
            Some(bitmap),
        )]
    } else {
        let icon = side * 0.35;
        vec![
            Draw::rect(0.0, 0.0, side, side, GRAY_80),
            Draw::centered(
                Draw::svg(0.0, 0.0, icon, icon, &icons::MUSIC, GRAY_60).with_anti_alias(),
            ),
        ]
    };
    canvas(props!(width: side, height: side), draws)
}

/// `0:42 ━━━━○──── 3:58`, the times in fixed-width boxes so the bar stays
/// centred.
fn seek_row(view: &View, m: &Metrics) -> Node {
    let duration = view.duration_ms.unwrap_or(0);
    let position = SEEK_PREVIEW
        .get()
        .filter(|_| view.can_seek())
        .map_or(view.position_ms, |frac| (frac * duration as f32) as u64);
    let mode = if duration > 0 {
        ProgressMode::Slider((position as f32 / duration as f32).clamp(0.0, 1.0))
    } else {
        ProgressMode::Slider(0.0)
    };
    let time_w = m.time as f32 * 3.2;
    row(
        props!(width: m.bar_width, gap: 10.0, cross_align: CrossAlign::Center),
        [
            text(
                typography::unbroken(clock(view.has_track.then_some(position))),
                style!(size: m.time, color: GRAY_40, align: TextAlign::Right,
                       max_width: time_w as u32),
            ),
            progress_bar!(mode,
                touch_key: "seek", track_h: m.track_h, active: view.is_playing(),
                fill_color: GRAY_20, track_color: GRAY_70, bg_color: TRANSPARENT,
            ),
            text(
                typography::unbroken(clock(view.duration_ms)),
                style!(size: m.time, color: GRAY_40, max_width: time_w as u32),
            ),
        ],
    )
}

/// `m:ss`, or `-:--` when unknown.
fn clock(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => {
            let secs = ms / 1_000;
            let s = secs % 60;
            if s < 10 {
                fmt!("{}:0{}", secs / 60, s)
            } else {
                fmt!("{}:{}", secs / 60, s)
            }
        }
        None => "-:--".into(),
    }
}

/// Shuffle, previous, play/pause, next, repeat: mirror-symmetric around the
/// larger play button.
fn button_row(view: &View, m: &Metrics) -> [Node; 5] {
    let has_track = view.has_track;
    let repeat_icon = if view.repeat == media::Repeat::One {
        &icons::REPEAT_ONE
    } else {
        &icons::REPEAT
    };
    [
        round_button(
            "shuffle",
            &icons::SHUFFLE,
            m.button,
            Look::Mode(view.shuffle),
        ),
        round_button(
            "previous",
            &icons::PREVIOUS,
            m.button,
            Look::Plain(has_track),
        ),
        round_button(
            "play",
            if view.is_playing() {
                &icons::PAUSE
            } else {
                &icons::PLAY
            },
            m.play,
            Look::Play,
        ),
        round_button("next", &icons::NEXT, m.button, Look::Plain(has_track)),
        round_button(
            "repeat",
            repeat_icon,
            m.button,
            Look::Mode(view.repeat != media::Repeat::Off),
        ),
    ]
}

/// `(−) ━━━━━○───── (+)`.
fn volume_row(view: &View, m: &Metrics) -> Node {
    let frac = VOLUME_PREVIEW
        .get()
        .unwrap_or(f32::from(view.volume) / 100.0);
    let side = m.button * 0.75;
    row(
        props!(width: m.bar_width, gap: 14.0, cross_align: CrossAlign::Center),
        [
            round_button(
                "volume_down",
                &icons::VOLUME_DOWN,
                side,
                Look::Plain(view.volume > 0),
            ),
            progress_bar!(ProgressMode::Slider(frac),
                touch_key: "volume", track_h: m.track_h,
                fill_color: GRAY_30, track_color: GRAY_70, bg_color: TRANSPARENT,
            ),
            round_button(
                "volume_up",
                &icons::VOLUME_UP,
                side,
                Look::Plain(view.volume < 100),
            ),
        ],
    )
}

#[derive(Clone, Copy)]
enum Look {
    Play,
    /// Shuffle or repeat; `true` while on.
    Mode(bool),
    /// `false` greys the icon; the button still takes the tap.
    Plain(bool),
}

/// A round, tappable button: a disc with the icon in the middle, lighter
/// while the finger is on it.
fn round_button(key: &str, icon: &'static Svg, side: f32, look: Look) -> Node {
    let pressed = host::get_touch_drag(key).is_some();
    let (fill, icon_color) = match look {
        Look::Play => (if pressed { PLAY_PRESSED } else { PLAY_FILL }, PLAY_ICON),
        Look::Mode(on) => (
            if pressed { BUTTON_PRESSED } else { BUTTON_FILL },
            if on { ACCENT } else { ICON },
        ),
        Look::Plain(enabled) => (
            if pressed { BUTTON_PRESSED } else { BUTTON_FILL },
            if enabled { ICON } else { ICON_DISABLED },
        ),
    };
    let r = side / 2.0;
    let icon_side = side * 0.4;
    let mut draws = vec![
        Draw::circle(r, r, r, fill),
        Draw::centered(
            Draw::svg(0.0, 0.0, icon_side, icon_side, icon, icon_color).with_anti_alias(),
        ),
    ];
    if let Look::Mode(true) = look {
        // A dot under the icon: "on" reads without colour too.
        draws.push(Draw::circle(r, side * 0.8, side * 0.04, ACCENT));
    }
    touchable(key, props!(width: side, height: side), draws)
}

// ── Input ────────────────────────────────────────────────────────

fn handle(result: &TreeRenderResult, status: &media::Status, view: &View) {
    // Bars: follow the finger, act on release (the click).
    if view.can_seek() {
        SEEK_PREVIEW.set(result.drags.get("seek").map(TouchHit::frac_x));
        if let Some(hit) = result.clicks.get("seek")
            && let Some(duration) = view.duration_ms
        {
            media::seek((hit.frac_x() * duration as f32) as u32);
            SEEK_PREVIEW.set(None);
        }
    } else {
        SEEK_PREVIEW.set(None);
    }
    VOLUME_PREVIEW.set(result.drags.get("volume").map(TouchHit::frac_x));
    if let Some(hit) = result.clicks.get("volume") {
        media::set_volume((hit.frac_x() * 100.0).round() as u8);
        VOLUME_PREVIEW.set(None);
    }
    if SEEK_PREVIEW.get().is_some() || VOLUME_PREVIEW.get().is_some() {
        request_frame();
    }

    let clicked = |key: &str| result.clicks.contains_key(key);
    if clicked("play") {
        if status.state() == media::PlayState::Stopped {
            // Nothing queued, or the queue played out: a fresh pick.
            queue::load_from_server(status, false);
        } else {
            media::toggle();
        }
    }
    if clicked("previous") {
        media::previous();
    }
    if clicked("next") {
        media::next();
    }
    if clicked("shuffle") {
        media::set_shuffle(!view.shuffle);
    }
    if clicked("repeat") {
        media::set_repeat(view.repeat.cycled());
    }
    if clicked("volume_down") {
        media::set_volume(view.volume.saturating_sub(VOLUME_STEP));
    }
    if clicked("volume_up") {
        media::set_volume(view.volume.saturating_add(VOLUME_STEP).min(100));
    }
    if !result.clicks.is_empty() {
        request_frame();
    }
}
