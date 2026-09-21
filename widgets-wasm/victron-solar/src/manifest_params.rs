// AUTO-GENERATED FROM ../manifest.json by `bmc-widget-codegen` v0.1.0.
// Do not edit by hand. Run `just wasm::gen <widget>` after changing the manifest.

#![allow(
    dead_code,
    reason = "fields are widget-specific; not every key is used by every render path"
)]

use bmc_wasm_sdk::params as snapshot;
use bmc_wasm_sdk::params::typed::ParamRead;
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub entity_month: String,
    pub entity_power: String,
    pub entity_today: String,
    pub entity_year: String,
    pub ha_url: String,
}
impl Params {
    /// Materialise a typed snapshot from a dynamic [`snapshot::Params`].
    #[must_use]
    pub fn from_snapshot(snap: &snapshot::Params) -> Self {
        Self {
            entity_month: <String as ParamRead>::read_required(snap, "entity_month"),
            entity_power: <String as ParamRead>::read_required(snap, "entity_power"),
            entity_today: <String as ParamRead>::read_required(snap, "entity_today"),
            entity_year: <String as ParamRead>::read_required(snap, "entity_year"),
            ha_url: <String as ParamRead>::read_required(snap, "ha_url"),
        }
    }
    /// Latest typed snapshot delivered for this widget instance.
    /// Cached per-thread; only re-parses when `snapshot::version()` changes
    /// since the last call.
    #[must_use]
    pub fn current() -> Self {
        thread_local! {
            static CACHE : core::cell::RefCell < Option < (u64, Params) >> = const {
            core::cell::RefCell::new(None) };
        }
        let v = snapshot::version();
        CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            if let Some((cv, ref params)) = *cache
                && cv == v
            {
                return params.clone();
            }
            let fresh = Self::from_snapshot(&snapshot::current());
            *cache = Some((v, fresh.clone()));
            fresh
        })
    }
    /// Snapshot delivered immediately before [`current`]; `None` until at
    /// least one update has been observed (i.e. during `init` and the
    /// first `render`).
    #[must_use]
    pub fn previous() -> Option<Self> {
        let prev = snapshot::previous();
        if prev.is_empty() {
            None
        } else {
            Some(Self::from_snapshot(&prev))
        }
    }
    /// Manifest keys whose value differs between `self` and `other`.
    ///
    /// Intended for `on_params_update` diffing — pass `current()` and the
    /// inside-hook value of `previous()` to get the set of keys to react
    /// to. Field-by-field `PartialEq`; emitted in struct-field order so
    /// the result is deterministic.
    #[must_use]
    pub fn changed_keys(&self, other: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.entity_month != other.entity_month {
            out.push("entity_month");
        }
        if self.entity_power != other.entity_power {
            out.push("entity_power");
        }
        if self.entity_today != other.entity_today {
            out.push("entity_today");
        }
        if self.entity_year != other.entity_year {
            out.push("entity_year");
        }
        if self.ha_url != other.ha_url {
            out.push("ha_url");
        }
        out
    }
}
/// Credential slots this widget declares, one module per slot.
pub mod credentials {
    ///Home Assistant — a `generic-token` account. Required — the widget cannot work until an account is bound.
    ///
    ///Long-lived access token for your Home Assistant instance
    pub mod ha {
        ///Placeholder for this slot's `token` field.
        pub const TOKEN: &str = "{{ credential.ha.token }}";
    }
}
