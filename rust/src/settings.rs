// The settings window. Runs as its own process (`--settings`) and writes the shared config file;
// the visualiser picks up changes within half a second.

use crate::config::{Config, Shape, Style};
use eframe::egui;

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AudioWallpaper Settings")
            .with_inner_size([400.0, 660.0])
            .with_resizable(false),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "AudioWallpaper Settings",
        options,
        Box::new(|_cc| Ok(Box::new(SettingsApp::new()))),
    );
}

struct SettingsApp {
    config: Config,
    saved: Config,
    raised: bool,
}

impl SettingsApp {
    fn new() -> SettingsApp {
        let config = Config::load();
        SettingsApp { saved: config.clone(), config, raised: false }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.raised {
            self.raised = true;
            #[cfg(target_os = "macos")]
            crate::platform::mac::bring_to_front();
        }

        let c = &mut self.config;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Style");
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut c.style, Style::Wave, "Wave");
                ui.selectable_value(&mut c.style, Style::Bars, "Bars");
                ui.selectable_value(&mut c.style, Style::Shape, "3D Shape");
                ui.selectable_value(&mut c.style, Style::Cover, "Album Cover");
            });
            ui.add_space(6.0);

            egui::ComboBox::from_label("3D shape")
                .selected_text(c.shape.title())
                .show_ui(ui, |ui| {
                    for shape in Shape::ALL {
                        ui.selectable_value(&mut c.shape, shape, shape.title());
                    }
                });
            ui.add(egui::Slider::new(&mut c.orbiters, 0..=8).text("Orbiting shapes"));
            ui.checkbox(&mut c.cover_motion, "Moving cover dots");
            ui.add(egui::Slider::new(&mut c.cover_resolution, 10..=96).text("Cover resolution"));

            ui.separator();
            ui.heading("Movement");
            ui.horizontal(|ui| {
                ui.radio_value(&mut c.mirrored, true, "Symmetrical");
                ui.radio_value(&mut c.mirrored, false, "Left to right");
            });
            ui.checkbox(&mut c.monstercat, "Monstercat smoothing");

            ui.separator();
            ui.heading("Size and position");
            ui.add(egui::Slider::new(&mut c.wave_width, 0.5..=1.3).text("Wave width"));
            ui.add(egui::Slider::new(&mut c.shape_size, 0.4..=2.5).text("Shape & cover size"));
            ui.add(egui::Slider::new(&mut c.bottom_offset, 0.0..=200.0).text("Lift above Dock / taskbar"));

            ui.separator();
            ui.heading("Wallpaper");
            ui.add(egui::Slider::new(&mut c.dim, 0.0..=0.95).text("Dim wallpaper"));

            ui.add_space(12.0);
            if ui.button("Reset to defaults").clicked() {
                *c = Config::default();
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Changes apply straight away.").weak());
        });

        if self.config != self.saved {
            self.config.save();
            self.saved = self.config.clone();
        }
    }
}
