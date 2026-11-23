use super::*;

#[derive(Default)]
pub(super) struct Hud {
    line: String,
}

impl Hud {
    pub fn update(&mut self, line: String, cx: &mut Context<Self>) {
        if self.line != line {
            self.line = line;
            cx.notify();
        }
    }
}

impl Render for Hud {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_start()
            .p_3()
            .font_family("Segoe UI")
            .when(!self.line.is_empty(), |hud| {
                hud.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgba(0x101923e8))
                        .text_color(rgb(0xd7ebf2))
                        .text_xs()
                        .child(div().size(px(6.)).rounded_full().bg(rgb(0x8bcfb0)))
                        .child(self.line.clone()),
                )
            })
    }
}

pub(super) fn open(viewport: &viewport::Viewport, cx: &mut App) -> anyhow::Result<Entity<Hud>> {
    let mut mounted = Ok(());
    let window = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(240.), px(120.)),
            ))),
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            window_background: WindowBackgroundAppearance::Transparent,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        |window, cx| {
            mounted = viewport.attach_overlay(window);
            cx.new(|_| Hud::default())
        },
    )?;
    if let Err(error) = mounted {
        let _ = window.update(cx, |_, window, _| window.remove_window());
        return Err(error);
    }
    window.entity(cx)
}
