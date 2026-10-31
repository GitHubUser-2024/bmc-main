// Copyright (C) 2026  Braiins Forge s.r.o.
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

//! Immich photo widget — the image widget's picture pipeline, plus a
//! **Label it** menu button that tags the photo on screen in Immich, which
//! turns into **Remove label** once the photo has that tag.
//!
//! The photo URL points at a helper in front of Immich (the widget can't send
//! headers, so it can't hold an Immich key). The helper names each photo in an
//! `X-Immich-Asset` response header and its tags in `X-Immich-Tags`; the menu
//! button POSTs the id with the configured label as JSON to the label URL
//! (`"remove": true` to take the tag off), and the helper updates Immich.

mod manifest_params;

/// Response header naming the Immich asset a photo response carries.
pub const ASSET_HEADER: &str = "X-Immich-Asset";
/// Response header listing the asset's tags, each percent-encoded, comma-separated.
pub const TAGS_HEADER: &str = "X-Immich-Tags";

/// The JSON body the menu button sends: `{"asset": …, "label": …}`, plus
/// `"remove": true` when it takes the label off.
#[must_use]
pub fn label_body(asset: &str, label: &str, remove: bool) -> String {
    format!(
        "{{\"asset\":\"{}\",\"label\":\"{}\"{}}}",
        json_escape(asset),
        json_escape(label),
        if remove { ",\"remove\":true" } else { "" }
    )
}

/// Split an `X-Immich-Tags` value into tag names.
#[must_use]
pub fn parse_tags(header: &str) -> Vec<String> {
    header
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(percent_decode)
        .collect()
}

/// Undo percent-encoding; a malformed escape stays as written.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(hi), Some(lo)) = (hex(hi), hex(lo))
        {
            // Two hex digits always fit a byte.
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_escape(s: &str) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// A param counts as set only with something besides whitespace in it.
#[must_use]
pub fn configured(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_body_escapes_quotes_and_backslashes() {
        assert_eq!(
            label_body("0989ac28", "a \"b\" \\ c", false),
            r#"{"asset":"0989ac28","label":"a \"b\" \\ c"}"#
        );
    }

    #[test]
    fn label_body_escapes_control_characters() {
        assert_eq!(
            label_body("x", "a\nb\u{1}", false),
            r#"{"asset":"x","label":"a\nb\u0001"}"#
        );
    }

    #[test]
    fn label_body_keeps_non_ascii() {
        assert_eq!(
            label_body("x", "Dovolená", false),
            r#"{"asset":"x","label":"Dovolená"}"#
        );
    }

    #[test]
    fn label_body_asks_for_removal() {
        assert_eq!(
            label_body("x", "Print", true),
            r#"{"asset":"x","label":"Print","remove":true}"#
        );
    }

    #[test]
    fn parse_tags_decodes_each_tag() {
        assert_eq!(
            parse_tags("ToDelete,Dovolen%C3%A1,a%2Cb,Family%2FKids"),
            ["ToDelete", "Dovolená", "a,b", "Family/Kids"]
        );
    }

    #[test]
    fn parse_tags_handles_empty_and_malformed() {
        assert!(parse_tags("").is_empty());
        assert!(parse_tags(" , ").is_empty());
        assert_eq!(parse_tags("50%,%zz"), ["50%", "%zz"]);
    }

    #[test]
    fn configured_ignores_blank_values() {
        assert_eq!(configured(None), None);
        assert_eq!(configured(Some("  ")), None);
        assert_eq!(configured(Some(" Print ")), Some("Print"));
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_glue {
    use super::manifest_params::{self, Sizing};
    use super::{ASSET_HEADER, TAGS_HEADER, configured, label_body, parse_tags};
    use std::cell::{Cell, RefCell};

    use bmc_wasm_sdk::net::FetchRequest;
    #[expect(
        clippy::wildcard_imports,
        reason = "widget render uses many SDK exports"
    )]
    use bmc_wasm_sdk::*;
    use remote_image::machine::{self, Action, Badge, Event, View};
    use remote_image::{Fit, picture, render};

    /// Menu clears itself this long after opening, untouched.
    const MENU_AUTO_DISMISS_MS: u32 = 10_000;
    /// How long a label change's outcome stays on screen.
    const NOTICE_MS: u32 = 3_000;

    const CONFIGURE_URL: &str = "Set a photo URL";
    const KEY_LABEL: &str = "menu_label";

    const ICON_RENEW: Svg = include_svg!("assets/renew.svg");
    const MENU_WIDTH: f32 = 200.0;
    const MENU_GAP: f32 = 8.0;
    const MENU_PADDING: f32 = 12.0;
    const NOTICE_TEXT_SIZE: u32 = 14;

    thread_local! {
        static VIEW: RefCell<View> = const { RefCell::new(View::Loading { decode: None }) };
        static POLL: RefCell<Option<PollHandle>> = const { RefCell::new(None) };
        // Menu auto-dismiss countdown (ms); 0 = closed.
        static MENU_MS: Cell<u32> = const { Cell::new(0) };
        // First render restores from cache (init() has no renderer scope).
        static INITIAL_RESTORE: Cell<bool> = const { Cell::new(false) };
        /// Off-scene, between `on_sleep` and `on_wake`. The host keeps
        /// delivering params and fetch replies there, and no SDK call answers
        /// it, so the edge is tracked by hand. It starts set: every slot is
        /// born dormant and `on_wake` always precedes the first frame, so a
        /// widget built for a scene nobody opens must not fetch a picture.
        static DORMANT: Cell<bool> = const { Cell::new(true) };
        /// Immich asset of the last photo fetched. A photo restored from flash
        /// after a restart has none until the next fetch.
        static ASSET: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Immich tags of that asset, as the photo response listed them and
        /// as this widget has changed them since.
        static TAGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// The label change on its way (asset, label, remove); further taps
        /// wait for it.
        static PENDING: RefCell<Option<(String, String, bool)>> = const { RefCell::new(None) };
        /// Label change outcome on screen, and how long it stays (ms).
        static NOTICE: RefCell<Option<String>> = const { RefCell::new(None) };
        static NOTICE_LEFT_MS: Cell<u32> = const { Cell::new(0) };
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        let handle = register_poll(
            build_request,
            on_image,
            PollConfig {
                interval_ms: Some(refresh_interval_ms()),
                enabled: false, // first render restores + schedules
                ..Default::default()
            },
        );
        POLL.set(Some(handle));
        INITIAL_RESTORE.with(|f| f.set(true));
    }

    /// Fold an event into the view and run its side effects.
    fn dispatch(event: Event) {
        let cur = VIEW.with(|v| v.replace(View::Loading { decode: None }));
        let (next, actions) = machine::step(cur, event);
        VIEW.with(|v| *v.borrow_mut() = next);
        let dormant = DORMANT.with(Cell::get);
        for action in actions {
            if dormant && action.starts_fetch() {
                continue;
            }
            match action {
                Action::EnablePollAfter(ms) => with_poll(|h| h.enable_after(ms)),
                Action::ResumePoll => with_poll(|h| {
                    h.set_enabled(true);
                    h.invalidate();
                }),
                Action::DisablePoll => with_poll(|h| h.set_enabled(false)),
                Action::Retry => with_poll(PollHandle::retry),
                Action::DeferPoll => with_poll(|h| h.retry_after(refresh_interval_ms())),
                Action::MarkStale => with_poll(PollHandle::mark_stale),
                Action::SeedAnchor(secs) => with_poll(|h| h.restore_anchor(secs)),
                Action::RequestFrame => request_frame(),
            }
        }
    }

    fn with_poll(f: impl FnOnce(&PollHandle)) {
        POLL.with_borrow(|p| {
            if let Some(handle) = p {
                f(handle);
            }
        });
    }

    // Zero guard only, deliberately not the manifest's `min`. Staleness fires
    // at `interval * stale_factor`, so clamping a stored interval up here
    // would move that threshold too — quietly weakening the configured freshness.
    fn refresh_interval_ms() -> u32 {
        let secs = manifest_params::Params::current().refresh_seconds.max(1);
        u32::try_from(secs).unwrap_or(u32::MAX).saturating_mul(1000)
    }

    fn fit() -> Fit {
        match manifest_params::Params::current().sizing {
            Sizing::Contain => Fit::Contain,
            Sizing::Cover => Fit::Cover,
        }
    }

    // {{width}}/{{height}} expand to the viewport pixels.
    fn expanded_url() -> Option<String> {
        let size = widget_size();
        let url = manifest_params::Params::current().url?;
        machine::expand_url(&url, size.width, size.height)
    }

    // Cache identity: expanded URL + fit, so a URL/viewport/sizing change is a distinct blob.
    fn cache_identity() -> Option<String> {
        let mut id = expanded_url()?;
        id.push('\u{1f}');
        id.push_str(fit().identity_token());
        Some(id)
    }

    fn build_request(_handle: &PollHandle) -> Option<FetchSpec> {
        expanded_url().map(|url| FetchSpec::get(url).host_body())
    }

    /// Bring back whichever picture is on flash. Render scope only.
    fn restore_from_cache() -> Event {
        picture::restore(&cache_identity().unwrap_or_default(), refresh_interval_ms())
    }

    fn on_image(_handle: &PollHandle, response: &FetchResponse) {
        // The headers are only readable inside this callback.
        if response.ok() {
            ASSET.set(response.header(ASSET_HEADER).filter(|id| !id.is_empty()));
            TAGS.set(
                response
                    .header(TAGS_HEADER)
                    .as_deref()
                    .map(parse_tags)
                    .unwrap_or_default(),
            );
        }
        dispatch(picture::classify_body(
            response,
            widget_size(),
            fit(),
            &cache_identity().unwrap_or_default(),
            VIEW.with(|v| v.borrow().decode()),
            on_decoded,
        ));
    }

    fn on_decoded(job: ImageJobId, bitmap: Option<BitmapId>) {
        dispatch(match bitmap {
            Some(bitmap) => Event::Decoded { job, bitmap },
            None => Event::DecodeFailed { job },
        });
    }

    /// The label and label URL, when both are set.
    fn label_target() -> Option<(String, String)> {
        let params = manifest_params::Params::current();
        let label = configured(params.label.as_deref())?.to_owned();
        let url = configured(params.label_url.as_deref())?.to_owned();
        Some((label, url))
    }

    /// Whether the photo on screen carries the configured label.
    fn labelled() -> bool {
        label_target().is_some_and(|(label, _)| {
            ASSET.with_borrow(Option::is_some) && TAGS.with_borrow(|tags| tags.contains(&label))
        })
    }

    fn show_notice(message: String) {
        NOTICE.set(Some(message));
        NOTICE_LEFT_MS.with(|n| n.set(NOTICE_MS));
        request_frame();
    }

    fn send_label() {
        let Some((label, url)) = label_target() else {
            return;
        };
        if PENDING.with_borrow(Option::is_some) {
            return;
        }
        let Some(asset) = ASSET.with_borrow(Clone::clone) else {
            show_notice("Reload the photo first".to_owned());
            return;
        };
        let remove = labelled();
        let body = label_body(&asset, &label, remove);
        let sent = FetchRequest::post(&url)
            .headers("Content-Type: application/json")
            .body(body.as_bytes())
            .send(on_labelled);
        if sent.is_some() {
            show_notice(if remove {
                fmt!("Removing {}…", label)
            } else {
                fmt!("Labelling as {}…", label)
            });
            PENDING.set(Some((asset, label, remove)));
        } else {
            show_notice(failed_notice(remove).to_owned());
        }
    }

    fn failed_notice(remove: bool) -> &'static str {
        if remove {
            "Remove label failed"
        } else {
            "Label it failed"
        }
    }

    fn on_labelled(response: &FetchResponse) {
        let Some((asset, label, remove)) = PENDING.take() else {
            return;
        };
        if !response.ok() {
            log_warn!(
                "immich-photo: label change not accepted by {}",
                label_target().map(|(_, url)| url).unwrap_or_default()
            );
            show_notice(failed_notice(remove).to_owned());
            return;
        }
        // The photo may have moved on meanwhile; only its own tags change.
        if ASSET.with_borrow(|a| a.as_deref() == Some(asset.as_str())) {
            TAGS.with_borrow_mut(|tags| {
                tags.retain(|t| *t != label);
                if !remove {
                    tags.push(label.clone());
                }
            });
        }
        show_notice(if remove {
            fmt!("Removed {}", label)
        } else {
            fmt!("Labelled {}", label)
        });
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        let prev = manifest_params::Params::previous();
        let cur = manifest_params::Params::current();
        // Retarget the poll live on a refresh-period change; invalidate to apply
        // now. Both are safe off-screen: the interval is only stored, and
        // `invalidate` skips a poll `on_sleep` disabled.
        if prev
            .as_ref()
            .is_some_and(|p| p.refresh_seconds != cur.refresh_seconds)
        {
            with_poll(|handle| {
                handle.set_interval(refresh_interval_ms());
                handle.invalidate();
            });
        }
        if prev
            .as_ref()
            .is_none_or(|p| p.url != cur.url || p.sizing != cur.sizing)
        {
            // A new source: the old photo's asset is no longer on screen.
            if prev.as_ref().is_some_and(|p| p.url != cur.url) {
                ASSET.set(None);
                TAGS.set(Vec::new());
            }
            dispatch(Event::TargetChanged);
        } else {
            request_frame();
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_system_update() {
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_touch() {
        // The host only delivers touch to on_touch exporters; wake a frame to read it.
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_sleep() {
        dispatch(Event::Sleep);
        DORMANT.with(|d| d.set(true));
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_wake() {
        DORMANT.with(|d| d.set(false));
        INITIAL_RESTORE.with(|f| f.set(false)); // wake subsumes the cold-start restore
        if let Some(job) = VIEW.with(|v| picture::abandoned_decode(&v.borrow())) {
            dispatch(Event::DecodeAbandoned { job });
        }
        dispatch(restore_from_cache());
    }

    fn menu_open() -> bool {
        MENU_MS.with(Cell::get) > 0
    }

    /// Full-bleed transparent catcher (absolute via insets): opens / tap-outside-dismisses.
    fn tap_catcher() -> Node {
        touchable(
            render::KEY_TAP,
            props!(inset_top: 0.0, inset_right: 0.0, inset_bottom: 0.0, inset_left: 0.0),
            Vec::<Draw>::new(),
        )
    }

    /// The reveal menu: Reload, the label button (only when a label is set up)
    /// and Close. The label button reads Remove label once the photo has it.
    fn menu_panel(label_button: Option<&'static str>) -> Node {
        let mut buttons = vec![
            button!(render::KEY_RELOAD, "Reload", style: Primary, icon: tree::ensure_registered(&ICON_RENEW)).stretch(),
        ];
        if let Some(caption) = label_button {
            buttons.push(button!(KEY_LABEL, caption, style: Secondary).stretch());
        }
        buttons.push(button!(render::KEY_CLOSE, "Close", style: Secondary).stretch());
        center(
            props!(inset_top: 0.0, inset_right: 0.0, inset_bottom: 0.0, inset_left: 0.0),
            [col(
                props!(
                    width: MENU_WIDTH,
                    gap: MENU_GAP,
                    padding: MENU_PADDING,
                    background: GRAY_100
                ),
                buttons,
            )],
        )
    }

    fn notice_pill(message: &str) -> Node {
        row(
            props!(
                background: GRAY_100,
                padding: 6.0,
                cross_align: CrossAlign::Center
            ),
            [text(
                message.to_owned(),
                style!(
                    size: NOTICE_TEXT_SIZE,
                    weight: FontWeight::REGULAR,
                    color: GRAY_30,
                    line_height: 1.0
                ),
            )],
        )
    }

    /// Overlay the tap catcher (always) and the menu (when open) onto the root.
    fn with_interaction(
        mut root: Node,
        menu_open: bool,
        label_button: Option<&'static str>,
    ) -> Node {
        if let Node::Column(_, children) | Node::Row(_, children) = &mut root {
            children.push(tap_catcher());
            if menu_open {
                children.push(menu_panel(label_button));
            }
        }
        root
    }

    /// Count a timer down by a frame; true once it reaches zero.
    fn tick(timer: &'static std::thread::LocalKey<Cell<u32>>, delta_ms: u32) -> bool {
        let left = timer.with(Cell::get);
        if left == 0 {
            return false;
        }
        let left = left.saturating_sub(delta_ms);
        timer.with(|t| t.set(left));
        left == 0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn render(delta_ms: u32) {
        // First render restores from cache (init() has no renderer scope).
        if INITIAL_RESTORE.with(Cell::get) {
            INITIAL_RESTORE.with(|f| f.set(false));
            dispatch(restore_from_cache());
        }

        tick(&MENU_MS, delta_ms);
        if tick(&NOTICE_LEFT_MS, delta_ms) {
            NOTICE.set(None);
        }

        let size = widget_size();
        let shape = widget_viewport().shape;
        let params = manifest_params::Params::current();
        let mut base = if params
            .url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
        {
            render::message_view(CONFIGURE_URL, size)
        } else {
            VIEW.with(|v| match &*v.borrow() {
                View::Shown {
                    bitmap,
                    aspect,
                    badge,
                    ..
                } => {
                    let view = render::image_view(*bitmap, *aspect, size, fit());
                    match badge {
                        Badge::Updating => with_overlay(view, render::updating_pill(), shape),
                        // is_stale adds the grace window, so the 10s retry heals a blip first.
                        Badge::Stale => match POLL.with_borrow(|p| {
                            p.as_ref()
                                .filter(|handle| handle.is_stale())
                                .and_then(PollHandle::last_success_time)
                        }) {
                            Some(anchor) => with_stale_overlay(view, anchor, shape),
                            None => view,
                        },
                        // A broken payload states its specific reason at once.
                        Badge::Error(kind) => {
                            with_error_overlay(view, render::error_message(*kind), shape)
                        }
                        Badge::Fresh => view,
                    }
                }
                View::Loading { .. } => render::message_view(render::LOADING, size),
                View::Failed(kind) => render::message_view(render::error_message(*kind), size),
            })
        };
        if let Some(message) = NOTICE.with_borrow(Clone::clone) {
            base = with_overlay(base, notice_pill(&message), shape);
        }

        let open = menu_open();
        let label_button = label_target().is_some().then(|| {
            if labelled() {
                "Remove label"
            } else {
                "Label it"
            }
        });
        let result = render_ui(
            size.width,
            size.height,
            with_interaction(base, open, label_button),
        );

        // Route taps: a button when the menu is open, otherwise open/dismiss it.
        let changed = if open {
            if result.clicks.contains_key(render::KEY_RELOAD) {
                dispatch(Event::Reload);
                MENU_MS.with(|m| m.set(0));
                true
            } else if result.clicks.contains_key(KEY_LABEL) {
                MENU_MS.with(|m| m.set(0));
                send_label();
                true
            } else if result.clicks.contains_key(render::KEY_CLOSE)
                || result.clicks.contains_key(render::KEY_TAP)
            {
                MENU_MS.with(|m| m.set(0));
                true
            } else {
                false
            }
        } else if result.clicks.contains_key(render::KEY_TAP) {
            MENU_MS.with(|m| m.set(MENU_AUTO_DISMISS_MS));
            true
        } else {
            false
        };

        // Re-render on a change; else hold one frame at the next deadline.
        let next = [MENU_MS.with(Cell::get), NOTICE_LEFT_MS.with(Cell::get)]
            .into_iter()
            .filter(|&ms| ms > 0)
            .min();
        if changed {
            request_frame();
        } else if let Some(ms) = next {
            request_frame_after(ms);
        }
    }
}
