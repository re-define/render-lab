//! Main-thread handles for short 2D effects. The device owns the sample clock.
mod clip;
use anyhow::{Context, Result, ensure};
pub use clip::AudioClip;
use rodio::{OutputStream, OutputStreamBuilder, Sink, mixer::Mixer};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_VOICES: usize = 16;
pub type VoiceId = u64;

enum Output {
    Device {
        stream: OutputStream,
        lost: Arc<AtomicBool>,
    },
    #[cfg(test)]
    Memory(Mixer),
}
impl Output {
    fn open() -> Result<Self> {
        let lost = Arc::new(AtomicBool::new(false));
        let signal = lost.clone();
        let mut stream = OutputStreamBuilder::from_default_device()
            .context("select default audio output")?
            .with_error_callback(move |_| signal.store(true, Ordering::Release))
            .open_stream_or_fallback()
            .context("open audio stream")?;
        stream.log_on_drop(false);
        Ok(Self::Device { stream, lost })
    }
    fn mixer(&self) -> &Mixer {
        match self {
            Self::Device { stream, .. } => stream.mixer(),
            #[cfg(test)]
            Self::Memory(mixer) => mixer,
        }
    }
}

pub struct Audio {
    voices: BTreeMap<VoiceId, Sink>,
    output: Option<Output>,
    next_id: VoiceId,
    volume: f32,
    paused: bool,
    scene_cues: bool,
    last_error: Option<String>,
}
impl Default for Audio {
    fn default() -> Self {
        Self {
            voices: BTreeMap::new(),
            output: None,
            next_id: 1,
            volume: 0.35,
            paused: false,
            scene_cues: false,
            last_error: None,
        }
    }
}
impl Audio {
    pub fn set_scene_cues(&mut self, enabled: bool) {
        self.scene_cues = enabled;
    }

    /// Retire finished voices and release a disconnected device on the main thread.
    pub fn update(&mut self) {
        if matches!(&self.output,Some(Output::Device {lost,..}) if lost.load(Ordering::Acquire)) {
            self.stop_all();
            self.output = None;
            self.last_error = Some("audio output interrupted, try audio play again".into());
        } else {
            self.voices.retain(|_, sink| !sink.empty());
        }
    }
    pub fn play(&mut self, clip: &AudioClip) -> Result<VoiceId> {
        self.update();
        ensure!(
            self.voices.len() < MAX_VOICES,
            "all {MAX_VOICES} audio voices are in use"
        );
        let output = match &self.output {
            Some(output) => output,
            None => match Output::open() {
                Ok(output) => self.output.insert(output),
                Err(e) => {
                    self.last_error = Some(format!("{e:#}"));
                    return Err(e);
                }
            },
        };
        let id = self.next_id;
        self.next_id = id.checked_add(1).context("voice ID exhausted")?;
        // Queue the clip before connecting so the mixer sees its format on the first pull.
        let (sink, queue) = Sink::new();
        sink.set_volume(self.volume);
        if self.paused {
            sink.pause();
        }
        sink.append(clip.0.clone());
        output.mixer().add(queue);
        self.voices.insert(id, sink);
        self.last_error = None;
        Ok(id)
    }
    /// Feedback for an already committed action. No side effects in unattended runs.
    pub(crate) fn scene_impact(&mut self) -> Result<Option<VoiceId>> {
        if self.scene_cues {
            self.play(AudioClip::impact()).map(Some)
        } else {
            Ok(None)
        }
    }
    pub fn set_volume(&mut self, value: f32) -> Result<()> {
        ensure!((0. ..=1.).contains(&value), "audio volume must be in 0..=1");
        self.volume = value;
        for sink in self.voices.values() {
            sink.set_volume(value);
        }
        Ok(())
    }
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        for sink in self.voices.values() {
            if paused { sink.pause() } else { sink.play() }
        }
    }
    pub fn stop(&mut self, id: VoiceId) -> Result<()> {
        self.update();
        self.voices
            .remove(&id)
            .with_context(|| format!("no active audio voice {id}"))?
            .stop();
        Ok(())
    }
    pub fn stop_all(&mut self) {
        // Dropping a Rodio sink requests stop and releases its queue.
        self.voices.clear();
    }
    pub fn status(&mut self) -> String {
        self.update();
        let device = match &self.output {
            Some(Output::Device { stream, .. }) => format!(
                "{} Hz, {} ch",
                stream.config().sample_rate(),
                stream.config().channel_count()
            ),
            #[cfg(test)]
            Some(Output::Memory(_)) => "memory".into(),
            None => "closed".into(),
        };
        let ids: Vec<_> = self.voices.keys().copied().collect();
        let mut status = format!(
            "Audio {device} · voices {}/{MAX_VOICES} {ids:?} · volume {:.2} · {} · scene cues {}",
            ids.len(),
            self.volume,
            if self.paused { "paused" } else { "playing" },
            if self.scene_cues { "on" } else { "off" }
        );
        if let Some(err) = &self.last_error {
            status.push_str(" · ");
            status.push_str(err);
        }
        status
    }
}

#[cfg(test)]
mod tests;
