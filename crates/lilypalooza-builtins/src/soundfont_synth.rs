use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use arc_swap::ArcSwap;
use lilypalooza_audio::BUILTIN_SOUNDFONT_ID;
use lilypalooza_audio::instrument::{
    Controller, ControllerError, EditorDescriptor, EditorError, EditorParent, EditorSession,
    EditorSize, InstrumentProcessor, InstrumentRuntimeContext, InstrumentRuntimeSpec, MidiEvent,
    ParameterDescriptor, Processor, ProcessorDescriptor, ProcessorState, ProcessorStateError,
    RuntimeBinding, RuntimeFactoryError, SlotState, SmoothedAudioValue,
};
use lilypalooza_audio::soundfont::{SoundfontPreset, SoundfontSynthSettings};
use lilypalooza_egui_baseview::{
    EguiApp, EguiWindowHandle, EguiWindowOptions, egui, open_parented,
};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use serde::{Deserialize, Serialize};

/// Maximum MIDI 14-bit controller value.
pub const MIDI_14BIT_MAX: u16 = 16_383;
const MIDI_PROGRAM_MAX: u8 = 127;
const MIDI_CONTROL_MAX: f32 = 127.0;
const MIDI_CC_REVERB_WET: i32 = 91;
const MIDI_CC_CHORUS_WET: i32 = 93;
const MINIMUM_POLYPHONY: u16 = 8;
const MAXIMUM_POLYPHONY: u16 = 256;
const MIN_OUTPUT_GAIN_DB: f32 = -24.0;
const MAX_OUTPUT_GAIN_DB: f32 = 12.0;
const DEFAULT_OUTPUT_GAIN: f32 = 1.0;
const DEFAULT_OUTPUT_GAIN_NORMALIZED: f32 = 2.0 / 3.0;
const DEFAULT_MAXIMUM_POLYPHONY: u16 = 64;
const DEFAULT_REVERB_WET: f32 = 40.0 / 127.0;
const DEFAULT_CHORUS_WET: f32 = 0.0;
const PROGRAM_SCROLL_WHEEL_POINTS_PER_ROW: f32 = 48.0;
const EDITOR_WIDTH: u32 = 820;
const EDITOR_HEIGHT: u32 = 456;
const RETRO_UI_FONT: &str = "retro-ui";
const RETRO_DISPLAY_FONT: &str = "retro-display";

/// Persisted state for the built-in SoundFont instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundfontProcessorState {
    /// Shared SoundFont resource identifier.
    pub soundfont_id: String,
    /// MIDI bank.
    pub bank: u16,
    /// MIDI program.
    pub program: u8,
    /// Whether MIDI bank/program messages should override the selected preset.
    #[serde(default)]
    pub follow_midi: bool,
    /// Maximum voice count for the synthesizer instance.
    #[serde(default = "default_maximum_polyphony")]
    pub maximum_polyphony: u16,
    /// Linear output gain.
    #[serde(default = "default_output_gain")]
    pub output_gain: f32,
    /// Reverb wet mix amount.
    #[serde(default = "default_reverb_wet")]
    pub reverb_wet: f32,
    /// Chorus wet mix amount.
    #[serde(default = "default_chorus_wet")]
    pub chorus_wet: f32,
}

impl Default for SoundfontProcessorState {
    fn default() -> Self {
        Self {
            soundfont_id: "default".to_string(),
            bank: 0,
            program: 0,
            follow_midi: false,
            maximum_polyphony: DEFAULT_MAXIMUM_POLYPHONY,
            output_gain: DEFAULT_OUTPUT_GAIN,
            reverb_wet: DEFAULT_REVERB_WET,
            chorus_wet: DEFAULT_CHORUS_WET,
        }
    }
}

const fn default_maximum_polyphony() -> u16 {
    DEFAULT_MAXIMUM_POLYPHONY
}

const fn default_output_gain() -> f32 {
    DEFAULT_OUTPUT_GAIN
}

const fn default_reverb_wet() -> f32 {
    DEFAULT_REVERB_WET
}

const fn default_chorus_wet() -> f32 {
    DEFAULT_CHORUS_WET
}

/// Decodes typed SoundFont state from the processor state blob stored in slots.
pub fn decode_state(
    state: &ProcessorState,
) -> Result<SoundfontProcessorState, ProcessorStateError> {
    bincode::deserialize(&state.0).map_err(|error| ProcessorStateError::Decode(error.to_string()))
}

/// Encodes typed SoundFont state into the processor state blob stored in slots.
#[must_use]
pub fn encode_state(state: &SoundfontProcessorState) -> ProcessorState {
    match bincode::serialize(state) {
        Ok(bytes) => ProcessorState(bytes),
        Err(error) => {
            eprintln!("failed to encode SoundFont state: {error}");
            ProcessorState::default()
        }
    }
}

/// Encodes a simple SoundFont program selection into the processor state blob.
#[must_use]
pub fn state(soundfont_id: impl Into<String>, bank: u16, program: u8) -> ProcessorState {
    encode_state(&SoundfontProcessorState {
        soundfont_id: soundfont_id.into(),
        bank,
        program,
        ..SoundfontProcessorState::default()
    })
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum SoundfontSynthError {
    #[error("failed to create synthesizer for soundfont `{id}`: {source}")]
    CreateSynth {
        id: String,
        #[source]
        source: rustysynth::SynthesizerError,
    },
}

#[derive(Debug, Clone)]
struct SoundfontCatalogEntry {
    id: String,
    name: String,
    presets: Arc<Vec<SoundfontPreset>>,
}

#[derive(Debug)]
pub(crate) struct SoundfontProcessor {
    settings: SoundfontSynthSettings,
    synthesizer: Synthesizer,
    state: SoundfontProcessorState,
    shared_state: Option<SharedSoundfontState>,
    applied_shared_revision: u32,
    needs_render: bool,
    silent_blocks: u32,
    output_gain: SmoothedAudioValue,
}

#[derive(Debug, Clone)]
pub(crate) struct SharedSoundfontState {
    inner: Arc<SharedSoundfontStateInner>,
}

#[derive(Debug)]
struct SharedSoundfontStateInner {
    soundfont: ArcSwap<SoundFont>,
    soundfont_id: ArcSwap<String>,
    bank: AtomicU16,
    program: AtomicU32,
    follow_midi: AtomicBool,
    maximum_polyphony: AtomicU32,
    output_gain_bits: AtomicU32,
    reverb_wet_bits: AtomicU32,
    chorus_wet_bits: AtomicU32,
    midi_activity: AtomicU32,
    revision: AtomicU32,
}

impl SharedSoundfontState {
    pub(crate) fn new(state: &SoundfontProcessorState, soundfont: Arc<SoundFont>) -> Self {
        Self {
            inner: Arc::new(SharedSoundfontStateInner {
                soundfont: ArcSwap::from(soundfont),
                soundfont_id: ArcSwap::from_pointee(state.soundfont_id.clone()),
                bank: AtomicU16::new(state.bank),
                program: AtomicU32::new(u32::from(state.program)),
                follow_midi: AtomicBool::new(state.follow_midi),
                maximum_polyphony: AtomicU32::new(u32::from(state.maximum_polyphony)),
                output_gain_bits: AtomicU32::new(state.output_gain.to_bits()),
                reverb_wet_bits: AtomicU32::new(state.reverb_wet.to_bits()),
                chorus_wet_bits: AtomicU32::new(state.chorus_wet.to_bits()),
                midi_activity: AtomicU32::new(0),
                revision: AtomicU32::new(1),
            }),
        }
    }

    pub(crate) fn update(&self, state: &SoundfontProcessorState, soundfont: Arc<SoundFont>) {
        self.inner.soundfont.store(soundfont);
        self.inner
            .soundfont_id
            .store(Arc::new(state.soundfont_id.clone()));
        self.inner.bank.store(state.bank, Ordering::Relaxed);
        self.inner
            .program
            .store(u32::from(state.program), Ordering::Relaxed);
        self.inner
            .follow_midi
            .store(state.follow_midi, Ordering::Relaxed);
        self.inner
            .maximum_polyphony
            .store(u32::from(state.maximum_polyphony), Ordering::Relaxed);
        self.inner
            .output_gain_bits
            .store(state.output_gain.to_bits(), Ordering::Relaxed);
        self.inner
            .reverb_wet_bits
            .store(state.reverb_wet.to_bits(), Ordering::Relaxed);
        self.inner
            .chorus_wet_bits
            .store(state.chorus_wet.to_bits(), Ordering::Relaxed);
        self.inner.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn update_bank_program(&self, bank: u16, program: u8) {
        self.inner.bank.store(bank, Ordering::Relaxed);
        self.inner
            .program
            .store(u32::from(program), Ordering::Relaxed);
        self.inner.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> (SoundfontProcessorState, u32) {
        (
            SoundfontProcessorState {
                soundfont_id: self.inner.soundfont_id.load().as_ref().clone(),
                bank: self.inner.bank.load(Ordering::Relaxed),
                program: self.inner.program.load(Ordering::Relaxed) as u8,
                follow_midi: self.inner.follow_midi.load(Ordering::Relaxed),
                maximum_polyphony: self.inner.maximum_polyphony.load(Ordering::Relaxed) as u16,
                output_gain: f32::from_bits(self.inner.output_gain_bits.load(Ordering::Relaxed)),
                reverb_wet: f32::from_bits(self.inner.reverb_wet_bits.load(Ordering::Relaxed)),
                chorus_wet: f32::from_bits(self.inner.chorus_wet_bits.load(Ordering::Relaxed)),
            },
            self.inner.revision.load(Ordering::Relaxed),
        )
    }

    fn soundfont(&self) -> Arc<SoundFont> {
        self.inner.soundfont.load_full()
    }

    fn mark_midi_activity(&self) {
        self.inner.midi_activity.fetch_add(1, Ordering::Relaxed);
    }

    fn midi_activity(&self) -> u32 {
        self.inner.midi_activity.load(Ordering::Relaxed)
    }
}

const SOUNDFONT_PARAMS: &[ParameterDescriptor] = &[
    ParameterDescriptor {
        id: "bank",
        name: "Bank",
        default: 0.0,
    },
    ParameterDescriptor {
        id: "program",
        name: "Program",
        default: 0.0,
    },
    ParameterDescriptor {
        id: "follow_midi",
        name: "Follow MIDI",
        default: 0.0,
    },
    ParameterDescriptor {
        id: "maximum_polyphony",
        name: "Polyphony",
        default: 0.225_806_44,
    },
    ParameterDescriptor {
        id: "output_gain",
        name: "Output Gain",
        default: DEFAULT_OUTPUT_GAIN_NORMALIZED,
    },
    ParameterDescriptor {
        id: "reverb_wet",
        name: "Reverb Dry/Wet",
        default: DEFAULT_REVERB_WET,
    },
    ParameterDescriptor {
        id: "chorus_wet",
        name: "Chorus Dry/Wet",
        default: DEFAULT_CHORUS_WET,
    },
];

pub(crate) const DESCRIPTOR: &ProcessorDescriptor = &ProcessorDescriptor {
    name: "SF-01",
    params: SOUNDFONT_PARAMS,
    editor: Some(EditorDescriptor {
        default_size: EditorSize {
            width: EDITOR_WIDTH,
            height: EDITOR_HEIGHT,
        },
        min_size: Some(EditorSize {
            width: EDITOR_WIDTH,
            height: EDITOR_HEIGHT,
        }),
        resizable: false,
    }),
};

fn descriptor() -> &'static ProcessorDescriptor {
    DESCRIPTOR
}

#[derive(Debug, Clone)]
struct SharedSoundfontBinding {
    catalog: Arc<Vec<SoundfontCatalogEntry>>,
    available_soundfonts: Arc<std::collections::HashMap<String, Arc<SoundFont>>>,
    state: SharedSoundfontState,
}

impl SharedSoundfontBinding {
    fn apply_state(&self, state: SoundfontProcessorState) -> Result<(), ControllerError> {
        let Some(soundfont) = self.available_soundfonts.get(&state.soundfont_id) else {
            return Err(ControllerError::Backend(format!(
                "soundfont resource `{}` is unavailable",
                state.soundfont_id
            )));
        };
        self.state.update(&state, Arc::clone(soundfont));
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct SoundfontBinding {
    shared: Arc<SharedSoundfontBinding>,
}

impl RuntimeBinding for SoundfontBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(SoundfontController {
            shared: Arc::clone(&self.shared),
        })
    }

    fn update_in_place(&self, slot: &SlotState) -> Result<bool, ProcessorStateError> {
        let Some(state) = slot.decode_built_in(BUILTIN_SOUNDFONT_ID, decode_state)? else {
            return Ok(false);
        };
        Ok(self.shared.apply_state(state).is_ok())
    }
}

struct SoundfontController {
    shared: Arc<SharedSoundfontBinding>,
}

impl Controller for SoundfontController {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        descriptor()
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        let (state, _) = self.shared.state.snapshot();
        match id {
            "bank" => Ok(f32::from(state.bank) / f32::from(MIDI_14BIT_MAX)),
            "program" => Ok(f32::from(state.program) / f32::from(MIDI_PROGRAM_MAX)),
            "follow_midi" => Ok(if state.follow_midi { 1.0 } else { 0.0 }),
            "maximum_polyphony" => Ok(normalize_polyphony(state.maximum_polyphony)),
            "output_gain" => Ok(normalize_output_gain(state.output_gain)),
            "reverb_wet" => Ok(state.reverb_wet.clamp(0.0, 1.0)),
            "chorus_wet" => Ok(state.chorus_wet.clamp(0.0, 1.0)),
            _ => Err(ControllerError::UnknownParameter(id.to_string())),
        }
    }

    fn set_param(&self, id: &str, normalized: f32) -> Result<(), ControllerError> {
        let (mut state, _) = self.shared.state.snapshot();
        match id {
            "bank" => {
                state.bank =
                    (normalized.clamp(0.0, 1.0) * f32::from(MIDI_14BIT_MAX)).round() as u16;
            }
            "program" => {
                state.program =
                    (normalized.clamp(0.0, 1.0) * f32::from(MIDI_PROGRAM_MAX)).round() as u8;
            }
            "follow_midi" => state.follow_midi = normalized >= 0.5,
            "maximum_polyphony" => {
                state.maximum_polyphony = denormalize_polyphony(normalized);
            }
            "output_gain" => state.output_gain = denormalize_output_gain(normalized),
            "reverb_wet" => state.reverb_wet = normalized.clamp(0.0, 1.0),
            "chorus_wet" => state.chorus_wet = normalized.clamp(0.0, 1.0),
            _ => return Err(ControllerError::UnknownParameter(id.to_string())),
        }
        self.shared.apply_state(state)
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        let (state, _) = self.shared.state.snapshot();
        Ok(encode_state(&state))
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        let state =
            decode_state(state).map_err(|error| ControllerError::Backend(error.to_string()))?;
        self.shared.apply_state(state)
    }

    fn create_editor_session(&self) -> Result<Option<Box<dyn EditorSession>>, EditorError> {
        Ok(Some(Box::new(SoundfontEditorSession::new(Arc::clone(
            &self.shared,
        )))))
    }
}

struct SoundfontEditorSession {
    shared: Arc<SharedSoundfontBinding>,
    window: Option<EguiWindowHandle>,
}

impl SoundfontEditorSession {
    fn new(shared: Arc<SharedSoundfontBinding>) -> Self {
        Self {
            shared,
            window: None,
        }
    }

    fn close_window(&mut self) {
        if let Some(mut window) = self.window.take() {
            window.close();
        }
    }
}

impl EditorSession for SoundfontEditorSession {
    fn attach(&mut self, parent: EditorParent) -> Result<(), EditorError> {
        if self.window.is_some() {
            self.detach()?;
        }

        let shared = Arc::clone(&self.shared);
        let window = open_parented(
            parent.window,
            EguiWindowOptions {
                title: "SF-01".to_string(),
                width: f64::from(EDITOR_WIDTH),
                height: f64::from(EDITOR_HEIGHT),
            },
            move || SoundfontEditorApp {
                shared,
                retro_style_installed: false,
                bank_text: String::new(),
                bank_text_focused: false,
                polyphony_text: String::new(),
                polyphony_text_focused: false,
                program_scroll_first: 0,
                program_scroll_remainder: 0.0,
                soundfont_dropdown_open: false,
                seen_midi_activity: 0,
                midi_flash_frames: 0,
            },
        )
        .map_err(|error| EditorError::Backend(error.to_string()))?;

        self.window = Some(window);
        Ok(())
    }

    fn detach(&mut self) -> Result<(), EditorError> {
        self.close_window();
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), EditorError> {
        if visible && self.window.is_none() {
            return Err(EditorError::HostUnavailable(
                "SoundFont editor is not attached".to_string(),
            ));
        }
        Ok(())
    }

    fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
        if size.width == 0 || size.height == 0 {
            return Err(EditorError::HostUnavailable(
                "SoundFont editor size must be non-zero".to_string(),
            ));
        }
        Ok(size)
    }
}

impl Drop for SoundfontEditorSession {
    fn drop(&mut self) {
        self.close_window();
    }
}

struct SoundfontEditorApp {
    shared: Arc<SharedSoundfontBinding>,
    retro_style_installed: bool,
    bank_text: String,
    bank_text_focused: bool,
    polyphony_text: String,
    polyphony_text_focused: bool,
    program_scroll_first: usize,
    program_scroll_remainder: f32,
    soundfont_dropdown_open: bool,
    seen_midi_activity: u32,
    midi_flash_frames: u8,
}

impl EguiApp for SoundfontEditorApp {
    fn update(&mut self, ctx: &egui::Context) {
        if !self.retro_style_installed {
            install_retro_style(ctx);
            self.retro_style_installed = true;
            ctx.request_repaint();
            return;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(retro::FACE)
                    .inner_margin(egui::Margin::ZERO),
            )
            .show(ctx, |ui| {
                let (snapshot, _) = self.shared.state.snapshot();
                let soundfont_names = self
                    .shared
                    .catalog
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect::<Vec<_>>();
                let selected_soundfont =
                    selected_soundfont_index(&self.shared.catalog, &snapshot.soundfont_id);
                let banks = bank_numbers(&self.shared.catalog, selected_soundfont);
                let bank_index = selected_bank_index(&banks, snapshot.bank);
                let programs =
                    program_choices(&self.shared.catalog, selected_soundfont, snapshot.bank);
                let program_index = selected_program_index(&programs, snapshot.program);
                if self.bank_text.is_empty() && !self.bank_text_focused {
                    self.bank_text = snapshot.bank.to_string();
                }
                if self.polyphony_text.is_empty() && !self.polyphony_text_focused {
                    self.polyphony_text = snapshot.maximum_polyphony.to_string();
                }

                let canvas = ui.max_rect();
                draw_window_shell(ui, canvas);
                let mut state = snapshot.clone();
                let mut changed = false;

                retro_group(ui, rect(18.0, 74.0, 390.0, 74.0), "SOUNDFONT", |ui| {
                    let selected = soundfont_names
                        .get(selected_soundfont)
                        .map_or("No SoundFonts", String::as_str);
                    if retro_select_box(ui, rect(0.0, 6.0, 350.0, 30.0), "soundfont", selected)
                        .clicked()
                        && !soundfont_names.is_empty()
                    {
                        self.soundfont_dropdown_open = !self.soundfont_dropdown_open;
                    }
                });

                retro_group(ui, rect(424.0, 74.0, 154.0, 74.0), "BANK", |ui| {
                    let bank_response = retro_number_field(
                        ui,
                        rect(0.0, 6.0, 92.0, 30.0),
                        "bank-number",
                        &mut self.bank_text,
                        snapshot.bank,
                        0,
                        MIDI_14BIT_MAX,
                    );
                    self.bank_text_focused = bank_response.focused;
                    if let Some(bank) = bank_response.value {
                        state.bank = bank;
                        changed = true;
                    }
                    if retro_step_button(ui, rect(98.0, 6.0, 28.0, 14.0), true).clicked()
                        && !banks.is_empty()
                    {
                        let next = banks[(bank_index + 1) % banks.len()];
                        self.bank_text = next.to_string();
                        self.bank_text_focused = false;
                        self.select_bank(next);
                    }
                    if retro_step_button(ui, rect(98.0, 22.0, 28.0, 14.0), false).clicked()
                        && !banks.is_empty()
                    {
                        let next = banks[(bank_index + banks.len() - 1) % banks.len()];
                        self.bank_text = next.to_string();
                        self.bank_text_focused = false;
                        self.select_bank(next);
                    }
                });

                let midi_activity = self.shared.state.midi_activity();
                if midi_activity != self.seen_midi_activity {
                    self.seen_midi_activity = midi_activity;
                    self.midi_flash_frames = 12;
                }
                let midi_active = self.midi_flash_frames > 0;
                self.midi_flash_frames = self.midi_flash_frames.saturating_sub(1);

                retro_group(ui, rect(604.0, 74.0, 196.0, 166.0), "MIDI", |ui| {
                    if retro_checkbox(
                        ui,
                        rect(2.0, 20.0, 136.0, 24.0),
                        snapshot.follow_midi,
                        "Follow program",
                    )
                    .clicked()
                    {
                        state.follow_midi = !snapshot.follow_midi;
                        changed = true;
                    }
                    draw_led(
                        ui,
                        pos(10.0, 72.0),
                        true,
                        if midi_active {
                            retro::GREEN
                        } else {
                            retro::LED_IDLE
                        },
                    );
                    retro_text(ui, pos(32.0, 64.0), "MIDI IN", 18.0, retro::TEXT, false);
                });

                retro_group(ui, rect(18.0, 166.0, 390.0, 280.0), "PROGRAM", |ui| {
                    draw_display_box(
                        ui,
                        rect(0.0, 16.0, 316.0, 30.0),
                        programs
                            .get(program_index)
                            .map_or("No programs", |program| program.label.as_str()),
                    );
                    if retro_step_button(ui, rect(322.0, 16.0, 28.0, 14.0), true).clicked()
                        && !programs.is_empty()
                    {
                        self.select_program(
                            programs[(program_index + programs.len() - 1) % programs.len()].program,
                        );
                    }
                    if retro_step_button(ui, rect(322.0, 32.0, 28.0, 14.0), false).clicked()
                        && !programs.is_empty()
                    {
                        self.select_program(programs[(program_index + 1) % programs.len()].program);
                    }
                    if let Some(program) = program_list(
                        ui,
                        rect(0.0, 54.0, 350.0, 192.0),
                        &programs,
                        program_index,
                        &mut self.program_scroll_first,
                        &mut self.program_scroll_remainder,
                    ) {
                        self.select_program(program);
                    }
                });

                retro_group(ui, rect(424.0, 166.0, 154.0, 74.0), "POLYPHONY", |ui| {
                    let polyphony_response = retro_number_field(
                        ui,
                        rect(0.0, 6.0, 92.0, 30.0),
                        "polyphony-number",
                        &mut self.polyphony_text,
                        snapshot.maximum_polyphony,
                        MINIMUM_POLYPHONY,
                        MAXIMUM_POLYPHONY,
                    );
                    self.polyphony_text_focused = polyphony_response.focused;
                    if let Some(polyphony) = polyphony_response.value {
                        state.maximum_polyphony = polyphony;
                        changed = true;
                    }
                    if retro_step_button(ui, rect(98.0, 6.0, 28.0, 14.0), true).clicked() {
                        state.maximum_polyphony = snapshot
                            .maximum_polyphony
                            .saturating_add(1)
                            .min(MAXIMUM_POLYPHONY);
                        self.polyphony_text = state.maximum_polyphony.to_string();
                        self.polyphony_text_focused = false;
                        changed = true;
                    }
                    if retro_step_button(ui, rect(98.0, 22.0, 28.0, 14.0), false).clicked() {
                        state.maximum_polyphony = snapshot
                            .maximum_polyphony
                            .saturating_sub(1)
                            .max(MINIMUM_POLYPHONY);
                        self.polyphony_text = state.maximum_polyphony.to_string();
                        self.polyphony_text_focused = false;
                        changed = true;
                    }
                });

                retro_group(
                    ui,
                    rect(424.0, 258.0, 376.0, 56.0),
                    "REVERB DRY / WET",
                    |ui| {
                        if let Some(next) = retro_slider(
                            ui,
                            "reverb-wet",
                            rect(54.0, 9.0, 208.0, 20.0),
                            snapshot.reverb_wet,
                        ) {
                            state.reverb_wet = next.unwrap_or(0.0);
                            changed = true;
                        }
                        retro_text(ui, pos(0.0, 10.0), "DRY", 14.0, retro::TEXT, true);
                        retro_text(ui, pos(270.0, 10.0), "WET", 14.0, retro::TEXT, true);
                        retro_text(
                            ui,
                            pos(306.0, 7.0),
                            &format!("{:.0}%", snapshot.reverb_wet * 100.0),
                            18.0,
                            retro::DISPLAY,
                            true,
                        );
                    },
                );

                retro_group(
                    ui,
                    rect(424.0, 324.0, 376.0, 56.0),
                    "CHORUS DRY / WET",
                    |ui| {
                        if let Some(next) = retro_slider(
                            ui,
                            "chorus-wet",
                            rect(54.0, 9.0, 208.0, 20.0),
                            snapshot.chorus_wet,
                        ) {
                            state.chorus_wet = next.unwrap_or(0.0);
                            changed = true;
                        }
                        retro_text(ui, pos(0.0, 10.0), "DRY", 14.0, retro::TEXT, true);
                        retro_text(ui, pos(270.0, 10.0), "WET", 14.0, retro::TEXT, true);
                        retro_text(
                            ui,
                            pos(306.0, 7.0),
                            &format!("{:.0}%", snapshot.chorus_wet * 100.0),
                            18.0,
                            retro::DISPLAY,
                            true,
                        );
                    },
                );

                retro_group(
                    ui,
                    rect(424.0, 390.0, 376.0, 56.0),
                    "OUTPUT GAIN (DB)",
                    |ui| {
                        let value = normalize_output_gain(snapshot.output_gain);
                        if let Some(next) =
                            retro_slider(ui, "output-gain", rect(54.0, 9.0, 208.0, 20.0), value)
                        {
                            state.output_gain = next
                                .map(denormalize_output_gain)
                                .unwrap_or_else(|| output_gain_from_db(0.0));
                            changed = true;
                        }
                        retro_text(ui, pos(0.0, 10.0), "-24", 14.0, retro::TEXT, true);
                        retro_text(ui, pos(270.0, 10.0), "+12", 14.0, retro::TEXT, true);
                        retro_text(
                            ui,
                            pos(306.0, 7.0),
                            &format_output_gain_number(output_gain_to_db(snapshot.output_gain)),
                            18.0,
                            retro::DISPLAY,
                            true,
                        );
                    },
                );

                if self.soundfont_dropdown_open
                    && let Some(index) = retro_choice_list(
                        ui,
                        rect(38.0, 128.0, 350.0, 96.0),
                        &soundfont_names,
                        selected_soundfont,
                        "soundfont-dropdown",
                    )
                {
                    self.select_soundfont(index);
                    self.soundfont_dropdown_open = false;
                }

                if changed {
                    self.apply_state(state);
                }
            });
    }
}

impl SoundfontEditorApp {
    fn select_soundfont(&self, index: usize) {
        let Some(entry) = self.shared.catalog.get(index) else {
            return;
        };

        let (mut state, _) = self.shared.state.snapshot();
        state.soundfont_id = entry.id.clone();
        if let Some(preset) = entry
            .presets
            .iter()
            .find(|preset| preset.bank == state.bank && preset.program == state.program)
            .or_else(|| entry.presets.first())
        {
            state.bank = preset.bank;
            state.program = preset.program;
        }
        self.apply_state(state);
    }

    fn select_bank(&self, bank: u16) {
        let (snapshot, _) = self.shared.state.snapshot();
        let soundfont_index =
            selected_soundfont_index(&self.shared.catalog, &snapshot.soundfont_id);
        let Some(entry) = self.shared.catalog.get(soundfont_index) else {
            return;
        };
        let Some(preset) = entry
            .presets
            .iter()
            .find(|preset| preset.bank == bank && preset.program == snapshot.program)
            .or_else(|| entry.presets.iter().find(|preset| preset.bank == bank))
        else {
            return;
        };

        let mut state = snapshot;
        state.bank = preset.bank;
        state.program = preset.program;
        self.apply_state(state);
    }

    fn select_program(&self, program: u8) {
        let (snapshot, _) = self.shared.state.snapshot();
        let soundfont_index =
            selected_soundfont_index(&self.shared.catalog, &snapshot.soundfont_id);
        let Some(entry) = self.shared.catalog.get(soundfont_index) else {
            return;
        };
        let Some(preset) = entry
            .presets
            .iter()
            .find(|preset| preset.bank == snapshot.bank && preset.program == program)
        else {
            return;
        };

        let mut state = snapshot;
        state.bank = preset.bank;
        state.program = preset.program;
        self.apply_state(state);
    }

    fn apply_state(&self, state: SoundfontProcessorState) {
        if let Err(error) = self.shared.apply_state(state) {
            eprintln!("SoundFont editor state update failed: {error}");
        }
    }
}

fn install_retro_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "w95fa".to_string(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/W95FA.otf"
        ))),
    );
    fonts.font_data.insert(
        "cozette".to_string(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/CozetteVector.ttf"
        ))),
    );
    fonts.families.insert(
        egui::FontFamily::Name(RETRO_UI_FONT.into()),
        vec!["w95fa".to_string()],
    );
    fonts.families.insert(
        egui::FontFamily::Name(RETRO_DISPLAY_FONT.into()),
        vec!["cozette".to_string()],
    );
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .expect("default proportional font family exists")
        .insert(0, "w95fa".to_string());
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .expect("default monospace font family exists")
        .insert(0, "cozette".to_string());
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.visuals.window_corner_radius = 0.into();
    style.visuals.widgets.noninteractive.corner_radius = 0.into();
    style.visuals.widgets.inactive.corner_radius = 0.into();
    style.visuals.widgets.hovered.corner_radius = 0.into();
    style.visuals.widgets.active.corner_radius = 0.into();
    style.spacing.item_spacing = egui::vec2(0.0, 0.0);
    ctx.set_style(style);
}

fn draw_window_shell(ui: &mut egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, retro::FACE);
    bevel(painter, rect.shrink(2.0), false);

    let title = egui::Rect::from_min_size(
        rect.min + egui::vec2(8.0, 8.0),
        egui::vec2(rect.width() - 16.0, 34.0),
    );
    painter.rect_filled(title, 0.0, retro::TITLE);
    bevel(painter, title, false);
    painter.text(
        title.left_center() + egui::vec2(12.0, 1.0),
        egui::Align2::LEFT_CENTER,
        "SF-01  SOUNDFONT ROMPLER",
        retro_font(25.0, false),
        retro::TITLE_TEXT,
    );
}

fn retro_group<F>(ui: &mut egui::Ui, local: egui::Rect, title: &'static str, content: F)
where
    F: FnOnce(&mut egui::Ui),
{
    let rect = local_rect(ui, local);
    ui.painter().rect_filled(rect, 0.0, retro::FACE);
    bevel(ui.painter(), rect, true);
    let label_pos = rect.min + egui::vec2(18.0, -3.0);
    let label_galley = ui.painter().layout_no_wrap(
        title.to_string(),
        retro_font(20.0, false),
        retro::LABEL_BLUE,
    );
    let label_cover = egui::Rect::from_min_size(
        label_pos + egui::vec2(-6.0, 2.0),
        label_galley.size() + egui::vec2(12.0, 2.0),
    );
    ui.painter().rect_filled(label_cover, 0.0, retro::FACE);
    ui.painter()
        .galley(label_pos, label_galley, retro::LABEL_BLUE);

    let content_rect = rect.shrink2(egui::vec2(20.0, 16.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        content,
    );
}

fn retro_select_box(
    ui: &mut egui::Ui,
    local: egui::Rect,
    id: &'static str,
    text: &str,
) -> egui::Response {
    let rect = local_rect(ui, local);
    let response = ui.interact(rect, ui.id().with(id), egui::Sense::click());
    ui.painter().rect_filled(rect, 0.0, retro::FIELD);
    bevel(ui.painter(), rect, true);
    retro_text_left_center_abs(
        ui,
        rect.left_center() + egui::vec2(10.0, -1.0),
        text,
        18.0,
        retro::TEXT,
        false,
    );
    let button = egui::Rect::from_min_max(
        rect.right_top() - egui::vec2(29.0, 0.0),
        rect.right_bottom(),
    );
    retro_button_frame(
        ui,
        button,
        response.hovered(),
        response.is_pointer_button_down_on(),
    );
    draw_triangle(ui.painter(), button.center(), false, retro::TEXT);
    response
}

fn retro_step_button(ui: &mut egui::Ui, local: egui::Rect, up: bool) -> egui::Response {
    let rect = local_rect(ui, local);
    let response = ui.interact(
        rect,
        ui.id()
            .with((rect.min.x.to_bits(), rect.min.y.to_bits(), up)),
        egui::Sense::click(),
    );
    retro_button_frame(
        ui,
        rect,
        response.hovered(),
        response.is_pointer_button_down_on(),
    );
    draw_triangle(ui.painter(), rect.center(), up, retro::TEXT);
    response
}

fn retro_choice_list(
    ui: &mut egui::Ui,
    local: egui::Rect,
    choices: &[String],
    selected_index: usize,
    id: &'static str,
) -> Option<usize> {
    let rect = local_rect(ui, local);
    ui.painter().rect_filled(rect, 0.0, retro::FIELD);
    bevel(ui.painter(), rect, true);

    let row_height = 24.0;
    let vertical_padding = 6.0;
    let visible_rows = ((rect.height() - vertical_padding * 2.0) / row_height)
        .floor()
        .max(1.0) as usize;
    let mut selected = None;
    for row in 0..visible_rows {
        let Some(choice) = choices.get(row) else {
            break;
        };
        let row_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(4.0, vertical_padding + row as f32 * row_height),
            egui::vec2(rect.width() - 8.0, row_height - 2.0),
        );
        let response = ui.interact(row_rect, ui.id().with((id, row)), egui::Sense::click());
        if row == selected_index {
            ui.painter().rect_filled(row_rect, 0.0, retro::SELECT);
        } else if response.hovered() {
            ui.painter().rect_filled(row_rect, 0.0, retro::LCD_HOVER);
        }
        retro_text_left_center_abs(
            ui,
            row_rect.left_center() + egui::vec2(8.0, 0.0),
            choice,
            17.0,
            if row == selected_index {
                retro::TITLE_TEXT
            } else {
                retro::TEXT
            },
            false,
        );
        if response.clicked() {
            selected = Some(row);
        }
    }
    selected
}

fn retro_checkbox(
    ui: &mut egui::Ui,
    local: egui::Rect,
    checked: bool,
    text: &str,
) -> egui::Response {
    let rect = local_rect(ui, local);
    let response = ui.interact(rect, ui.id().with(text), egui::Sense::click());
    let box_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 2.0), egui::vec2(18.0, 18.0));
    ui.painter().rect_filled(box_rect, 0.0, retro::FIELD);
    bevel(ui.painter(), box_rect, true);
    if checked {
        ui.painter()
            .rect_filled(box_rect.shrink(4.0), 0.0, retro::GREEN);
        ui.painter().rect_stroke(
            box_rect.shrink(4.0),
            0.0,
            egui::Stroke::new(1.0, retro::TEXT),
            egui::StrokeKind::Inside,
        );
    }
    retro_text_abs(
        ui,
        rect.min + egui::vec2(28.0, 1.0),
        text,
        16.0,
        retro::TEXT,
        false,
    );
    response
}

fn draw_display_box(ui: &mut egui::Ui, local: egui::Rect, text: &str) {
    let rect = local_rect(ui, local);
    ui.painter().rect_filled(rect, 0.0, retro::LCD);
    bevel(ui.painter(), rect, true);
    retro_text_left_center_abs(
        ui,
        rect.left_center() + egui::vec2(10.0, -1.0),
        text,
        20.0,
        retro::LCD_TEXT,
        false,
    );
}

struct NumberFieldResponse {
    value: Option<u16>,
    focused: bool,
}

fn retro_number_field(
    ui: &mut egui::Ui,
    local: egui::Rect,
    id: &'static str,
    text: &mut String,
    current: u16,
    min: u16,
    max: u16,
) -> NumberFieldResponse {
    let rect = local_rect(ui, local);
    ui.painter().rect_filled(rect, 0.0, retro::LCD);
    bevel(ui.painter(), rect, true);

    let edit_rect = rect.shrink2(egui::vec2(8.0, 3.0));
    let response = ui.put(
        edit_rect,
        egui::TextEdit::singleline(text)
            .id_salt(id)
            .font(retro_font(20.0, true))
            .text_color(retro::LCD_TEXT)
            .desired_width(edit_rect.width())
            .frame(false),
    );

    let commit = response.lost_focus() || ui.input(|input| input.key_pressed(egui::Key::Enter));
    let value = if commit {
        if text.trim().is_empty() {
            None
        } else {
            let parsed = text
                .trim()
                .parse::<u16>()
                .map(|value| value.clamp(min, max))
                .unwrap_or(current);
            *text = parsed.to_string();
            Some(parsed)
        }
    } else {
        None
    };

    NumberFieldResponse {
        value,
        focused: response.has_focus(),
    }
}

fn draw_led(ui: &mut egui::Ui, local_pos: egui::Pos2, on: bool, color: egui::Color32) {
    let center = ui.min_rect().min + local_pos.to_vec2();
    let fill = if on { color } else { retro::SHADOW };
    ui.painter().circle_filled(center, 7.0, retro::BLACK);
    ui.painter().circle_filled(center, 6.0, fill);
    ui.painter()
        .circle_stroke(center, 7.0, egui::Stroke::new(1.0, retro::BLACK));
    ui.painter()
        .circle_stroke(center, 5.0, egui::Stroke::new(1.0, retro::SHADOW));
}

fn program_list(
    ui: &mut egui::Ui,
    local: egui::Rect,
    programs: &[ProgramChoice],
    selected_index: usize,
    first: &mut usize,
    scroll_remainder: &mut f32,
) -> Option<u8> {
    let mut selected = None;
    let rect = local_rect(ui, local);
    ui.painter().rect_filled(rect, 0.0, retro::FIELD);
    bevel(ui.painter(), rect, true);
    let row_height = 24.0;
    let row_gap = 2.0;
    let vertical_padding = 6.0;
    let visible_rows = ((rect.height() - vertical_padding * 2.0) / row_height)
        .floor()
        .max(1.0) as usize;
    let max_first = programs.len().saturating_sub(visible_rows);
    *first = (*first).min(max_first);
    let list_response = ui.interact(
        rect,
        ui.id().with("program-list-scroll"),
        egui::Sense::hover(),
    );
    if list_response.hovered()
        || ui.input(|input| {
            input
                .pointer
                .latest_pos()
                .is_some_and(|pos| rect.contains(pos))
        })
    {
        let scroll_delta = ui.input(|input| input.raw_scroll_delta.y + input.smooth_scroll_delta.y);
        *scroll_remainder += scroll_delta;
        while *scroll_remainder <= -PROGRAM_SCROLL_WHEEL_POINTS_PER_ROW {
            *first = (*first).saturating_add(1).min(max_first);
            *scroll_remainder += PROGRAM_SCROLL_WHEEL_POINTS_PER_ROW;
        }
        while *scroll_remainder >= PROGRAM_SCROLL_WHEEL_POINTS_PER_ROW {
            *first = (*first).saturating_sub(1);
            *scroll_remainder -= PROGRAM_SCROLL_WHEEL_POINTS_PER_ROW;
        }
    }

    let list_rect = egui::Rect::from_min_max(rect.min, rect.max - egui::vec2(24.0, 0.0));
    for row in 0..visible_rows {
        let index = *first + row;
        let row_rect = egui::Rect::from_min_size(
            list_rect.min + egui::vec2(4.0, vertical_padding + row as f32 * row_height),
            egui::vec2(list_rect.width() - 8.0, row_height - row_gap),
        );
        if let Some(program) = programs.get(index) {
            let response = ui.interact(
                row_rect,
                ui.id().with(("program-row", index)),
                egui::Sense::click(),
            );
            if index == selected_index {
                ui.painter().rect_filled(row_rect, 0.0, retro::SELECT);
            } else if response.hovered() {
                ui.painter().rect_filled(row_rect, 0.0, retro::LCD_HOVER);
            }
            let text_color = if index == selected_index {
                retro::TITLE_TEXT
            } else {
                retro::LCD_TEXT
            };
            retro_text_left_center_abs(
                ui,
                row_rect.left_center() + egui::vec2(8.0, 0.0),
                &program.label,
                17.0,
                text_color,
                false,
            );
            if response.clicked() {
                selected = Some(program.program);
            }
        }
    }

    let scroll_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 24.0, rect.top()),
        rect.right_bottom(),
    );
    let up = egui::Rect::from_min_size(
        scroll_rect.min + egui::vec2(3.0, 3.0),
        egui::vec2(18.0, 16.0),
    );
    let down = egui::Rect::from_min_size(
        egui::pos2(scroll_rect.left() + 3.0, scroll_rect.bottom() - 19.0),
        egui::vec2(18.0, 16.0),
    );
    let up_response = ui.interact(up, ui.id().with("program-scroll-up"), egui::Sense::click());
    let down_response = ui.interact(
        down,
        ui.id().with("program-scroll-down"),
        egui::Sense::click(),
    );
    retro_button_frame(
        ui,
        up,
        up_response.hovered(),
        up_response.is_pointer_button_down_on(),
    );
    retro_button_frame(
        ui,
        down,
        down_response.hovered(),
        down_response.is_pointer_button_down_on(),
    );
    draw_triangle(ui.painter(), up.center(), true, retro::TEXT);
    draw_triangle(ui.painter(), down.center(), false, retro::TEXT);
    if up_response.clicked() {
        *first = (*first).saturating_sub(1);
    }
    if down_response.clicked() {
        *first = (*first).saturating_add(1).min(max_first);
    }
    let track = egui::Rect::from_min_max(
        egui::pos2(scroll_rect.left() + 4.0, up.bottom() + 2.0),
        egui::pos2(scroll_rect.right() - 4.0, down.top() - 2.0),
    );
    ui.painter().rect_filled(track, 0.0, retro::FACE);
    bevel(ui.painter(), track, true);
    let thumb_height = (track.height() * visible_rows as f32
        / programs.len().max(visible_rows) as f32)
        .clamp(18.0, track.height());
    let thumb_top = if max_first == 0 {
        track.top()
    } else {
        track.top() + (track.height() - thumb_height) * (*first as f32 / max_first as f32)
    };
    let thumb = egui::Rect::from_min_size(
        egui::pos2(track.left(), thumb_top),
        egui::vec2(track.width(), thumb_height),
    );
    let thumb_response = ui.interact(
        thumb,
        ui.id().with("program-scroll-thumb"),
        egui::Sense::click_and_drag(),
    );
    let track_response = ui.interact(
        track,
        ui.id().with("program-scroll-track"),
        egui::Sense::click_and_drag(),
    );
    let live_scroll_pointer = ui.input(|input| {
        let pos = input.pointer.latest_pos()?;
        if input.pointer.primary_down() && track.contains(pos) {
            Some(pos)
        } else {
            None
        }
    });
    if max_first > 0
        && (thumb_response.dragged()
            || thumb_response.clicked()
            || track_response.dragged()
            || track_response.clicked()
            || live_scroll_pointer.is_some())
        && let Some(pointer) = live_scroll_pointer
            .or_else(|| thumb_response.interact_pointer_pos())
            .or_else(|| track_response.interact_pointer_pos())
    {
        let travel = (track.height() - thumb_height).max(1.0);
        let ratio = ((pointer.y - track.top() - thumb_height / 2.0) / travel).clamp(0.0, 1.0);
        *first = (ratio * max_first as f32).round() as usize;
    }
    retro_button_frame(
        ui,
        thumb,
        thumb_response.hovered() || thumb_response.dragged(),
        thumb_response.is_pointer_button_down_on(),
    );
    selected
}

fn retro_slider(
    ui: &mut egui::Ui,
    id: &'static str,
    local: egui::Rect,
    value: f32,
) -> Option<Option<f32>> {
    let rect = local_rect(ui, local);
    let widget_id = ui.id().with(id);
    let response = ui.interact(
        rect.expand2(egui::vec2(8.0, 6.0)),
        widget_id,
        egui::Sense::click_and_drag(),
    );
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - 2.0),
        egui::pos2(rect.right(), rect.center().y + 2.0),
    );
    ui.painter().rect_filled(track, 0.0, retro::SHADOW);
    bevel(ui.painter(), track, true);
    for tick in 0..=6 {
        let x = rect.left() + rect.width() * tick as f32 / 6.0;
        ui.painter().line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 6.0)],
            egui::Stroke::new(1.0, retro::DARK_HILITE),
        );
    }

    let normalized = value.clamp(0.0, 1.0);
    let x = rect.left() + rect.width() * normalized;
    let thumb =
        egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(18.0, 26.0));
    retro_button_frame(
        ui,
        thumb,
        response.hovered(),
        response.is_pointer_button_down_on(),
    );
    for offset in [-4.0, 0.0, 4.0] {
        ui.painter().line_segment(
            [
                egui::pos2(thumb.center().x + offset, thumb.top() + 5.0),
                egui::pos2(thumb.center().x + offset, thumb.bottom() - 5.0),
            ],
            egui::Stroke::new(1.0, retro::DARK_HILITE),
        );
    }

    let reset = response.double_clicked() || slider_manual_double_click(ui, widget_id, &response);
    if reset {
        Some(None)
    } else if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        Some(Some(
            ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ))
    } else {
        None
    }
}

fn slider_manual_double_click(ui: &mut egui::Ui, id: egui::Id, response: &egui::Response) -> bool {
    if !response.clicked() {
        return false;
    }

    let Some(pointer) = response
        .interact_pointer_pos()
        .or_else(|| ui.input(|input| input.pointer.latest_pos()))
    else {
        return false;
    };
    let now = ui.input(|input| input.time);
    let storage_id = id.with("last-click");
    let previous = ui.data(|data| data.get_temp::<(f64, egui::Pos2)>(storage_id));
    ui.data_mut(|data| data.insert_temp(storage_id, (now, pointer)));

    previous.is_some_and(|(last_time, last_pos)| {
        now - last_time <= 0.35 && last_pos.distance(pointer) <= 8.0
    })
}

fn retro_button_frame(ui: &mut egui::Ui, rect: egui::Rect, hovered: bool, pressed: bool) {
    ui.painter().rect_filled(
        rect,
        0.0,
        if hovered {
            retro::BUTTON_HOVER
        } else {
            retro::FACE
        },
    );
    bevel(ui.painter(), rect, pressed);
}

fn bevel(painter: &egui::Painter, rect: egui::Rect, inset: bool) {
    let (top_left, bottom_right) = if inset {
        (retro::SHADOW, retro::HILITE)
    } else {
        (retro::HILITE, retro::SHADOW)
    };
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, top_left),
    );
    painter.line_segment(
        [rect.left_top(), rect.left_bottom()],
        egui::Stroke::new(1.0, top_left),
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, bottom_right),
    );
    painter.line_segment(
        [rect.right_top(), rect.right_bottom()],
        egui::Stroke::new(1.0, bottom_right),
    );
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, retro::BLACK),
        egui::StrokeKind::Inside,
    );
}

fn draw_triangle(painter: &egui::Painter, center: egui::Pos2, up: bool, color: egui::Color32) {
    let points = if up {
        vec![
            center + egui::vec2(-5.0, 3.0),
            center + egui::vec2(5.0, 3.0),
            center + egui::vec2(0.0, -4.0),
        ]
    } else {
        vec![
            center + egui::vec2(-5.0, -3.0),
            center + egui::vec2(5.0, -3.0),
            center + egui::vec2(0.0, 4.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

fn retro_text(
    ui: &mut egui::Ui,
    local_pos: egui::Pos2,
    text: &str,
    size: f32,
    color: egui::Color32,
    display: bool,
) {
    retro_text_abs(
        ui,
        ui.min_rect().min + local_pos.to_vec2(),
        text,
        size,
        color,
        display,
    );
}

fn retro_text_abs(
    ui: &mut egui::Ui,
    pos: egui::Pos2,
    text: &str,
    size: f32,
    color: egui::Color32,
    display: bool,
) {
    ui.painter().text(
        pos,
        egui::Align2::LEFT_TOP,
        text,
        retro_font(size, display),
        color,
    );
}

fn retro_text_left_center_abs(
    ui: &mut egui::Ui,
    pos: egui::Pos2,
    text: &str,
    size: f32,
    color: egui::Color32,
    display: bool,
) {
    ui.painter().text(
        pos,
        egui::Align2::LEFT_CENTER,
        text,
        retro_font(size, display),
        color,
    );
}

fn retro_font(size: f32, display: bool) -> egui::FontId {
    let family = if display {
        egui::FontFamily::Name(RETRO_DISPLAY_FONT.into())
    } else {
        egui::FontFamily::Name(RETRO_UI_FONT.into())
    };
    egui::FontId::new(size, family)
}

fn local_rect(ui: &egui::Ui, rect: egui::Rect) -> egui::Rect {
    rect.translate(ui.min_rect().min.to_vec2())
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> egui::Rect {
    egui::Rect::from_min_size(pos(x, y), egui::vec2(width, height))
}

fn pos(x: f32, y: f32) -> egui::Pos2 {
    egui::pos2(x, y)
}

mod retro {
    use lilypalooza_egui_baseview::egui::Color32;

    pub const FACE: Color32 = Color32::from_rgb(192, 192, 192);
    pub const FIELD: Color32 = Color32::from_rgb(232, 229, 220);
    pub const LCD: Color32 = Color32::from_rgb(148, 172, 104);
    pub const LCD_HOVER: Color32 = Color32::from_rgb(166, 188, 124);
    pub const LCD_TEXT: Color32 = Color32::from_rgb(16, 24, 12);
    pub const TEXT: Color32 = Color32::from_rgb(16, 16, 16);
    pub const TITLE: Color32 = Color32::from_rgb(0, 0, 128);
    pub const TITLE_TEXT: Color32 = Color32::from_rgb(255, 255, 255);
    pub const LABEL_BLUE: Color32 = Color32::from_rgb(0, 46, 140);
    pub const SELECT: Color32 = Color32::from_rgb(0, 0, 128);
    pub const GREEN: Color32 = Color32::from_rgb(96, 210, 82);
    pub const LED_IDLE: Color32 = Color32::from_rgb(58, 92, 50);
    pub const BUTTON_HOVER: Color32 = Color32::from_rgb(210, 210, 210);
    pub const HILITE: Color32 = Color32::from_rgb(255, 255, 255);
    pub const DARK_HILITE: Color32 = Color32::from_rgb(128, 128, 128);
    pub const SHADOW: Color32 = Color32::from_rgb(64, 64, 64);
    pub const BLACK: Color32 = Color32::from_rgb(0, 0, 0);
    pub const DISPLAY: Color32 = Color32::from_rgb(0, 80, 76);
}

struct ProgramChoice {
    program: u8,
    label: String,
}

fn bank_numbers(catalog: &[SoundfontCatalogEntry], soundfont_index: usize) -> Vec<u16> {
    catalog
        .get(soundfont_index)
        .map(|entry| {
            let mut banks = entry
                .presets
                .iter()
                .map(|preset| preset.bank)
                .collect::<Vec<_>>();
            banks.dedup();
            banks
        })
        .unwrap_or_default()
}

fn program_choices(
    catalog: &[SoundfontCatalogEntry],
    soundfont_index: usize,
    bank: u16,
) -> Vec<ProgramChoice> {
    catalog
        .get(soundfont_index)
        .map(|entry| {
            entry
                .presets
                .iter()
                .filter(|preset| preset.bank == bank)
                .map(|preset| ProgramChoice {
                    program: preset.program,
                    label: format!("{:03} {}", preset.program, preset.name),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn selected_soundfont_index(catalog: &[SoundfontCatalogEntry], soundfont_id: &str) -> usize {
    catalog
        .iter()
        .position(|entry| entry.id == soundfont_id)
        .unwrap_or(0)
}

fn selected_bank_index(banks: &[u16], bank: u16) -> usize {
    banks
        .iter()
        .position(|candidate| *candidate == bank)
        .unwrap_or(0)
}

fn selected_program_index(programs: &[ProgramChoice], program: u8) -> usize {
    programs
        .iter()
        .position(|candidate| candidate.program == program)
        .unwrap_or(0)
}

pub(crate) fn create_runtime(
    slot: &SlotState,
    context: &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
    let Some(state) = slot.decode_built_in(BUILTIN_SOUNDFONT_ID, decode_state)? else {
        return Ok(None);
    };
    let Some(loaded) = context.soundfonts.get(&state.soundfont_id) else {
        return Ok(None);
    };
    let shared_state = SharedSoundfontState::new(&state, Arc::clone(&loaded.soundfont));
    let available_soundfonts = Arc::new(
        context
            .soundfonts
            .iter()
            .map(|(id, loaded)| (id.clone(), Arc::clone(&loaded.soundfont)))
            .collect(),
    );
    let catalog = Arc::new(
        context
            .soundfont_resources
            .iter()
            .filter_map(|resource| {
                context
                    .soundfonts
                    .get(&resource.id)
                    .map(|loaded| SoundfontCatalogEntry {
                        id: resource.id.clone(),
                        name: resource.name.clone(),
                        presets: Arc::clone(&loaded.presets),
                    })
            })
            .collect(),
    );
    let processor = SoundfontProcessor::new_with_shared_program(
        &loaded.soundfont,
        context.soundfont_settings,
        state,
        Some(shared_state.clone()),
    )
    .map_err(|error| RuntimeFactoryError::Backend(error.to_string()))?;
    Ok(Some(InstrumentRuntimeSpec {
        processor: Box::new(processor),
        binding: Box::new(SoundfontBinding {
            shared: Arc::new(SharedSoundfontBinding {
                catalog,
                available_soundfonts,
                state: shared_state,
            }),
        }),
    }))
}

impl SoundfontProcessor {
    const TRACK_CHANNEL: i32 = 0;

    #[cfg(test)]
    fn new(
        soundfont: &Arc<SoundFont>,
        settings: SoundfontSynthSettings,
        state: SoundfontProcessorState,
    ) -> Result<Self, SoundfontSynthError> {
        Self::new_with_shared_program(soundfont, settings, state, None)
    }

    pub(crate) fn new_with_shared_program(
        soundfont: &Arc<SoundFont>,
        settings: SoundfontSynthSettings,
        state: SoundfontProcessorState,
        shared_state: Option<SharedSoundfontState>,
    ) -> Result<Self, SoundfontSynthError> {
        let initial_output_gain = state.output_gain;
        let mut synthesizer = build_synthesizer(soundfont, settings, &state)?;
        synthesizer.set_master_volume(1.0);
        let applied_shared_revision = shared_state
            .as_ref()
            .map_or(0, |shared| shared.snapshot().1);
        let mut processor = Self {
            settings,
            synthesizer,
            state,
            shared_state,
            applied_shared_revision,
            needs_render: false,
            silent_blocks: 0,
            output_gain: SmoothedAudioValue::new(
                initial_output_gain,
                settings.sample_rate as usize,
            ),
        };
        processor.apply_program();
        Ok(processor)
    }

    /// Decodes typed SoundFont state from the processor state blob stored in slots.
    fn decode_state(
        state: &ProcessorState,
    ) -> Result<SoundfontProcessorState, ProcessorStateError> {
        decode_state(state)
    }

    fn apply_program(&mut self) {
        self.synthesizer.note_off_all(true);
        self.synthesizer.reset();
        self.synthesizer.process_midi_message(
            Self::TRACK_CHANNEL,
            0xB0,
            0x00,
            i32::from(self.state.bank.min(127)),
        );
        self.synthesizer.process_midi_message(
            Self::TRACK_CHANNEL,
            0xC0,
            i32::from(self.state.program),
            0,
        );
        self.synthesizer.set_master_volume(1.0);
        self.output_gain.set_target(self.state.output_gain);
        self.apply_effect_mix();
        self.needs_render = false;
        self.silent_blocks = 0;
    }

    fn apply_effect_mix(&mut self) {
        self.synthesizer.process_midi_message(
            Self::TRACK_CHANNEL,
            0xB0,
            MIDI_CC_REVERB_WET,
            midi_control_value(self.state.reverb_wet),
        );
        self.synthesizer.process_midi_message(
            Self::TRACK_CHANNEL,
            0xB0,
            MIDI_CC_CHORUS_WET,
            midi_control_value(self.state.chorus_wet),
        );
    }

    fn rebuild_synth(&mut self) {
        let Some(shared) = &self.shared_state else {
            return;
        };
        let soundfont = shared.soundfont();
        if let Ok(synthesizer) = build_synthesizer(&soundfont, self.settings, &self.state) {
            self.synthesizer = synthesizer;
        }
        self.apply_program();
    }

    fn sync_shared_state(&mut self) {
        let Some(shared) = &self.shared_state else {
            return;
        };
        let (state, revision) = shared.snapshot();
        if revision == self.applied_shared_revision {
            return;
        }
        let rebuild_needed = state.soundfont_id != self.state.soundfont_id
            || state.maximum_polyphony != self.state.maximum_polyphony;
        let program_changed = state.bank != self.state.bank || state.program != self.state.program;
        let follow_changed = state.follow_midi != self.state.follow_midi;
        let gain_changed = (state.output_gain - self.state.output_gain).abs() > f32::EPSILON;
        let mix_changed = (state.reverb_wet - self.state.reverb_wet).abs() > f32::EPSILON
            || (state.chorus_wet - self.state.chorus_wet).abs() > f32::EPSILON;
        self.state = state;
        self.applied_shared_revision = revision;
        if rebuild_needed {
            self.rebuild_synth();
            return;
        }
        if program_changed || follow_changed {
            self.apply_program();
            return;
        }
        if gain_changed {
            self.output_gain.set_target(self.state.output_gain);
        }
        if mix_changed {
            self.apply_effect_mix();
        }
    }
}

impl Processor for SoundfontProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        DESCRIPTOR
    }

    fn set_param(&mut self, id: &str, normalized: f32) -> bool {
        let normalized = normalized.clamp(0.0, 1.0);
        match id {
            "bank" => {
                self.state.bank = (normalized * f32::from(MIDI_14BIT_MAX)).round() as u16;
                self.apply_program();
                true
            }
            "program" => {
                self.state.program = (normalized * f32::from(MIDI_PROGRAM_MAX)).round() as u8;
                self.apply_program();
                true
            }
            "follow_midi" => {
                self.state.follow_midi = normalized >= 0.5;
                true
            }
            "maximum_polyphony" => {
                self.state.maximum_polyphony = denormalize_polyphony(normalized);
                self.rebuild_synth();
                true
            }
            "output_gain" => {
                self.state.output_gain = denormalize_output_gain(normalized);
                self.output_gain.set_target(self.state.output_gain);
                true
            }
            "reverb_wet" => {
                self.state.reverb_wet = normalized.clamp(0.0, 1.0);
                self.apply_effect_mix();
                true
            }
            "chorus_wet" => {
                self.state.chorus_wet = normalized.clamp(0.0, 1.0);
                self.apply_effect_mix();
                true
            }
            _ => false,
        }
    }

    fn get_param(&self, id: &str) -> Option<f32> {
        match id {
            "bank" => Some(f32::from(self.state.bank) / f32::from(MIDI_14BIT_MAX)),
            "program" => Some(f32::from(self.state.program) / f32::from(MIDI_PROGRAM_MAX)),
            "follow_midi" => Some(if self.state.follow_midi { 1.0 } else { 0.0 }),
            "maximum_polyphony" => Some(normalize_polyphony(self.state.maximum_polyphony)),
            "output_gain" => Some(normalize_output_gain(self.state.output_gain)),
            "reverb_wet" => Some(self.state.reverb_wet.clamp(0.0, 1.0)),
            "chorus_wet" => Some(self.state.chorus_wet.clamp(0.0, 1.0)),
            _ => None,
        }
    }

    fn save_state(&self) -> ProcessorState {
        encode_state(&self.state)
    }

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        self.state = Self::decode_state(state)?;
        self.rebuild_synth();
        Ok(())
    }

    fn reset(&mut self) {
        self.apply_program();
    }
}

impl InstrumentProcessor for SoundfontProcessor {
    fn handle_midi(&mut self, event: MidiEvent) {
        self.sync_shared_state();
        if let Some(shared) = &self.shared_state {
            shared.mark_midi_activity();
        }
        match event {
            MidiEvent::NoteOn { note, velocity, .. } => {
                if velocity > 0 {
                    self.needs_render = true;
                    self.silent_blocks = 0;
                }
                self.synthesizer
                    .note_on(Self::TRACK_CHANNEL, i32::from(note), i32::from(velocity))
            }
            MidiEvent::NoteOff { note, velocity, .. } => self.synthesizer.process_midi_message(
                Self::TRACK_CHANNEL,
                0x80,
                i32::from(note),
                i32::from(velocity),
            ),
            MidiEvent::ControlChange {
                controller, value, ..
            } => {
                if self.state.follow_midi && controller == 0 {
                    self.state.bank = u16::from(value);
                    if let Some(shared) = &self.shared_state {
                        shared.update_bank_program(self.state.bank, self.state.program);
                        self.applied_shared_revision = shared.snapshot().1;
                    }
                    self.apply_program();
                }
                if !matches!(controller, 32) {
                    self.synthesizer.process_midi_message(
                        Self::TRACK_CHANNEL,
                        0xB0,
                        i32::from(controller),
                        i32::from(value),
                    );
                }
            }
            MidiEvent::ProgramChange { program, .. } => {
                if self.state.follow_midi {
                    self.state.program = program;
                    if let Some(shared) = &self.shared_state {
                        shared.update_bank_program(self.state.bank, self.state.program);
                        self.applied_shared_revision = shared.snapshot().1;
                    }
                    self.apply_program();
                }
            }
            MidiEvent::ChannelPressure { pressure, .. } => self.synthesizer.process_midi_message(
                Self::TRACK_CHANNEL,
                0xD0,
                i32::from(pressure),
                0,
            ),
            MidiEvent::PolyPressure { note, pressure, .. } => {
                self.synthesizer.process_midi_message(
                    Self::TRACK_CHANNEL,
                    0xA0,
                    i32::from(note),
                    i32::from(pressure),
                )
            }
            MidiEvent::PitchBend { value, .. } => {
                let midi_value = (i32::from(value) + 8192).clamp(0, i32::from(MIDI_14BIT_MAX));
                self.synthesizer.process_midi_message(
                    Self::TRACK_CHANNEL,
                    0xE0,
                    midi_value & 0x7F,
                    (midi_value >> 7) & 0x7F,
                );
            }
            MidiEvent::AllNotesOff { .. } => self
                .synthesizer
                .note_off_all_channel(Self::TRACK_CHANNEL, false),
            MidiEvent::AllSoundOff { .. } => {
                self.needs_render = false;
                self.silent_blocks = 0;
                self.synthesizer
                    .note_off_all_channel(Self::TRACK_CHANNEL, true)
            }
            MidiEvent::ResetAllControllers { .. } => {
                self.synthesizer
                    .process_midi_message(Self::TRACK_CHANNEL, 0xB0, 121, 0)
            }
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.sync_shared_state();
        if !self.needs_render {
            left.fill(0.0);
            right.fill(0.0);
            return;
        }
        self.synthesizer.render(left, right);
        self.output_gain.set_target(self.state.output_gain);
        for (left_sample, right_sample) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.output_gain.next_sample();
            *left_sample *= gain;
            *right_sample *= gain;
        }
        let peak = left
            .iter()
            .chain(right.iter())
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        if peak <= 1.0e-6 {
            self.silent_blocks = self.silent_blocks.saturating_add(1);
            if self.silent_blocks >= 8 {
                self.needs_render = false;
            }
        } else {
            self.silent_blocks = 0;
        }
    }

    fn is_sleeping(&self) -> bool {
        !self.needs_render
    }
}

fn normalize_polyphony(value: u16) -> f32 {
    let span = f32::from(MAXIMUM_POLYPHONY - MINIMUM_POLYPHONY);
    f32::from(value.clamp(MINIMUM_POLYPHONY, MAXIMUM_POLYPHONY) - MINIMUM_POLYPHONY) / span
}

fn denormalize_polyphony(normalized: f32) -> u16 {
    let span = f32::from(MAXIMUM_POLYPHONY - MINIMUM_POLYPHONY);
    MINIMUM_POLYPHONY + (normalized.clamp(0.0, 1.0) * span).round() as u16
}

fn midi_control_value(normalized: f32) -> i32 {
    (normalized.clamp(0.0, 1.0) * MIDI_CONTROL_MAX).round() as i32
}

fn output_gain_to_db(linear: f32) -> f32 {
    (20.0 * linear.max(0.0).log10()).clamp(MIN_OUTPUT_GAIN_DB, MAX_OUTPUT_GAIN_DB)
}

fn output_gain_from_db(db: f32) -> f32 {
    10.0_f32.powf(db.clamp(MIN_OUTPUT_GAIN_DB, MAX_OUTPUT_GAIN_DB) / 20.0)
}

fn normalize_output_gain(linear: f32) -> f32 {
    (output_gain_to_db(linear) - MIN_OUTPUT_GAIN_DB) / (MAX_OUTPUT_GAIN_DB - MIN_OUTPUT_GAIN_DB)
}

fn denormalize_output_gain(normalized: f32) -> f32 {
    let db =
        MIN_OUTPUT_GAIN_DB + normalized.clamp(0.0, 1.0) * (MAX_OUTPUT_GAIN_DB - MIN_OUTPUT_GAIN_DB);
    output_gain_from_db(db)
}

#[cfg(test)]
fn format_output_gain_db(db: f32) -> String {
    format!("{} dB", format_output_gain_number(db))
}

fn format_output_gain_number(db: f32) -> String {
    if db.abs() < 0.05 {
        "0.0".to_string()
    } else if db > 0.0 {
        format!("+{db:.1}")
    } else {
        format!("{db:.1}")
    }
}

fn build_synthesizer(
    soundfont: &Arc<SoundFont>,
    settings: SoundfontSynthSettings,
    state: &SoundfontProcessorState,
) -> Result<Synthesizer, SoundfontSynthError> {
    let mut synth_settings = SynthesizerSettings::new(settings.sample_rate);
    synth_settings.block_size = settings.block_size;
    synth_settings.maximum_polyphony = usize::from(
        state
            .maximum_polyphony
            .clamp(MINIMUM_POLYPHONY, MAXIMUM_POLYPHONY),
    );
    synth_settings.enable_reverb_and_chorus = true;
    Synthesizer::new(soundfont, &synth_settings).map_err(|source| {
        SoundfontSynthError::CreateSynth {
            id: state.soundfont_id.clone(),
            source,
        }
    })
}

#[cfg(test)]
fn soundfont_presets(soundfont: &SoundFont) -> Vec<SoundfontPreset> {
    let mut presets = soundfont
        .get_presets()
        .iter()
        .filter_map(|preset| {
            let bank = u16::try_from(preset.get_bank_number()).ok()?;
            let program = u8::try_from(preset.get_patch_number()).ok()?;
            Some(SoundfontPreset {
                bank,
                program,
                name: preset.get_name().trim().to_string(),
            })
        })
        .collect::<Vec<_>>();
    presets.sort_by(|left, right| {
        left.bank
            .cmp(&right.bank)
            .then(left.program.cmp(&right.program))
            .then_with(|| left.name.cmp(&right.name))
    });
    presets
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    use super::{
        ProgramChoice, SoundfontProcessor, SoundfontProcessorState, create_runtime, encode_state,
    };
    use lilypalooza_audio::SoundfontPreset;
    use lilypalooza_audio::instrument::{
        EditorSize, InstrumentProcessor, InstrumentRuntimeContext, MidiEvent, Processor,
    };
    use lilypalooza_audio::soundfont::{
        LoadedSoundfont, SoundfontResource, SoundfontSynthSettings,
    };
    use lilypalooza_audio::{BUILTIN_SOUNDFONT_ID, SlotState};
    use lilypalooza_egui_baseview::EguiApp;

    fn test_soundfont_resource() -> SoundfontResource {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/soundfonts/lilypalooza-test.sf2")
            .canonicalize()
            .expect("test SoundFont should exist");
        SoundfontResource {
            id: "default".to_string(),
            name: "FluidR3".to_string(),
            path,
        }
    }

    #[test]
    fn soundfont_processor_renders_after_note_on() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
            if left
                .iter()
                .chain(right.iter())
                .any(|sample| sample.abs() > 1.0e-6)
            {
                return;
            }
        }

        panic!("soundfont processor produced silence after note on");
    }

    #[test]
    fn soundfont_presets_are_sorted_by_bank_program_and_name() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let presets = super::soundfont_presets(&loaded.soundfont);

        assert!(presets.windows(2).all(|pair| {
            let left = &pair[0];
            let right = &pair[1];
            (left.bank, left.program, left.name.as_str())
                <= (right.bank, right.program, right.name.as_str())
        }));
    }

    #[test]
    fn soundfont_processor_stays_silent_without_note_on() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        let mut left = vec![1.0; 64];
        let mut right = vec![1.0; 64];
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
            assert!(
                left.iter()
                    .chain(right.iter())
                    .all(|sample| sample.abs() <= 1.0e-6),
                "soundfont processor should stay silent before any note on"
            );
            left.fill(1.0);
            right.fill(1.0);
        }
    }

    #[test]
    fn soundfont_processor_renders_after_note_on_on_nonzero_channel() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 3,
            note: 60,
            velocity: 100,
        });

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
            if left
                .iter()
                .chain(right.iter())
                .any(|sample| sample.abs() > 1.0e-6)
            {
                return;
            }
        }

        panic!("soundfont processor produced silence after nonzero-channel note on");
    }

    #[test]
    fn soundfont_processor_reset_silences_active_note() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
        }

        processor.reset();
        left.fill(0.0);
        right.fill(0.0);
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
        }

        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.abs() <= 1.0e-6),
            "soundfont processor reset should silence active notes"
        );
    }

    #[test]
    fn soundfont_processor_renders_after_reset_then_note_on() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.reset();
        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        for _ in 0..8 {
            processor.render(&mut left, &mut right);
            if left
                .iter()
                .chain(right.iter())
                .any(|sample| sample.abs() > 1.0e-6)
            {
                return;
            }
        }

        panic!("soundfont processor produced silence after reset then note on");
    }

    #[test]
    fn soundfont_processor_ignores_midi_program_override() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let settings = SoundfontSynthSettings::new(44_100, 64);
        let mut selected_program = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");
        let mut overridden_program = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");

        overridden_program.handle_midi(MidiEvent::ProgramChange {
            channel: 0,
            program: 0,
        });
        overridden_program.handle_midi(MidiEvent::ControlChange {
            channel: 0,
            controller: 0,
            value: 0,
        });
        overridden_program.handle_midi(MidiEvent::ControlChange {
            channel: 0,
            controller: 32,
            value: 0,
        });

        selected_program.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        overridden_program.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut selected_left = vec![0.0; 512];
        let mut selected_right = vec![0.0; 512];
        let mut overridden_left = vec![0.0; 512];
        let mut overridden_right = vec![0.0; 512];

        for _ in 0..8 {
            selected_program.render(&mut selected_left, &mut selected_right);
            overridden_program.render(&mut overridden_left, &mut overridden_right);
        }

        assert_eq!(selected_left, overridden_left);
        assert_eq!(selected_right, overridden_right);
    }

    #[test]
    fn soundfont_processor_follows_midi_program_and_bank_when_enabled() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let settings = SoundfontSynthSettings::new(44_100, 64);
        let mut selected_program = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                follow_midi: true,
                maximum_polyphony: 64,
                output_gain: 0.5,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");
        let mut overridden_program = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                follow_midi: true,
                maximum_polyphony: 64,
                output_gain: 0.5,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");

        overridden_program.handle_midi(MidiEvent::ControlChange {
            channel: 0,
            controller: 0,
            value: 0,
        });
        overridden_program.handle_midi(MidiEvent::ProgramChange {
            channel: 0,
            program: 0,
        });

        selected_program.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        overridden_program.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut selected_left = vec![0.0; 512];
        let mut selected_right = vec![0.0; 512];
        let mut overridden_left = vec![0.0; 512];
        let mut overridden_right = vec![0.0; 512];

        for _ in 0..8 {
            selected_program.render(&mut selected_left, &mut selected_right);
            overridden_program.render(&mut overridden_left, &mut overridden_right);
        }

        assert!(
            selected_left
                .iter()
                .zip(overridden_left.iter())
                .chain(selected_right.iter().zip(overridden_right.iter()))
                .any(|(a, b)| (a - b).abs() > 1.0e-6),
            "midi-selected program should change the rendered signal when follow_midi is enabled"
        );
    }

    #[test]
    fn soundfont_wet_params_roundtrip_and_enable_internal_effects() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState {
                reverb_wet: 0.25,
                chorus_wet: 0.75,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");

        assert!(processor.synthesizer.get_enable_reverb_and_chorus());
        assert_eq!(processor.get_param("reverb_wet"), Some(0.25));
        assert_eq!(processor.get_param("chorus_wet"), Some(0.75));

        assert!(processor.set_param("reverb_wet", 0.5));
        assert!(processor.set_param("chorus_wet", 0.125));
        let decoded =
            SoundfontProcessor::decode_state(&processor.save_state()).expect("state should decode");

        assert_eq!(decoded.reverb_wet, 0.5);
        assert_eq!(decoded.chorus_wet, 0.125);
    }

    #[test]
    fn soundfont_output_gain_param_uses_trim_db_scale() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        assert_eq!(
            super::DESCRIPTOR
                .params
                .iter()
                .find(|param| param.id == "output_gain")
                .expect("output gain param should exist")
                .default,
            2.0 / 3.0
        );
        assert_eq!(processor.get_param("output_gain"), Some(2.0 / 3.0));

        assert!(processor.set_param("output_gain", 1.0));
        assert_eq!(
            processor.state.output_gain,
            super::output_gain_from_db(12.0)
        );

        assert!(processor.set_param("output_gain", 0.0));
        assert!((processor.state.output_gain - super::output_gain_from_db(-24.0)).abs() < 1.0e-6);

        assert!(processor.set_param("output_gain", 0.5));
        let expected = super::output_gain_from_db(-6.0);
        assert!((processor.state.output_gain - expected).abs() < 1.0e-6);
    }

    #[test]
    fn soundfont_output_gain_db_format_shows_signed_gain() {
        assert_eq!(super::format_output_gain_db(12.0), "+12.0 dB");
        assert_eq!(super::format_output_gain_db(-6.0), "-6.0 dB");
        assert_eq!(super::format_output_gain_db(0.0), "0.0 dB");
    }

    #[test]
    fn soundfont_controller_exposes_editor_session() {
        let resource = test_soundfont_resource();
        let loaded = LoadedSoundfont::load(&resource).expect("test SoundFont should load");
        let mut soundfonts = std::collections::HashMap::new();
        soundfonts.insert(resource.id.clone(), loaded);
        let resources = vec![resource];
        let slot = SlotState::built_in(
            BUILTIN_SOUNDFONT_ID,
            encode_state(&SoundfontProcessorState::default()),
        );
        let context = InstrumentRuntimeContext {
            soundfonts: &soundfonts,
            soundfont_resources: &resources,
            soundfont_settings: SoundfontSynthSettings::new(44_100, 64),
        };

        let runtime = create_runtime(&slot, &context)
            .expect("runtime should build")
            .expect("soundfont runtime should exist");
        let controller = runtime.binding.controller();

        assert!(controller.descriptor().editor.is_some());
        assert!(
            controller
                .create_editor_session()
                .expect("editor creation should succeed")
                .is_some()
        );
    }

    #[test]
    fn soundfont_editor_is_not_resizable() {
        assert_eq!(
            super::descriptor().editor.map(|editor| editor.resizable),
            Some(false)
        );
    }

    #[test]
    fn soundfont_editor_uses_retro_fixed_size() {
        let editor = super::descriptor()
            .editor
            .expect("soundfont editor descriptor should exist");

        assert_eq!(
            editor.default_size,
            EditorSize {
                width: super::EDITOR_WIDTH,
                height: super::EDITOR_HEIGHT,
            }
        );
        assert_eq!(editor.min_size, Some(editor.default_size));
    }

    #[test]
    fn soundfont_descriptor_is_named_sf_01() {
        assert_eq!(super::descriptor().name, "SF-01");
    }

    #[test]
    fn soundfont_editor_bundles_retro_fonts() {
        assert!(!include_bytes!("../assets/fonts/W95FA.otf").is_empty());
        assert!(!include_bytes!("../assets/fonts/CozetteVector.ttf").is_empty());
    }

    #[test]
    fn soundfont_program_list_uses_same_background_as_select_box() {
        let mut first = 0;
        let mut scroll_remainder = 0.0;
        let shapes = render_test_ui(|ui| {
            let programs = vec![ProgramChoice {
                program: 0,
                label: "000 Piano".to_string(),
            }];
            super::program_list(
                ui,
                super::rect(0.0, 0.0, 160.0, 96.0),
                &programs,
                0,
                &mut first,
                &mut scroll_remainder,
            );
        });

        assert!(
            shapes.iter().any(|shape| {
                matches!(
                    shape,
                    super::egui::Shape::Rect(rect)
                        if rect.rect == super::rect(0.0, 0.0, 160.0, 96.0)
                            && rect.fill == super::retro::FIELD
                )
            }),
            "program list background should match select-box background"
        );
    }

    #[test]
    fn soundfont_program_list_item_text_is_vertically_centered() {
        let mut first = 0;
        let mut scroll_remainder = 0.0;
        let shapes = render_test_ui(|ui| {
            let programs = vec![ProgramChoice {
                program: 0,
                label: "000 Piano".to_string(),
            }];
            super::program_list(
                ui,
                super::rect(0.0, 0.0, 160.0, 96.0),
                &programs,
                0,
                &mut first,
                &mut scroll_remainder,
            );
        });
        let row = super::egui::Rect::from_min_size(
            super::egui::pos2(4.0, 6.0),
            super::egui::vec2(128.0, 24.0),
        );

        let text = shapes
            .iter()
            .find_map(|shape| match shape {
                super::egui::Shape::Text(text) if text.galley.text().contains("000 Piano") => {
                    Some(text)
                }
                _ => None,
            })
            .expect("program row text should be painted");
        let text_center = text.pos.y + text.galley.size().y / 2.0;

        assert!(
            (text_center - row.center().y).abs() <= 1.0,
            "program row text center {text_center} should match row center {}",
            row.center().y
        );
    }

    #[test]
    fn soundfont_select_box_text_is_vertically_centered() {
        let shapes = render_test_ui(|ui| {
            super::retro_select_box(
                ui,
                super::rect(0.0, 0.0, 180.0, 30.0),
                "soundfont-test",
                "FluidR3",
            );
        });
        let text = text_shape(&shapes, "FluidR3");
        let center = text.pos.y + text.galley.size().y / 2.0;

        assert!((center - 15.0).abs() <= 1.0);
    }

    #[test]
    fn soundfont_dropdown_rows_match_select_box_and_center_text() {
        let shapes = render_test_ui(|ui| {
            super::retro_choice_list(
                ui,
                super::rect(0.0, 0.0, 180.0, 60.0),
                &["FluidR3".to_string()],
                0,
                "dropdown-test",
            );
        });

        assert!(
            shapes.iter().any(|shape| {
                matches!(
                    shape,
                    super::egui::Shape::Rect(rect)
                        if rect.rect == super::rect(0.0, 0.0, 180.0, 60.0)
                            && rect.fill == super::retro::FIELD
                )
            }),
            "dropdown background should match select-box background"
        );

        let text = text_shape(&shapes, "FluidR3");
        let center = text.pos.y + text.galley.size().y / 2.0;
        assert!((center - 17.0).abs() <= 1.0);
    }

    #[test]
    fn soundfont_program_list_scrolls_with_mouse_wheel() {
        let ctx = super::egui::Context::default();
        super::install_retro_style(&ctx);
        let programs = (0..8)
            .map(|program| ProgramChoice {
                program,
                label: format!("{program:03} Program"),
            })
            .collect::<Vec<_>>();
        let mut first = 0;

        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![super::egui::Event::PointerMoved(super::egui::pos2(
                20.0, 20.0,
            ))],
        );
        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![
                super::egui::Event::PointerMoved(super::egui::pos2(20.0, 20.0)),
                super::egui::Event::MouseWheel {
                    unit: super::egui::MouseWheelUnit::Point,
                    delta: super::egui::vec2(0.0, -48.0),
                    modifiers: super::egui::Modifiers::default(),
                },
            ],
        );

        assert!(
            first > 0,
            "mouse wheel should advance the visible program window"
        );
    }

    #[test]
    fn soundfont_program_list_ignores_tiny_wheel_deltas() {
        let ctx = super::egui::Context::default();
        super::install_retro_style(&ctx);
        let programs = (0..8)
            .map(|program| ProgramChoice {
                program,
                label: format!("{program:03} Program"),
            })
            .collect::<Vec<_>>();
        let mut first = 0;

        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![super::egui::Event::PointerMoved(super::egui::pos2(
                20.0, 20.0,
            ))],
        );
        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![
                super::egui::Event::PointerMoved(super::egui::pos2(20.0, 20.0)),
                super::egui::Event::MouseWheel {
                    unit: super::egui::MouseWheelUnit::Point,
                    delta: super::egui::vec2(0.0, -4.0),
                    modifiers: super::egui::Modifiers::default(),
                },
            ],
        );

        assert_eq!(first, 0, "tiny wheel deltas should not skip a row");
    }

    #[test]
    fn soundfont_program_list_thumb_drag_scrolls() {
        let ctx = super::egui::Context::default();
        super::install_retro_style(&ctx);
        let programs = (0..12)
            .map(|program| ProgramChoice {
                program,
                label: format!("{program:03} Program"),
            })
            .collect::<Vec<_>>();
        let mut first = 0;

        render_program_list_frame(&ctx, &programs, &mut first, vec![]);
        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![
                super::egui::Event::PointerMoved(super::egui::pos2(147.0, 37.0)),
                super::egui::Event::PointerButton {
                    pos: super::egui::pos2(147.0, 37.0),
                    button: super::egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: super::egui::Modifiers::default(),
                },
                super::egui::Event::PointerMoved(super::egui::pos2(147.0, 74.0)),
            ],
        );
        render_program_list_frame(
            &ctx,
            &programs,
            &mut first,
            vec![super::egui::Event::PointerButton {
                pos: super::egui::pos2(147.0, 74.0),
                button: super::egui::PointerButton::Primary,
                pressed: false,
                modifiers: super::egui::Modifiers::default(),
            }],
        );

        assert!(first > 0, "dragging the scrollbar thumb should scroll");
    }

    #[test]
    fn soundfont_editor_program_list_shows_at_least_three_items() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_presets(&loaded, &["first"], 6);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let visible_programs = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .filter_map(|shape| match shape {
                super::egui::Shape::Text(text)
                    if text.galley.text().contains("Program")
                        && text.visual_bounding_rect().top() >= 235.0 =>
                {
                    Some(text.galley.text().to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(
            visible_programs.len() >= 3,
            "program list should show at least three rows, got {visible_programs:?}"
        );
    }

    #[test]
    fn soundfont_midi_indicator_is_round_led() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let has_led_circle = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .any(|shape| match shape {
                super::egui::Shape::Circle(circle) => {
                    circle.center.distance(super::egui::pos2(634.0, 162.0)) < 2.0
                }
                _ => false,
            });

        assert!(has_led_circle, "MIDI IN indicator should be a circle");
    }

    #[test]
    fn soundfont_midi_indicator_has_no_white_circle_highlight() {
        let shapes = render_test_ui(|ui| {
            super::draw_led(ui, super::pos(24.0, 24.0), true, super::retro::GREEN);
        });

        let has_white_led_stroke = shapes.iter().any(|shape| match shape {
            super::egui::Shape::Circle(circle) => {
                circle.center.distance(super::egui::pos2(24.0, 24.0)) < 4.0
                    && circle.stroke.color == super::retro::HILITE
            }
            _ => false,
        });

        assert!(
            !has_white_led_stroke,
            "MIDI IN LED should not draw a white circular highlight"
        );
    }

    #[test]
    fn soundfont_group_label_background_tracks_label_width() {
        let shapes = render_test_ui(|ui| {
            super::retro_group(ui, super::rect(0.0, 20.0, 196.0, 80.0), "MIDI", |_| {});
        });
        let label_cover = shapes
            .iter()
            .find_map(|shape| match shape {
                super::egui::Shape::Rect(rect)
                    if rect.fill == super::retro::FACE
                        && rect.rect.top() < 24.0
                        && rect.rect.left() > 0.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("group label should paint an opaque caption background");

        assert!(
            label_cover.width() < 80.0,
            "caption background should be sized to the label, got {}",
            label_cover.width()
        );
    }

    #[test]
    fn soundfont_slider_value_labels_stay_inside_group_bounds() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let shapes = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .collect::<Vec<_>>();
        let slider_value_bounds = shapes
            .iter()
            .filter_map(|shape| match shape {
                super::egui::Shape::Text(text)
                    if text.galley.text().ends_with('%') || text.galley.text().ends_with("dB") =>
                {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(!slider_value_bounds.is_empty());
        for bounds in slider_value_bounds {
            assert!(
                bounds.left() >= 424.0 && bounds.right() <= 800.0 && bounds.bottom() <= 438.0,
                "slider value label should fit the editor groups: {bounds:?}"
            );
        }
    }

    #[test]
    fn soundfont_slider_groups_leave_bottom_padding_for_thumbs() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let shapes = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .collect::<Vec<_>>();
        let slider_groups = shapes
            .iter()
            .filter_map(|shape| match shape {
                super::egui::Shape::Rect(rect)
                    if rect.fill == super::retro::FACE
                        && (rect.rect.width() - 376.0).abs() < 0.1
                        && rect.rect.left() == 424.0
                        && rect.rect.top() >= 258.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(slider_groups.len(), 3);
        for group in slider_groups {
            let thumb = shapes
                .iter()
                .find_map(|shape| match shape {
                    super::egui::Shape::Rect(rect)
                        if rect.fill == super::retro::FACE
                            && (rect.rect.width() - 18.0).abs() < 0.1
                            && (rect.rect.height() - 26.0).abs() < 0.1
                            && group.contains_rect(rect.rect) =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("slider group should contain a thumb");
            let bottom_padding = group.bottom() - thumb.bottom();
            assert!(
                bottom_padding >= 8.0,
                "slider thumb needs bottom padding, got {bottom_padding} in {group:?}"
            );
        }
    }

    #[test]
    fn soundfont_slider_groups_have_vertical_spacing() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let mut slider_groups = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .filter_map(|shape| match shape {
                super::egui::Shape::Rect(rect)
                    if rect.fill == super::retro::FACE
                        && (rect.rect.width() - 376.0).abs() < 0.1
                        && rect.rect.left() == 424.0
                        && rect.rect.top() >= 258.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        slider_groups.sort_by(|a, b| a.top().total_cmp(&b.top()));
        assert_eq!(slider_groups.len(), 3);
        for pair in slider_groups.windows(2) {
            let gap = pair[1].top() - pair[0].bottom();
            assert!(
                gap >= 10.0,
                "slider groups need more vertical gap, got {gap}"
            );
        }
    }

    #[test]
    fn soundfont_output_gain_db_is_in_group_label_not_value() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let texts = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .filter_map(|shape| match shape {
                super::egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(texts.iter().any(|text| text == "OUTPUT GAIN (DB)"));
        assert!(
            texts.iter().all(|text| text != "0.0 dB"),
            "output gain value should omit dB suffix"
        );
    }

    #[test]
    fn soundfont_header_says_soundfont_rompler_once() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let texts = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .filter_map(|shape| match shape {
                super::egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(texts.iter().any(|text| text == "SF-01  SOUNDFONT ROMPLER"));
        assert!(
            texts.iter().all(|text| text != "ROMPLER"),
            "header should not paint a separate ROMPLER label"
        );
    }

    #[test]
    fn soundfont_header_text_is_vertically_centered() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let header = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .find_map(|shape| match shape {
                super::egui::Shape::Text(text)
                    if text.galley.text() == "SF-01  SOUNDFONT ROMPLER" =>
                {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .expect("header text should be painted");

        assert!(
            (header.center().y - 25.0).abs() <= 1.0,
            "header text center should match title bar center: {header:?}"
        );
    }

    #[test]
    fn soundfont_slider_double_click_resets_values() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();
        let mut state = app.shared.state.snapshot().0;
        state.reverb_wet = 0.7;
        state.chorus_wet = 0.6;
        state.output_gain = super::output_gain_from_db(12.0);
        app.shared
            .apply_state(state)
            .expect("test state should apply");

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        double_click_editor(&ctx, &mut app, super::egui::pos2(600.0, 293.0));
        double_click_editor(&ctx, &mut app, super::egui::pos2(600.0, 359.0));
        double_click_editor(&ctx, &mut app, super::egui::pos2(600.0, 425.0));

        let (state, _) = app.shared.state.snapshot();
        assert_eq!(state.reverb_wet, 0.0);
        assert_eq!(state.chorus_wet, 0.0);
        assert!((super::output_gain_to_db(state.output_gain) - 0.0).abs() < 1.0e-6);
    }

    #[test]
    fn soundfont_number_field_can_remain_empty_while_editing() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();
        let bank_field = super::egui::pos2(488.0, 111.0);

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        click_editor(&ctx, &mut app, bank_field);
        let _ = ctx.run(
            editor_input(vec![super::egui::Event::Key {
                key: super::egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: super::egui::Modifiers::default(),
            }]),
            |ctx| app.update(ctx),
        );
        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));

        assert_eq!(
            app.bank_text, "",
            "empty number field text should not be repopulated while focused"
        );
    }

    #[test]
    fn soundfont_top_row_controls_are_centered_in_groups() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let shapes = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .collect::<Vec<_>>();
        let select_box = shapes
            .iter()
            .find_map(|shape| match shape {
                super::egui::Shape::Rect(rect)
                    if rect.rect.width() == 350.0 && rect.rect.height() == 30.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("soundfont select box should be painted");

        assert_eq!(select_box.center().y, 111.0);
    }

    #[test]
    fn soundfont_chooser_click_opens_choices_without_changing_selection() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first", "second"]);
        let ctx = super::egui::Context::default();
        let chooser_pos = super::egui::pos2(210.0, 121.0);

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let _ = ctx.run(
            editor_input(vec![
                super::egui::Event::PointerMoved(chooser_pos),
                super::egui::Event::PointerButton {
                    pos: chooser_pos,
                    button: super::egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: super::egui::Modifiers::default(),
                },
            ]),
            |ctx| app.update(ctx),
        );
        let _ = ctx.run(
            editor_input(vec![
                super::egui::Event::PointerMoved(chooser_pos),
                super::egui::Event::PointerButton {
                    pos: chooser_pos,
                    button: super::egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: super::egui::Modifiers::default(),
                },
            ]),
            |ctx| app.update(ctx),
        );

        let (state, _) = app.shared.state.snapshot();
        assert_eq!(
            state.soundfont_id, "first",
            "clicking the chooser should open choices, not cycle selection"
        );
    }

    #[test]
    fn soundfont_chooser_item_click_selects_choice() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first", "second"]);
        let ctx = super::egui::Context::default();
        let chooser_pos = super::egui::pos2(210.0, 121.0);
        let second_choice_pos = super::egui::pos2(210.0, 179.0);

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        click_editor(&ctx, &mut app, chooser_pos);
        assert!(
            app.soundfont_dropdown_open,
            "clicking the chooser should open the dropdown"
        );
        click_editor(&ctx, &mut app, second_choice_pos);

        let (state, _) = app.shared.state.snapshot();
        assert_eq!(state.soundfont_id, "second");
    }

    #[test]
    fn soundfont_midi_panel_labels_input_without_idle_state_text() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut app = editor_app_with_soundfonts(&loaded, &["first"]);
        let ctx = super::egui::Context::default();

        let _ = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let output = ctx.run(editor_input(vec![]), |ctx| app.update(ctx));
        let text = output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .filter_map(|shape| match shape {
                super::egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(
            text.iter().any(|text| text == "MIDI IN"),
            "MIDI panel should have a stable input label"
        );
        assert!(
            text.iter().all(|text| text != "IDLE"),
            "MIDI panel should not label no activity as IDLE"
        );
    }

    fn render_test_ui(
        mut add_contents: impl FnMut(&mut super::egui::Ui),
    ) -> Vec<super::egui::Shape> {
        let ctx = super::egui::Context::default();
        super::install_retro_style(&ctx);
        let output = ctx.run(
            super::egui::RawInput {
                screen_rect: Some(super::rect(0.0, 0.0, 240.0, 180.0)),
                ..super::egui::RawInput::default()
            },
            |ctx| {
                super::egui::CentralPanel::default()
                    .frame(super::egui::Frame::default())
                    .show(ctx, |ui| add_contents(ui));
            },
        );

        output
            .shapes
            .into_iter()
            .flat_map(|shape| flatten_shape(shape.shape))
            .collect()
    }

    fn render_program_list_frame(
        ctx: &super::egui::Context,
        programs: &[ProgramChoice],
        first: &mut usize,
        events: Vec<super::egui::Event>,
    ) {
        let mut scroll_remainder = 0.0;
        let _ = ctx.run(
            super::egui::RawInput {
                screen_rect: Some(super::rect(0.0, 0.0, 240.0, 180.0)),
                events,
                ..super::egui::RawInput::default()
            },
            |ctx| {
                super::egui::CentralPanel::default()
                    .frame(super::egui::Frame::default())
                    .show(ctx, |ui| {
                        super::program_list(
                            ui,
                            super::rect(0.0, 0.0, 160.0, 96.0),
                            programs,
                            0,
                            first,
                            &mut scroll_remainder,
                        );
                    });
            },
        );
    }

    fn editor_input(events: Vec<super::egui::Event>) -> super::egui::RawInput {
        super::egui::RawInput {
            screen_rect: Some(super::rect(
                0.0,
                0.0,
                super::EDITOR_WIDTH as f32,
                super::EDITOR_HEIGHT as f32,
            )),
            events,
            ..super::egui::RawInput::default()
        }
    }

    fn click_editor(
        ctx: &super::egui::Context,
        app: &mut super::SoundfontEditorApp,
        pos: super::egui::Pos2,
    ) {
        let _ = ctx.run(
            editor_input(vec![
                super::egui::Event::PointerMoved(pos),
                super::egui::Event::PointerButton {
                    pos,
                    button: super::egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: super::egui::Modifiers::default(),
                },
            ]),
            |ctx| app.update(ctx),
        );
        let _ = ctx.run(
            editor_input(vec![
                super::egui::Event::PointerMoved(pos),
                super::egui::Event::PointerButton {
                    pos,
                    button: super::egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: super::egui::Modifiers::default(),
                },
            ]),
            |ctx| app.update(ctx),
        );
    }

    fn double_click_editor(
        ctx: &super::egui::Context,
        app: &mut super::SoundfontEditorApp,
        pos: super::egui::Pos2,
    ) {
        for (time, pressed) in [(1.00, true), (1.01, false), (1.08, true), (1.09, false)] {
            let mut input = editor_input(vec![
                super::egui::Event::PointerMoved(pos),
                super::egui::Event::PointerButton {
                    pos,
                    button: super::egui::PointerButton::Primary,
                    pressed,
                    modifiers: super::egui::Modifiers::default(),
                },
            ]);
            input.time = Some(time);
            let _ = ctx.run(input, |ctx| app.update(ctx));
        }
    }

    fn editor_app_with_soundfonts(
        loaded: &LoadedSoundfont,
        ids: &[&str],
    ) -> super::SoundfontEditorApp {
        editor_app_with_presets(loaded, ids, 1)
    }

    fn editor_app_with_presets(
        loaded: &LoadedSoundfont,
        ids: &[&str],
        preset_count: u8,
    ) -> super::SoundfontEditorApp {
        let catalog = ids
            .iter()
            .map(|id| super::SoundfontCatalogEntry {
                id: (*id).to_string(),
                name: (*id).to_string(),
                presets: Arc::new(
                    (0..preset_count)
                        .map(|program| SoundfontPreset {
                            bank: 0,
                            program,
                            name: format!("Program {program}"),
                        })
                        .collect(),
                ),
            })
            .collect::<Vec<_>>();
        let available = ids
            .iter()
            .map(|id| ((*id).to_string(), Arc::clone(&loaded.soundfont)))
            .collect::<std::collections::HashMap<_, _>>();
        let state = SoundfontProcessorState {
            soundfont_id: ids[0].to_string(),
            ..SoundfontProcessorState::default()
        };
        super::SoundfontEditorApp {
            shared: Arc::new(super::SharedSoundfontBinding {
                catalog: Arc::new(catalog),
                available_soundfonts: Arc::new(available),
                state: super::SharedSoundfontState::new(&state, Arc::clone(&loaded.soundfont)),
            }),
            retro_style_installed: false,
            bank_text: String::new(),
            bank_text_focused: false,
            polyphony_text: String::new(),
            polyphony_text_focused: false,
            program_scroll_first: 0,
            program_scroll_remainder: 0.0,
            soundfont_dropdown_open: false,
            seen_midi_activity: 0,
            midi_flash_frames: 0,
        }
    }

    fn flatten_shape(shape: super::egui::Shape) -> Vec<super::egui::Shape> {
        match shape {
            super::egui::Shape::Vec(shapes) => shapes.into_iter().flat_map(flatten_shape).collect(),
            shape => vec![shape],
        }
    }

    fn text_shape<'a>(
        shapes: &'a [super::egui::Shape],
        expected: &str,
    ) -> &'a super::egui::epaint::TextShape {
        shapes
            .iter()
            .find_map(|shape| match shape {
                super::egui::Shape::Text(text) if text.galley.text().contains(expected) => {
                    Some(text)
                }
                _ => None,
            })
            .expect("expected text should be painted")
    }

    #[test]
    fn soundfont_processor_selected_program_changes_rendered_signal() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let settings = SoundfontSynthSettings::new(44_100, 64);
        let mut piano = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 0,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");
        let mut violin = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");

        piano.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        violin.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut piano_left = vec![0.0; 512];
        let mut piano_right = vec![0.0; 512];
        let mut violin_left = vec![0.0; 512];
        let mut violin_right = vec![0.0; 512];

        for _ in 0..8 {
            piano.render(&mut piano_left, &mut piano_right);
            violin.render(&mut violin_left, &mut violin_right);
        }

        assert!(
            piano_left
                .iter()
                .zip(violin_left.iter())
                .chain(piano_right.iter().zip(violin_right.iter()))
                .any(|(a, b)| (a - b).abs() > 1.0e-6),
            "different selected SoundFont programs rendered the same signal"
        );
    }

    #[test]
    fn soundfont_processor_reset_preserves_selected_program() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let settings = SoundfontSynthSettings::new(44_100, 64);
        let mut violin = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 40,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");
        let mut piano = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState {
                soundfont_id: "default".to_string(),
                bank: 0,
                program: 0,
                ..SoundfontProcessorState::default()
            },
        )
        .expect("processor should initialize");

        violin.reset();
        violin.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        piano.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut violin_left = vec![0.0; 512];
        let mut violin_right = vec![0.0; 512];
        let mut piano_left = vec![0.0; 512];
        let mut piano_right = vec![0.0; 512];

        for _ in 0..8 {
            violin.render(&mut violin_left, &mut violin_right);
            piano.render(&mut piano_left, &mut piano_right);
        }

        assert!(
            violin_left
                .iter()
                .zip(piano_left.iter())
                .chain(violin_right.iter().zip(piano_right.iter()))
                .any(|(a, b)| (a - b).abs() > 1.0e-6),
            "reset restored the SoundFont processor to the default piano program"
        );
    }

    #[test]
    fn soundfont_processor_reset_restores_silent_fast_path() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        processor.render(&mut left, &mut right);

        processor.reset();
        left.fill(1.0);
        right.fill(1.0);
        processor.render(&mut left, &mut right);
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.abs() <= 1.0e-6),
            "soundfont processor reset should restore the silent fast path"
        );
    }

    #[test]
    fn soundfont_processor_returns_to_silent_fast_path_after_release_tail() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        processor.handle_midi(MidiEvent::NoteOff {
            channel: 0,
            note: 60,
            velocity: 0,
        });

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        for _ in 0..1_024 {
            processor.render(&mut left, &mut right);
            if !processor.needs_render {
                return;
            }
        }

        panic!("soundfont processor never returned to the silent fast path after note release");
    }

    #[test]
    fn soundfont_processor_reports_sleeping_when_dormant() {
        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let mut processor = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            SoundfontSynthSettings::new(44_100, 64),
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");

        assert!(
            processor.is_sleeping(),
            "fresh soundfont processor should start dormant"
        );

        processor.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        assert!(
            !processor.is_sleeping(),
            "note on should wake the processor"
        );

        processor.handle_midi(MidiEvent::AllSoundOff { channel: 0 });
        assert!(
            processor.is_sleeping(),
            "all sound off should return the processor to dormant state"
        );
    }

    #[test]
    #[ignore = "manual perf report"]
    fn perf_report_soundfont_processor_block_costs() {
        const BLOCKS: usize = 20_000;
        const BLOCK_SIZE: usize = 64;

        let loaded =
            LoadedSoundfont::load(&test_soundfont_resource()).expect("test SoundFont should load");
        let settings = SoundfontSynthSettings::new(44_100, BLOCK_SIZE);

        let mut idle = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");
        let mut armed = SoundfontProcessor::new(
            &Arc::clone(&loaded.soundfont),
            settings,
            SoundfontProcessorState::default(),
        )
        .expect("processor should initialize");
        armed.handle_midi(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });

        let mut idle_left = vec![0.0; BLOCK_SIZE];
        let mut idle_right = vec![0.0; BLOCK_SIZE];
        let idle_started = Instant::now();
        for _ in 0..BLOCKS {
            idle.render(&mut idle_left, &mut idle_right);
        }
        let idle_elapsed = idle_started.elapsed();

        let mut armed_left = vec![0.0; BLOCK_SIZE];
        let mut armed_right = vec![0.0; BLOCK_SIZE];
        let armed_started = Instant::now();
        for _ in 0..BLOCKS {
            armed.render(&mut armed_left, &mut armed_right);
        }
        let armed_elapsed = armed_started.elapsed();

        println!(
            "soundfont processor perf over {BLOCKS} blocks: idle={idle_elapsed:?} armed={armed_elapsed:?}"
        );
    }
}
