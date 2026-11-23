use super::*;
use anyhow::{Result, ensure};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, rc::Rc};
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::{BeginPaint, ClientToScreen, EndPaint, PAINTSTRUCT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const CLASS: *const u16 = windows_sys::w!("RenderLabVulkanViewport");

unsafe extern "system" fn viewport_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    unsafe {
        match message {
            WM_NCHITTEST => HTTRANSPARENT as isize,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut region = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut region);
                EndPaint(hwnd, &region);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe extern "system" fn hud_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    id: usize,
    _: usize,
) -> isize {
    unsafe {
        match message {
            WM_NCHITTEST => return HTTRANSPARENT as isize,
            WM_MOUSEACTIVATE => return MA_NOACTIVATE as isize,
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(hud_proc), id);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
pub(super) struct Viewport {
    pub hwnd: isize,
    owner: isize,
    rect: Cell<[i32; 4]>,
    overlay: Cell<Option<isize>>,
    overlay_rect: Cell<[i32; 4]>,
}
impl Viewport {
    pub fn new(window: &Window) -> Result<Rc<Self>> {
        let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .as_raw()
        else {
            anyhow::bail!("Win32 window required")
        };
        Self::owned(raw.hwnd.get())
    }

    fn owned(owner: isize) -> Result<Rc<Self>> {
        unsafe {
            // GPUI's DirectComposition visual sits above child HWNDs. An owned
            // popup stays above that visual and follows the studio window.
            let module = GetModuleHandleW(std::ptr::null());
            static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            ensure!(
                *REGISTERED.get_or_init(|| {
                    RegisterClassW(&WNDCLASSW {
                        lpfnWndProc: Some(viewport_proc),
                        hInstance: module,
                        lpszClassName: CLASS,
                        ..Default::default()
                    }) != 0
                }),
                "register Vulkan viewport class"
            );
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                CLASS,
                windows_sys::w!("Render Lab Viewport"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner as _,
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
                owner,
                rect: Cell::new([0; 4]),
                overlay: Cell::new(None),
                overlay_rect: Cell::new([0; 4]),
            }))
        }
    }
    pub fn attach_overlay(&self, window: &Window) -> Result<()> {
        let RawWindowHandle::Win32(raw) = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .as_raw()
        else {
            anyhow::bail!("Win32 HUD window required")
        };
        self.attach_overlay_hwnd(raw.hwnd.get())
    }

    fn attach_overlay_hwnd(&self, handle: isize) -> Result<()> {
        let hwnd = handle as HWND;
        unsafe {
            let extended = GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
                | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, extended);
            SetWindowLongPtrW(hwnd, GWL_STYLE, WS_POPUP as isize);
            ensure!(
                SetWindowSubclass(hwnd, Some(hud_proc), 0, 0) != 0,
                "install HUD pointer pass-through"
            );
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, self.hwnd);
            ensure!(
                GetWindow(hwnd, GW_OWNER) == self.hwnd as _,
                "set HUD window owner"
            );
        }
        self.overlay.set(Some(handle));
        Ok(())
    }

    /// GPUI owns the HUD. Release the Win32 owner link before destroying its viewport.
    pub fn detach_overlay(&self) {
        if let Some(handle) = self.overlay.take() {
            let hwnd = handle as HWND;
            unsafe {
                if GetWindow(hwnd, GW_OWNER) == self.hwnd as HWND {
                    SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0);
                    RemoveWindowSubclass(hwnd, Some(hud_proc), 0);
                }
            }
        }
    }

    pub fn element(
        self: &Rc<Self>,
        navigation: Option<&Rc<input::Navigation>>,
        show_hud: bool,
    ) -> impl IntoElement + use<> {
        let child = self.clone();
        let navigation = navigation.cloned();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                if let Some(navigation) = &navigation {
                    navigation.paint(bounds, window);
                }
                let px = |value: Pixels| (f32::from(value) * window.scale_factor()).round() as i32;
                child.place(
                    POINT {
                        x: px(bounds.origin.x),
                        y: px(bounds.origin.y),
                    },
                    [px(bounds.size.width), px(bounds.size.height)],
                );
                child.place_overlay(if show_hud { child.rect.get() } else { [0; 4] });
            },
        )
        .flex_1()
        .h_full()
        .min_w_0()
    }
    fn place(&self, mut origin: POINT, [w, h]: [i32; 2]) {
        unsafe {
            let owner = self.owner as _;
            let hwnd = self.hwnd as _;
            if w <= 0 || h <= 0 || IsIconic(owner) != 0 {
                if self.rect.replace([0; 4]) != [0; 4] {
                    ShowWindow(hwnd, SW_HIDE);
                }
                return;
            }
            if ClientToScreen(owner, &mut origin) == 0 {
                return;
            }
            let rect = [origin.x, origin.y, w, h];
            if self.rect.get() != rect
                && SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    origin.x,
                    origin.y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                ) != 0
            {
                self.rect.set(rect);
            }
        }
    }
    pub fn extent(&self) -> Option<[u32; 2]> {
        let [_, _, w, h] = self.rect.get();
        (w > 0 && h > 0).then_some([w as u32, h as u32])
    }

    fn place_overlay(&self, rect: [i32; 4]) {
        let Some(hwnd) = self.overlay.get() else {
            return;
        };
        if self.overlay_rect.get() == rect {
            return;
        }
        unsafe {
            if rect == [0; 4] {
                ShowWindow(hwnd as _, SW_HIDE);
                self.overlay_rect.set(rect);
            } else if SetWindowPos(
                hwnd as _,
                std::ptr::null_mut(),
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                SWP_NOACTIVATE
                    | SWP_NOZORDER
                    | SWP_NOOWNERZORDER
                    | SWP_SHOWWINDOW
                    | SWP_FRAMECHANGED,
            ) != 0
            {
                self.overlay_rect.set(rect);
            }
        }
    }
}
impl Drop for Viewport {
    fn drop(&mut self) {
        self.detach_overlay();
        unsafe {
            DestroyWindow(self.hwnd as _);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HWND, Viewport};
    use anyhow::{Result, ensure};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    #[test]
    fn viewport_and_hud_pass_through_mouse_hits_without_disabling_windows() -> Result<()> {
        struct TestWindow(HWND);
        impl TestWindow {
            fn open() -> Result<Self> {
                let hwnd = unsafe {
                    CreateWindowExW(
                        0,
                        windows_sys::w!("STATIC"),
                        std::ptr::null(),
                        WS_POPUP,
                        0,
                        0,
                        32,
                        32,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        std::ptr::null_mut(),
                    )
                };
                ensure!(!hwnd.is_null(), "create test window");
                Ok(Self(hwnd))
            }
        }
        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }

        let owner = TestWindow::open()?;
        let viewport = Viewport::owned(owner.0 as isize)?;
        let hud = TestWindow::open()?;
        viewport.attach_overlay_hwnd(hud.0 as isize)?;
        for hwnd in [viewport.hwnd as HWND, hud.0] {
            unsafe {
                assert_eq!(GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_DISABLED as isize, 0);
                assert_eq!(
                    SendMessageW(hwnd, WM_NCHITTEST, 0, 0),
                    HTTRANSPARENT as isize
                );
                assert_eq!(
                    SendMessageW(hwnd, WM_MOUSEACTIVATE, 0, 0),
                    MA_NOACTIVATE as isize
                );
            }
        }
        viewport.detach_overlay();
        drop(viewport);
        unsafe {
            assert_ne!(IsWindow(hud.0), 0);
            assert!(GetWindow(hud.0, GW_OWNER).is_null());
        }
        Ok(())
    }
}
