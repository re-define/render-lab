mod support;
use anyhow::Result;
use image::{AnimationDecoder, codecs::gif::GifDecoder};
use std::{fs, io::BufReader, process::Command};

#[test]
#[ignore = "requires Vulkan and FFmpeg with GIF and H.264 encoders"]
fn fixed_step_recording_exports_both_formats() -> Result<()> {
    let output = support::tempdir()?;
    let gif = output.path().join("preview.gif");
    let mp4 = output.path().join("preview.mp4");
    let result = Command::new(env!("CARGO_BIN_EXE_render-lab"))
        .args(["--duration", "0.3", "--fps", "10", "--record"])
        .arg(&gif)
        .arg("--record")
        .arg(&mp4)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let frames = GifDecoder::new(BufReader::new(fs::File::open(gif)?))?
        .into_frames()
        .collect_frames()?;
    assert_eq!(frames.len(), 3);
    for frame in frames {
        assert_eq!(frame.buffer().dimensions(), (800, 500));
        assert_eq!(frame.delay().numer_denom_ms(), (100, 1));
    }
    assert_eq!(fs::read(mp4)?.get(4..8), Some(b"ftyp".as_slice()));
    Ok(())
}
