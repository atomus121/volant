use gpui_kit::base::StyledExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;
use volant_app_core::model::Paths;

struct VolantShell {
    home: String,
}

impl Render for VolantShell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .bg(rgb(0x0f1013))
            .text_color(rgb(0xf2f3f5))
            .child(
                div()
                    .h(px(44.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x272a30))
                    .child("Volant"),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child("GPUI migration shell")
                    .child(div().text_color(rgb(0x8b909a)).child(self.home.clone()))
                    .child(
                        Button::new("new-download")
                            .primary()
                            .label("新建下载")
                            .on_click(|_, _, _| {}),
                    ),
            )
    }
}

fn main() {
    let home = Paths::resolve().home.display().to_string();

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| {
            window.set_window_title("Volant");
            cx.new(|_| VolantShell { home })
        })
        .expect("open Volant GPUI window");
    });
}
