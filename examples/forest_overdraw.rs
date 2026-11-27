//! Replay real visibility draws against scratch attachments to measure foliage overdraw.
use anyhow::{Context, Result, ensure};
use render_lab::{console::Console, experiments, rdg::*, render::SceneExtractor, rhi::Backend};
macro_rules! spv {
    ($s:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $s, ".spv"))
    };
}
const VISIBLE: [&str; 3] = [
    "Visible ordinary pixels",
    "Visible triangle cluster pixels",
    "Visible voxel pixels",
];

fn probes(g: &mut Graph) -> Result<()> {
    let color = g.resource("Coverage replay IDs", Storage::Color);
    let depth = g.resource("Coverage replay depth", Storage::Depth);
    let mut repeats = Vec::new();
    for pass in &g.passes {
        let Op::Draw { program, count, .. } = &pass.op else {
            continue;
        };
        ensure!(
            pass.phase == PassPhase::Raster && pass.name.contains("visibility"),
            "unexpected forest draw {}",
            pass.name
        );
        let Shader::Mesh(mesh, fragment) = program.shader else {
            anyhow::bail!("unexpected non-mesh forest draw")
        };
        let mut replay = |name, shader: &'static [u8]| {
            let mut program = program.clone();
            program.shader = Shader::Mesh(mesh, shader);
            repeats.push((
                name,
                Op::Draw {
                    program,
                    count: *count,
                    color,
                    depth,
                    clear: true,
                },
            ));
        };
        if pass.name.contains("voxel brick") {
            replay("Covered brick surfaces", spv!("coverage_brick.frag"));
            replay("Covered brick rectangles", spv!("coverage_rect.frag"));
        } else {
            let frag: &'static [u8] = if fragment == spv!("visibility_plant.frag") {
                spv!("coverage_plant.frag")
            } else {
                ensure!(
                    fragment == spv!("visibility.frag"),
                    "unknown visibility fragment stage"
                );
                spv!("coverage_ordinary.frag")
            };
            replay("Covered triangle surfaces", frag);
        }
    }
    for (name, draw) in repeats {
        g.pass_in(PassPhase::Raster, name, draw);
    }
    let counter = g.input("Final visibility pixel counts", vec![0; 12]);
    let material = g
        .passes
        .iter()
        .find(|p| p.phase == PassPhase::Shade && p.name.starts_with("Shade "))
        .context("scene needs a visibility material")?;
    let mut bindings = material.op.program().unwrap().bindings[..5].to_vec();
    bindings[2] = (counter, Access::ComputeReadWrite);
    let [w, h] = g.extent;
    g.pass_in(
        PassPhase::Counters,
        "Count final visibility categories",
        Op::Compute {
            program: Dispatch {
                shader: Shader::Compute(spv!("overdraw_visible.comp")),
                bindings,
                push: Vec::new(),
            },
            groups: [w.div_ceil(8), h.div_ceil(8), 1],
        },
    );
    g.pass(
        "Read final visibility categories",
        Op::Counters {
            buffer: counter,
            first: 0,
            labels: VISIBLE.to_vec(),
        },
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let preset = args.next().context(
        "usage: forest_overdraw karst|forest auto|triangles|reference WIDTHxHEIGHT [compare|compute|linear|mesh [image-directory]]",
    )?;
    let mode = args
        .next()
        .context("choose auto, triangles or full-detail reference")?;
    ensure!(
        matches!(mode.as_str(), "auto" | "triangles" | "reference"),
        "invalid vegetation mode"
    );
    let size = args.next().context("missing WIDTHxHEIGHT")?;
    let (w, h) = size.split_once('x').context("invalid extent")?;
    let extent = [w.parse()?, h.parse()?];
    let backend = args.next();
    ensure!(
        backend
            .as_deref()
            .is_none_or(|b| matches!(b, "mesh" | "compute" | "linear" | "compare")),
        "invalid raster backend"
    );
    let capture = args.next().map(std::path::PathBuf::from);
    if let Some(dir) = &capture {
        std::fs::create_dir_all(dir)?;
    }
    ensure!(
        !extent.contains(&0) && args.next().is_none(),
        "invalid size or extra arguments"
    );
    let world = match preset.as_str() {
        "karst" => experiments::restir::karst_study()?,
        "forest" => experiments::vegetation::forest()?,
        _ => anyhow::bail!("unknown scene {preset}"),
    };
    let mut engine = experiments::open(world)?;
    let mut console = Console::default();
    let cut = if mode == "reference" {
        "triangles"
    } else {
        mode.as_str()
    };
    if mode == "reference" {
        console.execute(&mut engine, &[], "lod 0")?;
    }
    for cmd in [
        "vegetation wind 0",
        "post off",
        &format!("vegetation {cut}"),
        &format!("vegetation raster {}", backend.as_deref().unwrap_or("mesh")),
    ] {
        console.execute(&mut engine, &[], cmd)?;
    }
    let origin = engine.camera;
    let mut extractor = SceneExtractor::default();
    let mut gpu = if backend.is_some() {
        Backend::open()?
    } else {
        Backend::open_with_raster_queries()?
    };
    let positions: &[f32] = if preset == "karst" {
        &[0., 36., 72., 36., 0., -18., -36.]
    } else {
        &[110., 185., 272.5, 360., 410.]
    };
    if backend.is_some() {
        println!(
            "stage,position,sample,candidates,lod_rejects,early_z_rejects,dda_calls,dda_hits,atomic_wins,mesh_changed,compute_changed,coverage_mismatch,id_mismatch,depth_delta_gt_1e_minus_5,id_mismatch_within_1ulp,post_candidates,post_dda_calls,max_depth_delta,visible_bricks,trace_ms,bridge_ms,gpu_ms"
        );
    } else {
        println!(
            "stage,position,sample,foreground_px,voxel_px,raw_tri_samples,raw_brick_samples,brick_rect_samples,depth_pass_samples,brick_depth_pass_samples,fragment_invocations,brick_fragment_invocations"
        );
    }
    let timed = matches!(backend.as_deref(), Some("compute" | "linear" | "mesh"));
    let (warm, samples) = if timed { (12, 30) } else { (3, 3) };
    for (stage, &position) in positions.iter().enumerate() {
        engine.camera = origin;
        if preset == "karst" {
            engine
                .camera
                .translate(0., position.signum(), position.abs() / 60.);
        } else {
            engine.camera.distance = position;
        }
        for i in 0..warm + samples {
            let mut view = extractor.snapshot(
                &engine.world,
                engine.camera,
                extent,
                engine.materials.clone(),
            )?;
            view.buffer_atomic64 = gpu.buffer_atomic64();
            view.ray_caps = gpu.ray_capabilities();
            let mut graph = engine.technique.build(&view)?;
            if backend.is_none() && i >= warm {
                probes(&mut graph)?;
            }
            let frame = gpu.execute(graph.compile()?, &engine.tasks)?;
            if i == warm + samples - 1
                && let Some(dir) = &capture
            {
                render_lab::recording::save_capture(
                    dir.join(format!("{stage}.png")),
                    frame.extent,
                    frame.pixels.clone(),
                )?;
            }
            let arrival = backend.as_deref() == Some("compare") && stage > 0 && i == 0;
            if i < warm && !arrival {
                continue;
            }
            if let Some(backend) = backend.as_deref() {
                let counter = |name: &str| frame.counters.get(name).copied().unwrap_or(0);
                let counts = [
                    "Brick candidate pixels",
                    "Brick LOD rejects",
                    "Brick early depth rejects",
                    "Brick DDA calls",
                    "Brick DDA hits",
                    "Brick atomic wins",
                    "Mesh brick changed pixels",
                    "Compute brick changed pixels",
                    "Brick coverage differences",
                    "Brick ID differences",
                    "Brick depth differences over 1e-5",
                ]
                .map(counter);
                if backend == "compare" {
                    ensure!(
                        counts[0] == counts[1] + counts[2] + counts[3]
                            && counts[3] >= counts[4]
                            && counts[4] >= counts[5],
                        "voxel work counters inconsistent"
                    );
                }
                let (mut trace, mut bridge) = (0., 0.);
                for p in frame
                    .passes
                    .iter()
                    .filter(|p| p.name.contains("voxel brick visibility"))
                {
                    if p.name.ends_with(" trace") || p.name.ends_with(" visibility") {
                        trace += p.ms;
                    } else {
                        bridge += p.ms;
                    }
                }
                print!("{stage},{position},{}", if arrival { -1 } else { i - warm });
                for n in counts {
                    print!(",{n}");
                }
                println!(
                    ",{},{},{},{:.9},{},{trace:.6},{bridge:.6},{:.6}",
                    counter("Brick ID differences within one depth ULP"),
                    counter("Recovered brick candidate pixels"),
                    counter("Recovered brick DDA calls"),
                    f32::from_bits(counter("Brick maximum depth difference bits") as u32),
                    counter("Visible voxel bricks"),
                    frame.profile.gpu_span_ms.context("missing GPU span")?
                );
                continue;
            }
            let visible = VISIBLE.map(|n| frame.counters[n]);
            let fg: u64 = visible.iter().sum();
            let (mut tri, mut brick, mut rect, mut passed, mut bp, mut inv, mut bi) =
                (0, 0, 0, 0, 0, 0, 0);
            for pass in &frame.passes {
                let Some(c) = pass.raster else { continue };
                match pass.name.as_str() {
                    "Covered triangle surfaces" => tri += c.passed_samples,
                    "Covered brick surfaces" => brick += c.passed_samples,
                    "Covered brick rectangles" => rect += c.passed_samples,
                    _ => {
                        passed += c.passed_samples;
                        inv += c.fragment_invocations;
                        if pass.name.contains("voxel brick") {
                            bp += c.passed_samples;
                            bi += c.fragment_invocations;
                        }
                    }
                }
            }
            ensure!(
                fg > 0 && tri + brick >= passed && passed >= fg && rect >= brick && brick >= bp,
                "coverage mismatch at {stage}: {fg}, {tri}, {brick}, {rect}, {passed}, {bp}"
            );
            println!(
                "{stage},{position},{},{fg},{},{tri},{brick},{rect},{passed},{bp},{inv},{bi}",
                i - 3,
                visible[2]
            );
        }
    }
    Ok(())
}
