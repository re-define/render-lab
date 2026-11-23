use super::*;
use crate::{
    render::{Camera, Forward, SceneExtractor, Snapshot, Technique},
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
    let mut extraction = SceneExtractor::default();
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
        let view = extraction.snapshot(&world, Camera::fit(&world)?, extent, Default::default())?;
        let mut graph = Forward::default().build(&view)?;
        let output = graph.present()?;
        if capture {
            graph.pass("Capture recording", crate::rdg::Op::HostRead(output));
        }
        let diagnostic = graph.input("Diagnostic words", vec![0x55; 16]);
        graph.pass(
            "Read diagnostic after selecting output",
            crate::rdg::Op::HostRead(diagnostic),
        );
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
    let mixed = crate::experiments::open(crate::experiments::mixed::scene()?)?;
    let snapshot = Snapshot::new(
        &mixed.world,
        mixed.camera,
        [193, 121],
        mixed.materials.clone(),
    )?;
    for _ in 0..2 {
        let mut graph = mixed.technique.build(&snapshot)?;
        graph.present()?;
        assert!(gpu.enqueue(graph.compile()?, &pool, owner.0)?.is_none());
    }
    let first = gpu.retire_next().unwrap()?;
    let second = gpu.retire_next().unwrap()?;
    assert_eq!(first.profile.frames_in_flight, 1);
    assert_eq!(second.profile.frames_in_flight, 2);
    assert!(first.profile.resident_upload_bytes > 0);
    assert_eq!(second.profile.resident_upload_bytes, 0);
    // Each outstanding slot owns its temporary targets, both reference the same resident inputs.
    let first_live = &first.memory.managed;
    let second_live = &second.memory.managed;
    let asset_bytes = |m: &crate::rhi::ManagedMemory| m.device_local.asset + m.nonlocal.asset;
    let transient_bytes =
        |m: &crate::rhi::ManagedMemory| m.device_local.transient + m.nonlocal.transient;
    assert!(asset_bytes(first_live) > 0);
    assert_eq!(asset_bytes(second_live), asset_bytes(first_live));
    assert!(transient_bytes(second_live) > transient_bytes(first_live));
    assert!(second_live.peak_device_local_bytes >= second_live.device_local.total());
    assert_eq!(second.sequence, first.sequence + 1);
    assert!(second.profile.timeline_waits > first.profile.timeline_waits);
    assert!(second.pixels.is_empty());
    verify_scratch_slots(&mut gpu, &pool, owner.0)?;
    drop(gpu);
    compare_resident_updates(owner.0, &pool)?;
    preserve_pending_capture_on_budget_failure(owner.0, &pool)
}

/// Reorder same-name owners, remove one, bring it back, grow it and then shrink its logical range.
fn verify_scratch_slots(
    backend: &mut Backend,
    pool: &rayon::ThreadPool,
    hwnd: isize,
) -> Result<()> {
    use crate::render::FrameGraph;
    let extent = [193, 121];
    let image = Storage::Image(ImageDesc {
        extent: [1, 1],
        format: ImageFormat::Rgba32Float,
    });
    let mut addresses = [[0u64; 3]; 2];
    let mut growth = [0; 2];
    for (stage, requested) in [1024, 900, 0, 700, 5000, 4096].into_iter().enumerate() {
        for frame in 0..2 {
            let mut graph = FrameGraph::new(extent);
            let value = (stage * 2 + frame + 1) as u32;
            let src = graph.input("input words", value.to_le_bytes().repeat(4));
            let a = if stage == 0 {
                Some(graph.scratch_buffer("Caster faces", 7, requested))
            } else {
                None
            };
            let b = graph.scratch_buffer("Caster faces", 9, 768);
            let a = a.or_else(|| {
                (requested > 0).then(|| graph.scratch_buffer("Caster faces", 7, requested))
            });
            if stage == 1 {
                graph.input("optional header", vec![0; 16]);
            }
            let output = graph.input(
                "pixels",
                vec![0; extent[0] as usize * extent[1] as usize * 4],
            );
            let intermediate = graph.resource("copy image", image.clone());
            let work = a.unwrap_or(b);
            graph.pass(
                "upload",
                Op::WritePixels {
                    image: intermediate,
                    buffer: src,
                },
            );
            graph.pass(
                "write scratch",
                Op::ReadPixels {
                    image: intermediate,
                    buffer: work,
                },
            );
            graph.pass(
                "read scratch",
                Op::WritePixels {
                    image: intermediate,
                    buffer: work,
                },
            );
            graph.pass(
                "copy result",
                Op::ReadPixels {
                    image: intermediate,
                    buffer: output,
                },
            );
            graph.pass(
                "result",
                Op::Counters {
                    buffer: output,
                    first: 0,
                    labels: vec!["scratch value"],
                },
            );
            graph.set_output(output);
            graph.present()?;
            assert!(backend.enqueue(graph.compile()?, pool, hwnd)?.is_none());
            let submission = backend.pending.back().unwrap();
            let index = submission.slot;
            let memory = &submission.managed;
            let purpose = memory
                .purposes
                .iter()
                .find(|p| {
                    p.name.as_ref() == "Caster faces" && p.owner == Some(7) && p.slot == Some(index)
                })
                .unwrap();
            assert_eq!(purpose.used_bytes, requested as u64);
            assert_eq!(
                purpose.unused_bytes,
                purpose.capacity_bytes - requested as u64
            );
            if stage == 0 || stage == 4 {
                growth[index] = memory.submission;
            }
            assert_eq!(purpose.last_growth_submission, Some(growth[index]));
            assert_eq!(
                purpose.frame_bindings.allocations,
                u64::from(stage == 0 || stage == 4)
            );
            let committed = memory.device_local.total() + memory.nonlocal.total();
            let binding_bytes: u64 = memory
                .purposes
                .iter()
                .map(|p| p.bindings.allocated_bytes - p.bindings.freed_bytes)
                .sum();
            assert_eq!(
                binding_bytes + memory.device_local.pool_free + memory.nonlocal.pool_free,
                committed
            );
            assert_eq!(
                memory
                    .purposes
                    .iter()
                    .map(|p| p.driver.allocations)
                    .sum::<u64>(),
                memory.activity.allocations
            );
            assert_eq!(
                memory.purposes.iter().map(|p| p.driver.frees).sum::<u64>(),
                memory.activity.frees
            );
            assert_eq!(
                memory
                    .purposes
                    .iter()
                    .map(|p| p.driver.allocated_bytes - p.driver.freed_bytes)
                    .sum::<u64>(),
                committed
            );
            let slot = &backend.slots[index];
            for (column, resource) in [a, Some(b), Some(output)].into_iter().enumerate() {
                let Some(resource) = resource else { continue };
                let buffer = slot.resources[resource.0].buffer();
                if column == 0 {
                    assert!(buffer.size >= requested as u64);
                    assert!(
                        slot.heap.slots[resource.0]
                            == Some(HeapBinding::Buffer(buffer.address, requested as u64))
                    );
                }
                if stage == 0 || (stage == 4 && column == 0) {
                    addresses[index][column] = buffer.address;
                } else {
                    assert_eq!(buffer.address, addresses[index][column]);
                }
            }
        }
        assert_ne!(addresses[0][0], addresses[1][0]);
        for frame in 0..2 {
            let result = backend.retire_next().unwrap()?;
            assert_eq!(
                result.counters["scratch value"],
                (2 * stage + frame + 1) as u64
            );
        }
    }
    Ok(())
}

/// Budget exhaustion while preparing the next slot must leave a submitted recording retrievable.
fn preserve_pending_capture_on_budget_failure(hwnd: isize, pool: &rayon::ThreadPool) -> Result<()> {
    let mut backend = Backend::open()?;
    let world = World::demo();
    let camera = Camera::fit(&world)?;
    let mut extractor = SceneExtractor::default();
    let mut plan = |extent| {
        let view = extractor.snapshot(&world, camera, extent, Default::default())?;
        let mut graph = Forward::default().build(&view)?;
        let output = graph.present()?;
        graph.pass("Capture recording", crate::rdg::Op::HostRead(output));
        graph.compile()
    };

    let original = [193, 121];
    let reference = backend.execute_to(plan(original)?, pool, Some(hwnd))?;
    assert!(reference.counters["PBR pixels"] > 0 && reference.counters["Toon pixels"] > 0);
    let larger = [512, 384];
    let mut sequence = reference.sequence;
    for submitted in [1, 2] {
        for _ in 0..submitted {
            assert!(backend.enqueue(plan(original)?, pool, hwnd)?.is_none());
        }
        let cap = backend
            .gpu
            .allocator
            .lock()
            .unwrap()
            .stats
            .device_local
            .total();
        backend.set_device_local_budget(Some(cap))?;
        // At depth two, enqueue retires slot 0 before allocation fails; slot 1 is still in flight.
        let rejected = backend
            .enqueue(plan(larger)?, pool, hwnd)
            .err()
            .context("expected larger attachments to exceed budget")?;
        assert!(
            format!("{rejected:#}").contains("device-local budget exceeded"),
            "{rejected:#}"
        );
        for _ in 0..submitted {
            let pending = backend
                .retire_next()
                .context("budget rejection discarded submitted capture")??;
            sequence += 1;
            assert_eq!(pending.sequence, sequence);
            assert_eq!(pending.extent, original);
            assert_eq!(pending.pixels.len(), 193 * 121 * 4);
            assert_eq!(pending.pixels, reference.pixels);
            assert_eq!(pending.counters, reference.counters);
        }
        assert!(backend.retire_next().is_none());
        backend.set_device_local_budget(None)?;
    }

    unsafe {
        ensure!(
            SetWindowPos(
                hwnd as _,
                std::ptr::null_mut(),
                0,
                0,
                larger[0] as _,
                larger[1] as _,
                SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER
            ) != 0,
            "could not resize test viewport"
        );
    }
    assert!(backend.enqueue(plan(larger)?, pool, hwnd)?.is_none());
    let resumed = backend
        .retire_next()
        .context("missing recovered capture")??;
    assert_eq!(resumed.sequence, sequence + 1);
    assert_eq!(resumed.extent, larger);
    assert_eq!(resumed.pixels.len(), 512 * 384 * 4);
    assert!(resumed.counters["PBR pixels"] > 0 && resumed.counters["Toon pixels"] > 0);
    Ok(())
}

#[test]
#[ignore = "requires a Vulkan device"]
fn recovers_after_hard_device_local_budget_is_exceeded() -> Result<()> {
    let mut backend = Backend::open()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let world = World::demo();
    let mut extractor = SceneExtractor::default();
    let mut plan = |extent| {
        let view = extractor.snapshot(&world, Camera::fit(&world)?, extent, Default::default())?;
        Forward::default().build(&view)?.compile()
    };
    let initial = backend.execute(plan([32, 24])?, &pool)?;
    let cap = initial.memory.managed.device_local.total();
    assert!(cap > 0);
    backend.set_device_local_budget(Some(cap))?;
    // Larger depth and visibility attachments require device-local memory, even with host spill.
    let extent = [512, 384];
    let limited = backend.execute(plan(extent)?, &pool);
    let error = limited
        .err()
        .expect("device-local limit must reject resized attachments");
    assert!(
        format!("{error:#}").contains("device-local budget exceeded"),
        "{error:#}"
    );
    backend.set_device_local_budget(None)?;
    let recovered = backend.execute(plan(extent)?, &pool)?;
    assert_eq!(recovered.pixels.len(), 512 * 384 * 4);
    assert!(recovered.counters["PBR pixels"] > 0 && recovered.counters["Toon pixels"] > 0);
    assert!(recovered.memory.managed.device_local.total() > cap);
    assert_eq!(recovered.memory.managed.budget_bytes, None);
    Ok(())
}

/// Keep two real WSI submissions outstanding. Extract the intermediate transform without
/// rendering it; B's matrix and C's changed material must both survive when the slot is reused.
fn compare_resident_updates(hwnd: isize, pool: &rayon::ThreadPool) -> Result<()> {
    use crate::{
        experiments::clusters::{self, ClusterLod},
        render::post::{Mode, Sequence, Settings},
        scene::Shading,
    };
    let mut initial = World::demo();
    clusters::prepare(&mut initial)?;
    let camera = Camera::fit(&initial)?;
    let (&rigid, _) = initial
        .entities
        .iter()
        .find(|(_, e)| e.instance.as_ref().is_some_and(|i| !i.outline))
        .unwrap();
    let material = initial.entities[&rigid]
        .instance
        .as_ref()
        .unwrap()
        .material
        .unwrap();
    let start = initial.entities[&rigid].local;
    let mut reference = vec![];
    for full in [true, false] {
        let mut device = Backend::open()?;
        let technique: Arc<dyn Technique> = Arc::new(if full {
            ClusterLod::full_upload_reference()
        } else {
            ClusterLod::default()
        });
        let mut extraction = SceneExtractor::default();
        let mut sequence = Sequence::default();
        let mut settings = Settings::default();
        settings.select(Mode::Taa);
        let mut world = initial.clone();
        let programs = Arc::default();
        let mut images = vec![];
        for step in 0..6 {
            if step == 2 {
                world.set_local(
                    rigid,
                    glam::Mat4::from_translation(glam::Vec3::X * 0.55) * start,
                )?;
                let _skipped =
                    extraction.snapshot(&world, camera, [193, 121], Arc::clone(&programs))?;
                let mut m = world.entities[&material].material.clone().unwrap();
                m.shading = Shading::Toon;
                m.base_color = [0.13, 0.95, 0.14, 1.];
                world.set_material(material, m)?;
            }
            if step == 4 {
                world.set_local(rigid, start)?;
            }
            let mut snapshot =
                extraction.snapshot(&world, camera, [193, 121], Arc::clone(&programs))?;
            sequence.assign(settings, &technique, &mut snapshot);
            if step == 2 {
                assert!(snapshot.temporal.as_ref().unwrap().moved(rigid));
            }
            let mut g = technique.build(&snapshot)?;
            let output = g.present()?;
            g.pass("Capture resident updates", crate::rdg::Op::HostRead(output));
            assert!(device.enqueue(g.compile()?, pool, hwnd)?.is_none());
            if step % 2 == 1 {
                for _ in 0..2 {
                    let f = device.retire_next().unwrap()?;
                    if step >= 3 {
                        assert_eq!(f.counters["PBR pixels"], 0);
                    }
                    images.push(f.pixels);
                }
            }
        }
        assert_ne!(images[1], images[2]);
        if full {
            reference = images;
        } else {
            for (i, (a, b)) in images.iter().zip(&reference).enumerate() {
                assert_eq!(a.len(), 193 * 121 * 4);
                let error: u64 = a
                    .iter()
                    .zip(b)
                    .map(|(a, b)| u64::from(a.abs_diff(*b)))
                    .sum();
                assert!(error as f64 / (a.len() as f64) < 0.01, "frame {i}: {error}");
            }
        }
    }
    Ok(())
}
