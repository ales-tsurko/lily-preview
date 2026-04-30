use iced::widget::Id;
use iced::widget::{
    button, column, container, lazy, mouse_area, opaque, pick_list, responsive, row, scrollable,
    stack, text, text_input,
};
use iced::{Color, Element, Fill, FillPortion, Length, Padding, alignment, border, mouse};
use iced_aw::helpers::color_picker_with_change;
use lilypalooza_audio::instrument::registry;
use lilypalooza_audio::mixer::{
    BusId, ChannelMeterSnapshot, MixerMeterSnapshot, MixerMeterSnapshotWindow, STRIP_METER_MIN_DB,
    StripMeterSnapshot, TrackRoute, TrackRouting,
};
use lilypalooza_audio::{BUILTIN_METRONOME_ID, BUILTIN_NONE_ID, MixerState, SlotState};

use super::controls::{
    GAIN_MIN_DB, compact_gain_slider, gain_control_width, gain_fader, gain_fader_scale,
    gain_fader_scale_width, gain_knob, pan_knob,
};
use super::messages::MixerMessage;
use super::meters::{
    MeterColors, meter_colors, stereo_meter, stereo_meter_bar_width, stereo_meter_width,
    stereo_meter_with_scale,
};
use super::{Lilypalooza, Message};
use crate::{fonts, icons, ui_style};

pub(super) const MIXER_MIN_HEIGHT: f32 = ui_style::grid_f32(102);
pub(super) const MIXER_MIN_WIDTH: f32 = 520.0;
const INSTRUMENT_SCROLL_ID: &str = "mixer-instrument-scroll";

pub(super) fn instrument_scroll_id() -> Id {
    Id::new(INSTRUMENT_SCROLL_ID)
}

pub(in crate::app) fn effect_rack_scroll_id(track_index: usize) -> Id {
    Id::from(format!("effect-rack-scroll-{track_index}"))
}

const GROUP_SIDE_BORDER_WIDTH: f32 = 1.0;
const MAIN_STRIP_WIDTH: f32 = ui_style::grid_f32(36);
const MAIN_SECTION_WIDTH: f32 = MAIN_STRIP_WIDTH + GROUP_SIDE_BORDER_WIDTH * 2.0;
const STRIP_WIDTH: f32 = ui_style::grid_f32(37);
const STRIP_SPACING: f32 = 0.0;
const PROCESSOR_SLOT_HEIGHT: f32 = ui_style::grid_f32(7);
const PROCESSOR_SLOT_BUTTON_HEIGHT: f32 = ui_style::grid_f32(6);
const PROCESSOR_SLOT_WIDTH: f32 = 112.0;
const PROCESSOR_SLOT_SEGMENT_WIDTH: f32 = ui_style::grid_f32(4);
const PROCESSOR_SLOT_SEPARATOR_WIDTH: f32 = 1.0;
const PROCESSOR_SLOT_LABEL_MAX_LEN: usize = 11;
const PROCESSOR_BROWSER_WIDTH: f32 = 520.0;
const PROCESSOR_BROWSER_HEIGHT: f32 = 360.0;
const PROCESSOR_BROWSER_ICON_SIZE: f32 = ui_style::grid_f32(3);
const EFFECT_RACK_VISIBLE_SLOTS: usize = 7;
pub(in crate::app) const EFFECT_RACK_HEIGHT: f32 =
    EFFECT_RACK_ROW_HEIGHT * EFFECT_RACK_VISIBLE_SLOTS as f32;
pub(in crate::app) const EFFECT_RACK_EDGE_SCROLL_ZONE: f32 = ui_style::grid_f32(3);
pub(in crate::app) const EFFECT_RACK_EDGE_SCROLL_STEP: f32 = ui_style::grid_f32(2);
const EFFECT_RACK_PANEL_WIDTH: f32 = ui_style::grid_f32(36);
const EFFECT_RACK_SLOT_WIDTH: f32 = EFFECT_RACK_PANEL_WIDTH;
const EFFECT_RACK_SEPARATOR_HEIGHT: f32 = 1.0;
pub(in crate::app) const EFFECT_RACK_ROW_HEIGHT: f32 =
    PROCESSOR_SLOT_BUTTON_HEIGHT + EFFECT_RACK_SEPARATOR_HEIGHT;
const EFFECT_RACK_SEPARATOR_INSET: f32 = ui_style::grid_f32(4);
const EFFECT_RACK_SCROLLBAR_WIDTH: f32 = 6.0;
const EFFECT_RACK_SCROLLBAR_SCROLLER_WIDTH: f32 = 3.0;
const EFFECT_RACK_SCROLLBAR_SPACING: f32 = 2.0;
const EFFECT_RACK_SCROLLBAR_MARGIN: f32 = 1.0;
const ROUTE_PICKER_HEIGHT: f32 = ui_style::grid_f32(6);
const ROUTE_PICKER_TOP_SPACING: f32 = ui_style::grid_f32(3);
const ROUTE_PICKER_BOTTOM_INSET: f32 = 0.0;
const ROUTE_MENU_ITEM_HEIGHT: f32 = ui_style::grid_f32(7);
const ROUTE_MENU_MAX_ITEMS: usize = 6;
const ROUTE_PICKER_MAX_LEN: usize = 12;
const SEND_ROW_HEIGHT: f32 = ui_style::grid_f32(14);
const SEND_ROW_CONTENT_BOTTOM_SPACING: f32 = ui_style::grid_f32(1);
const SEND_PANEL_TOP_SPACING: f32 = ui_style::grid_f32(1);
const SEND_PANEL_HEADER_HEIGHT: f32 = ui_style::grid_f32(5);
const SEND_CONTROL_HEIGHT: f32 = ui_style::grid_f32(5);
const SEND_MODE_HEIGHT: f32 = SEND_CONTROL_HEIGHT;
const SEND_MODE_WIDTH: f32 = ui_style::grid_f32(10);
const SEND_PICKER_WIDTH: f32 = ui_style::grid_f32(16);
const SEND_GAIN_MIN_DB: f32 = GAIN_MIN_DB;
const SEND_GAIN_MAX_DB: f32 = 6.0;
const SEND_GAIN_STEP_DB: f32 = 0.5;
const INSTRUMENT_PICKER_HEIGHT: f32 = PROCESSOR_SLOT_HEIGHT;
const INSTRUMENT_SLOT_BUTTON_HEIGHT: f32 = PROCESSOR_SLOT_BUTTON_HEIGHT;
#[cfg(test)]
const INSTRUMENT_SLOT_WIDTH: f32 = PROCESSOR_SLOT_WIDTH;
#[cfg(test)]
const INSTRUMENT_SLOT_EDITOR_AREA_WIDTH: f32 = PROCESSOR_SLOT_SEGMENT_WIDTH;
const INSTRUMENT_SLOT_SEPARATOR_WIDTH: f32 = PROCESSOR_SLOT_SEPARATOR_WIDTH;
const INSTRUMENT_BROWSER_WIDTH: f32 = PROCESSOR_BROWSER_WIDTH;
const INSTRUMENT_BROWSER_HEIGHT: f32 = PROCESSOR_BROWSER_HEIGHT;
#[cfg(test)]
const INSTRUMENT_BROWSER_ICON_SIZE: f32 = PROCESSOR_BROWSER_ICON_SIZE;
const SECTION_HEADER_HEIGHT: f32 = 24.0;
const STRIP_MIN_HEIGHT: f32 = ui_style::grid_f32(88);
const STRIP_FOOTER_HEIGHT: f32 = TITLE_TOP_SPACING
    + TRACK_TITLE_EDITOR_HEIGHT
    + ROUTE_PICKER_TOP_SPACING
    + ROUTE_PICKER_HEIGHT
    + ROUTE_PICKER_BOTTOM_INSET;
const STRIP_TOGGLE_SIZE: f32 = INSTRUMENT_PICKER_HEIGHT - 4.0;
const TRACK_TITLE_EDITOR_HEIGHT: f32 = ui_style::grid_f32(5);
const TRACK_TITLE_EDITOR_CONTROL_HEIGHT: f32 = TRACK_TITLE_EDITOR_HEIGHT;
const TRACK_TITLE_EDITOR_SWATCH_SIZE: f32 = TRACK_TITLE_EDITOR_HEIGHT;
const TRACK_TITLE_EDITOR_INPUT_PADDING_V: u16 = 2;
const TRACK_TITLE_EDITOR_INPUT_PADDING_H: u16 = ui_style::grid(1);
const COMPACT_GAIN_SWITCH_OFFSET: f32 = ui_style::grid_f32(8);
const VALUE_LABEL_HEIGHT: f32 = ui_style::grid_f32(4);
const HEADER_SIDE_INSET: f32 = 12.0;
const SECTION_BODY_GAP: f32 = 0.0;
const METER_STACK_SPACING: f32 = ui_style::grid_f32(2);
const STRIP_STACK_SPACING: f32 = ui_style::grid_f32(1);
const LABEL_CONTROL_SPACING: f32 = ui_style::SPACE_XS as f32;
const GAIN_SCALE_SPACING: f32 = 1.0;
const TITLE_TOP_SPACING: f32 = ui_style::grid_f32(3);
const STRIP_VIRTUALIZATION_OVERSCAN: usize = 2;

pub(super) fn instrument_track_scroll_x(track_index: usize) -> f32 {
    strip_span_width(track_index)
}

struct StripActions<'a> {
    panel: Option<(bool, bool, Message)>,
    solo: Option<(bool, Message)>,
    mute: Option<(bool, Message)>,
    on_gain: Option<Box<dyn Fn(f32) -> Message + 'a>>,
    on_pan: Option<Box<dyn Fn(f32) -> Message + 'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RoutingStrip {
    Track(usize),
    Bus(u16),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RouteChoice {
    route: TrackRoute,
    label: String,
}

impl std::fmt::Display for RouteChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SendDestinationAction {
    Route(u16),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SendDestinationChoice {
    action: SendDestinationAction,
    label: String,
}

impl std::fmt::Display for SendDestinationChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SendDependency {
    bus_id: u16,
    gain_bits: u32,
    enabled: bool,
    pre_fader: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectRackPanelRouting {
    source: RoutingStrip,
    sends: Vec<SendDependency>,
    send_choices: Vec<SendDestinationChoice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectRackDragState {
    source_effect_index: usize,
    target_effect_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectRackDropIndicator {
    Top,
    After(usize),
}

fn effect_rack_drag_state(
    source: Option<(usize, usize)>,
    target: Option<(usize, usize)>,
    strip_index: usize,
) -> Option<EffectRackDragState> {
    let (source_strip, source_effect_index) = source?;
    let (target_strip, target_effect_index) = target?;

    (source_strip == strip_index
        && target_strip == strip_index
        && source_effect_index != target_effect_index)
        .then_some(EffectRackDragState {
            source_effect_index,
            target_effect_index,
        })
}

fn effect_rack_drop_indicator(
    drag_state: Option<EffectRackDragState>,
) -> Option<EffectRackDropIndicator> {
    let drag_state = drag_state?;
    if drag_state.source_effect_index < drag_state.target_effect_index {
        Some(EffectRackDropIndicator::After(
            drag_state.target_effect_index,
        ))
    } else if drag_state.target_effect_index == 0 {
        Some(EffectRackDropIndicator::Top)
    } else {
        Some(EffectRackDropIndicator::After(
            drag_state.target_effect_index - 1,
        ))
    }
}

fn noop_message() -> Message {
    Message::Noop
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ProcessorChoice {
    None,
    Processor {
        processor_id: String,
        name: String,
        backend: ProcessorBrowserBackend,
    },
}

pub(super) type InstrumentChoice = ProcessorChoice;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum ProcessorBrowserBackend {
    BuiltIn,
    Clap,
    Vst3,
}

pub(super) type InstrumentBrowserBackend = ProcessorBrowserBackend;

impl ProcessorBrowserBackend {
    fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "Built-in",
            Self::Clap => "CLAP",
            Self::Vst3 => "VST3",
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum ProcessorSlotRole {
    Instrument,
    Effect,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum ProcessorSlotSegment {
    Bypass,
    Editor,
    Picker,
}

impl ProcessorSlotRole {
    fn registry_role(self) -> registry::Role {
        match self {
            Self::Instrument => registry::Role::Instrument,
            Self::Effect => registry::Role::Effect,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Instrument => "Choose Instrument",
            Self::Effect => "Choose Effect",
        }
    }

    fn search_placeholder(self) -> &'static str {
        match self {
            Self::Instrument => "Search instruments",
            Self::Effect => "Search effects",
        }
    }

    fn empty_search_label(self) -> &'static str {
        match self {
            Self::Instrument => "No matching instruments",
            Self::Effect => "No matching effects",
        }
    }

    fn backend_empty_label(self, backend: ProcessorBrowserBackend) -> &'static str {
        match (self, backend) {
            (Self::Instrument, ProcessorBrowserBackend::Clap) => "No CLAP instruments yet",
            (Self::Instrument, ProcessorBrowserBackend::Vst3) => "No VST3 instruments yet",
            (Self::Effect, ProcessorBrowserBackend::Clap) => "No CLAP effects yet",
            (Self::Effect, ProcessorBrowserBackend::Vst3) => "No VST3 effects yet",
            (_, ProcessorBrowserBackend::BuiltIn) => "",
        }
    }

    fn slot_icon(self) -> iced::widget::svg::Handle {
        match self {
            Self::Instrument => icons::keyboard_music(),
            Self::Effect => icons::audio_waveform(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstrumentBrowserEntries {
    show_none: bool,
    entries: Vec<ProcessorChoice>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct MainStripDependency {
    gain_bits: u32,
    pan_bits: u32,
    meter: MeterDependency,
    compact_gain: bool,
    effect_rack_open: bool,
    panel_has_content: bool,
    strip_height_bits: u32,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TrackStripDependency {
    index: usize,
    name: String,
    selected: Option<ProcessorChoice>,
    editor_enabled: bool,
    effects: Vec<EffectSlotDependency>,
    hovered_processor_slot: Option<(usize, ProcessorSlotSegment)>,
    color_bits: [u32; 4],
    gain_bits: u32,
    pan_bits: u32,
    route: RouteChoice,
    route_choices: Vec<RouteChoice>,
    meter: MeterDependency,
    compact_gain: bool,
    effect_rack_open: bool,
    panel_has_content: bool,
    strip_height_bits: u32,
    soloed: bool,
    muted: bool,
    tint_enabled: bool,
    highlighted: bool,
    renaming: bool,
    rename_value: String,
    color_picker_open: bool,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct EffectSlotDependency {
    slot_index: usize,
    instance_id: u64,
    instance_label_index: u32,
    selected: Option<ProcessorChoice>,
    editor_enabled: bool,
    bypassed: bool,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct BusStripDependency {
    strip_index: usize,
    id: u16,
    name: String,
    effects: Vec<EffectSlotDependency>,
    effect_rack_open: bool,
    gain_bits: u32,
    pan_bits: u32,
    route: RouteChoice,
    route_choices: Vec<RouteChoice>,
    meter: MeterDependency,
    compact_gain: bool,
    strip_height_bits: u32,
    panel_has_content: bool,
    soloed: bool,
    muted: bool,
    renaming: bool,
    rename_value: String,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct MeterStackDependency {
    meter: MeterDependency,
    colors: MeterColorsDependency,
    compact_gain: bool,
    strip_height_bits: u32,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct MeterDependency {
    left_level_bits: u32,
    right_level_bits: u32,
    left_hold_bits: u32,
    right_hold_bits: u32,
    left_hold_db_bits: u32,
    right_hold_db_bits: u32,
    clip_latched: bool,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct MeterColorsDependency {
    rail: [u32; 4],
    safe: [u32; 4],
    warning: [u32; 4],
    hot: [u32; 4],
    hold: [u32; 4],
    clip: [u32; 4],
    scale_text: [u32; 4],
    scale_tick: [u32; 4],
}

impl MeterDependency {
    fn from_snapshot(snapshot: StripMeterSnapshot) -> Self {
        Self {
            left_level_bits: snapshot.left.level.to_bits(),
            right_level_bits: snapshot.right.level.to_bits(),
            left_hold_bits: snapshot.left.hold.to_bits(),
            right_hold_bits: snapshot.right.hold.to_bits(),
            left_hold_db_bits: snapshot.left.hold_db.to_bits(),
            right_hold_db_bits: snapshot.right.hold_db.to_bits(),
            clip_latched: snapshot.clip_latched,
        }
    }

    fn snapshot(self) -> StripMeterSnapshot {
        StripMeterSnapshot {
            left: ChannelMeterSnapshot {
                level: f32::from_bits(self.left_level_bits),
                hold: f32::from_bits(self.left_hold_bits),
                hold_db: f32::from_bits(self.left_hold_db_bits),
            },
            right: ChannelMeterSnapshot {
                level: f32::from_bits(self.right_level_bits),
                hold: f32::from_bits(self.right_hold_bits),
                hold_db: f32::from_bits(self.right_hold_db_bits),
            },
            clip_latched: self.clip_latched,
        }
    }
}

impl MeterColorsDependency {
    fn from_colors(colors: MeterColors) -> Self {
        Self {
            rail: color_bits(colors.rail),
            safe: color_bits(colors.safe),
            warning: color_bits(colors.warning),
            hot: color_bits(colors.hot),
            hold: color_bits(colors.hold),
            clip: color_bits(colors.clip),
            scale_text: color_bits(colors.scale_text),
            scale_tick: color_bits(colors.scale_tick),
        }
    }

    fn colors(self) -> MeterColors {
        MeterColors {
            rail: color_from_bits(self.rail),
            safe: color_from_bits(self.safe),
            warning: color_from_bits(self.warning),
            hot: color_from_bits(self.hot),
            hold: color_from_bits(self.hold),
            clip: color_from_bits(self.clip),
            scale_text: color_from_bits(self.scale_text),
            scale_tick: color_from_bits(self.scale_tick),
        }
    }
}

fn color_bits(color: iced::Color) -> [u32; 4] {
    [
        color.r.to_bits(),
        color.g.to_bits(),
        color.b.to_bits(),
        color.a.to_bits(),
    ]
}

fn color_from_bits(bits: [u32; 4]) -> iced::Color {
    iced::Color {
        r: f32::from_bits(bits[0]),
        g: f32::from_bits(bits[1]),
        b: f32::from_bits(bits[2]),
        a: f32::from_bits(bits[3]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainControlMode {
    Fader,
    Knob,
}

pub(super) fn content(app: &Lilypalooza) -> Element<'_, Message> {
    let colors = meter_colors(&app.theme);
    let existing_track_count = app
        .piano_roll
        .current_file()
        .map(|file| file.data.tracks.len())
        .unwrap_or(0);
    let track_colors: Vec<_> = (0..existing_track_count)
        .map(|track_index| app.effective_track_color(track_index))
        .collect();
    let renaming_target = app.renaming_target;
    let renaming_origin = app.renaming_origin;
    let track_rename_value = app.track_rename_value.clone();
    let track_rename_color_value = app.track_rename_color_value;
    let track_rename_color_picker_open = app.track_rename_color_picker_open;

    if let Some(playback) = app.playback.as_ref() {
        let mixer = playback.mixer_state();

        return responsive(move |size| {
            let gain_mode = gain_control_mode(size.height);
            let strip_height =
                (size.height - (ui_style::PADDING_SM as f32 * 2.0) - SECTION_HEADER_HEIGHT)
                    .max(STRIP_MIN_HEIGHT);
            let instrument_visible = visible_strip_window(
                mixer.tracks().len(),
                app.mixer_instrument_scroll_x,
                app.mixer_instrument_viewport_width.max(size.width * 0.5),
            );
            let bus_visible = visible_strip_window(
                mixer.buses().len(),
                app.mixer_bus_scroll_x,
                app.mixer_bus_viewport_width.max(size.width * 0.2),
            );
            let meter_window =
                playback.meter_snapshot_window(instrument_visible.clone(), bus_visible.clone());
            let master_width = if app.open_mixer_effect_rack_tracks.contains(&0) {
                MAIN_SECTION_WIDTH + EFFECT_RACK_PANEL_WIDTH
            } else {
                MAIN_SECTION_WIDTH
            };
            let mixer_row = row![
                container(master_track_area(
                    mixer,
                    meter_window.main,
                    colors,
                    strip_height,
                    gain_mode,
                    &app.open_mixer_effect_rack_tracks,
                    app.hovered_processor_slot,
                    effect_rack_drag_state(app.effect_drag_source, app.effect_drag_target, 0),
                    true,
                ))
                .width(Length::Fixed(master_width))
                .height(Fill)
                .style(ui_style::mixer_side_group_surface),
                container(instrument_track_area(
                    mixer,
                    &meter_window,
                    colors,
                    strip_height,
                    gain_mode,
                    instrument_visible,
                    existing_track_count,
                    &track_colors,
                    renaming_target,
                    renaming_origin,
                    &track_rename_value,
                    track_rename_color_value,
                    track_rename_color_picker_open,
                    app.selected_track_index,
                    app.hovered_processor_slot,
                    app.effect_drag_source,
                    app.effect_drag_target,
                    &app.open_mixer_effect_rack_tracks,
                    true,
                ))
                .width(FillPortion(5))
                .height(Fill)
                .style(ui_style::mixer_instrument_group_surface),
                container(bus_track_area(
                    mixer,
                    &meter_window,
                    colors,
                    strip_height,
                    gain_mode,
                    bus_visible,
                    &app.open_mixer_effect_rack_tracks,
                    app.hovered_processor_slot,
                    app.effect_drag_source,
                    app.effect_drag_target,
                    renaming_target,
                    renaming_origin,
                    &track_rename_value,
                    true,
                ))
                .width(FillPortion(2))
                .height(Fill)
                .style(ui_style::mixer_side_group_surface)
            ];
            mixer_row
                .spacing(ui_style::SPACE_SM)
                .padding(ui_style::PADDING_SM)
                .width(Fill)
                .height(Fill)
                .into()
        })
        .into();
    }

    content_without_audio()
}

fn content_without_audio() -> Element<'static, Message> {
    container(
        text("Audio engine disabled")
            .size(ui_style::FONT_SIZE_UI_SM)
            .font(fonts::MONO),
    )
    .width(Fill)
    .height(Fill)
    .center_x(Fill)
    .center_y(Fill)
    .style(ui_style::mixer_instrument_group_surface)
    .into()
}

#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
fn mixer_layout_without_audio<'a>(
    mixer: &'a MixerState,
    meter_snapshot: &'a MixerMeterSnapshot,
    colors: MeterColors,
    size: iced::Size,
    instrument_scroll_x: f32,
    instrument_viewport_width: f32,
    bus_scroll_x: f32,
    bus_viewport_width: f32,
    renaming_target: Option<super::RenameTarget>,
    track_rename_value: String,
    selected_track_index: Option<usize>,
) -> Element<'a, Message> {
    let existing_track_count = 0;
    let gain_mode = gain_control_mode(size.height);
    let strip_height = (size.height - (ui_style::PADDING_SM as f32 * 2.0) - SECTION_HEADER_HEIGHT)
        .max(STRIP_MIN_HEIGHT);
    let instrument_visible = visible_strip_window(
        mixer.tracks().len(),
        instrument_scroll_x,
        instrument_viewport_width.max(size.width * 0.5),
    );
    let bus_visible = visible_strip_window(
        mixer.buses().len(),
        bus_scroll_x,
        bus_viewport_width.max(size.width * 0.2),
    );
    let meter_window = MixerMeterSnapshotWindow {
        main: meter_snapshot.main,
        tracks: meter_snapshot.tracks[instrument_visible.clone()].to_vec(),
        buses: meter_snapshot.buses[bus_visible.clone()]
            .iter()
            .map(|(_, snapshot)| *snapshot)
            .collect(),
    };

    row![
        container(master_track_area(
            mixer,
            meter_window.main,
            colors,
            strip_height,
            gain_mode,
            &[],
            None,
            None,
            false,
        ))
        .width(Length::Fixed(MAIN_SECTION_WIDTH))
        .height(Fill)
        .style(ui_style::mixer_side_group_surface),
        container(instrument_track_area(
            mixer,
            &meter_window,
            colors,
            strip_height,
            gain_mode,
            instrument_visible,
            existing_track_count,
            &[],
            renaming_target,
            None,
            &track_rename_value,
            Color::TRANSPARENT,
            false,
            selected_track_index,
            None,
            None,
            None,
            &[],
            false,
        ))
        .width(FillPortion(5))
        .height(Fill)
        .style(ui_style::mixer_instrument_group_surface),
        container(bus_track_area(
            mixer,
            &meter_window,
            colors,
            strip_height,
            gain_mode,
            bus_visible,
            &[],
            None,
            None,
            None,
            renaming_target,
            None,
            &track_rename_value,
            false,
        ))
        .width(FillPortion(2))
        .height(Fill)
        .style(ui_style::mixer_side_group_surface)
    ]
    .spacing(ui_style::SPACE_SM)
    .padding(ui_style::PADDING_SM)
    .width(Fill)
    .height(Fill)
    .into()
}

#[allow(clippy::too_many_arguments)]
fn master_track_area(
    mixer: &MixerState,
    meter_snapshot: StripMeterSnapshot,
    colors: MeterColors,
    strip_height: f32,
    gain_mode: GainControlMode,
    open_effect_rack_strips: &[usize],
    hovered_processor_slot: Option<(
        super::processor_editor_windows::EditorTarget,
        ProcessorSlotSegment,
    )>,
    effect_drag: Option<EffectRackDragState>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let mut master_row = row![sticky_master_strip(
        mixer,
        meter_snapshot,
        colors,
        strip_height,
        gain_mode,
        open_effect_rack_strips.contains(&0),
        controls_enabled,
    )]
    .align_y(alignment::Vertical::Top)
    .height(Length::Fixed(strip_height));
    if open_effect_rack_strips.contains(&0) {
        master_row = master_row.push(track_effect_rack_panel(
            0,
            effect_slot_dependencies(mixer.master()),
            None,
            hovered_processor_slot,
            effect_drag,
            controls_enabled,
            strip_height,
        ));
    }

    column![
        container(section_header_bar(row![section_title("Main")]))
            .style(ui_style::workspace_toolbar_surface),
        container(text("")).height(Length::Fixed(SECTION_BODY_GAP)),
        row![
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
            container(master_row).width(Fill).height(Fill),
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
        ]
        .height(Fill),
        container(text(""))
            .width(Fill)
            .height(Length::Fixed(1.0))
            .style(ui_style::chrome_separator)
    ]
    .spacing(0)
    .height(Fill)
    .into()
}

fn sticky_master_strip(
    mixer: &MixerState,
    meter_snapshot: StripMeterSnapshot,
    colors: MeterColors,
    strip_height: f32,
    gain_mode: GainControlMode,
    effect_rack_open: bool,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let master = mixer.master();
    lazy(
        MainStripDependency {
            gain_bits: master.state.gain_db.to_bits(),
            pan_bits: master.state.pan.to_bits(),
            meter: MeterDependency::from_snapshot(meter_snapshot),
            compact_gain: matches!(gain_mode, GainControlMode::Knob),
            effect_rack_open,
            panel_has_content: !master.effects().is_empty(),
            strip_height_bits: strip_height.to_bits(),
        },
        move |dependency| {
            let gain_mode = if dependency.compact_gain {
                GainControlMode::Knob
            } else {
                GainControlMode::Fader
            };
            strip_panel(
                strip_shell(
                    container(section_title("Main"))
                        .width(Fill)
                        .center_x(Fill)
                        .into(),
                    None,
                    None,
                    f32::from_bits(dependency.gain_bits),
                    f32::from_bits(dependency.pan_bits),
                    meter_stack(
                        MeterStackDependency {
                            meter: dependency.meter,
                            colors: MeterColorsDependency::from_colors(colors),
                            compact_gain: dependency.compact_gain,
                            strip_height_bits: dependency.strip_height_bits,
                        },
                        Some(if controls_enabled {
                            Message::Mixer(MixerMessage::ResetMasterMeter)
                        } else {
                            noop_message()
                        }),
                    ),
                    StripActions {
                        panel: Some((
                            dependency.effect_rack_open,
                            dependency.panel_has_content,
                            if controls_enabled {
                                Message::Mixer(MixerMessage::ToggleMixerEffectRack(0))
                            } else {
                                noop_message()
                            },
                        )),
                        solo: None,
                        mute: None,
                        on_gain: Some(Box::new(move |value| {
                            if controls_enabled {
                                Message::Mixer(MixerMessage::SetMasterGain(value))
                            } else {
                                noop_message()
                            }
                        })),
                        on_pan: Some(Box::new(move |value| {
                            if controls_enabled {
                                Message::Mixer(MixerMessage::SetMasterPan(value))
                            } else {
                                noop_message()
                            }
                        })),
                    },
                    f32::from_bits(dependency.strip_height_bits),
                    gain_mode,
                    true,
                ),
                MAIN_STRIP_WIDTH,
                f32::from_bits(dependency.strip_height_bits),
                false,
                None,
            )
        },
    )
    .into()
}

#[allow(clippy::too_many_arguments)]
fn instrument_track_area(
    mixer: &MixerState,
    meters: &MixerMeterSnapshotWindow,
    colors: MeterColors,
    strip_height: f32,
    gain_mode: GainControlMode,
    visible: std::ops::Range<usize>,
    existing_track_count: usize,
    track_colors: &[Color],
    renaming_target: Option<super::RenameTarget>,
    renaming_origin: Option<super::WorkspacePaneKind>,
    track_rename_value: &str,
    track_rename_color_value: Color,
    track_rename_color_picker_open: bool,
    selected_track_index: Option<usize>,
    hovered_processor_slot: Option<(
        super::processor_editor_windows::EditorTarget,
        ProcessorSlotSegment,
    )>,
    effect_drag_source: Option<(usize, usize)>,
    effect_drag_target: Option<(usize, usize)>,
    open_effect_rack_strips: &[usize],
    controls_enabled: bool,
) -> Element<'static, Message> {
    let left_spacer = strip_span_width(visible.start);
    let right_spacer = strip_span_width(mixer.tracks().len().saturating_sub(visible.end));
    let track_row = mixer.tracks()[visible.clone()].iter().enumerate().fold(
        row![]
            .spacing(STRIP_SPACING)
            .align_y(alignment::Vertical::Top)
            .height(Length::Fixed(strip_height))
            .push(horizontal_spacer(left_spacer)),
        move |row, (local_index, track)| {
            let track_index = visible.start + local_index;
            let strip_index = track_index + 1;
            let effect_rack_open = open_effect_rack_strips.contains(&strip_index);
            let selected_choice = selected_instrument_choice(track.instrument_slot(), mixer);
            let effects = effect_slot_dependencies(track);
            let route_choices = route_choices(mixer, RoutingStrip::Track(track_index));
            let selected_route = selected_route_choice(track.routing.main, &route_choices);
            let strip_hovered_processor_slot =
                hovered_processor_slot.filter(|(target, _)| target.strip_index == strip_index);
            let hovered_processor_slot = strip_hovered_processor_slot
                .filter(|(target, _)| target.slot_index == 0)
                .map(|(target, segment)| (target.slot_index, segment));
            let track_color = track_colors
                .get(track_index)
                .copied()
                .unwrap_or_else(|| crate::track_colors::default_track_color(track_index));
            let meter_dependency = MeterStackDependency {
                meter: MeterDependency::from_snapshot(
                    meters.tracks.get(local_index).copied().unwrap_or_default(),
                ),
                colors: MeterColorsDependency::from_colors(colors),
                compact_gain: matches!(gain_mode, GainControlMode::Knob),
                strip_height_bits: strip_height.to_bits(),
            };
            let row = row.push(lazy(
                TrackStripDependency {
                    index: track_index,
                    name: track.name.clone(),
                    selected: selected_choice.clone(),
                    editor_enabled: track
                        .instrument_slot()
                        .filter(|slot| !slot.is_empty())
                        .and_then(|slot| slot.descriptor())
                        .and_then(|descriptor| descriptor.editor)
                        .is_some(),
                    effects: effects.clone(),
                    hovered_processor_slot,
                    color_bits: color_bits(track_color),
                    gain_bits: track.state.gain_db.to_bits(),
                    pan_bits: track.state.pan.to_bits(),
                    route: selected_route,
                    route_choices,
                    meter: meter_dependency.meter,
                    compact_gain: matches!(gain_mode, GainControlMode::Knob),
                    effect_rack_open,
                    panel_has_content: !effects.is_empty()
                        || !track.routing.sends.is_empty()
                        || track.routing.main != TrackRoute::Master,
                    strip_height_bits: strip_height.to_bits(),
                    soloed: track.state.soloed,
                    muted: track.state.muted,
                    tint_enabled: track_should_use_roll_tint(track_index, existing_track_count),
                    highlighted: selected_track_index == Some(track_index),
                    renaming: renaming_target == Some(super::RenameTarget::Track(track_index))
                        && renaming_origin == Some(super::WorkspacePaneKind::Mixer),
                    rename_value: track_rename_value.to_string(),
                    color_picker_open: renaming_target
                        == Some(super::RenameTarget::Track(track_index))
                        && renaming_origin == Some(super::WorkspacePaneKind::Mixer)
                        && track_rename_color_picker_open,
                },
                move |dependency| {
                    let name = dependency.name.clone();
                    let is_selected = dependency.highlighted;
                    let track_color = if dependency.renaming {
                        track_rename_color_value
                    } else {
                        color_from_bits(dependency.color_bits)
                    };
                    let strip_height = f32::from_bits(dependency.strip_height_bits);
                    let gain_mode = if dependency.compact_gain {
                        GainControlMode::Knob
                    } else {
                        GainControlMode::Fader
                    };
                    let shell = strip_shell(
                        track_title_content(
                            track_index,
                            &name,
                            dependency.renaming,
                            &dependency.rename_value,
                            track_color,
                            dependency.color_picker_open,
                        ),
                        Some({
                            let target = super::processor_editor_windows::EditorTarget {
                                strip_index,
                                slot_index: 0,
                            };
                            strip_processor_header(
                                strip_index,
                                Some((
                                    target,
                                    dependency.selected.as_ref(),
                                    dependency.editor_enabled,
                                    dependency
                                        .hovered_processor_slot
                                        .map(|(_, segment)| segment),
                                )),
                                controls_enabled,
                            )
                        }),
                        Some(route_picker(
                            RoutingStrip::Track(track_index),
                            dependency.route.clone(),
                            dependency.route_choices.clone(),
                            controls_enabled,
                        )),
                        f32::from_bits(dependency.gain_bits),
                        f32::from_bits(dependency.pan_bits),
                        meter_stack(
                            meter_dependency,
                            Some(if controls_enabled {
                                Message::Mixer(MixerMessage::ResetTrackMeter(track_index))
                            } else {
                                noop_message()
                            }),
                        ),
                        StripActions {
                            panel: Some((
                                dependency.effect_rack_open,
                                dependency.panel_has_content,
                                if controls_enabled {
                                    Message::Mixer(MixerMessage::ToggleMixerEffectRack(strip_index))
                                } else {
                                    noop_message()
                                },
                            )),
                            solo: Some((
                                dependency.soloed,
                                if controls_enabled {
                                    Message::Mixer(MixerMessage::ToggleTrackSolo(track_index))
                                } else {
                                    noop_message()
                                },
                            )),
                            mute: Some((
                                dependency.muted,
                                if controls_enabled {
                                    Message::Mixer(MixerMessage::ToggleTrackMute(track_index))
                                } else {
                                    noop_message()
                                },
                            )),
                            on_gain: Some(Box::new(move |value| {
                                if controls_enabled {
                                    Message::Mixer(MixerMessage::SetTrackGain(track_index, value))
                                } else {
                                    noop_message()
                                }
                            })),
                            on_pan: Some(Box::new(move |value| {
                                if controls_enabled {
                                    Message::Mixer(MixerMessage::SetTrackPan(track_index, value))
                                } else {
                                    noop_message()
                                }
                            })),
                        },
                        strip_height,
                        gain_mode,
                        true,
                    );

                    if dependency.tint_enabled {
                        tinted_track_strip_panel(
                            shell,
                            STRIP_WIDTH,
                            strip_height,
                            track_color,
                            is_selected,
                            Some(Message::Mixer(MixerMessage::SelectTrack(track_index))),
                        )
                    } else {
                        strip_panel(
                            shell,
                            STRIP_WIDTH,
                            strip_height,
                            is_selected,
                            Some(Message::Mixer(MixerMessage::SelectTrack(track_index))),
                        )
                    }
                },
            ));
            if effect_rack_open {
                row.push(track_effect_rack_panel(
                    strip_index,
                    effects.clone(),
                    Some(EffectRackPanelRouting {
                        source: RoutingStrip::Track(track_index),
                        sends: send_dependencies(&track.routing),
                        send_choices: send_destination_choices(
                            mixer,
                            RoutingStrip::Track(track_index),
                        ),
                    }),
                    strip_hovered_processor_slot,
                    effect_rack_drag_state(effect_drag_source, effect_drag_target, strip_index),
                    controls_enabled,
                    strip_height,
                ))
            } else {
                row
            }
        },
    );
    let track_row = track_row.push(horizontal_spacer(right_spacer));

    column![
        container(section_header_bar(
            row![
                section_title("Instrument Tracks"),
                container(text("")).width(Fill),
            ]
            .align_y(alignment::Vertical::Center),
        ))
        .style(ui_style::workspace_toolbar_surface),
        container(text("")).height(Length::Fixed(SECTION_BODY_GAP)),
        row![
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
            scrollable(track_row)
                .id(instrument_scroll_id())
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new()
                ))
                .on_scroll(
                    |viewport| Message::Mixer(MixerMessage::InstrumentViewportScrolled(viewport))
                )
                .style(ui_style::workspace_scrollable)
                .width(Fill)
                .height(Fill),
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
        ]
        .height(Fill),
        container(text(""))
            .width(Fill)
            .height(Length::Fixed(1.0))
            .style(ui_style::chrome_separator)
    ]
    .spacing(0)
    .height(Fill)
    .into()
}

fn effect_slot_dependencies(strip: &lilypalooza_audio::mixer::Track) -> Vec<EffectSlotDependency> {
    strip
        .effects()
        .iter()
        .enumerate()
        .map(|(effect_index, slot)| EffectSlotDependency {
            slot_index: effect_index + 1,
            instance_id: slot.instance_id,
            instance_label_index: slot.instance_label_index,
            selected: selected_processor_choice(Some(slot), ProcessorSlotRole::Effect),
            editor_enabled: slot
                .descriptor()
                .and_then(|descriptor| descriptor.editor)
                .is_some(),
            bypassed: slot.bypassed,
        })
        .collect()
}

fn effect_slot_display_labels(effects: &[EffectSlotDependency]) -> Vec<Option<String>> {
    let mut counts_by_name = std::collections::BTreeMap::<String, usize>::new();
    for effect in effects {
        if let Some(name) = effect_slot_name(effect) {
            *counts_by_name.entry(name.to_string()).or_default() += 1;
        }
    }

    effects
        .iter()
        .map(|effect| {
            let name = effect_slot_name(effect)?;
            if counts_by_name.get(name).copied().unwrap_or_default() <= 1 {
                return Some(name.to_string());
            }

            Some(format!("{name} [{}]", effect.instance_label_index))
        })
        .collect()
}

fn effect_slot_name(effect: &EffectSlotDependency) -> Option<&str> {
    match effect.selected.as_ref()? {
        ProcessorChoice::None => None,
        ProcessorChoice::Processor { name, .. } => Some(name),
    }
}

fn route_choices(mixer: &MixerState, source: RoutingStrip) -> Vec<RouteChoice> {
    let mut choices = vec![RouteChoice {
        route: TrackRoute::Master,
        label: "Master".to_string(),
    }];
    choices.extend(mixer.buses().iter().filter_map(|bus| {
        let bus_id = bus.bus_id?;
        if matches!(source, RoutingStrip::Bus(source_id) if source_id == bus_id.0) {
            return None;
        }
        if matches!(source, RoutingStrip::Bus(source_id) if !mixer.can_route_bus_to_bus(BusId(source_id), bus_id))
        {
            return None;
        }
        Some(RouteChoice {
            route: TrackRoute::Bus(bus_id),
            label: route_label(&bus.name),
        })
    }));
    choices
}

fn selected_route_choice(route: TrackRoute, choices: &[RouteChoice]) -> RouteChoice {
    choices
        .iter()
        .find(|choice| choice.route == route)
        .cloned()
        .unwrap_or_else(|| choices[0].clone())
}

fn route_menu_height_for_items(item_count: usize) -> f32 {
    ROUTE_MENU_ITEM_HEIGHT * item_count.clamp(1, ROUTE_MENU_MAX_ITEMS) as f32
}

fn send_destination_choices(
    mixer: &MixerState,
    source: RoutingStrip,
) -> Vec<SendDestinationChoice> {
    mixer
        .buses()
        .iter()
        .filter_map(|bus| {
            let bus_id = bus.bus_id?;
            if matches!(source, RoutingStrip::Bus(source_id) if source_id == bus_id.0) {
                return None;
            }
            if matches!(source, RoutingStrip::Bus(source_id) if !mixer.can_route_bus_to_bus(BusId(source_id), bus_id))
            {
                return None;
            }
            Some(SendDestinationChoice {
                action: SendDestinationAction::Route(bus_id.0),
                label: route_label(&bus.name),
            })
        })
        .collect()
}

fn send_menu_choices(mut choices: Vec<SendDestinationChoice>) -> Vec<SendDestinationChoice> {
    choices.insert(
        0,
        SendDestinationChoice {
            action: SendDestinationAction::Remove,
            label: "Remove".to_string(),
        },
    );
    choices
}

fn first_send_bus_id(choices: &[SendDestinationChoice]) -> Option<u16> {
    choices.iter().find_map(|choice| match choice.action {
        SendDestinationAction::Route(bus_id) => Some(bus_id),
        SendDestinationAction::Remove => None,
    })
}

fn selected_send_destination_choice(
    bus_id: u16,
    choices: &[SendDestinationChoice],
) -> Option<SendDestinationChoice> {
    choices.iter().find_map(|choice| match choice.action {
        SendDestinationAction::Route(choice_bus_id) if choice_bus_id == bus_id => {
            Some(choice.clone())
        }
        SendDestinationAction::Route(_) | SendDestinationAction::Remove => None,
    })
}

fn send_dependencies(routing: &TrackRouting) -> Vec<SendDependency> {
    routing
        .sends
        .iter()
        .map(|send| SendDependency {
            bus_id: send.bus_id.0,
            gain_bits: send.gain_db.to_bits(),
            enabled: send.enabled,
            pre_fader: send.pre_fader,
        })
        .collect()
}

fn track_effect_rack_panel(
    strip_index: usize,
    effects: Vec<EffectSlotDependency>,
    routing: Option<EffectRackPanelRouting>,
    hovered_processor_slot: Option<(
        super::processor_editor_windows::EditorTarget,
        ProcessorSlotSegment,
    )>,
    effect_drag: Option<EffectRackDragState>,
    controls_enabled: bool,
    strip_height: f32,
) -> Element<'static, Message> {
    let hovered_processor_slot = hovered_processor_slot
        .filter(|(target, _)| target.strip_index == strip_index)
        .map(|(target, segment)| (target.slot_index, segment));
    let rack = effect_rack(
        strip_index,
        effects,
        hovered_processor_slot,
        effect_drag,
        controls_enabled,
        EFFECT_RACK_VISIBLE_SLOTS,
    );
    let has_routing = routing.is_some();
    let (rack_height, routing_height) = effect_rack_panel_heights(strip_height, has_routing);
    let mut panel = column![
        container(rack)
            .width(Fill)
            .height(Length::Fixed(rack_height))
            .style(effect_rack_surface)
    ]
    .spacing(0);

    if let Some(routing) = routing {
        panel = panel
            .push(
                container(text(""))
                    .width(Fill)
                    .height(Length::Fixed(EFFECT_RACK_SEPARATOR_HEIGHT))
                    .style(effect_rack_separator_surface),
            )
            .push(
                container(send_panel(
                    routing.source,
                    routing.sends,
                    routing.send_choices,
                    controls_enabled,
                ))
                .width(Fill)
                .height(Length::Fixed(routing_height))
                .style(effect_rack_surface),
            );
    }

    container(panel)
        .width(Length::Fixed(EFFECT_RACK_PANEL_WIDTH))
        .height(Length::Fixed(strip_height))
        .style(effect_rack_surface)
        .into()
}

fn effect_rack_panel_heights(strip_height: f32, has_routing: bool) -> (f32, f32) {
    if !has_routing {
        return (strip_height, 0.0);
    }

    let available_height = (strip_height - EFFECT_RACK_SEPARATOR_HEIGHT).max(0.0);
    let rack_height = available_height / 2.0;
    (rack_height, available_height - rack_height)
}

#[allow(clippy::too_many_arguments)]
fn bus_track_area(
    mixer: &MixerState,
    meters: &MixerMeterSnapshotWindow,
    colors: MeterColors,
    strip_height: f32,
    gain_mode: GainControlMode,
    visible: std::ops::Range<usize>,
    open_effect_rack_strips: &[usize],
    hovered_processor_slot: Option<(
        super::processor_editor_windows::EditorTarget,
        ProcessorSlotSegment,
    )>,
    effect_drag_source: Option<(usize, usize)>,
    effect_drag_target: Option<(usize, usize)>,
    renaming_target: Option<super::RenameTarget>,
    renaming_origin: Option<super::WorkspacePaneKind>,
    track_rename_value: &str,
    controls_enabled: bool,
) -> Element<'static, Message> {
    if mixer.buses().is_empty() {
        return column![
            container(section_header_bar(row![section_title("Buses")]))
                .style(ui_style::workspace_toolbar_surface),
            container(text("")).height(Length::Fixed(SECTION_BODY_GAP)),
            row![
                container(text(""))
                    .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                    .height(Fill)
                    .style(ui_style::chrome_separator),
                container(add_bus_button(controls_enabled))
                    .width(Fill)
                    .height(Fill)
                    .center_x(Fill)
                    .center_y(Fill),
                container(text(""))
                    .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                    .height(Fill)
                    .style(ui_style::chrome_separator),
            ]
            .height(Fill),
            container(text(""))
                .width(Fill)
                .height(Length::Fixed(1.0))
                .style(ui_style::chrome_separator)
        ]
        .spacing(0)
        .height(Fill)
        .into();
    }

    let total_buses = mixer.buses().len();
    let left_spacer = strip_span_width(visible.start);
    let right_spacer = if visible.end < total_buses {
        strip_span_width(total_buses.saturating_sub(visible.end) + 1)
    } else {
        0.0
    };
    let bus_row = mixer.buses()[visible.clone()].iter().enumerate().fold(
        row![]
            .spacing(STRIP_SPACING)
            .align_y(alignment::Vertical::Top)
            .height(Length::Fixed(strip_height))
            .push(horizontal_spacer(left_spacer)),
        |row, (local_index, bus)| {
            let Some(bus_id) = bus.bus_id else {
                return row;
            };
            let strip_index = 1 + mixer.track_count() + visible.start + local_index;
            let effect_rack_open = open_effect_rack_strips.contains(&strip_index);
            let effects = effect_slot_dependencies(bus);
            let routing_strip = RoutingStrip::Bus(bus_id.0);
            let route_choices = route_choices(mixer, routing_strip);
            let selected_route = selected_route_choice(bus.routing.main, &route_choices);
            let meter_dependency = MeterStackDependency {
                meter: MeterDependency::from_snapshot(
                    meters.buses.get(local_index).copied().unwrap_or_default(),
                ),
                colors: MeterColorsDependency::from_colors(colors),
                compact_gain: matches!(gain_mode, GainControlMode::Knob),
                strip_height_bits: strip_height.to_bits(),
            };
            let row = row.push(lazy(
                BusStripDependency {
                    strip_index,
                    id: bus_id.0,
                    name: bus.name.clone(),
                    effects: effects.clone(),
                    effect_rack_open,
                    gain_bits: bus.state.gain_db.to_bits(),
                    pan_bits: bus.state.pan.to_bits(),
                    route: selected_route,
                    route_choices,
                    meter: meter_dependency.meter,
                    compact_gain: matches!(gain_mode, GainControlMode::Knob),
                    strip_height_bits: strip_height.to_bits(),
                    panel_has_content: !effects.is_empty()
                        || !bus.routing.sends.is_empty()
                        || bus.routing.main != TrackRoute::Master,
                    soloed: bus.state.soloed,
                    muted: bus.state.muted,
                    renaming: renaming_target == Some(super::RenameTarget::Bus(bus_id.0))
                        && renaming_origin == Some(super::WorkspacePaneKind::Mixer),
                    rename_value: track_rename_value.to_string(),
                },
                move |dependency| {
                    let name = dependency.name.clone();
                    let strip_index = dependency.strip_index;
                    let bus_id = dependency.id;
                    let gain_db = f32::from_bits(dependency.gain_bits);
                    let pan = f32::from_bits(dependency.pan_bits);
                    let strip_height = f32::from_bits(dependency.strip_height_bits);
                    let soloed = dependency.soloed;
                    let muted = dependency.muted;
                    let gain_mode = if dependency.compact_gain {
                        GainControlMode::Knob
                    } else {
                        GainControlMode::Fader
                    };
                    let base_strip = strip_panel(
                        strip_shell(
                            bus_title_content(
                                bus_id,
                                &name,
                                dependency.renaming,
                                &dependency.rename_value,
                            ),
                            None,
                            Some(route_picker(
                                RoutingStrip::Bus(bus_id),
                                dependency.route.clone(),
                                dependency.route_choices.clone(),
                                controls_enabled,
                            )),
                            gain_db,
                            pan,
                            meter_stack(
                                meter_dependency,
                                Some(if controls_enabled {
                                    Message::Mixer(MixerMessage::ResetBusMeter(bus_id))
                                } else {
                                    noop_message()
                                }),
                            ),
                            StripActions {
                                panel: Some((
                                    dependency.effect_rack_open,
                                    dependency.panel_has_content,
                                    if controls_enabled {
                                        Message::Mixer(MixerMessage::ToggleMixerEffectRack(
                                            strip_index,
                                        ))
                                    } else {
                                        noop_message()
                                    },
                                )),
                                solo: Some((
                                    soloed,
                                    if controls_enabled {
                                        Message::Mixer(MixerMessage::ToggleBusSolo(bus_id))
                                    } else {
                                        noop_message()
                                    },
                                )),
                                mute: Some((
                                    muted,
                                    if controls_enabled {
                                        Message::Mixer(MixerMessage::ToggleBusMute(bus_id))
                                    } else {
                                        noop_message()
                                    },
                                )),
                                on_gain: Some(Box::new(move |value| {
                                    if controls_enabled {
                                        Message::Mixer(MixerMessage::SetBusGain(bus_id, value))
                                    } else {
                                        noop_message()
                                    }
                                })),
                                on_pan: Some(Box::new(move |value| {
                                    if controls_enabled {
                                        Message::Mixer(MixerMessage::SetBusPan(bus_id, value))
                                    } else {
                                        noop_message()
                                    }
                                })),
                            },
                            strip_height,
                            gain_mode,
                            true,
                        ),
                        STRIP_WIDTH,
                        strip_height,
                        false,
                        None,
                    );
                    let remove_button: Element<'static, Message> = container(
                        ui_style::flat_icon_button(
                            icons::x(),
                            ui_style::grid_f32(4),
                            ui_style::grid_f32(3),
                            ui_style::button_flat_compact_control,
                            ui_style::svg_dimmed_control,
                        )
                        .on_press(Message::Mixer(MixerMessage::RemoveBus(bus_id))),
                    )
                    .width(Fill)
                    .height(Fill)
                    .align_x(alignment::Horizontal::Right)
                    .align_y(alignment::Vertical::Top)
                    .padding([ui_style::grid(2), ui_style::grid(2)])
                    .into();
                    let layered: Element<'static, Message> =
                        stack([base_strip, remove_button]).into();
                    layered
                },
            ));
            if effect_rack_open {
                row.push(track_effect_rack_panel(
                    strip_index,
                    effects.clone(),
                    Some(EffectRackPanelRouting {
                        source: RoutingStrip::Bus(bus_id.0),
                        sends: send_dependencies(&bus.routing),
                        send_choices: send_destination_choices(mixer, RoutingStrip::Bus(bus_id.0)),
                    }),
                    hovered_processor_slot,
                    effect_rack_drag_state(effect_drag_source, effect_drag_target, strip_index),
                    controls_enabled,
                    strip_height,
                ))
            } else {
                row
            }
        },
    );
    let bus_row = if visible.end >= total_buses {
        bus_row.push(add_bus_lane(strip_height, controls_enabled))
    } else {
        bus_row
    };
    let bus_row = bus_row.push(horizontal_spacer(right_spacer));

    column![
        container(section_header_bar(row![section_title("Buses")]))
            .style(ui_style::workspace_toolbar_surface),
        container(text("")).height(Length::Fixed(SECTION_BODY_GAP)),
        row![
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
            scrollable(bus_row)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new()
                ))
                .on_scroll(|viewport| Message::Mixer(MixerMessage::BusViewportScrolled(viewport)))
                .style(ui_style::workspace_scrollable)
                .width(Fill)
                .height(Fill),
            container(text(""))
                .width(Length::Fixed(GROUP_SIDE_BORDER_WIDTH))
                .height(Fill)
                .style(ui_style::chrome_separator),
        ]
        .height(Fill),
        container(text(""))
            .width(Fill)
            .height(Length::Fixed(1.0))
            .style(ui_style::chrome_separator)
    ]
    .spacing(0)
    .height(Fill)
    .into()
}

fn add_bus_button(controls_enabled: bool) -> Element<'static, Message> {
    ui_style::flat_icon_button(
        icons::plus(),
        ui_style::grid_f32(7),
        ui_style::grid_f32(4),
        ui_style::button_flat_compact_control,
        ui_style::svg_muted_control,
    )
    .on_press_maybe(Some(if controls_enabled {
        Message::Mixer(MixerMessage::AddBus)
    } else {
        noop_message()
    }))
    .into()
}

fn add_bus_lane(strip_height: f32, controls_enabled: bool) -> Element<'static, Message> {
    container(add_bus_button(controls_enabled))
        .width(Length::Fixed(STRIP_WIDTH))
        .height(Length::Fixed(strip_height))
        .center_x(Length::Fixed(STRIP_WIDTH))
        .center_y(Length::Fixed(strip_height))
        .into()
}

fn section_title<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(
        text(label.into())
            .size(ui_style::FONT_SIZE_UI_SM)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..fonts::UI
            }),
    )
    .height(Length::Fixed(SECTION_HEADER_HEIGHT))
    .center_y(Length::Fixed(SECTION_HEADER_HEIGHT))
    .into()
}

fn route_picker(
    source: RoutingStrip,
    selected: RouteChoice,
    choices: Vec<RouteChoice>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    if choices.len() <= 1 {
        return route_picker_placeholder(selected.label);
    }
    let menu_height = route_menu_height_for_items(choices.len());

    let label = route_label(&selected.label);
    let pick_list = pick_list(choices, Some(selected), move |choice| {
        if controls_enabled {
            Message::Mixer(MixerMessage::SetMainRoute(source, choice.route))
        } else {
            noop_message()
        }
    })
    .placeholder("Master")
    .width(Fill)
    .menu_height(Length::Fixed(menu_height))
    .padding([ui_style::grid(1), ui_style::grid(2)])
    .text_size(ui_style::FONT_SIZE_UI_XS)
    .font(fonts::UI)
    .style(route_pick_list_centered_style)
    .menu_style(route_pick_list_menu_style);

    stack![
        pick_list,
        route_picker_centered_label(label, button::Status::Active)
    ]
    .into()
}

fn route_picker_placeholder(label: String) -> Element<'static, Message> {
    container(route_picker_centered_label(
        route_label(&label),
        button::Status::Disabled,
    ))
    .width(Fill)
    .height(Length::Fixed(ROUTE_PICKER_HEIGHT))
    .style(route_picker_placeholder_surface)
    .into()
}

fn route_picker_centered_label(label: String, status: button::Status) -> Element<'static, Message> {
    container(
        text(label)
            .size(ui_style::FONT_SIZE_UI_XS)
            .align_x(alignment::Horizontal::Center)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .width(Fill)
    .height(Length::Fixed(ROUTE_PICKER_HEIGHT))
    .padding([0, ui_style::grid(4)])
    .center_x(Fill)
    .center_y(Length::Fixed(ROUTE_PICKER_HEIGHT))
    .style(move |theme| {
        let button = ui_style::button_selector_field(theme, status, false);
        container::Style {
            text_color: Some(button.text_color),
            ..container::Style::default()
        }
    })
    .into()
}

fn route_label(label: &str) -> String {
    crate::track_names::ellipsize_middle(label, ROUTE_PICKER_MAX_LEN)
}

fn route_pick_list_style(
    theme: &iced::Theme,
    status: iced::widget::pick_list::Status,
) -> iced::widget::pick_list::Style {
    let open = matches!(status, iced::widget::pick_list::Status::Opened { .. });
    let button_status = match status {
        iced::widget::pick_list::Status::Active => button::Status::Active,
        iced::widget::pick_list::Status::Hovered => button::Status::Hovered,
        iced::widget::pick_list::Status::Opened { is_hovered } => {
            if is_hovered {
                button::Status::Hovered
            } else {
                button::Status::Active
            }
        }
    };
    let button = ui_style::button_selector_field(theme, button_status, open);
    let palette = theme.extended_palette();
    iced::widget::pick_list::Style {
        text_color: button.text_color,
        placeholder_color: button.text_color,
        handle_color: palette.background.weak.text,
        background: button
            .background
            .unwrap_or(palette.background.weak.color.into()),
        border: button.border,
    }
}

fn route_pick_list_centered_style(
    theme: &iced::Theme,
    status: iced::widget::pick_list::Status,
) -> iced::widget::pick_list::Style {
    let mut style = route_pick_list_style(theme, status);
    style.text_color = Color::TRANSPARENT;
    style.placeholder_color = Color::TRANSPARENT;
    style
}

fn route_pick_list_menu_style(theme: &iced::Theme) -> iced::widget::overlay::menu::Style {
    let palette = theme.extended_palette();
    iced::widget::overlay::menu::Style {
        background: palette.background.weak.color.into(),
        border: border::rounded(ui_style::RADIUS_UI)
            .width(1)
            .color(palette.background.strong.color),
        text_color: palette.background.weak.text,
        selected_text_color: palette.background.strong.text,
        selected_background: palette.background.strong.color.into(),
        shadow: iced::Shadow::default(),
    }
}

fn route_picker_placeholder_surface(theme: &iced::Theme) -> container::Style {
    let button = ui_style::button_selector_field(theme, button::Status::Disabled, false);
    container::Style {
        text_color: Some(button.text_color),
        background: button.background,
        border: button.border,
        shadow: button.shadow,
        ..container::Style::default()
    }
}

fn strip_processor_header(
    _strip_index: usize,
    instrument: Option<(
        super::processor_editor_windows::EditorTarget,
        Option<&ProcessorChoice>,
        bool,
        Option<ProcessorSlotSegment>,
    )>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let mut content = row![]
        .spacing(ui_style::SPACE_XS)
        .align_y(alignment::Vertical::Center);
    if let Some((target, selected, editor_enabled, hovered_segment)) = instrument {
        content = content.push(processor_slot_controls(
            target,
            ProcessorSlotRole::Instrument,
            selected,
            editor_enabled,
            false,
            hovered_segment,
            controls_enabled,
        ));
    } else {
        content = content.push(container(text("")).width(Length::Fixed(PROCESSOR_SLOT_WIDTH)));
    }
    content.into()
}

fn visible_strip_window(
    total: usize,
    scroll_x: f32,
    viewport_width: f32,
) -> std::ops::Range<usize> {
    if total == 0 {
        return 0..0;
    }

    let stride = STRIP_WIDTH + STRIP_SPACING;
    let first_visible = (scroll_x.max(0.0) / stride.max(1.0)).floor() as usize;
    let visible_count = ((viewport_width.max(stride) / stride.max(1.0)).ceil() as usize)
        .saturating_add(STRIP_VIRTUALIZATION_OVERSCAN * 2);
    let start = first_visible
        .saturating_sub(STRIP_VIRTUALIZATION_OVERSCAN)
        .min(total);
    let end = start.saturating_add(visible_count).min(total);
    start..end
}

fn strip_span_width(count: usize) -> f32 {
    if count == 0 {
        0.0
    } else {
        count as f32 * STRIP_WIDTH + count.saturating_sub(1) as f32 * STRIP_SPACING
    }
}

fn horizontal_spacer(width: f32) -> Element<'static, Message> {
    container(text(""))
        .width(Length::Fixed(width.max(0.0)))
        .height(Fill)
        .into()
}

fn section_header_bar<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(row![
        container(text("")).width(Length::Fixed(HEADER_SIDE_INSET)),
        container(content.into())
            .height(Length::Fixed(SECTION_HEADER_HEIGHT))
            .center_y(Length::Fixed(SECTION_HEADER_HEIGHT))
    ])
    .align_y(alignment::Vertical::Center)
    .height(Length::Fixed(SECTION_HEADER_HEIGHT))
    .width(Fill)
    .center_y(Length::Fixed(SECTION_HEADER_HEIGHT))
    .into()
}

fn value_label_slot<'a>(
    width: f32,
    label: impl Into<String>,
    color: Option<iced::Color>,
) -> Element<'a, Message> {
    let text = text(label.into()).size(ui_style::FONT_SIZE_UI_XS.saturating_sub(1));
    let text = if let Some(color) = color {
        text.color(color)
    } else {
        text
    };

    container(text)
        .width(Length::Fixed(width))
        .height(Length::Fixed(VALUE_LABEL_HEIGHT))
        .center_x(Length::Fixed(width))
        .align_y(alignment::Vertical::Bottom)
        .into()
}

fn gain_label(gain_db: f32) -> String {
    if gain_db <= GAIN_MIN_DB {
        "-inf".to_string()
    } else {
        format!("{gain_db:.1}")
    }
}

#[allow(clippy::too_many_arguments)]
fn strip_shell<'a>(
    title: Element<'a, Message>,
    instrument_picker: Option<Element<'a, Message>>,
    route_picker: Option<Element<'a, Message>>,
    gain_db: f32,
    pan: f32,
    meter_stack: Element<'a, Message>,
    actions: StripActions<'a>,
    strip_height: f32,
    gain_mode: GainControlMode,
    show_gain_scale: bool,
) -> Element<'a, Message> {
    let mut content = column![]
        .spacing(STRIP_STACK_SPACING)
        .align_x(alignment::Horizontal::Center)
        .width(Fill);

    content = content.push(
        container(instrument_picker.unwrap_or_else(|| container(text("")).into()))
            .width(Fill)
            .height(Length::Fixed(INSTRUMENT_PICKER_HEIGHT)),
    );

    if let Some(on_pan) = actions.on_pan {
        content = content.push(
            column![
                container(text("")).height(Length::Fixed(ui_style::SPACE_XS as f32)),
                value_label_slot(INSTRUMENT_PICKER_HEIGHT, format!("{:+.2}", pan), None),
                pan_knob(pan, on_pan),
            ]
            .spacing(LABEL_CONTROL_SPACING)
            .align_x(alignment::Horizontal::Center),
        );
    }

    if let Some(on_gain) = actions.on_gain {
        let control_height = gain_control_height(strip_height, gain_mode);
        let gain_width = gain_control_width(matches!(gain_mode, GainControlMode::Knob));
        let stack_height = control_stack_height(control_height);

        let gain_control = match gain_mode {
            GainControlMode::Fader => container(gain_fader(gain_db, on_gain))
                .width(Fill)
                .height(Length::Fixed(control_height))
                .center_x(Fill)
                .into(),
            GainControlMode::Knob => gain_knob(gain_db, on_gain),
        };

        let gain_column = column![
            value_label_slot(gain_width, gain_label(gain_db), None),
            container(gain_control)
                .width(Length::Fixed(gain_width))
                .height(Length::Fixed(control_height))
                .center_x(Length::Fixed(gain_width))
                .align_y(alignment::Vertical::Bottom),
        ]
        .spacing(LABEL_CONTROL_SPACING)
        .height(Length::Fixed(stack_height))
        .align_x(alignment::Horizontal::Center)
        .width(Length::Shrink);

        let gain_controls: Element<'a, Message> =
            if matches!(gain_mode, GainControlMode::Fader) && show_gain_scale {
                row![
                    column![
                        container(text("")).height(Length::Fixed(VALUE_LABEL_HEIGHT)),
                        gain_fader_scale(control_height),
                    ]
                    .spacing(LABEL_CONTROL_SPACING)
                    .height(Length::Fixed(stack_height))
                    .width(Length::Fixed(gain_fader_scale_width())),
                    gain_column,
                ]
                .spacing(GAIN_SCALE_SPACING)
                .height(Length::Fixed(stack_height))
                .width(Length::Shrink)
                .into()
            } else {
                gain_column.into()
            };

        content = content.push(
            row![gain_controls, meter_stack]
                .spacing(METER_STACK_SPACING)
                .height(Length::Fixed(stack_height))
                .width(Length::Shrink),
        );
    }

    content = content.push(
        container(
            row![
                actions
                    .mute
                    .map_or_else(strip_toggle_placeholder, |(active, message)| {
                        strip_toggle_button("M", active, message)
                    },),
                actions
                    .solo
                    .map_or_else(strip_toggle_placeholder, |(active, message)| {
                        strip_toggle_button("S", active, message)
                    },),
                actions.panel.map_or_else(
                    strip_toggle_placeholder,
                    |(active, has_content, message)| {
                        strip_panel_toggle_button(active, has_content, message)
                    },
                ),
            ]
            .spacing(ui_style::SPACE_XS)
            .align_y(alignment::Vertical::Center),
        )
        .width(Fill)
        .center_x(Fill),
    );

    let title = container(title)
        .width(Fill)
        .height(Length::Fixed(TRACK_TITLE_EDITOR_HEIGHT))
        .align_y(alignment::Vertical::Center);

    container(
        column![
            container(content).width(Fill),
            container(text("")).height(Length::Fixed(TITLE_TOP_SPACING)),
            title,
            container(text("")).height(Length::Fixed(ROUTE_PICKER_TOP_SPACING)),
            container(route_picker.unwrap_or_else(|| container(text("")).into()))
                .width(Fill)
                .height(Length::Fixed(ROUTE_PICKER_HEIGHT))
                .center_y(Length::Fixed(ROUTE_PICKER_HEIGHT)),
            container(text("")).height(Length::Fixed(ROUTE_PICKER_BOTTOM_INSET)),
        ]
        .spacing(0)
        .width(Fill),
    )
    .padding(ui_style::PADDING_SM)
    .width(Fill)
    .height(Length::Fixed(strip_height))
    .style(ui_style::transparent_surface)
    .into()
}

fn track_title_content<'a>(
    track_index: usize,
    title: &str,
    renaming: bool,
    rename_value: &str,
    color: Color,
    color_picker_open: bool,
) -> Element<'a, Message> {
    if renaming {
        let swatch = button(
            container(text(""))
                .width(Length::Fixed(TRACK_TITLE_EDITOR_SWATCH_SIZE))
                .height(Length::Fixed(TRACK_TITLE_EDITOR_SWATCH_SIZE)),
        )
        .padding(0)
        .width(Length::Fixed(TRACK_TITLE_EDITOR_SWATCH_SIZE))
        .height(Length::Fixed(TRACK_TITLE_EDITOR_CONTROL_HEIGHT))
        .style(move |theme, status| ui_style::track_color_swatch_button(theme, status, color))
        .on_press(Message::Mixer(MixerMessage::OpenTrackColorPicker));
        let input = text_input::<Message, iced::Theme, iced::Renderer>("", rename_value)
            .id(Id::new(super::TRACK_RENAME_INPUT_ID))
            .on_input(|value| Message::Mixer(MixerMessage::TrackRenameInputChanged(value)))
            .on_submit(Message::Mixer(MixerMessage::CommitTrackRename))
            .style(ui_style::track_name_input)
            .size(ui_style::FONT_SIZE_UI_SM)
            .padding([
                TRACK_TITLE_EDITOR_INPUT_PADDING_V,
                TRACK_TITLE_EDITOR_INPUT_PADDING_H,
            ])
            .width(Fill);
        let focused = color_picker_open;
        let editor_row = container(
            row![
                swatch,
                container(text(""))
                    .width(1)
                    .height(Length::Fixed(TRACK_TITLE_EDITOR_CONTROL_HEIGHT))
                    .style(move |theme| { ui_style::track_name_editor_divider(theme, focused) }),
                container(input)
                    .height(Length::Fixed(TRACK_TITLE_EDITOR_CONTROL_HEIGHT))
                    .center_y(Length::Fixed(TRACK_TITLE_EDITOR_CONTROL_HEIGHT))
            ]
            .spacing(0)
            .align_y(alignment::Vertical::Center)
            .width(Fill),
        )
        .padding(0)
        .height(Length::Fixed(TRACK_TITLE_EDITOR_HEIGHT))
        .style(move |theme| ui_style::track_name_editor_shell(theme, focused))
        .width(Fill);
        return color_picker_with_change(
            color_picker_open,
            color,
            editor_row,
            Message::Mixer(MixerMessage::CancelTrackRename),
            |color| Message::Mixer(MixerMessage::SubmitTrackColor(color)),
            |color| Message::Mixer(MixerMessage::PreviewTrackColor(color)),
        )
        .style(ui_style::color_picker_widget_style)
        .into();
    }

    mouse_area(
        container(
            text(crate::track_names::ellipsize_middle(title, 18))
                .size(ui_style::FONT_SIZE_UI_SM)
                .font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..fonts::UI
                })
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Fill)
        .center_x(Fill),
    )
    .on_press(Message::Mixer(MixerMessage::StartTrackRename(track_index)))
    .into()
}

fn bus_title_content<'a>(
    bus_id: u16,
    title: &str,
    renaming: bool,
    rename_value: &str,
) -> Element<'a, Message> {
    if renaming {
        return container(
            text_input::<Message, iced::Theme, iced::Renderer>("", rename_value)
                .id(Id::new(super::TRACK_RENAME_INPUT_ID))
                .on_input(|value| Message::Mixer(MixerMessage::TrackRenameInputChanged(value)))
                .on_submit(Message::Mixer(MixerMessage::CommitTrackRename))
                .size(ui_style::FONT_SIZE_UI_SM)
                .padding([
                    TRACK_TITLE_EDITOR_INPUT_PADDING_V,
                    TRACK_TITLE_EDITOR_INPUT_PADDING_H,
                ])
                .width(Fill),
        )
        .height(Length::Fixed(TRACK_TITLE_EDITOR_HEIGHT))
        .center_y(Length::Fixed(TRACK_TITLE_EDITOR_HEIGHT))
        .into();
    }

    mouse_area(
        container(
            text(crate::track_names::ellipsize_middle(title, 18))
                .size(ui_style::FONT_SIZE_UI_SM)
                .font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..fonts::UI
                })
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Fill)
        .center_x(Fill),
    )
    .on_press(Message::Mixer(MixerMessage::StartBusRename(bus_id)))
    .into()
}

fn meter_stack<'a>(
    dependency: MeterStackDependency,
    meter_reset: Option<Message>,
) -> Element<'a, Message> {
    lazy(dependency, move |dependency| -> Element<'static, Message> {
        let gain_mode = if dependency.compact_gain {
            GainControlMode::Knob
        } else {
            GainControlMode::Fader
        };
        let strip_height = f32::from_bits(dependency.strip_height_bits);
        let meter_snapshot = dependency.meter.snapshot();
        let meter_colors = dependency.colors.colors();
        let meter_height = meter_control_height(strip_height, gain_mode);
        let meter_width = stereo_meter_width(meter_scale_visible(gain_mode));
        let meter_bar_width = stereo_meter_bar_width();
        let meter_scale_width = (meter_width - meter_bar_width).max(0.0);
        let meter_label = meter_peak_label(meter_snapshot);
        let meter_label_color = if meter_snapshot.clip_latched {
            meter_colors.clip
        } else {
            meter_colors.scale_text
        };
        let meter = if meter_scale_visible(gain_mode) {
            stereo_meter_with_scale(meter_snapshot, meter_colors, meter_height)
        } else {
            stereo_meter(meter_snapshot, meter_colors, meter_height)
        };
        let meter = if let Some(message) = meter_reset.clone() {
            mouse_area(meter).on_press(message).into()
        } else {
            meter
        };

        column![
            row![
                value_label_slot(meter_bar_width, meter_label, Some(meter_label_color)),
                container(text("")).width(Length::Fixed(meter_scale_width)),
            ]
            .width(Length::Fixed(meter_width))
            .height(Length::Fixed(VALUE_LABEL_HEIGHT))
            .align_y(alignment::Vertical::Bottom),
            container(meter)
                .width(Length::Fixed(meter_width))
                .height(Length::Fixed(meter_height))
                .center_x(Length::Fixed(meter_width))
                .align_y(alignment::Vertical::Bottom),
        ]
        .spacing(LABEL_CONTROL_SPACING)
        .height(Length::Fixed(control_stack_height(meter_height)))
        .align_x(alignment::Horizontal::Center)
        .width(Length::Shrink)
        .into()
    })
    .into()
}

fn gain_control_height(strip_height: f32, gain_mode: GainControlMode) -> f32 {
    match gain_mode {
        GainControlMode::Knob => 48.0,
        GainControlMode::Fader => (strip_height
            - (ui_style::PADDING_SM as f32 * 2.0)
            - SECTION_HEADER_HEIGHT
            - INSTRUMENT_PICKER_HEIGHT
            - STRIP_TOGGLE_SIZE
            - STRIP_FOOTER_HEIGHT
            - 30.0
            - (VALUE_LABEL_HEIGHT * 3.0)
            - (ui_style::SPACE_XS as f32 * 6.0))
            .max(96.0),
    }
}

fn meter_control_height(strip_height: f32, gain_mode: GainControlMode) -> f32 {
    gain_control_height(strip_height, gain_mode)
}

fn control_stack_height(control_height: f32) -> f32 {
    control_height + VALUE_LABEL_HEIGHT + ui_style::SPACE_XS as f32
}

fn meter_peak_label(snapshot: StripMeterSnapshot) -> String {
    let hold_db = snapshot.left.hold_db.max(snapshot.right.hold_db);
    if hold_db <= STRIP_METER_MIN_DB {
        "-inf".to_string()
    } else {
        format!("{hold_db:.1}")
    }
}

fn gain_control_mode(pane_height: f32) -> GainControlMode {
    if pane_height <= MIXER_MIN_HEIGHT + COMPACT_GAIN_SWITCH_OFFSET {
        GainControlMode::Knob
    } else {
        GainControlMode::Fader
    }
}

fn meter_scale_visible(gain_mode: GainControlMode) -> bool {
    matches!(gain_mode, GainControlMode::Fader)
}

fn strip_panel<'a>(
    content: Element<'a, Message>,
    width: f32,
    height: f32,
    selected: bool,
    on_select: Option<Message>,
) -> Element<'a, Message> {
    let content: Element<'a, Message> = if let Some(message) = on_select {
        stack![
            mouse_area(container(text("")).width(Fill).height(Fill)).on_press(message),
            content
        ]
        .into()
    } else {
        content
    };

    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .style(move |theme| ui_style::mixer_track_strip_surface(theme, None, selected))
        .into()
}

fn tinted_track_strip_panel<'a>(
    content: Element<'a, Message>,
    width: f32,
    height: f32,
    track_color: Color,
    selected: bool,
    on_select: Option<Message>,
) -> Element<'a, Message> {
    let content: Element<'a, Message> = if let Some(message) = on_select {
        stack![
            mouse_area(container(text("")).width(Fill).height(Fill)).on_press(message),
            content
        ]
        .into()
    } else {
        content
    };

    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .style(move |theme| ui_style::mixer_track_strip_surface(theme, Some(track_color), selected))
        .into()
}

fn track_should_use_roll_tint(track_index: usize, existing_track_count: usize) -> bool {
    track_index < existing_track_count
}

fn strip_toggle_button(
    label: &'static str,
    active: bool,
    message: Message,
) -> Element<'static, Message> {
    button(
        container(text(label).size(ui_style::FONT_SIZE_UI_XS))
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill),
    )
    .style(if active {
        ui_style::button_compact_active
    } else {
        ui_style::button_compact_solid
    })
    .padding(0)
    .width(Length::Fixed(STRIP_TOGGLE_SIZE))
    .height(Length::Fixed(STRIP_TOGGLE_SIZE))
    .on_press(message)
    .into()
}

fn strip_panel_toggle_button(
    active: bool,
    has_content: bool,
    message: Message,
) -> Element<'static, Message> {
    button(
        container(ui_style::icon(
            icons::cable(),
            ui_style::grid_f32(3),
            move |theme, status| strip_panel_toggle_icon_style(theme, status, active, has_content),
        ))
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill),
    )
    .style(move |theme, status| strip_panel_toggle_style(theme, status, active))
    .padding(0)
    .width(Length::Fixed(STRIP_TOGGLE_SIZE))
    .height(Length::Fixed(STRIP_TOGGLE_SIZE))
    .on_press(message)
    .into()
}

fn strip_panel_toggle_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
    active: bool,
    has_content: bool,
) -> iced::widget::svg::Style {
    if active || has_content {
        return processor_slot_icon_style(theme, status, true);
    }

    processor_slot_icon_style(theme, status, false)
}

fn strip_panel_toggle_style(
    theme: &iced::Theme,
    status: button::Status,
    active: bool,
) -> button::Style {
    if active {
        return ui_style::button_flat_compact_control(
            theme,
            if matches!(status, button::Status::Disabled) {
                status
            } else {
                button::Status::Hovered
            },
        );
    }
    ui_style::button_flat_compact_control(theme, status)
}

fn strip_toggle_placeholder() -> Element<'static, Message> {
    container(text(""))
        .width(Length::Fixed(STRIP_TOGGLE_SIZE))
        .height(Length::Fixed(STRIP_TOGGLE_SIZE))
        .into()
}

#[allow(dead_code)]
fn instrument_slot_controls(
    track_index: usize,
    selected: Option<ProcessorChoice>,
    editor_enabled: bool,
    controls_enabled: bool,
) -> Element<'static, Message> {
    processor_slot_controls(
        super::processor_editor_windows::EditorTarget {
            strip_index: track_index + 1,
            slot_index: 0,
        },
        ProcessorSlotRole::Instrument,
        selected.as_ref(),
        editor_enabled,
        false,
        None,
        controls_enabled,
    )
}

#[cfg(test)]
fn slot_selector_controls(
    label: String,
    primary_action: Option<Message>,
    secondary_action: Option<Message>,
) -> Element<'static, Message> {
    let editor_segment = button(
        container(ui_style::icon(
            icons::keyboard_music(),
            INSTRUMENT_BROWSER_ICON_SIZE,
            ui_style::svg_muted_control,
        ))
        .width(Length::Fixed(INSTRUMENT_SLOT_EDITOR_AREA_WIDTH))
        .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
        .center_x(Length::Fixed(INSTRUMENT_SLOT_EDITOR_AREA_WIDTH))
        .center_y(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT)),
    )
    .style(transparent_hit_button)
    .padding(0)
    .width(Length::Fixed(INSTRUMENT_SLOT_EDITOR_AREA_WIDTH))
    .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
    .on_press_maybe(primary_action);

    let picker_segment = button(
        container(
            text(label)
                .size(ui_style::FONT_SIZE_UI_XS)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Fill)
        .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
        .clip(true)
        .center_x(Fill)
        .align_y(alignment::Vertical::Center),
    )
    .style(transparent_hit_button)
    .padding(0)
    .width(Fill)
    .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
    .on_press_maybe(secondary_action.clone());

    let surface: Element<'static, Message> = button(
        container(
            row![
                editor_segment,
                slot_area_separator(),
                picker_segment,
                container(text(""))
                    .width(Length::Fixed(INSTRUMENT_BROWSER_ICON_SIZE))
                    .height(Fill),
            ]
            .spacing(ui_style::SPACE_XS)
            .align_y(alignment::Vertical::Center),
        )
        .width(Fill)
        .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
        .center_x(Fill)
        .center_y(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT)),
    )
    .style(|theme, status| ui_style::button_selector_field(theme, status, false))
    .padding([0, ui_style::grid(2)])
    .width(Length::Fixed(INSTRUMENT_SLOT_WIDTH))
    .height(Length::Fixed(INSTRUMENT_SLOT_BUTTON_HEIGHT))
    .on_press(Message::Noop)
    .into();

    let button = if let Some(message) = secondary_action {
        mouse_area(surface).on_right_press(message).into()
    } else {
        surface
    };

    container(button)
        .width(Fill)
        .height(Length::Fixed(INSTRUMENT_PICKER_HEIGHT))
        .center_x(Fill)
        .center_y(Length::Fixed(INSTRUMENT_PICKER_HEIGHT))
        .into()
}

fn effect_rack(
    strip_index: usize,
    effects: Vec<EffectSlotDependency>,
    hovered_processor_slot: Option<(usize, ProcessorSlotSegment)>,
    effect_drag: Option<EffectRackDragState>,
    controls_enabled: bool,
    min_slots: usize,
) -> Element<'static, Message> {
    let indicator = effect_rack_drop_indicator(effect_drag);
    let labels = effect_slot_display_labels(&effects);
    let mut content = column![].spacing(0).width(Fill);
    if indicator == Some(EffectRackDropIndicator::Top) {
        content = content.push(effect_rack_drop_indicator_line());
    }
    for (effect, label) in effects.iter().zip(labels) {
        let effect_index = effect.slot_index - 1;
        let target = super::processor_editor_windows::EditorTarget {
            strip_index,
            slot_index: effect.slot_index,
        };
        content = content.push(effect_rack_draggable_filled_slot(
            target,
            processor_slot_controls_sized(
                target,
                ProcessorSlotRole::Effect,
                effect.selected.as_ref(),
                effect.editor_enabled,
                effect.bypassed,
                hovered_processor_slot
                    .filter(|(slot_index, _)| *slot_index == effect.slot_index)
                    .map(|(_, segment)| segment),
                controls_enabled,
                EFFECT_RACK_SLOT_WIDTH,
                true,
                label,
            ),
            indicator == Some(EffectRackDropIndicator::After(effect_index)),
            controls_enabled,
        ));
    }

    let slot_count = min_slots.max(effects.len() + 1);
    for slot_index in effects.len() + 1..=slot_count {
        let add_target = super::processor_editor_windows::EditorTarget {
            strip_index,
            slot_index,
        };
        let first_empty = slot_index == effects.len() + 1;
        content = content.push(effect_rack_empty_slot(
            add_target,
            first_empty,
            controls_enabled,
        ));
    }

    let rack: Element<'static, Message> = container(
        scrollable(content)
            .id(effect_rack_scroll_id(strip_index))
            .width(Fill)
            .on_scroll(move |viewport| {
                Message::Mixer(MixerMessage::EffectRackViewportScrolled {
                    strip_index,
                    viewport,
                })
            })
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(EFFECT_RACK_SCROLLBAR_WIDTH)
                    .scroller_width(EFFECT_RACK_SCROLLBAR_SCROLLER_WIDTH)
                    .spacing(EFFECT_RACK_SCROLLBAR_SPACING)
                    .margin(EFFECT_RACK_SCROLLBAR_MARGIN),
            ))
            .style(effect_rack_scrollable),
    )
    .width(Fill)
    .height(Fill)
    .style(effect_rack_surface)
    .into();

    container(
        mouse_area(rack)
            .on_move(move |position| {
                Message::Mixer(MixerMessage::TrackEffectDragMoved {
                    strip_index,
                    y: position.y,
                })
            })
            .on_exit(Message::Mixer(MixerMessage::EffectRackCursorLeft(
                strip_index,
            ))),
    )
    .width(Fill)
    .height(Fill)
    .into()
}

fn send_panel(
    source: RoutingStrip,
    sends: Vec<SendDependency>,
    choices: Vec<SendDestinationChoice>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let mut content = column![
        container(text("")).height(Length::Fixed(SEND_PANEL_TOP_SPACING)),
        container(
            row![
                text("Sends")
                    .size(ui_style::FONT_SIZE_UI_XS)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..fonts::UI
                    }),
                container(text("")).width(Fill),
                send_add_button(source, first_send_bus_id(&choices), controls_enabled)
            ]
            .align_y(alignment::Vertical::Center)
        )
        .height(Length::Fixed(SEND_PANEL_HEADER_HEIGHT))
        .padding([0, ui_style::grid(2)])
        .center_y(Length::Fixed(SEND_PANEL_HEADER_HEIGHT))
    ]
    .spacing(0)
    .width(Fill);

    if choices.is_empty() {
        content = content.push(
            container(text("No buses").size(ui_style::FONT_SIZE_UI_XS))
                .width(Fill)
                .height(Fill)
                .center_x(Fill)
                .center_y(Fill),
        );
    } else {
        for (index, send) in sends.into_iter().enumerate() {
            content = content.push(send_row(
                source,
                index,
                send,
                choices.clone(),
                controls_enabled,
            ));
        }
    }

    container(scrollable(content).style(effect_rack_scrollable))
        .width(Fill)
        .height(Fill)
        .style(effect_rack_surface)
        .into()
}

fn send_add_button(
    source: RoutingStrip,
    first_bus_id: Option<u16>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    ui_style::flat_icon_button(
        icons::plus(),
        ui_style::grid_f32(4),
        ui_style::grid_f32(3),
        ui_style::button_flat_compact_control,
        ui_style::svg_dimmed_control,
    )
    .width(Length::Fixed(ui_style::grid_f32(5)))
    .height(Length::Fixed(ui_style::grid_f32(5)))
    .on_press_maybe(
        (controls_enabled)
            .then_some(first_bus_id)
            .flatten()
            .map(|bus_id| Message::Mixer(MixerMessage::AddSend(source, bus_id))),
    )
    .into()
}

fn send_row(
    source: RoutingStrip,
    index: usize,
    send: SendDependency,
    choices: Vec<SendDestinationChoice>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let choices = send_menu_choices(choices);
    let selected = selected_send_destination_choice(send.bus_id, &choices);
    let enabled = send.enabled;
    let pre_fader = send.pre_fader;
    let gain = f32::from_bits(send.gain_bits);
    let gain_text = gain_label(gain);

    let content_height = SEND_ROW_HEIGHT - EFFECT_RACK_SEPARATOR_HEIGHT;
    container(
        column![
            container(
                column![
                    row![
                        send_icon_button(
                            if enabled {
                                icons::power()
                            } else {
                                icons::power_off()
                            },
                            Message::Mixer(MixerMessage::ToggleSendEnabled(source, index)),
                            controls_enabled,
                        ),
                        container(
                            pick_list(choices.clone(), selected, move |choice| {
                                if !controls_enabled {
                                    return noop_message();
                                }
                                match choice.action {
                                    SendDestinationAction::Route(bus_id) => Message::Mixer(
                                        MixerMessage::SetSendDestination(source, index, bus_id),
                                    ),
                                    SendDestinationAction::Remove => {
                                        Message::Mixer(MixerMessage::RemoveSend(source, index))
                                    }
                                }
                            })
                            .placeholder("Bus")
                            .width(Length::Fixed(SEND_PICKER_WIDTH))
                            .menu_height(Length::Fixed(route_menu_height_for_items(choices.len())))
                            .padding([ui_style::grid(1), ui_style::grid(2)])
                            .text_size(ui_style::FONT_SIZE_UI_XS)
                            .font(fonts::UI)
                            .style(route_pick_list_style)
                            .menu_style(route_pick_list_menu_style),
                        )
                        .height(Length::Fixed(SEND_CONTROL_HEIGHT))
                        .center_y(Length::Fixed(SEND_CONTROL_HEIGHT)),
                        send_mode_button(source, index, pre_fader, controls_enabled),
                    ]
                    .spacing(ui_style::SPACE_XS)
                    .align_y(alignment::Vertical::Center),
                    row![
                        send_gain_slider(source, index, gain, controls_enabled),
                        container(text(gain_text).size(ui_style::FONT_SIZE_UI_XS))
                            .width(Length::Fixed(ui_style::grid_f32(7)))
                            .align_y(alignment::Vertical::Center),
                    ]
                    .spacing(ui_style::SPACE_XS)
                    .align_y(alignment::Vertical::Center),
                ]
                .spacing(ui_style::SPACE_XS)
                .align_x(alignment::Horizontal::Center),
            )
            .width(Fill)
            .height(Length::Fixed(content_height))
            .padding(Padding {
                top: 0.0,
                right: ui_style::grid_f32(2),
                bottom: SEND_ROW_CONTENT_BOTTOM_SPACING,
                left: ui_style::grid_f32(2),
            })
            .center_y(Length::Fixed(content_height)),
            effect_rack_separator(),
        ]
        .spacing(0)
        .width(Fill),
    )
    .width(Fill)
    .height(Length::Fixed(SEND_ROW_HEIGHT))
    .into()
}

fn send_gain_slider(
    source: RoutingStrip,
    index: usize,
    gain: f32,
    controls_enabled: bool,
) -> Element<'static, Message> {
    compact_gain_slider(
        gain,
        SEND_GAIN_MIN_DB,
        SEND_GAIN_MAX_DB,
        SEND_GAIN_STEP_DB,
        0.0,
        move |value| {
            if controls_enabled {
                Message::Mixer(MixerMessage::SetSendGain(source, index, value))
            } else {
                noop_message()
            }
        },
    )
}

fn send_icon_button(
    icon: iced::widget::svg::Handle,
    message: Message,
    controls_enabled: bool,
) -> Element<'static, Message> {
    ui_style::flat_icon_button(
        icon,
        ui_style::grid_f32(4),
        ui_style::grid_f32(3),
        ui_style::button_flat_compact_control,
        ui_style::svg_dimmed_control,
    )
    .width(Length::Fixed(ui_style::grid_f32(5)))
    .height(Length::Fixed(ui_style::grid_f32(5)))
    .on_press_maybe(controls_enabled.then_some(message))
    .into()
}

fn send_mode_button(
    source: RoutingStrip,
    index: usize,
    pre_fader: bool,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let label = if pre_fader { "Pre" } else { "Post" };
    button(
        container(
            text(label)
                .size(ui_style::FONT_SIZE_UI_XS)
                .line_height(1.0)
                .align_x(alignment::Horizontal::Center),
        )
        .width(Fill)
        .height(Length::Fixed(SEND_MODE_HEIGHT))
        .center_x(Fill)
        .center_y(Length::Fixed(SEND_MODE_HEIGHT)),
    )
    .style(move |theme, status| ui_style::button_pane_tab(theme, status, pre_fader))
    .padding([0, 0])
    .width(Length::Fixed(SEND_MODE_WIDTH))
    .height(Length::Fixed(SEND_MODE_HEIGHT))
    .on_press_maybe(
        controls_enabled.then_some(Message::Mixer(MixerMessage::ToggleSendPreFader(
            source, index,
        ))),
    )
    .into()
}

fn processor_slot_controls(
    target: super::processor_editor_windows::EditorTarget,
    role: ProcessorSlotRole,
    selected: Option<&ProcessorChoice>,
    editor_enabled: bool,
    bypassed: bool,
    hovered_segment: Option<ProcessorSlotSegment>,
    controls_enabled: bool,
) -> Element<'static, Message> {
    processor_slot_controls_sized(
        target,
        role,
        selected,
        editor_enabled,
        bypassed,
        hovered_segment,
        controls_enabled,
        PROCESSOR_SLOT_WIDTH,
        false,
        None,
    )
}

fn effect_rack_empty_slot(
    target: super::processor_editor_windows::EditorTarget,
    add_slot: bool,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let picker_action = (controls_enabled && add_slot)
        .then_some(Message::Mixer(MixerMessage::ToggleProcessorBrowser(target)));

    if !add_slot {
        return container(effect_rack_separator())
            .width(Length::Fixed(EFFECT_RACK_SLOT_WIDTH))
            .height(Length::Fixed(EFFECT_RACK_ROW_HEIGHT))
            .align_y(alignment::Vertical::Bottom)
            .into();
    }

    let add_button: Element<'static, Message> = button(
        container(
            row![
                container(ui_style::icon(
                    icons::plus(),
                    PROCESSOR_BROWSER_ICON_SIZE,
                    effect_rack_add_icon_style,
                ))
                .width(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
                .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
                .center_x(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
                .center_y(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT)),
                text("Add effect")
                    .size(ui_style::FONT_SIZE_UI_XS)
                    .wrapping(iced::widget::text::Wrapping::None)
            ]
            .spacing(ui_style::SPACE_XS)
            .align_y(alignment::Vertical::Center),
        )
        .width(Fill)
        .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
        .center_x(Fill)
        .center_y(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT)),
    )
    .padding([0, ui_style::grid(2)])
    .style(effect_rack_add_button_style)
    .width(Length::Fixed(EFFECT_RACK_SLOT_WIDTH))
    .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
    .on_press_maybe(picker_action)
    .into();

    effect_rack_filled_slot(add_button)
}

fn effect_rack_filled_slot(content: Element<'static, Message>) -> Element<'static, Message> {
    effect_rack_filled_slot_with_separator(content, effect_rack_separator())
}

fn effect_rack_filled_slot_with_separator(
    content: Element<'static, Message>,
    separator: Element<'static, Message>,
) -> Element<'static, Message> {
    column![content, separator]
        .spacing(0)
        .width(Length::Fixed(EFFECT_RACK_SLOT_WIDTH))
        .into()
}

fn effect_rack_draggable_filled_slot(
    target: super::processor_editor_windows::EditorTarget,
    content: Element<'static, Message>,
    drop_indicator_after: bool,
    controls_enabled: bool,
) -> Element<'static, Message> {
    let effect_index = target.slot_index - 1;
    let separator = if drop_indicator_after {
        effect_rack_drop_indicator_line()
    } else {
        effect_rack_separator()
    };
    let mut row = mouse_area(effect_rack_filled_slot_with_separator(content, separator))
        .on_move(move |position| {
            Message::Mixer(MixerMessage::TrackEffectDragMoved {
                strip_index: target.strip_index,
                y: EFFECT_RACK_ROW_HEIGHT * effect_index as f32 + position.y,
            })
        })
        .interaction(mouse::Interaction::Pointer);
    if controls_enabled {
        row = row
            .on_enter(Message::Mixer(MixerMessage::TrackEffectDragMoved {
                strip_index: target.strip_index,
                y: EFFECT_RACK_ROW_HEIGHT * (effect_index as f32 + 0.5),
            }))
            .on_press(Message::Mixer(MixerMessage::StartTrackEffectDrag {
                strip_index: target.strip_index,
                effect_index,
            }))
            .on_release(Message::Mixer(MixerMessage::DropTrackEffect {
                strip_index: target.strip_index,
                effect_index,
            }));
    }

    container(row)
        .width(Length::Fixed(EFFECT_RACK_SLOT_WIDTH))
        .height(Length::Fixed(EFFECT_RACK_ROW_HEIGHT))
        .into()
}

fn effect_rack_separator() -> Element<'static, Message> {
    container(
        container(text(""))
            .width(Fill)
            .height(Length::Fixed(EFFECT_RACK_SEPARATOR_HEIGHT))
            .style(effect_rack_separator_surface),
    )
    .padding([0, EFFECT_RACK_SEPARATOR_INSET as u16])
    .width(Fill)
    .height(Length::Fixed(EFFECT_RACK_SEPARATOR_HEIGHT))
    .into()
}

fn effect_rack_drop_indicator_line() -> Element<'static, Message> {
    container(
        container(text(""))
            .width(Fill)
            .height(Length::Fixed(EFFECT_RACK_SEPARATOR_HEIGHT))
            .style(effect_rack_drop_indicator_surface),
    )
    .padding([0, EFFECT_RACK_SEPARATOR_INSET as u16])
    .width(Fill)
    .height(Length::Fixed(EFFECT_RACK_SEPARATOR_HEIGHT))
    .into()
}

fn effect_rack_add_button_style(theme: &iced::Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: None,
        text_color: slot_segment_foreground(theme, hovered, false),
        border: border::rounded(0).width(0),
        shadow: iced::Shadow::default(),
        ..button::Style::default()
    }
}

fn effect_rack_add_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
) -> iced::widget::svg::Style {
    let hovered = matches!(status, iced::widget::svg::Status::Hovered);
    iced::widget::svg::Style {
        color: Some(slot_segment_foreground(theme, hovered, false)),
    }
}

fn processor_slot_segment_button_style(
    theme: &iced::Theme,
    status: button::Status,
    active: bool,
    slot_hovered: bool,
) -> button::Style {
    let hovered =
        slot_hovered || matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: None,
        text_color: slot_segment_foreground(theme, hovered, active),
        ..button::Style::default()
    }
}

fn processor_slot_segment_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
    active: bool,
    slot_hovered: bool,
) -> iced::widget::svg::Style {
    let hovered = slot_hovered || matches!(status, iced::widget::svg::Status::Hovered);
    iced::widget::svg::Style {
        color: Some(slot_segment_foreground(theme, hovered, active)),
    }
}

fn processor_slot_label_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
    active: bool,
    slot_hovered: bool,
) -> iced::widget::svg::Style {
    processor_slot_segment_icon_style(theme, status, active, slot_hovered)
}

fn processor_slot_active_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
    _active: bool,
    slot_hovered: bool,
) -> iced::widget::svg::Style {
    processor_slot_segment_icon_style(theme, status, false, slot_hovered)
}

fn processor_slot_label_button_style(
    theme: &iced::Theme,
    status: button::Status,
    active: bool,
    slot_hovered: bool,
) -> button::Style {
    processor_slot_segment_button_style(theme, status, active, slot_hovered)
}

fn processor_slot_icon_button_style(
    theme: &iced::Theme,
    status: button::Status,
    slot_hovered: bool,
) -> button::Style {
    processor_slot_segment_button_style(theme, status, false, slot_hovered)
}

#[cfg(test)]
fn transparent_hit_button(theme: &iced::Theme, status: button::Status) -> button::Style {
    processor_slot_segment_button_style(theme, status, false, false)
}

fn slot_segment_foreground(theme: &iced::Theme, hovered: bool, active: bool) -> Color {
    let palette = theme.extended_palette();
    if active {
        let accent = if hovered {
            palette.primary.strong.color
        } else {
            palette.primary.base.color
        };
        let text = palette.background.weak.text;
        let amount = 0.45;
        return Color {
            r: accent.r + (text.r - accent.r) * amount,
            g: accent.g + (text.g - accent.g) * amount,
            b: accent.b + (text.b - accent.b) * amount,
            a: accent.a + (text.a - accent.a) * amount,
        };
    }
    if hovered {
        return palette.background.weak.text;
    }

    let color = palette.background.weak.text;
    let background = palette.background.weak.color;
    let amount = 0.38;
    Color {
        r: color.r + (background.r - color.r) * amount,
        g: color.g + (background.g - color.g) * amount,
        b: color.b + (background.b - color.b) * amount,
        a: color.a + (background.a - color.a) * amount,
    }
}

fn effect_rack_surface(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.base.color.into()),
        ..container::Style::default()
    }
}

fn effect_rack_scrollable(theme: &iced::Theme, status: scrollable::Status) -> scrollable::Style {
    let palette = theme.extended_palette();
    let mut style = ui_style::workspace_scrollable(theme, status);
    style.container.background = Some(palette.background.base.color.into());
    style.container.text_color = Some(palette.background.base.text);
    style.vertical_rail.background = Some(palette.background.base.color.into());
    style.vertical_rail.scroller.background = palette.background.weak.color.into();
    style
}

fn effect_rack_separator_surface(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.weak.color.into()),
        ..container::Style::default()
    }
}

fn effect_rack_drop_indicator_surface(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.primary.strong.color.into()),
        ..container::Style::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn processor_slot_controls_sized(
    target: super::processor_editor_windows::EditorTarget,
    role: ProcessorSlotRole,
    selected: Option<&ProcessorChoice>,
    editor_enabled: bool,
    bypassed: bool,
    hovered_segment: Option<ProcessorSlotSegment>,
    controls_enabled: bool,
    slot_width: f32,
    list_item: bool,
    label_override: Option<String>,
) -> Element<'static, Message> {
    let picker_action =
        controls_enabled.then_some(Message::Mixer(MixerMessage::ToggleProcessorBrowser(target)));
    let editor_action =
        processor_slot_editor_action(target, selected, editor_enabled, controls_enabled);
    let bypass_action =
        (controls_enabled && role == ProcessorSlotRole::Effect && !is_empty_choice(selected))
            .then_some(Message::Mixer(MixerMessage::ToggleSlotBypass(target)));
    let can_drag_effect = controls_enabled
        && role == ProcessorSlotRole::Effect
        && target.slot_index > 0
        && !is_empty_choice(selected);
    let label = if list_item && is_empty_choice(selected) {
        "Add effect".to_string()
    } else if let Some(label) = label_override {
        processor_hover_label(&label, list_item)
    } else {
        processor_hover_label(&processor_trigger_label(selected), list_item)
    };
    let bypass_hovered = hovered_segment == Some(ProcessorSlotSegment::Bypass);
    let editor_hovered = hovered_segment == Some(ProcessorSlotSegment::Editor);
    let picker_hovered = hovered_segment == Some(ProcessorSlotSegment::Picker);
    let label_active = role == ProcessorSlotRole::Instrument && !is_empty_choice(selected);

    let mut split = row![]
        .spacing(ui_style::SPACE_XS)
        .align_y(alignment::Vertical::Center);
    if role == ProcessorSlotRole::Effect {
        split = split.push(processor_slot_icon_segment(
            processor_slot_bypass_icon(bypassed),
            bypass_action,
            false,
            bypass_hovered,
            target,
            ProcessorSlotSegment::Bypass,
        ));
        split = split.push(slot_area_separator());
    }
    split = split.push(processor_slot_label_segment(
        role.slot_icon(),
        label,
        label_active,
        editor_hovered,
        target,
        ProcessorSlotSegment::Editor,
        editor_action.or_else(|| {
            if is_empty_choice(selected) {
                picker_action.clone()
            } else {
                None
            }
        }),
    ));
    split = split.push(slot_area_separator());
    split = split.push(processor_slot_icon_segment(
        icons::list_tree(),
        picker_action.clone(),
        false,
        picker_hovered,
        target,
        ProcessorSlotSegment::Picker,
    ));
    let content: Element<'static, Message> = split.into();

    let button_content = container(content)
        .width(Fill)
        .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
        .center_y(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT));
    let surface: Element<'static, Message> = button_content
        .style(move |theme| processor_slot_surface(theme, false, list_item))
        .padding([0, ui_style::grid(2)])
        .width(Length::Fixed(slot_width))
        .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
        .into();

    let mut hit_area = mouse_area(surface)
        .on_right_press(picker_action.unwrap_or_else(noop_message))
        .interaction(mouse::Interaction::Pointer);
    if can_drag_effect {
        hit_area = hit_area
            .on_press(Message::Mixer(MixerMessage::StartTrackEffectDrag {
                strip_index: target.strip_index,
                effect_index: target.slot_index - 1,
            }))
            .on_release(Message::Mixer(MixerMessage::DropTrackEffect {
                strip_index: target.strip_index,
                effect_index: target.slot_index - 1,
            }));
    }

    container(hit_area)
        .width(Length::Fixed(slot_width))
        .height(Length::Fixed(if list_item {
            PROCESSOR_SLOT_BUTTON_HEIGHT
        } else {
            PROCESSOR_SLOT_HEIGHT
        }))
        .center_x(Length::Fixed(slot_width))
        .center_y(Length::Fixed(if list_item {
            PROCESSOR_SLOT_BUTTON_HEIGHT
        } else {
            PROCESSOR_SLOT_HEIGHT
        }))
        .into()
}

fn processor_hover_label(label: &str, _list_item: bool) -> String {
    crate::track_names::ellipsize_middle(label, PROCESSOR_SLOT_LABEL_MAX_LEN)
}

fn processor_slot_label_segment(
    icon: iced::widget::svg::Handle,
    label: String,
    active: bool,
    slot_hovered: bool,
    target: super::processor_editor_windows::EditorTarget,
    segment: ProcessorSlotSegment,
    action: Option<Message>,
) -> Element<'static, Message> {
    let button: Element<'static, Message> = button(
        row![
            container(ui_style::icon(
                icon,
                PROCESSOR_BROWSER_ICON_SIZE,
                move |theme, status| {
                    processor_slot_label_icon_style(theme, status, active, slot_hovered)
                },
            ))
            .width(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
            .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
            .center_x(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
            .center_y(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT)),
            container(
                text(label)
                    .size(ui_style::FONT_SIZE_UI_XS)
                    .wrapping(iced::widget::text::Wrapping::None)
            )
            .width(Fill)
            .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
            .align_y(alignment::Vertical::Center)
            .clip(true)
        ]
        .spacing(ui_style::SPACE_XS)
        .align_y(alignment::Vertical::Center),
    )
    .style(move |theme, status| {
        processor_slot_label_button_style(theme, status, active, slot_hovered)
    })
    .padding(0)
    .width(Fill)
    .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
    .on_press_maybe(action)
    .into();

    mouse_area(button)
        .on_enter(Message::Mixer(MixerMessage::SetProcessorSlotHovered(Some(
            (target, segment),
        ))))
        .on_exit(Message::Mixer(MixerMessage::SetProcessorSlotHovered(None)))
        .into()
}

fn processor_slot_icon_segment(
    icon: iced::widget::svg::Handle,
    action: Option<Message>,
    active: bool,
    slot_hovered: bool,
    target: super::processor_editor_windows::EditorTarget,
    segment: ProcessorSlotSegment,
) -> Element<'static, Message> {
    let button: Element<'static, Message> = button(
        container(ui_style::icon(
            icon,
            PROCESSOR_BROWSER_ICON_SIZE,
            move |theme, status| {
                processor_slot_active_icon_style(theme, status, active, slot_hovered)
            },
        ))
        .width(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
        .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
        .center_x(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
        .center_y(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT)),
    )
    .style(move |theme, status| processor_slot_icon_button_style(theme, status, slot_hovered))
    .padding(0)
    .width(Length::Fixed(PROCESSOR_SLOT_SEGMENT_WIDTH))
    .height(Length::Fixed(PROCESSOR_SLOT_BUTTON_HEIGHT))
    .on_press_maybe(action)
    .into();

    mouse_area(button)
        .on_enter(Message::Mixer(MixerMessage::SetProcessorSlotHovered(Some(
            (target, segment),
        ))))
        .on_exit(Message::Mixer(MixerMessage::SetProcessorSlotHovered(None)))
        .into()
}

fn slot_area_separator() -> Element<'static, Message> {
    container(text(""))
        .style(slot_area_separator_surface)
        .width(Length::Fixed(INSTRUMENT_SLOT_SEPARATOR_WIDTH))
        .height(Length::Fixed(
            INSTRUMENT_SLOT_BUTTON_HEIGHT - ui_style::grid_f32(2),
        ))
        .into()
}

fn slot_area_separator_surface(theme: &iced::Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.strong.color.into()),
        ..container::Style::default()
    }
}

fn processor_slot_icon_style(
    theme: &iced::Theme,
    status: iced::widget::svg::Status,
    active: bool,
) -> iced::widget::svg::Style {
    if active {
        let palette = theme.extended_palette();
        return iced::widget::svg::Style {
            color: Some(match status {
                iced::widget::svg::Status::Idle => palette.primary.base.color,
                iced::widget::svg::Status::Hovered => palette.primary.strong.color,
            }),
        };
    }

    ui_style::svg_muted_control(theme, status)
}

fn processor_slot_button_style(
    theme: &iced::Theme,
    status: button::Status,
    open: bool,
    list_item: bool,
) -> button::Style {
    let mut style = ui_style::button_selector_field(theme, status, open);
    if list_item {
        let palette = theme.extended_palette();
        style.background = None;
        style.border = border::rounded(0)
            .width(0)
            .color(palette.background.strong.color);
        style.shadow = iced::Shadow::default();
    }
    style
}

fn processor_slot_surface(theme: &iced::Theme, open: bool, list_item: bool) -> container::Style {
    let button_style = processor_slot_button_style(theme, button::Status::Active, open, list_item);
    container::Style {
        text_color: Some(button_style.text_color),
        background: button_style.background,
        border: button_style.border,
        shadow: button_style.shadow,
        ..container::Style::default()
    }
}

#[cfg(test)]
fn instrument_slot_primary_action(
    track_index: usize,
    selected: Option<&InstrumentChoice>,
    editor_enabled: bool,
    controls_enabled: bool,
) -> Option<Message> {
    processor_slot_editor_action(
        super::processor_editor_windows::EditorTarget {
            strip_index: track_index + 1,
            slot_index: 0,
        },
        selected,
        editor_enabled,
        controls_enabled,
    )
    .or_else(|| {
        if controls_enabled && is_empty_choice(selected) {
            Some(Message::Mixer(MixerMessage::ToggleProcessorBrowser(
                super::processor_editor_windows::EditorTarget {
                    strip_index: track_index + 1,
                    slot_index: 0,
                },
            )))
        } else {
            None
        }
    })
}

fn processor_slot_editor_action(
    target: super::processor_editor_windows::EditorTarget,
    selected: Option<&ProcessorChoice>,
    editor_enabled: bool,
    controls_enabled: bool,
) -> Option<Message> {
    if !controls_enabled {
        return None;
    }

    match (selected, editor_enabled) {
        (Some(ProcessorChoice::Processor { .. }), true) => {
            Some(Message::Mixer(MixerMessage::OpenEditor(target)))
        }
        _ => None,
    }
}

fn is_empty_choice(choice: Option<&ProcessorChoice>) -> bool {
    matches!(choice, None | Some(ProcessorChoice::None))
}

fn processor_slot_bypass_icon(bypassed: bool) -> iced::widget::svg::Handle {
    if bypassed {
        icons::power_off()
    } else {
        icons::power()
    }
}

pub(super) fn instrument_browser_overlay(app: &Lilypalooza) -> Element<'_, Message> {
    let Some(target) = app.open_processor_browser_target else {
        return container(text("")).width(Fill).height(Fill).into();
    };
    let Some(playback) = app.playback.as_ref() else {
        return container(text("")).width(Fill).height(Fill).into();
    };
    let mixer = playback.mixer_state();
    let Some(strip) = mixer.strip_by_index(target.strip_index) else {
        return container(text("")).width(Fill).height(Fill).into();
    };
    let role = if target.slot_index == 0 {
        ProcessorSlotRole::Instrument
    } else {
        ProcessorSlotRole::Effect
    };
    let choices = processor_choices(role);
    let selected = selected_processor_choice(strip.slot(target.slot_index), role);

    let header = container(
        row![
            column![
                text(role.title())
                    .size(ui_style::FONT_SIZE_UI_SM)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..fonts::UI
                    }),
                text(strip.name.clone())
                    .size(ui_style::FONT_SIZE_UI_XS)
                    .font(fonts::MONO),
            ]
            .spacing(ui_style::SPACE_XS),
            container(text("")).width(Fill),
            ui_style::flat_icon_button(
                icons::x(),
                ui_style::grid_f32(5),
                ui_style::grid_f32(3),
                ui_style::button_pane_header_control,
                ui_style::svg_dimmed_control,
            )
            .width(Length::Fixed(ui_style::grid_f32(5)))
            .height(Length::Fixed(ui_style::grid_f32(5)))
            .on_press(Message::Mixer(MixerMessage::CloseProcessorBrowser)),
        ]
        .spacing(ui_style::SPACE_XS)
        .align_y(alignment::Vertical::Center),
    )
    .width(Fill)
    .padding([ui_style::PADDING_XS, ui_style::PADDING_SM])
    .style(ui_style::prompt_header);

    let tabs = row![
        instrument_browser_tab_button(
            InstrumentBrowserBackend::BuiltIn,
            app.instrument_browser_backend
        ),
        instrument_browser_tab_button(
            InstrumentBrowserBackend::Clap,
            app.instrument_browser_backend
        ),
        instrument_browser_tab_button(
            InstrumentBrowserBackend::Vst3,
            app.instrument_browser_backend
        ),
    ]
    .spacing(ui_style::SPACE_XS)
    .width(Fill);

    let search = text_input(role.search_placeholder(), &app.instrument_browser_search)
        .on_input(|value| Message::Mixer(MixerMessage::ProcessorBrowserSearchChanged(value)))
        .id(app.instrument_browser_search_input_id.clone())
        .style(ui_style::browser_search_input)
        .size(ui_style::FONT_SIZE_UI_SM)
        .padding([ui_style::PADDING_XS, ui_style::PADDING_SM])
        .width(Fill);

    let backend = app.instrument_browser_backend;
    let body = processor_browser_list(
        target,
        role,
        &choices,
        selected.as_ref(),
        &app.instrument_browser_search,
        backend,
        app.plugin_scan.is_active(),
    );

    let dialog = container(
        column![
            header,
            container(column![tabs, search, body].spacing(ui_style::SPACE_SM))
                .padding(ui_style::PADDING_SM)
        ]
        .spacing(0),
    )
    .width(Length::Fixed(INSTRUMENT_BROWSER_WIDTH))
    .style(ui_style::prompt_dialog);

    let centered_dialog = container(
        mouse_area(opaque(dialog))
            .on_press(Message::Noop)
            .interaction(iced::mouse::Interaction::Pointer),
    )
    .width(Fill)
    .height(Fill)
    .center_x(Fill)
    .center_y(Fill);

    let backdrop = mouse_area(
        container(centered_dialog)
            .width(Fill)
            .height(Fill)
            .style(ui_style::prompt_backdrop),
    )
    .on_press(Message::Mixer(MixerMessage::CloseProcessorBrowser));

    opaque(backdrop)
}

fn instrument_browser_tab_button(
    tab: InstrumentBrowserBackend,
    active: InstrumentBrowserBackend,
) -> Element<'static, Message> {
    button(text(tab.label()).size(ui_style::FONT_SIZE_UI_XS))
        .style(move |theme, status| ui_style::button_pane_tab(theme, status, tab == active))
        .padding([ui_style::grid(1), ui_style::grid(3)])
        .height(Length::Fixed(ui_style::grid_f32(6)))
        .on_press(Message::Mixer(MixerMessage::SelectProcessorBrowserBackend(
            tab,
        )))
        .into()
}

fn processor_browser_list(
    target: super::processor_editor_windows::EditorTarget,
    role: ProcessorSlotRole,
    choices: &[ProcessorChoice],
    selected: Option<&ProcessorChoice>,
    search: &str,
    backend: ProcessorBrowserBackend,
    scan_active: bool,
) -> Element<'static, Message> {
    let browser = processor_browser_entries(choices, backend, search);
    let InstrumentBrowserEntries { show_none, entries } = browser;
    let mut content = column![].spacing(0).width(Fill);
    if show_none {
        content = content.push(instrument_browser_choice_button(
            target,
            ProcessorChoice::None,
            selected == Some(&InstrumentChoice::None),
        ));
    }

    let has_entries = !entries.is_empty();
    for choice in entries {
        content = content.push(instrument_browser_choice_button(
            target,
            choice.clone(),
            selected == Some(&choice),
        ));
    }

    if !show_none && !has_entries {
        let label = if backend == ProcessorBrowserBackend::BuiltIn {
            role.empty_search_label()
        } else if scan_active {
            "Scanning plugins..."
        } else {
            role.backend_empty_label(backend)
        };
        return instrument_browser_empty_state(label);
    }

    scrollable(content)
        .height(Length::Fixed(INSTRUMENT_BROWSER_HEIGHT))
        .style(ui_style::workspace_scrollable)
        .into()
}

fn instrument_browser_choice_button(
    target: super::processor_editor_windows::EditorTarget,
    choice: ProcessorChoice,
    selected: bool,
) -> Element<'static, Message> {
    button(
        container(
            text(instrument_choice_primary_label(&choice))
                .size(ui_style::FONT_SIZE_UI_SM)
                .width(Fill)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Fill)
        .center_y(Fill),
    )
    .style(move |theme, status| ui_style::button_browser_entry(theme, status, selected))
    .padding([ui_style::grid(2), ui_style::grid(3)])
    .width(Fill)
    .on_press(Message::Mixer(MixerMessage::SelectProcessor(
        target, choice,
    )))
    .into()
}

fn instrument_browser_empty_state(label: &'static str) -> Element<'static, Message> {
    container(
        text(label)
            .size(ui_style::FONT_SIZE_UI_SM)
            .font(fonts::MONO),
    )
    .width(Fill)
    .height(Length::Fixed(INSTRUMENT_BROWSER_HEIGHT))
    .center_x(Fill)
    .center_y(Length::Fixed(INSTRUMENT_BROWSER_HEIGHT))
    .into()
}

#[allow(clippy::too_many_arguments)]
fn instrument_choice_primary_label(choice: &InstrumentChoice) -> String {
    match choice {
        InstrumentChoice::None => "Empty".to_string(),
        InstrumentChoice::Processor { name, .. } => name.clone(),
    }
}

#[cfg(test)]
fn instrument_trigger_label(choice: Option<&InstrumentChoice>) -> String {
    processor_trigger_label(choice)
}

fn processor_trigger_label(choice: Option<&ProcessorChoice>) -> String {
    crate::track_names::ellipsize_middle(
        &choice
            .map(instrument_choice_primary_label)
            .unwrap_or_else(|| "Empty".to_string()),
        PROCESSOR_SLOT_LABEL_MAX_LEN,
    )
}

fn instrument_choice_search_haystack(choice: &InstrumentChoice) -> String {
    match choice {
        InstrumentChoice::None => "empty none no instrument".to_string(),
        InstrumentChoice::Processor { name, backend, .. } => {
            format!("{} {}", name.to_lowercase(), backend.label().to_lowercase())
        }
    }
}

#[cfg(test)]
fn instrument_browser_entries(
    choices: &[InstrumentChoice],
    active_backend: InstrumentBrowserBackend,
    search: &str,
) -> InstrumentBrowserEntries {
    processor_browser_entries(choices, active_backend, search)
}

fn processor_browser_entries(
    choices: &[ProcessorChoice],
    active_backend: ProcessorBrowserBackend,
    search: &str,
) -> InstrumentBrowserEntries {
    let query = search.trim().to_lowercase();
    let matches = |choice: &ProcessorChoice| {
        query.is_empty() || instrument_choice_search_haystack(choice).contains(&query)
    };

    let mut entries = Vec::new();
    let mut show_none = false;
    for choice in choices {
        match choice {
            InstrumentChoice::None => {
                if active_backend == InstrumentBrowserBackend::BuiltIn && matches(choice) {
                    show_none = true;
                }
            }
            InstrumentChoice::Processor { backend, .. } if *backend != active_backend => {}
            InstrumentChoice::Processor { .. } if matches(choice) => {
                entries.push(choice.clone());
            }
            InstrumentChoice::Processor { .. } => {}
        }
    }

    InstrumentBrowserEntries { show_none, entries }
}

#[allow(dead_code)]
fn instrument_choices() -> Vec<InstrumentChoice> {
    processor_choices(ProcessorSlotRole::Instrument)
}

fn processor_choices(role: ProcessorSlotRole) -> Vec<ProcessorChoice> {
    let mut choices = Vec::new();
    choices.push(ProcessorChoice::None);
    choices.extend(
        registry::all()
            .iter()
            .filter(|entry| entry.role == role.registry_role())
            .filter(|entry| entry.id != BUILTIN_NONE_ID && entry.id != BUILTIN_METRONOME_ID)
            .map(|entry| ProcessorChoice::Processor {
                processor_id: entry.id.to_string(),
                name: entry.name.to_string(),
                backend: match entry.backend {
                    registry::Backend::BuiltIn => ProcessorBrowserBackend::BuiltIn,
                    registry::Backend::Clap => ProcessorBrowserBackend::Clap,
                    registry::Backend::Vst3 => ProcessorBrowserBackend::Vst3,
                },
            }),
    );
    choices
}

fn selected_instrument_choice(
    slot: Option<&SlotState>,
    _mixer: &MixerState,
) -> Option<InstrumentChoice> {
    selected_processor_choice(slot, ProcessorSlotRole::Instrument)
}

fn selected_processor_choice(
    slot: Option<&SlotState>,
    _role: ProcessorSlotRole,
) -> Option<ProcessorChoice> {
    let slot = slot?;
    if slot.is_empty() {
        return Some(ProcessorChoice::None);
    }
    let entry = registry::resolve(&slot.kind)?;
    Some(ProcessorChoice::Processor {
        processor_id: entry.id.to_string(),
        name: entry.name.to_string(),
        backend: match entry.backend {
            registry::Backend::BuiltIn => ProcessorBrowserBackend::BuiltIn,
            registry::Backend::Clap => ProcessorBrowserBackend::Clap,
            registry::Backend::Vst3 => ProcessorBrowserBackend::Vst3,
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::ui_style;
    use iced::widget::{button, container, row};
    use iced::{Color, Element, Event, Length, Point, Theme, mouse};
    use iced_test::{Simulator, simulator};
    use lilypalooza_audio::mixer::MixerMeterSnapshotWindow;
    use lilypalooza_audio::{AudioEngine, AudioEngineOptions};
    use lilypalooza_audio::{BUILTIN_GAIN_ID, BUILTIN_SOUNDFONT_ID, MixerState, SlotState};
    use std::path::{Path, PathBuf};

    use super::{
        COMPACT_GAIN_SWITCH_OFFSET, GROUP_SIDE_BORDER_WIDTH, GainControlMode,
        INSTRUMENT_PICKER_HEIGHT, INSTRUMENT_SLOT_WIDTH, InstrumentBrowserBackend,
        InstrumentChoice, MAIN_SECTION_WIDTH, MAIN_STRIP_WIDTH, MIXER_MIN_HEIGHT, MeterDependency,
        ROUTE_PICKER_BOTTOM_INSET, ROUTE_PICKER_TOP_SPACING, RouteChoice, RoutingStrip,
        SECTION_BODY_GAP, SEND_CONTROL_HEIGHT, SEND_MODE_HEIGHT, SEND_PANEL_TOP_SPACING,
        SEND_ROW_CONTENT_BOTTOM_SPACING, SEND_ROW_HEIGHT, STRIP_MIN_HEIGHT, STRIP_TOGGLE_SIZE,
        STRIP_VIRTUALIZATION_OVERSCAN, STRIP_WIDTH, SendDependency, SendDestinationAction,
        SendDestinationChoice, StripMeterSnapshot, TITLE_TOP_SPACING,
        TRACK_TITLE_EDITOR_CONTROL_HEIGHT, TRACK_TITLE_EDITOR_HEIGHT,
        TRACK_TITLE_EDITOR_INPUT_PADDING_H, TRACK_TITLE_EDITOR_INPUT_PADDING_V,
        TRACK_TITLE_EDITOR_SWATCH_SIZE, TrackStripDependency, VALUE_LABEL_HEIGHT, color_bits,
        control_stack_height, gain_control_height, gain_control_mode, gain_label,
        instrument_browser_entries, instrument_slot_primary_action, instrument_trigger_label,
        meter_colors, meter_control_height, meter_peak_label, meter_scale_visible,
        processor_choices, selected_instrument_choice, selected_processor_choice,
        track_should_use_roll_tint, visible_strip_window,
    };
    use crate::app::Message;
    use crate::app::messages::MixerMessage;
    use crate::icons;
    use lilypalooza_audio::mixer::{ChannelMeterSnapshot, TrackRoute};

    fn assert_snapshot_matches(
        ui: &mut iced_test::Simulator<'_, crate::app::Message>,
        baseline_name: &str,
    ) -> Result<(), iced_test::Error> {
        let _snapshot_guard = super::super::ICED_SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = ui.snapshot(&Theme::Dark)?;
        let baseline_path = Path::new(baseline_name);

        assert!(
            snapshot.matches_hash(baseline_name)?,
            "snapshot hash mismatch for: {baseline_name}"
        );
        assert!(
            snapshot.matches_image(baseline_path)?,
            "snapshot image mismatch for: {baseline_name}"
        );

        Ok(())
    }

    fn assert_snapshots_equal(
        first: &mut iced_test::Simulator<'_, crate::app::Message>,
        second: &mut iced_test::Simulator<'_, crate::app::Message>,
        baseline_name: &str,
    ) -> Result<(), iced_test::Error> {
        let _snapshot_guard = super::super::ICED_SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut baseline_path = PathBuf::from("/tmp");
        baseline_path.push(baseline_name);

        let png = baseline_path.with_file_name(format!("{baseline_name}-wgpu.png"));
        let sha = baseline_path.with_file_name(format!("{baseline_name}-wgpu.sha256"));
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&sha);

        let first_snapshot = first.snapshot(&Theme::Dark)?;
        assert!(first_snapshot.matches_hash(&baseline_path)?);
        assert!(first_snapshot.matches_image(&baseline_path)?);

        let second_snapshot = second.snapshot(&Theme::Dark)?;
        assert!(
            second_snapshot.matches_hash(&baseline_path)?,
            "snapshot hash mismatch for: {baseline_name}"
        );
        assert!(
            second_snapshot.matches_image(&baseline_path)?,
            "snapshot image mismatch for: {baseline_name}"
        );

        Ok(())
    }

    fn color_distance(first: Color, second: Color) -> f32 {
        (first.r - second.r).abs() + (first.g - second.g).abs() + (first.b - second.b).abs()
    }

    fn test_effect_slot(
        slot_index: usize,
        instance_id: u64,
        instance_label_index: u32,
        name: &str,
    ) -> super::EffectSlotDependency {
        super::EffectSlotDependency {
            slot_index,
            instance_id,
            instance_label_index,
            selected: Some(super::ProcessorChoice::Processor {
                processor_id: name.to_lowercase(),
                name: name.to_string(),
                backend: super::ProcessorBrowserBackend::BuiltIn,
            }),
            editor_enabled: false,
            bypassed: false,
        }
    }

    fn assert_snapshots_differ(
        first: &mut iced_test::Simulator<'_, crate::app::Message>,
        second: &mut iced_test::Simulator<'_, crate::app::Message>,
        baseline_name: &str,
    ) -> Result<(), iced_test::Error> {
        let _snapshot_guard = super::super::ICED_SNAPSHOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut baseline_path = PathBuf::from("/tmp");
        baseline_path.push(baseline_name);

        let png = baseline_path.with_file_name(format!("{baseline_name}-wgpu.png"));
        let sha = baseline_path.with_file_name(format!("{baseline_name}-wgpu.sha256"));
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&sha);

        let first_snapshot = first.snapshot(&Theme::Dark)?;
        assert!(first_snapshot.matches_hash(&baseline_path)?);
        assert!(first_snapshot.matches_image(&baseline_path)?);

        let second_snapshot = second.snapshot(&Theme::Dark)?;
        assert!(
            !second_snapshot.matches_hash(&baseline_path)?,
            "snapshot hash unexpectedly matched for: {baseline_name}"
        );

        Ok(())
    }

    fn is_grid_multiple(value: f32) -> bool {
        ((value / 4.0).round() - (value / 4.0)).abs() < 1.0e-4
    }

    #[test]
    fn fixed_strip_sizes_follow_four_px_grid() {
        for value in [
            MAIN_STRIP_WIDTH,
            STRIP_WIDTH,
            STRIP_MIN_HEIGHT,
            MIXER_MIN_HEIGHT,
            INSTRUMENT_PICKER_HEIGHT,
            TRACK_TITLE_EDITOR_HEIGHT,
            VALUE_LABEL_HEIGHT,
            ROUTE_PICKER_TOP_SPACING,
            ROUTE_PICKER_BOTTOM_INSET,
            SEND_ROW_HEIGHT,
            SEND_ROW_CONTENT_BOTTOM_SPACING,
            SEND_PANEL_TOP_SPACING,
            SEND_CONTROL_HEIGHT,
            SEND_MODE_HEIGHT,
            COMPACT_GAIN_SWITCH_OFFSET,
        ] {
            assert!(is_grid_multiple(value), "{value} should use the 4px grid");
        }
    }

    #[test]
    fn mixer_track_title_editor_height_increases_by_one_grid_unit() {
        assert_eq!(TRACK_TITLE_EDITOR_HEIGHT, ui_style::grid_f32(5));
    }

    #[test]
    fn mixer_track_title_editor_input_padding_matches_taller_height() {
        assert_eq!(TRACK_TITLE_EDITOR_INPUT_PADDING_V, 2);
        assert_eq!(TRACK_TITLE_EDITOR_INPUT_PADDING_H, ui_style::grid(1));
    }

    #[test]
    fn mixer_track_title_editor_controls_scale_with_taller_shell() {
        assert_eq!(TRACK_TITLE_EDITOR_CONTROL_HEIGHT, TRACK_TITLE_EDITOR_HEIGHT);
    }

    #[test]
    fn mixer_track_title_editor_swatch_is_square() {
        assert_eq!(TRACK_TITLE_EDITOR_SWATCH_SIZE, TRACK_TITLE_EDITOR_HEIGHT);
    }

    #[test]
    fn mixer_strip_title_gap_uses_three_grid_units() {
        assert_eq!(TITLE_TOP_SPACING, ui_style::grid_f32(3));
    }

    #[test]
    fn mixer_section_header_has_no_body_gap() {
        assert_eq!(SECTION_BODY_GAP, 0.0);
    }

    #[test]
    fn mixer_track_title_editor_widget_matches_snapshot() -> Result<(), iced_test::Error> {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                STRIP_WIDTH + ui_style::grid_f32(4),
                TRACK_TITLE_EDITOR_HEIGHT + ui_style::grid_f32(4),
            ],
            container(super::track_title_content(
                0,
                "Track 1",
                true,
                "Track 1",
                Color::from_rgb(0.42, 0.58, 0.86),
                false,
            ))
            .width(Length::Fixed(STRIP_WIDTH))
            .padding(ui_style::PADDING_SM),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_track_title_editor_widget_five_grid",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_strip_title_editor_matches_snapshot() -> Result<(), iced_test::Error> {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                STRIP_WIDTH + ui_style::grid_f32(8),
                STRIP_MIN_HEIGHT + ui_style::grid_f32(8),
            ],
            container(super::strip_shell(
                super::track_title_content(
                    0,
                    "Track 1",
                    true,
                    "Track 1",
                    Color::from_rgb(0.42, 0.58, 0.86),
                    false,
                ),
                None,
                None,
                0.0,
                0.0,
                container(iced::widget::text("")).into(),
                super::StripActions {
                    panel: None,
                    solo: None,
                    mute: None,
                    on_gain: None,
                    on_pan: None,
                },
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                false,
            ))
            .padding(ui_style::PADDING_SM),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_strip_title_editor_integration_five_grid",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_strip_controls_leave_gap_above_title_matches_snapshot() -> Result<(), iced_test::Error>
    {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [STRIP_WIDTH + ui_style::grid_f32(8), 520.0],
            container(super::strip_shell(
                super::track_title_content(
                    0,
                    "Violin",
                    true,
                    "Violin",
                    Color::from_rgb(0.92, 0.34, 0.34),
                    false,
                ),
                Some(super::processor_slot_controls(
                    crate::app::processor_editor_windows::EditorTarget {
                        strip_index: 1,
                        slot_index: 0,
                    },
                    super::ProcessorSlotRole::Instrument,
                    Some(&InstrumentChoice::None),
                    false,
                    false,
                    None,
                    true,
                )),
                None,
                0.0,
                0.0,
                container(iced::widget::text(""))
                    .width(Length::Fixed(72.0))
                    .height(Length::Fixed(220.0))
                    .into(),
                super::StripActions {
                    panel: Some((false, false, super::noop_message())),
                    solo: Some((false, super::noop_message())),
                    mute: Some((false, super::noop_message())),
                    on_gain: Some(Box::new(|_| super::noop_message())),
                    on_pan: Some(Box::new(|_| super::noop_message())),
                },
                480.0,
                GainControlMode::Knob,
                false,
            ))
            .padding(ui_style::PADDING_SM),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_strip_controls_title_gap_fixed",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_full_track_strip_rename_matches_snapshot() -> Result<(), iced_test::Error> {
        let mixer = MixerState::new();
        let meters = MixerMeterSnapshotWindow::default();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [STRIP_WIDTH + ui_style::grid_f32(8), 520.0],
            super::instrument_track_area(
                &mixer,
                &meters,
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                0..1,
                1,
                &[crate::track_colors::default_track_color(0)],
                Some(crate::app::RenameTarget::Track(0)),
                Some(crate::app::WorkspacePaneKind::Mixer),
                "Violin",
                Color::from_rgb(0.92, 0.34, 0.34),
                false,
                None,
                None,
                None,
                None,
                &[],
                true,
            ),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_full_track_strip_rename_no_section_gap",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_master_strip_main_title_matches_snapshot() -> Result<(), iced_test::Error> {
        let mixer = MixerState::new();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                MAIN_STRIP_WIDTH + ui_style::grid_f32(8),
                STRIP_MIN_HEIGHT + ui_style::grid_f32(8),
            ],
            super::sticky_master_strip(
                &mixer,
                StripMeterSnapshot::default(),
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                false,
                true,
            ),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_master_strip_main_title_no_section_gap",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_bus_area_empty_centers_add_bus_matches_snapshot() -> Result<(), iced_test::Error> {
        let mixer = MixerState::new();
        let meters = MixerMeterSnapshotWindow::default();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [STRIP_WIDTH + ui_style::grid_f32(8), 320.0],
            super::bus_track_area(
                &mixer,
                &meters,
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                0..0,
                &[],
                None,
                None,
                None,
                None,
                None,
                "",
                true,
            ),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_bus_area_empty_centered_add_icon_no_section_gap",
        )?;

        Ok(())
    }

    #[test]
    fn mixer_bus_area_nonempty_appends_add_bus_lane_matches_snapshot()
    -> Result<(), iced_test::Error> {
        let (mut app, _task) = crate::app::new_with_default_test_state();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        let _ = app.handle_mixer_message(MixerMessage::AddBus);

        let mixer = app
            .playback
            .as_ref()
            .expect("playback should exist")
            .mixer_state()
            .clone();
        let meters = MixerMeterSnapshotWindow::default();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [STRIP_WIDTH * 2.0 + ui_style::grid_f32(8), 320.0],
            super::bus_track_area(
                &mixer,
                &meters,
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                0..mixer.bus_count(),
                &[],
                None,
                None,
                None,
                None,
                None,
                "",
                true,
            ),
        );
        assert_snapshot_matches(
            &mut ui,
            "tests/snapshots/mixer_bus_area_nonempty_add_icon_lane_flat_close",
        )?;

        Ok(())
    }

    #[test]
    fn add_bus_button_hover_is_consistent_across_whole_button() -> Result<(), iced_test::Error> {
        let view = || -> Element<'static, Message> {
            container(super::add_bus_button(true))
                .width(Length::Fixed(ui_style::grid_f32(12)))
                .height(Length::Fixed(ui_style::grid_f32(12)))
                .center_x(Length::Fixed(ui_style::grid_f32(12)))
                .center_y(Length::Fixed(ui_style::grid_f32(12)))
                .into()
        };

        let mut button_hover = Simulator::with_size(
            iced::Settings::default(),
            [ui_style::grid_f32(12), ui_style::grid_f32(12)],
            view(),
        );
        button_hover.point_at(iced::Point::new(
            ui_style::grid_f32(4),
            ui_style::grid_f32(6),
        ));

        let mut icon_hover = Simulator::with_size(
            iced::Settings::default(),
            [ui_style::grid_f32(12), ui_style::grid_f32(12)],
            view(),
        );
        icon_hover.point_at(iced::Point::new(
            ui_style::grid_f32(6),
            ui_style::grid_f32(6),
        ));

        assert_snapshots_equal(
            &mut button_hover,
            &mut icon_hover,
            "add_bus_button_hover_consistency",
        )?;

        Ok(())
    }

    #[test]
    fn instrument_slot_icon_area_opens_editor() {
        let target = crate::app::processor_editor_windows::EditorTarget {
            strip_index: 3,
            slot_index: 0,
        };
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::slot_selector_controls(
                "Violin".to_string(),
                Some(Message::Mixer(MixerMessage::OpenEditor(target))),
                Some(Message::Mixer(MixerMessage::ToggleTrackInstrumentBrowser(
                    2,
                ))),
            ),
        );

        ui.point_at(iced::Point::new(20.0, INSTRUMENT_PICKER_HEIGHT / 2.0));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::OpenEditor(clicked)) if clicked == target
            )
        }));
    }

    #[test]
    fn instrument_slot_name_area_opens_picker() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::slot_selector_controls(
                "Violin".to_string(),
                Some(Message::Mixer(MixerMessage::OpenEditor(
                    crate::app::processor_editor_windows::EditorTarget {
                        strip_index: 3,
                        slot_index: 0,
                    },
                ))),
                Some(Message::Mixer(MixerMessage::ToggleTrackInstrumentBrowser(
                    2,
                ))),
            ),
        );

        ui.point_at(iced::Point::new(
            INSTRUMENT_SLOT_WIDTH / 2.0,
            INSTRUMENT_PICKER_HEIGHT / 2.0,
        ));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleTrackInstrumentBrowser(2))
            )
        }));
    }

    #[test]
    fn hovered_effect_slot_power_area_toggles_bypass() {
        let target = crate::app::processor_editor_windows::EditorTarget {
            strip_index: 3,
            slot_index: 1,
        };
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::processor_slot_controls(
                target,
                super::ProcessorSlotRole::Effect,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_GAIN_ID.to_string(),
                    name: "Gain".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                true,
                false,
                Some(super::ProcessorSlotSegment::Bypass),
                true,
            ),
        );

        ui.point_at(iced::Point::new(16.0, INSTRUMENT_PICKER_HEIGHT / 2.0));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleSlotBypass(clicked)) if clicked == target
            )
        }));
    }

    #[test]
    fn hovered_effect_slot_list_area_opens_processor_picker() {
        let target = crate::app::processor_editor_windows::EditorTarget {
            strip_index: 3,
            slot_index: 1,
        };
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::processor_slot_controls(
                target,
                super::ProcessorSlotRole::Effect,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_GAIN_ID.to_string(),
                    name: "Gain".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                true,
                false,
                Some(super::ProcessorSlotSegment::Picker),
                true,
            ),
        );

        ui.point_at(iced::Point::new(96.0, INSTRUMENT_PICKER_HEIGHT / 2.0));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleProcessorBrowser(clicked)) if clicked == target
            )
        }));
    }

    #[test]
    fn effect_slot_click_emits_drag_start_and_drop_messages() {
        let target = crate::app::processor_editor_windows::EditorTarget {
            strip_index: 3,
            slot_index: 2,
        };
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::processor_slot_controls(
                target,
                super::ProcessorSlotRole::Effect,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_GAIN_ID.to_string(),
                    name: "Gain".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                true,
                false,
                None,
                true,
            ),
        );

        ui.point_at(iced::Point::new(24.0, INSTRUMENT_PICKER_HEIGHT / 2.0));
        let _ = ui.simulate(simulator::click());
        let messages: Vec<_> = ui.into_messages().collect();

        assert!(messages.iter().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::StartTrackEffectDrag {
                    strip_index: 3,
                    effect_index: 1,
                })
            )
        }));
        assert!(messages.iter().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::DropTrackEffect {
                    strip_index: 3,
                    effect_index: 1,
                })
            )
        }));
    }

    #[test]
    fn master_effect_slot_click_emits_drag_start_and_drop_messages() {
        let target = crate::app::processor_editor_windows::EditorTarget {
            strip_index: 0,
            slot_index: 1,
        };
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [INSTRUMENT_SLOT_WIDTH, INSTRUMENT_PICKER_HEIGHT],
            super::processor_slot_controls(
                target,
                super::ProcessorSlotRole::Effect,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_GAIN_ID.to_string(),
                    name: "Gain".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                true,
                false,
                None,
                true,
            ),
        );

        ui.point_at(iced::Point::new(24.0, INSTRUMENT_PICKER_HEIGHT / 2.0));
        let _ = ui.simulate(simulator::click());
        let messages: Vec<_> = ui.into_messages().collect();

        assert!(messages.iter().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::StartTrackEffectDrag {
                    strip_index: 0,
                    effect_index: 0,
                })
            )
        }));
        assert!(messages.iter().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::DropTrackEffect {
                    strip_index: 0,
                    effect_index: 0,
                })
            )
        }));
    }

    #[test]
    fn master_effect_rack_empty_slot_opens_master_effect_picker() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                super::EFFECT_RACK_SLOT_WIDTH,
                super::PROCESSOR_SLOT_BUTTON_HEIGHT,
            ],
            super::effect_rack(0, Vec::new(), None, None, true, 1),
        );

        ui.point_at(iced::Point::new(
            super::EFFECT_RACK_SLOT_WIDTH / 2.0,
            super::PROCESSOR_SLOT_BUTTON_HEIGHT / 2.0,
        ));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleProcessorBrowser(target))
                    if target.strip_index == 0 && target.slot_index == 1
            )
        }));
    }

    #[test]
    fn master_effect_browser_overlay_shows_effect_picker() {
        lilypalooza_builtins::register_all();
        let (mut app, _task) = crate::app::new_with_default_test_state();
        app.playback = Some(
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start"),
        );
        app.open_processor_browser_target =
            Some(crate::app::processor_editor_windows::EditorTarget {
                strip_index: 0,
                slot_index: 1,
            });

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [800.0, 600.0],
            super::instrument_browser_overlay(&app),
        );

        assert!(ui.find("Choose Effect").is_ok());
        assert!(ui.find("Master").is_ok());
        assert!(ui.find("Gain").is_ok());
    }

    #[test]
    fn effect_rack_only_first_empty_slot_opens_picker() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                super::EFFECT_RACK_SLOT_WIDTH,
                super::PROCESSOR_SLOT_BUTTON_HEIGHT * 3.0,
            ],
            super::effect_rack(1, Vec::new(), None, None, true, 3),
        );

        ui.point_at(iced::Point::new(
            super::EFFECT_RACK_SLOT_WIDTH / 2.0,
            super::PROCESSOR_SLOT_BUTTON_HEIGHT * 1.5,
        ));
        let _ = ui.simulate(simulator::click());

        assert!(!ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleProcessorBrowser(_))
            )
        }));
    }

    #[test]
    fn panel_toggle_button_emits_panel_toggle_message() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [super::STRIP_TOGGLE_SIZE, super::STRIP_TOGGLE_SIZE],
            super::strip_panel_toggle_button(
                false,
                false,
                Message::Mixer(MixerMessage::ToggleMixerEffectRack(0)),
            ),
        );

        ui.point_at(iced::Point::new(
            super::STRIP_TOGGLE_SIZE / 2.0,
            super::STRIP_TOGGLE_SIZE / 2.0,
        ));
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::ToggleMixerEffectRack(0))
            )
        }));
    }

    #[test]
    fn panel_toggle_button_style_differs_from_mute_solo_style() {
        let panel = super::strip_panel_toggle_style(&Theme::Dark, button::Status::Active, false);
        let mute_solo = ui_style::button_compact_solid(&Theme::Dark, button::Status::Active);

        assert_ne!(panel.background, mute_solo.background);
    }

    #[test]
    fn open_panel_toggle_uses_hover_style() {
        let open = super::strip_panel_toggle_style(&Theme::Dark, button::Status::Active, true);
        let hovered = ui_style::button_flat_compact_control(&Theme::Dark, button::Status::Hovered);

        assert_eq!(open.background, hovered.background);
        assert_eq!(open.border, hovered.border);
    }

    #[test]
    fn folded_panel_toggle_indicates_hidden_content() {
        let empty = super::strip_panel_toggle_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            false,
        );
        let has_content = super::strip_panel_toggle_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            true,
        );

        assert_ne!(empty.color, has_content.color);
    }

    #[test]
    fn instrument_slot_hit_button_keeps_label_text_visible() {
        let style = super::transparent_hit_button(&Theme::Dark, button::Status::Active);

        assert_ne!(
            style.text_color,
            Color::TRANSPARENT,
            "slot label text must remain visible inside the transparent hit area"
        );
    }

    #[test]
    fn instrument_slot_area_separator_is_visible() {
        let style = super::slot_area_separator_surface(&Theme::Dark);

        assert!(
            style.background.is_some(),
            "slot editor and picker areas should have a visible separator"
        );
    }

    #[test]
    fn instrument_slot_editor_area_is_compact() {
        assert!(
            super::INSTRUMENT_SLOT_EDITOR_AREA_WIDTH
                <= super::INSTRUMENT_BROWSER_ICON_SIZE + ui_style::grid_f32(1),
            "slot editor area should hug the icon instead of taking a wide chunk"
        );
    }

    #[test]
    fn effect_rack_uses_fixed_scrollable_height() {
        let rack_height = super::EFFECT_RACK_HEIGHT;
        let picker_height = super::INSTRUMENT_PICKER_HEIGHT;

        assert_eq!(
            rack_height,
            super::EFFECT_RACK_ROW_HEIGHT * super::EFFECT_RACK_VISIBLE_SLOTS as f32
        );
        assert!(rack_height > picker_height);
    }

    #[test]
    fn effect_rack_panel_is_narrower_than_channel_strip() {
        let panel_width = super::EFFECT_RACK_PANEL_WIDTH;
        let strip_width = super::STRIP_WIDTH;
        assert!(panel_width < strip_width);
    }

    #[test]
    fn effect_rack_panel_reserves_even_space_for_rack_and_routing() {
        let strip_height = 480.0;
        let (rack_height, routing_height) = super::effect_rack_panel_heights(strip_height, true);
        let available_height = strip_height - super::EFFECT_RACK_SEPARATOR_HEIGHT;

        assert!((rack_height / available_height - 0.5).abs() < 0.001);
        assert!((routing_height / available_height - 0.5).abs() < 0.001);
        assert!(
            (rack_height + routing_height + super::EFFECT_RACK_SEPARATOR_HEIGHT - strip_height)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn master_effect_rack_panel_uses_full_height_without_routing() {
        let strip_height = 480.0;
        let (rack_height, routing_height) = super::effect_rack_panel_heights(strip_height, false);

        assert_eq!(rack_height, strip_height);
        assert_eq!(routing_height, 0.0);
    }

    #[test]
    fn effect_rack_row_height_includes_one_separator() {
        assert_eq!(
            super::EFFECT_RACK_ROW_HEIGHT,
            super::PROCESSOR_SLOT_BUTTON_HEIGHT + super::EFFECT_RACK_SEPARATOR_HEIGHT
        );
    }

    #[test]
    fn effect_rack_scrollbar_is_narrow_and_reserved() {
        let width = super::EFFECT_RACK_SCROLLBAR_WIDTH;
        let scroller_width = super::EFFECT_RACK_SCROLLBAR_SCROLLER_WIDTH;
        let spacing = super::EFFECT_RACK_SCROLLBAR_SPACING;

        assert!(width < ui_style::grid_f32(2));
        assert!(scroller_width < width);
        assert!(spacing > 0.0);
    }

    #[test]
    fn effect_rack_slot_style_does_not_draw_stacked_cell_borders() {
        let style =
            super::processor_slot_button_style(&Theme::Dark, button::Status::Active, false, true);

        assert_eq!(
            style.border.width, 0.0,
            "rack rows should use explicit shared separators, not per-row top/bottom borders"
        );
    }

    #[test]
    fn effect_rack_add_button_background_is_transparent() {
        let idle = super::effect_rack_add_button_style(&Theme::Dark, button::Status::Active);
        let hovered = super::effect_rack_add_button_style(&Theme::Dark, button::Status::Hovered);

        assert_eq!(idle.background, None);
        assert_eq!(hovered.background, None);
        assert_ne!(idle.text_color, hovered.text_color);
    }

    #[test]
    fn effect_rack_slot_background_is_transparent() {
        let idle =
            super::processor_slot_button_style(&Theme::Dark, button::Status::Active, false, true);
        let hovered =
            super::processor_slot_button_style(&Theme::Dark, button::Status::Hovered, false, true);

        assert_eq!(idle.background, None);
        assert_eq!(hovered.background, None);
    }

    #[test]
    fn effect_drag_state_only_applies_to_matching_strip() {
        assert_eq!(
            super::effect_rack_drag_state(Some((1, 0)), Some((1, 2)), 1),
            Some(super::EffectRackDragState {
                source_effect_index: 0,
                target_effect_index: 2,
            })
        );
        assert_eq!(
            super::effect_rack_drag_state(Some((1, 0)), Some((2, 2)), 1),
            None
        );
        assert_eq!(
            super::effect_rack_drag_state(Some((1, 0)), Some((1, 0)), 1),
            None
        );
    }

    #[test]
    fn effect_rack_drop_indicator_tracks_insert_position() {
        assert_eq!(
            super::effect_rack_drop_indicator(Some(super::EffectRackDragState {
                source_effect_index: 0,
                target_effect_index: 2,
            })),
            Some(super::EffectRackDropIndicator::After(2))
        );
        assert_eq!(
            super::effect_rack_drop_indicator(Some(super::EffectRackDragState {
                source_effect_index: 2,
                target_effect_index: 1,
            })),
            Some(super::EffectRackDropIndicator::After(0))
        );
        assert_eq!(
            super::effect_rack_drop_indicator(Some(super::EffectRackDragState {
                source_effect_index: 2,
                target_effect_index: 0,
            })),
            Some(super::EffectRackDropIndicator::Top)
        );
    }

    #[test]
    fn effect_rack_drop_indicator_is_visually_distinct_from_separator() {
        let separator = super::effect_rack_separator_surface(&Theme::Dark);
        let indicator = super::effect_rack_drop_indicator_surface(&Theme::Dark);

        assert_ne!(separator.background, indicator.background);
    }

    #[test]
    fn effect_rack_drag_indicator_changes_rendered_rack() -> Result<(), iced_test::Error> {
        let effects = vec![
            super::EffectSlotDependency {
                slot_index: 1,
                instance_id: 10,
                instance_label_index: 1,
                selected: None,
                editor_enabled: false,
                bypassed: false,
            },
            super::EffectSlotDependency {
                slot_index: 2,
                instance_id: 11,
                instance_label_index: 2,
                selected: None,
                editor_enabled: false,
                bypassed: false,
            },
            super::EffectSlotDependency {
                slot_index: 3,
                instance_id: 12,
                instance_label_index: 3,
                selected: None,
                editor_enabled: false,
                bypassed: false,
            },
        ];
        let size = [
            super::EFFECT_RACK_SLOT_WIDTH,
            super::EFFECT_RACK_ROW_HEIGHT * 4.0,
        ];
        let mut idle = Simulator::with_size(
            iced::Settings::default(),
            size,
            super::effect_rack(1, effects.clone(), None, None, true, 4),
        );
        let mut dragging = Simulator::with_size(
            iced::Settings::default(),
            size,
            super::effect_rack(
                1,
                effects,
                None,
                Some(super::EffectRackDragState {
                    source_effect_index: 0,
                    target_effect_index: 2,
                }),
                true,
                4,
            ),
        );

        assert_snapshots_differ(
            &mut idle,
            &mut dragging,
            "effect_rack_drag_indicator_changes_rendered_rack",
        )?;

        Ok(())
    }

    #[test]
    fn effect_slot_display_labels_enumerate_duplicate_names_only() {
        let effects = vec![
            test_effect_slot(1, 10, 1, "Gain"),
            test_effect_slot(2, 11, 1, "Reverb"),
            test_effect_slot(3, 12, 2, "Gain"),
        ];

        assert_eq!(
            super::effect_slot_display_labels(&effects),
            vec![
                Some("Gain [1]".to_string()),
                Some("Reverb".to_string()),
                Some("Gain [2]".to_string())
            ]
        );
    }

    #[test]
    fn effect_slot_display_labels_follow_persistent_label_index_after_reorder() {
        let effects = vec![
            test_effect_slot(1, 12, 3, "Gain"),
            test_effect_slot(2, 10, 1, "Gain"),
            test_effect_slot(3, 11, 2, "Gain"),
        ];

        assert_eq!(
            super::effect_slot_display_labels(&effects),
            vec![
                Some("Gain [3]".to_string()),
                Some("Gain [1]".to_string()),
                Some("Gain [2]".to_string())
            ]
        );
    }

    #[test]
    fn effect_slot_display_labels_do_not_compact_after_middle_instance_removed() {
        let effects = vec![
            test_effect_slot(1, 10, 1, "Gain"),
            test_effect_slot(2, 12, 3, "Gain"),
        ];

        assert_eq!(
            super::effect_slot_display_labels(&effects),
            vec![Some("Gain [1]".to_string()), Some("Gain [3]".to_string())]
        );
    }

    #[test]
    fn effect_rack_renders_duplicate_instance_numbers() {
        let effects = vec![
            test_effect_slot(1, 10, 1, "Gain"),
            test_effect_slot(2, 11, 2, "Gain"),
            test_effect_slot(3, 12, 3, "Gain"),
        ];
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                super::EFFECT_RACK_SLOT_WIDTH,
                super::EFFECT_RACK_ROW_HEIGHT * 4.0,
            ],
            super::effect_rack(1, effects, None, None, true, 4),
        );

        assert!(ui.find("Gain [1]").is_ok());
        assert!(ui.find("Gain [2]").is_ok());
        assert!(ui.find("Gain [3]").is_ok());
    }

    #[test]
    fn processor_slot_hover_highlights_title_and_icon_together() {
        let idle_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            false,
            false,
        )
        .text_color;
        let hovered_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            false,
            true,
        )
        .text_color;
        let hovered_icon = super::processor_slot_label_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            true,
        )
        .color;

        assert_ne!(idle_text, hovered_text);
        assert_eq!(hovered_icon, Some(hovered_text));
    }

    #[test]
    fn processor_slot_hover_highlights_icon_across_clickable_area() {
        let idle_button =
            super::processor_slot_icon_button_style(&Theme::Dark, button::Status::Active, false)
                .text_color;
        let hovered_button =
            super::processor_slot_icon_button_style(&Theme::Dark, button::Status::Active, true)
                .text_color;
        let hovered_icon = super::processor_slot_active_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            true,
        )
        .color;

        assert_ne!(idle_button, hovered_button);
        assert_eq!(hovered_icon, Some(hovered_button));
    }

    #[test]
    fn processor_slot_editor_hover_does_not_highlight_picker_icon() {
        let editor_hover_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            false,
            true,
        )
        .text_color;
        let picker_idle_icon = super::processor_slot_active_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            false,
        )
        .color;

        assert_ne!(picker_idle_icon, Some(editor_hover_text));
    }

    #[test]
    fn selected_instrument_slot_accents_icon_and_name() {
        let idle_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            false,
            false,
        )
        .text_color;
        let active_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            true,
            false,
        )
        .text_color;
        let active_icon = super::processor_slot_label_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            true,
            false,
        )
        .color;

        assert_ne!(idle_text, active_text);
        assert_eq!(active_icon, Some(active_text));
    }

    #[test]
    fn selected_instrument_slot_accent_stays_readable() {
        let active_text = super::processor_slot_label_button_style(
            &Theme::Dark,
            button::Status::Active,
            true,
            false,
        )
        .text_color;
        let palette = Theme::Dark.extended_palette();
        let raw_primary = palette.primary.base.color;
        let readable_text = palette.background.weak.text;

        assert!(
            color_distance(active_text, readable_text) < color_distance(raw_primary, readable_text)
        );
    }

    #[test]
    fn bypass_icon_toggles_shape_without_state_color() {
        assert_eq!(super::processor_slot_bypass_icon(false), icons::power());
        assert_eq!(super::processor_slot_bypass_icon(true), icons::power_off());
        assert_ne!(
            super::processor_slot_bypass_icon(false),
            super::processor_slot_bypass_icon(true)
        );

        let normal = super::processor_slot_active_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            false,
            false,
        )
        .color;
        let bypassed = super::processor_slot_active_icon_style(
            &Theme::Dark,
            iced::widget::svg::Status::Idle,
            true,
            false,
        )
        .color;

        assert_eq!(normal, bypassed);
    }

    #[test]
    fn effect_rack_panel_uses_even_rack_and_routing_heights() {
        let (rack_height, routing_height) = super::effect_rack_panel_heights(301.0, true);

        assert_eq!(rack_height, 150.0);
        assert_eq!(routing_height, 150.0);
    }

    #[test]
    fn route_menus_shrink_to_item_count_until_maximum() {
        assert_eq!(
            super::route_menu_height_for_items(1),
            super::ROUTE_MENU_ITEM_HEIGHT
        );
        assert_eq!(
            super::route_menu_height_for_items(3),
            super::ROUTE_MENU_ITEM_HEIGHT * 3.0
        );
        assert_eq!(
            super::route_menu_height_for_items(super::ROUTE_MENU_MAX_ITEMS + 4),
            super::ROUTE_MENU_ITEM_HEIGHT * super::ROUTE_MENU_MAX_ITEMS as f32
        );
    }

    #[test]
    fn send_rows_reserve_a_second_line_for_gain() {
        let row_height = super::SEND_ROW_HEIGHT;
        let compact_controls_width = [super::SEND_PICKER_WIDTH, super::SEND_MODE_WIDTH]
            .iter()
            .sum::<f32>();
        let extra_spacing = [
            super::SEND_ROW_CONTENT_BOTTOM_SPACING,
            super::SEND_PANEL_TOP_SPACING,
        ]
        .iter()
        .sum::<f32>();

        assert!(row_height >= ui_style::grid_f32(12));
        assert_eq!(super::SEND_MODE_HEIGHT, super::SEND_CONTROL_HEIGHT);
        assert!(extra_spacing > 0.0);
        assert!(compact_controls_width <= super::EFFECT_RACK_PANEL_WIDTH);
    }

    #[test]
    fn send_gain_slider_double_click_resets_to_zero_db() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [super::EFFECT_RACK_PANEL_WIDTH, super::SEND_ROW_HEIGHT],
            super::send_row(
                RoutingStrip::Track(0),
                0,
                SendDependency {
                    bus_id: 1,
                    gain_bits: (-6.0f32).to_bits(),
                    enabled: true,
                    pre_fader: false,
                },
                vec![SendDestinationChoice {
                    action: SendDestinationAction::Route(1),
                    label: "Verb".to_string(),
                }],
                true,
            ),
        );

        ui.point_at(iced::Point::new(
            super::EFFECT_RACK_PANEL_WIDTH * 0.5,
            super::SEND_ROW_HEIGHT - ui_style::grid_f32(4),
        ));
        let _ = ui.simulate(simulator::click());
        let _ = ui.simulate(simulator::click());

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::SetSendGain(RoutingStrip::Track(0), 0, gain))
                    if gain == 0.0
            )
        }));
    }

    #[test]
    fn send_gain_slider_drag_changes_value() {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [super::EFFECT_RACK_PANEL_WIDTH, super::SEND_ROW_HEIGHT],
            super::send_row(
                RoutingStrip::Track(0),
                0,
                SendDependency {
                    bus_id: 1,
                    gain_bits: (-6.0f32).to_bits(),
                    enabled: true,
                    pre_fader: false,
                },
                vec![SendDestinationChoice {
                    action: SendDestinationAction::Route(1),
                    label: "Verb".to_string(),
                }],
                true,
            ),
        );
        let start = Point::new(
            ui_style::grid_f32(8),
            super::SEND_ROW_HEIGHT - ui_style::grid_f32(4),
        );
        let end = Point::new(
            super::EFFECT_RACK_PANEL_WIDTH - ui_style::grid_f32(8),
            super::SEND_ROW_HEIGHT - ui_style::grid_f32(4),
        );

        ui.point_at(start);
        let _ = ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        ui.point_at(end);
        let _ = ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: end })]);
        let _ = ui.simulate([Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))]);

        assert!(ui.into_messages().any(|message| {
            matches!(
                message,
                Message::Mixer(MixerMessage::SetSendGain(RoutingStrip::Track(0), 0, gain))
                    if gain != -6.0 && gain != 0.0
            )
        }));
    }

    #[test]
    fn strip_minimum_height_covers_route_footer() {
        let fixed_controls = super::INSTRUMENT_PICKER_HEIGHT
            + super::STRIP_TOGGLE_SIZE
            + super::STRIP_FOOTER_HEIGHT
            + (ui_style::PADDING_SM as f32 * 2.0);

        assert!(super::STRIP_MIN_HEIGHT > fixed_controls + ui_style::grid_f32(16));
        assert!(
            super::MIXER_MIN_HEIGHT
                >= super::STRIP_MIN_HEIGHT
                    + super::SECTION_HEADER_HEIGHT
                    + (ui_style::PADDING_SM as f32 * 2.0)
        );
    }

    #[test]
    fn strip_minimum_height_covers_fader_controls_and_route_footer() {
        let pan_stack_height = ui_style::SPACE_XS as f32
            + super::VALUE_LABEL_HEIGHT
            + 40.0
            + (super::LABEL_CONTROL_SPACING * 2.0);
        let gain_stack_height = super::control_stack_height(96.0);
        let content_height = super::INSTRUMENT_PICKER_HEIGHT
            + pan_stack_height
            + gain_stack_height
            + super::STRIP_TOGGLE_SIZE
            + (super::STRIP_STACK_SPACING * 3.0);
        let required_strip_height =
            content_height + super::STRIP_FOOTER_HEIGHT + (ui_style::PADDING_SM as f32 * 2.0);

        assert!(super::STRIP_MIN_HEIGHT >= required_strip_height);
    }

    #[test]
    fn output_picker_bottom_inset_matches_instrument_top_inset() {
        assert_eq!(super::ROUTE_PICKER_BOTTOM_INSET, 0.0);
    }

    #[test]
    fn route_picker_closed_button_hides_native_left_label_for_centered_overlay() {
        let style = super::route_pick_list_centered_style(
            &Theme::Dark,
            iced::widget::pick_list::Status::Active,
        );

        assert_eq!(style.text_color, Color::TRANSPARENT);
        assert_eq!(style.placeholder_color, Color::TRANSPARENT);
    }

    #[test]
    fn fader_height_accounts_for_route_footer() {
        let strip_height = 360.0;
        let expected = (strip_height
            - (ui_style::PADDING_SM as f32 * 2.0)
            - super::SECTION_HEADER_HEIGHT
            - super::INSTRUMENT_PICKER_HEIGHT
            - super::STRIP_TOGGLE_SIZE
            - super::STRIP_FOOTER_HEIGHT
            - 30.0
            - (super::VALUE_LABEL_HEIGHT * 3.0)
            - (ui_style::SPACE_XS as f32 * 6.0))
            .max(96.0);

        assert_eq!(
            super::gain_control_height(strip_height, super::GainControlMode::Fader),
            expected
        );
    }

    #[test]
    fn route_choices_follow_added_removed_buses_and_exclude_source_bus() {
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let (verb, delay) = {
            let mut mixer = playback.mixer();
            let verb = mixer.add_bus("Verb").expect("bus should be added");
            let delay = mixer.add_bus("Delay").expect("bus should be added");
            (verb, delay)
        };
        let mixer = playback.mixer_state().clone();

        let track_choices = super::route_choices(&mixer, RoutingStrip::Track(0));
        assert_eq!(
            track_choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Master", "Verb", "Delay"]
        );

        let bus_choices = super::route_choices(&mixer, RoutingStrip::Bus(verb.0));
        assert!(
            !bus_choices
                .iter()
                .any(|choice| choice.route == TrackRoute::Bus(verb))
        );
        assert!(
            bus_choices
                .iter()
                .any(|choice| choice.route == TrackRoute::Bus(delay))
        );

        playback
            .mixer()
            .remove_bus(delay)
            .expect("bus removal should succeed");
        let mixer = playback.mixer_state().clone();
        let track_choices = super::route_choices(&mixer, RoutingStrip::Track(0));
        assert!(
            !track_choices
                .iter()
                .any(|choice| choice.route == TrackRoute::Bus(delay))
        );
    }

    #[test]
    fn send_destination_choices_exclude_source_bus() {
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let (verb, delay) = {
            let mut mixer = playback.mixer();
            let verb = mixer.add_bus("Verb").expect("bus should be added");
            let delay = mixer.add_bus("Delay").expect("bus should be added");
            (verb, delay)
        };
        let mixer = playback.mixer_state().clone();

        let choices = super::send_destination_choices(&mixer, RoutingStrip::Bus(verb.0));

        assert_eq!(
            choices
                .iter()
                .filter_map(|choice| match choice.action {
                    super::SendDestinationAction::Route(bus_id) => Some(bus_id),
                    super::SendDestinationAction::Remove => None,
                })
                .collect::<Vec<_>>(),
            vec![delay.0]
        );
    }

    #[test]
    fn route_and_send_choices_exclude_feedback_destinations() {
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let (verb, delay) = {
            let mut mixer = playback.mixer();
            let verb = mixer.add_bus("Verb").expect("bus should be added");
            let delay = mixer.add_bus("Delay").expect("bus should be added");
            mixer
                .set_bus_route(verb, TrackRoute::Bus(delay))
                .expect("forward route should be allowed");
            (verb, delay)
        };
        let mixer = playback.mixer_state().clone();

        let delay_route_choices = super::route_choices(&mixer, RoutingStrip::Bus(delay.0));
        assert!(
            !delay_route_choices
                .iter()
                .any(|choice| choice.route == TrackRoute::Bus(verb))
        );

        let delay_send_choices =
            super::send_destination_choices(&mixer, RoutingStrip::Bus(delay.0));
        assert!(!delay_send_choices.iter().any(|choice| {
            matches!(choice.action, super::SendDestinationAction::Route(bus_id) if bus_id == verb.0)
        }));
    }

    #[test]
    fn send_menu_adds_remove_action_before_bus_choices() {
        let choices = super::send_menu_choices(vec![super::SendDestinationChoice {
            action: super::SendDestinationAction::Route(7),
            label: "Verb".to_string(),
        }]);

        assert!(matches!(
            choices[0].action,
            super::SendDestinationAction::Remove
        ));
        assert!(matches!(
            choices[1].action,
            super::SendDestinationAction::Route(7)
        ));
    }

    #[test]
    fn route_choices_reflect_renamed_buses() {
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        let bus_id = playback
            .mixer()
            .add_bus("Verb")
            .expect("bus should be added");
        playback
            .mixer()
            .set_bus_name(bus_id, "Long Hall")
            .expect("bus should be renamed");

        let choices = super::route_choices(playback.mixer_state(), RoutingStrip::Track(0));

        assert!(choices.iter().any(|choice| choice.label == "Long Hall"));
        assert!(!choices.iter().any(|choice| choice.label == "Verb"));
    }

    #[test]
    fn track_effect_rack_panel_matches_snapshot() -> Result<(), iced_test::Error> {
        lilypalooza_builtins::register_all();
        let mut playback =
            AudioEngine::start_cpal(MixerState::new(), AudioEngineOptions::default())
                .expect("test audio engine should start");
        playback
            .mixer()
            .set_track_effects(
                lilypalooza_audio::TrackId(0),
                vec![SlotState::built_in(
                    BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState::default(),
                )],
            )
            .expect("effect should be installed");
        let mixer = playback.mixer_state().clone();

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [super::EFFECT_RACK_PANEL_WIDTH, 360.0],
            super::track_effect_rack_panel(
                1,
                super::effect_slot_dependencies(&mixer.tracks()[0]),
                Some(super::EffectRackPanelRouting {
                    source: RoutingStrip::Track(0),
                    sends: super::send_dependencies(&mixer.tracks()[0].routing),
                    send_choices: super::send_destination_choices(&mixer, RoutingStrip::Track(0)),
                }),
                None,
                None,
                true,
                360.0,
            ),
        );

        assert_snapshot_matches(&mut ui, "tests/snapshots/track_effect_rack_panel")?;

        Ok(())
    }

    #[test]
    fn instrument_track_area_open_panel_matches_snapshot() -> Result<(), iced_test::Error> {
        let mixer = MixerState::new();
        let meters = MixerMeterSnapshotWindow::default();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                STRIP_WIDTH + super::EFFECT_RACK_PANEL_WIDTH + ui_style::grid_f32(8),
                520.0,
            ],
            super::instrument_track_area(
                &mixer,
                &meters,
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                0..1,
                1,
                &[crate::track_colors::default_track_color(0)],
                None,
                None,
                "",
                Color::TRANSPARENT,
                false,
                None,
                None,
                None,
                None,
                &[1],
                true,
            ),
        );

        assert_snapshot_matches(&mut ui, "tests/snapshots/instrument_track_area_open_panel")?;

        Ok(())
    }

    #[test]
    fn master_track_area_open_panel_matches_snapshot() -> Result<(), iced_test::Error> {
        let mixer = MixerState::new();
        let colors = meter_colors(&Theme::Dark);

        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [
                MAIN_SECTION_WIDTH + super::EFFECT_RACK_PANEL_WIDTH,
                STRIP_MIN_HEIGHT + ui_style::grid_f32(8),
            ],
            super::master_track_area(
                &mixer,
                StripMeterSnapshot::default(),
                colors,
                STRIP_MIN_HEIGHT,
                GainControlMode::Knob,
                &[0],
                None,
                None,
                true,
            ),
        );

        assert_snapshot_matches(&mut ui, "tests/snapshots/master_track_area_open_panel")?;

        Ok(())
    }

    #[test]
    fn mixer_strip_min_height_no_longer_reserves_per_track_effect_rack() {
        assert!(
            super::gain_control_height(360.0, super::GainControlMode::Fader) >= 96.0,
            "effect rack lives in the side panel while the strip footer still needs routing space"
        );
    }

    #[test]
    fn instrument_slot_hit_button_does_not_draw_segment_hover() {
        let idle = super::transparent_hit_button(&Theme::Dark, button::Status::Active);
        let hovered = super::transparent_hit_button(&Theme::Dark, button::Status::Hovered);

        assert_eq!(
            idle.background, hovered.background,
            "slot hit areas should not draw rectangular segment hover"
        );
    }

    #[test]
    fn instrument_slot_hit_button_has_split_foreground_hover() {
        let idle = super::transparent_hit_button(&Theme::Dark, button::Status::Active);
        let hovered = super::transparent_hit_button(&Theme::Dark, button::Status::Hovered);

        assert_ne!(
            idle.text_color, hovered.text_color,
            "slot hit areas should highlight only their own foreground"
        );
    }

    #[test]
    fn instrument_slot_text_foreground_matches_icon_foreground() {
        let idle_text =
            super::transparent_hit_button(&Theme::Dark, button::Status::Active).text_color;
        let idle_icon = ui_style::svg_muted_control(&Theme::Dark, iced::widget::svg::Status::Idle)
            .color
            .expect("muted icon idle color should exist");
        assert_eq!(idle_text, idle_icon);

        let hovered_text =
            super::transparent_hit_button(&Theme::Dark, button::Status::Hovered).text_color;
        let hovered_icon =
            ui_style::svg_muted_control(&Theme::Dark, iced::widget::svg::Status::Hovered)
                .color
                .expect("muted icon hover color should exist");
        assert_eq!(hovered_text, hovered_icon);
    }

    #[test]
    fn hovered_processor_label_is_truncated() {
        let label = super::processor_hover_label("Very Long Effect Processor Name", false);

        assert!(label.chars().count() <= super::PROCESSOR_SLOT_LABEL_MAX_LEN);
        assert_ne!(label, "Very Long Effect Processor Name");
    }

    #[test]
    fn effect_rack_processor_label_is_truncated_to_split_slot_width() {
        let label = super::processor_hover_label("Very Long Effect Processor Name", true);

        assert!(label.chars().count() <= super::PROCESSOR_SLOT_LABEL_MAX_LEN);
        assert_ne!(label, "Very Long Effect Processor Name");
    }

    #[test]
    fn instrument_slot_surface_has_whole_button_hover_reaction() {
        let idle = ui_style::button_selector_field(&Theme::Dark, button::Status::Active, false);
        let hovered = ui_style::button_selector_field(&Theme::Dark, button::Status::Hovered, false);

        assert_ne!(
            idle.background, hovered.background,
            "slot surface should still have a whole-button hover reaction"
        );
    }

    #[test]
    fn instrument_browser_tabs_match_snapshot() -> Result<(), iced_test::Error> {
        let mut ui = Simulator::with_size(
            iced::Settings::default(),
            [ui_style::grid_f32(48), ui_style::grid_f32(8)],
            row![
                super::instrument_browser_tab_button(
                    InstrumentBrowserBackend::BuiltIn,
                    InstrumentBrowserBackend::BuiltIn,
                ),
                super::instrument_browser_tab_button(
                    InstrumentBrowserBackend::Clap,
                    InstrumentBrowserBackend::BuiltIn,
                ),
                super::instrument_browser_tab_button(
                    InstrumentBrowserBackend::Vst3,
                    InstrumentBrowserBackend::BuiltIn,
                ),
            ]
            .spacing(ui_style::SPACE_XS),
        );

        assert_snapshot_matches(&mut ui, "tests/snapshots/instrument_browser_tabs")?;

        Ok(())
    }

    #[test]
    fn remove_bus_button_idle_and_hover_render_differ() -> Result<(), iced_test::Error> {
        let view = || -> Element<'static, Message> {
            container(
                ui_style::flat_icon_button(
                    icons::x(),
                    ui_style::grid_f32(4),
                    ui_style::grid_f32(3),
                    ui_style::button_flat_compact_control,
                    ui_style::svg_dimmed_control,
                )
                .on_press(Message::Noop),
            )
            .width(Length::Fixed(ui_style::grid_f32(8)))
            .height(Length::Fixed(ui_style::grid_f32(8)))
            .center_x(Length::Fixed(ui_style::grid_f32(8)))
            .center_y(Length::Fixed(ui_style::grid_f32(8)))
            .into()
        };

        let mut idle = Simulator::with_size(
            iced::Settings::default(),
            [ui_style::grid_f32(8), ui_style::grid_f32(8)],
            view(),
        );

        let mut hover = Simulator::with_size(
            iced::Settings::default(),
            [ui_style::grid_f32(8), ui_style::grid_f32(8)],
            view(),
        );
        hover.point_at(iced::Point::new(
            ui_style::grid_f32(4),
            ui_style::grid_f32(4),
        ));

        assert_snapshots_differ(
            &mut idle,
            &mut hover,
            "remove_bus_button_idle_hover_difference",
        )?;

        Ok(())
    }

    #[test]
    fn empty_slot_maps_to_none_choice() {
        let mixer = MixerState::new();
        assert_eq!(
            selected_instrument_choice(Some(&SlotState::default()), &mixer),
            Some(InstrumentChoice::None)
        );
    }

    #[test]
    fn soundfont_slot_maps_to_soundfont_choice() {
        lilypalooza_builtins::register_all();
        let mixer = MixerState::new();
        assert_eq!(
            selected_instrument_choice(
                Some(&SlotState::built_in(
                    BUILTIN_SOUNDFONT_ID,
                    lilypalooza_builtins::soundfont_synth::state("default", 0, 2),
                )),
                &mixer
            ),
            Some(InstrumentChoice::Processor {
                processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
                name: "SF-01".to_string(),
                backend: InstrumentBrowserBackend::BuiltIn,
            })
        );
    }

    #[test]
    fn processor_choices_filter_instruments_and_effects_by_role() {
        lilypalooza_builtins::register_all();

        let instruments = processor_choices(super::ProcessorSlotRole::Instrument);
        let effects = processor_choices(super::ProcessorSlotRole::Effect);

        assert!(instruments.iter().any(|choice| matches!(
            choice,
            InstrumentChoice::Processor { processor_id, .. } if processor_id == BUILTIN_SOUNDFONT_ID
        )));
        assert!(!instruments.iter().any(|choice| matches!(
            choice,
            InstrumentChoice::Processor { processor_id, .. } if processor_id == BUILTIN_GAIN_ID
        )));
        assert!(effects.iter().any(|choice| matches!(
            choice,
            InstrumentChoice::Processor { processor_id, .. } if processor_id == BUILTIN_GAIN_ID
        )));
        assert!(!effects.iter().any(|choice| matches!(
            choice,
            InstrumentChoice::Processor { processor_id, .. } if processor_id == BUILTIN_SOUNDFONT_ID
        )));
    }

    #[test]
    fn gain_effect_slot_maps_to_effect_choice() {
        lilypalooza_builtins::register_all();

        assert_eq!(
            selected_processor_choice(
                Some(&SlotState::built_in(
                    BUILTIN_GAIN_ID,
                    lilypalooza_audio::ProcessorState::default(),
                )),
                super::ProcessorSlotRole::Effect,
            ),
            Some(InstrumentChoice::Processor {
                processor_id: BUILTIN_GAIN_ID.to_string(),
                name: "Gain".to_string(),
                backend: InstrumentBrowserBackend::BuiltIn,
            })
        );
    }

    #[test]
    fn built_in_browser_entries_filter_by_instrument_name_without_section_headers() {
        let choices = vec![
            InstrumentChoice::None,
            InstrumentChoice::Processor {
                processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
                name: "SoundFont".to_string(),
                backend: InstrumentBrowserBackend::BuiltIn,
            },
        ];

        let browser =
            instrument_browser_entries(&choices, InstrumentBrowserBackend::BuiltIn, "sound");

        assert!(!browser.show_none);
        assert_eq!(
            browser.entries,
            vec![InstrumentChoice::Processor {
                processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
                name: "SoundFont".to_string(),
                backend: InstrumentBrowserBackend::BuiltIn,
            }]
        );
    }

    #[test]
    fn instrument_trigger_label_uses_none_and_truncates_long_names() {
        assert_eq!(instrument_trigger_label(None), "Empty");

        let label = instrument_trigger_label(Some(&InstrumentChoice::Processor {
            processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
            name: "Extremely Long SoundFont Synth Name".to_string(),
            backend: InstrumentBrowserBackend::BuiltIn,
        }));
        assert!(label.chars().count() <= super::PROCESSOR_SLOT_LABEL_MAX_LEN);
        assert_ne!(label, "Extremely Long SoundFont Synth Name");
    }

    #[test]
    fn instrument_trigger_label_uses_none_for_empty_choice() {
        assert_eq!(
            instrument_trigger_label(Some(&InstrumentChoice::None)),
            "Empty"
        );
    }

    #[test]
    fn instrument_slot_primary_action_opens_editor_when_available() {
        assert!(matches!(
            instrument_slot_primary_action(
                2,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
                    name: "SoundFont".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                true,
                true,
            ),
            Some(crate::app::messages::Message::Mixer(
                crate::app::messages::MixerMessage::OpenEditor(_)
            ))
        ));
    }

    #[test]
    fn instrument_slot_primary_action_opens_picker_only_when_empty() {
        assert!(matches!(
            instrument_slot_primary_action(2, Some(&InstrumentChoice::None), false, true),
            Some(crate::app::messages::Message::Mixer(
                crate::app::messages::MixerMessage::ToggleProcessorBrowser(target)
            )) if target.strip_index == 3 && target.slot_index == 0
        ));
        assert!(
            instrument_slot_primary_action(
                2,
                Some(&InstrumentChoice::Processor {
                    processor_id: BUILTIN_SOUNDFONT_ID.to_string(),
                    name: "SoundFont".to_string(),
                    backend: InstrumentBrowserBackend::BuiltIn,
                }),
                false,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn short_mixer_height_uses_gain_knob_mode() {
        assert_eq!(gain_control_mode(MIXER_MIN_HEIGHT), GainControlMode::Knob);
        assert_eq!(
            gain_control_mode(MIXER_MIN_HEIGHT - 10.0),
            GainControlMode::Knob
        );
        assert_eq!(
            gain_control_mode(MIXER_MIN_HEIGHT + COMPACT_GAIN_SWITCH_OFFSET - 1.0),
            GainControlMode::Knob
        );
        assert_eq!(
            gain_control_mode(MIXER_MIN_HEIGHT + COMPACT_GAIN_SWITCH_OFFSET + 1.0),
            GainControlMode::Fader
        );
    }

    #[test]
    fn empty_toggle_slots_use_track_toggle_size() {
        assert_eq!(STRIP_TOGGLE_SIZE, INSTRUMENT_PICKER_HEIGHT - 4.0);
    }

    #[test]
    fn meter_and_gain_controls_share_same_height() {
        let strip_height = 280.0;
        assert_eq!(
            meter_control_height(strip_height, GainControlMode::Fader),
            gain_control_height(strip_height, GainControlMode::Fader)
        );
        assert_eq!(
            meter_control_height(strip_height, GainControlMode::Knob),
            gain_control_height(strip_height, GainControlMode::Knob)
        );
        assert_eq!(
            control_stack_height(meter_control_height(strip_height, GainControlMode::Fader)),
            control_stack_height(gain_control_height(strip_height, GainControlMode::Fader))
        );
    }

    #[test]
    fn meter_peak_label_uses_hold_and_floor() {
        assert_eq!(meter_peak_label(StripMeterSnapshot::default()), "-inf");
        let snapshot = StripMeterSnapshot {
            left: ChannelMeterSnapshot {
                level: 0.2,
                hold: 1.0,
                hold_db: 3.2,
            },
            right: ChannelMeterSnapshot {
                level: 0.2,
                hold: 0.5,
                hold_db: -6.0,
            },
            clip_latched: false,
        };
        assert_eq!(meter_peak_label(snapshot), "3.2");
    }

    #[test]
    fn gain_label_uses_negative_infinity_at_floor() {
        assert_eq!(gain_label(-60.0), "-inf");
        assert_eq!(gain_label(-24.0), "-24.0");
    }

    #[test]
    fn main_section_width_includes_group_borders() {
        assert_eq!(
            MAIN_SECTION_WIDTH,
            MAIN_STRIP_WIDTH + GROUP_SIDE_BORDER_WIDTH * 2.0
        );
    }

    #[test]
    fn value_labels_use_shared_slot_height() {
        assert!(is_grid_multiple(VALUE_LABEL_HEIGHT));
    }

    #[test]
    fn control_stack_height_adds_shared_label_slot() {
        let control: f32 = 100.0;
        assert_eq!(
            control_stack_height(control),
            control + VALUE_LABEL_HEIGHT + ui_style::SPACE_XS as f32
        );
    }

    #[test]
    fn compact_gain_mode_hides_meter_scale() {
        assert!(!meter_scale_visible(GainControlMode::Knob));
        assert!(meter_scale_visible(GainControlMode::Fader));
    }

    #[test]
    fn visible_strip_window_limits_rendered_strip_count() {
        let visible = visible_strip_window(128, 0.0, STRIP_WIDTH * 4.0);
        assert!(visible.end - visible.start <= 4 + STRIP_VIRTUALIZATION_OVERSCAN * 2);

        let scrolled = visible_strip_window(128, STRIP_WIDTH * 40.0, STRIP_WIDTH * 4.0);
        assert!(scrolled.start >= 38);
        assert!(scrolled.end <= 46);
    }

    #[test]
    fn only_existing_roll_tracks_use_tint() {
        assert!(track_should_use_roll_tint(0, 4));
        assert!(track_should_use_roll_tint(3, 4));
        assert!(!track_should_use_roll_tint(4, 4));
        assert!(!track_should_use_roll_tint(127, 4));
    }

    #[test]
    fn track_strip_dependency_includes_tint_state() {
        let base = TrackStripDependency {
            index: 0,
            name: "Track".to_string(),
            selected: Some(InstrumentChoice::None),
            editor_enabled: false,
            effects: Vec::new(),
            hovered_processor_slot: None,
            color_bits: color_bits(iced::Color::from_rgb(0.1, 0.2, 0.3)),
            gain_bits: 0.0f32.to_bits(),
            pan_bits: 0.0f32.to_bits(),
            route: RouteChoice {
                route: TrackRoute::Master,
                label: "Master".to_string(),
            },
            route_choices: vec![RouteChoice {
                route: TrackRoute::Master,
                label: "Master".to_string(),
            }],
            meter: MeterDependency::from_snapshot(StripMeterSnapshot::default()),
            compact_gain: false,
            effect_rack_open: false,
            panel_has_content: false,
            strip_height_bits: 140.0f32.to_bits(),
            soloed: false,
            muted: false,
            tint_enabled: false,
            highlighted: false,
            renaming: false,
            rename_value: String::new(),
            color_picker_open: false,
        };
        let tinted = TrackStripDependency {
            tint_enabled: true,
            ..base.clone()
        };

        assert_ne!(base, tinted);
    }

    #[test]
    fn strip_lazy_dependencies_include_panel_open_state() {
        let master_closed = super::MainStripDependency {
            gain_bits: 0.0f32.to_bits(),
            pan_bits: 0.0f32.to_bits(),
            meter: MeterDependency::from_snapshot(StripMeterSnapshot::default()),
            compact_gain: false,
            effect_rack_open: false,
            panel_has_content: false,
            strip_height_bits: 240.0f32.to_bits(),
        };
        let master_open = super::MainStripDependency {
            effect_rack_open: true,
            ..master_closed.clone()
        };

        let track_closed = TrackStripDependency {
            index: 0,
            name: "Track".to_string(),
            selected: Some(InstrumentChoice::None),
            editor_enabled: false,
            effects: Vec::new(),
            hovered_processor_slot: None,
            color_bits: color_bits(iced::Color::from_rgb(0.1, 0.2, 0.3)),
            gain_bits: 0.0f32.to_bits(),
            pan_bits: 0.0f32.to_bits(),
            route: RouteChoice {
                route: TrackRoute::Master,
                label: "Master".to_string(),
            },
            route_choices: vec![RouteChoice {
                route: TrackRoute::Master,
                label: "Master".to_string(),
            }],
            meter: MeterDependency::from_snapshot(StripMeterSnapshot::default()),
            compact_gain: false,
            effect_rack_open: false,
            panel_has_content: false,
            strip_height_bits: 240.0f32.to_bits(),
            soloed: false,
            muted: false,
            tint_enabled: false,
            highlighted: false,
            renaming: false,
            rename_value: String::new(),
            color_picker_open: false,
        };
        let track_open = TrackStripDependency {
            effect_rack_open: true,
            ..track_closed.clone()
        };

        assert_ne!(master_closed, master_open);
        assert_ne!(track_closed, track_open);
    }
}
