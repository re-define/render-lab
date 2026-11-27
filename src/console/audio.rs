use crate::audio::{Audio, AudioClip};
use anyhow::{Result, bail, ensure};
use std::path::Path;

pub(super) fn command(audio: &mut Audio, arg: &str) -> Result<String> {
    let (verb, arg) = arg
        .trim()
        .split_once(char::is_whitespace)
        .unwrap_or((arg.trim(), ""));
    let arg = arg.trim();
    match (verb, arg) {
        ("" | "status", "") => Ok(audio.status()),
        ("play", "") => bail!("audio play impact or audio play FILE.wav"),
        ("play", asset) => {
            let id = if asset == "impact" {
                audio.play(AudioClip::impact())?
            } else {
                let clip = AudioClip::load_wav(Path::new(asset.trim_matches('"')))?;
                audio.play(&clip)?
            };
            Ok(format!("Audio voice {id} · {asset}"))
        }
        ("stop", "all") => {
            audio.stop_all();
            Ok("Stopped all audio voices".into())
        }
        ("stop", id) => {
            audio.stop(id.parse()?)?;
            Ok(format!("Stopped audio voice {id}"))
        }
        ("volume", v) => {
            audio.set_volume(v.parse()?)?;
            Ok(audio.status())
        }
        ("cues", v) => {
            ensure!(v == "on" || v == "off", "audio cues on/off");
            audio.set_scene_cues(v == "on");
            Ok(audio.status())
        }
        _ => bail!("audio status | play impact/FILE.wav | stop ID/all | volume 0..1 | cues on/off"),
    }
}
