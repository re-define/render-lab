mod benchmark;
mod hud;
mod input;
mod viewport;
use gpui::{prelude::*, *};
use render_lab::{
    console::Console,
    engine::Engine,
    experiments::motion,
    rdg::Queue,
    rhi::{FrameProfile, PassReport},
};
use std::{rc::Rc, sync::Arc, time::Instant};

struct Studio {
    engine: Engine,
    focus: FocusHandle,
    previous: Instant,
    navigation: Rc<input::Navigation>,
    motion_keys: [i8; 2],
    motion_anchor: Option<(render_lab::scene::Id, glam::Vec3)>,
    benchmark: Option<benchmark::Benchmark>,
    viewport: Rc<viewport::Viewport>,
    counters: render_lab::rhi::FrameCounters,
    passes: Vec<PassReport>,
    gpu: String,
    hud: Option<Entity<hud::Hud>>,
    hud_updated: Option<Instant>,
    profile: FrameProfile,
    command: Option<String>,
    message: String,
    console: Console,
    selected_pass: usize,
}

fn queue_color(queue: Queue) -> u32 {
    match queue {
        Queue::Graphics => 0x83ccee,
        Queue::Compute => 0xb6a3fa,
        Queue::Transfer => 0xeac685,
    }
}

fn toolbar_button(id: &'static str, label: &'static str, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .rounded_md()
        .px_3()
        .py_1()
        .text_sm()
        .bg(rgb(if active { 0x244d61 } else { 0x232d3b }))
        .hover(|style| style.bg(rgb(0x335066)))
        .child(label)
}
impl Studio {
    fn control_motion(&mut self, dt: f32) -> bool {
        let Some(id) = self.engine.movement.target(&self.engine.world) else {
            self.motion_keys = [0; 2];
            self.motion_anchor = None;
            return false;
        };
        let root = self.engine.world.entities[&id].local.w_axis.truncate();
        let keys = self.navigation.motion_keys();
        let new_actor = self.motion_anchor.is_none_or(|(old, _)| old != id);
        let follows = self.engine.movement.camera_follows(&self.engine.world, id);
        let followed = self.motion_anchor.map_or(root, |(old, previous)| {
            if old != id || !follows {
                return root;
            }
            let delta = (root - previous) * (1. - (-5. * dt).exp());
            self.engine.camera.target += delta;
            previous + delta
        });
        self.motion_anchor = Some((id, followed));
        // An idle keyboard leaves a restored or scripted analog command in control.
        if (keys != self.motion_keys || (new_actor && keys != [0; 2]))
            && let Err(e) = self.engine.movement.apply(
                Arc::make_mut(&mut self.engine.world),
                id,
                render_lab::engine::MoveIntent {
                    side: keys[0],
                    forward: keys[1],
                },
            )
        {
            self.message = format!("{e:#}");
        }
        self.motion_keys = keys;
        true
    }

    fn pass_graph(&self, cx: &mut Context<Self>) -> Div {
        const ROW: f32 = 48.;
        let count = self.passes.len();
        let edges = self
            .passes
            .iter()
            .enumerate()
            .flat_map(|(to, p)| p.parents.iter().map(move |&from| (from, to)))
            .collect::<Vec<_>>();
        let height = px(count as f32 * ROW);
        let selected = self.selected_pass;
        let lines = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                for highlighted in [false, true] {
                    let color = rgb(if highlighted { 0x83ccee } else { 0x344454 });
                    let segment = |window: &mut Window, x: f32, y: f32, w: f32, h: f32| {
                        let origin = bounds.origin + point(px(x), px(y));
                        window.paint_quad(fill(Bounds::new(origin, size(px(w), px(h))), color));
                    };
                    for &(from, to) in &edges {
                        if (from == selected || to == selected) != highlighted {
                            continue;
                        }
                        let x = 4. + (from % 5) as f32 * 4.;
                        let top = from as f32 * ROW + 28.;
                        let bottom = to as f32 * ROW + 20.;
                        segment(window, x, top, 28. - x, 1.);
                        segment(window, x, top, 1., bottom - top);
                        segment(window, x, bottom, 28. - x, 1.);
                    }
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
                .h(px(42.))
                .px_2()
                .flex()
                .flex_col()
                .justify_center()
                .rounded_md()
                .border_l_2()
                .border_color(rgb(queue_color(pass.queue)))
                .bg(rgb(if self.selected_pass == row {
                    0x283c50
                } else {
                    0x1b2432
                }))
                .hover(|style| style.bg(rgb(0x283447)))
                .child(div().text_sm().truncate().child(pass.name.clone()))
                .child(
                    div()
                        .text_xs()
                        .flex()
                        .justify_between()
                        .text_color(rgb(queue_color(pass.queue)))
                        .child(format!("{:?} · {}", pass.queue, pass.phase.key()))
                        .child(format!("{:.3} ms", pass.ms)),
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
        let mut panel = div().flex().flex_col().flex_1().min_h_0().gap_2();
        if let Some(p) = self.passes.get(self.selected_pass) {
            let barriers = div()
                .id("pass-barriers")
                .max_h(px(106.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .children(p.barriers.iter().map(|barrier| {
                    div()
                        .text_xs()
                        .text_color(rgb(0xb9cde1))
                        .child(barrier.clone())
                }));
            panel = panel.child(
                div()
                    .rounded_md()
                    .bg(rgb(0x1b2533))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(p.name.clone()))
                    .child(div().text_xs().text_color(rgb(0x98abc1)).child(format!(
                        "{} dependencies   ·   {} barriers",
                        p.parents.len(),
                        p.barriers.len()
                    )))
                    .child(barriers),
            );
        }
        panel.child(
            div()
                .id("rdg-passes")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(div().flex().child(lines).child(nodes)),
        )
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
                    self.motion_anchor = None;
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
                "/" | "enter" if !event.is_held => {
                    self.navigation.clear();
                    self.command = Some(String::new());
                }
                "f" => {
                    let _ = self.console.execute(&mut self.engine, &self.passes, "fit");
                }
                _ => self.navigation.key(k, true),
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
        if !window.is_window_active() || self.command.is_some() {
            self.navigation.clear();
        }
        self.navigation.advance(&mut self.engine.camera, dt);
        let keyboard_motion = self.control_motion(dt);
        if let Some(extent) = self.viewport.extent()
            && let Err(e) = self
                .engine
                .tick(if self.console.paused { 0. } else { dt }, extent)
        {
            self.message = format!("{e:#}");
        }
        while let Some(frame) = self.engine.take_frame() {
            match frame {
                Ok(mut f) => {
                    if let Some(recording) = self.console.recording.as_mut()
                        && let Err(error) = recording.offer(&mut f)
                    {
                        if let Some(failed) = self.console.recording.take() {
                            failed.abort();
                        }
                        self.engine.capture_viewport = false;
                        self.message = format!("Recording stopped: {error:#}");
                    }
                    if self.benchmark.as_mut().is_some_and(|b| b.observe(&f)) {
                        if let Some(recording) = self.console.recording.take() {
                            let dropped = recording.dropped();
                            match recording.finish() {
                                Ok(()) => println!("LIVE_RECORDING dropped_samples={dropped}"),
                                Err(error) => eprintln!("Benchmark recording failed: {error:#}"),
                            }
                        }
                        cx.quit();
                        break;
                    }
                    if self.console.show_hud
                        && self
                            .hud_updated
                            .is_none_or(|previous| now.duration_since(previous).as_millis() >= 125)
                        && let (Some(hud), Some(mode)) = (&self.hud, f.presentation.as_deref())
                    {
                        let temporal = f
                            .passes
                            .iter()
                            .find_map(|p| match p.name.as_str() {
                                "Temporal AA resolve" => Some("TAA"),
                                "NVIDIA DLAA" => Some("DLAA"),
                                "NVIDIA DLSS Quality" => Some("DLSS Quality"),
                                _ => None,
                            })
                            .unwrap_or("AA off");
                        let gpu_ms = f
                            .profile
                            .gpu_span_ms
                            .unwrap_or_else(|| f.passes.iter().map(|p| p.ms).sum());
                        let line = format!("{gpu_ms:.1} ms GPU   ·   {mode}   ·   {temporal}");
                        hud.update(cx, |hud, cx| hud.update(line, cx));
                        self.hud_updated = Some(now);
                    }
                    self.counters = f.counters;
                    self.passes = f.passes;
                    self.gpu = f.gpu;
                    self.profile = f.profile;
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
        let mut details = div()
            .id("experiment-details")
            .overflow_y_scroll()
            .max_h(px(128.))
            .flex()
            .flex_col()
            .gap_1()
            .text_xs()
            .text_color(rgb(0xb9cde1));
        if let Ok(Some(p)) = motion::silver::status_for(
            &self.engine.world,
            self.engine.movement.target(&self.engine.world),
        ) {
            details = details
                .child(format!(
                    "Motion · {} · sample {} · cost {:.3}",
                    p.clip, p.sample, p.cost
                ))
                .child(format!("{} · {} poses scored", p.search, p.evaluated));
        } else if let Some((_, p)) = self.engine.world.first_raw_component(motion::PLAYER) {
            details = details.child(format!(
                "Motion · {} · sample {} · cost {:.3}",
                p["clip"].as_str().unwrap_or("?"),
                p["sample"],
                p["cost"].as_f64().unwrap_or_default()
            ));
            if let Some(mode) = p["mode"].as_str() {
                details = details.child(format!(
                    "{} · {} poses scored",
                    mode.to_uppercase(),
                    p["evaluated"]
                ));
            }
        }
        for (name, n) in self.counters.iter() {
            if *n > 0 {
                details = details.child(format!("{name}   {n}"));
            }
        }
        let key = render_lab::experiments::clusters::VISUALIZE;
        let entities = &self.engine.world.entities;
        let visualize = entities
            .component_owners(key)
            .iter()
            .any(|id| entities[id].components[key].as_bool() == Some(true));
        let mut legend = div().flex().gap_2();
        let cartoon = self
            .engine
            .world
            .first_raw_component(render_lab::experiments::cartoon::SETTING);
        if let Some((_, settings)) = cartoon {
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
                        .rounded_md()
                        .px_2()
                        .py_1()
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
        let tools = div()
            .flex()
            .gap_2()
            .items_center()
            .child(
                toolbar_button(
                    "pause",
                    if self.console.paused {
                        "Resume"
                    } else {
                        "Pause"
                    },
                    self.console.paused,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.console.paused = !this.console.paused;
                    cx.notify();
                })),
            )
            .child(
                toolbar_button("rdg-toggle", "RDG", self.console.show_graph).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.console.show_graph = !this.console.show_graph;
                        cx.notify();
                    },
                )),
            );
        let viewport = self.viewport.element(
            self.command.is_none().then_some(&self.navigation),
            self.console.show_hud,
        );
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                this.navigation.key(&event.keystroke, false);
            }))
            .bg(rgb(0x141b27))
            .text_color(rgb(0xe5edf0))
            .flex()
            .flex_col()
            .font_family("Segoe UI")
            .child(
                div()
                    .px_4()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .bg(rgb(0x1b2432))
                    .child(div().flex().flex_col().min_w_0()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Render Lab"))
                        .child(div().truncate().text_xs().text_color(rgb(0x9db0c8)).child(self.gpu.clone())))
                    .child(tools),
            )
            .when(visualize || cartoon.is_some(), |root| {
                root.child(div().px_3().py_1().text_xs().child(legend.items_center().flex_wrap()))
            })
            .child(div().flex().flex_1().min_h_0().child(viewport).when(
                self.console.show_graph,
                |d| {
                    d.child(
                        div()
                            .id("rdg")
                            .w(px(304.))
                            .min_w(px(235.))
                            .p_2()
                            .bg(rgb(0x141b27))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().px_1().text_sm().font_weight(FontWeight::SEMIBOLD).child("Render dependency graph"))
                            .child(div().px_1().text_xs().text_color(rgb(0x9db0c8)).child(format!(
                                "{} passes   ·   {} waits   ·   {} record workers",
                                self.passes.len(), self.profile.timeline_waits, self.profile.recording_workers
                            )))
                            .child(details)
                            .child(self.pass_graph(cx)),
                    )
                },
            ))
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0xb3c7ce))
                    .child(if keyboard_motion {
                        "IJKL  Move character     WASD  Move camera     Right mouse  Look     /  Console"
                    } else {
                        "WASD  Move     Right mouse  Look     F  Frame scene     /  Console"
                    }),
            )
            .child(
                div().px_3().py_2().text_sm().bg(rgb(0x0b121a)).child(
                    self.command
                        .as_ref()
                        .map(|s| format!("❯ {s}▏"))
                        .unwrap_or_else(|| self.message.clone()),
                ),
            )
    }
}
pub fn run(mut engine: Engine, console: Console, benchmark: Option<usize>) {
    engine.set_ordered_completions(benchmark.is_some());
    Application::new().run(move |cx| {
        let bounds = Bounds::centered(None, size(px(1120.), px(690.)), cx);
        let studio = cx
            .open_window(
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
                        cx.observe_window_activation(window, |studio: &mut Studio, window, _| {
                            if !window.is_window_active() {
                                studio.navigation.clear();
                            }
                        })
                        .detach();
                        let viewport =
                            viewport::Viewport::new(window).expect("create Vulkan viewport");
                        let closing = viewport.clone();
                        window.on_window_should_close(cx, move |_, _| {
                            closing.detach_overlay();
                            true
                        });
                        unsafe {
                            engine.set_viewport_window(viewport.hwnd);
                        }
                        Studio {
                            engine,
                            focus,
                            previous: Instant::now(),
                            navigation: Default::default(),
                            motion_keys: [0; 2],
                            motion_anchor: None,
                            benchmark: benchmark.map(benchmark::Benchmark::new),
                            viewport,
                            counters: Default::default(),
                            passes: vec![],
                            gpu: String::new(),
                            hud: None,
                            hud_updated: None,
                            profile: FrameProfile::default(),
                            command: None,
                            message: "Type /help for commands".into(),
                            console,
                            selected_pass: 0,
                        }
                    })
                },
            )
            .unwrap();
        cx.on_window_closed(move |cx| {
            if !cx
                .windows()
                .iter()
                .any(|window| window.window_id() == studio.window_id())
            {
                cx.quit();
            }
        })
        .detach();
        let viewport = studio
            .update(cx, |studio, _, _| studio.viewport.clone())
            .unwrap();
        let overlay = hud::open(&viewport, cx);
        cx.on_app_quit(move |_| {
            viewport.detach_overlay();
            std::future::ready(())
        })
        .detach();
        let _ = studio.update(cx, move |studio, _, cx| {
            match overlay {
                Ok(overlay) => studio.hud = Some(overlay),
                Err(error) => studio.message = format!("HUD unavailable: {error:#}"),
            }
            cx.notify();
        });
        cx.activate(true);
    });
}
