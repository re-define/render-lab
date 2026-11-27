//! Offscreen replay of the normal interactive forest-stream scene. CSV columns are per completed frame.
use anyhow::{Context, Result, ensure};
use glam::Vec3;
use render_lab::{
    engine::Engine,
    experiments::{self, vegetation::streaming},
    render::Camera,
    rhi::Frame,
};
use std::{
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

const EXTENT: [u32; 2] = [960, 540];
fn camera(frame: u32) -> (&'static str, Camera) {
    let mut view = Camera {
        target: Vec3::new(22., 3., 1.),
        yaw: 0.13,
        pitch: 0.05,
        distance: 12.,
    };
    let phase = match frame {
        0..60 => {
            view.distance = 20. - 8. * (frame.min(36) as f32 / 36.);
            "cold"
        }
        60..156 => {
            let turn = (frame - 60) % 32;
            if turn >= 16 {
                view.look(std::f32::consts::PI, 0.);
            }
            "return"
        }
        156..178 => {
            view.target.x = 10.;
            view.distance = 8.;
            "overlap"
        }
        178..212 => {
            view.target.x = -1.;
            "teleport"
        }
        _ => {
            view.target.x = 250.;
            "unload"
        }
    };
    (phase, view)
}
fn next(e: &mut Engine) -> Result<Frame> {
    ensure!(
        e.tick(1. / 60., EXTENT)?,
        "offscreen queue admission failed"
    );
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(frame) = e.take_frame() {
            return frame.map_err(anyhow::Error::msg);
        }
        ensure!(
            Instant::now() < deadline,
            "timeout waiting for forest readback"
        );
        std::thread::sleep(Duration::from_micros(250));
    }
}
fn pct(values: &mut [f64], n: usize) -> f64 {
    if values.is_empty() {
        return 0.;
    }
    let index = (values.len() * n).div_ceil(100) - 1;
    *values
        .select_nth_unstable_by(index, |a, b| a.total_cmp(b))
        .1
}
fn replay(run: usize, cache: u64, file: &mut BufWriter<std::fs::File>, dir: &Path) -> Result<()> {
    let mut e = experiments::open(streaming::scene_with_cache_frames(cache)?)?;
    e.set_ordered_completions(true);
    let mut gpu = Vec::new();
    let mut wall = Vec::new();
    let mut bytes = 0;
    let mut peak_pages = 0;
    let mut peak_blocks = 0;
    let mut direct_uploads = 0;
    let mut host_uploads = 0;
    let mut peak_roots = 0;
    let mut peak_retiring = 0;
    let mut peak_staging = 0;
    let mut rereads = 0;
    let mut reread_bytes = 0;
    let mut saw_shared_exit = false;
    let mut saw_preload_only = false;
    let mut fine_bark = false;
    let mut fine_canopy = false;
    let mut confirmed_unload = false;
    let mut max_active = 0;
    let mut paged = false;
    let mut previous_phase = "";
    let mut use_p95 = 0;
    for i in 0..260 {
        let (phase, view) = camera(i);
        e.camera = view;
        if previous_phase != phase && !previous_phase.is_empty() {
            println!(
                "{previous_phase} gpu_p99_ms={:.3} tick_to_completion_p99_ms={:.3}",
                pct(&mut gpu, 99),
                pct(&mut wall, 99)
            );
            gpu.clear();
            wall.clear();
        }
        previous_phase = phase;
        let frame = next(&mut e)?;
        let k = |name| frame.counters.get(name).copied().unwrap_or(0);
        let gpu_ms = frame
            .profile
            .gpu_span_ms
            .context("GPU timestamp unavailable")?;
        let wall_ms = frame
            .profile
            .tick_to_completion_ms
            .context("CPU completion unavailable")?;
        let mem = &frame.memory.managed;
        direct_uploads += k("stream DirectStorage uploads");
        host_uploads += k("stream host uploads");
        let live = mem.device_local.stream_page;
        let blocks = mem.stream_pool_bytes;
        bytes = bytes.max(k("forest requested bytes"));
        rereads = rereads.max(k("forest rereads"));
        if k("oak cache active") != 0 {
            use_p95 = k("forest first use p95 us");
        }
        peak_pages = peak_pages.max(live);
        peak_blocks = peak_blocks.max(blocks);
        peak_roots = peak_roots.max(mem.stream_root_bytes);
        peak_retiring = peak_retiring.max(mem.stream_retiring_bytes);
        peak_staging = peak_staging.max(mem.stream_staging_bytes);
        let instances = k("forest oak instances");
        if instances == 12 && max_active == 24 {
            saw_shared_exit = true;
        }
        if instances == 0 && k("oak cache active") != 0 && k("forest reads") > 0 {
            saw_preload_only = true;
        }
        max_active = max_active.max(instances);
        paged |= k("forest nonempty draws") >= 2;
        reread_bytes = reread_bytes.max(k("forest reread bytes"));
        fine_bark |= k("fine bark pixels") > 0;
        fine_canopy |= k("resident canopy groups") > 0 && k("canopy triangles") > 0;
        if i == 259 {
            confirmed_unload = live == 0
                && blocks == 0
                && instances == 0
                && mem.stream_root_bytes == 0
                && mem.stream_retiring_bytes == 0
                && mem.stream_staging_bytes == 0;
        }
        gpu.push(gpu_ms);
        wall.push(wall_ms);
        writeln!(
            file,
            "{run},{cache},{phase},{i},{gpu_ms:.4},{wall_ms:.4},{instances},{},{},{},{},{},{},{},{},{live},{blocks},{},{},{},{},{}",
            k("canopy triangles"),
            k("resident canopy groups"),
            k("resident bark tiles"),
            k("fine bark pixels"),
            k("forest page uploads"),
            k("forest reads"),
            k("forest rereads"),
            k("forest requested bytes"),
            k("forest first use p95 us"),
            k("forest reread bytes"),
            mem.stream_root_bytes,
            mem.stream_retiring_bytes,
            mem.stream_staging_bytes
        )?;
        if cache != 0 && i == 57 {
            render_lab::recording::save_capture(
                dir.join("forest-stream-near.png"),
                EXTENT,
                frame.pixels,
            )?;
        }
    }
    println!(
        "{previous_phase} gpu_p99_ms={:.3} tick_to_completion_p99_ms={:.3}",
        pct(&mut gpu, 99),
        pct(&mut wall, 99)
    );
    println!(
        "run={run}, retention={cache} frames, requested={bytes} bytes, repeat_reads={rereads}, reread_bytes={reread_bytes}, request_to_usable_p95_ms={:.3}, peak_bound={peak_pages}, peak_blocks={peak_blocks}, peak_roots={peak_roots}, peak_retiring={peak_retiring}, peak_staging={peak_staging}, direct_uploads={direct_uploads}, host_uploads={host_uploads}, final_zero={confirmed_unload}, max_instances={max_active}",
        use_p95 as f64 / 1000.
    );
    ensure!(
        fine_bark
            && fine_canopy
            && confirmed_unload
            && max_active == 24
            && saw_shared_exit
            && saw_preload_only
            && paged,
        "missing streamed bark/canopy, world activation or retirement"
    );
    Ok(())
}
fn main() -> Result<()> {
    let dir = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "target/forest-stream-study".into());
    std::fs::create_dir_all(&dir)?;
    let mut file = BufWriter::new(std::fs::File::create(dir.join("forest-stream.csv"))?);
    writeln!(
        file,
        "run,cache_frames,phase,frame,gpu_ms,tick_to_completion_ms,oak_instances,canopy_triangles,resident_groups,resident_tiles,fine_bark_pixels,uploads,reads,rereads,requested_bytes,live_page_bytes,pool_block_bytes,request_to_use_p95_us,reread_bytes,root_bytes,retiring_bytes,staging_bytes"
    )?;
    // Both passes start with an empty engine page cache. The OS file cache and Vulkan driver cache may be warm.
    for (run, cache) in [0, 75, 75, 0].into_iter().enumerate() {
        replay(run, cache, &mut file, &dir)?;
    }
    file.flush()?;
    Ok(())
}
