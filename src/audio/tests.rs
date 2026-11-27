use super::*;
use hound::{SampleFormat, WavSpec, WavWriter};
use rodio::{
    Source,
    buffer::SamplesBuffer,
    mixer::{self, MixerSource},
};
use std::{
    io::Cursor,
    time::{Duration, Instant},
};

fn memory() -> (Audio, MixerSource) {
    let (mixer, output) = mixer::mixer(1, 8000);
    (
        Audio {
            output: Some(Output::Memory(mixer)),
            ..Default::default()
        },
        output,
    )
}
fn render(output: &mut MixerSource, n: usize) -> Vec<f32> {
    (0..n).map(|_| output.next().unwrap_or(0.)).collect()
}

#[test]
fn polyphonic_gain_and_individual_stop_reach_the_mixer() -> Result<()> {
    let (mut audio, mut out) = memory();
    let clip = AudioClip(SamplesBuffer::new(1, 8000, vec![0.25; 8000]));
    audio.set_volume(0.5)?;
    let a = audio.play(&clip)?;
    let b = audio.play(&clip)?;
    assert_ne!(a, b);
    assert!(
        render(&mut out, 128)
            .iter()
            .all(|v| (*v - 0.25).abs() < 1e-6)
    );
    assert!(audio.set_volume(f32::NAN).is_err());
    assert!(audio.set_volume(2.).is_err());
    audio.stop(a)?;
    assert!(audio.stop(a).is_err());
    assert!((render(&mut out, 128)[127] - 0.125).abs() < 1e-6);
    audio.set_volume(0.25)?;
    assert!((render(&mut out, 128)[127] - 0.0625).abs() < 1e-6);
    audio.stop_all();
    assert_eq!(render(&mut out, 128)[127], 0.);
    Ok(())
}

#[test]
fn pause_freezes_new_and_existing_voices_and_finished_slots_are_reused() -> Result<()> {
    let (mut audio, mut out) = memory();
    let clip = AudioClip(SamplesBuffer::new(1, 8000, vec![0.25; 800]));
    audio.set_volume(1.)?;
    audio.set_paused(true);
    for _ in 0..MAX_VOICES {
        audio.play(&clip)?;
    }
    assert!(audio.play(&clip).is_err());
    assert!(render(&mut out, 1600).iter().all(|v| *v == 0.));
    audio.set_paused(false);
    let mut output = render(&mut out, 173);
    audio.set_paused(true);
    output.extend(render(&mut out, 2000));
    assert_eq!(*output.last().unwrap(), 0.);
    audio.set_paused(false);
    output.extend(render(&mut out, 1600));
    assert_eq!(output.iter().filter(|v| **v > 0.).count(), 800);
    audio.update();
    assert!(audio.voices.is_empty());
    assert!(audio.play(&clip)? > MAX_VOICES as u64);
    Ok(())
}

fn wave<T: hound::Sample + Copy>(spec: WavSpec, samples: &[T]) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut out, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(out.into_inner())
}
#[test]
fn wav_keeps_stereo_and_rate_and_rejects_bad_samples() -> Result<()> {
    let mut spec = WavSpec {
        channels: 2,
        sample_rate: 24_000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let bytes = wave(spec, &[16384_i16, -16384, 8192, -8192])?;
    let clip = AudioClip::read_wav(bytes.as_slice())?;
    assert_eq!((clip.0.channels(), clip.0.sample_rate()), (2, 24000));
    assert_eq!(clip.0.clone().collect::<Vec<_>>(), [0.5, -0.5, 0.25, -0.25]);
    assert!(AudioClip::read_wav(&bytes[..bytes.len() - 1]).is_err());
    spec.bits_per_sample = 32;
    spec.sample_format = SampleFormat::Float;
    assert!(AudioClip::read_wav(&wave(spec, &[0., f32::NAN])?[..]).is_err());
    spec.channels = 3;
    assert!(AudioClip::read_wav(&wave(spec, &[0., 1., 0.])?[..]).is_err());
    assert!(AudioClip::read_wav(&b"bad"[..]).is_err());
    let cue = AudioClip::impact();
    assert_eq!(cue.0.total_duration(), Some(Duration::from_millis(240)));
    Ok(())
}

#[test]
#[ignore = "requires a real audio output; plays one short quiet sound"]
fn default_device_consumes_the_clip() -> Result<()> {
    let mut audio = Audio::default();
    audio.set_volume(0.12)?;
    let id = audio.play(AudioClip::impact())?;
    eprintln!("{}", audio.status());
    let start = Instant::now();
    while audio.voices.contains_key(&id) {
        anyhow::ensure!(
            start.elapsed() < Duration::from_secs(4),
            "device did not consume clip"
        );
        std::thread::sleep(Duration::from_millis(10));
        audio.update();
    }
    assert!(audio.last_error.is_none());
    Ok(())
}

#[test]
#[ignore = "opens Vulkan for normal engine and console entry points; audio uses an in-memory mixer"]
fn scene_commands_and_replacement_manage_voice_lifetime() -> Result<()> {
    use crate::{
        console::Console,
        experiments::{self, movement_lab},
    };
    let mut engine = experiments::open(movement_lab::scene()?)?;
    let (audio, mut output) = memory();
    engine.audio = audio;
    let mut console = Console::default();
    let run = |c: &mut Console, e: &mut crate::engine::Engine, cmd: &str| c.execute(e, &[], cmd);
    let impact = "object 6 impulse 0 2 0 at 3.5 2 0";
    run(&mut console, &mut engine, impact)?;
    assert!(engine.audio.voices.is_empty());
    run(&mut console, &mut engine, "audio cues on")?;
    assert!(run(&mut console, &mut engine, "object 3 impulse 0 2 0").is_err());
    run(&mut console, &mut engine, "object 6 impulse 0 0 0")?;
    assert!(engine.audio.voices.is_empty());
    run(&mut console, &mut engine, impact)?;
    assert_eq!(engine.audio.voices.len(), 1);
    assert!(render(&mut output, 64).iter().any(|v| v.abs() > 0.001));
    run(&mut console, &mut engine, "pause")?;
    assert!(engine.audio.paused);
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("effect with spaces.wav");
    std::fs::write(&file, include_bytes!("../../assets/audio/impact.wav"))?;
    run(
        &mut console,
        &mut engine,
        &format!(r#"audio play "{}""#, file.display()),
    )?;
    assert_eq!(engine.audio.voices.len(), 2);
    let mut bad = movement_lab::scene()?;
    bad.entities.get_mut(&6).unwrap().parent = Some(6);
    assert!(experiments::replace(&mut engine, bad).is_err());
    assert_eq!(engine.audio.voices.len(), 2);
    experiments::replace(&mut engine, movement_lab::scene()?)?;
    assert!(engine.audio.voices.is_empty() && engine.audio.paused);
    Ok(())
}
