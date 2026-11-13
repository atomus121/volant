//! Volant M0 · gpui 原型（先验证能否在本机 windows-gnu 上编译、跑起来、量体积）

use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};

struct Volant {
    rows: Vec<bench_model::Row>,
}

impl Render for Volant {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.rows.len();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x0B0C0E))
            .text_color(rgb(0xF2F3F5))
            .child(
                div()
                    .h(px(56.0))
                    .px(px(20.0))
                    .flex()
                    .items_center()
                    .text_size(px(15.0))
                    .child("Volant"),
            )
            .child(div().flex_1().child(format!("{n} rows")))
    }
}

fn main() {
    let rows = bench_model::sample_rows(1000);
    gpui::Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(780.0), px(470.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Volant { rows }),
        )
        .unwrap();
        cx.activate(true);
    });
}
