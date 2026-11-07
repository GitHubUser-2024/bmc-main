// AUTO-GENERATED FROM ../manifest.json by `bmc-widget-codegen` v0.1.0.
// Do not edit by hand. Run `just wasm::gen <widget>` after changing the manifest.

#![allow(
    dead_code,
    reason = "fields are widget-specific; not every key is used by every render path"
)]

use bmc_wasm_sdk::params as snapshot;
use bmc_wasm_sdk::params::typed::ParamRead;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxBitrate {
    _128,
    _192,
    _320,
}
impl MaxBitrate {
    /// Every variant, in manifest-declaration order. Useful when a widget
    /// wants to render a "pick one" UI or audit the enum exhaustively.
    pub const ALL: &'static [Self] = &[Self::_128, Self::_192, Self::_320];
    /// Manifest wire value for this variant.
    #[must_use]
    pub fn as_manifest_value(self) -> i32 {
        match self {
            Self::_128 => 128,
            Self::_192 => 192,
            Self::_320 => 320,
        }
    }
    /// Human-readable label declared in the manifest's `enum_values`.
    #[must_use]
    pub fn as_manifest_label(self) -> &'static str {
        match self {
            Self::_128 => "128",
            Self::_192 => "192",
            Self::_320 => "320",
        }
    }
    #[must_use]
    pub fn from_manifest_value(v: i32) -> Option<Self> {
        match v {
            128 => Some(Self::_128),
            192 => Some(Self::_192),
            320 => Some(Self::_320),
            _ => None,
        }
    }
}
bmc_wasm_sdk::impl_manifest_i32_enum!(MaxBitrate);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubsonicSource {
    Random,
    Starred,
    Playlist,
}
impl SubsonicSource {
    /// Every variant, in manifest-declaration order. Useful when a widget
    /// wants to render a "pick one" UI or audit the enum exhaustively.
    pub const ALL: &'static [Self] = &[Self::Random, Self::Starred, Self::Playlist];
    /// Manifest wire value for this variant.
    #[must_use]
    pub fn as_manifest_value(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Starred => "starred",
            Self::Playlist => "playlist",
        }
    }
    /// Human-readable label declared in the manifest's `enum_values`.
    #[must_use]
    pub fn as_manifest_label(self) -> &'static str {
        match self {
            Self::Random => "Random songs",
            Self::Starred => "Favourites",
            Self::Playlist => "Playlist",
        }
    }
    #[must_use]
    pub fn from_manifest_value(s: &str) -> Option<Self> {
        match s {
            "random" => Some(Self::Random),
            "starred" => Some(Self::Starred),
            "playlist" => Some(Self::Playlist),
            _ => None,
        }
    }
}
bmc_wasm_sdk::impl_manifest_str_enum!(SubsonicSource);
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub max_bitrate: MaxBitrate,
    pub subsonic_password: Option<String>,
    pub subsonic_playlist: Option<String>,
    pub subsonic_source: SubsonicSource,
    pub subsonic_url: Option<String>,
    pub subsonic_user: Option<String>,
}
impl Params {
    /// Materialise a typed snapshot from a dynamic [`snapshot::Params`].
    #[must_use]
    pub fn from_snapshot(snap: &snapshot::Params) -> Self {
        Self {
            max_bitrate: <MaxBitrate as ParamRead>::read_required(snap, "max_bitrate"),
            subsonic_password: <String as ParamRead>::read_optional(snap, "subsonic_password"),
            subsonic_playlist: <String as ParamRead>::read_optional(snap, "subsonic_playlist"),
            subsonic_source: <SubsonicSource as ParamRead>::read_required(snap, "subsonic_source"),
            subsonic_url: <String as ParamRead>::read_optional(snap, "subsonic_url"),
            subsonic_user: <String as ParamRead>::read_optional(snap, "subsonic_user"),
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
        if self.max_bitrate != other.max_bitrate {
            out.push("max_bitrate");
        }
        if self.subsonic_password != other.subsonic_password {
            out.push("subsonic_password");
        }
        if self.subsonic_playlist != other.subsonic_playlist {
            out.push("subsonic_playlist");
        }
        if self.subsonic_source != other.subsonic_source {
            out.push("subsonic_source");
        }
        if self.subsonic_url != other.subsonic_url {
            out.push("subsonic_url");
        }
        if self.subsonic_user != other.subsonic_user {
            out.push("subsonic_user");
        }
        out
    }
}
