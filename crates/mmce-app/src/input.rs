//! Keyboard + mouse → app action routing.

use egui::{Context, Key, Modifiers, PointerButton};
use mmce_config::BindDir;

use crate::{App, View};

pub fn handle(app: &mut App, ctx: &Context) {
    let (keys, scroll, primary, secondary, modifiers) = ctx.input(|i| {
        (
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        repeat: _,
                        physical_key: _,
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            i.smooth_scroll_delta,
            i.pointer.button_clicked(PointerButton::Primary),
            i.pointer.button_clicked(PointerButton::Secondary),
            i.modifiers,
        )
    });

    for (key, mods) in keys {
        dispatch_key(app, ctx, key, mods);
    }

    // Only fire click navigation in Book view — Explorer has its own click
    // handler via response.clicked().
    if app.view == View::Book {
        if primary {
            forward_step(app);
        }
        if secondary {
            backward_step(app);
        }
    }

    // Ctrl+wheel: zoom in book, resize tiles in explorer.
    if modifiers.ctrl && scroll.y.abs() > 0.5 {
        let up = scroll.y > 0.0;
        match app.view {
            View::Book => {
                if up {
                    app.viewer.zoom_in();
                } else {
                    app.viewer.zoom_out();
                }
            }
            View::Explorer => {
                if up {
                    app.explorer_thumb_bigger();
                } else {
                    app.explorer_thumb_smaller();
                }
            }
        }
    }

    // Middle-drag / Alt+left-drag: pan (book only).
    if app.view == View::Book {
        let drag = ctx.input(|i| {
            if i.pointer.button_down(PointerButton::Middle)
                || (i.pointer.button_down(PointerButton::Primary) && i.modifiers.alt)
            {
                Some(i.pointer.delta())
            } else {
                None
            }
        });
        if let Some(d) = drag {
            if d.length() > 0.0 {
                app.viewer.pan[0] += d.x;
                app.viewer.pan[1] += d.y;
            }
        }
    }
}

fn dispatch_key(app: &mut App, ctx: &Context, key: Key, mods: Modifiers) {
    let plain = mods == Modifiers::NONE;
    let shift_only = mods.shift && !mods.alt && !mods.ctrl && !mods.command;
    let alt_only = mods.alt && !mods.shift && !mods.ctrl && !mods.command;
    let cmd_only = mods.command && !mods.alt && !mods.shift;

    // In Explorer view most keys are for tile navigation; route there
    // first so we don't accidentally advance the book.
    if app.view == View::Explorer {
        match key {
            Key::ArrowRight if plain => return app.explorer_move(1, 0),
            Key::ArrowLeft if plain => return app.explorer_move(-1, 0),
            Key::ArrowDown if plain => return app.explorer_move(0, 1),
            Key::ArrowUp if plain => return app.explorer_move(0, -1),
            Key::Home if plain => return app.explorer_move(-(1 << 20), 0),
            Key::End if plain => return app.explorer_move(1 << 20, 0),
            // Jump a screenful-ish of rows at a time. We don't track the
            // actual visible-row count, so 5 rows matches a reasonable
            // default viewport.
            Key::PageDown if plain => return app.explorer_move(0, 5),
            Key::PageUp if plain => return app.explorer_move(0, -5),
            Key::Enter if plain => return app.explorer_activate(ctx),
            Key::Backspace => return app.explorer_up(),
            Key::Plus | Key::Equals if cmd_only => return app.explorer_thumb_bigger(),
            Key::Minus if cmd_only => return app.explorer_thumb_smaller(),
            Key::E if plain => return app.toggle_explorer(ctx),
            Key::F11 | Key::Escape => { /* fall through to shared handler */ }
            _ => {
                // fall through for dialogs / fullscreen / open
            }
        }
    }

    let is_manga = app.viewer.bind_dir == BindDir::RightToLeft;

    match key {
        // Spread / page navigation (book view).
        Key::ArrowRight if plain => {
            if is_manga { backward_step(app) } else { forward_step(app) }
        }
        Key::ArrowLeft if plain => {
            if is_manga { forward_step(app) } else { backward_step(app) }
        }
        Key::ArrowRight if shift_only => {
            if is_manga { app.advance_pages(-1) } else { app.advance_pages(1) }
        }
        Key::ArrowLeft if shift_only => {
            if is_manga { app.advance_pages(1) } else { app.advance_pages(-1) }
        }
        Key::Home if plain => {
            if let Some(b) = app.book.as_mut() { b.first(); }
        }
        Key::End if plain => {
            if let Some(b) = app.book.as_mut() { b.last(); }
        }

        // ±10 fast jump.
        Key::PageDown => app.advance_pages(10),
        Key::PageUp => app.advance_pages(-10),

        // Sibling folder / archive.
        Key::ArrowDown if shift_only => app.jump_sibling(ctx, 1),
        Key::ArrowUp if shift_only => app.jump_sibling(ctx, -1),

        // View mode / zoom.
        Key::Space if plain => app.viewer.toggle_page_mode(),
        Key::Plus | Key::Equals if plain => app.viewer.zoom_in(),
        Key::Minus if plain => app.viewer.zoom_out(),
        Key::Num0 if plain => app.viewer.reset_zoom(),

        // Fullscreen.
        Key::F11 => {
            app.viewer.fullscreen = !app.viewer.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.viewer.fullscreen));
        }
        Key::Enter if alt_only => {
            app.viewer.fullscreen = !app.viewer.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.viewer.fullscreen));
        }
        Key::Escape => {
            if app.viewer.fullscreen {
                app.viewer.fullscreen = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }

        // Backspace in Book view returns to the explorer at the book's
        // parent, with the current book pre-selected.
        Key::Backspace => app.back_to_explorer(ctx),

        // Image-filter bindings.
        Key::R if plain => app.rotate(true),
        Key::R if shift_only => app.rotate(false),
        // Ctrl+R resets filters to identity (matches the legacy "no
        // rotation" command).
        Key::R if cmd_only => app.reset_filters(),

        // Overlays.
        Key::I if plain => app.toggle_info_overlay(),
        Key::L if plain => app.toggle_loupe(),
        // Hide the seek bar for an immersive reading view.
        Key::S if plain => app.toggle_seekbar(),

        // File ops.
        Key::O if plain || cmd_only => app.open_dialog(ctx),
        Key::O if shift_only => app.open_folder_dialog(ctx),

        // Explorer toggle.
        Key::E if plain => app.toggle_explorer(ctx),

        _ => {}
    }
}

fn forward_step(app: &mut App) {
    let s = app.current_stride() as isize;
    app.advance_pages(s);
}

fn backward_step(app: &mut App) {
    let s = app.current_stride() as isize;
    app.advance_pages(-s);
}
