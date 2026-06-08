# mmce — MangaMeeya, reborn in Rust

A cross-platform manga / image viewer modelled on the old Japanese freeware
**MangaMeeya CE** (2005–2007). That binary is Win32-only and has no source
code; this repo rebuilds its behaviour in Rust on top of [eframe/egui][egui].

The original's `.exe`, DLLs, and a real `MangaMeeyaCE.ini` (useful as a
ground-truth reference) live under [`legacy/`](legacy/).

## What's in the box

- Loose-image folders, `.zip` / `.cbz`, `.7z` / `.cb7` archives
- 1-page and 2-page spread modes
- Right-to-left (manga) or left-to-right binding
- Natural sort (`page 2 < page 10`)
- Fit / fit-width / fit-height / 100% / custom zoom, with *no-zoom-in* cap
- Background prefetch + LRU page cache
- Keyboard bindings matching the readme:
  - `←` / `→` — previous / next spread
  - `Shift+←` / `Shift+→` — step a single page
  - `/` / `?` — skip: pin the prior page, slide only the later page of a 2-up spread (`?` reverses)
  - `Home` / `End` — first / last page
  - `Space` — toggle single / spread
  - `+` / `-` — zoom in / out
  - `0` — reset fit
  - `F11` or `Alt+Enter` — fullscreen
  - `Ctrl+O` — open folder or archive
  - `Esc` — leave fullscreen
- Mouse: left-click next, right-click previous, middle-drag pan,
  `Ctrl+Wheel` zoom
- Loads and saves UTF-16 LE `.ini` files; unknown sections are preserved
  round-trip so existing MangaMeeya configs don't get mangled.

## What we explicitly dropped from the original

- Furigana / ruby text overlay (`[Text]` and `Ruby*` keys)
- Per-archive resume state
- RAR archives
- PDF (legacy `pdf.dll`)
- Tool buttons / bitmap toolbars
- Folder tree / file list / thumbnail side panels (the book view is the app)

These can be added later — none of them are on the fast path for reading.

## Build

### NixOS (recommended — what this repo targets)

```sh
direnv allow     # picks up flake.nix automatically, or:
nix develop

cargo build --release
./target/release/mmce /path/to/book.cbz
```

All runtime libraries (OpenGL, Wayland, X11, fontconfig, bzip2) are wired
into the dev shell by the flake.

### Non-Nix Linux / macOS / Windows

Needs Rust 1.82+. On Linux you'll also need the distro packages for
`libxkbcommon`, `libGL`, `wayland`, `libX11`, `libxcursor`, `libxi`,
`libxrandr`, `fontconfig`, and `bzip2`.

```sh
cargo build --release
```

## Layout

```
crates/
  mmce-config/   INI parse / save + typed Settings
  mmce-codecs/   Image decoders + folder/zip/7z page sources
  mmce-core/     Book, Spread pairing, ViewerState
  mmce-render/   LRU page cache + background decoder + fit math
  mmce-app/      eframe frontend, keybindings, CLI
```

The split is a trust boundary: `mmce-core` and below have no `egui`
dependency, so everything above the renderer can be unit-tested without a
GPU context.

## Tests

```sh
cargo test --workspace
```

20 tests covering INI round-trips, natural sort, spread pairing (including
RTL flip), fit-math corner cases, and folder + ZIP integration.

### Headless smoke

`scripts/headless-smoke.sh` builds release, generates an 8-page PNG
fixture, then launches `mmce` under `xvfb-run` for 3 seconds — it passes as
long as `mmce` doesn't crash before the timeout kills it.

## Compatibility notes

- `MangaMeeyaCE.ini` is UTF-16 LE with a BOM. We read it, load the subset
  of keys we honour, preserve all other sections verbatim, and write back
  UTF-16 LE on save.
- `ViewMode.Sort=11` in the legacy file means natural name sort; we honour
  that as the default.
- `ScaleMode.Mode` maps: `0 → Original`, `1|2 → Fit`, `3 → FitWidth`,
  `4 → FitHeight`, everything else → Fit.
- `BindDir=1` (right-to-left) is the default because manga.

## License

MIT. The legacy binaries bundled under `legacy/` are the original anonymous
author's freeware and are not covered by this license — they are included
only as reference material for reverse-engineering the behaviour this
port targets.

[egui]: https://github.com/emilk/egui
