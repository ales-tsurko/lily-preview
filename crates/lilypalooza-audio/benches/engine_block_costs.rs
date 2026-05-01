//! Manual audio engine block cost benchmark.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use knyst::KnystError;
use knyst::audio_backend::{AudioBackend, AudioBackendError};
use knyst::controller::Controller as KnystController;
use knyst::graph::{Graph, RunGraph, RunGraphSettings};
use knyst::prelude::{Sample, SphereSettings};
use knyst::resources::Resources;
use lilypalooza_audio::instrument::{
    InstrumentRuntimeContext, InstrumentRuntimeSpec, RuntimeBinding, RuntimeFactoryError,
};
use lilypalooza_audio::{
    AudioEngine, AudioEngineOptions, BUILTIN_SOUNDFONT_ID, Controller, ControllerError,
    InstrumentProcessor, MidiEvent, MixerState, Processor, ProcessorDescriptor, ProcessorState,
    ProcessorStateError, SlotState, SoundfontResource, TrackId,
};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track, TrackEvent, TrackEventKind,
};

struct BenchBackend {
    sample_rate: usize,
    block_size: usize,
    num_outputs: usize,
    shared: Arc<Mutex<BenchBackendState>>,
}

struct BenchBackendHandle {
    shared: Arc<Mutex<BenchBackendState>>,
}

struct BenchBackendState {
    run_graph: Option<RunGraph>,
}

struct TestNoteProcessor {
    active: bool,
}

struct TestSoundfontState {
    soundfont_id: String,
}

struct TestSoundfontBinding;

impl BenchBackend {
    fn new(
        sample_rate: usize,
        block_size: usize,
        num_outputs: usize,
    ) -> (Self, BenchBackendHandle) {
        let shared = Arc::new(Mutex::new(BenchBackendState { run_graph: None }));
        (
            Self {
                sample_rate,
                block_size,
                num_outputs,
                shared: Arc::clone(&shared),
            },
            BenchBackendHandle { shared },
        )
    }
}

impl BenchBackendHandle {
    fn process_block(&self) {
        let mut shared = self
            .shared
            .lock()
            .expect("bench backend should not be poisoned");
        if let Some(run_graph) = &mut shared.run_graph {
            run_graph.run_resources_communication(10_000);
            run_graph.process_block();
        }
    }
}

impl AudioBackend for BenchBackend {
    fn start_processing_return_controller(
        &mut self,
        mut graph: Graph,
        resources: Resources,
        run_graph_settings: RunGraphSettings,
        error_handler: Box<dyn FnMut(KnystError) + Send + 'static>,
    ) -> Result<KnystController, AudioBackendError> {
        let mut shared = self
            .shared
            .lock()
            .expect("bench backend should not be poisoned");
        if shared.run_graph.is_some() {
            return Err(AudioBackendError::BackendAlreadyRunning);
        }

        let (run_graph, resources_command_sender, resources_command_receiver) =
            RunGraph::new(&mut graph, resources, run_graph_settings)?;
        let controller = KnystController::new(
            graph,
            error_handler,
            resources_command_sender,
            resources_command_receiver,
        );
        shared.run_graph = Some(run_graph);
        Ok(controller)
    }

    fn stop(&mut self) -> Result<(), AudioBackendError> {
        let mut shared = self
            .shared
            .lock()
            .expect("bench backend should not be poisoned");
        if shared.run_graph.take().is_some() {
            Ok(())
        } else {
            Err(AudioBackendError::BackendNotRunning)
        }
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate
    }

    fn block_size(&self) -> Option<usize> {
        Some(self.block_size)
    }

    fn native_output_channels(&self) -> Option<usize> {
        Some(self.num_outputs)
    }

    fn native_input_channels(&self) -> Option<usize> {
        Some(0)
    }
}

impl Processor for TestNoteProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        soundfont_descriptor()
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

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        if state.0.is_empty() {
            Ok(())
        } else {
            Err(ProcessorStateError::Decode(
                "bench note state must be empty".to_string(),
            ))
        }
    }

    fn reset(&mut self) {
        self.active = false;
    }
}

impl InstrumentProcessor for TestNoteProcessor {
    fn handle_midi(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn { velocity, .. } if velocity > 0 => self.active = true,
            MidiEvent::NoteOff { .. } | MidiEvent::AllNotesOff { .. } => self.active = false,
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [Sample], right: &mut [Sample]) {
        let value = if self.active { 0.25 } else { 0.0 };
        left.fill(value);
        right.fill(value);
    }
}

impl RuntimeBinding for TestSoundfontBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(Self)
    }
}

impl Controller for TestSoundfontBinding {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        soundfont_descriptor()
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        Err(ControllerError::UnknownParameter(id.to_string()))
    }

    fn set_param(&self, id: &str, _normalized: f32) -> Result<(), ControllerError> {
        Err(ControllerError::UnknownParameter(id.to_string()))
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        Ok(ProcessorState::default())
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        if state.0.is_empty() {
            Ok(())
        } else {
            Err(ControllerError::Backend(
                "bench note state must be empty".to_string(),
            ))
        }
    }
}

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    const BLOCKS: usize = 20_000;

    register_bench_processors();

    let (empty_backend, empty_handle) = BenchBackend::new(44_100, 64, 2);
    let _empty_engine = AudioEngine::start(
        MixerState::new(),
        empty_backend,
        AudioEngineOptions {
            sphere_settings: SphereSettings::default(),
            ..AudioEngineOptions::default()
        },
    )
    .expect("empty engine should start");
    settle_backend(&empty_handle);
    let empty_idle = benchmark_blocks(&empty_handle, BLOCKS);

    let (armed_backend, armed_handle) = BenchBackend::new(44_100, 64, 2);
    let mut armed_engine = AudioEngine::start(
        MixerState::new(),
        armed_backend,
        AudioEngineOptions::default(),
    )
    .expect("armed engine should start");
    {
        let mut mixer = armed_engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot())
            .expect("track should accept soundfont");
    }
    settle_backend(&armed_handle);
    let armed_idle = benchmark_blocks(&armed_handle, BLOCKS);

    let (play_backend, play_handle) = BenchBackend::new(44_100, 64, 2);
    let mut play_engine = AudioEngine::start(
        MixerState::new(),
        play_backend,
        AudioEngineOptions::default(),
    )
    .expect("playback engine should start");
    play_engine
        .replace_score_from_midi_bytes(&simple_midi_bytes(480))
        .expect("midi should load");
    {
        let mut mixer = play_engine.mixer();
        mixer
            .set_soundfont(test_soundfont_resource())
            .expect("soundfont should load");
        mixer
            .set_track_instrument(TrackId(0), soundfont_slot())
            .expect("track should accept soundfont");
    }
    settle_backend(&play_handle);
    play_engine.transport().play();
    settle_backend(&play_handle);
    let playback = benchmark_blocks(&play_handle, BLOCKS);

    println!(
        "engine perf over {BLOCKS} blocks: empty_idle={:?} armed_idle={:?} playback={:?}",
        empty_idle, armed_idle, playback
    );
}

fn register_bench_processors() {
    lilypalooza_audio::instrument::registry::register([
        lilypalooza_audio::instrument::registry::Entry::builtin_instrument(
            BUILTIN_SOUNDFONT_ID,
            "SoundFont",
            soundfont_descriptor(),
            create_test_soundfont_runtime,
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
    Ok(Some(InstrumentRuntimeSpec {
        processor: Box::new(TestNoteProcessor { active: false }),
        binding: Box::new(TestSoundfontBinding),
    }))
}

fn soundfont_slot() -> SlotState {
    SlotState::built_in(BUILTIN_SOUNDFONT_ID, ProcessorState(b"default".to_vec()))
}

fn decode_test_soundfont_state(
    state: &ProcessorState,
) -> Result<TestSoundfontState, ProcessorStateError> {
    let soundfont_id = std::str::from_utf8(&state.0)
        .map_err(|error| ProcessorStateError::Decode(error.to_string()))?
        .to_string();
    Ok(TestSoundfontState { soundfont_id })
}

fn test_soundfont_resource() -> SoundfontResource {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/soundfonts/lilypalooza-test.sf2")
        .canonicalize()
        .expect("test SoundFont should exist");
    SoundfontResource {
        id: "default".to_string(),
        name: "Test SoundFont".to_string(),
        path,
    }
}

fn simple_midi_bytes(ppq: u16) -> Vec<u8> {
    let header = Header::new(Format::Parallel, Timing::Metrical(u15::from(ppq)));
    let tempo_track: Track<'static> = vec![
        TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from(500_000))),
        },
        TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ];
    let note_track: Track<'static> = vec![
        TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Midi {
                channel: u4::from(0),
                message: MidiMessage::NoteOn {
                    key: u7::from(60),
                    vel: u7::from(100),
                },
            },
        },
        TrackEvent {
            delta: u28::from(u32::from(ppq)),
            kind: TrackEventKind::Midi {
                channel: u4::from(0),
                message: MidiMessage::NoteOff {
                    key: u7::from(60),
                    vel: u7::from(0),
                },
            },
        },
        TrackEvent {
            delta: u28::from(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ];
    let smf = Smf {
        header,
        tracks: vec![tempo_track, note_track],
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)
        .expect("test MIDI should serialize");
    bytes
}

fn settle_backend(backend: &BenchBackendHandle) {
    for _ in 0..50 {
        backend.process_block();
    }
}

fn benchmark_blocks(backend: &BenchBackendHandle, blocks: usize) -> std::time::Duration {
    let started = Instant::now();
    for _ in 0..blocks {
        backend.process_block();
    }
    started.elapsed()
}
