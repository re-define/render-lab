//! Short, predecoded sound assets. Voice cursors clone only the shared sample buffer.
use anyhow::{Context, Result, ensure};
use hound::{SampleFormat, WavReader};
use rodio::buffer::SamplesBuffer;
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
    sync::LazyLock,
};

#[derive(Clone)]
pub struct AudioClip(pub(super) SamplesBuffer);

impl AudioClip {
    pub fn load_wav(path: &Path) -> Result<Self> {
        let load = || Self::read_wav(BufReader::new(File::open(path)?));
        load().with_context(|| format!("load WAV {}", path.display()))
    }

    pub fn read_wav(reader: impl Read) -> Result<Self> {
        let mut wav = WavReader::new(reader)?;
        let spec = wav.spec();
        ensure!(
            matches!(spec.channels, 1 | 2),
            "expected a mono or stereo WAV"
        );
        ensure!(
            (1..=192_000).contains(&spec.sample_rate),
            "unsupported WAV sample rate"
        );
        // Cap decoded memory before allocation. Duration is frames, len is interleaved samples.
        let count = wav.len() as usize;
        ensure!(
            (1..=8 * 1024 * 1024).contains(&count)
                && count.is_multiple_of(usize::from(spec.channels)),
            "empty, oversized or incomplete WAV"
        );
        let data = match (spec.sample_format, spec.bits_per_sample) {
            (SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
                let divisor = (1_u32 << (bits - 1)) as f32;
                wav.samples::<i32>()
                    .map(|s| s.map(|i| i as f32 / divisor))
                    .collect::<hound::Result<Vec<_>>>()?
            }
            (SampleFormat::Float, 32) => {
                let mut data = wav.samples::<f32>().collect::<hound::Result<Vec<_>>>()?;
                ensure!(data.iter().all(|v| v.is_finite()), "non-finite WAV samples");
                for v in &mut data {
                    *v = v.clamp(-1., 1.);
                }
                data
            }
            _ => anyhow::bail!("expected 8/16/24/32-bit PCM or 32-bit float WAV"),
        };
        Ok(Self(SamplesBuffer::new(
            spec.channels,
            spec.sample_rate,
            data,
        )))
    }

    pub fn impact() -> &'static Self {
        static CLIP: LazyLock<AudioClip> = LazyLock::new(|| {
            AudioClip::read_wav(&include_bytes!("../../assets/audio/impact.wav")[..])
                .expect("bundled impact clip")
        });
        &CLIP
    }
}
