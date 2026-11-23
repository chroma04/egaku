//! egaku — a minimal, fast raster painting app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod color_picker;
mod icons;
mod panels;
mod prefs;
mod theme;

use eframe::egui;

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

        if !self.ui_hidden {
            egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
            egui::Panel::left("tools")
                .exact_size(48.0)
                .resizable(false)
                .show(ui, |ui| self.tool_strip(ui));
            egui::Panel::right("side")
                .default_size(272.0)
                .size_range(220.0..=420.0)
                .show(ui, |ui| self.side_panel(ui));
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
            .with_min_inner_size([720.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "egaku",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))),
    )
}
