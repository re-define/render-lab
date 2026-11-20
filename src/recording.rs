//! Stream completed BGRA viewport frames to an encoder with bounded memory.
use crate::rhi::Frame;
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
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
    child: Child,
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
            .tempfile_in(parent)?
            .into_temp_path();
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
                "+faststart",
            ]);
        }
        let child = cmd
            .args(["-f", kind])
            .arg(&temp)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("starting FFmpeg; install ffmpeg on PATH or set RENDER_LAB_FFMPEG")?;
        Ok(Self {
            child,
            temp: Some(temp),
            destination: path.into(),
            extent,
        })
    }
    pub fn write(&mut self, frame: &Frame) -> Result<()> {
        ensure!(
            frame.pixels.len() == frame.extent[0] as usize * frame.extent[1] as usize * 4,
            "missing viewport pixels"
        );
        let pipe = self.child.stdin.as_mut().context("encoder input closed")?;
        if self.extent == frame.extent {
            pipe.write_all(&frame.pixels)?;
        } else {
            let src = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
                frame.extent[0],
                frame.extent[1],
                frame.pixels.as_slice(),
            )
            .context("invalid frame")?;
            let scale = (self.extent[0] as f64 / frame.extent[0] as f64)
                .min(self.extent[1] as f64 / frame.extent[1] as f64);
            let w = (frame.extent[0] as f64 * scale)
                .round()
                .clamp(1., self.extent[0] as f64) as u32;
            let h = (frame.extent[1] as f64 * scale)
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
        drop(self.child.stdin.take());
        let status = self.child.wait()?;
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
            drop(self.child.stdin.take());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub struct LiveRecording {
    path: PathBuf,
    stream: Option<(Encoder, Instant, u64)>,
}
impl LiveRecording {
    pub fn new(path: &Path) -> Result<Self> {
        format(path)?;
        Ok(Self {
            path: path.into(),
            stream: None,
        })
    }
    pub fn push(&mut self, frame: &Frame) -> Result<()> {
        if frame.pixels.is_empty() {
            return Ok(());
        }
        if self.stream.is_none() {
            self.stream = Some((
                Encoder::open(&self.path, frame.extent, 30)?,
                frame.completed,
                0,
            ));
        }
        let (encoder, start, written) = self.stream.as_mut().unwrap();
        let due = (frame
            .completed
            .saturating_duration_since(*start)
            .as_secs_f64()
            * 30.)
            .floor() as u64
            + 1;
        while *written < due {
            encoder.write(frame)?;
            *written += 1;
        }
        Ok(())
    }
    pub fn abort(mut self) {
        drop(self.stream.take());
    }
    pub fn finish(mut self) -> Result<()> {
        self.stream.take().context("no frames recorded")?.0.finish()
    }
}
impl Drop for LiveRecording {
    fn drop(&mut self) {
        if let Some((encoder, _, _)) = self.stream.take()
            && let Err(error) = encoder.finish()
        {
            eprintln!("finishing recording: {error:#}");
        }
    }
}
