mod benchmark;
mod viewport;
use gpui::{prelude::*, *};
use render_lab::{console::Console, engine::Engine, experiments::motion, rhi::PassReport};
use std::{rc::Rc, time::Instant};

struct Studio {
    engine: Engine,
    focus: FocusHandle,
    previous: Instant,
    benchmark: Option<benchmark::Benchmark>,
    viewport: Rc<viewport::Viewport>,
    counters: std::collections::BTreeMap<&'static str, u64>,
    passes: Vec<PassReport>,
    gpu: String,
    command: Option<String>,
    message: String,
    console: Console,
    selected_pass: usize,
}
impl Studio {
    fn pass_graph(&self, cx: &mut Context<Self>) -> Div {
        const ROW: f32 = 64.;
        let count = self.passes.len();
        let edges = self
            .passes
            .iter()
            .enumerate()
            .flat_map(|(to, p)| p.parents.iter().map(move |&from| (from, to)))
            .collect::<Vec<_>>();
        let height = px(count as f32 * ROW);
        let lines = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let segment = |window: &mut Window, x: f32, y: f32, w: f32, h: f32| {
                    let origin = bounds.origin + point(px(x), px(y));
                    window.paint_quad(fill(Bounds::new(origin, size(px(w), px(h))), rgb(0x67b5bc)));
                };
                for (from, to) in edges {
                    let x = 4. + (from % 4) as f32 * 4.;
                    let top = from as f32 * ROW + 35.;
                    let bottom = to as f32 * ROW + 26.;
                    segment(window, x, top, 28. - x, 1.);
                    segment(window, x, top, 1., bottom - top);
                    segment(window, x, bottom, 28. - x, 1.);
                    segment(window, 25., bottom - 2., 4., 4.);
                }
            },
        )
        .w(px(30.))
        .h(height);
        let mut nodes = div().flex_col().flex().flex_1().min_w_0();
        for (row, pass) in self.passes.iter().enumerate() {
            let node = div()
                .id(("pass", row))
                .cursor_pointer()
                .h(px(54.))
                .p_1()
                .border_l_2()
                .border_color(rgb(0x67b5bc))
                .bg(rgb(if self.selected_pass == row {
                    0x293f49
                } else {
                    0x1d2c35
                }))
                .child(div().text_sm().truncate().child(pass.name.clone()))
                .child(
                    div()
                        .text_xs()
                        .child(format!("{:?}    {:.3} ms", pass.queue, pass.ms)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_pass = row;
                    cx.notify();
                }));
            nodes = nodes.child(
                div()
                    .h(px(ROW))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(node),
            );
        }
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().child(lines).child(nodes));
        if let Some(p) = self.passes.get(self.selected_pass) {
            panel = panel.child(div().text_sm().child(p.name.clone()));
            for barrier in &p.barriers {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xacc0c4))
                        .child(barrier.clone()),
                );
            }
        }
        panel
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if let Some(line) = &mut self.command {
            if k.modifiers.control && k.key == "v" {
                if let Some(s) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    line.push_str(s.trim_end());
                }
                cx.notify();
                return;
            }
            match k.key.as_str() {
                "enter" => {
                    let text = self.command.take().unwrap();
                    self.message = self
                        .console
                        .execute(&mut self.engine, &self.passes, &text)
                        .unwrap_or_else(|e| format!("{e:#}"));
                }
                "escape" => self.command = None,
                "backspace" => {
                    line.pop();
                }
                "space" => line.push(' '),
                _ => {
                    if let Some(c) = &k.key_char {
                        line.push_str(c)
                    }
                }
            }
        } else {
            match k.key.as_str() {
                "/" | "enter" => self.command = Some(String::new()),
                "f" => {
                    let _ = self.console.execute(&mut self.engine, &self.passes, "fit");
                }
                _ => (),
            }
        }
        cx.notify();
    }
}
impl Render for Studio {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let dt = (now - self.previous).as_secs_f32().min(0.1);
        self.previous = now;
        self.viewport
            .navigate(&mut self.engine.camera, dt, self.command.is_none());
        if let Some(extent) = self.viewport.extent()
            && let Err(e) = self
                .engine
                .tick(if self.console.paused { 0. } else { dt }, extent)
        {
            self.message = format!("{e:#}");
        }
        if let Some(frame) = self.engine.take_frame() {
            match frame {
                Ok(f) => {
                    if let Some(recording) = self.console.recording.as_mut()
                        && let Err(error) = recording.push(&f)
                    {
                        if let Some(failed) = self.console.recording.take() {
                            failed.abort();
                        }
                        self.engine.capture_viewport = false;
                        self.message = format!("Recording stopped: {error:#}");
                    }
                    if self.benchmark.as_mut().is_some_and(|b| b.observe(&f)) {
                        cx.quit();
                    }
                    self.counters = f.counters;
                    self.passes = f.passes;
                    self.gpu = format!(
                        "{} · {} · {} submits",
                        f.gpu,
                        f.presentation.unwrap_or_default(),
                        f.submissions
                    );
                }
                Err(e) => {
                    if self.benchmark.is_some() {
                        eprintln!("Benchmark failed: {e}");
                        cx.quit();
                    }
                    self.message = e;
                }
            }
        }
        window.request_animation_frame();
        let mut graph = self.pass_graph(cx);
        if let Some(p) = self.engine.world.entities.values().find_map(|e| {
            e.components
                .get(motion::silver::PLAYER)
                .or_else(|| e.components.get(motion::PLAYER))
        }) {
            graph = graph.child(format!(
                "Motion · {} · sample {} · cost {:.3}",
                p["clip"].as_str().unwrap_or("?"),
                p["sample"],
                p["cost"].as_f64().unwrap_or_default()
            ));
            if let Some(mode) = p["mode"].as_str() {
                graph = graph.child(format!(
                    "{} · {} poses scored",
                    mode.to_uppercase(),
                    p["evaluated"]
                ));
            }
        }
        for (name, n) in &self.counters {
            if *n > 0 {
                graph = graph.child(format!("{name}   {n}"));
            }
        }
        let visualize = self.engine.world.entities.values().any(|e| {
            e.components
                .get(render_lab::experiments::clusters::VISUALIZE)
                .and_then(|v| v.as_bool())
                == Some(true)
        });
        let mut legend = div().flex().gap_2();
        if let Some(settings) = self
            .engine
            .world
            .entities
            .values()
            .find_map(|e| e.components.get(render_lab::experiments::cartoon::SETTING))
        {
            let mode = settings["outline"].as_str().unwrap_or("off");
            let angle = settings["azimuth"].as_f64().unwrap_or_default();
            let sdf = settings["face_shadow"].as_bool() == Some(true);
            let mut buttons = Vec::new();
            for (label, choice) in [
                ("Off", "off"),
                ("Image", "image"),
                ("Hull", "hull"),
                ("Edge quads", "quad"),
            ] {
                buttons.push((label, format!("outline {choice}"), mode == choice));
            }
            buttons.extend([
                ("−15°", format!("light {}", angle - 15.), false),
                ("+15°", format!("light {}", angle + 15.), false),
                (
                    "Face SDF",
                    format!("sdf {}", if sdf { "off" } else { "on" }),
                    sdf,
                ),
            ]);
            legend = legend.child("Outline");
            for (index, (label, command, selected)) in buttons.into_iter().enumerate() {
                if index == 4 {
                    legend = legend.child(format!("Light {angle:.0}°"));
                }
                legend = legend.child(
                    div()
                        .id(("toon-control", index))
                        .cursor_pointer()
                        .px_1()
                        .bg(rgb(if selected { 0x365563 } else { 0x23333d }))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.message = render_lab::experiments::cartoon::command(
                                &mut this.engine,
                                &command,
                            )
                            .unwrap_or_else(|e| e.to_string());
                            cx.notify();
                        })),
                );
            }
        }
        if visualize {
            for (i, &color) in render_lab::experiments::clusters::LOD_COLORS
                .iter()
                .enumerate()
            {
                let name = if i == 9 {
                    "L9+".to_owned()
                } else {
                    format!("L{i}")
                };
                if self.counters.get(name.as_str()).copied().unwrap_or(0) > 0 {
                    legend = legend.child(div().text_color(rgb(color)).child(name));
                }
            }
        }
        let viewport = self.viewport.element();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .bg(rgb(0x151d28))
            .text_color(rgb(0xe5edf0))
            .flex()
            .flex_col()
            .font_family("Segoe UI")
            .child(
                div()
                    .p_3()
                    .flex()
                    .justify_between()
                    .child("Render Lab")
                    .child(legend)
                    .child(self.gpu.clone()),
            )
            .child(div().flex().flex_1().min_h_0().child(viewport).when(
                self.console.show_graph,
                |d| {
                    d.child(
                        div()
                            .id("rdg")
                            .overflow_y_scroll()
                            .w(px(265.))
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child("Render dependency graph")
                            .child(graph),
                    )
                },
            ))
            .child(
                div()
                    .p_2()
                    .text_sm()
                    .text_color(rgb(0xb3c7ce))
                    .child("WASD  Move     Right mouse  Look     F  Frame scene     /  Console"),
            )
            .child(
                div().p_3().bg(rgb(0x0b121a)).child(
                    self.command
                        .as_ref()
                        .map(|s| format!("❯ {s}▏"))
                        .unwrap_or_else(|| self.message.clone()),
                ),
            )
    }
}
pub fn run(mut engine: Engine, console: Console, benchmark: Option<usize>) {
    Application::new().run(move |cx| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit()
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1120.), px(690.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Render Lab".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(move |cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window);
                    let viewport = viewport::Viewport::new(window).expect("create Vulkan viewport");
                    unsafe {
                        engine.set_viewport_window(viewport.hwnd);
                    }
                    Studio {
                        engine,
                        focus,
                        previous: Instant::now(),
                        benchmark: benchmark.map(benchmark::Benchmark::new),
                        viewport,
                        counters: Default::default(),
                        passes: vec![],
                        gpu: String::new(),
                        command: None,
                        message: "Type /help for commands".into(),
                        console,
                        selected_pass: 0,
                    }
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
