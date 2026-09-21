// Copyright (C) 2026
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Victron Solar widget — live PV power and Home-Assistant-reported solar
//! yield (today, this month, this year) for a Victron Energy system.
//!
//! Everything is fetched in one request: Home Assistant's `/api/template`
//! endpoint evaluates a small Jinja template that reads the configured
//! entities and answers with exactly the JSON object this widget wants,
//! rather than pulling the full `/api/states` list.

mod manifest_params;

/// A Home Assistant entity_id is `domain.object_id` — lowercase ascii,
/// digits, underscores, and exactly the dots that separate domain from
/// object. Rejecting anything else keeps a stray quote (or brace) in a
/// param from breaking out of the single-quoted Jinja string literal it is
/// spliced into by [`build_template`].
fn entity_id_safe(id: &str) -> bool {
    !id.is_empty()
        && id.contains('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
}

/// Builds the Jinja template that HA's `/api/template` endpoint evaluates
/// to return exactly the six values this widget needs, as a flat JSON
/// object of strings — one request instead of six. `month` and `year` are
/// each read twice: once for their current-cycle total, once for their
/// `last_period` attribute (the previous, already-completed cycle).
///
/// The opening `{{{{` (not `{{`) is deliberate: the host's credential
/// substitution pass (`bmc_wasm_runtime::runtime::imports::credentials::
/// substitute`) scans every fetch's headers *and body* for `{{ … }}` and
/// refuses the whole request unless each one resolves as a
/// `credential.<slot>.<field>` reference — the same escaping rule as
/// Rust's own format strings (`{{{{` → literal `{{`). Since this body is
/// itself Jinja (HA's template syntax, unrelated to that host mechanism),
/// its opening brace must be escaped so the host's scanner doesn't try to
/// resolve it as a credential and reject the request outright. Only the
/// opening needs escaping — the host only ever looks for `{{`, never `}}`
/// on its own, so the single closing `}}` below is correct as-is.
///
/// `None` when any entity id looks unsafe to splice into a single-quoted
/// Jinja string literal; the caller skips the fetch for that tick rather
/// than sending a malformed or unsafe template.
fn build_template(power: &str, today: &str, month: &str, year: &str) -> Option<String> {
    for id in [power, today, month, year] {
        if !entity_id_safe(id) {
            return None;
        }
    }
    let mut t = String::with_capacity(224);
    t.push_str("{{{{ {'pv_power': states('");
    t.push_str(power);
    t.push_str("'), 'today_kwh': states('");
    t.push_str(today);
    t.push_str("'), 'month_kwh': states('");
    t.push_str(month);
    t.push_str("'), 'last_month_kwh': state_attr('");
    t.push_str(month);
    t.push_str("','last_period'), 'year_kwh': states('");
    t.push_str(year);
    t.push_str("'), 'last_year_kwh': state_attr('");
    t.push_str(year);
    t.push_str("','last_period')} | tojson }}");
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::{build_template, entity_id_safe};

    #[test]
    fn real_entity_ids_pass() {
        assert!(entity_id_safe("sensor.solar_yield_today"));
        assert!(entity_id_safe("sensor.victronsolarcharger_yield_pv224"));
    }

    #[test]
    fn ids_without_a_domain_separator_or_with_stray_characters_are_rejected() {
        assert!(!entity_id_safe(""));
        assert!(!entity_id_safe("no_dot_here"));
        assert!(!entity_id_safe("sensor.foo'; {{ malicious"));
        assert!(!entity_id_safe("sensor.foo bar"));
        assert!(!entity_id_safe("sensor.FooBar"));
    }

    #[test]
    fn template_reuses_the_month_and_year_entity_for_their_last_period_attribute() {
        let t = build_template("sensor.pv", "sensor.today", "sensor.month", "sensor.year")
            .expect("all four ids are safe");
        assert!(t.contains("states('sensor.pv')"));
        assert!(t.contains("states('sensor.today')"));
        assert!(t.contains("states('sensor.month')"));
        assert!(t.contains("state_attr('sensor.month','last_period')"));
        assert!(t.contains("states('sensor.year')"));
        assert!(t.contains("state_attr('sensor.year','last_period')"));
        assert!(t.starts_with("{{{{ "));
        assert!(t.ends_with(" }}"));
    }

    /// The host's credential-substitution pass unescapes `{{{{` to a
    /// literal `{{` (Rust format-string rules) and leaves everything else
    /// alone. Simulate that pass here so a regression that breaks the
    /// escaping — e.g. someone "cleaning up" the doubled brace — fails a
    /// fast host test instead of only surfacing as a live fetch refusal.
    #[test]
    fn template_survives_the_hosts_escape_unescape_round_trip() {
        let t = build_template("sensor.pv", "sensor.today", "sensor.month", "sensor.year")
            .expect("all four ids are safe");
        let unescaped = t.replacen("{{{{", "{{", 1);
        assert!(unescaped.starts_with("{{ "));
        assert!(!unescaped.starts_with("{{{{"));
        assert!(unescaped.contains("states('sensor.pv')"));
        assert!(unescaped.ends_with(" }}"));
    }

    #[test]
    fn an_unsafe_entity_id_anywhere_rejects_the_whole_template() {
        assert!(
            build_template("sensor.pv'); {}", "sensor.today", "sensor.month", "sensor.year")
                .is_none()
        );
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_glue {
    use std::cell::{Cell, RefCell};

    #[expect(
        clippy::wildcard_imports,
        reason = "widget render code uses many SDK exports and macros in one file"
    )]
    use bmc_wasm_sdk::*;
    use bmc_wasm_sdk::types::Availability;

    use super::build_template;
    use super::manifest_params;
    use super::manifest_params::credentials as slots;

    const REFRESH_MS: u32 = 15_000;
    const NOT_AVAILABLE: &str = "--";
    const BG_COLOR: Color = BLACK;
    const HEADER_COLOR: Color = GRAY_60;
    const SUN_COLOR: Color = ORANGE_50;

    const SUN_ICON: Svg = include_svg!("assets/sun.svg");

    #[derive(Clone, Copy, Default)]
    struct SolarData {
        pv_power_w: Availability<f64>,
        today_kwh: Availability<f64>,
        month_kwh: Availability<f64>,
        last_month_kwh: Availability<f64>,
        year_kwh: Availability<f64>,
        last_year_kwh: Availability<f64>,
    }

    thread_local! {
        static DATA: RefCell<SolarData> = RefCell::new(SolarData::default());
        static POLL: Cell<Option<PollHandle>> = const { Cell::new(None) };
    }

    // ---------------------------------------------------------------------
    // Fetching
    // ---------------------------------------------------------------------

    fn ha_bound() -> bool {
        credentials::current().is_bound("ha")
    }

    fn auth_header() -> String {
        fmt!(
            "Authorization: Bearer {}\nContent-Type: application/json",
            slots::ha::TOKEN
        )
    }

    /// Enable the poll only once an account is bound to the `ha` slot;
    /// otherwise it stays dormant and the widget renders its placeholder
    /// state without ever spending a request.
    fn reconcile() {
        if let Some(handle) = POLL.with(Cell::get) {
            handle.set_enabled(ha_bound());
        }
    }

    fn build_request(_handle: PollHandle) -> Option<FetchSpec> {
        let params = manifest_params::Params::current();
        let template = build_template(
            &params.entity_power,
            &params.entity_today,
            &params.entity_month,
            &params.entity_year,
        )?;
        let body = json!({"template": #s(&template)});
        let url = fmt!("{}/api/template", params.ha_url.trim_end_matches('/'));
        Some(FetchSpec::post(url).headers(auth_header()).body(body))
    }

    fn parse_field(doc: &JsonDoc, path: &str) -> Availability<f64> {
        doc.str(path).and_then(|s| s.parse::<f64>().ok()).into()
    }

    /// A fresh value replaces whatever was there; a missing or unparsable
    /// one (HA's `unknown`/`unavailable`, or a bad path) marks that single
    /// field failed without touching a value it already has, and without
    /// touching any of the other five fields in this same reply.
    fn merge_field(field: &mut Availability<f64>, new: Availability<f64>) {
        match new {
            Availability::Available(v) => *field = Availability::Available(v),
            Availability::Unavailable | Availability::Failed => {
                field.mark_failed();
            }
        }
    }

    fn on_reply(handle: PollHandle, response: &FetchResponse) {
        if !response.ok() {
            log_debug!("victron-solar: fetch failed (status {})", response.status);
            DATA.with(|d| {
                let mut d = d.borrow_mut();
                d.pv_power_w.mark_failed();
                d.today_kwh.mark_failed();
                d.month_kwh.mark_failed();
                d.last_month_kwh.mark_failed();
                d.year_kwh.mark_failed();
                d.last_year_kwh.mark_failed();
            });
            request_frame();
            return;
        }
        let json = response.json();
        if !json.is_valid() {
            log_warn!("victron-solar: response was not valid JSON");
            handle.retry();
            request_frame();
            return;
        }
        DATA.with(|d| {
            let mut d = d.borrow_mut();
            merge_field(&mut d.pv_power_w, parse_field(&json, "/pv_power"));
            merge_field(&mut d.today_kwh, parse_field(&json, "/today_kwh"));
            merge_field(&mut d.month_kwh, parse_field(&json, "/month_kwh"));
            merge_field(&mut d.last_month_kwh, parse_field(&json, "/last_month_kwh"));
            merge_field(&mut d.year_kwh, parse_field(&json, "/year_kwh"));
            merge_field(&mut d.last_year_kwh, parse_field(&json, "/last_year_kwh"));
        });
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        let handle = register_poll(
            build_request,
            on_reply,
            PollConfig {
                interval_ms: Some(REFRESH_MS),
                enabled: false,
                ..Default::default()
            },
        );
        POLL.with(|p| p.set(Some(handle)));
        reconcile();
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        if let Some(handle) = POLL.with(Cell::get) {
            handle.invalidate();
        }
        request_frame();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_credentials_update() {
        // A rebind may point at a different HA instance: blank and refetch.
        DATA.with(|d| *d.borrow_mut() = SolarData::default());
        reconcile();
        if let Some(handle) = POLL.with(Cell::get) {
            handle.invalidate();
        }
        request_frame();
    }

    // ---------------------------------------------------------------------
    // Rendering
    // ---------------------------------------------------------------------

    #[derive(Clone, Copy)]
    struct SizeParams {
        header_font: u32,
        icon_px: f32,
        power_font: u32,
        label_font: u32,
        value_font: u32,
        sub_font: u32,
        padding: f32,
        gap: f32,
        show_sub: bool,
    }

    const SMALL: SizeParams = SizeParams {
        header_font: 16,
        icon_px: 18.0,
        power_font: 40,
        label_font: 13,
        value_font: 18,
        sub_font: 11,
        padding: 12.0,
        gap: 6.0,
        show_sub: false,
    };
    const MEDIUM: SizeParams = SizeParams {
        header_font: 18,
        icon_px: 20.0,
        power_font: 56,
        label_font: 14,
        value_font: 22,
        sub_font: 12,
        padding: 14.0,
        gap: 8.0,
        show_sub: true,
    };
    const LARGE: SizeParams = SizeParams {
        header_font: 20,
        icon_px: 22.0,
        power_font: 72,
        label_font: 16,
        value_font: 26,
        sub_font: 13,
        padding: 16.0,
        gap: 10.0,
        show_sub: true,
    };
    const FULL: SizeParams = SizeParams {
        header_font: 24,
        icon_px: 28.0,
        power_font: 96,
        label_font: 20,
        value_font: 34,
        sub_font: 16,
        padding: 24.0,
        gap: 14.0,
        show_sub: true,
    };

    fn size_params(variant: SizeVariant) -> &'static SizeParams {
        match variant {
            SizeVariant::Small => &SMALL,
            SizeVariant::Medium => &MEDIUM,
            SizeVariant::Large => &LARGE,
            SizeVariant::Full => &FULL,
        }
    }

    fn header(font: u32, icon_px: f32) -> Node {
        row(
            props!(gap: 8.0, cross_align: CrossAlign::Center),
            [
                canvas(
                    props!(width: icon_px, height: icon_px),
                    [Draw::svg(0.0, 0.0, icon_px, icon_px, &SUN_ICON, SUN_COLOR)],
                ),
                text(
                    "Solar",
                    style!(size: font, weight: FontWeight::BOLD, color: HEADER_COLOR),
                ),
            ],
        )
    }

    fn power_str(data: &SolarData) -> String {
        match data.pv_power_w {
            Availability::Available(w) => fmt!("{} W", format_number!(w, 0)),
            Availability::Unavailable | Availability::Failed => NOT_AVAILABLE.to_string(),
        }
    }

    fn kwh_str(v: Availability<f64>) -> String {
        match v {
            Availability::Available(v) => fmt!("{} kWh", format_number!(v, 1)),
            Availability::Unavailable | Availability::Failed => NOT_AVAILABLE.to_string(),
        }
    }

    fn stat_row(sp: &SizeParams, label: &str, value: Availability<f64>) -> Node {
        row(
            props!(cross_align: CrossAlign::Center),
            [
                text(label, style!(size: sp.label_font, color: GRAY_50, flex: 1.0)),
                text(
                    kwh_str(value),
                    style!(size: sp.value_font, weight: FontWeight::BOLD, color: WHITE, align: TextAlign::Right),
                ),
            ],
        )
    }

    /// `sub`, when shown, is the previous completed cycle's total — e.g.
    /// last month's yield printed under this month's running total.
    fn stat_block(
        sp: &SizeParams,
        label: &str,
        value: Availability<f64>,
        sub: Option<(&str, Availability<f64>)>,
    ) -> Node {
        let mut children = vec![stat_row(sp, label, value)];
        if sp.show_sub
            && let Some((sub_label, sub_value)) = sub
        {
            children.push(text(
                fmt!("{} {}", sub_label, kwh_str(sub_value)),
                style!(size: sp.sub_font, color: GRAY_50),
            ));
        }
        col(props!(gap: 2.0), children)
    }

    fn stats(sp: &SizeParams, data: &SolarData) -> Vec<Node> {
        vec![
            stat_block(sp, "Today", data.today_kwh, None),
            stat_block(
                sp,
                "This month",
                data.month_kwh,
                Some(("Last month:", data.last_month_kwh)),
            ),
            stat_block(
                sp,
                "This year",
                data.year_kwh,
                Some(("Last year:", data.last_year_kwh)),
            ),
        ]
    }

    fn view_rect(sp: &SizeParams, data: &SolarData) -> Node {
        col(
            props!(background: BG_COLOR, padding: sp.padding, gap: sp.gap),
            [
                header(sp.header_font, sp.icon_px),
                text(
                    power_str(data),
                    style!(size: sp.power_font, weight: FontWeight::BOLD, color: WHITE, family: FontFamily::DeckSans, line_height: 0.9),
                ),
                col(props!(gap: sp.gap, flex: 1.0, justify_content: Justify::End), stats(sp, data)),
            ],
        )
    }

    /// Round layout (BFM100): everything centered and scaled off the
    /// viewport's diameter, since the rectangular layout's edge-pinned
    /// header would spill past the circular cutout.
    fn view_round(ws: WidgetSize, data: &SolarData) -> Node {
        let scale = ws.round_scale();
        let sp = SizeParams {
            header_font: scale_font(20, scale),
            icon_px: 22.0 * scale,
            power_font: scale_font(64, scale),
            label_font: scale_font(15, scale),
            value_font: scale_font(22, scale),
            sub_font: scale_font(13, scale),
            padding: 48.0 * scale,
            gap: 8.0 * scale,
            show_sub: true,
        };
        let mut children = vec![
            header(sp.header_font, sp.icon_px),
            text(
                power_str(data),
                style!(size: sp.power_font, weight: FontWeight::BOLD, color: WHITE, family: FontFamily::DeckSans, line_height: 0.9),
            ),
        ];
        children.extend(stats(&sp, data));
        col(
            props!(background: BG_COLOR, padding: sp.padding, gap: sp.gap, cross_align: CrossAlign::Center),
            children,
        )
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn render(_delta_ms: u32) {
        let ws = widget_size();
        let shape = widget_viewport().shape;
        let data = DATA.with(|d| *d.borrow());
        let root = if matches!(shape, ViewportShape::Round) {
            view_round(ws, &data)
        } else {
            view_rect(size_params(ws.variant), &data)
        };
        let _ = render_ui(ws.width, ws.height, root);
    }
}
