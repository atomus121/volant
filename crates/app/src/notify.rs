//! egui adapter for the framework-independent UI wake-up boundary.

use std::sync::Arc;

use eframe::egui;
use volant_app_core::notify::{Notifier, UiNotifier};

struct EguiNotifier(egui::Context);

impl UiNotifier for EguiNotifier {
    fn notify(&self) {
        self.0.request_repaint();
    }
}

pub fn egui(ctx: &egui::Context) -> Notifier {
    Arc::new(EguiNotifier(ctx.clone()))
}
