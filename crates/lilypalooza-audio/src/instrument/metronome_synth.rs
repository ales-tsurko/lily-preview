use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::instrument::{
    InstrumentProcessor, MidiEvent, ParameterDescriptor, Processor, ProcessorDescriptor,
    ProcessorState, ProcessorStateError, SmoothedAudioValue,
};

const MIN_GAIN_DB: f32 = -36.0;
const MAX_GAIN_DB: f32 = 6.0;
const GAIN_RANGE_DB: f32 = MAX_GAIN_DB - MIN_GAIN_DB;
const DEFAULT_GAIN_DB: f32 = -12.0;
const DEFAULT_GAIN_NORMALIZED: f32 = (DEFAULT_GAIN_DB - MIN_GAIN_DB) / GAIN_RANGE_DB;
const DEFAULT_PITCH: f32 = 0.5;

#[derive(Debug, Clone)]
pub(crate) struct SharedMetronomeState {
    gain_db_bits: Arc<AtomicU32>,
    pitch_bits: Arc<AtomicU32>,
}

impl Default for SharedMetronomeState {
    fn default() -> Self {
        Self::new(DEFAULT_GAIN_DB, DEFAULT_PITCH)
    }
}

impl SharedMetronomeState {
    pub(crate) fn new(gain_db: f32, pitch: f32) -> Self {
        Self {
            gain_db_bits: Arc::new(AtomicU32::new(gain_db.to_bits())),
            pitch_bits: Arc::new(AtomicU32::new(pitch.to_bits())),
        }
    }

    pub(crate) fn set_gain_db(&self, gain_db: f32) {
        self.gain_db_bits
            .store(gain_db.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_pitch(&self, pitch: f32) {
        self.pitch_bits.store(pitch.to_bits(), Ordering::Relaxed);
    }

    fn gain_db(&self) -> f32 {
        f32::from_bits(self.gain_db_bits.load(Ordering::Relaxed))
    }

    fn pitch(&self) -> f32 {
        f32::from_bits(self.pitch_bits.load(Ordering::Relaxed))
    }
}

const PARAMS: &[ParameterDescriptor] = &[
    ParameterDescriptor {
        id: "gain_db",
        name: "Gain",
        default: DEFAULT_GAIN_NORMALIZED,
    },
    ParameterDescriptor {
        id: "pitch",
        name: "Pitch",
        default: DEFAULT_PITCH,
    },
];

pub(crate) const DESCRIPTOR: &ProcessorDescriptor = &ProcessorDescriptor {
    name: "Metronome",
    params: PARAMS,
    editor: None,
};

pub(crate) struct MetronomeProcessor {
    shared: SharedMetronomeState,
    sample_rate: f32,
    phase: f32,
    body_env: f32,
    noise_env: f32,
    frequency_hz: f32,
    body_level: f32,
    transient_level: f32,
    noise_state: u32,
    noise_prev: f32,
    noise_hp: f32,
    gain: SmoothedAudioValue,
}

impl MetronomeProcessor {
    pub(crate) fn new(sample_rate: f32, shared: SharedMetronomeState) -> Self {
        let gain = knyst::db_to_amplitude(shared.gain_db());
        Self {
            shared,
            sample_rate: sample_rate.max(1.0),
            phase: 0.0,
            body_env: 0.0,
            noise_env: 0.0,
            frequency_hz: 1200.0,
            body_level: 0.0,
            transient_level: 0.0,
            noise_state: 0x1234_5678,
            noise_prev: 0.0,
            noise_hp: 0.0,
            gain: SmoothedAudioValue::new(gain, sample_rate as usize),
        }
    }

    fn trigger(&mut self, accent: bool) {
        let pitch = self.shared.pitch().clamp(0.0, 1.0);
        let base_hz = 700.0 * 2.0_f32.powf(pitch * 1.65);
        self.frequency_hz = if accent { base_hz * 1.12 } else { base_hz };
        self.body_env = 1.0;
        self.noise_env = 1.0;
        self.body_level = if accent { 0.95 } else { 0.72 };
        self.transient_level = if accent { 0.34 } else { 0.24 };
    }

    fn next_noise(&mut self) -> f32 {
        self.noise_state ^= self.noise_state << 13;
        self.noise_state ^= self.noise_state >> 17;
        self.noise_state ^= self.noise_state << 5;
        let unit = self.noise_state as f32 / u32::MAX as f32;
        unit * 2.0 - 1.0
    }
}

impl Processor for MetronomeProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        DESCRIPTOR
    }

    fn set_param(&mut self, id: &str, normalized: f32) -> bool {
        match id {
            "gain_db" => {
                self.shared
                    .set_gain_db(MIN_GAIN_DB + normalized.clamp(0.0, 1.0) * GAIN_RANGE_DB);
                true
            }
            "pitch" => {
                self.shared.set_pitch(normalized.clamp(0.0, 1.0));
                true
            }
            _ => false,
        }
    }

    fn get_param(&self, id: &str) -> Option<f32> {
        match id {
            "gain_db" => {
                Some(((self.shared.gain_db() - MIN_GAIN_DB) / GAIN_RANGE_DB).clamp(0.0, 1.0))
            }
            "pitch" => Some(self.shared.pitch().clamp(0.0, 1.0)),
            _ => None,
        }
    }

    fn save_state(&self) -> ProcessorState {
        ProcessorState::default()
    }

    fn load_state(&mut self, _state: &ProcessorState) -> Result<(), ProcessorStateError> {
        Ok(())
    }

    fn reset(&mut self) {
        self.body_env = 0.0;
        self.noise_env = 0.0;
        self.phase = 0.0;
        self.noise_prev = 0.0;
        self.noise_hp = 0.0;
    }
}

impl InstrumentProcessor for MetronomeProcessor {
    fn handle_midi(&mut self, event: MidiEvent) {
        if let MidiEvent::NoteOn { velocity, .. } = event
            && velocity > 0
        {
            self.trigger(velocity >= 120);
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.gain
            .set_target(knyst::db_to_amplitude(self.shared.gain_db()));
        let body_decay = (-1.0 / (self.sample_rate * 0.020)).exp();
        let noise_decay = (-1.0 / (self.sample_rate * 0.005)).exp();
        let hp_feedback = 0.88 - 0.10 * self.shared.pitch().clamp(0.0, 1.0);
        let phase_inc = TAU * self.frequency_hz / self.sample_rate.max(1.0);

        for (left_sample, right_sample) in left.iter_mut().zip(right.iter_mut()) {
            if self.body_env <= 1.0e-4 && self.noise_env <= 1.0e-4 {
                *left_sample = 0.0;
                *right_sample = 0.0;
                continue;
            }

            self.phase = (self.phase + phase_inc).rem_euclid(TAU);
            self.body_env *= body_decay;
            self.noise_env *= noise_decay;

            let sine = self.phase.sin() * self.body_env * self.body_level;
            let noise = self.next_noise();
            self.noise_hp = noise - self.noise_prev + hp_feedback * self.noise_hp;
            self.noise_prev = noise;
            let transient = self.noise_hp * self.noise_env * self.transient_level;
            let gain = self.gain.next_sample();
            let sample = (sine + transient) * gain;
            *left_sample = sample;
            *right_sample = sample;
        }
    }

    fn is_sleeping(&self) -> bool {
        self.body_env <= 1.0e-4 && self.noise_env <= 1.0e-4
    }
}

#[cfg(test)]
mod tests {
    use super::{MetronomeProcessor, SharedMetronomeState};
    use crate::instrument::{InstrumentProcessor, MidiEvent};

    #[test]
    fn metronome_trigger_outputs_signal() {
        let shared = SharedMetronomeState::default();
        let mut processor = MetronomeProcessor::new(48_000.0, shared);
        let mut left = [0.0; 64];
        let mut right = [0.0; 64];
        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        processor.render(&mut left, &mut right);
        assert!(
            left.iter()
                .chain(right.iter())
                .any(|sample| sample.abs() > 1.0e-6)
        );
    }

    #[test]
    fn metronome_pitch_changes_output_shape() {
        let low = SharedMetronomeState::new(-12.0, 0.1);
        let high = SharedMetronomeState::new(-12.0, 0.9);
        let mut low_processor = MetronomeProcessor::new(48_000.0, low);
        let mut high_processor = MetronomeProcessor::new(48_000.0, high);
        let mut low_left = [0.0; 64];
        let mut high_left = [0.0; 64];
        let mut scratch = [0.0; 64];

        for processor in [&mut low_processor, &mut high_processor] {
            processor.handle_midi(MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            });
        }
        low_processor.render(&mut low_left, &mut scratch);
        high_processor.render(&mut high_left, &mut scratch);

        assert_ne!(low_left, high_left);
    }
}
