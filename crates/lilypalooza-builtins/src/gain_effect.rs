use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lilypalooza_audio::instrument::{
    EffectRuntimeContext, EffectRuntimeSpec, RuntimeBinding, RuntimeFactoryError,
};
use lilypalooza_audio::{
    BUILTIN_GAIN_ID, Controller, ControllerError, EffectProcessor, ParameterDescriptor, Processor,
    ProcessorDescriptor, ProcessorKind, ProcessorState, ProcessorStateError, SlotState,
    SmoothedAudioValue,
};

pub(crate) const MIN_GAIN_DB: f32 = -60.0;
const MAX_GAIN_DB: f32 = 12.0;
pub(crate) const GAIN_RANGE_DB: f32 = MAX_GAIN_DB - MIN_GAIN_DB;
const DEFAULT_GAIN_DB: f32 = 0.0;
const DEFAULT_GAIN_NORMALIZED: f32 = (DEFAULT_GAIN_DB - MIN_GAIN_DB) / GAIN_RANGE_DB;

const GAIN_EFFECT_PARAMS: &[ParameterDescriptor] = &[ParameterDescriptor {
    id: "gain_db",
    name: "Gain",
    default: DEFAULT_GAIN_NORMALIZED,
}];

pub(crate) const DESCRIPTOR: &ProcessorDescriptor = &ProcessorDescriptor {
    name: "Gain",
    params: GAIN_EFFECT_PARAMS,
    editor: None,
};

fn descriptor() -> &'static ProcessorDescriptor {
    DESCRIPTOR
}

fn is_slot(slot: &SlotState) -> bool {
    matches!(
        slot.kind,
        ProcessorKind::BuiltIn { ref processor_id } if processor_id == BUILTIN_GAIN_ID
    )
}

#[derive(Debug, Clone)]
struct GainBinding {
    shared: SharedGainState,
}

impl RuntimeBinding for GainBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(GainController {
            shared: self.shared.clone(),
        })
    }
}

struct GainController {
    shared: SharedGainState,
}

impl Controller for GainController {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        descriptor()
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        if id != "gain_db" {
            return Err(ControllerError::UnknownParameter(id.to_string()));
        }
        Ok(((self.shared.gain_db() - MIN_GAIN_DB) / GAIN_RANGE_DB).clamp(0.0, 1.0))
    }

    fn set_param(&self, id: &str, normalized: f32) -> Result<(), ControllerError> {
        if id != "gain_db" {
            return Err(ControllerError::UnknownParameter(id.to_string()));
        }
        self.shared
            .set_gain_db(MIN_GAIN_DB + normalized.clamp(0.0, 1.0) * GAIN_RANGE_DB);
        Ok(())
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        let normalized = ((self.shared.gain_db() - MIN_GAIN_DB) / GAIN_RANGE_DB).clamp(0.0, 1.0);
        let mut processor = GainEffectProcessor::from_state(&ProcessorState::default())
            .map_err(|error| ControllerError::Backend(error.to_string()))?;
        if !processor.set_param("gain_db", normalized) {
            return Err(ControllerError::UnknownParameter("gain_db".to_string()));
        }
        Ok(processor.save_state())
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        let processor = GainEffectProcessor::from_state(state)
            .map_err(|error| ControllerError::Backend(error.to_string()))?;
        self.shared.set_gain_db(
            processor
                .get_param("gain_db")
                .ok_or_else(|| ControllerError::UnknownParameter("gain_db".to_string()))
                .map(|normalized| MIN_GAIN_DB + normalized * GAIN_RANGE_DB)?,
        );
        Ok(())
    }
}

pub(crate) fn create_runtime(
    slot: &SlotState,
    context: &EffectRuntimeContext,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    if !is_slot(slot) {
        return Ok(None);
    }
    let shared = SharedGainState::default();
    let processor = Box::new(GainEffectProcessor::from_state_with_shared_and_sample_rate(
        &slot.state,
        Some(shared.clone()),
        context.sample_rate,
    )?);
    Ok(Some(EffectRuntimeSpec {
        processor,
        binding: Some(Box::new(GainBinding { shared })),
    }))
}

#[derive(Debug, Clone)]
pub(crate) struct SharedGainState {
    gain_db_bits: Arc<AtomicU32>,
}

impl Default for SharedGainState {
    fn default() -> Self {
        Self::new(DEFAULT_GAIN_DB)
    }
}

impl SharedGainState {
    pub(crate) fn new(gain_db: f32) -> Self {
        Self {
            gain_db_bits: Arc::new(AtomicU32::new(gain_db.to_bits())),
        }
    }

    pub(crate) fn set_gain_db(&self, gain_db: f32) {
        self.gain_db_bits
            .store(gain_db.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn gain_db(&self) -> f32 {
        f32::from_bits(self.gain_db_bits.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct GainEffectState {
    gain_db: f32,
}

impl Default for GainEffectState {
    fn default() -> Self {
        Self {
            gain_db: DEFAULT_GAIN_DB,
        }
    }
}

pub(crate) struct GainEffectProcessor {
    state: GainEffectState,
    shared: Option<SharedGainState>,
    gain: SmoothedAudioValue,
    sample_rate: usize,
}

impl GainEffectProcessor {
    pub(crate) fn from_state(state: &ProcessorState) -> Result<Self, ProcessorStateError> {
        Self::from_state_with_shared(state, None)
    }

    pub(crate) fn from_state_with_shared(
        state: &ProcessorState,
        shared: Option<SharedGainState>,
    ) -> Result<Self, ProcessorStateError> {
        Self::from_state_with_shared_and_sample_rate(state, shared, 44_100)
    }

    pub(crate) fn from_state_with_shared_and_sample_rate(
        state: &ProcessorState,
        shared: Option<SharedGainState>,
        sample_rate: usize,
    ) -> Result<Self, ProcessorStateError> {
        let state = if state.0.is_empty() {
            GainEffectState::default()
        } else {
            bincode::deserialize(&state.0)
                .map_err(|err| ProcessorStateError::Decode(err.to_string()))?
        };
        if let Some(shared) = &shared {
            shared.set_gain_db(state.gain_db);
        }
        Ok(Self {
            gain: SmoothedAudioValue::new(knyst::db_to_amplitude(state.gain_db), sample_rate),
            sample_rate,
            state,
            shared,
        })
    }
}

impl Processor for GainEffectProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        descriptor()
    }

    fn set_param(&mut self, id: &str, normalized: f32) -> bool {
        if id != "gain_db" {
            return false;
        }
        self.state.gain_db = MIN_GAIN_DB + normalized.clamp(0.0, 1.0) * GAIN_RANGE_DB;
        if let Some(shared) = &self.shared {
            shared.set_gain_db(self.state.gain_db);
        }
        true
    }

    fn get_param(&self, id: &str) -> Option<f32> {
        let gain_db = self
            .shared
            .as_ref()
            .map_or(self.state.gain_db, SharedGainState::gain_db);
        (id == "gain_db").then_some(((gain_db - MIN_GAIN_DB) / GAIN_RANGE_DB).clamp(0.0, 1.0))
    }

    fn save_state(&self) -> ProcessorState {
        match bincode::serialize(&self.state) {
            Ok(state) => ProcessorState(state),
            Err(error) => {
                eprintln!("Gain state serialization failed: {error}");
                ProcessorState::default()
            }
        }
    }

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        *self = Self::from_state_with_shared_and_sample_rate(
            state,
            self.shared.clone(),
            self.sample_rate,
        )?;
        Ok(())
    }

    fn reset(&mut self) {}
}

impl EffectProcessor for GainEffectProcessor {
    fn process(
        &mut self,
        in_left: &[f32],
        in_right: &[f32],
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        self.gain.set_target(knyst::db_to_amplitude(
            self.shared
                .as_ref()
                .map_or(self.state.gain_db, SharedGainState::gain_db),
        ));
        for frame in 0..out_left.len() {
            let gain = self.gain.next_sample();
            out_left[frame] = in_left[frame] * gain;
            out_right[frame] = in_right[frame] * gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GAIN_RANGE_DB, GainEffectProcessor, MIN_GAIN_DB};
    use lilypalooza_audio::{EffectProcessor, Processor, ProcessorState};

    #[test]
    fn gain_effect_scales_expected_signal() {
        let mut processor =
            GainEffectProcessor::from_state(&ProcessorState::default()).expect("processor");

        let left_in = [0.0, 0.25, -0.5, 1.0];
        let right_in = [1.0, -0.5, 0.25, 0.0];
        let mut left_out = [0.0; 4];
        let mut right_out = [0.0; 4];
        processor.process(&left_in, &right_in, &mut left_out, &mut right_out);

        for index in 0..left_in.len() {
            assert!((left_out[index] - left_in[index]).abs() < 1.0e-6);
            assert!((right_out[index] - right_in[index]).abs() < 1.0e-6);
        }
    }

    #[test]
    fn gain_effect_smooths_parameter_changes() {
        let mut processor =
            GainEffectProcessor::from_state(&ProcessorState::default()).expect("processor");
        processor.set_param("gain_db", (-6.0 - MIN_GAIN_DB) / GAIN_RANGE_DB);

        let input = [1.0; 8];
        let mut output = [0.0; 8];
        let mut right = [0.0; 8];
        processor.process(&input, &input, &mut output, &mut right);

        let target = knyst::db_to_amplitude(-6.0);
        assert!(output[0] < 1.0);
        assert!(output[0] > target);
        assert!(output[7] < output[0]);
        assert!(output[7] > target);
    }
}
