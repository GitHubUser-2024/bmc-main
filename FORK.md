# About this fork

**Co-authored by Claude (Anthropic) and Jan Novák.**

This is an unofficial fork of [BraiinsForge/bmc-main](https://github.com/BraiinsForge/bmc-main),
the display application, widgets and web UI of the Braiins Deck. It is not affiliated with
Braiins. It collects the changes made for one Deck at home: first the places where it
deviates from upstream behaviour, then the new features. Everything is published as is,
under upstream's licence ([GPL-3.0](COPYING)), for anyone who finds it useful.

## How the branches work

- **`master`** is an untouched copy of upstream `master`.
- **`fork`** (the default branch) is upstream `master` plus this file and the landing page,
  nothing else.
- **Every change lives on its own branch**, as a few commits on top of upstream. Nothing is
  merged together, so each one can be read, built or cherry-picked alone.
- **`<name>`** branches sit on upstream `master`. **`stable/<name>`** branches carry the same
  change on the newest upstream `bmcpkgs/stable-*` branch (now `bmcpkgs/stable-26.10`), the
  line released firmware is built from. They are what actually runs on the Deck.
- **Branches are synced nightly.** The changes are developed in a private Git server. Every
  night all branches are rebased onto the latest upstream and pushed here. History is
  rewritten whenever upstream moves, so fetch again rather than pull.
- **Some branches are behind draft pull requests** in the upstream repo, and those keep a
  single squashed commit. The private working branches keep their full history.

To run several changes on a Deck, merge the `stable/*` branches you want onto the same
upstream stable branch and deploy as upstream describes
(`nix run .#deck -- deploy --device <address>`).

## 1. Deviations from upstream

These change how existing parts behave.

### Mesa: etnaviv GPU fixes for older kernels

Branches: [`deck-fd-fix`](https://github.com/GitHubUser-2024/bmc-main/tree/deck-fd-fix),
[`stable/deck-fd-fix`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/deck-fd-fix)

- **Stale fence fd** (stable only): `etna_cmd_stream_flush()` handed back fence fd 0 when a
  GPU submit failed. Waiting on it polled an unrelated fd forever while holding the GPU
  render lock, which froze the screen. This is a backport of Mesa 398d84e3 (in 26.3). Upstream
  `master` already uses a Mesa that has it, so on `deck-fd-fix` the backport is added and
  reverted again.
- **No softpin before Linux 5.19:** on older kernels a GPU address that Mesa reuses can
  still be mapped by another process. Submits then fail with ENOSPC ("submit failed: -28")
  and frames are dropped. On such kernels the kernel places buffers itself. This matters on
  Deck firmware 26.02 and older (kernel 5.10); Braiins carry a kernel-side fix from 26.09.
- **Build fix** (stable only): `egui_autocomplete` 13.0.0 is taken from crates.io, because the
  pinned git revision of the fork it came from no longer exists.

### Runtime: extra CA certificates

Branches: [`deck-extra-ca`](https://github.com/GitHubUser-2024/bmc-main/tree/deck-extra-ca) (squashed, behind
[an upstream draft PR](https://github.com/BraiinsForge/bmc-main/pull/6)),
[`stable/deck-extra-ca`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/deck-extra-ca)

Widget fetches and TLS sockets trust the PEM certificates in `/etc/bmc/extra-ca.pem` as
well as the usual WebPKI roots. A widget can then reach a home server whose certificate
comes from a private CA, such as Home Assistant, Immich, Nextcloud or an MQTT broker.
Without the file nothing changes.

### Runtime: DELETE requests keep their body

Branch: [`stable/deck-delete-body`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/deck-delete-body). On `master` this
fix is part of `immich-photo`.

A widget's DELETE fetch dropped its body, because only POST, PUT and PATCH sent one. Some
APIs name what to delete in the body: Immich answered 400 when a tag was removed.

### Scene cycling: a widget can keep its scene on screen

Branches: [`deck-music`](https://github.com/GitHubUser-2024/bmc-main/tree/deck-music),
[`stable/deck-music`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/deck-music)
(the first and third commits on top of Deck Music)

- **Manifest flag:** a widget manifest can set `"holds_screen": true`. Automatic scene
  cycling skips a scene with such a widget; it is reached only by swiping to it.
- **Stays put once there:** while that scene is showing, automatic cycling pauses. Swiping
  to another scene resumes it.
- **Night mode and settings untouched:** the hold is a separate gate next to the night-mode
  suspension and never changes the cycling settings. Firmware without this change ignores
  the flag, so a manifest that sets it still loads.

### Financial Ticker: price precision

Branches: [`ticker-single-precision`](https://github.com/GitHubUser-2024/bmc-main/tree/ticker-single-precision),
[`stable/ticker-single-precision`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/ticker-single-precision)

An optional Precision setting that fixes the number of decimals shown. This is **Václav
Dvořák's work** from [his upstream pull request](https://github.com/BraiinsForge/bmc-main/pull/2),
carried here unchanged so it can be combined with the changes below.

### Financial Ticker: price-only view

Branches: [`ticker-single-price-only`](https://github.com/GitHubUser-2024/bmc-main/tree/ticker-single-price-only) (squashed, behind
[an upstream draft PR](https://github.com/BraiinsForge/bmc-main/pull/7)),
[`stable/ticker-single-price-only`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/ticker-single-price-only)

A third View option, **Price only**. It shows the header and the large price where the
sparkline view puts them, with no chart, for tiles where only the current price matters.

### Financial Ticker List: price precision

Branches: [`ticker-list-precision`](https://github.com/GitHubUser-2024/bmc-main/tree/ticker-list-precision) (squashed, behind
[an upstream draft PR](https://github.com/BraiinsForge/bmc-main/pull/8)),
[`stable/ticker-list-precision`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/ticker-list-precision)

- **`master`:** an optional Precision field (0–6) on each Symbols row, so a row like
  EUR / CZK can show 24.31 while the other rows keep the automatic decimals. The precision
  rule lives in the shared prices module, so every price widget formats it the same way.
- **stable:** one Precision setting for the whole list (0–8), because the stable manifest
  still has eight `symbol_N` slots rather than row objects.

## 2. New features

### Victron Solar widget

Branches: [`victron-solar`](https://github.com/GitHubUser-2024/bmc-main/tree/victron-solar) (squashed, behind
[an upstream draft PR](https://github.com/BraiinsForge/bmc-main/pull/4)),
[`stable/victron-solar`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/victron-solar)

Live PV power and solar yield (today, month, year) from a Victron system running Venus OS.
It reads the GX device directly over MQTT with TLS, plain MQTT or Modbus TCP on the local
network, so no Home Assistant or cloud is needed.
The power figure fits itself to the tile with canvas autofit. Yield totals are kept in the
cache bucket, because kv storage on the Deck only lives in RAM.

### Immich Photo widget

Branches: [`immich-photo`](https://github.com/GitHubUser-2024/bmc-main/tree/immich-photo) (squashed, behind
[an upstream draft PR](https://github.com/BraiinsForge/bmc-main/pull/5)),
[`stable/image-label`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/image-label)

- **Photos:** shows a random photo from an Immich server, fitted to the widget, and a new
  one on each refresh. It only picks timeline photos, so videos, archived and hidden photos
  are skipped.
- **Labels:** **Label it** in the tap menu tags the photo on screen in Immich. On a photo
  that already has the label, the button reads **Remove label**.
- **API key:** it sits in a credential slot, so the Deck adds it to the requests and the
  widget never sees it.

### Deck Music widget and runtime media player

Branches: [`deck-music`](https://github.com/GitHubUser-2024/bmc-main/tree/deck-music),
[`stable/deck-music`](https://github.com/GitHubUser-2024/bmc-main/tree/stable/deck-music)

- **Runtime player:** a process-wide streaming player behind new `host_media_*` imports,
  with `bmc_wasm_sdk::media` (queue builder, commands, typed status) on the SDK side. A
  widget hands it a queue of MP3 URLs with metadata per track, then sends play, pause,
  next, previous, seek, volume, shuffle and repeat.
- **Playback:** each track streams through `madplay` into `aplay`. The player moves through
  the queue by itself, so music keeps playing while another widget is on screen. The PCM
  passes through the player, which gives an exact position, an instant volume change and a
  pause that only stops writing. After a minute of pause the sound device is given back.
- **Deck Music widget:** fills the queue from a Subsonic server (Navidrome, Gonic, …) with
  random songs, favourites or a playlist. It shows the cover, title, artist and album, a
  seek bar, one symmetric row of round buttons (shuffle, previous, play/pause, next,
  repeat) and a volume row.
- **Out of the cycle:** the widget sets `holds_screen`, so automatic cycling skips its scene
  and stays on it once you swipe there (see "Scene cycling" above).
- **Cover in step with the song:** the old picture is dropped when the track changes, and
  only the answer to the request still wanted is shown. A late cover from a skipped song
  never appears.
