#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use knyst::controller::KnystCommands;
use knyst::graph::SimultaneousChanges;
use knyst::handles::HandleData;
use knyst::prelude::{Beats, BlockSize, GenState, Sample, graph_output, handle, impl_gen};

use super::{AudioEngine, AudioEngineOptions, wait_for_transport_reset_to};
use crate::instrument::SlotState;
use crate::instrument::registry::Entry;
use crate::instrument::{
    BUILTIN_GAIN_ID, BUILTIN_SOUNDFONT_ID, Controller, ControllerError, EffectProcessor,
    EffectRuntimeSpec, InstrumentProcessor, InstrumentProcessorNode, InstrumentRuntimeContext,
    InstrumentRuntimeSpec, MidiEvent, Processor, ProcessorDescriptor, ProcessorKind,
    ProcessorState, ProcessorStateError, RuntimeBinding, RuntimeFactoryError, registry,
};
use crate::mixer::{INSTRUMENT_TRACK_COUNT, MixerState, SlotAddress, TrackId};
use crate::test_utils::{
    SharedTestBackend, SharedTestBackendHandle, TestBackend, delayed_note_midi_bytes,
    four_track_midi_bytes, simple_midi_bytes, sustained_note_midi_bytes, test_soundfont_resource,
};
use crate::transport::Transport;

struct ScheduledValueGen;
struct TestNoteProcessor {
    active: bool,
    program: Arc<AtomicU32>,
}
struct TestMuteEffect;

#[derive(Clone)]
struct TestGainBinding {
    normalized_bits: Arc<AtomicU32>,
}

struct TestSoundfontBinding {
    program: Arc<AtomicU32>,
}

struct TestSoundfontState {
    soundfont_id: String,
    program: u8,
}

fn settle_backend(backend: &SharedTestBackendHandle) {
    for _ in 0..50 {
        backend.process_block();
        thread::sleep(Duration::from_millis(1));
    }
}

fn output_peak(backend: &SharedTestBackendHandle) -> f32 {
    backend
        .output_channel(0)
        .into_iter()
        .chain(backend.output_channel(1))
        .map(f32::abs)
        .fold(0.0, f32::max)
}

fn raw_transport_play(engine: &mut AudioEngine) {
    engine.commands.transport_play();
    super::wait_for_transport_settled(&mut engine.commands);
}

fn soundfont_slot(program: u8) -> SlotState {
    register_test_processors();
    SlotState::built_in(
        BUILTIN_SOUNDFONT_ID,
        encode_test_soundfont_state("default", program),
    )
}

fn gain_effect_slot() -> SlotState {
    register_test_processors();
    SlotState::built_in(BUILTIN_GAIN_ID, ProcessorState::default())
}

fn mute_effect_slot(bypassed: bool) -> SlotState {
    register_test_processors();
    let mut slot = SlotState::built_in("test-mute", ProcessorState::default());
    slot.bypassed = bypassed;
    slot
}

fn register_test_processors() {
    registry::register([
        Entry::builtin_instrument(
            BUILTIN_SOUNDFONT_ID,
            "SoundFont",
            soundfont_descriptor(),
            create_test_soundfont_runtime,
        ),
        Entry::builtin_effect(
            BUILTIN_GAIN_ID,
            "Gain",
            gain_descriptor(),
            create_test_gain_runtime,
        ),
        Entry::builtin_effect(
            "test-mute",
            "Mute",
            mute_descriptor(),
            create_test_mute_runtime,
        ),
    ]);
}

fn soundfont_descriptor() -> &'static ProcessorDescriptor {
    static DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
        name: "SoundFont",
        params: &[],
        editor: None,
    };
    &DESCRIPTOR
}

fn gain_descriptor() -> &'static ProcessorDescriptor {
    static DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
        name: "Gain",
        params: &[],
        editor: None,
    };
    &DESCRIPTOR
}

fn mute_descriptor() -> &'static ProcessorDescriptor {
    static DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
        name: "Mute",
        params: &[],
        editor: None,
    };
    &DESCRIPTOR
}

fn create_test_soundfont_runtime(
    slot: &SlotState,
    context: &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
    let Some(state) = slot.decode_built_in(BUILTIN_SOUNDFONT_ID, decode_test_soundfont_state)?
    else {
        return Ok(None);
    };
    if !context.soundfonts.contains_key(&state.soundfont_id) {
        return Ok(None);
    }
    let program = Arc::new(AtomicU32::new(u32::from(state.program)));
    Ok(Some(InstrumentRuntimeSpec {
        processor: Box::new(TestNoteProcessor {
            active: false,
            program: Arc::clone(&program),
        }),
        binding: Box::new(TestSoundfontBinding { program }),
    }))
}

fn encode_test_soundfont_state(soundfont_id: &str, program: u8) -> ProcessorState {
    let mut bytes = vec![program];
    bytes.extend_from_slice(soundfont_id.as_bytes());
    ProcessorState(bytes)
}

fn decode_test_soundfont_state(
    state: &ProcessorState,
) -> Result<TestSoundfontState, ProcessorStateError> {
    let Some((&program, id_bytes)) = state.0.split_first() else {
        return Err(ProcessorStateError::Decode(
            "test SoundFont state is empty".to_string(),
        ));
    };
    let soundfont_id = std::str::from_utf8(id_bytes)
        .map_err(|error| ProcessorStateError::Decode(error.to_string()))?
        .to_string();
    Ok(TestSoundfontState {
        soundfont_id,
        program,
    })
}

fn create_test_gain_runtime(
    slot: &SlotState,
    _context: &crate::instrument::EffectRuntimeContext,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    if !matches!(
        slot.kind,
        ProcessorKind::BuiltIn { ref processor_id } if processor_id == BUILTIN_GAIN_ID
    ) {
        return Ok(None);
    }
    let binding = TestGainBinding {
        normalized_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
    };
    Ok(Some(EffectRuntimeSpec {
        processor: Box::new(TestGainEffect {
            binding: binding.clone(),
        }),
        binding: Some(Box::new(binding)),
    }))
}

fn create_test_mute_runtime(
    slot: &SlotState,
    _context: &crate::instrument::EffectRuntimeContext,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    if !matches!(
        slot.kind,
        ProcessorKind::BuiltIn { ref processor_id } if processor_id == "test-mute"
    ) {
        return Ok(None);
    }
    Ok(Some(EffectRuntimeSpec {
        processor: Box::new(TestMuteEffect),
        binding: None,
    }))
}

fn render_soundfont_program(program: u8) -> Vec<Sample> {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(program))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }
    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return backend_handle.output_channel(0);
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("engine end-to-end path produced silence");
}

#[test]
fn controller_resolves_track_instrument_slot() {
    let (backend, _backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(12))
            .expect("track should accept soundfont instrument");
    }

    let controller = engine
        .controller(SlotAddress {
            strip_index: 1,
            slot_index: 0,
        })
        .expect("controller lookup should succeed")
        .expect("soundfont controller should exist");

    assert_eq!(controller.descriptor().name, "SoundFont");
    assert!(
        (controller.get_param("program").expect("program param") - (12.0 / 127.0)).abs() < 1.0e-6
    );
}

#[test]
fn controller_resolves_track_gain_effect_slot() {
    register_test_processors();
    let (backend, _backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    crate::instrument::BUILTIN_GAIN_ID,
                    ProcessorState::default(),
                )],
            )
            .expect("track should accept gain effect");
    }

    let controller = engine
        .controller(SlotAddress {
            strip_index: 1,
            slot_index: 1,
        })
        .expect("controller lookup should succeed")
        .expect("gain controller should exist");

    assert_eq!(controller.descriptor().name, "Gain");
    controller
        .set_param("gain_db", 0.25)
        .expect("gain set should succeed");
    assert!((controller.get_param("gain_db").expect("gain param") - 0.25).abs() < 1.0e-6);
}

#[test]
fn bypassed_track_gain_effect_still_exposes_controller() {
    register_test_processors();
    let (backend, _backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_effects(
                TrackId(0),
                vec![SlotState::built_in(
                    crate::instrument::BUILTIN_GAIN_ID,
                    ProcessorState::default(),
                )],
            )
            .expect("track should accept gain effect");
        mixer
            .set_slot_bypassed(
                SlotAddress {
                    strip_index: 1,
                    slot_index: 1,
                },
                true,
            )
            .expect("bypass should succeed");
    }

    let controller = engine
        .controller(SlotAddress {
            strip_index: 1,
            slot_index: 1,
        })
        .expect("controller lookup should succeed")
        .expect("gain controller should exist");

    assert_eq!(controller.descriptor().name, "Gain");
}

#[impl_gen]
impl ScheduledValueGen {
    #[new]
    fn new() -> Self {
        Self
    }

    #[process]
    fn process(&mut self, value: &[Sample], out: &mut [Sample], block_size: BlockSize) -> GenState {
        out[..block_size.0].copy_from_slice(&value[..block_size.0]);
        GenState::Continue
    }
}

impl Processor for TestNoteProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        static DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
            name: "Test Note Processor",
            params: &[],
            editor: None,
        };
        &DESCRIPTOR
    }

    fn set_param(&mut self, _id: &str, _normalized: f32) -> bool {
        false
    }

    fn get_param(&self, _id: &str) -> Option<f32> {
        None
    }

    fn save_state(&self) -> ProcessorState {
        ProcessorState::default()
    }

    fn load_state(&mut self, _state: &ProcessorState) -> Result<(), ProcessorStateError> {
        Ok(())
    }

    fn reset(&mut self) {
        self.active = false;
    }
}

impl InstrumentProcessor for TestNoteProcessor {
    fn handle_midi(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn { .. } => self.active = true,
            MidiEvent::NoteOff { .. }
            | MidiEvent::AllNotesOff { .. }
            | MidiEvent::AllSoundOff { .. } => self.active = false,
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let program = self.program.load(Ordering::Relaxed) as f32 / 127.0;
        let value = if self.active {
            0.25 + program * 0.1
        } else {
            0.0
        };
        left.fill(value);
        right.fill(value);
    }
}

impl RuntimeBinding for TestSoundfontBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(TestSoundfontController {
            program: Arc::clone(&self.program),
        })
    }

    fn update_in_place(&self, slot: &SlotState) -> Result<bool, ProcessorStateError> {
        let Some(state) =
            slot.decode_built_in(BUILTIN_SOUNDFONT_ID, decode_test_soundfont_state)?
        else {
            return Ok(false);
        };
        self.program
            .store(u32::from(state.program), Ordering::Relaxed);
        Ok(true)
    }
}

struct TestSoundfontController {
    program: Arc<AtomicU32>,
}

impl Controller for TestSoundfontController {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        soundfont_descriptor()
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        if id == "program" {
            Ok(self.program.load(Ordering::Relaxed) as f32 / 127.0)
        } else {
            Err(ControllerError::UnknownParameter(id.to_string()))
        }
    }

    fn set_param(&self, id: &str, normalized: f32) -> Result<(), ControllerError> {
        if id == "program" && (0.0..=1.0).contains(&normalized) {
            self.program
                .store((normalized * 127.0).round() as u32, Ordering::Relaxed);
            Ok(())
        } else {
            Err(ControllerError::UnknownParameter(id.to_string()))
        }
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        Ok(ProcessorState::default())
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        if state.0.is_empty() {
            Ok(())
        } else {
            Err(ControllerError::Backend(
                "test soundfont state must be empty".to_string(),
            ))
        }
    }
}

struct TestGainEffect {
    binding: TestGainBinding,
}

impl Processor for TestGainEffect {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        gain_descriptor()
    }

    fn set_param(&mut self, id: &str, normalized: f32) -> bool {
        if id == "gain_db" {
            self.binding
                .normalized_bits
                .store(normalized.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    fn get_param(&self, id: &str) -> Option<f32> {
        (id == "gain_db")
            .then(|| f32::from_bits(self.binding.normalized_bits.load(Ordering::Relaxed)))
    }

    fn save_state(&self) -> ProcessorState {
        ProcessorState::default()
    }

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        if state.0.is_empty() {
            Ok(())
        } else {
            Err(ProcessorStateError::Decode(
                "test gain state must be empty".to_string(),
            ))
        }
    }

    fn reset(&mut self) {}
}

impl EffectProcessor for TestGainEffect {
    fn process(
        &mut self,
        left: &[f32],
        right: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        let gain = f32::from_bits(self.binding.normalized_bits.load(Ordering::Relaxed));
        for (out, input) in left_out.iter_mut().zip(left) {
            *out = *input * gain;
        }
        for (out, input) in right_out.iter_mut().zip(right) {
            *out = *input * gain;
        }
    }
}

impl Processor for TestMuteEffect {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        mute_descriptor()
    }

    fn set_param(&mut self, _id: &str, _normalized: f32) -> bool {
        false
    }

    fn get_param(&self, _id: &str) -> Option<f32> {
        None
    }

    fn save_state(&self) -> ProcessorState {
        ProcessorState::default()
    }

    fn load_state(&mut self, _state: &ProcessorState) -> Result<(), ProcessorStateError> {
        Ok(())
    }

    fn reset(&mut self) {}
}

impl EffectProcessor for TestMuteEffect {
    fn process(
        &mut self,
        left: &[f32],
        right: &[f32],
        left_out: &mut [f32],
        right_out: &mut [f32],
    ) {
        left_out[..left.len()].fill(0.0);
        right_out[..right.len()].fill(0.0);
    }
}

impl RuntimeBinding for TestGainBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(TestGainController {
            binding: self.clone(),
        })
    }
}

struct TestGainController {
    binding: TestGainBinding,
}

impl Controller for TestGainController {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        gain_descriptor()
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        if id == "gain_db" {
            Ok(f32::from_bits(
                self.binding.normalized_bits.load(Ordering::Relaxed),
            ))
        } else {
            Err(ControllerError::UnknownParameter(id.to_string()))
        }
    }

    fn set_param(&self, id: &str, normalized: f32) -> Result<(), ControllerError> {
        if id != "gain_db" {
            return Err(ControllerError::UnknownParameter(id.to_string()));
        }
        self.binding
            .normalized_bits
            .store(normalized.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        Ok(())
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        Ok(ProcessorState::default())
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        if state.0.is_empty() {
            Ok(())
        } else {
            Err(ControllerError::Backend(
                "test gain state must be empty".to_string(),
            ))
        }
    }
}

#[test]
fn engine_starts_with_transport_paused_at_zero() {
    let backend = TestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    let snapshot = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available");

    assert_eq!(
        snapshot.playback_state,
        crate::transport::PlaybackState::Paused
    );
    assert_eq!(snapshot.beats_position, Beats::ZERO);
}

#[test]
fn engine_renders_audio_after_soundfont_and_midi_load() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            let debug = engine.sequencer.debug_state();
            assert!(
                debug.schedule_count <= 4,
                "sequencer should not reschedule excessively: {debug:?}"
            );
            assert!(
                debug.reset_count <= 1,
                "sequencer should not keep resetting notes: {debug:?}"
            );
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("engine end-to-end path produced silence");
}

#[test]
fn engine_switches_soundfont_programs_at_track_level() {
    let piano = render_soundfont_program(0);
    let violin = render_soundfont_program(40);

    assert!(
        piano
            .iter()
            .zip(violin.iter())
            .any(|(a, b)| (a - b).abs() > 1.0e-6),
        "different track-level SoundFont programs rendered the same output"
    );
}

#[test]
fn loading_soundfont_after_track_assignment_restores_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept pending soundfont instrument");
    }
    settle_backend(&backend_handle);

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("late soundfont load produced track signal without master output");
}

#[test]
fn selecting_soundfont_program_before_playback_produces_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    engine
        .sequencer()
        .replace_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    engine.transport().play();
    settle_backend(&backend_handle);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("pre-play program selection produced silence at master output");
}

#[test]
fn selecting_soundfont_program_during_playback_produces_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");
    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }
    settle_backend(&backend_handle);
    engine.transport().play();
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("during-playback program selection produced silence at master output");
}

#[test]
fn persistent_engine_reload_then_preplay_program_selection_produces_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine
        .replace_score_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    settle_backend(&backend_handle);
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("persistent-engine reload followed by pre-play program selection produced silence");
}

#[test]
fn persistent_engine_reload_then_live_program_selection_reaches_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine
        .replace_score_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    settle_backend(&backend_handle);
    engine.transport().play();
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("persistent-engine reload followed by live program selection produced silence");
}

#[test]
fn persistent_engine_live_track_assignment_allows_direct_midi_to_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine
        .replace_score_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    settle_backend(&backend_handle);
    engine.transport().play();
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("direct MIDI into live-assigned persistent-engine track did not reach master");
}

#[test]
fn app_lifecycle_preplay_program_selection_reaches_master_after_score_replace() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont load should work");
    }

    engine
        .replace_score_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        for track_index in 0..INSTRUMENT_TRACK_COUNT {
            mixer
                .set_track_name(
                    TrackId(track_index as u16),
                    format!("Track {}", track_index + 1),
                )
                .expect("track rename should work");
        }
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    settle_backend(&backend_handle);
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("app-style preplay program selection produced no master output");
}

#[test]
fn app_lifecycle_live_program_selection_reaches_master_after_score_replace() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont load should work");
    }

    engine
        .replace_score_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        for track_index in 0..INSTRUMENT_TRACK_COUNT {
            mixer
                .set_track_name(
                    TrackId(track_index as u16),
                    format!("Track {}", track_index + 1),
                )
                .expect("track rename should work");
        }
    }

    settle_backend(&backend_handle);
    engine.transport().play();
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("app-style live program selection produced no master output");
}

#[test]
fn app_lifecycle_preplay_program_selection_without_backend_settle_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont load should work");
    }

    engine
        .replace_score_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("preplay program selection without backend settle produced no master output");
}

#[test]
fn app_lifecycle_live_program_selection_without_backend_settle_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont load should work");
    }

    engine
        .replace_score_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    engine.transport().play();
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("live program selection without backend settle produced no master output");
}

#[test]
fn persistent_engine_reset_then_live_track_assignment_allows_direct_midi_to_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine.transport().pause();
    engine.transport().rewind();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("persistent-engine reset followed by live assignment stayed silent");
}

#[test]
fn persistent_engine_reset_then_live_track_assignment_allows_direct_note_on_to_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine.transport().pause();
    engine.transport().rewind();

    let handle = {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
        engine
            .mixer
            .instrument_handle(TrackId(0))
            .expect("track runtime should expose instrument handle")
    };

    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    super::wait_for_transport_settled(&mut engine.commands);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.context.with_activation(|| {
        handle.note_on(0, 60, 100);
    });

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("persistent-engine reset followed by direct note_on stayed silent");
}

#[test]
fn raw_transport_reset_then_live_track_assignment_allows_direct_note_on_to_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }

    engine.commands.transport_pause();
    super::wait_for_transport_settled(&mut engine.commands);
    engine.commands.transport_seek_to_beats(Beats::ZERO);
    super::wait_for_transport_settled(&mut engine.commands);

    let handle = {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
        engine
            .mixer
            .instrument_handle(TrackId(0))
            .expect("track runtime should expose instrument handle")
    };

    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    super::wait_for_transport_settled(&mut engine.commands);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.context.with_activation(|| {
        handle.note_on(0, 60, 100);
    });

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("raw transport reset followed by direct note_on stayed silent");
}

#[test]
fn pre_play_track_mix_sync_does_not_silence_first_playback() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    engine
        .sequencer()
        .replace_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");
    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
        mixer
            .set_track_muted(TrackId(0), false)
            .expect("mute sync should succeed");
        mixer
            .set_track_soloed(TrackId(0), false)
            .expect("solo sync should succeed");
    }
    settle_backend(&backend_handle);
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("pre-play mute/solo sync silenced first playback");
}

#[test]
fn engine_renders_audio_without_callback_installation() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");

    {
        let mut transport = Transport::new(
            &mut engine.commands,
            Some(&mut engine.mixer),
            Some(&engine.sequencer),
        );
        transport.play();
    }

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("engine without callback produced silence");
}

#[test]
fn engine_renders_audio_for_four_track_midi_with_tempo_track() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        for track_index in 0..4 {
            mixer
                .set_track_instrument(
                    TrackId(track_index as u16),
                    soundfont_slot(track_index as u8),
                )
                .expect("track should accept soundfont instrument");
        }
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&four_track_midi_bytes(480))
        .expect("midi should load");
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!(
        "engine four-track end-to-end path produced silence; debug state: {:?}",
        engine.sequencer.debug_state()
    );
}

#[test]
fn track_rename_does_not_require_graph_settle() {
    let backend = TestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");

    engine
        .mixer()
        .set_track_name(TrackId(0), "Violin")
        .expect("track rename should succeed");
}

#[test]
fn soundfont_load_commits_graph_changes_immediately() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    engine
        .mixer()
        .set_soundfont(test_soundfont_resource())
        .expect("soundfont should load");

    let mut mixer = engine.mixer();
    mixer
        .set_track_instrument(TrackId(0), soundfont_slot(40))
        .expect("track should accept soundfont instrument");

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }
    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("soundfont load did not commit graph changes immediately");
}

#[test]
fn track_instrument_assignment_commits_graph_changes_immediately() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }
    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("track instrument assignment did not commit graph changes immediately");
}

#[test]
fn paused_seek_then_play_starts_from_seek_position() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().seek_beats(1.0);
    let before_play = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available");
    engine.transport().play();

    let mut max_peak = 0.0_f32;

    for _ in 0..128 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        let peak = backend_handle
            .output_channel(0)
            .into_iter()
            .chain(backend_handle.output_channel(1))
            .map(f32::abs)
            .fold(0.0_f32, f32::max);
        max_peak = max_peak.max(peak);
        thread::sleep(Duration::from_millis(2));
    }

    let after_play = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available");
    let debug = engine.sequencer.debug_state();
    panic!(
        "paused seek followed by play produced silence; before_play={before_play:?}; after_play={after_play:?}; debug={debug:?}; max_peak={max_peak}"
    );
}

#[test]
fn paused_seek_then_play_immediate_starts_from_seek_position() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().seek_beats(1.0);
    engine.transport().play_immediate();

    let mut max_peak = 0.0_f32;
    for _ in 0..128 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        let peak = backend_handle
            .output_channel(0)
            .into_iter()
            .chain(backend_handle.output_channel(1))
            .map(f32::abs)
            .fold(0.0_f32, f32::max);
        max_peak = max_peak.max(peak);
        thread::sleep(Duration::from_millis(2));
    }

    let snapshot = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available");
    let debug = engine.sequencer.debug_state();
    panic!(
        "paused seek followed by play_immediate produced silence; snapshot={snapshot:?}; debug={debug:?}; max_peak={max_peak}"
    );
}

#[test]
fn paused_seek_then_play_schedules_note_beyond_initial_jump() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().seek_beats(3.0);
    engine.transport().play();

    let mut max_peak = 0.0_f32;
    for _ in 0..4096 {
        backend_handle.process_block();
        max_peak = max_peak.max(
            backend_handle
                .output_channel(0)
                .into_iter()
                .chain(backend_handle.output_channel(1))
                .map(f32::abs)
                .fold(0.0, f32::max),
        );
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let debug = engine.sequencer.debug_state();
    panic!(
        "paused seek followed by play should reach delayed note after jump; debug={debug:?}; max_peak={max_peak}"
    );
}

#[test]
fn paused_seek_into_sustained_note_chases_note_on() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().seek_beats(2.0);
    engine.transport().play();

    for _ in 0..512 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("seek into sustained note should chase active note on");
}

#[test]
fn seek_while_playing_into_sustained_note_chases_note_on() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 1920))
        .expect("midi should load");
    engine.transport().play();

    for _ in 0..32 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.transport().seek_beats(2.0);

    for _ in 0..512 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("seek while playing into sustained note should chase active note on");
}

#[test]
fn pause_resets_active_notes() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");
    engine.transport().seek_beats(1.5);
    engine.transport().play();

    let mut heard_signal = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_signal = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(heard_signal, "playback should produce signal before rewind");

    engine.transport().pause();

    let mut silent_blocks = 0_usize;
    for _ in 0..256 {
        backend_handle.process_block();
        if !backend_handle.output_has_signal() {
            silent_blocks += 1;
            if silent_blocks >= 16 {
                return;
            }
        } else {
            silent_blocks = 0;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("pause should eventually clear active notes");
}

#[test]
fn pause_immediate_resets_active_notes() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");
    engine.transport().seek_beats(1.5);
    engine.transport().play();

    let mut heard_signal = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_signal = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        heard_signal,
        "playback should produce signal before immediate pause"
    );

    engine.transport().pause_immediate();

    let mut silent_blocks = 0_usize;
    for _ in 0..256 {
        backend_handle.process_block();
        if !backend_handle.output_has_signal() {
            silent_blocks += 1;
            if silent_blocks >= 16 {
                return;
            }
        } else {
            silent_blocks = 0;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("pause_immediate should eventually clear active notes");
}

#[test]
fn inserting_first_effect_during_playback_keeps_track_audible() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 3840))
        .expect("midi should load");
    engine.transport().play();

    let mut heard_before_insert = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_before_insert = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        heard_before_insert,
        "track should be audible before effect insert"
    );

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_effects(TrackId(0), vec![gain_effect_slot()])
            .expect("track should accept inserted effect");
    }

    let mut heard_after_insert = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_after_insert = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }

    assert!(
        heard_after_insert,
        "track should stay audible after inserting first effect during playback"
    );
}

#[test]
fn toggling_effect_bypass_during_playback_rebuilds_processing_path() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
        mixer
            .set_track_effects(TrackId(0), vec![mute_effect_slot(true)])
            .expect("track should accept bypassed mute effect");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 3840))
        .expect("midi should load");
    engine.transport().play();

    let mut heard_bypassed = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_bypassed = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(heard_bypassed, "bypassed mute effect should pass audio");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_slot_bypassed(
                SlotAddress {
                    strip_index: 1,
                    slot_index: 1,
                },
                false,
            )
            .expect("effect should unbypass");
    }

    let mut silent_blocks = 0_usize;
    for _ in 0..1024 {
        backend_handle.process_block();
        if !backend_handle.output_has_signal() {
            silent_blocks += 1;
            if silent_blocks >= 16 {
                return;
            }
        } else {
            silent_blocks = 0;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("unbypassed mute effect should process and silence audio");
}

#[test]
fn effect_controller_stays_live_after_bypass_toggle() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
        mixer
            .set_track_effects(TrackId(0), vec![gain_effect_slot()])
            .expect("track should accept gain effect");
    }
    settle_backend(&backend_handle);
    let controller = engine
        .controller(SlotAddress {
            strip_index: 1,
            slot_index: 1,
        })
        .expect("controller lookup should succeed")
        .expect("gain controller should exist");
    controller
        .set_param("gain_db", 0.0)
        .expect("gain should update");

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 3840))
        .expect("midi should load");
    engine.transport().play();
    settle_backend(&backend_handle);
    assert!(
        !backend_handle.output_has_signal(),
        "zero gain should silence audio before bypass toggle"
    );

    {
        let mut mixer = engine.mixer();
        mixer
            .set_slot_bypassed(
                SlotAddress {
                    strip_index: 1,
                    slot_index: 1,
                },
                true,
            )
            .expect("effect should bypass");
    }
    settle_backend(&backend_handle);
    assert!(
        backend_handle.output_has_signal(),
        "bypassed zero-gain effect should pass audio"
    );

    {
        let mut mixer = engine.mixer();
        mixer
            .set_slot_bypassed(
                SlotAddress {
                    strip_index: 1,
                    slot_index: 1,
                },
                false,
            )
            .expect("effect should unbypass");
    }
    settle_backend(&backend_handle);

    assert!(
        !backend_handle.output_has_signal(),
        "original controller should still control live effect after bypass toggle"
    );
}

#[test]
fn repeated_effect_bypass_toggle_does_not_duplicate_processing_path() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
        mixer
            .set_track_effects(TrackId(0), vec![gain_effect_slot()])
            .expect("track should accept gain effect");
    }
    settle_backend(&backend_handle);
    engine
        .controller(SlotAddress {
            strip_index: 1,
            slot_index: 1,
        })
        .expect("controller lookup should succeed")
        .expect("gain controller should exist")
        .set_param("gain_db", 0.5)
        .expect("gain should update");

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 3840))
        .expect("midi should load");
    engine.transport().play();
    settle_backend(&backend_handle);
    let baseline_peak = output_peak(&backend_handle);
    assert!(baseline_peak > 0.0, "gain effect should pass reduced audio");

    for _ in 0..4 {
        {
            let mut mixer = engine.mixer();
            mixer
                .set_slot_bypassed(
                    SlotAddress {
                        strip_index: 1,
                        slot_index: 1,
                    },
                    true,
                )
                .expect("effect should bypass");
        }
        settle_backend(&backend_handle);
        {
            let mut mixer = engine.mixer();
            mixer
                .set_slot_bypassed(
                    SlotAddress {
                        strip_index: 1,
                        slot_index: 1,
                    },
                    false,
                )
                .expect("effect should unbypass");
        }
        settle_backend(&backend_handle);
    }

    let after_peak = output_peak(&backend_handle);
    assert!(
        after_peak <= baseline_peak * 1.05,
        "bypass toggles should not add parallel effect paths: baseline {baseline_peak}, after {after_peak}"
    );
}

#[test]
fn rewind_while_playing_keeps_playback_running() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");
    engine.transport().seek_beats(1.5);
    engine.transport().play();

    let mut heard_signal = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_signal = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(heard_signal, "playback should produce signal before rewind");

    engine.transport().rewind();
    let snapshot = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available");
    assert_eq!(
        snapshot.playback_state,
        crate::transport::PlaybackState::Playing
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("rewind while playing should resume audible playback");
}

#[test]
fn rewind_while_playing_then_pause_clears_notes() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&sustained_note_midi_bytes(480, 3840))
        .expect("midi should load");
    engine.transport().play();

    for _ in 0..128 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.transport().seek_beats(4.0);
    for _ in 0..128 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.transport().rewind();
    engine.transport().pause();

    for _ in 0..128 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    let max_after_pause = backend_handle
        .output_channel(0)
        .into_iter()
        .chain(backend_handle.output_channel(1))
        .map(f32::abs)
        .fold(0.0_f32, f32::max);
    assert!(
        max_after_pause < 1.0e-4,
        "rewind then pause should leave no active notes, peak after pause was {max_after_pause}"
    );
}

#[test]
fn rewind_while_playing_then_pause_then_play_resumes_cleanly() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(
        MixerState::new(),
        backend,
        AudioEngineOptions {
            chase_notes_on_seek: true,
            ..AudioEngineOptions::default()
        },
    )
    .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");
    engine.transport().seek_beats(1.5);
    engine.transport().play();

    let mut heard_signal = false;
    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            heard_signal = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(heard_signal, "playback should produce signal before rewind");

    engine.transport().rewind();
    engine.transport().pause();
    engine.transport().play();
    settle_backend(&backend_handle);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("rewind then pause then play should resume audible playback");
}

#[test]
fn paused_seek_then_direct_parameter_change_and_play_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    let node = engine.context.with_activation(|| {
        let node = handle(ScheduledValueGen::new());
        graph_output(0, node.channels(1));
        node
    });
    let node_id = node
        .node_ids()
        .next()
        .expect("scheduled value node should exist");

    engine.commands.transport_pause();
    engine
        .commands
        .transport_seek_to_beats(Beats::from_beats(1));
    wait_for_transport_reset_to(&mut engine.commands, Beats::from_beats(1));
    let mut changes = SimultaneousChanges::duration_from_now(Duration::ZERO);
    changes.push(node_id.change().set("value", 1.0));
    engine.commands.schedule_changes(changes);
    raw_transport_play(&mut engine);

    for _ in 0..128 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("paused seek followed by direct parameter change produced silence");
}

#[test]
fn paused_seek_then_direct_midi_into_instrument_node_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    let instrument = engine.context.with_activation(|| {
        let reset_state = crate::instrument::SharedInstrumentResetState::default();
        let node = handle(InstrumentProcessorNode::new(
            Box::new(TestNoteProcessor {
                active: false,
                program: Arc::new(AtomicU32::new(0)),
            }),
            reset_state.clone(),
        ));
        graph_output(0, node.channels(2));
        (node, reset_state)
    });
    let handle = crate::instrument::InstrumentRuntimeHandle::new(instrument.0, instrument.1);

    engine.commands.transport_pause();
    engine
        .commands
        .transport_seek_to_beats(Beats::from_beats(1));
    wait_for_transport_reset_to(&mut engine.commands, Beats::from_beats(1));
    raw_transport_play(&mut engine);

    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..128 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("paused seek followed by direct MIDI into instrument node produced silence");
}

#[test]
fn paused_seek_then_direct_scheduled_midi_into_instrument_node_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    let instrument = engine.context.with_activation(|| {
        let reset_state = crate::instrument::SharedInstrumentResetState::default();
        let node = handle(InstrumentProcessorNode::new(
            Box::new(TestNoteProcessor {
                active: false,
                program: Arc::new(AtomicU32::new(0)),
            }),
            reset_state.clone(),
        ));
        graph_output(0, node.channels(2));
        (node, reset_state)
    });
    let handle = crate::instrument::InstrumentRuntimeHandle::new(instrument.0, instrument.1);

    engine.commands.transport_pause();
    engine
        .commands
        .transport_seek_to_beats(Beats::from_beats(1));
    wait_for_transport_reset_to(&mut engine.commands, Beats::from_beats(1));
    handle.schedule_reset_at(&mut engine.commands, Beats::from_beats_f64(1.01), 1);
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        Beats::from_beats_f64(1.02),
        1,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    raw_transport_play(&mut engine);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("paused seek followed by direct scheduled MIDI into instrument node produced silence");
}

#[test]
fn paused_seek_then_direct_scheduled_midi_into_soundfont_track_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    engine.transport().seek_beats(1.0);
    handle.schedule_reset_at(&mut engine.commands, Beats::from_beats_f64(1.01), 1);
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        Beats::from_beats_f64(1.02),
        1,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    raw_transport_play(&mut engine);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("paused seek followed by direct scheduled MIDI into soundfont track produced silence");
}

#[test]
fn paused_seek_then_immediate_midi_into_soundfont_track_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    engine.transport().seek_beats(1.0);
    raw_transport_play(&mut engine);

    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let meters = engine.meter_snapshot();
    panic!(
        "paused seek followed by immediate MIDI into soundfont track produced silence; meters={meters:?}"
    );
}

#[test]
fn paused_seek_then_direct_note_on_into_soundfont_track_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    let handle = {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
        engine
            .mixer
            .instrument_handle(TrackId(0))
            .expect("track runtime should expose instrument handle")
    };
    settle_backend(&backend_handle);

    engine.transport().seek_beats(1.0);
    raw_transport_play(&mut engine);
    super::wait_for_transport_settled(&mut engine.commands);

    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.context.with_activation(|| {
        handle.note_on(0, 60, 100);
    });

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("paused seek followed by direct note_on into soundfont track produced silence");
}

#[test]
fn direct_note_on_into_soundfont_track_without_seek_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    let handle = {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
        engine
            .mixer
            .instrument_handle(TrackId(0))
            .expect("track runtime should expose instrument handle")
    };
    settle_backend(&backend_handle);

    raw_transport_play(&mut engine);
    super::wait_for_transport_settled(&mut engine.commands);

    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    engine.context.with_activation(|| {
        handle.note_on(0, 60, 100);
    });

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let meters = engine.meter_snapshot();
    panic!("direct note_on into soundfont track without seek produced silence; meters={meters:?}");
}

#[test]
fn scheduled_midi_into_program_switched_soundfont_track_produces_signal() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);

    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    engine.commands.transport_pause();
    engine.commands.transport_seek_to_beats(Beats::ZERO);
    wait_for_transport_reset_to(&mut engine.commands, Beats::ZERO);
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        Beats::from_beats_f64(0.25),
        0,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    raw_transport_play(&mut engine);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("scheduled MIDI into program-switched soundfont track produced silence");
}

#[test]
fn delayed_score_with_preplay_program_selection_produces_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("delayed score with pre-play program selection produced silence");
}

#[test]
fn delayed_score_with_preplay_default_program_selection_produces_master_output() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    engine.transport().play();

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("delayed score with pre-play default program selection produced silence");
}

#[test]
fn direct_midi_after_score_load_and_preplay_instrument_selection_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }
    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("direct MIDI after score load and pre-play instrument selection produced silence");
}

#[test]
fn scheduled_midi_after_score_load_and_preplay_instrument_selection_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    engine
        .sequencer()
        .replace_from_midi_bytes(&delayed_note_midi_bytes(480, 480))
        .expect("midi should load");

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    settle_backend(&backend_handle);
    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");

    engine.commands.transport_pause();
    engine.commands.transport_seek_to_beats(Beats::ZERO);
    wait_for_transport_reset_to(&mut engine.commands, Beats::ZERO);
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        Beats::from_beats_f64(1.0),
        0,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        Beats::from_beats_f64(2.0),
        0,
        MidiEvent::NoteOff {
            channel: 0,
            note: 60,
            velocity: 0,
        },
    );
    raw_transport_play(&mut engine);

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("scheduled MIDI after score load and pre-play instrument selection produced silence");
}

#[test]
fn scheduled_midi_after_live_instrument_selection_during_playback_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }
    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(0))
            .expect("track should accept soundfont instrument");
    }
    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    let current_beat = engine
        .transport()
        .snapshot()
        .expect("transport snapshot should be available")
        .beats_position;
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        current_beat + Beats::from_beats_f64(1.0),
        0,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );
    handle.schedule_midi_at_with_offset(
        &mut engine.commands,
        current_beat + Beats::from_beats_f64(2.0),
        0,
        MidiEvent::NoteOff {
            channel: 0,
            note: 60,
            velocity: 0,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("scheduled MIDI after live instrument selection during playback produced silence");
}

#[test]
fn direct_midi_after_live_program_selection_during_playback_reaches_master() {
    let (backend, backend_handle) = SharedTestBackend::new(44_100, 64, 2);
    let mut engine = AudioEngine::start(MixerState::new(), backend, AudioEngineOptions::default())
        .expect("engine should start");
    let _audio = backend_handle.start_realtime();

    {
        let mut mixer = engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
    }
    settle_backend(&backend_handle);
    raw_transport_play(&mut engine);
    for _ in 0..8 {
        backend_handle.process_block();
        thread::sleep(Duration::from_millis(2));
    }

    {
        let mut mixer = engine.mixer();
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot(40))
            .expect("track should accept soundfont instrument");
    }
    let handle = engine
        .mixer
        .instrument_handle(TrackId(0))
        .expect("track runtime should expose instrument handle");
    handle.send_midi(
        &mut engine.commands,
        MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        },
    );

    for _ in 0..1024 {
        backend_handle.process_block();
        if backend_handle.output_has_signal() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("direct MIDI after live program selection during playback produced silence");
}
