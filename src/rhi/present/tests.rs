use super::*;
use crate::{
    render::{Camera, Forward, Snapshot, Technique},
    scene::World,
};
use windows_sys::Win32::{System::LibraryLoader::GetModuleHandleW, UI::WindowsAndMessaging::*};
#[test]
#[ignore = "requires Win32 Vulkan WSI"]
fn presents_and_resizes_with_optional_frame_capture() -> Result<()> {
    let window = unsafe {
        CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            160,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    ensure!(!window.is_null(), "hidden test window unavailable");
    struct Owner(isize);
    impl Drop for Owner {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0 as _);
            }
        }
    }
    let owner = Owner(window as isize);
    let mut gpu = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let world = Arc::new(World::demo());
    for (extent, capture) in [([160, 100], false), ([193, 121], true), ([193, 121], false)] {
        unsafe {
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                0,
                0,
                extent[0] as _,
                extent[1] as _,
                SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let mut graph = Forward::default().build(&Snapshot {
            materials: Default::default(),
            world: world.clone(),
            camera: Camera::fit(&world)?,
            extent,
        })?;
        let output = graph.present()?;
        if capture {
            graph.pass("Capture recording", crate::rdg::Op::HostRead(output));
        }
        let frame = gpu.execute_to(graph.compile()?, &pool, Some(owner.0))?;
        let surface = gpu.surface.as_ref().unwrap();
        assert_eq!(surface.extent, extent);
        assert!(!surface.chain.is_null());
        if capture {
            assert_eq!(frame.pixels.len(), (extent[0] * extent[1] * 4) as usize);
            assert!(
                frame
                    .pixels
                    .chunks_exact(4)
                    .any(|p| p != &frame.pixels[..4])
            );
        } else {
            assert!(frame.pixels.is_empty());
        }
        assert_eq!(frame.profile.wsi_transitions, 2);
        assert!(frame.profile.resource_barriers >= frame.profile.barrier_calls);
        assert_eq!(frame.profile.timeline_signals, frame.submissions);
        assert!(frame.profile.recording_workers > 0);
        if frame.profile.physical_queues > 1 {
            assert!(frame.profile.timeline_waits > 0);
        }

        let modes = unsafe {
            gpu.gpu
                .instance()
                .get_physical_device_surface_present_modes_khr(gpu.gpu.physical, surface.surface)?
        };
        assert_eq!(
            surface.mode,
            if modes.contains(&vk::PresentModeKHR::MAILBOX) {
                vk::PresentModeKHR::MAILBOX
            } else {
                vk::PresentModeKHR::FIFO
            }
        );
        assert!(frame.counters["PBR pixels"] > 0 && frame.counters["Toon pixels"] > 0);
        println!(
            "presented {extent:?}, {:?}, {} VkQueues",
            surface.mode,
            gpu.gpu.queues.len()
        );
    }
    Ok(())
}
