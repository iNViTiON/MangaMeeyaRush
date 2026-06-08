use std::path::PathBuf;

use anyhow::Result;
use mmce_app::{App, CliArgs};


fn main() -> Result<()> {
    // Default: warn for everything, info for our own crates. Third-party
    // libraries (zbus, calloop, winit, …) emit useful-to-them but
    // noisy-to-us INFO lines during normal operation — we don't want
    // those on stdout. Override with RUST_LOG=... to see more.
    //
    // `calloop=error`: winit's Wayland backend logs a benign WARN on every
    // frame —  "Received an event for non-existence source: …" — when a
    // calloop source is removed while an event for it is still queued. It's
    // an upstream sctk/winit teardown race we don't drive and can't fix from
    // here, so we drop it below ERROR rather than let it flood the console.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "warn,calloop=error,mmce_app=info,mmce_core=info,mmce_render=info,\
             mmce_codecs=info,mmce_store=info,mmce_filters=info,\
             mmce_config=info",
    ))
    .init();

    let cli = parse_cli();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mmce — manga viewer")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([400.0, 300.0]),
        ..Default::default()
    };

    eframe::run_native(
        "mmce",
        native_options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, cli)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

fn parse_cli() -> CliArgs {
    let mut args = CliArgs::default();
    let mut iter = std::env::args().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "/fullscreen" | "--fullscreen" => args.fullscreen = true,
            "/last" | "--last" => args.last = true,
            "/inifile" | "--inifile" => {
                if let Some(p) = iter.next() {
                    args.ini = Some(PathBuf::from(p));
                }
            }
            "/viewmode" | "--viewmode" => {
                if let Some(v) = iter.next() {
                    args.view_mode = v.parse().ok();
                }
            }
            "/add" | "--add" => args.add = true,
            "-h" | "--help" => {
                eprintln!("{}", HELP);
                std::process::exit(0);
            }
            other => {
                args.paths.push(PathBuf::from(other));
            }
        }
    }
    args
}

const HELP: &str = "\
mmce — MangaMeeya Rust port

Usage:
  mmce [OPTIONS] [PATH...]

Options:
  --fullscreen       Start in fullscreen
  --last             Re-open last folder (from config)
  --inifile PATH     Use this INI as config
  --viewmode N       0=book 1=thumbnail 2=explorer (currently only 0 honoured)
  --add              Add files to existing list rather than replacing
  -h, --help         Show this help

Keyboard:
  ←/→       previous / next spread
  Shift+←/→ previous / next single page
  / , ?     skip: slide only the later page of a 2-up spread (? = back)
  Home/End  first / last page
  Space     toggle single / spread mode
  + / -     zoom in / out
  0         reset zoom / fit
  F11 or Alt+Enter  fullscreen toggle
  Ctrl+O    open folder or archive
  Esc       exit fullscreen (or quit if not fullscreen)
";
