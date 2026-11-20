use super::*;
use anyhow::{Result, ensure};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, rc::Rc};
use windows_sys::Win32::{
    Foundation::{POINT, RECT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RBUTTON},
        WindowsAndMessaging::*,
    },
};
pub(super) struct Viewport {
    pub hwnd: isize,
    rect: Cell<[i32; 4]>,
    anchor: Cell<Option<(i32, i32)>>,
}
impl Viewport {
    pub fn new(window: &Window) -> Result<Rc<Self>> {
        unsafe {
            let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .as_raw()
            else {
                anyhow::bail!("Win32 window required")
            };
            let hwnd = CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_DISABLED | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                raw.hwnd.get() as _,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            ensure!(
                !hwnd.is_null(),
                "create viewport: {}",
                std::io::Error::last_os_error()
            );
            Ok(Rc::new(Self {
                hwnd: hwnd as isize,
                rect: Cell::new([0; 4]),
                anchor: Cell::new(None),
            }))
        }
    }
    pub fn element(self: &Rc<Self>) -> impl IntoElement {
        let child = self.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let px = |value: Pixels| (f32::from(value) * window.scale_factor()).round() as i32;
                let rect = [
                    px(bounds.origin.x),
                    px(bounds.origin.y),
                    px(bounds.size.width),
                    px(bounds.size.height),
                ];
                if child.rect.replace(rect) != rect {
                    unsafe {
                        SetWindowPos(
                            child.hwnd as _,
                            std::ptr::null_mut(),
                            rect[0],
                            rect[1],
                            rect[2],
                            rect[3],
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                }
            },
        )
        .flex_1()
        .h_full()
        .min_w_0()
    }
    pub fn extent(&self) -> Option<[u32; 2]> {
        let [_, _, w, h] = self.rect.get();
        (w > 0 && h > 0).then_some([w as u32, h as u32])
    }
    pub fn navigate(&self, cam: &mut render_lab::render::Camera, dt: f32, enabled: bool) {
        unsafe {
            let child = self.hwnd as _;
            let parent = GetAncestor(child, GA_ROOT);
            if !enabled || GetForegroundWindow() != parent {
                self.anchor.set(None);
                return;
            }
            let down = |key: i32| GetAsyncKeyState(key) < 0;
            let side = i32::from(down(68)) - i32::from(down(65));
            let forward = i32::from(down(87)) - i32::from(down(83));
            cam.translate(side as f32, forward as f32, dt);
            let mut pt = POINT::default();
            let mut r = RECT::default();
            GetCursorPos(&mut pt);
            GetWindowRect(child, &mut r);
            if down(VK_RBUTTON as i32) {
                if let Some((x, y)) = self.anchor.get() {
                    cam.look((pt.x - x) as f32 * 0.003, (pt.y - y) as f32 * 0.003);
                    if pt.x != x || pt.y != y {
                        SetCursorPos(x, y);
                    }
                } else if pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom {
                    self.anchor.set(Some((pt.x, pt.y)));
                }
            } else {
                self.anchor.set(None);
            }
        }
    }
}
impl Drop for Viewport {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd as _);
        }
    }
}
