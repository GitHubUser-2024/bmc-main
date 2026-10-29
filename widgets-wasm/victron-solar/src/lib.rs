// Copyright (C) 2026
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Victron Solar widget — live PV power and solar yield (today, this month,
//! this year) read straight from a Victron GX device.
//!
//! Every GX (Cerbo, Ekrano, Venus GX, Venus OS on a Pi) runs an MQTT broker;
//! the widget holds one TLS connection to it (the GX's certificate is
//! self-signed), finds the portal id, subscribes to the solar topics and
//! sends the keepalive that makes Venus OS publish them. See [`venus`] for
//! the topics, [`totals`] for how month/year are counted.

mod manifest_params;
mod mqtt;
mod totals;
mod venus;

#[cfg(target_arch = "wasm32")]
mod wasm_glue {
    use std::cell::RefCell;

    use bmc_wasm_sdk::socket::{Socket, SocketEvent, tls_connect_insecure};
    use bmc_wasm_sdk::timer::{self, Timer};
    use bmc_wasm_sdk::types::Availability;
    #[expect(
        clippy::wildcard_imports,
        reason = "widget render code uses many SDK exports and macros in one file"
    )]
    use bmc_wasm_sdk::*;

    use super::manifest_params;
    use super::mqtt::{self, Decoder, Incoming};
    use super::totals::{Date, Ledger, Totals};
    use super::venus::{self, Readings};

    /// Ping + keepalive cadence. Venus OS stops publishing ~60 s after the
    /// last keepalive, and the MQTT session itself expires after
    /// [`mqtt::KEEPALIVE_SECS`].
    const TICK_MS: u32 = 25_000;
    /// No packet at all for this long: the connection is dead, reconnect.
    const STALE_SECS: i64 = 90;
    /// Sent on every keepalive after the first: refreshes the subscription
    /// without making the GX republish every value (older Venus OS ignores
    /// the option and republishes, which is merely heavier).
    const KEEPALIVE_QUIET: &[u8] = br#"{"keepalive-options": ["suppress-republish"]}"#;

    const NOT_AVAILABLE: &str = "--";
    const BG_COLOR: Color = BLACK;
    const HEADER_COLOR: Color = GRAY_60;
    const SUN_COLOR: Color = ORANGE_50;

    const SUN_ICON: Svg = include_svg!("assets/sun.svg");

    #[derive(Clone, Copy, PartialEq)]
    enum Status {
        Connecting,
        Live,
        Unreachable,
        Refused(u8),
    }

    #[derive(Clone, Copy)]
    struct SolarData {
        status: Status,
        pv_power_w: Availability<f64>,
        today_kwh: Availability<f64>,
        month_kwh: Availability<f64>,
        month_since: Option<Date>,
        last_month_kwh: Availability<f64>,
        year_kwh: Availability<f64>,
        year_since: Option<Date>,
        last_year_kwh: Availability<f64>,
    }

    impl Default for SolarData {
        fn default() -> Self {
            Self {
                status: Status::Connecting,
                pv_power_w: Availability::default(),
                today_kwh: Availability::default(),
                month_kwh: Availability::default(),
                month_since: None,
                last_month_kwh: Availability::default(),
                year_kwh: Availability::default(),
                year_since: None,
                last_year_kwh: Availability::default(),
            }
        }
    }

    #[derive(Default)]
    struct Session {
        decoder: Decoder,
        portal: Option<String>,
        readings: Readings,
        ledger: Option<Ledger>,
        last_rx_secs: i64,
        status: Option<Status>,
    }

    thread_local! {
        static DATA: RefCell<SolarData> = RefCell::new(SolarData::default());
        static SESSION: RefCell<Session> = RefCell::new(Session::default());
        static SOCKET: RefCell<Option<Socket>> = const { RefCell::new(None) };
        static TICK: RefCell<Option<Timer>> = const { RefCell::new(None) };
    }

    // ---------------------------------------------------------------------
    // GX connection
    // ---------------------------------------------------------------------

    fn now_secs() -> i64 {
        SystemTime::now().unix_secs
    }

    /// Local date and hour, in the Deck's configured timezone.
    fn local_now() -> (Date, u32) {
        let now = SystemTime::now();
        let local = SystemTime {
            unix_secs: format::local_unix_secs_or_system(&now, None),
        }
        .utc();
        (
            Date::new(
                i32::from(local.year),
                u32::from(local.month),
                u32::from(local.day),
            ),
            u32::from(local.hour),
        )
    }

    fn ledger_key(portal: &str) -> String {
        fmt!("ledger-{}", portal)
    }

    fn connect() {
        let params = manifest_params::Params::current();
        let host = params.gx_host.trim().to_string();
        let port = u16::try_from(params.gx_port).unwrap_or(8883);
        SESSION.with_borrow_mut(|s| {
            let ledger = s.ledger.take();
            *s = Session {
                last_rx_secs: now_secs(),
                ..Session::default()
            };
            s.ledger = ledger;
        });
        set_status(Status::Connecting);
        if host.is_empty() {
            SOCKET.set(None);
            set_status(Status::Unreachable);
            return;
        }
        SOCKET.set(tls_connect_insecure(&host, port, on_socket_event));
        if SOCKET.with_borrow(Option::is_none) {
            set_status(Status::Unreachable);
        }
    }

    fn set_status(status: Status) {
        DATA.with_borrow_mut(|d| d.status = status);
        request_frame();
    }

    fn on_socket_event(socket: &Socket, event: &SocketEvent<'_>) {
        match event {
            SocketEvent::Connected => {
                socket.write(&mqtt::connect(&fmt!("bmc-victron-solar-{}", now_secs())));
            }
            SocketEvent::Data(bytes) => on_data(socket, bytes),
            SocketEvent::Closed(code) => {
                log_info!("victron-solar: GX connection closed ({})", code);
                SOCKET.set(None);
                // A refusal is the more useful thing to show than the close that follows it.
                if !matches!(DATA.with_borrow(|d| d.status), Status::Refused(_)) {
                    set_status(Status::Unreachable);
                }
            }
        }
    }

    fn on_data(socket: &Socket, bytes: &[u8]) {
        let mut changed = false;
        SESSION.with_borrow_mut(|s| {
            s.last_rx_secs = now_secs();
            s.decoder.push(bytes);
            while let Some(packet) = s.decoder.next_packet() {
                match packet {
                    Incoming::ConnAck(0) => {
                        socket.write(&mqtt::subscribe(1, &[venus::DISCOVERY_FILTER]));
                    }
                    Incoming::ConnAck(code) => {
                        log_warn!(
                            "victron-solar: GX refused the MQTT connection (code {})",
                            code
                        );
                        s.status = Some(Status::Refused(code));
                    }
                    Incoming::Publish { topic, payload } => match s.portal.clone() {
                        Some(portal) => changed |= s.readings.apply(&portal, &topic, &payload),
                        None => {
                            if let Some(portal) = venus::portal_from_serial_topic(&topic) {
                                let filters = venus::subscriptions(portal);
                                let filters: Vec<&str> =
                                    filters.iter().map(String::as_str).collect();
                                socket.write(&mqtt::subscribe(2, &filters));
                                socket.write(&mqtt::publish(&fmt!("R/{}/keepalive", portal), b""));
                                s.ledger = kv::get_string(&ledger_key(portal))
                                    .and_then(|t| Ledger::from_text(&t));
                                s.portal = Some(portal.to_string());
                                s.status = Some(Status::Live);
                            }
                        }
                    },
                    Incoming::Other => {}
                }
            }
        });
        if let Some(status) = SESSION.with_borrow_mut(|s| s.status.take()) {
            set_status(status);
        }
        if changed {
            recompute();
        }
    }

    /// Re-derive what's on screen from the readings, rolling the ledger
    /// over (and persisting it) when the day changed.
    fn recompute() {
        let (today, hour) = local_now();
        let (power, totals) = SESSION.with_borrow_mut(|s| {
            let power = s.readings.pv_power_w();
            let lifetime = s.readings.lifetime_kwh();
            let totals = match (s.readings.complete, lifetime, s.portal.as_deref()) {
                (true, Some(lifetime), Some(portal)) => {
                    let daily = s.readings.daily_history_kwh();
                    let changed = if let Some(ledger) = s.ledger.as_mut() {
                        ledger.observe(today, hour, lifetime, &daily)
                    } else {
                        s.ledger = Some(Ledger::seed(today, lifetime, &daily));
                        true
                    };
                    let ledger = s.ledger.as_ref().expect("set above");
                    if changed {
                        kv::set(&ledger_key(portal), ledger.to_text().as_bytes());
                    }
                    Some(ledger.totals(lifetime))
                }
                _ => None,
            };
            (power, totals)
        });
        DATA.with_borrow_mut(|d| {
            d.pv_power_w = power.into();
            apply_totals(d, totals);
        });
        request_frame();
    }

    fn apply_totals(d: &mut SolarData, totals: Option<Totals>) {
        let Some(t) = totals else {
            return;
        };
        d.today_kwh = Availability::Available(t.today);
        d.month_kwh = Availability::Available(t.month);
        d.month_since = t.month_since;
        d.last_month_kwh = t.last_month.into();
        d.year_kwh = Availability::Available(t.year);
        d.year_since = t.year_since;
        d.last_year_kwh = t.last_year.into();
    }

    fn on_tick(_timer: &Timer, _periods: u32) {
        let stale = SESSION.with_borrow(|s| now_secs() - s.last_rx_secs > STALE_SECS);
        if SOCKET.with_borrow(Option::is_none) || stale {
            if stale {
                log_info!("victron-solar: no data from the GX, reconnecting");
            }
            connect();
            return;
        }
        let portal = SESSION.with_borrow(|s| s.portal.clone());
        SOCKET.with_borrow(|socket| {
            if let Some(socket) = socket {
                socket.write(&mqtt::pingreq());
                if let Some(portal) = portal {
                    socket.write(&mqtt::publish(
                        &fmt!("R/{}/keepalive", portal),
                        KEEPALIVE_QUIET,
                    ));
                }
            }
        });
        // Rolls the day over even when the sun is down and nothing changes.
        recompute();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn init() {
        TICK.set(timer::start(TICK_MS, on_tick));
        connect();
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn on_params_update() {
        // A different GX: its counters have nothing to do with the old one's.
        DATA.set(SolarData::default());
        SESSION.with_borrow_mut(|s| s.ledger = None);
        connect();
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
                text(
                    label,
                    style!(size: sp.label_font, color: GRAY_50, flex: 1.0),
                ),
                text(
                    kwh_str(value),
                    style!(size: sp.value_font, weight: FontWeight::BOLD, color: WHITE, align: TextAlign::Right),
                ),
            ],
        )
    }

    /// `sub`, when shown, is the line under a running total: the previous
    /// completed cycle's total (e.g. last month's yield), or, while the
    /// running total doesn't cover the whole cycle yet, where it starts.
    fn stat_block(
        sp: &SizeParams,
        label: &str,
        value: Availability<f64>,
        sub: Option<String>,
    ) -> Node {
        let mut children = vec![stat_row(sp, label, value)];
        if sp.show_sub
            && let Some(sub) = sub
        {
            children.push(text(sub, style!(size: sp.sub_font, color: GRAY_50)));
        }
        col(props!(gap: 2.0), children)
    }

    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    fn sub_line(since: Option<Date>, last_label: &str, last: Availability<f64>) -> String {
        since.map_or_else(
            || fmt!("{} {}", last_label, kwh_str(last)),
            |d| {
                fmt!(
                    "Counting since {} {}",
                    d.d,
                    MONTHS[(d.m as usize).clamp(1, 12) - 1]
                )
            },
        )
    }

    fn stats(sp: &SizeParams, data: &SolarData) -> Vec<Node> {
        vec![
            stat_block(sp, "Today", data.today_kwh, None),
            stat_block(
                sp,
                "This month",
                data.month_kwh,
                Some(sub_line(
                    data.month_since,
                    "Last month:",
                    data.last_month_kwh,
                )),
            ),
            stat_block(
                sp,
                "This year",
                data.year_kwh,
                Some(sub_line(data.year_since, "Last year:", data.last_year_kwh)),
            ),
        ]
    }

    /// Shown under the header until live data flows.
    fn status_line(data: &SolarData) -> Option<String> {
        match data.status {
            Status::Live => None,
            Status::Connecting => Some("Connecting to GX…".to_string()),
            Status::Unreachable => {
                Some("GX not reachable — check address and MQTT on LAN".to_string())
            }
            Status::Refused(code) => Some(fmt!("GX refused MQTT (code {}) — password set?", code)),
        }
    }

    fn status_node(sp: &SizeParams, data: &SolarData) -> Option<Node> {
        status_line(data).map(|s| text(s, style!(size: sp.sub_font, color: ORANGE_50)))
    }

    fn view_rect(sp: &SizeParams, data: &SolarData) -> Node {
        let mut top = vec![header(sp.header_font, sp.icon_px)];
        top.extend(status_node(sp, data));
        col(
            props!(background: BG_COLOR, padding: sp.padding, gap: sp.gap),
            [
                col(props!(gap: 2.0), top),
                text(
                    power_str(data),
                    style!(size: sp.power_font, weight: FontWeight::BOLD, color: WHITE, family: FontFamily::DeckSans, line_height: 0.9),
                ),
                col(
                    props!(gap: sp.gap, flex: 1.0, justify_content: Justify::End),
                    stats(sp, data),
                ),
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
        let mut children = vec![header(sp.header_font, sp.icon_px)];
        children.extend(status_node(&sp, data));
        children.extend([
            text(
                power_str(data),
                style!(size: sp.power_font, weight: FontWeight::BOLD, color: WHITE, family: FontFamily::DeckSans, line_height: 0.9),
            ),
        ]);
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
