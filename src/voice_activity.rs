//! Local speech gate. VAD never trims words or chooses recording boundaries.
use webrtc_vad::{SampleRate, Vad, VadMode};

pub struct Detector {
    vad: Vad,
    pending: Vec<f32>,
    rate: u32,
    channels: u16,
    votes: std::collections::VecDeque<bool>,
}

impl Detector {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            vad: Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive),
            pending: Vec::new(),
            rate,
            channels,
            votes: Default::default(),
        }
    }

    pub fn observe(&mut self, data: &[f32], threshold: f32) -> bool {
        self.pending.extend_from_slice(data);
        let channels = self.channels as usize;
        let count = (self.rate as usize / 50).max(1) * channels;
        let mut confirmed = false;
        for raw in self.pending.chunks_exact(count) {
            let mono: Vec<f32> = raw
                .chunks_exact(channels)
                .map(|s| s.iter().sum::<f32>() / channels as f32)
                .collect();
            // Convert each complete 20 ms frame, including 44.1 kHz input.
            let mut pcm = [0i16; 320];
            for (i, out) in pcm.iter_mut().enumerate() {
                let pos = i as f32 * mono.len() as f32 / 320.0;
                let a = pos as usize;
                let b = (a + 1).min(mono.len() - 1);
                let value = mono[a] + (mono[b] - mono[a]) * (pos - a as f32);
                *out = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            let energy = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
            let speech = self.vad.is_voice_segment(&pcm).unwrap_or(false) && energy >= threshold;
            self.votes.push_back(speech);
            if self.votes.len() > 25 {
                self.votes.pop_front();
            }
            // At least 200 ms of speech in 500 ms; one click cannot reset idle.
            confirmed |= speech && self.votes.iter().filter(|&&v| v).count() >= 10;
        }
        let consumed = self.pending.len() / count * count;
        self.pending.drain(..consumed);
        confirmed
    }
}

pub fn wav_has_speech(wav: &[u8]) -> anyhow::Result<bool> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(wav))?;
    let spec = reader.spec();
    let samples = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Detector::new(spec.sample_rate, spec.channels).observe(&samples, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_and_isolated_clicks_are_not_speech() {
        for (rate, channels) in [(16000, 1), (44100, 2), (48000, 1)] {
            let mut d = Detector::new(rate, channels);
            for _ in 0..70 {
                let mut data = vec![0.0; rate as usize * channels as usize];
                data[0] = 0.9;
                assert!(!d.observe(&data, 0.008));
            }
        }
    }
    #[test]
    fn spoken_mode_assets_pass_even_with_small_callback_blocks() {
        for wav in [
            include_bytes!("../assets/voices/local.wav").as_slice(),
            include_bytes!("../assets/voices/groq.wav").as_slice(),
        ] {
            assert!(wav_has_speech(wav).unwrap());
            let mut r = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap();
            let mut d = Detector::new(r.spec().sample_rate, r.spec().channels);
            let samples: Vec<f32> = r
                .samples::<i16>()
                .map(|s| s.unwrap() as f32 / 32768.0)
                .collect();
            let mut detected = false;
            for block in samples.chunks(127) {
                detected |= d.observe(block, 0.008);
            }
            assert!(detected);
        }
    }
}
