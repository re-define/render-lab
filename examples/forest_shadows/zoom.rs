//! Replay the karst W/S path on a real swapchain, with wind and two independent frame slots.
use anyhow::{Context, Result, ensure};
use render_lab::{
    console::Console,
    experiments,
    render::Camera,
    rhi::{AllocationActivity, PurposeMemory},
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};
use windows_sys::Win32::{System::LibraryLoader::GetModuleHandleW, UI::WindowsAndMessaging::*};

const FRAMES: u64 = 420;

struct Viewport(isize);
impl Drop for Viewport {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0 as _);
        }
    }
}

fn path(frame: u64) -> (&'static str, f32) {
    match frame {
        0..48 => ("warmup", 0.),
        48..96 => ("overview", 0.),
        96..168 => ("forward", (frame - 95) as f32),
        168..216 => ("close", 72.),
        216..288 => ("back", (287 - frame) as f32),
        288..336 => ("returned", 0.),
        336..372 => ("back_farther", -((frame - 335) as f32)),
        _ => ("distant", -36.),
    }
}

pub(super) fn camera_at(mut origin: Camera, steps: f32) -> Camera {
    origin.translate(0., steps.signum(), steps.abs() / 60.);
    origin
}

#[derive(serde::Serialize)]
struct PurposeFrame<'a> {
    submission: u64,
    stage: &'static str,
    full: bool,
    purposes: Vec<&'a PurposeMemory>,
}

pub(super) fn run(
    directory: &Path,
    laps: u32,
    trace: bool,
    extent: [u32; 2],
    raster_backend: &str,
    ray_backend: Option<&str>,
    gi: Option<&str>,
) -> Result<()> {
    ensure!(laps > 0, "at least one camera lap is required");
    std::fs::create_dir_all(directory)?;
    let mut csv = BufWriter::new(File::create(directory.join("zoom.csv"))?);
    let mut details = if trace {
        Some(BufWriter::new(File::create(
            directory.join("memory-purposes.jsonl"),
        )?))
    } else {
        None
    };
    writeln!(
        csv,
        "lap,frame,stage,forward_steps,interval_ms,gpu_ms,rhi_prepare_ms,shadow_ms,main_clusters,detailed_instances,detailed_triangles,coarse_triangles,allocations,allocated_bytes,frees,freed_bytes,device_local_bytes,peak_device_local_bytes,suballocations,suballocated_bytes,returned_ranges,returned_range_bytes,pool_blocks,local_pool_free_bytes,nonlocal_pool_free_bytes,raster_ms,voxel_raster_ms,voxel_bins_ms,voxel_clusters,visible_bricks,proxy_prepare_ms,software_bvh_ms,blas_ms,tlas_ms,as_builds,as_updates,shadow_backend,gi_trace,gi_trace_ms,gi_cache_ms,gi_probe_ms,gi_resolve_ms,gi_total_ms"
    )?;
    // This thread owns and pumps the hidden window until the engine and swapchain are dropped.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            extent[0] as i32,
            extent[1] as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    ensure!(!hwnd.is_null(), "could not create the benchmark viewport");
    let window = Viewport(hwnd as isize);
    let mut engine = experiments::open(experiments::restir::karst_study()?)?;
    let mut console = Console::default();
    for command in [
        "post taa".into(),
        format!("vegetation raster {raster_backend}"),
    ] {
        console.execute(&mut engine, &[], &command)?;
    }
    if let Some(mode) = ray_backend {
        console.execute(&mut engine, &[], &format!("restir rays {mode}"))?;
    }
    if let Some(mode) = gi {
        console.execute(&mut engine, &[], &format!("restir gi {mode}"))?;
    }
    unsafe {
        engine.set_viewport_window(window.0);
    }
    let origin = engine.camera;
    let total = u64::from(laps) * FRAMES;
    let mut queued = 0;
    let mut seen = 0;
    let mut retry = false;
    let mut last_completion = None;
    let mut activity = AllocationActivity::default();
    let mut suballocations = AllocationActivity::default();
    let mut progress = Instant::now();
    while seen < total {
        unsafe {
            let mut message = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if queued < total && queued < seen + 2 {
            let (_, steps) = path(queued % FRAMES);
            engine.camera = camera_at(origin, steps);
            // A full queue still advances the simulation. Retry the same snapshot at dt = 0.
            if engine.tick(if retry { 0. } else { 1. / 60. }, extent)? {
                queued += 1;
                retry = false;
            } else {
                retry = true;
            }
        }
        let Some(frame) = engine.take_frame() else {
            ensure!(
                progress.elapsed() < Duration::from_secs(90),
                "timed out at frame {seen}"
            );
            std::thread::sleep(Duration::from_micros(200));
            continue;
        };
        let frame = frame.map_err(anyhow::Error::msg)?;
        ensure!(
            frame.sequence == seen + 1
                && frame.extent == extent
                && frame.profile.wsi_binary_pairs > 0
                && frame.pixels.is_empty(),
            "expected consecutive presented frames without readback, got frame {}",
            frame.sequence
        );
        if seen == 0 {
            println!(
                "{} · {:?} · {} × {}",
                frame.gpu, frame.presentation, extent[0], extent[1]
            );
        }
        let (stage, steps) = path(seen % FRAMES);
        let interval = last_completion.replace(frame.completed).map_or(0., |prev| {
            frame.completed.duration_since(prev).as_secs_f64() * 1000.
        });
        let gpu = frame
            .profile
            .gpu_span_ms
            .context("GPU needs a common timestamp clock")?;
        let query = frame
            .passes
            .iter()
            .find(|p| {
                matches!(
                    p.name.as_str(),
                    "Trace hardware proxy shadow rays" | "Trace one shadow ray per sampled pixel"
                )
            })
            .context("missing forest shadow pass")?;
        let shadow = query.ms;
        let backend = if query.name == "Trace hardware proxy shadow rays" {
            "hardware"
        } else {
            "software"
        };
        let duration = |names: &[&str]| {
            frame
                .passes
                .iter()
                .filter(|p| names.contains(&p.name.as_str()))
                .map(|p| p.ms)
                .sum::<f64>()
        };
        let prepare = duration(&[
            "Deform detailed vegetation shadow triangles",
            "Deform offscreen and visible ray proxies",
            "Gather shared ray triangles",
            "Pack native ray indices",
        ]);
        let refit = duration(&["Bound deformed triangles", "Refit ray BVH level"]);
        let blas = duration(&["Build triangle BLAS"]);
        let tlas = duration(&["Build scene TLAS"]);
        let trace_pass = frame
            .passes
            .iter()
            .find(|p| p.name.starts_with("GI-1.2") && p.name.contains("probe paths"));
        let trace_kind = trace_pass.map_or("off", |p| p.name.as_str());
        let gi_trace = trace_pass.map_or(0., |p| p.ms);
        let gi_cache = duration(&[
            "GI-1.2 clear samples and expire stale cells",
            "GI-1.2 resolve direct and delayed second bounce",
        ]);
        let gi_probe = duration(&[
            "GI-1.2 integrate probes and transfer second hits",
            "GI-1.2 store probe history",
        ]);
        let gi_resolve = duration(&["GI-1.2 interpolate indirect diffuse"]);
        let gi_total = gi_trace + gi_cache + gi_probe + gi_resolve;
        let memory = frame.memory.managed;
        let driver = memory.activity.since(&activity);
        let ranges = memory.suballocations.since(&suballocations);
        let c = &frame.counters;
        let raster: f64 = frame
            .passes
            .iter()
            .filter(|p| p.phase == render_lab::rdg::PassPhase::Raster)
            .map(|p| p.ms)
            .sum();
        let mut voxel_ms = 0.;
        let mut bins_ms = 0.;
        for pass in &frame.passes {
            if pass.name.contains("voxel brick visibility") {
                if pass.name.ends_with("depth bins") {
                    bins_ms += pass.ms;
                } else {
                    voxel_ms += pass.ms;
                }
            }
        }
        writeln!(
            csv,
            "{},{},{stage},{steps},{interval:.5},{gpu:.5},{:.5},{shadow:.5},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{raster:.5},{voxel_ms:.5},{bins_ms:.5},{},{},{prepare:.5},{refit:.5},{blas:.5},{tlas:.5},{},{},{backend},{trace_kind},{gi_trace:.5},{gi_cache:.5},{gi_probe:.5},{gi_resolve:.5},{gi_total:.5}",
            seen / FRAMES + 1,
            seen % FRAMES + 1,
            frame.profile.rhi_prepare_ms,
            c["Main clusters"],
            c["Detailed shadow instances"],
            c["Detailed shadow triangles"],
            c["Coarse shadow triangles"],
            driver.allocations,
            driver.allocated_bytes,
            driver.frees,
            driver.freed_bytes,
            memory.device_local.total(),
            memory.peak_device_local_bytes,
            ranges.allocations,
            ranges.allocated_bytes,
            ranges.frees,
            ranges.freed_bytes,
            memory.pool_blocks,
            memory.device_local.pool_free,
            memory.nonlocal.pool_free,
            c["Voxel plant clusters"],
            c.get("Visible voxel bricks").copied().unwrap_or(0),
            frame.profile.acceleration_builds,
            frame.profile.acceleration_updates
        )?;
        if let Some(writer) = &mut details {
            let full = path((seen + 1) % FRAMES).0 != stage;
            let purposes = memory
                .purposes
                .iter()
                .filter(|p| full || !p.frame_bindings.is_empty() || !p.frame_driver.is_empty())
                .collect();
            serde_json::to_writer(
                &mut *writer,
                &PurposeFrame {
                    submission: memory.submission,
                    stage,
                    full,
                    purposes,
                },
            )?;
            writer.write_all(b"\n")?;
        }
        activity = memory.activity;
        suballocations = memory.suballocations;
        seen += 1;
        progress = Instant::now();
    }
    csv.flush()?;
    if let Some(mut writer) = details {
        writer.flush()?;
    }
    Ok(())
}
