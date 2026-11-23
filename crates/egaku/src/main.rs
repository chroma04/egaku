//! egaku — a fast painting and image editor.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod color_picker;
mod dialogs;
mod handles;
mod input;
mod overlay;
mod panels;
mod prefs;
mod recovery;
mod tablet;
mod theme;
mod widgets;

use eframe::egui::{self, Frame, Margin, Stroke};

impl eframe::App for app::App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // A painting app has no use for keyboard focus on buttons, and a
        // focused button would swallow Space (the pan key).
        if !ctx.egui_wants_keyboard_input()
            && let Some(id) = ctx.memory(|m| m.focused())
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        self.poll_io(&ctx);
        self.handle_dropped_and_pasted(&ctx);
        self.handle_close(&ctx);
        self.update_title(&ctx);
        self.autosave(&ctx);

        if !self.ui_hidden {
            let bar = |fill| {
                Frame::new()
                    .fill(fill)
                    .inner_margin(Margin::symmetric(6, 2))
            };
            egui::Panel::top("menu")
                .frame(bar(theme::BG_APP))
                .show(ui, |ui| self.menu_bar(ui));
            egui::Panel::top("context")
                .exact_size(40.0)
                .frame(
                    bar(theme::BG_PANEL)
                        .inner_margin(Margin::symmetric(10, 0))
                        .stroke(Stroke::new(1.0, theme::LINE)),
                )
                .show(ui, |ui| self.context_bar(ui));
            egui::Panel::left("tools")
                .exact_size(52.0)
                .resizable(false)
                .frame(bar(theme::BG_PANEL).stroke(Stroke::new(1.0, theme::LINE)))
                .show(ui, |ui| self.tool_strip(ui));
            egui::Panel::right("side")
                .default_size(292.0)
                .size_range(250.0..=440.0)
                .frame(
                    bar(theme::BG_PANEL)
                        .inner_margin(Margin::symmetric(12, 6))
                        .stroke(Stroke::new(1.0, theme::LINE)),
                )
                .show(ui, |ui| self.side_panel(ui));
            if self.prefs.show_timeline || self.ed.doc.state.frames > 1 {
                egui::Panel::bottom("timeline")
                    .exact_size(88.0)
                    .resizable(false)
                    .frame(
                        bar(theme::BG_PANEL)
                            .inner_margin(Margin::symmetric(12, 4))
                            .stroke(Stroke::new(1.0, theme::LINE)),
                    )
                    .show(ui, |ui| self.timeline(ui));
            }
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let vp = ui.max_rect();
            self.shortcuts(&ctx, vp);
            self.canvas_ui(ui);
        });
        self.modals(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.store(storage);
    }
}

fn main() -> eframe::Result {
    let open = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("egaku")
            .with_app_id("egaku")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "egaku",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))),
    )
}
