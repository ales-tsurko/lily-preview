use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::engine::AudioEngineSettings;
use crate::instrument::metronome_synth::{MetronomeProcessor, SharedMetronomeState};
use crate::instrument::{
    Controller, EffectProcessorNode, EffectRuntimeHandle, InstrumentProcessor,
    InstrumentProcessorNode, InstrumentRuntimeContext, InstrumentRuntimeHandle,
    ProcessorStateError, RuntimeBinding, RuntimeFactoryError, ScheduledInstrumentEvent,
    SharedInstrumentResetState, SlotState, create_effect_runtime as build_effect_runtime_spec,
    create_instrument_runtime as build_instrument_runtime_spec, decode_instrument_event,
    generation_is_current_or_newer, registry,
};
use crate::mixer::{
    BusId, BusSend, ChannelMeterSnapshot, MixerError, MixerMeterSnapshot, MixerMeterSnapshotWindow,
    MixerState, STRIP_METER_MAX_DB, STRIP_METER_MIN_DB, SlotAddress, StripMeterSnapshot, Track,
    TrackId, TrackRoute,
};
use crate::soundfont::{
    LoadedSoundfont, SoundfontResource, SoundfontSynthError, SoundfontSynthSettings,
};
use knyst::graph::GenOrGraph;
use knyst::graph::connection::InputBundle;
use knyst::inputs;
use knyst::modal_interface::{KnystContext, knyst_commands};
use knyst::prelude::{
    BlockSize, Connection, GenState, GenericHandle, Handle, HandleData, KnystCommands,
    MultiThreadedKnystCommands, NodeId, Sample, bus, graph_output, handle, impl_gen,
};
use wide::f32x4;

#[derive(thiserror::Error, Debug)]
pub(crate) enum MixerRuntimeError {
    #[error(transparent)]
    Mixer(#[from] MixerError),
    #[error(transparent)]
    Soundfont(#[from] SoundfontSynthError),
    #[error(transparent)]
    ProcessorState(#[from] ProcessorStateError),
    #[error(transparent)]
    RuntimeFactory(#[from] RuntimeFactoryError),
}

pub(crate) enum TrackInstrumentSync {
    GraphChanged,
    UpdatedInPlace,
}

const METER_FLOOR: f32 = 0.00003162278;

#[derive(Debug, Clone)]
struct SharedStripMeter {
    inner: Arc<SharedStripMeterInner>,
    sample_rate: f32,
    block_size: usize,
}

#[derive(Debug, Default)]
struct SharedStripMeterInner {
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    hold_l: AtomicU32,
    hold_r: AtomicU32,
    clip_latched: AtomicBool,
}

impl SharedStripMeter {
    fn new(sample_rate: usize, block_size: usize) -> Self {
        Self {
            inner: Arc::new(SharedStripMeterInner::default()),
            sample_rate: sample_rate.max(1) as f32,
            block_size: block_size.max(1),
        }
    }

    fn observe_stereo(&self, left: f32, right: f32) {
        let left = left.abs();
        let right = right.abs();

        let peak_l = f32::from_bits(self.inner.peak_l.load(Ordering::Relaxed));
        let peak_r = f32::from_bits(self.inner.peak_r.load(Ordering::Relaxed));
        let displayed_l = apply_meter_release(peak_l, left, self.sample_rate, self.block_size);
        let displayed_r = apply_meter_release(peak_r, right, self.sample_rate, self.block_size);

        self.inner
            .peak_l
            .store(displayed_l.to_bits(), Ordering::Relaxed);
        self.inner
            .peak_r
            .store(displayed_r.to_bits(), Ordering::Relaxed);

        let hold_l = f32::from_bits(self.inner.hold_l.load(Ordering::Relaxed));
        if left > hold_l {
            self.inner.hold_l.store(left.to_bits(), Ordering::Relaxed);
        }

        let hold_r = f32::from_bits(self.inner.hold_r.load(Ordering::Relaxed));
        if right > hold_r {
            self.inner.hold_r.store(right.to_bits(), Ordering::Relaxed);
        }

        if left >= 1.0 || right >= 1.0 {
            self.inner.clip_latched.store(true, Ordering::Relaxed);
        }
    }

    fn reset(&self) {
        self.inner.peak_l.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.inner.peak_r.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.inner.hold_l.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.inner.hold_r.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.inner.clip_latched.store(false, Ordering::Relaxed);
    }

    fn snapshot(&self) -> StripMeterSnapshot {
        let peak_l = f32::from_bits(self.inner.peak_l.load(Ordering::Relaxed));
        let peak_r = f32::from_bits(self.inner.peak_r.load(Ordering::Relaxed));
        let hold_l = f32::from_bits(self.inner.hold_l.load(Ordering::Relaxed));
        let hold_r = f32::from_bits(self.inner.hold_r.load(Ordering::Relaxed));

        StripMeterSnapshot {
            left: ChannelMeterSnapshot {
                level: normalize_meter_level(peak_l),
                hold: normalize_meter_level(hold_l),
                hold_db: amplitude_to_db(hold_l).max(STRIP_METER_MIN_DB),
            },
            right: ChannelMeterSnapshot {
                level: normalize_meter_level(peak_r),
                hold: normalize_meter_level(hold_r),
                hold_db: amplitude_to_db(hold_r).max(STRIP_METER_MIN_DB),
            },
            clip_latched: self.inner.clip_latched.load(Ordering::Relaxed),
        }
    }
}

impl Default for SharedStripMeter {
    fn default() -> Self {
        Self::new(44_100, 64)
    }
}

#[derive(Debug, Clone)]
struct SharedStripLevel {
    inner: Arc<SharedStripLevelInner>,
}

#[derive(Debug)]
struct SharedStripLevelInner {
    gain: AtomicU32,
    pan: AtomicU32,
}

impl SharedStripLevel {
    fn new(gain: f32, pan: f32) -> Self {
        Self {
            inner: Arc::new(SharedStripLevelInner {
                gain: AtomicU32::new(gain.to_bits()),
                pan: AtomicU32::new(pan.to_bits()),
            }),
        }
    }

    fn set(&self, gain: f32, pan: f32) {
        self.inner.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.inner.pan.store(pan.to_bits(), Ordering::Relaxed);
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.inner.gain.load(Ordering::Relaxed))
    }

    fn pan(&self) -> f32 {
        f32::from_bits(self.inner.pan.load(Ordering::Relaxed))
    }
}

fn normalize_meter_level(amplitude: f32) -> f32 {
    let db = 20.0 * amplitude.abs().max(METER_FLOOR).log10();
    ((db - STRIP_METER_MIN_DB) / (STRIP_METER_MAX_DB - STRIP_METER_MIN_DB)).clamp(0.0, 1.0)
}

const METER_RELEASE_DB_PER_SECOND: f32 = 18.0;

fn amplitude_to_db(amplitude: f32) -> f32 {
    20.0 * amplitude.abs().max(METER_FLOOR).log10()
}

fn apply_meter_release(current: f32, observed: f32, sample_rate: f32, block_size: usize) -> f32 {
    if observed >= current {
        return observed;
    }

    let current_db = amplitude_to_db(current);
    let observed_db = amplitude_to_db(observed);
    let block_seconds = block_size as f32 / sample_rate.max(1.0);
    let released_db = current_db - METER_RELEASE_DB_PER_SECOND * block_seconds;
    db_to_amplitude(released_db.max(observed_db).max(STRIP_METER_MIN_DB))
}

pub(crate) struct MixerRuntime {
    master: MasterRuntime,
    metronome: MetronomeRuntime,
    tracks: Vec<Option<TrackRuntime>>,
    buses: HashMap<BusId, BusRuntime>,
    soundfonts: HashMap<String, LoadedSoundfont>,
    soundfont_settings: SoundfontSynthSettings,
    meter_settings: AudioEngineSettings,
}

impl MixerRuntime {
    pub(crate) fn free(self) {
        self.master.free();
        self.metronome.free();
        for runtime in self.tracks.into_iter().flatten() {
            runtime.free();
        }
        for runtime in self.buses.into_values() {
            runtime.free();
        }
    }
}

impl MixerRuntime {
    pub(crate) fn meter_settings(&self) -> AudioEngineSettings {
        self.meter_settings
    }

    pub(crate) fn controller(
        &self,
        mixer: &MixerState,
        address: SlotAddress,
    ) -> Result<Option<Box<dyn Controller>>, MixerRuntimeError> {
        let Some(strip) = mixer.strip_by_index(address.strip_index) else {
            return Ok(None);
        };
        let Some(_slot) = strip.slot(address.slot_index) else {
            return Ok(None);
        };

        if address.strip_index == 0 {
            return Ok(self
                .master
                .effects
                .get(address.slot_index.checked_sub(1).unwrap_or(usize::MAX))
                .and_then(|runtime| runtime.as_ref())
                .and_then(EffectRuntime::controller));
        }

        if let Some(track_offset) = address.strip_index.checked_sub(1)
            && track_offset < mixer.track_count()
        {
            let Some(runtime) = self
                .tracks
                .get(track_offset)
                .and_then(|runtime| runtime.as_ref())
            else {
                return Ok(None);
            };
            return Ok(match address.slot_index {
                0 => runtime
                    .instrument
                    .as_ref()
                    .map(TrackInstrumentRuntime::controller),
                effect_index => runtime
                    .effects
                    .get(effect_index - 1)
                    .and_then(|runtime| runtime.as_ref())
                    .and_then(EffectRuntime::controller),
            });
        }

        let Some(bus_id) = strip.bus_id else {
            return Ok(None);
        };
        let Some(runtime) = self.buses.get(&bus_id) else {
            return Ok(None);
        };
        Ok(runtime
            .effects
            .get(address.slot_index.checked_sub(1).unwrap_or(usize::MAX))
            .and_then(|runtime| runtime.as_ref())
            .and_then(EffectRuntime::controller))
    }

    pub(crate) fn instrument_handle(&self, track_id: TrackId) -> Option<InstrumentRuntimeHandle> {
        Some(
            self.tracks
                .get(track_id.index())?
                .as_ref()?
                .instrument
                .as_ref()?
                .handle
                .clone(),
        )
    }

    pub(crate) fn metronome_handle(&self) -> InstrumentRuntimeHandle {
        self.metronome.handle.clone()
    }

    pub(crate) fn set_metronome_gain_db(&self, gain_db: f32) {
        self.metronome.shared.set_gain_db(gain_db);
    }

    pub(crate) fn set_metronome_pitch(&self, pitch: f32) {
        self.metronome.shared.set_pitch(pitch);
    }

    pub(crate) fn meter_snapshot(&self, mixer: &MixerState) -> MixerMeterSnapshot {
        MixerMeterSnapshot {
            main: self.master.meter.snapshot(),
            tracks: mixer
                .tracks()
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    self.tracks
                        .get(index)
                        .and_then(|runtime| runtime.as_ref())
                        .map_or_else(StripMeterSnapshot::default, |runtime| {
                            runtime.meter.snapshot()
                        })
                })
                .collect(),
            buses: mixer
                .buses()
                .iter()
                .filter_map(|bus| {
                    let bus_id = bus.bus_id?;
                    Some((
                        bus_id,
                        self.buses
                            .get(&bus_id)
                            .map_or_else(StripMeterSnapshot::default, |runtime| {
                                runtime.meter.snapshot()
                            }),
                    ))
                })
                .collect(),
        }
    }

    pub(crate) fn meter_snapshot_window(
        &self,
        mixer: &MixerState,
        track_range: std::ops::Range<usize>,
        bus_range: std::ops::Range<usize>,
    ) -> MixerMeterSnapshotWindow {
        let track_end = track_range.end.min(mixer.tracks().len());
        let bus_end = bus_range.end.min(mixer.buses().len());

        MixerMeterSnapshotWindow {
            main: self.master.meter.snapshot(),
            tracks: mixer.tracks()[track_range.start.min(track_end)..track_end]
                .iter()
                .enumerate()
                .map(|(offset, _)| {
                    let index = track_range.start + offset;
                    self.tracks
                        .get(index)
                        .and_then(|runtime| runtime.as_ref())
                        .map_or_else(StripMeterSnapshot::default, |runtime| {
                            runtime.meter.snapshot()
                        })
                })
                .collect(),
            buses: mixer.buses()[bus_range.start.min(bus_end)..bus_end]
                .iter()
                .enumerate()
                .filter_map(|(offset, _)| {
                    let index = bus_range.start + offset;
                    let id = mixer.buses()[index].bus_id?;
                    Some(
                        self.buses
                            .get(&id)
                            .map_or_else(StripMeterSnapshot::default, |runtime| {
                                runtime.meter.snapshot()
                            }),
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn reset_meters(&self) {
        self.master.meter.reset();
        for runtime in self.tracks.iter().flatten() {
            runtime.meter.reset();
        }
        for runtime in self.buses.values() {
            runtime.meter.reset();
        }
    }

    pub(crate) fn reset_master_meter(&self) {
        self.master.meter.reset();
    }

    pub(crate) fn reset_track_meter(&self, id: TrackId) -> Result<(), MixerRuntimeError> {
        let runtime = self
            .tracks
            .get(id.index())
            .ok_or(MixerError::InvalidTrackId(id))?
            .as_ref()
            .ok_or(MixerError::InvalidTrackId(id))?;
        runtime.meter.reset();
        Ok(())
    }

    pub(crate) fn reset_bus_meter(&self, id: BusId) -> Result<(), MixerRuntimeError> {
        let runtime = self.buses.get(&id).ok_or(MixerError::InvalidBusId(id))?;
        runtime.meter.reset();
        Ok(())
    }

    pub(crate) fn attach(
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        settings: &AudioEngineSettings,
        mixer: &MixerState,
    ) -> Result<Self, MixerRuntimeError> {
        context.with_activation(|| {
            let master = MasterRuntime::new(context, commands, settings, mixer);
            let metronome = MetronomeRuntime::new(context, master.input_node(), settings);
            let soundfont_settings =
                SoundfontSynthSettings::new(settings.sample_rate as i32, settings.block_size);

            let mut buses = HashMap::with_capacity(mixer.buses().len());
            for bus_track in mixer.buses() {
                if let Some(bus_id) = bus_track.bus_id {
                    buses.insert(
                        bus_id,
                        BusRuntime::new(context, commands, settings, bus_track, mixer),
                    );
                }
            }

            let mut runtime = Self {
                master,
                metronome,
                tracks: Vec::with_capacity(mixer.tracks().len()),
                buses,
                soundfonts: HashMap::new(),
                soundfont_settings,
                meter_settings: *settings,
            };
            runtime.sync_soundfonts(mixer)?;

            let mut tracks = Vec::with_capacity(mixer.tracks().len());
            for track in mixer.tracks() {
                tracks.push(if track_needs_runtime(track) {
                    let bus_inputs = runtime.bus_input_nodes();
                    Some(TrackRuntime::new(
                        context,
                        commands,
                        track,
                        TrackRuntimeBuildContext {
                            settings,
                            mixer,
                            master_input: runtime.master.input_node(),
                            bus_inputs: &bus_inputs,
                            soundfont_resources: mixer.soundfonts(),
                            soundfonts: &runtime.soundfonts,
                            soundfont_settings: runtime.soundfont_settings,
                        },
                    )?)
                } else {
                    None
                });
            }
            runtime.tracks = tracks;
            runtime.sync_all_routing(context, commands, mixer)?;
            runtime.sync_all_levels(mixer);
            Ok(runtime)
        })
    }

    pub(crate) fn sync_soundfonts(&mut self, mixer: &MixerState) -> Result<(), MixerRuntimeError> {
        let resources: HashMap<_, _> = mixer
            .soundfonts()
            .iter()
            .map(|resource| (resource.id.clone(), resource))
            .collect();

        self.soundfonts.retain(|id, _| resources.contains_key(id));

        for resource in mixer.soundfonts() {
            let should_reload = self
                .soundfonts
                .get(&resource.id)
                .is_none_or(|loaded| loaded.path != resource.path);
            if should_reload {
                let loaded = LoadedSoundfont::load(resource)?;
                self.soundfonts.insert(resource.id.clone(), loaded);
            }
        }

        Ok(())
    }

    pub(crate) fn sync_tracks_after_soundfonts_changed(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
    ) -> Result<(), MixerRuntimeError> {
        for (track_id, track) in mixer.tracks_with_ids() {
            if track.instrument_slot().is_none() {
                continue;
            }
            if matches!(
                self.sync_track_instrument(context, commands, mixer, track_id)?,
                TrackInstrumentSync::GraphChanged
            ) {
                self.sync_track_routing(context, commands, mixer, track_id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn add_bus(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        bus_id: BusId,
    ) -> Result<(), MixerRuntimeError> {
        let bus_track = mixer.bus(bus_id)?;
        context.with_activation(|| {
            self.buses.insert(
                bus_id,
                BusRuntime::new(context, commands, &self.meter_settings, bus_track, mixer),
            );
        });
        self.sync_bus_routing(context, commands, mixer, bus_id)?;
        self.sync_all_levels(mixer);
        Ok(())
    }

    pub(crate) fn remove_bus(
        &mut self,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        bus_id: BusId,
    ) -> Result<(), MixerRuntimeError> {
        if let Some(runtime) = self.buses.remove(&bus_id) {
            runtime.free();
        }
        self.sync_all_routing_no_create(commands, mixer)?;
        self.sync_all_levels(mixer);
        Ok(())
    }

    pub(crate) fn sync_track_instrument(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        track_id: TrackId,
    ) -> Result<TrackInstrumentSync, MixerRuntimeError> {
        let track = mixer.track(track_id)?;
        let master_input = self.master.input_node();
        let bus_inputs = self.bus_input_nodes();
        let runtime = self
            .tracks
            .get_mut(track_id.index())
            .ok_or(MixerError::InvalidTrackId(track_id))?;
        if !track_needs_runtime(track) {
            if let Some(runtime) = runtime.take() {
                runtime.free();
            }
            return Ok(TrackInstrumentSync::GraphChanged);
        }
        if let Some(runtime) = runtime.as_mut() {
            if runtime.sync_source(
                context,
                commands,
                track,
                mixer.soundfonts(),
                &self.soundfonts,
                self.soundfont_settings,
            )? {
                return Ok(TrackInstrumentSync::UpdatedInPlace);
            }
        } else {
            *runtime = Some(TrackRuntime::new(
                context,
                commands,
                track,
                TrackRuntimeBuildContext {
                    settings: &self.meter_settings,
                    mixer,
                    master_input,
                    bus_inputs: &bus_inputs,
                    soundfont_resources: mixer.soundfonts(),
                    soundfonts: &self.soundfonts,
                    soundfont_settings: self.soundfont_settings,
                },
            )?);
        }
        Ok(TrackInstrumentSync::GraphChanged)
    }

    pub(crate) fn sync_track_effects(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        track_id: TrackId,
    ) -> Result<(), MixerRuntimeError> {
        let track = mixer.track(track_id)?;
        let master_input = self.master.input_node();
        let bus_inputs = self.bus_input_nodes();
        let runtime = self
            .tracks
            .get_mut(track_id.index())
            .ok_or(MixerError::InvalidTrackId(track_id))?;
        if !track_needs_runtime(track) {
            if let Some(runtime) = runtime.take() {
                runtime.free();
            }
            return Ok(());
        }
        if let Some(existing) = runtime.take() {
            let mut existing = existing;
            if existing.rebuild_effects(context, commands, track) {
                *runtime = Some(existing);
            } else {
                existing.free();
                *runtime = Some(TrackRuntime::new(
                    context,
                    commands,
                    track,
                    TrackRuntimeBuildContext {
                        settings: &self.meter_settings,
                        mixer,
                        master_input,
                        bus_inputs: &bus_inputs,
                        soundfont_resources: mixer.soundfonts(),
                        soundfonts: &self.soundfonts,
                        soundfont_settings: self.soundfont_settings,
                    },
                )?);
            }
        } else {
            *runtime = Some(TrackRuntime::new(
                context,
                commands,
                track,
                TrackRuntimeBuildContext {
                    settings: &self.meter_settings,
                    mixer,
                    master_input,
                    bus_inputs: &bus_inputs,
                    soundfont_resources: mixer.soundfonts(),
                    soundfonts: &self.soundfonts,
                    soundfont_settings: self.soundfont_settings,
                },
            )?);
        }
        Ok(())
    }

    pub(crate) fn sync_track_strip(
        &mut self,
        mixer: &MixerState,
        track_id: TrackId,
    ) -> Result<(), MixerRuntimeError> {
        let amplitude = track_effective_amplitude(mixer, mixer.track(track_id)?);
        let track = mixer.track(track_id)?;
        let runtime = self
            .tracks
            .get_mut(track_id.index())
            .ok_or(MixerError::InvalidTrackId(track_id))?;
        if let Some(runtime) = runtime.as_mut() {
            runtime.apply_strip(track, amplitude);
        }
        Ok(())
    }

    pub(crate) fn sync_bus_strip(
        &mut self,
        mixer: &MixerState,
        bus_id: BusId,
    ) -> Result<(), MixerRuntimeError> {
        let amplitude = bus_effective_amplitude(mixer.bus(bus_id)?);
        let bus = mixer.bus(bus_id)?;
        let runtime = self
            .buses
            .get_mut(&bus_id)
            .ok_or(MixerError::InvalidBusId(bus_id))?;
        runtime.apply_strip(bus, amplitude);
        Ok(())
    }

    pub(crate) fn sync_bus_effects(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        bus_id: BusId,
    ) -> Result<(), MixerRuntimeError> {
        let bus = mixer.bus(bus_id)?;
        let runtime = self
            .buses
            .get_mut(&bus_id)
            .ok_or(MixerError::InvalidBusId(bus_id))?;
        runtime.rebuild_effects(context, commands, bus);
        Ok(())
    }

    pub(crate) fn sync_master_effects(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
    ) -> Result<(), MixerRuntimeError> {
        self.master
            .rebuild_effects(context, commands, mixer.master());
        Ok(())
    }

    pub(crate) fn sync_track_routing(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        track_id: TrackId,
    ) -> Result<(), MixerRuntimeError> {
        let bus_inputs = self.bus_input_nodes();
        let track = mixer.track(track_id)?;
        let runtime_slot = self
            .tracks
            .get_mut(track_id.index())
            .ok_or(MixerError::InvalidTrackId(track_id))?;
        let needs_rebuild = runtime_slot
            .as_ref()
            .is_some_and(|runtime| !runtime.matches_signal_path(track));
        if needs_rebuild && let Some(old_runtime) = runtime_slot.take() {
            old_runtime.free();
            *runtime_slot = Some(TrackRuntime::new(
                context,
                commands,
                track,
                TrackRuntimeBuildContext {
                    settings: &self.meter_settings,
                    mixer,
                    master_input: self.master.input_node(),
                    bus_inputs: &bus_inputs,
                    soundfont_resources: mixer.soundfonts(),
                    soundfonts: &self.soundfonts,
                    soundfont_settings: self.soundfont_settings,
                },
            )?);
            return Ok(());
        }
        if let Some(runtime) = runtime_slot.as_mut() {
            runtime.sync_routing(
                context,
                commands,
                mixer,
                self.master.input_node(),
                &bus_inputs,
                track,
            )?;
        }
        Ok(())
    }

    pub(crate) fn sync_bus_routing(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        bus_id: BusId,
    ) -> Result<(), MixerRuntimeError> {
        let bus_inputs = self.bus_input_nodes();
        let bus = mixer.bus(bus_id)?;
        let runtime = self
            .buses
            .get_mut(&bus_id)
            .ok_or(MixerError::InvalidBusId(bus_id))?;
        runtime.sync_routing(
            context,
            commands,
            mixer,
            self.master.input_node(),
            &bus_inputs,
            bus,
        )?;
        Ok(())
    }

    pub(crate) fn sync_all_routing(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
    ) -> Result<(), MixerRuntimeError> {
        for (track_id, _) in mixer.tracks_with_ids() {
            self.sync_track_routing(context, commands, mixer, track_id)?;
        }
        let bus_ids: Vec<_> = mixer.buses_with_ids().map(|(bus_id, _)| bus_id).collect();
        for bus_id in bus_ids {
            self.sync_bus_routing(context, commands, mixer, bus_id)?;
        }
        self.sync_all_levels(mixer);
        Ok(())
    }

    fn sync_all_routing_no_create(
        &mut self,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
    ) -> Result<(), MixerRuntimeError> {
        let bus_inputs = self.bus_input_nodes();
        for (track_id, track) in mixer.tracks_with_ids() {
            let runtime = self
                .tracks
                .get_mut(track_id.index())
                .ok_or(MixerError::InvalidTrackId(track_id))?;
            if let Some(runtime) = runtime.as_mut() {
                runtime.sync_routing_existing(
                    commands,
                    mixer,
                    self.master.input_node(),
                    &bus_inputs,
                    track,
                )?;
            }
        }
        for (bus_id, bus) in mixer.buses_with_ids() {
            if let Some(runtime) = self.buses.get_mut(&bus_id) {
                runtime.sync_routing_existing(
                    commands,
                    mixer,
                    self.master.input_node(),
                    &bus_inputs,
                    bus,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn sync_all_levels(&mut self, mixer: &MixerState) {
        self.master.set_level(
            db_to_amplitude(mixer.master().state.gain_db),
            mixer.master().state.pan,
        );
        let track_amplitudes: Vec<_> = mixer
            .tracks()
            .iter()
            .map(|track| track_effective_amplitude(mixer, track))
            .collect();
        for (runtime, (track, amplitude)) in self
            .tracks
            .iter_mut()
            .zip(mixer.tracks().iter().zip(track_amplitudes))
        {
            if let Some(runtime) = runtime.as_mut() {
                runtime.apply_strip(track, amplitude);
            }
        }
        for (bus_id, bus) in mixer.buses_with_ids() {
            if let Some(runtime) = self.buses.get_mut(&bus_id) {
                runtime.apply_strip(bus, bus_effective_amplitude(bus));
            }
        }
    }

    fn bus_input_nodes(&self) -> HashMap<BusId, NodeId> {
        self.buses
            .iter()
            .map(|(bus_id, runtime)| (*bus_id, runtime.input_node()))
            .collect()
    }
}

struct MasterRuntime {
    input: Handle<GenericHandle>,
    effects: Vec<Option<EffectRuntime>>,
    strip: Handle<GenericHandle>,
    meter: SharedStripMeter,
    level: SharedStripLevel,
}

struct MetronomeRuntime {
    handle: InstrumentRuntimeHandle,
    shared: SharedMetronomeState,
}

impl MetronomeRuntime {
    fn new(context: &KnystContext, master_input: NodeId, settings: &AudioEngineSettings) -> Self {
        context.with_activation(|| {
            let reset_state = SharedInstrumentResetState::default();
            let shared = SharedMetronomeState::default();
            let processor = MetronomeProcessor::new(settings.sample_rate as f32, shared.clone());
            let handle = handle(InstrumentProcessorNode::new(
                Box::new(processor),
                reset_state.clone(),
            ));
            connect_stereo(node_id_of(handle), master_input);
            Self {
                handle: InstrumentRuntimeHandle::new(handle, reset_state),
                shared,
            }
        })
    }

    fn free(self) {
        knyst_commands().disconnect(Connection::clear_from_nodes(self.handle.node_id()));
        knyst_commands().disconnect(Connection::clear_to_nodes(self.handle.node_id()));
        knyst_commands().free_node(self.handle.node_id());
    }
}

impl MasterRuntime {
    fn new(
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        settings: &AudioEngineSettings,
        mixer: &MixerState,
    ) -> Self {
        let meter = SharedStripMeter::new(settings.sample_rate, settings.block_size);
        let level = SharedStripLevel::new(
            db_to_amplitude(mixer.master().state.gain_db),
            mixer.master().state.pan,
        );
        let strip = handle_with_inputs(
            commands,
            StereoBalanceMeter::new(level.clone(), meter.clone()),
            inputs!(),
        );
        let input = context.with_activation(|| {
            let input = bus(2);
            graph_output(0, strip.channels(2));
            input
        });
        let mut runtime = Self {
            input,
            effects: Vec::new(),
            strip,
            meter,
            level,
        };
        runtime.rebuild_effects(context, commands, mixer.master());
        runtime
    }

    fn input_node(&self) -> NodeId {
        node_id_of(self.input)
    }

    fn set_level(&mut self, gain: f32, pan: f32) {
        self.level.set(gain, pan);
    }

    fn rebuild_effects(
        &mut self,
        context: &KnystContext,
        _commands: &mut MultiThreadedKnystCommands,
        track: &Track,
    ) {
        context.with_activation(|| {
            knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.input)));
            for effect in self.effects.drain(..).flatten() {
                free_effect(effect);
            }
            let mut previous = node_id_of(self.input);
            for effect_slot in track.effect_states() {
                let effect = create_effect_runtime(effect_slot);
                if let Some(effect) = effect.as_ref().filter(|_| !effect_slot.bypassed) {
                    let node = effect.node_id();
                    connect_stereo(previous, node);
                    previous = node;
                }
                self.effects.push(effect);
            }
            connect_stereo(previous, node_id_of(self.strip));
        });
    }

    fn free(self) {
        for effect in self.effects.into_iter().flatten() {
            free_effect(effect);
        }
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.strip)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.strip)));
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.input)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.input)));
        knyst_commands().free_node(node_id_of(self.strip));
        knyst_commands().free_node(node_id_of(self.input));
    }
}

struct TrackRuntime {
    effects: Vec<Option<EffectRuntime>>,
    meter: SharedStripMeter,
    level: SharedStripLevel,
    route_bus: Handle<GenericHandle>,
    instrument: Option<TrackInstrumentRuntime>,
    send_nodes: Vec<NodeId>,
    signal_path: TrackSignalPath,
}

struct TrackRuntimeBuildContext<'a> {
    settings: &'a AudioEngineSettings,
    mixer: &'a MixerState,
    master_input: NodeId,
    bus_inputs: &'a HashMap<BusId, NodeId>,
    soundfont_resources: &'a [SoundfontResource],
    soundfonts: &'a HashMap<String, LoadedSoundfont>,
    soundfont_settings: SoundfontSynthSettings,
}

impl TrackRuntime {
    fn pre_send_source_node(&self) -> NodeId {
        match &self.signal_path {
            TrackSignalPath::Separated { source_bus, .. } => node_id_of(*source_bus),
            TrackSignalPath::Combined => self
                .instrument
                .as_ref()
                .map(|instrument| instrument.handle.node_id())
                .unwrap_or_else(|| node_id_of(self.route_bus)),
        }
    }

    fn post_send_source_node(&self) -> NodeId {
        match &self.signal_path {
            TrackSignalPath::Separated { strip, .. } => node_id_of(*strip),
            TrackSignalPath::Combined => self
                .instrument
                .as_ref()
                .map(|instrument| instrument.handle.node_id())
                .unwrap_or_else(|| node_id_of(self.route_bus)),
        }
    }

    fn new(
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        track: &Track,
        build: TrackRuntimeBuildContext<'_>,
    ) -> Result<Self, MixerRuntimeError> {
        let initial_gain = track_effective_amplitude(build.mixer, track);
        let meter = SharedStripMeter::new(build.settings.sample_rate, build.settings.block_size);
        let level = SharedStripLevel::new(initial_gain, track.state.pan);
        let route_bus = context.with_activation(|| bus(2));

        let mut instrument = None;
        let signal_path = if track_prefers_combined_signal_path(track) {
            if let Some(created_instrument) = create_track_instrument(
                context,
                track,
                build.soundfont_resources,
                build.soundfonts,
                build.soundfont_settings,
                Some((level.clone(), meter.clone())),
            )? {
                context.with_activation(|| {
                    created_instrument.connect(node_id_of(route_bus));
                });
                instrument = Some(created_instrument);
                TrackSignalPath::Combined
            } else {
                TrackSignalPath::Separated {
                    source_bus: create_track_source_bus(context),
                    strip: create_track_strip(commands, level.clone(), meter.clone(), route_bus),
                }
            }
        } else {
            TrackSignalPath::Separated {
                source_bus: create_track_source_bus(context),
                strip: create_track_strip(commands, level.clone(), meter.clone(), route_bus),
            }
        };

        let mut runtime = Self {
            effects: Vec::new(),
            meter,
            level,
            route_bus,
            instrument,
            send_nodes: Vec::new(),
            signal_path,
        };
        if !matches!(runtime.signal_path, TrackSignalPath::Combined) {
            runtime.rebuild_effects(context, commands, track);
            runtime.sync_source(
                context,
                commands,
                track,
                build.soundfont_resources,
                build.soundfonts,
                build.soundfont_settings,
            )?;
        }
        runtime.sync_routing(
            context,
            commands,
            build.mixer,
            build.master_input,
            build.bus_inputs,
            track,
        )?;
        Ok(runtime)
    }

    fn sync_source(
        &mut self,
        context: &KnystContext,
        _commands: &mut MultiThreadedKnystCommands,
        track: &Track,
        soundfont_resources: &[SoundfontResource],
        soundfonts: &HashMap<String, LoadedSoundfont>,
        soundfont_settings: SoundfontSynthSettings,
    ) -> Result<bool, MixerRuntimeError> {
        if matches!(self.signal_path, TrackSignalPath::Combined) {
            if let Some(instrument) = self.instrument.as_mut()
                && instrument.update_in_place(track)?
            {
                return Ok(true);
            }
            context.with_activation(|| {
                if let Some(instrument) = self.instrument.take() {
                    instrument.free();
                }
            });
            let Some(instrument) = create_track_instrument(
                context,
                track,
                soundfont_resources,
                soundfonts,
                soundfont_settings,
                Some((self.level.clone(), self.meter.clone())),
            )?
            else {
                return Ok(false);
            };
            context.with_activation(|| {
                instrument.connect(node_id_of(self.route_bus));
            });
            self.instrument = Some(instrument);
            return Ok(false);
        }

        if let Some(instrument) = self.instrument.as_mut()
            && instrument.update_in_place(track)?
        {
            return Ok(true);
        }
        context.with_activation(|| {
            if let Some(instrument) = self.instrument.take() {
                instrument.free();
            }
            knyst_commands().disconnect(Connection::clear_from_nodes(self.pre_send_source_node()));
        });

        let Some(instrument) = create_track_instrument(
            context,
            track,
            soundfont_resources,
            soundfonts,
            soundfont_settings,
            None,
        )?
        else {
            return Ok(false);
        };
        context.with_activation(|| {
            instrument.connect(self.pre_send_source_node());
        });
        self.instrument = Some(instrument);
        Ok(false)
    }

    fn matches_signal_path(&self, track: &Track) -> bool {
        matches!(self.signal_path, TrackSignalPath::Combined)
            == track_prefers_combined_signal_path(track)
    }

    fn rebuild_effects(
        &mut self,
        context: &KnystContext,
        _commands: &mut MultiThreadedKnystCommands,
        track: &Track,
    ) -> bool {
        if matches!(self.signal_path, TrackSignalPath::Combined) {
            return track_prefers_combined_signal_path(track);
        }
        context.with_activation(|| {
            knyst_commands().disconnect(Connection::clear_to_nodes(self.pre_send_source_node()));
            for effect in self.effects.drain(..).flatten() {
                free_effect(effect);
            }
            let mut previous = self.pre_send_source_node();
            for effect_slot in track.effect_states() {
                let effect = create_effect_runtime(effect_slot);
                if let Some(effect) = effect.as_ref().filter(|_| !effect_slot.bypassed) {
                    let node = effect.node_id();
                    connect_stereo(previous, node);
                    previous = node;
                }
                self.effects.push(effect);
            }
            connect_stereo(previous, self.post_send_source_node());
        });
        true
    }

    fn apply_strip(&mut self, track: &Track, gain: f32) {
        self.level.set(gain, track.state.pan);
    }

    fn sync_routing(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        master_input: NodeId,
        bus_inputs: &HashMap<BusId, NodeId>,
        track: &Track,
    ) -> Result<(), MixerRuntimeError> {
        context.with_activation(|| {
            self.sync_routing_existing(commands, mixer, master_input, bus_inputs, track)
        })?;
        self.rebuild_sends(
            context,
            commands,
            bus_inputs,
            &track.routing.sends,
            self.pre_send_source_node(),
            self.post_send_source_node(),
        );
        Ok(())
    }

    fn sync_routing_existing(
        &mut self,
        _commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        master_input: NodeId,
        bus_inputs: &HashMap<BusId, NodeId>,
        track: &Track,
    ) -> Result<(), MixerRuntimeError> {
        let destination = destination_node(track.routing.main, master_input, bus_inputs, mixer)?;
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.route_bus)));
        connect_stereo(node_id_of(self.route_bus), destination);
        Ok(())
    }

    fn rebuild_sends(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        bus_inputs: &HashMap<BusId, NodeId>,
        sends: &[BusSend],
        pre_source: NodeId,
        post_source: NodeId,
    ) {
        for node in self.send_nodes.drain(..) {
            knyst_commands().disconnect(Connection::clear_from_nodes(node));
            knyst_commands().disconnect(Connection::clear_to_nodes(node));
            knyst_commands().free_node(node);
        }

        context.with_activation(|| {
            for send in sends {
                if !send.enabled {
                    continue;
                }
                let Some(destination) = bus_inputs.get(&send.bus_id).copied() else {
                    continue;
                };
                let gain = handle_with_inputs(
                    commands,
                    StereoGain::new(),
                    inputs!((2 : db_to_amplitude(send.gain_db))),
                );
                let gain_node = node_id_of(gain);
                connect_stereo(
                    if send.pre_fader {
                        pre_source
                    } else {
                        post_source
                    },
                    gain_node,
                );
                connect_stereo(gain_node, destination);
                self.send_nodes.push(gain_node);
            }
        });
    }

    fn free(self) {
        if let Some(instrument) = self.instrument {
            instrument.free();
        }
        for effect in self.effects.into_iter().flatten() {
            free_effect(effect);
        }
        for node in self.send_nodes {
            knyst_commands().disconnect(Connection::clear_from_nodes(node));
            knyst_commands().disconnect(Connection::clear_to_nodes(node));
            knyst_commands().free_node(node);
        }
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.route_bus)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.route_bus)));
        self.signal_path.free();
        knyst_commands().free_node(node_id_of(self.route_bus));
    }
}

enum TrackSignalPath {
    Separated {
        source_bus: Handle<GenericHandle>,
        strip: Handle<GenericHandle>,
    },
    Combined,
}

impl TrackSignalPath {
    fn free(self) {
        match self {
            Self::Separated { source_bus, strip } => {
                knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(strip)));
                knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(strip)));
                knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(source_bus)));
                knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(source_bus)));
                knyst_commands().free_node(node_id_of(strip));
                knyst_commands().free_node(node_id_of(source_bus));
            }
            Self::Combined => {}
        }
    }
}

struct TrackInstrumentRuntime {
    handle: InstrumentRuntimeHandle,
    binding: Box<dyn RuntimeBinding>,
}

impl TrackInstrumentRuntime {
    fn update_in_place(&mut self, track: &Track) -> Result<bool, MixerRuntimeError> {
        let Some(slot) = track.instrument_slot() else {
            return Ok(false);
        };
        Ok(self.binding.update_in_place(slot)?)
    }

    fn connect(&self, destination: NodeId) {
        connect_stereo(self.handle.node_id(), destination);
    }

    fn free(self) {
        let node = self.handle.node_id();
        knyst_commands().disconnect(Connection::clear_from_nodes(node));
        knyst_commands().disconnect(Connection::clear_to_nodes(node));
        knyst_commands().free_node(node);
    }

    fn controller(&self) -> Box<dyn Controller> {
        self.binding.controller()
    }
}

struct EffectRuntime {
    handle: EffectRuntimeHandle,
    binding: Option<Box<dyn RuntimeBinding>>,
}

impl EffectRuntime {
    fn node_id(&self) -> NodeId {
        self.handle.node_id()
    }

    fn controller(&self) -> Option<Box<dyn Controller>> {
        self.binding.as_ref().map(|binding| binding.controller())
    }
}

fn create_effect_runtime(effect: &SlotState) -> Option<EffectRuntime> {
    let spec = build_effect_runtime_spec(effect).ok()??;
    let node = handle(EffectProcessorNode::new(spec.processor));
    Some(EffectRuntime {
        handle: EffectRuntimeHandle::new(node),
        binding: spec.binding,
    })
}

fn free_effect(effect: EffectRuntime) {
    let node = effect.node_id();
    knyst_commands().disconnect(Connection::clear_from_nodes(node));
    knyst_commands().disconnect(Connection::clear_to_nodes(node));
    knyst_commands().free_node(node);
}

struct BusRuntime {
    input: Handle<GenericHandle>,
    effects: Vec<Option<EffectRuntime>>,
    strip: Handle<GenericHandle>,
    meter: SharedStripMeter,
    level: SharedStripLevel,
    route_bus: Handle<GenericHandle>,
    send_nodes: Vec<NodeId>,
}

impl BusRuntime {
    fn new(
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        settings: &AudioEngineSettings,
        bus_track: &Track,
        _mixer: &MixerState,
    ) -> Self {
        let initial_gain = bus_effective_amplitude(bus_track);
        let meter = SharedStripMeter::new(settings.sample_rate, settings.block_size);
        let level = SharedStripLevel::new(initial_gain, bus_track.state.pan);
        let strip = handle_with_inputs(
            commands,
            StereoBalanceMeter::new(level.clone(), meter.clone()),
            inputs!(),
        );
        let (input, route_bus) = context.with_activation(|| {
            let input = bus(2);
            let route_bus = bus(2);
            connect_stereo(node_id_of(strip), node_id_of(route_bus));
            (input, route_bus)
        });
        let mut runtime = Self {
            input,
            effects: Vec::new(),
            strip,
            meter,
            level,
            route_bus,
            send_nodes: Vec::new(),
        };
        runtime.rebuild_effects(context, commands, bus_track);
        runtime
    }

    fn input_node(&self) -> NodeId {
        node_id_of(self.input)
    }

    fn apply_strip(&mut self, bus_track: &Track, gain: f32) {
        self.level.set(gain, bus_track.state.pan);
    }

    fn rebuild_effects(
        &mut self,
        context: &KnystContext,
        _commands: &mut MultiThreadedKnystCommands,
        bus_track: &Track,
    ) {
        context.with_activation(|| {
            knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.input)));
            for effect in self.effects.drain(..).flatten() {
                free_effect(effect);
            }
            let mut previous = node_id_of(self.input);
            for effect_slot in bus_track.effect_states() {
                let effect = create_effect_runtime(effect_slot);
                if let Some(effect) = effect.as_ref().filter(|_| !effect_slot.bypassed) {
                    let node = effect.node_id();
                    connect_stereo(previous, node);
                    previous = node;
                }
                self.effects.push(effect);
            }
            connect_stereo(previous, node_id_of(self.strip));
        });
    }

    fn sync_routing(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        master_input: NodeId,
        bus_inputs: &HashMap<BusId, NodeId>,
        bus_track: &Track,
    ) -> Result<(), MixerRuntimeError> {
        context.with_activation(|| {
            self.sync_routing_existing(commands, mixer, master_input, bus_inputs, bus_track)
        })?;
        self.rebuild_sends(
            context,
            commands,
            bus_inputs,
            &bus_track.routing.sends,
            node_id_of(self.input),
            node_id_of(self.strip),
        );
        Ok(())
    }

    fn sync_routing_existing(
        &mut self,
        _commands: &mut MultiThreadedKnystCommands,
        mixer: &MixerState,
        master_input: NodeId,
        bus_inputs: &HashMap<BusId, NodeId>,
        bus_track: &Track,
    ) -> Result<(), MixerRuntimeError> {
        let destination =
            destination_node(bus_track.routing.main, master_input, bus_inputs, mixer)?;
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.route_bus)));
        connect_stereo(node_id_of(self.route_bus), destination);
        Ok(())
    }

    fn rebuild_sends(
        &mut self,
        context: &KnystContext,
        commands: &mut MultiThreadedKnystCommands,
        bus_inputs: &HashMap<BusId, NodeId>,
        sends: &[BusSend],
        pre_source: NodeId,
        post_source: NodeId,
    ) {
        for node in self.send_nodes.drain(..) {
            knyst_commands().disconnect(Connection::clear_from_nodes(node));
            knyst_commands().disconnect(Connection::clear_to_nodes(node));
            knyst_commands().free_node(node);
        }

        context.with_activation(|| {
            for send in sends {
                if !send.enabled {
                    continue;
                }
                let Some(destination) = bus_inputs.get(&send.bus_id).copied() else {
                    continue;
                };
                let gain = handle_with_inputs(
                    commands,
                    StereoGain::new(),
                    inputs!((2 : db_to_amplitude(send.gain_db))),
                );
                let gain_node = node_id_of(gain);
                connect_stereo(
                    if send.pre_fader {
                        pre_source
                    } else {
                        post_source
                    },
                    gain_node,
                );
                connect_stereo(gain_node, destination);
                self.send_nodes.push(gain_node);
            }
        });
    }

    fn free(self) {
        for effect in self.effects.into_iter().flatten() {
            free_effect(effect);
        }
        for node in self.send_nodes {
            knyst_commands().disconnect(Connection::clear_from_nodes(node));
            knyst_commands().disconnect(Connection::clear_to_nodes(node));
            knyst_commands().free_node(node);
        }
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.strip)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.strip)));
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.route_bus)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.route_bus)));
        knyst_commands().disconnect(Connection::clear_from_nodes(node_id_of(self.input)));
        knyst_commands().disconnect(Connection::clear_to_nodes(node_id_of(self.input)));
        knyst_commands().free_node(node_id_of(self.strip));
        knyst_commands().free_node(node_id_of(self.route_bus));
        knyst_commands().free_node(node_id_of(self.input));
    }
}

fn create_track_source_bus(context: &KnystContext) -> Handle<GenericHandle> {
    context.with_activation(|| bus(2))
}

fn create_track_strip(
    commands: &mut MultiThreadedKnystCommands,
    level: SharedStripLevel,
    meter: SharedStripMeter,
    route_bus: Handle<GenericHandle>,
) -> Handle<GenericHandle> {
    let strip = handle_with_inputs(commands, StereoBalanceMeter::new(level, meter), inputs!());
    connect_stereo(node_id_of(strip), node_id_of(route_bus));
    strip
}

fn create_track_instrument(
    context: &KnystContext,
    track: &Track,
    soundfont_resources: &[SoundfontResource],
    soundfonts: &HashMap<String, LoadedSoundfont>,
    soundfont_settings: SoundfontSynthSettings,
    inline_strip: Option<(SharedStripLevel, SharedStripMeter)>,
) -> Result<Option<TrackInstrumentRuntime>, MixerRuntimeError> {
    let Some(slot) = track.instrument_slot() else {
        return Ok(None);
    };
    let Some(spec) = build_instrument_runtime_spec(
        slot,
        &InstrumentRuntimeContext {
            soundfonts,
            soundfont_resources,
            soundfont_settings,
        },
    )?
    else {
        return Ok(None);
    };
    let crate::instrument::InstrumentRuntimeSpec { processor, binding } = spec;
    let node = context.with_activation(|| {
        let reset_state = SharedInstrumentResetState::default();
        let node = if let Some((level, meter)) = inline_strip {
            handle(TrackInstrumentStripNode::new(
                processor,
                reset_state.clone(),
                level,
                meter,
            ))
        } else {
            handle(InstrumentProcessorNode::new(processor, reset_state.clone()))
        };
        InstrumentRuntimeHandle::new(node, reset_state)
    });
    Ok(Some(TrackInstrumentRuntime {
        handle: node,
        binding,
    }))
}

fn destination_node(
    route: TrackRoute,
    master_input: NodeId,
    bus_inputs: &HashMap<BusId, NodeId>,
    mixer: &MixerState,
) -> Result<NodeId, MixerRuntimeError> {
    match route {
        TrackRoute::Master => Ok(master_input),
        TrackRoute::Bus(bus_id) => {
            mixer.bus(bus_id)?;
            bus_inputs
                .get(&bus_id)
                .copied()
                .ok_or(MixerError::InvalidBusId(bus_id).into())
        }
    }
}

fn connect_stereo(source: NodeId, destination: NodeId) {
    knyst_commands().connect(source.to(destination).from_index(0).to_index(0));
    knyst_commands().connect(source.to(destination).from_index(1).to_index(1));
}

fn handle_with_inputs(
    commands: &mut MultiThreadedKnystCommands,
    processor: impl GenOrGraph,
    inputs: impl Into<InputBundle>,
) -> Handle<GenericHandle> {
    let num_inputs = processor.num_inputs();
    let num_outputs = processor.num_outputs();
    let node_id = commands.push(processor, inputs);
    Handle::new(GenericHandle::new(node_id, num_inputs, num_outputs))
}

fn node_id_of<H: HandleData + Copy>(handle: Handle<H>) -> NodeId {
    handle
        .node_ids()
        .next()
        .unwrap_or_else(|| NodeId::new(u64::MAX))
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct MeterTap {
    meter: SharedStripMeter,
}

#[cfg(test)]
#[impl_gen]
impl MeterTap {
    #[new]
    fn new(meter: SharedStripMeter) -> Self {
        Self { meter }
    }

    #[process]
    fn process(
        &mut self,
        left_in: &[Sample],
        right_in: &[Sample],
        left_out: &mut [Sample],
        right_out: &mut [Sample],
        block_size: BlockSize,
    ) -> GenState {
        let mut peak_left = 0.0_f32;
        let mut peak_right = 0.0_f32;

        for frame in 0..block_size.0 {
            let left = left_in[frame];
            let right = right_in[frame];
            left_out[frame] = left;
            right_out[frame] = right;
            peak_left = peak_left.max(left.abs());
            peak_right = peak_right.max(right.abs());
        }

        self.meter.observe_stereo(peak_left, peak_right);
        GenState::Continue
    }
}

fn track_effective_amplitude(mixer: &MixerState, track: &Track) -> f32 {
    let any_solo = mixer.tracks().iter().any(|track| track.state.soloed)
        || mixer.buses().iter().any(|bus| bus.state.soloed);
    let routed_to_soloed_bus = route_bus_id(track.routing.main)
        .is_some_and(|bus_id| mixer.bus(bus_id).is_ok_and(|bus| bus.state.soloed))
        || track
            .routing
            .sends
            .iter()
            .any(|send| mixer.bus(send.bus_id).is_ok_and(|bus| bus.state.soloed));
    if track.state.muted || (any_solo && !track.state.soloed && !routed_to_soloed_bus) {
        0.0
    } else {
        db_to_amplitude(track.state.gain_db)
    }
}

fn bus_effective_amplitude(bus: &Track) -> f32 {
    if bus.state.muted {
        0.0
    } else {
        db_to_amplitude(bus.state.gain_db)
    }
}

fn route_bus_id(route: TrackRoute) -> Option<BusId> {
    match route {
        TrackRoute::Master => None,
        TrackRoute::Bus(bus_id) => Some(bus_id),
    }
}

fn track_needs_runtime(track: &Track) -> bool {
    !track
        .instrument_slot()
        .is_some_and(|slot| registry::is_empty(&slot.kind))
        || track.effect_count() > 0
}

fn track_prefers_combined_signal_path(track: &Track) -> bool {
    track.effect_count() == 0 && track.routing.sends.is_empty()
}

fn db_to_amplitude(db: f32) -> f32 {
    if db <= -60.0 {
        0.0
    } else {
        knyst::db_to_amplitude(db)
    }
}

fn process_stereo_balance_meter_scalar(
    left_in: &[Sample],
    right_in: &[Sample],
    left_out: &mut [Sample],
    right_out: &mut [Sample],
    left_mul: f32,
    right_mul: f32,
    frames: usize,
) -> (f32, f32) {
    let mut peak_left = 0.0_f32;
    let mut peak_right = 0.0_f32;
    for frame in 0..frames {
        let left = left_in[frame] * left_mul;
        let right = right_in[frame] * right_mul;
        left_out[frame] = left;
        right_out[frame] = right;
        peak_left = peak_left.max(left.abs());
        peak_right = peak_right.max(right.abs());
    }
    (peak_left, peak_right)
}

fn process_stereo_balance_meter_simd(
    left_in: &[Sample],
    right_in: &[Sample],
    left_out: &mut [Sample],
    right_out: &mut [Sample],
    left_mul: f32,
    right_mul: f32,
    frames: usize,
) -> (f32, f32) {
    let simd_width = 4;
    let left_mul4 = f32x4::splat(left_mul);
    let right_mul4 = f32x4::splat(right_mul);
    let mut peak_left4 = f32x4::splat(0.0);
    let mut peak_right4 = f32x4::splat(0.0);

    let simd_frames = frames / simd_width * simd_width;
    for frame in (0..simd_frames).step_by(simd_width) {
        let left = f32x4::from([
            left_in[frame],
            left_in[frame + 1],
            left_in[frame + 2],
            left_in[frame + 3],
        ]) * left_mul4;
        let right = f32x4::from([
            right_in[frame],
            right_in[frame + 1],
            right_in[frame + 2],
            right_in[frame + 3],
        ]) * right_mul4;

        let left_arr = left.to_array();
        let right_arr = right.to_array();
        left_out[frame..frame + simd_width].copy_from_slice(&left_arr);
        right_out[frame..frame + simd_width].copy_from_slice(&right_arr);

        peak_left4 = peak_left4.max(left.abs());
        peak_right4 = peak_right4.max(right.abs());
    }

    let left_peak_arr = peak_left4.to_array();
    let right_peak_arr = peak_right4.to_array();
    let mut peak_left = left_peak_arr.into_iter().fold(0.0_f32, f32::max);
    let mut peak_right = right_peak_arr.into_iter().fold(0.0_f32, f32::max);

    if simd_frames < frames {
        let (tail_left, tail_right) = process_stereo_balance_meter_scalar(
            &left_in[simd_frames..frames],
            &right_in[simd_frames..frames],
            &mut left_out[simd_frames..frames],
            &mut right_out[simd_frames..frames],
            left_mul,
            right_mul,
            frames - simd_frames,
        );
        peak_left = peak_left.max(tail_left);
        peak_right = peak_right.max(tail_right);
    }

    (peak_left, peak_right)
}

pub(super) struct StereoGain;

#[impl_gen]
impl StereoGain {
    #[new]
    fn new() -> Self {
        Self
    }

    #[process]
    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        left_in: &[Sample],
        right_in: &[Sample],
        gain: &[Sample],
        left_out: &mut [Sample],
        right_out: &mut [Sample],
        block_size: BlockSize,
    ) -> GenState {
        for frame in 0..block_size.0 {
            let gain = gain[frame];
            left_out[frame] = left_in[frame] * gain;
            right_out[frame] = right_in[frame] * gain;
        }
        GenState::Continue
    }
}

#[cfg(test)]
pub(super) struct StereoBalanceGain {
    level: SharedStripLevel,
}

#[cfg(test)]
#[impl_gen]
impl StereoBalanceGain {
    #[new]
    fn new(level: SharedStripLevel) -> Self {
        Self { level }
    }

    #[process]
    fn process(
        &mut self,
        left_in: &[Sample],
        right_in: &[Sample],
        left_out: &mut [Sample],
        right_out: &mut [Sample],
        block_size: BlockSize,
    ) -> GenState {
        let gain = self.level.gain();
        let pan = self.level.pan().clamp(-1.0, 1.0);
        let left_gain = if pan > 0.0 { 1.0 - pan } else { 1.0 };
        let right_gain = if pan < 0.0 { 1.0 + pan } else { 1.0 };
        for frame in 0..block_size.0 {
            left_out[frame] = left_in[frame] * gain * left_gain;
            right_out[frame] = right_in[frame] * gain * right_gain;
        }
        GenState::Continue
    }
}

#[derive(Debug, Clone)]
struct StereoBalanceMeter {
    level: SharedStripLevel,
    meter: SharedStripMeter,
}

#[impl_gen]
impl StereoBalanceMeter {
    #[new]
    fn new(level: SharedStripLevel, meter: SharedStripMeter) -> Self {
        Self { level, meter }
    }

    #[process]
    fn process(
        &mut self,
        left_in: &[Sample],
        right_in: &[Sample],
        left_out: &mut [Sample],
        right_out: &mut [Sample],
        block_size: BlockSize,
    ) -> GenState {
        let gain = self.level.gain();
        let pan = self.level.pan().clamp(-1.0, 1.0);
        let left_gain = if pan > 0.0 { 1.0 - pan } else { 1.0 };
        let right_gain = if pan < 0.0 { 1.0 + pan } else { 1.0 };
        let left_mul = gain * left_gain;
        let right_mul = gain * right_gain;
        let (peak_left, peak_right) = process_stereo_balance_meter_simd(
            left_in,
            right_in,
            left_out,
            right_out,
            left_mul,
            right_mul,
            block_size.0,
        );

        self.meter.observe_stereo(peak_left, peak_right);
        GenState::Continue
    }
}

struct TrackInstrumentStripNode {
    active_generation: u32,
    reset_state: SharedInstrumentResetState,
    processor: Box<dyn InstrumentProcessor>,
    scratch_left: Vec<Sample>,
    scratch_right: Vec<Sample>,
    level: SharedStripLevel,
    meter: SharedStripMeter,
}

impl TrackInstrumentStripNode {
    fn new(
        processor: Box<dyn InstrumentProcessor>,
        reset_state: SharedInstrumentResetState,
        level: SharedStripLevel,
        meter: SharedStripMeter,
    ) -> Self {
        Self {
            active_generation: 0,
            reset_state,
            processor,
            scratch_left: Vec::new(),
            scratch_right: Vec::new(),
            level,
            meter,
        }
    }
}

impl knyst::r#gen::Gen for TrackInstrumentStripNode {
    fn process(
        &mut self,
        ctx: knyst::r#gen::GenContext<'_, '_, '_>,
        _resources: &mut knyst::Resources,
    ) -> GenState {
        let frames = ctx.outputs.block_size();
        self.scratch_left.resize(frames, 0.0);
        self.scratch_right.resize(frames, 0.0);

        let requested_reset = self.reset_state.load();
        if requested_reset != self.active_generation
            && generation_is_current_or_newer(requested_reset, self.active_generation)
        {
            self.active_generation = requested_reset;
            self.processor.reset();
        }

        for event in ctx.events {
            if event.input != 0 {
                continue;
            }
            let knyst::graph::EventPayload::Bytes(bytes) = &event.payload else {
                continue;
            };
            let Some(event) = decode_instrument_event(bytes) else {
                continue;
            };
            match event {
                ScheduledInstrumentEvent::Reset { generation } => {
                    if generation_is_current_or_newer(generation, self.active_generation) {
                        self.active_generation = generation;
                        self.processor.reset();
                    }
                }
                ScheduledInstrumentEvent::Midi { generation, event } => {
                    if generation == self.active_generation {
                        self.processor.handle_midi(event);
                    }
                }
            }
        }

        self.processor.render(
            &mut self.scratch_left[..frames],
            &mut self.scratch_right[..frames],
        );

        let gain = self.level.gain();
        let pan = self.level.pan().clamp(-1.0, 1.0);
        let left_gain = if pan > 0.0 { 1.0 - pan } else { 1.0 };
        let right_gain = if pan < 0.0 { 1.0 + pan } else { 1.0 };
        let left_mul = gain * left_gain;
        let right_mul = gain * right_gain;

        let mut outputs = ctx.outputs.iter_mut();
        let Some(left_out) = outputs.next() else {
            return GenState::Continue;
        };
        let Some(right_out) = outputs.next() else {
            return GenState::Continue;
        };

        let (peak_left, peak_right) = process_stereo_balance_meter_simd(
            &self.scratch_left[..frames],
            &self.scratch_right[..frames],
            left_out,
            right_out,
            left_mul,
            right_mul,
            frames,
        );
        self.meter.observe_stereo(peak_left, peak_right);

        if self.processor.is_sleeping() {
            GenState::Sleep
        } else {
            GenState::Continue
        }
    }

    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn num_event_inputs(&self) -> usize {
        1
    }

    fn event_input_desc(&self, input: usize) -> &'static str {
        match input {
            0 => "event",
            _ => "",
        }
    }

    fn name(&self) -> &'static str {
        "TrackInstrumentStripNode"
    }
}

#[cfg(test)]
mod tests {
    use knyst::controller::KnystCommands;
    use knyst::inputs;
    use knyst::modal_interface::knyst_commands;
    use knyst::prelude::{
        BlockSize, GenState, InputBundle, Sample, bus, graph_output, handle, impl_gen,
    };
    use wide::f32x4;

    use super::{
        InstrumentProcessorNode, InstrumentRuntimeHandle, MasterRuntime, MeterTap,
        MixerRuntimeError, RuntimeFactoryError, SharedInstrumentResetState, SharedStripMeter,
        connect_stereo, db_to_amplitude, node_id_of, normalize_meter_level,
    };
    use crate::instrument::registry::Entry;
    use crate::instrument::{
        BUILTIN_SOUNDFONT_ID, Controller, ControllerError, InstrumentProcessor,
        InstrumentRuntimeContext, InstrumentRuntimeSpec, MidiEvent, Processor, ProcessorDescriptor,
        ProcessorKind, ProcessorState, ProcessorStateError, RuntimeBinding, SlotState, registry,
    };
    use crate::mixer::{BusSend, Mixer, MixerState, TrackId};
    use crate::test_utils::{OfflineHarness, test_soundfont_resource};
    use knyst::time::Beats;

    fn schedule_test_note(harness: &mut OfflineHarness, handle: InstrumentRuntimeHandle) {
        let scheduled_at = harness
            .commands()
            .current_transport_snapshot()
            .and_then(|snapshot| snapshot.beats)
            .unwrap_or(Beats::ZERO)
            + Beats::from_beats_f64(0.01);
        handle.schedule_midi_at_with_offset(
            harness.commands(),
            scheduled_at,
            0,
            MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
        );
    }

    fn unity_strip_gain() -> super::SharedStripLevel {
        super::SharedStripLevel::new(1.0, 0.0)
    }

    fn soundfont_slot(program: u8) -> SlotState {
        register_test_soundfont_builtin();
        SlotState::built_in(BUILTIN_SOUNDFONT_ID, ProcessorState(vec![program]))
    }

    fn register_test_soundfont_builtin() {
        registry::register([Entry::builtin_instrument(
            BUILTIN_SOUNDFONT_ID,
            "SoundFont",
            test_instrument_descriptor(),
            create_test_soundfont_runtime,
        )]);
    }

    fn test_instrument_descriptor() -> &'static ProcessorDescriptor {
        static DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
            name: "Test Instrument",
            params: &[],
            editor: None,
        };
        &DESCRIPTOR
    }

    fn create_test_soundfont_runtime(
        slot: &SlotState,
        context: &InstrumentRuntimeContext<'_>,
    ) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
        if !matches!(
            slot.kind,
            ProcessorKind::BuiltIn { ref processor_id } if processor_id == BUILTIN_SOUNDFONT_ID
        ) || context.soundfonts.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(InstrumentRuntimeSpec {
            processor: Box::<TestInstrumentProcessor>::default(),
            binding: Box::new(TestInstrumentBinding),
        }))
    }

    #[derive(Default)]
    struct TestInstrumentProcessor {
        active: bool,
    }

    impl Processor for TestInstrumentProcessor {
        fn descriptor(&self) -> &'static ProcessorDescriptor {
            test_instrument_descriptor()
        }

        fn set_param(&mut self, id: &str, normalized: f32) -> bool {
            id.is_empty() && normalized == 0.0
        }

        fn get_param(&self, id: &str) -> Option<f32> {
            (id.is_empty()).then_some(0.0)
        }

        fn save_state(&self) -> ProcessorState {
            ProcessorState::default()
        }

        fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
            if state.0.is_empty() {
                Ok(())
            } else {
                Err(ProcessorStateError::Decode(
                    "test instrument state must be empty".to_string(),
                ))
            }
        }

        fn reset(&mut self) {
            self.active = false;
        }
    }

    impl InstrumentProcessor for TestInstrumentProcessor {
        fn handle_midi(&mut self, event: MidiEvent) {
            match event {
                MidiEvent::NoteOn { velocity, .. } if velocity > 0 => self.active = true,
                MidiEvent::NoteOff { .. }
                | MidiEvent::AllNotesOff { .. }
                | MidiEvent::AllSoundOff { .. } => self.active = false,
                _ => {}
            }
        }

        fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
            let value = if self.active { 0.25 } else { 0.0 };
            left.fill(value);
            right.fill(value);
        }

        fn is_sleeping(&self) -> bool {
            !self.active
        }
    }

    struct TestInstrumentBinding;

    impl RuntimeBinding for TestInstrumentBinding {
        fn controller(&self) -> Box<dyn Controller> {
            Box::new(TestInstrumentController)
        }

        fn update_in_place(&self, slot: &SlotState) -> Result<bool, ProcessorStateError> {
            Ok(matches!(
                slot.kind,
                ProcessorKind::BuiltIn { ref processor_id } if processor_id == BUILTIN_SOUNDFONT_ID
            ))
        }
    }

    struct TestInstrumentController;

    impl Controller for TestInstrumentController {
        fn descriptor(&self) -> &'static ProcessorDescriptor {
            test_instrument_descriptor()
        }

        fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
            Err(ControllerError::UnknownParameter(id.to_string()))
        }

        fn set_param(&self, id: &str, normalized: f32) -> Result<(), ControllerError> {
            if id.is_empty() && normalized == 0.0 {
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
                    "test instrument state must be empty".to_string(),
                ))
            }
        }
    }

    #[test]
    fn strip_meter_captures_stereo_peak_and_hold() {
        let meter = SharedStripMeter::default();

        meter.observe_stereo(0.25, 0.5);
        let snapshot = meter.snapshot();

        assert!(snapshot.left.level > 0.0);
        assert!(snapshot.right.level > snapshot.left.level);
        assert_eq!(snapshot.left.hold, snapshot.left.level);
        assert_eq!(snapshot.right.hold, snapshot.right.level);
        assert!(!snapshot.clip_latched);
    }

    #[test]
    fn strip_meter_hold_and_clip_stick_until_reset() {
        let meter = SharedStripMeter::default();

        meter.observe_stereo(1.1, 0.2);
        let hot = meter.snapshot();
        meter.observe_stereo(0.1, 0.05);
        let cooled = meter.snapshot();

        assert!(hot.clip_latched);
        assert!(cooled.clip_latched);
        assert_eq!(cooled.left.hold, hot.left.hold);

        meter.reset();
        let reset = meter.snapshot();
        assert!(!reset.clip_latched);
        assert_eq!(reset.left.hold, 0.0);
        assert_eq!(reset.right.hold, 0.0);
    }

    #[test]
    fn strip_meter_release_is_ballistic_not_instant() {
        let meter = SharedStripMeter::default();

        meter.observe_stereo(1.0, 0.5);
        let hot = meter.snapshot();
        meter.observe_stereo(0.05, 0.025);
        let falling = meter.snapshot();

        assert!(falling.left.level < hot.left.level);
        assert!(falling.left.level > normalize_meter_level(0.05));
        assert!(falling.right.level < hot.right.level);
        assert!(falling.right.level > normalize_meter_level(0.025));
    }

    #[test]
    fn mixer_gain_floor_is_silence() {
        assert_eq!(db_to_amplitude(-60.0), 0.0);
        assert!(db_to_amplitude(-59.5) > 0.0);
    }

    #[test]
    fn strip_meter_release_eventually_reaches_floor() {
        let meter = SharedStripMeter::default();

        meter.observe_stereo(1.0, 1.0);
        for _ in 0..4_000 {
            meter.observe_stereo(0.0, 0.0);
        }
        let cooled = meter.snapshot();

        assert!(cooled.left.level <= 0.001);
        assert!(cooled.right.level <= 0.001);
    }

    #[test]
    fn meter_snapshot_normalizes_db_monotonically() {
        assert!(normalize_meter_level(0.05) < normalize_meter_level(0.5));
        assert!(normalize_meter_level(0.5) < normalize_meter_level(1.0));
    }

    #[test]
    fn strip_level_updates_apply_without_scheduled_parameter_changes() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let level = super::SharedStripLevel::new(1.0, 0.0);
        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(level.clone()),
            inputs!(),
        );
        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            graph_output(0, strip.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(strip));
        });

        harness.process_blocks(8);
        assert!(harness.output_has_signal());

        level.set(0.0, 0.0);
        harness.process_blocks(8);

        assert!(
            !harness.output_has_signal(),
            "strip level updates should affect audio without going through scheduled parameter changes"
        );
    }

    #[test]
    fn simd_strip_pass_matches_scalar_path() {
        let frames = 11;
        let left_in = vec![-0.9, -0.6, -0.3, 0.0, 0.2, 0.4, 0.6, 0.8, -0.7, 0.5, -0.1];
        let right_in = vec![0.7, -0.5, 0.3, -0.1, 0.0, 0.2, -0.4, 0.6, -0.8, 0.9, -0.2];
        let mut scalar_left = vec![0.0; frames];
        let mut scalar_right = vec![0.0; frames];
        let mut repeated_left = vec![0.0; frames];
        let mut repeated_right = vec![0.0; frames];

        let scalar = super::process_stereo_balance_meter_scalar(
            &left_in,
            &right_in,
            &mut scalar_left,
            &mut scalar_right,
            0.75,
            0.25,
            frames,
        );
        let repeated = super::process_stereo_balance_meter_scalar(
            &left_in,
            &right_in,
            &mut repeated_left,
            &mut repeated_right,
            0.75,
            0.25,
            frames,
        );

        for (a, b) in scalar_left.iter().zip(repeated_left.iter()) {
            assert!((a - b).abs() < 1.0e-6);
        }
        for (a, b) in scalar_right.iter().zip(repeated_right.iter()) {
            assert!((a - b).abs() < 1.0e-6);
        }
        assert!((scalar.0 - repeated.0).abs() < 1.0e-6);
        assert!((scalar.1 - repeated.1).abs() < 1.0e-6);
    }

    fn process_stereo_balance_meter_simd_test(
        left_in: &[Sample],
        right_in: &[Sample],
        left_out: &mut [Sample],
        right_out: &mut [Sample],
        left_mul: f32,
        right_mul: f32,
        frames: usize,
    ) -> (f32, f32) {
        let simd_width = 4;
        let left_mul4 = f32x4::splat(left_mul);
        let right_mul4 = f32x4::splat(right_mul);
        let mut peak_left4 = f32x4::splat(0.0);
        let mut peak_right4 = f32x4::splat(0.0);

        let simd_frames = frames / simd_width * simd_width;
        for frame in (0..simd_frames).step_by(simd_width) {
            let left = f32x4::from([
                left_in[frame],
                left_in[frame + 1],
                left_in[frame + 2],
                left_in[frame + 3],
            ]) * left_mul4;
            let right = f32x4::from([
                right_in[frame],
                right_in[frame + 1],
                right_in[frame + 2],
                right_in[frame + 3],
            ]) * right_mul4;

            let left_arr = left.to_array();
            let right_arr = right.to_array();
            left_out[frame..frame + simd_width].copy_from_slice(&left_arr);
            right_out[frame..frame + simd_width].copy_from_slice(&right_arr);

            peak_left4 = peak_left4.max(left.abs());
            peak_right4 = peak_right4.max(right.abs());
        }

        let left_peak_arr = peak_left4.to_array();
        let right_peak_arr = peak_right4.to_array();
        let mut peak_left = left_peak_arr.into_iter().fold(0.0_f32, f32::max);
        let mut peak_right = right_peak_arr.into_iter().fold(0.0_f32, f32::max);

        if simd_frames < frames {
            let (tail_left, tail_right) = super::process_stereo_balance_meter_scalar(
                &left_in[simd_frames..frames],
                &right_in[simd_frames..frames],
                &mut left_out[simd_frames..frames],
                &mut right_out[simd_frames..frames],
                left_mul,
                right_mul,
                frames - simd_frames,
            );
            peak_left = peak_left.max(tail_left);
            peak_right = peak_right.max(tail_right);
        }

        (peak_left, peak_right)
    }

    #[test]
    #[ignore = "manual perf report"]
    fn perf_report_strip_scalar_vs_simd_release_shape() {
        use std::hint::black_box;
        use std::time::Instant;

        const FRAMES: usize = 64;
        const ITERS: usize = 200_000;

        let left_in: Vec<f32> = (0..FRAMES)
            .map(|frame| ((frame as f32 * 0.17).sin() * 0.5) - 0.2)
            .collect();
        let right_in: Vec<f32> = (0..FRAMES)
            .map(|frame| ((frame as f32 * 0.11).cos() * 0.45) + 0.15)
            .collect();
        let mut left_out = vec![0.0; FRAMES];
        let mut right_out = vec![0.0; FRAMES];

        let scalar_started = Instant::now();
        for _ in 0..ITERS {
            black_box(super::process_stereo_balance_meter_scalar(
                black_box(&left_in),
                black_box(&right_in),
                black_box(&mut left_out),
                black_box(&mut right_out),
                black_box(0.82),
                black_box(0.67),
                black_box(FRAMES),
            ));
        }
        let scalar_elapsed = scalar_started.elapsed();

        let simd_started = Instant::now();
        for _ in 0..ITERS {
            black_box(process_stereo_balance_meter_simd_test(
                black_box(&left_in),
                black_box(&right_in),
                black_box(&mut left_out),
                black_box(&mut right_out),
                black_box(0.82),
                black_box(0.67),
                black_box(FRAMES),
            ));
        }
        let simd_elapsed = simd_started.elapsed();

        eprintln!("strip perf over {ITERS} iters: scalar={scalar_elapsed:?} simd={simd_elapsed:?}");
    }

    #[test]
    fn reset_one_strip_meter_does_not_touch_another() {
        let left = SharedStripMeter::default();
        let right = SharedStripMeter::default();

        left.observe_stereo(1.1, 0.7);
        right.observe_stereo(0.9, 0.8);

        left.reset();

        let left_snapshot = left.snapshot();
        let right_snapshot = right.snapshot();

        assert!(!left_snapshot.clip_latched);
        assert_eq!(left_snapshot.left.hold, 0.0);
        assert_eq!(left_snapshot.right.hold, 0.0);

        assert!(!right_snapshot.clip_latched);
        assert!(right_snapshot.left.hold > 0.0);
        assert!(right_snapshot.right.hold > 0.0);
    }

    struct TestSineGen {
        phase: f32,
        phase_increment: f32,
    }

    #[impl_gen]
    impl TestSineGen {
        #[new]
        fn new(sample_rate: f32, frequency: f32) -> Self {
            Self {
                phase: 0.0,
                phase_increment: std::f32::consts::TAU * frequency / sample_rate,
            }
        }

        #[process]
        fn process(
            &mut self,
            left_out: &mut [Sample],
            right_out: &mut [Sample],
            block_size: BlockSize,
        ) -> GenState {
            for frame in 0..block_size.0 {
                let sample = self.phase.sin();
                left_out[frame] = sample;
                right_out[frame] = sample * 0.5;
                self.phase += self.phase_increment;
            }
            GenState::Continue
        }
    }

    fn build_soundfont_mixer(harness: &mut OfflineHarness) -> Result<Mixer, MixerRuntimeError> {
        let mut state = MixerState::new();
        state.set_soundfont(test_soundfont_resource());
        for track_id in 1..crate::mixer::INSTRUMENT_TRACK_COUNT {
            state
                .track_mut(TrackId(track_id as u16))
                .expect("track should exist")
                .set_instrument_slot(SlotState::new(
                    ProcessorKind::Plugin {
                        plugin_id: "none".to_string(),
                    },
                    ProcessorState::default(),
                ));
        }
        state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(0));
        let context = harness.context().clone();
        let settings = harness.settings();
        let mixer = Mixer::new(&context, harness.commands(), &settings, state)?;
        harness.wait_for_graph_settled();
        Ok(mixer)
    }

    #[test]
    fn raw_commands_and_active_context_use_same_graph() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let raw_graph = harness.commands().current_graph();
        let active_graph = harness
            .context()
            .with_activation(|| knyst_commands().current_graph());
        assert_eq!(raw_graph, active_graph);

        let _mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");
        let raw_graph = harness.commands().current_graph();
        let active_graph = harness
            .context()
            .with_activation(|| knyst_commands().current_graph());
        assert_eq!(raw_graph, active_graph);
    }

    #[test]
    fn inspect_mixer_graph() {
        let mut harness = OfflineHarness::new_with_outputs(44_100, 64, 4);
        let mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.context().with_activation(|| {
            graph_output(2, handle.raw_handle().channels(2));
        });
        let inspection = harness.inspection();
        eprintln!("outputs: {}", inspection.num_outputs);
        eprintln!(
            "graph output edges: {:?}",
            inspection.graph_output_input_edges
        );
        for (index, node) in inspection.nodes.iter().enumerate() {
            eprintln!(
                "{index}: {} {:?} inputs={:?} outputs={:?}",
                node.name, node.address, node.input_edges, node.output_channels
            );
        }
    }

    #[test]
    fn track_soundfont_reaches_master_output_with_thread_local_note_on() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.process_blocks(50);
        harness.commands().transport_play();
        harness.wait_for_transport_settled();

        schedule_test_note(&mut harness, handle);

        harness.process_blocks(50);

        assert!(harness.errors().is_empty(), "{:?}", harness.errors());
        assert!(harness.output_has_signal());
    }

    #[test]
    fn combined_track_soundfont_preserves_stereo_output_at_center_pan() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.process_blocks(50);
        harness.commands().transport_play();
        harness.wait_for_transport_settled();

        schedule_test_note(&mut harness, handle);
        harness.process_blocks(50);

        let left_peak = harness
            .output_channel(0)
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max);
        let right_peak = harness
            .output_channel(1)
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max);

        assert!(left_peak > 0.001, "left channel stayed silent");
        assert!(right_peak > 0.001, "right channel stayed silent");
    }

    #[test]
    fn direct_sine_node_to_bus_preserves_expected_samples() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let bus_handle = harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            let bus_handle = bus(2);
            graph_output(0, bus_handle.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(bus_handle));
            bus_handle
        });

        let _ = bus_handle;
        harness.process_block();

        let phase_increment = std::f32::consts::TAU * 440.0 / 44_100.0;
        for frame in 0..8 {
            let expected_left = (phase_increment * frame as f32).sin();
            let expected_right = expected_left * 0.5;
            assert!((harness.output_channel(0)[frame] - expected_left).abs() < 1.0e-5);
            assert!((harness.output_channel(1)[frame] - expected_right).abs() < 1.0e-5);
        }
    }

    #[test]
    fn disabled_track_send_does_not_create_runtime_send_node() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mut state = MixerState::new();
        state.set_soundfont(test_soundfont_resource());
        let bus_id = state.add_bus("Verb");
        let mut send = BusSend::new(bus_id, -6.0, false);
        send.enabled = false;
        state
            .add_track_bus_send(TrackId(0), send)
            .expect("send should be accepted");
        state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(0));
        let context = harness.context().clone();
        let settings = harness.settings();

        let mixer = Mixer::new(&context, harness.commands(), &settings, state)
            .expect("mixer should initialize");

        let track_runtime = mixer.runtime.tracks[0]
            .as_ref()
            .expect("track runtime should exist");
        assert!(track_runtime.send_nodes.is_empty());
    }

    #[test]
    fn pre_fader_track_send_ignores_track_gain() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mut state = MixerState::new();
        state.set_soundfont(test_soundfont_resource());
        let bus_id = state.add_bus("Cue");
        state
            .add_track_bus_send(TrackId(0), BusSend::new(bus_id, 0.0, true))
            .expect("send should be accepted");
        let track = state.track_mut(TrackId(0)).expect("track 0 should exist");
        track.set_instrument_slot(soundfont_slot(0));
        track.state.gain_db = -60.0;
        let context = harness.context().clone();
        let settings = harness.settings();

        let mixer = Mixer::new(&context, harness.commands(), &settings, state)
            .expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();

        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "pre-fader send should still feed its bus when the track fader is closed"
        );
    }

    #[test]
    fn post_fader_track_send_follows_track_gain() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mut state = MixerState::new();
        state.set_soundfont(test_soundfont_resource());
        let bus_id = state.add_bus("Verb");
        state
            .add_track_bus_send(TrackId(0), BusSend::new(bus_id, 0.0, false))
            .expect("send should be accepted");
        let track = state.track_mut(TrackId(0)).expect("track 0 should exist");
        track.set_instrument_slot(soundfont_slot(0));
        track.state.gain_db = -60.0;
        let context = harness.context().clone();
        let settings = harness.settings();

        let mixer = Mixer::new(&context, harness.commands(), &settings, state)
            .expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();

        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            !harness.output_has_signal(),
            "post-fader send should be silent when the track fader is closed"
        );
    }

    #[test]
    fn muted_track_stays_silent() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mut state = MixerState::new();
        state.set_soundfont(test_soundfont_resource());
        let track = state.track_mut(TrackId(0)).expect("track 0 should exist");
        track.set_instrument_slot(soundfont_slot(0));
        track.state.muted = true;
        let context = harness.context().clone();
        let settings = harness.settings();
        let mixer = Mixer::new(&context, harness.commands(), &settings, state)
            .expect("mixer should initialize");
        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();

        schedule_test_note(&mut harness, handle);

        harness.wait_for_transport_settled();

        assert!(!harness.output_has_signal());
    }

    #[test]
    fn live_created_track_runtime_routes_to_master_output() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let settings = harness.settings();
        let mut mixer = Mixer::new(&context, harness.commands(), &settings, MixerState::new())
            .expect("mixer should initialize");

        mixer.state.set_soundfont(test_soundfont_resource());
        mixer
            .runtime
            .sync_soundfonts(&mixer.state)
            .expect("soundfont should sync");
        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");
        mixer
            .runtime
            .sync_track_routing(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track routing should sync");
        harness.wait_for_graph_settled();

        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "live-created track runtime stayed silent"
        );
    }

    #[test]
    fn live_created_track_runtime_routes_to_master_output_without_extra_routing_pass() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let settings = harness.settings();
        let mut mixer = Mixer::new(&context, harness.commands(), &settings, MixerState::new())
            .expect("mixer should initialize");

        mixer.state.set_soundfont(test_soundfont_resource());
        mixer
            .runtime
            .sync_soundfonts(&mixer.state)
            .expect("soundfont should sync");
        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");
        harness.wait_for_graph_settled();

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "live-created track runtime stayed silent without explicit routing sync"
        );
    }

    #[test]
    fn bus_chain_created_after_transport_reset_passes_signal() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(0.0));
        harness.wait_for_transport_settled();

        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            let first_bus = bus(2);
            let second_bus = bus(2);
            graph_output(0, second_bus.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(first_bus));
            connect_stereo(node_id_of(first_bus), node_id_of(second_bus));
        });

        harness.wait_for_transport_settled();

        assert!(
            harness.output_has_signal(),
            "bus chain created after transport reset stayed silent"
        );
    }

    #[test]
    fn bus_chain_created_after_nonzero_transport_seek_passes_signal() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            let first_bus = bus(2);
            let second_bus = bus(2);
            graph_output(0, second_bus.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(first_bus));
            connect_stereo(node_id_of(first_bus), node_id_of(second_bus));
        });

        harness.wait_for_transport_settled();

        assert!(
            harness.output_has_signal(),
            "bus chain created after non-zero transport seek stayed silent"
        );
    }

    #[test]
    fn raw_soundfont_strip_chain_after_nonzero_transport_seek_produces_signal() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        let processor = TestInstrumentProcessor::default();

        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        let handle = harness.context().with_activation(|| {
            let meter = SharedStripMeter::new(44_100, 64);
            let meter_node = handle(MeterTap::new(meter));
            let route_bus = bus(2);
            let reset_state = SharedInstrumentResetState::default();
            let instrument = handle(InstrumentProcessorNode::new(
                Box::new(processor),
                reset_state.clone(),
            ));
            graph_output(0, route_bus.channels(2));
            connect_stereo(node_id_of(instrument), node_id_of(strip));
            connect_stereo(node_id_of(strip), node_id_of(meter_node));
            connect_stereo(node_id_of(meter_node), node_id_of(route_bus));
            InstrumentRuntimeHandle::new(instrument, reset_state)
        });
        harness.wait_for_graph_settled();

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "raw soundfont strip chain after non-zero transport seek stayed silent"
        );
    }

    #[test]
    fn raw_soundfont_source_bus_chain_after_nonzero_transport_seek_produces_signal() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        let processor = TestInstrumentProcessor::default();

        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        let handle = harness.context().with_activation(|| {
            let source_bus = bus(2);
            let meter = SharedStripMeter::new(44_100, 64);
            let meter_node = handle(MeterTap::new(meter));
            let route_bus = bus(2);
            let reset_state = SharedInstrumentResetState::default();
            let instrument = handle(InstrumentProcessorNode::new(
                Box::new(processor),
                reset_state.clone(),
            ));
            graph_output(0, route_bus.channels(2));
            connect_stereo(node_id_of(instrument), node_id_of(source_bus));
            connect_stereo(node_id_of(source_bus), node_id_of(strip));
            connect_stereo(node_id_of(strip), node_id_of(meter_node));
            connect_stereo(node_id_of(meter_node), node_id_of(route_bus));
            InstrumentRuntimeHandle::new(instrument, reset_state)
        });
        harness.wait_for_graph_settled();

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "raw soundfont source-bus chain after non-zero transport seek stayed silent"
        );
    }

    #[test]
    fn raw_soundfont_chain_routed_into_master_after_nonzero_transport_seek_produces_signal() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        let mixer_state = MixerState::new();
        let context = harness.context().clone();
        let settings = harness.settings();
        let master = MasterRuntime::new(&context, harness.commands(), &settings, &mixer_state);

        let processor = TestInstrumentProcessor::default();

        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        let handle = harness.context().with_activation(|| {
            let source_bus = bus(2);
            let meter = SharedStripMeter::new(44_100, 64);
            let meter_node = handle(MeterTap::new(meter));
            let route_bus = bus(2);
            let reset_state = SharedInstrumentResetState::default();
            let instrument = handle(InstrumentProcessorNode::new(
                Box::new(processor),
                reset_state.clone(),
            ));
            connect_stereo(node_id_of(instrument), node_id_of(source_bus));
            connect_stereo(node_id_of(source_bus), node_id_of(strip));
            connect_stereo(node_id_of(strip), node_id_of(meter_node));
            connect_stereo(node_id_of(meter_node), node_id_of(route_bus));
            connect_stereo(node_id_of(route_bus), master.input_node());
            InstrumentRuntimeHandle::new(instrument, reset_state)
        });
        harness.wait_for_graph_settled();

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "raw soundfont chain routed into master after non-zero transport seek stayed silent"
        );
    }

    #[test]
    fn preexisting_raw_soundfont_chain_survives_nonzero_transport_seek() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mixer_state = MixerState::new();
        let context = harness.context().clone();
        let settings = harness.settings();
        let master = MasterRuntime::new(&context, harness.commands(), &settings, &mixer_state);

        let processor = TestInstrumentProcessor::default();

        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        let handle = harness.context().with_activation(|| {
            let source_bus = bus(2);
            let meter = SharedStripMeter::new(44_100, 64);
            let meter_node = handle(MeterTap::new(meter));
            let route_bus = bus(2);
            let reset_state = SharedInstrumentResetState::default();
            let instrument = handle(InstrumentProcessorNode::new(
                Box::new(processor),
                reset_state.clone(),
            ));
            connect_stereo(node_id_of(instrument), node_id_of(source_bus));
            connect_stereo(node_id_of(source_bus), node_id_of(strip));
            connect_stereo(node_id_of(strip), node_id_of(meter_node));
            connect_stereo(node_id_of(meter_node), node_id_of(route_bus));
            connect_stereo(node_id_of(route_bus), master.input_node());
            InstrumentRuntimeHandle::new(instrument, reset_state)
        });
        harness.wait_for_graph_settled();

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "preexisting raw soundfont chain went silent after non-zero transport seek"
        );
    }

    #[test]
    fn preexisting_sine_chain_survives_nonzero_transport_seek() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let mixer_state = MixerState::new();
        let context = harness.context().clone();
        let settings = harness.settings();
        let master = MasterRuntime::new(&context, harness.commands(), &settings, &mixer_state);

        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        harness.context().with_activation(|| {
            let source_bus = bus(2);
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            let meter = SharedStripMeter::new(44_100, 64);
            let meter_node = handle(MeterTap::new(meter));
            let route_bus = bus(2);
            connect_stereo(node_id_of(signal), node_id_of(source_bus));
            connect_stereo(node_id_of(source_bus), node_id_of(strip));
            connect_stereo(node_id_of(strip), node_id_of(meter_node));
            connect_stereo(node_id_of(meter_node), node_id_of(route_bus));
            connect_stereo(node_id_of(route_bus), master.input_node());
        });

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "preexisting sine chain went silent after non-zero transport seek"
        );
    }

    #[test]
    fn preexisting_bus_chain_survives_nonzero_transport_seek() {
        let mut harness = OfflineHarness::new(44_100, 64);
        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            let first_bus = bus(2);
            let second_bus = bus(2);
            graph_output(0, second_bus.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(first_bus));
            connect_stereo(node_id_of(first_bus), node_id_of(second_bus));
        });

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "preexisting bus chain went silent after non-zero transport seek"
        );
    }

    #[test]
    fn preexisting_sine_strip_survives_nonzero_transport_seek() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            graph_output(0, strip.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(strip));
        });

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "preexisting sine strip went silent after non-zero transport seek"
        );
    }

    #[test]
    fn settled_preexisting_sine_strip_survives_nonzero_transport_seek() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let strip = super::handle_with_inputs(
            harness.commands(),
            super::StereoBalanceGain::new(unity_strip_gain()),
            inputs!(),
        );
        harness.context().with_activation(|| {
            let signal = handle(TestSineGen::new(44_100.0, 440.0));
            graph_output(0, strip.channels(2));
            connect_stereo(node_id_of(signal), node_id_of(strip));
        });

        harness.process_blocks(16);
        assert!(
            harness.output_has_signal(),
            "preexisting sine strip should be audible before seek"
        );

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "settled preexisting sine strip went silent after non-zero transport seek"
        );
    }

    #[test]
    fn live_created_track_runtime_after_transport_reset_routes_to_master_output() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let settings = harness.settings();
        let mut mixer = Mixer::new(&context, harness.commands(), &settings, MixerState::new())
            .expect("mixer should initialize");

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(0.0));
        harness.wait_for_transport_settled();

        mixer.state.set_soundfont(test_soundfont_resource());
        mixer
            .runtime
            .sync_soundfonts(&mixer.state)
            .expect("soundfont should sync");
        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");
        mixer
            .runtime
            .sync_track_routing(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track routing should sync");
        harness.wait_for_graph_settled();

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "live-created track runtime after transport reset stayed silent"
        );
    }

    #[test]
    fn live_created_track_runtime_after_transport_reset_routes_without_extra_routing_pass() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let settings = harness.settings();
        let mut mixer = Mixer::new(&context, harness.commands(), &settings, MixerState::new())
            .expect("mixer should initialize");

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(0.0));
        harness.wait_for_transport_settled();

        mixer.state.set_soundfont(test_soundfont_resource());
        mixer
            .runtime
            .sync_soundfonts(&mixer.state)
            .expect("soundfont should sync");
        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");
        harness.wait_for_graph_settled();

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "live-created track runtime after transport reset stayed silent without explicit routing sync"
        );
    }

    #[test]
    fn live_created_track_runtime_after_nonzero_transport_seek_routes_to_master_output() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let settings = harness.settings();
        let mut mixer = Mixer::new(&context, harness.commands(), &settings, MixerState::new())
            .expect("mixer should initialize");

        harness.commands().transport_pause();
        harness
            .commands()
            .transport_seek_to_beats(Beats::from_beats_f64(1.0));
        harness.wait_for_transport_settled();

        mixer.state.set_soundfont(test_soundfont_resource());
        mixer
            .runtime
            .sync_soundfonts(&mixer.state)
            .expect("soundfont should sync");
        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");
        mixer
            .runtime
            .sync_track_routing(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track routing should sync");
        harness.wait_for_graph_settled();

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");

        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "live-created track runtime after non-zero transport seek stayed silent"
        );
    }

    #[test]
    fn same_soundfont_program_change_keeps_existing_instrument_node() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let mut mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");

        let original_node = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist")
            .node_id();

        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");

        let updated_node = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should still exist")
            .node_id();

        assert_eq!(
            updated_node, original_node,
            "same-soundfont program changes should keep the existing instrument node alive"
        );
    }

    #[test]
    fn same_soundfont_program_change_stays_audible_without_routing_resync() {
        let mut harness = OfflineHarness::new(44_100, 64);
        let context = harness.context().clone();
        let mut mixer = build_soundfont_mixer(&mut harness).expect("mixer should initialize");

        mixer
            .state
            .track_mut(TrackId(0))
            .expect("track 0 should exist")
            .set_instrument_slot(soundfont_slot(40));
        let sync = mixer
            .runtime
            .sync_track_instrument(&context, harness.commands(), &mixer.state, TrackId(0))
            .expect("track instrument should sync");

        assert!(
            matches!(sync, super::TrackInstrumentSync::UpdatedInPlace),
            "same-soundfont program changes should not require graph rebuild"
        );

        let handle = mixer
            .instrument_handle(TrackId(0))
            .expect("track instrument should exist");
        harness.commands().transport_play();
        harness.wait_for_transport_settled();
        schedule_test_note(&mut harness, handle);
        harness.process_blocks(16);

        assert!(
            harness.output_has_signal(),
            "same-soundfont program change should stay audible without routing resync"
        );
    }
}
