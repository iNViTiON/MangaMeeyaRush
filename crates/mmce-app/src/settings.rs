//! Settings dialog. Tabbed UI over the app's live state; changes apply
//! immediately and persist to the INI on exit.

use egui::{Align2, Context, Key, Slider};
use mmce_config::{BindDir, FitMode, PageMode};

use crate::App;

#[derive(Debug, Default)]
pub struct SettingsDialog {
    pub open: bool,
    pub tab: Tab,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    View,
    Playback,
    Appearance,
}

impl SettingsDialog {
    pub fn open_dialog(&mut self) {
        self.open = true;
    }
}

/// Show the settings dialog against `app`. Returns true if the user
/// changed anything that requires the caller to invalidate the page cache
/// (for now: nothing — filter changes go through dedicated methods).
pub fn show(ctx: &Context, app: &mut App) {
    if !app.settings_dialog.open {
        return;
    }
    let mut close = false;
    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(true)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([540.0, 420.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.settings_dialog.tab, Tab::General, "General");
                ui.selectable_value(&mut app.settings_dialog.tab, Tab::View, "View");
                ui.selectable_value(&mut app.settings_dialog.tab, Tab::Playback, "Playback");
                ui.selectable_value(&mut app.settings_dialog.tab, Tab::Appearance, "Appearance");
            });
            ui.separator();

            match app.settings_dialog.tab {
                Tab::General => draw_general(ui, app),
                Tab::View => draw_view(ui, app),
                Tab::Playback => draw_playback(ui, app),
                Tab::Appearance => draw_appearance(ui, app),
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() || ctx.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
            });
        });
    if close {
        app.settings_dialog.open = false;
    }
}

fn draw_general(ui: &mut egui::Ui, app: &mut App) {
    ui.checkbox(
        &mut app.settings.general.confirm_delete,
        "Confirm before deleting files in the explorer",
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Background colour (0xBBGGRR):");
        let mut v = app.settings.general.bg_color;
        if ui
            .add(egui::DragValue::new(&mut v).hexadecimal(6, false, true))
            .changed()
        {
            app.settings.general.bg_color = v;
            app.viewer.bg_color = v;
        }
    });
}

fn draw_view(ui: &mut egui::Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label("Default page mode:");
        egui::ComboBox::new("page_mode_cb", "")
            .selected_text(format!("{:?}", app.settings.view.page_mode))
            .show_ui(ui, |ui| {
                for m in [PageMode::Single, PageMode::Spread, PageMode::Auto] {
                    if ui
                        .selectable_label(app.settings.view.page_mode == m, format!("{m:?}"))
                        .clicked()
                    {
                        app.settings.view.page_mode = m;
                        app.viewer.page_mode = m;
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Reading direction:");
        for (label, dir) in [
            ("Right → Left (manga)", BindDir::RightToLeft),
            ("Left → Right (western)", BindDir::LeftToRight),
        ] {
            if ui
                .selectable_label(app.settings.view.bind_dir == dir, label)
                .clicked()
            {
                app.settings.view.bind_dir = dir;
                app.viewer.bind_dir = dir;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Default fit mode:");
        egui::ComboBox::new("fit_cb", "")
            .selected_text(format!("{:?}", app.settings.scale.mode))
            .show_ui(ui, |ui| {
                for m in [
                    FitMode::Fit,
                    FitMode::FitWidth,
                    FitMode::FitHeight,
                    FitMode::Original,
                    FitMode::Custom,
                ] {
                    if ui
                        .selectable_label(app.settings.scale.mode == m, format!("{m:?}"))
                        .clicked()
                    {
                        app.settings.scale.mode = m;
                        app.viewer.fit = m;
                    }
                }
            });
    });
    ui.checkbox(
        &mut app.settings.scale.no_zoom_in,
        "No zoom-in (don't scale images above 100%)",
    );
}

fn draw_playback(ui: &mut egui::Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label("Slideshow interval:");
        let mut ms = app.playback.interval.as_millis() as u64;
        if ui
            .add(Slider::new(&mut ms, 300..=60_000).suffix(" ms"))
            .changed()
        {
            app.playback.interval = std::time::Duration::from_millis(ms);
        }
    });
    ui.checkbox(
        &mut app.animations_enabled,
        "Smooth page-turn animations (crossfade)",
    );
}

fn draw_appearance(ui: &mut egui::Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label("Loupe radius:");
        ui.add(Slider::new(&mut app.overlays.loupe_radius, 40.0..=300.0).suffix(" px"));
    });
    ui.horizontal(|ui| {
        ui.label("Loupe magnification:");
        ui.add(Slider::new(&mut app.overlays.loupe_magnification, 1.5..=8.0).suffix("×"));
    });
    ui.checkbox(&mut app.overlays.seekbar, "Show seek bar");
    ui.horizontal(|ui| {
        ui.label("Picture-cache size:");
        ui.add(Slider::new(
            &mut app.settings.cache.picture_cache_size,
            16..=512,
        ));
    });
}
