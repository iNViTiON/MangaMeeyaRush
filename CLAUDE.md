# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**mmce** — a cross-platform Rust port of the Japanese freeware manga reader **MangaMeeya CE** (Win32, 2005-2007, no source). The legacy binaries + a reference `MangaMeeyaCE.ini` live under `legacy/` for behavior lookup; this repo rebuilds the viewer on `eframe`/`egui` with a Cargo workspace of focused crates.

## Common commands

Dev shell (NixOS — preferred; wires up GL, Wayland, X11, fontconfig, bzip2, and
`nasm` — required to assemble libjpeg-turbo's SIMD for the `mozjpeg` dep. Building
outside the dev shell needs `nasm` + `cmake` on PATH or `mmce-codecs` won't link):

```sh
direnv allow          # or: nix develop
```

Build / run / test:

```sh
cargo build --release
./target/release/mmce /path/to/book.cbz     # or a folder, or a .7z / .cb7

cargo test --workspace --release            # full suite
cargo test -p mmce-codecs zip_source_supports_concurrent_reads  # single test
cargo test -p mmce-render -- tests::queue_prioritises_high_over_low

cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
```

Smoke test under Xvfb (3-second crash canary against a generated PNG fixture):

```sh
scripts/headless-smoke.sh
```

Legacy-INI fixture quickly (no fixture regeneration):

```sh
scripts/smoke.sh
```

## Workspace architecture

**Trust boundary**: `mmce-core` and everything below it have no `egui` dep — they can be unit-tested without a GPU context. `mmce-render` is the first crate that sees egui, and `mmce-app` is the only one that sees `eframe`.

```
crates/
  mmce-config/     INI (UTF-16 LE + BOM) parse/save, typed Settings, `extra` preserves unknown sections round-trip
  mmce-codecs/     PageSource trait + FolderSource / ZipSource / SevenzSource, cover_image() fast path
  mmce-core/       Book, Spread pairing (honours PageMode + BindDir), ViewerState — pure logic
  mmce-filters/    FilterOp pipeline: rotate / clip / adjust / sharpen / resize
  mmce-store/      SQLite (rusqlite bundled): bookmarks, history, per-book state, one-shot legacy-INI import
  mmce-render/     PageCache: CPU-scaled decoder pool (clamp 2-8) with priority queue (high = current window,
                   low = prefetch), epoch-based stale-decode cancellation on big jumps, directional
                   prefetch_directed(window, fwd, bwd, hint), GPU texture LRU mirroring the CPU LRU,
                   mipmapped linear-filtered textures, from_rgba_premultiplied for opaque pages
  mmce-app/        eframe frontend: keybindings, CLI, page-flip animation (tessellated curl mesh),
                   explorer gallery with thumbnail prefetch, settings dialog, playback, overlays,
                   bookmarks/history/goto/rename/delete dialogs
```

### Key invariants you will break if you don't know them

- **Page flip hinge is always at the viewport centre.** `draw_spread` anchors both pages' spine-adjacent edges to `ui.min_rect().center().x`; single-page mode uses `PagePaint::split_at(spine_x)` to produce the two half-textures the animator needs. Do not change spread layout without re-reading `mmce-app/src/anim.rs` + `normalise_pair` in `lib.rs`.
- **Archive enumeration is the one time we decompress entries we don't need.** For 7z `SevenzSource::open` has to walk the solid-block decoder forward (`for_each_entries` with `std::io::copy(r, sink)`), which is why there's a separate `cover_image()` fast path in `mmce-codecs/src/lib.rs` that early-exits after the first image. Thumbnails must use `cover_image`, not `open_source(path)?.read(0)`.
- **Archive reads are pooled for parallelism.** `ZipSource` / `SevenzSource` own `Mutex<Vec<Archive>>` pools (cap 8) so N decoder workers can inflate concurrently. Never reintroduce a single-instance `Mutex<ZipArchive<File>>` — it serializes the inflate path and NVMe sits idle.
- **`tex` in `PageCache` must stay bounded.** It's a `TexLru` mirroring the CPU cache's capacity; if someone turns it back into an unbounded `HashMap<usize, TextureHandle>` VRAM leaks a texture per visited page.
- **Session state is intentionally ephemeral.** `attach_store_book` does NOT auto-resume to `last_page` on open — users asked for explicit nav only. Bookmarks + the History dialog are the supported path back to a spot.
- **Legacy INI keys cannot be renamed.** `mmce-config/src/lib.rs::from_ini`/`to_ini` round-trip the exact casing in `legacy/MangaMeeyaCE.ini` so existing configs don't get mangled. The `extra: Ini` field stashes sections we don't interpret so save re-emits them.

### SIMD / performance levers already in place

Don't reintroduce these by reverting them:

- `.cargo/config.toml`: `rustflags = ["-C", "target-cpu=native"]` — activates zune-jpeg AVX2/NEON IDCT + compiler auto-vectorization across the workspace.
- `flate2` with **`zlib-rs`** backend (workspace-level feature override, anchored by a direct dep in `mmce-codecs`). DEFLATE inflate for CBZ + PNG goes through pure-Rust SIMD paths instead of `miniz_oxide`.
- `DecodedPage::from_dynamic` uses `into_rgba8()` (zero-copy when already RGBA8) rather than `to_rgba8()` which always clones.
- Thumbnail resize uses `DynamicImage::thumbnail_exact` (fast box filter), not `resize_exact(Triangle)`.
- **Thumbnail cover decode is DCT-scaled for JPEG.** `mmce_codecs::decode_cover_image(bytes, target)` decodes JPEG covers through `mozjpeg` (libjpeg-turbo) at the smallest `1/8…1/1` step whose longest edge stays ≥ `target` (`scale(n)` where `n = ceil(8·target / max(w,h))`). Measured ~1.5–2× faster than the full zune decode on real covers, and it also collapses the box-shrink (you downscale a ~150px image, not a ~1500px one). It is **JPEG-only** and falls back to the full zune `decode_image` for non-JPEG, CMYK/YCCK, or any decode error — worst case is "slower", never "wrong". The SOI magic-byte guard in `decode_jpeg_scaled` is load-bearing: handed non-JPEG bytes libjpeg can *abort the process*, so never feed it anything that isn't a JPEG. Don't revert this to a plain `decode_image` in `thumbs.rs::decode_cover`.
- Forward-biased prefetch: app passes a `hint_direction` derived from cursor delta into `prefetch_directed(&window, 4, 1, hint)`.
- **Thumbnail decode pool is oversubscribed ~1.5× cores.** Cold explorer scans are I/O-wait-bound — a worker blocks on a cold archive read and idles its core — so `worker_pool_size()` in `mmce-app/src/thumbs.rs` defaults to `(cores*3/2).clamp(4,16)` instead of `cores-1`. Measured ~+12% cold-scan throughput already at cores+1 and rising toward a ~+75% ceiling near ~1.66× cores (the cold floor ≈ the warm time), while warm throughput stays flat out to 4× cores (no regression — the oversubscription is free when reads hit the page cache). Override with `MMCE_THUMB_WORKERS` (usize, ignored if unset/invalid/zero, clamp [1,64]); push it higher on slow / FUSE storage where reads block longer. Don't revert the default to `cores-1`.

## CLI

```
mmce [OPTIONS] [PATH...]

--fullscreen      Start fullscreen
--last            Re-open last folder from config
--inifile PATH    Use this INI as config
--viewmode N      0=book 1=thumbnail 2=explorer (only 0 + 2 wired)
--add             Append to existing list rather than replacing
```

Paths may be folders, `.zip`/`.cbz`, `.7z`/`.cb7`, or loose image files.

## Keybindings (runtime, not source)

See `README.md` and `const HELP` in `crates/mmce-app/src/main.rs`. Notable: `E` toggles explorer / book view; `Space` toggles single / spread; `F11` / `Alt+Enter` fullscreen; `Shift+↑/↓` jumps to sibling folder / archive at the same level.

## Legacy compatibility notes

- `MangaMeeyaCE.ini` is UTF-16 LE with a BOM. We read UTF-8 or UTF-16, write UTF-16 LE + BOM.
- `ViewMode.Sort=11` (legacy default) means natural name sort.
- `ScaleMode.Mode`: `0 → Original`, `1|2 → Fit`, `3 → FitWidth`, `4 → FitHeight`, anything else → `Fit`.
- `BindDir=1` (right-to-left) is the default because manga.

## Intentionally NOT in scope (dropped from the original)

Furigana/ruby overlay, per-archive resume state, RAR archives, PDF (legacy `pdf.dll`), tool-button bitmap toolbars, folder-tree / file-list / thumbnail side panels. None are on the fast path for reading.

## Historical note

An earlier branch explored VA-API GPU JPEG decode (`mmce-gpu-decode` crate). It was rolled back after the VA-API handle pool hit reproducible heap corruption under valgrind that couldn't be isolated. If you see stray `mmce_gpu_decode`, `use_gpu_decode`, `UseGpuDecode`, or `MMCE_GPU_DECODE` references anywhere, they're leftover — clean them up.
