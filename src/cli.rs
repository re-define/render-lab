//! Parse command-line options before allocating a scene or opening the GPU.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

pub struct Options {
    pub experiment: String,
    pub scene: Option<PathBuf>,
    pub commands: Vec<String>,
    pub output: Output,
}

pub enum Output {
    Window {
        benchmark: Option<usize>,
    },
    Capture {
        path: PathBuf,
        secondary: Option<PathBuf>,
        size: [u32; 2],
        warmup: u32,
        yaw: f32,
    },
    Record {
        paths: Vec<PathBuf>,
        secondary: Option<PathBuf>,
        size: [u32; 2],
        fps: u32,
        duration: f64,
        yaw: f32,
        events: Vec<(f64, String)>,
    },
}

const USAGE: &str = "render-lab [--list-experiments] [--experiment NAME] [--scene FILE] [--command COMMAND] [--capture FILE.png [--size WIDTHxHEIGHT] [--warmup FRAMES] [--secondary-capture FILE.png] | --benchmark FRAMES | --record FILE.mp4|FILE.gif [--size WIDTHxHEIGHT] [--secondary-record FILE.mp4|FILE.gif] [--secondary-yaw DEGREES] [--duration SEC] [--fps N] [--at SEC COMMAND]]";

fn image_size(value: OsString) -> Result<[u32; 2]> {
    let text = value.to_str().context("invalid image size")?;
    let (width, height) = text.split_once('x').context("--size needs WIDTHxHEIGHT")?;
    let (width, height) = (width.parse::<u32>()?, height.parse::<u32>()?);
    ensure!(
        width > 0 && height > 0 && width.checked_mul(height).is_some(),
        "invalid image dimensions"
    );
    Ok([width, height])
}

pub fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Options>> {
    let mut scene = None;
    let mut experiment = String::new();
    let mut commands = Vec::new();
    let mut capture = None;
    let mut secondary_capture = None;
    let mut recordings = Vec::new();
    let mut secondary_record = None;
    let mut benchmark = None;
    let mut warmup = None;
    let mut size = None;
    let mut fps = 30u32;
    let mut duration = 5f64;
    let mut yaw = 45f32;
    let mut events = Vec::<(f64, String)>::new();
    while let Some(option) = args.next() {
        let mut next = |missing| args.next().context(missing);
        match option.to_str() {
            Some("--experiment") => {
                experiment = next("--experiment needs a scene name")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid experiment name"))?
            }
            Some("--scene") => scene = Some(PathBuf::from(next("--scene needs a file")?)),
            Some("--command") => commands.push(
                next("--command needs console text")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid command"))?,
            ),
            Some("--capture") => capture = Some(PathBuf::from(next("--capture needs a PNG path")?)),
            Some("--secondary-capture") => {
                secondary_capture = Some(PathBuf::from(next("missing secondary PNG path")?))
            }
            Some("--record") => {
                recordings.push(PathBuf::from(next("--record needs a GIF or MP4 path")?))
            }
            Some("--secondary-record") => {
                secondary_record = Some(PathBuf::from(next("missing secondary recording path")?))
            }
            Some("--size") => size = Some(image_size(next("--size needs WIDTHxHEIGHT")?)?),
            Some("--warmup") => {
                warmup = Some(
                    next("--warmup needs a frame count")?
                        .to_string_lossy()
                        .parse()?,
                )
            }
            Some("--fps") => {
                fps = next("--fps needs a frame rate")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--duration") => {
                duration = next("--duration needs seconds")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--secondary-yaw") => {
                yaw = next("missing orbit angle in degrees")?
                    .to_string_lossy()
                    .parse()?
            }
            Some("--benchmark") => {
                let n = next("--benchmark needs a frame count")?
                    .to_string_lossy()
                    .parse()?;
                ensure!(n > 0, "positive sample count required");
                benchmark = Some(n);
            }
            Some("--at") => {
                let time = next("--at needs seconds")?.to_string_lossy().parse()?;
                let line = next("--at needs a console command")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid command"))?;
                events.push((time, line));
            }
            Some("--list-experiments") => {
                for (name, _) in super::presets::PRESETS {
                    println!("{name}");
                }
                return Ok(None);
            }
            Some("--help") => {
                println!("{USAGE}");
                return Ok(None);
            }
            _ => bail!("unknown option {option:?}"),
        }
    }
    ensure!(
        usize::from(capture.is_some())
            + usize::from(benchmark.is_some())
            + usize::from(!recordings.is_empty())
            <= 1,
        "choose capture, benchmark or recording"
    );
    ensure!(
        warmup.is_none() || capture.is_some(),
        "--warmup requires --capture"
    );
    ensure!(
        size.is_none() || capture.is_some() || !recordings.is_empty(),
        "--size applies to captured images and recordings"
    );
    ensure!(
        secondary_capture.is_none() || capture.is_some(),
        "a secondary capture needs a distinct primary capture"
    );
    ensure!(
        secondary_record.is_none() || !recordings.is_empty(),
        "a secondary recording needs a distinct primary recording"
    );
    validate_outputs(
        capture
            .iter()
            .chain(&secondary_capture)
            .chain(&recordings)
            .chain(&secondary_record),
    )?;
    ensure!(yaw.is_finite(), "invalid secondary camera yaw");
    ensure!(
        (1..=100).contains(&fps) && duration.is_finite() && duration > 0.,
        "invalid recording duration or fps"
    );
    ensure!(
        events.is_empty() || !recordings.is_empty(),
        "--at requires --record"
    );
    ensure!(
        events
            .iter()
            .all(|(t, _)| t.is_finite() && *t >= 0. && *t < duration),
        "command time outside recording"
    );
    let size = size.unwrap_or([800, 500]);
    let output = if let Some(path) = capture {
        Output::Capture {
            path,
            secondary: secondary_capture,
            size,
            yaw,
            warmup: warmup.unwrap_or(1),
        }
    } else if !recordings.is_empty() {
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        Output::Record {
            paths: recordings,
            secondary: secondary_record,
            size,
            fps,
            duration,
            yaw,
            events,
        }
    } else {
        Output::Window { benchmark }
    };
    Ok(Some(Options {
        experiment,
        scene,
        commands,
        output,
    }))
}
fn validate_outputs<'a>(outputs: impl Iterator<Item = &'a PathBuf>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for path in outputs {
        let file = path.file_name().context("output needs a filename")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let resolved = if path.exists() {
            path.canonicalize()?
        } else {
            parent.canonicalize()?.join(file)
        };
        #[cfg(windows)]
        let resolved = PathBuf::from(resolved.to_string_lossy().to_lowercase());
        anyhow::ensure!(
            seen.insert(resolved),
            "multiple outputs resolve to {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    fn args<'a>(words: &'a [&'a str]) -> impl Iterator<Item = OsString> + 'a {
        words.iter().map(OsString::from)
    }

    #[test]
    fn recording_events_remain_stable_and_output_modes_are_exclusive() -> Result<()> {
        let parsed = parse(args(&[
            "--record",
            "check.mp4",
            "--at",
            "2",
            "last",
            "--at",
            "1",
            "first",
            "--at",
            "1",
            "second",
        ]))?
        .unwrap();
        let Output::Record {
            events, size, fps, ..
        } = parsed.output
        else {
            panic!("expected recording")
        };
        assert_eq!(
            events,
            [
                (1., "first".to_owned()),
                (1., "second".to_owned()),
                (2., "last".to_owned())
            ]
        );
        assert_eq!((size, fps), ([800, 500], 30));
        assert!(parse(args(&["--record", "test.mp4", "--benchmark", "2"])).is_err());
        assert!(parse(args(&["--secondary-capture", "side.png"])).is_err());
        Ok(())
    }

    #[test]
    fn output_aliases_cannot_overwrite_another_view() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let first = dir.path().join("frame.png");
        let second = dir.path().join("nested/../frame.png");
        assert!(validate_outputs([&first, &second].into_iter()).is_err());
        #[cfg(windows)]
        assert!(validate_outputs([&first, &dir.path().join("FRAME.png")].into_iter()).is_err());
        Ok(())
    }
}
