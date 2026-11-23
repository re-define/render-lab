use gpui::{
    Bounds, DispatchPhase, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Window,
};
use render_lab::render::Camera;
use std::{cell::Cell, rc::Rc};

#[derive(Default)]
pub(super) struct Navigation {
    held: Cell<u8>,
    pointer: Cell<Option<Point<Pixels>>>,
    delta: Cell<[f32; 2]>,
}

impl Navigation {
    pub fn key(&self, key: &Keystroke, pressed: bool) {
        let Some(index) = ["w", "s", "a", "d", "i", "k", "j", "l"]
            .iter()
            .position(|&name| name == key.key)
        else {
            return;
        };
        let flag = 1 << index;
        let held = self.held.get() & !flag;
        let modifier = key.modifiers.control || key.modifiers.alt || key.modifiers.platform;
        self.held.set(if pressed && !modifier {
            held | flag
        } else {
            held
        });
    }

    fn axes(&self, start: usize) -> [i8; 2] {
        let held = self.held.get();
        let down = |index: usize| i8::from((held & (1u8 << index)) != 0);
        [
            down(start + 3) - down(start + 2),
            down(start) - down(start + 1),
        ]
    }

    pub fn motion_keys(&self) -> [i8; 2] {
        self.axes(4)
    }

    pub fn clear(&self) {
        self.held.set(0);
        self.pointer.set(None);
        self.delta.set([0.; 2]);
    }

    pub fn advance(&self, camera: &mut Camera, dt: f32) {
        let [dx, dy] = self.delta.replace([0.; 2]);
        if dx != 0. || dy != 0. {
            camera.look(dx * 0.003, dy * 0.003);
        }
        let [side, forward] = self.axes(0);
        if side != 0 || forward != 0 {
            camera.translate(side.into(), forward.into(), dt);
        }
    }

    pub fn paint(self: &Rc<Self>, bounds: Bounds<Pixels>, window: &mut Window) {
        let navigation = self.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
            if phase == DispatchPhase::Capture
                && event.button == MouseButton::Right
                && window.is_window_active()
                && bounds.contains(&event.position)
            {
                navigation.pointer.set(Some(event.position));
            }
        });
        let navigation = self.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, _| {
            if phase == DispatchPhase::Capture {
                navigation.drag(event);
            }
        });
        let navigation = self.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
            if phase == DispatchPhase::Capture && event.button == MouseButton::Right {
                navigation.pointer.set(None);
            }
        });
    }

    fn drag(&self, event: &MouseMoveEvent) {
        let Some(previous) = self.pointer.get() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Right) {
            self.pointer.set(None);
            return;
        }
        self.pointer.set(Some(event.position));
        let delta = event.position - previous;
        let [x, y] = self.delta.get();
        self.delta
            .set([x + f32::from(delta.x), y + f32::from(delta.y)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn right_drag_rotates_the_camera_and_stops_after_release() {
        let navigation = Navigation::default();
        let mut camera = Camera::default();
        let initial = camera;
        navigation.pointer.set(Some(point(px(20.), px(30.))));
        let mut hover = MouseMoveEvent {
            position: point(px(80.), px(45.)),
            pressed_button: Some(MouseButton::Right),
            ..Default::default()
        };
        navigation.drag(&hover);
        navigation.advance(&mut camera, 0.016);
        assert!(camera.yaw < initial.yaw && camera.pitch > initial.pitch);
        assert!(camera.eye().abs_diff_eq(initial.eye(), 0.0001));
        let dragged = camera.yaw;
        hover.pressed_button = None;
        navigation.drag(&hover);
        hover.pressed_button = Some(MouseButton::Right);
        hover.position.x += px(50.);
        navigation.drag(&hover);
        navigation.advance(&mut camera, 0.016);
        assert_eq!(camera.yaw, dragged);
    }
}
