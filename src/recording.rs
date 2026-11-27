//! Stream completed BGRA viewport frames to an encoder with bounded memory.
use crate::rhi::Frame;
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Instant,
};
use tempfile::TempPath;

fn format(path: &Path) -> Result<&'static str> {
    match path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "gif" => Ok("gif"),
        "mp4" => Ok("mp4"),
        _ => bail!("choose an .mp4 or .gif recording path"),
    }
}
pub struct Encoder {
    child: Arc<Mutex<Child>>,
    pipe: Option<ChildStdin>,
    temp: Option<TempPath>,
    destination: PathBuf,
    extent: [u32; 2],
}
impl Encoder {
    pub fn open(path: &Path, extent: [u32; 2], fps: u32) -> Result<Self> {
        ensure!(
            extent.iter().all(|v| *v > 0) && (1..=100).contains(&fps),
            "invalid recording dimensions or frame rate"
        );
        let kind = format(path)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temp = tempfile::Builder::new()
            .prefix(".render-lab-")
            .tempfile_in(parent)?;
        let (output, temp) = temp.into_parts();
        let local = Path::new("target/tools").join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        let bin = std::env::var_os("RENDER_LAB_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if local.is_file() {
                    local
                } else {
                    PathBuf::from("ffmpeg")
                }
            });
        let mut cmd = Command::new(bin);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "bgra",
            "-video_size",
            &format!("{}x{}", extent[0], extent[1]),
            "-framerate",
            &fps.to_string(),
            "-i",
            "pipe:0",
            "-an",
        ]);
        if kind == "gif" {
            // One palette per frame keeps long live captures bounded in memory.
            cmd.args(["-filter_complex","split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=bayer:bayer_scale=3","-loop","0"]);
        } else {
            cmd.args([
                "-vf",
                "pad=ceil(iw/2)*2:ceil(ih/2)*2",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
            ]);
        }
        let mut child = cmd
            // The child inherits an already-open file. A second open by FFmpeg can
            // be denied by Windows even though the application may write the destination.
            .args(["-f", kind, "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output))
            .spawn()
            .context("starting FFmpeg; install ffmpeg on PATH or set RENDER_LAB_FFMPEG")?;
        let pipe = child.stdin.take();
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            pipe,
            temp: Some(temp),
            destination: path.into(),
            extent,
        })
    }
    pub fn write(&mut self, frame: &Frame) -> Result<()> {
        self.write_pixels(frame.extent, &frame.pixels)
    }
    fn write_pixels(&mut self, extent: [u32; 2], pixels: &[u8]) -> Result<()> {
        ensure!(
            extent.into_iter().all(|n| n != 0)
                && Some(pixels.len())
                    == (extent[0] as usize)
                        .checked_mul(extent[1] as usize)
                        .and_then(|n| n.checked_mul(4)),
            "missing viewport pixels"
        );
        let pipe = self.pipe.as_mut().context("encoder input closed")?;
        if self.extent == extent {
            pipe.write_all(pixels)?;
        } else {
            let src =
                image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(extent[0], extent[1], pixels)
                    .context("invalid frame")?;
            let scale = (self.extent[0] as f64 / extent[0] as f64)
                .min(self.extent[1] as f64 / extent[1] as f64);
            let w = (extent[0] as f64 * scale)
                .round()
                .clamp(1., self.extent[0] as f64) as u32;
            let h = (extent[1] as f64 * scale)
                .round()
                .clamp(1., self.extent[1] as f64) as u32;
            let resized =
                image::imageops::resize(&src, w, h, image::imageops::FilterType::Triangle);
            let mut canvas = image::RgbaImage::new(self.extent[0], self.extent[1]);
            image::imageops::replace(
                &mut canvas,
                &resized,
                ((self.extent[0] - w) / 2).into(),
                ((self.extent[1] - h) / 2).into(),
            );
            pipe.write_all(&canvas)?;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        drop(self.pipe.take());
        let status = self.child.lock().unwrap().wait()?;
        ensure!(status.success(), "FFmpeg exited with {status}");
        self.temp
            .take()
            .unwrap()
            .persist(&self.destination)
            .context("publishing completed recording")?;
        Ok(())
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if self.temp.is_some() {
            drop(self.pipe.take());
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

const LIVE_FPS: u32 = 30;
const QUEUE_CAPACITY: usize = 2;

struct Sample {
    slot: u64,
    extent: [u32; 2],
    pixels: Vec<u8>,
}

#[derive(Default)]
struct WorkerState {
    aborted: bool,
    child: Weak<Mutex<Child>>,
}

pub struct LiveRecording {
    sender: Option<SyncSender<Sample>>,
    worker: Option<JoinHandle<Result<()>>>,
    state: Arc<Mutex<WorkerState>>,
    origin: Option<Instant>,
    /// The UI advances the output clock even if it has to discard a sample.
    slots: Arc<AtomicU64>,
    dropped: u64,
}

impl LiveRecording {
    pub fn new(path: &Path) -> Result<Self> {
        format(path)?;
        let path = path.to_owned();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let state = Arc::new(Mutex::new(WorkerState::default()));
        let slots = Arc::new(AtomicU64::new(0));
        let worker = thread::Builder::new().name("video-encoder".into()).spawn({
            let state = state.clone();
            let slots = slots.clone();
            move || encode_live(&path, receiver, &slots, &state)
        })?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            origin: None,
            slots,
            state,
            dropped: 0,
        })
    }

    /// Transfer ownership of the finished readback on a sampled frame.
    pub fn offer(&mut self, frame: &mut Frame) -> Result<()> {
        if frame.pixels.is_empty() {
            return Ok(());
        }
        self.sample(frame.completed, frame.extent, || {
            std::mem::take(&mut frame.pixels)
        })
    }

    /// Borrowed-frame compatibility for clients that retain readback pixels.
    pub fn push(&mut self, frame: &Frame) -> Result<()> {
        if frame.pixels.is_empty() {
            return Ok(());
        }
        self.sample(frame.completed, frame.extent, || frame.pixels.clone())
    }

    fn sample(
        &mut self,
        completed: Instant,
        extent: [u32; 2],
        pixels: impl FnOnce() -> Vec<u8>,
    ) -> Result<()> {
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            self.join()?;
            bail!("encoder stopped before capture finished");
        }
        let start = self.origin.get_or_insert(completed);
        let slot = (completed.saturating_duration_since(*start).as_nanos() * LIVE_FPS as u128
            / 1_000_000_000) as u64;
        if self.slots.load(Ordering::Relaxed) > slot {
            return Ok(());
        }
        self.slots.store(slot + 1, Ordering::Release);
        let sample = Sample {
            slot,
            extent,
            pixels: pixels(),
        };
        match self
            .sender
            .as_ref()
            .context("recording closed")?
            .try_send(sample)
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.join()?;
                bail!("encoder disconnected")
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn join(&mut self) -> Result<()> {
        drop(self.sender.take());
        self.worker
            .take()
            .context("recording closed")?
            .join()
            .map_err(|_| anyhow::anyhow!("encoder thread panicked"))?
    }
    pub fn abort(mut self) {
        let child = {
            let mut state = self.state.lock().unwrap();
            state.aborted = true;
            state.child.upgrade()
        };
        if let Some(child) = child {
            let _ = child.lock().unwrap().kill();
        }
        if self.worker.is_some() {
            let _ = self.join();
        }
    }
    pub fn finish(mut self) -> Result<()> {
        self.join()
    }
}

fn encode_live(
    path: &Path,
    receiver: Receiver<Sample>,
    slots: &AtomicU64,
    state: &Mutex<WorkerState>,
) -> Result<()> {
    let first = receiver.recv().context("no frames recorded")?;
    let mut encoder = Encoder::open(path, first.extent, LIVE_FPS)?;
    {
        let mut state = state.lock().unwrap();
        if state.aborted {
            return Ok(());
        }
        state.child = Arc::downgrade(&encoder.child);
    }
    fill_slots(first, receiver, slots, |s| {
        encoder.write_pixels(s.extent, &s.pixels)
    })?;
    if state.lock().unwrap().aborted {
        return Ok(());
    }
    encoder.finish()
}

/// Retain the most recent available sample until the next one's presentation time.
fn fill_slots(
    mut last: Sample,
    receiver: Receiver<Sample>,
    slots: &AtomicU64,
    mut write: impl FnMut(&Sample) -> Result<()>,
) -> Result<()> {
    let mut written = 0;
    for sample in receiver {
        while written < sample.slot {
            write(&last)?;
            written += 1;
        }
        last = sample;
    }
    while written < slots.load(Ordering::Acquire) {
        write(&last)?;
        written += 1;
    }
    Ok(())
}

impl Drop for LiveRecording {
    fn drop(&mut self) {
        if self.worker.is_some()
            && let Err(error) = self.join()
        {
            eprintln!("finishing recording: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn gaps_repeat_prior_pixels_including_dropped_tail() -> Result<()> {
        let sample = |slot, value| Sample {
            slot,
            extent: [1, 1],
            pixels: vec![value; 4],
        };
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(sample(3, 42))?;
        drop(tx);
        let mut colors = Vec::new();
        fill_slots(sample(0, 7), rx, &AtomicU64::new(5), |s| {
            colors.push(s.pixels[0]);
            Ok(())
        })?;
        assert_eq!(colors, [7, 7, 7, 42, 42]);
        Ok(())
    }

    #[test]
    fn stalled_encoder_drops_new_frames_and_keeps_the_clock() -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let mut live = LiveRecording {
            sender: Some(tx),
            worker: None,
            state: Default::default(),
            origin: None,
            slots: Default::default(),
            dropped: 0,
        };
        let now = Instant::now();
        for n in 0..5 {
            live.sample(now + Duration::from_millis(n * 34), [1, 1], || {
                vec![n as u8; 4]
            })?;
        }
        assert_eq!(live.dropped(), 3);
        assert_eq!(live.slots.load(Ordering::Acquire), 5);
        live.sample(now + Duration::from_millis(137), [1, 1], || {
            panic!("duplicate copied")
        })?;
        assert_eq!(rx.try_iter().map(|s| s.slot).collect::<Vec<_>>(), [0, 1]);
        Ok(())
    }
    #[test]
    fn empty_worker_shutdown_preserves_existing_destination() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("prior.gif");
        std::fs::write(&path, b"previous recording")?;
        assert!(LiveRecording::new(&path)?.finish().is_err());
        LiveRecording::new(&path)?.abort();
        assert_eq!(std::fs::read(&path)?, b"previous recording");
        assert_eq!(directory.path().read_dir()?.count(), 1);
        Ok(())
    }

    #[test]
    #[ignore = "requires FFmpeg"]
    fn ffmpeg_worker_publishes_gif_and_abort_removes_partial_file() -> Result<()> {
        // Exercise a relative output path with two simultaneous encoders.
        let directory = tempfile::tempdir_in("target")?;
        let path = directory.path().join("clip.gif");
        let start = Instant::now();
        let video = directory.path().join("second.mp4");
        let mut offline = Encoder::open(&video, [8, 8], 30)?;
        let mut live = LiveRecording::new(&path)?;
        live.sample(start, [8, 8], || vec![255; 8 * 8 * 4])?;
        offline.write_pixels([8, 8], &[96; 8 * 8 * 4])?;
        offline.finish()?;
        assert!(
            std::fs::read(&video)?
                .windows(4)
                .any(|bytes| bytes == b"moof")
        );
        live.sample(start + Duration::from_millis(70), [4, 4], || {
            vec![96; 4 * 4 * 4]
        })?;
        live.finish()?;
        let published = std::fs::read(&path)?;
        assert!(published.starts_with(b"GIF89a") && published.ends_with(&[0x3b]));
        let mut replacement = LiveRecording::new(&path)?;
        replacement.sample(start, [64, 64], || vec![0; 64 * 64 * 4])?;
        replacement.abort();
        assert_eq!(std::fs::read(&path)?, published);
        assert_eq!(directory.path().read_dir()?.count(), 2);
        Ok(())
    }
}
