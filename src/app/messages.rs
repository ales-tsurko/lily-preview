use std::path::PathBuf;

use editor_host::WindowSnapshot;
use iced::event;
use iced::time::Instant;
use iced::widget::{pane_grid, text_editor};
use iced::{Color, Size, keyboard, mouse, window};
use iced_code_editor::Message as EditorWidgetMessage;
use iced_core::image;

use super::{
    EditorFileMenuSection, EditorHeaderMenuSection, ProjectMenuSection, WorkspacePaneKind, mixer,
};
#[derive(Debug, Clone)]
pub(super) enum Message {
    Noop,
    StartupChecked(Result<crate::lilypond::VersionCheck, String>),
    BrowserHistoryCleanupFinished(Result<(), String>),
    PluginScanCacheSaved(Result<(), String>),
    Pane(PaneMessage),
    File(FileMessage),
    Viewer(ViewerMessage),
    ScorePreviewReady(Result<ScorePreviewReady, String>),
    CompileOutputsReady(CompileOutputsReady),
    PianoRoll(PianoRollMessage),
    Mixer(MixerMessage),
    Editor(EditorMessage),
    Logger(LoggerMessage),
    Shortcuts(ShortcutsMessage),
    Prompt(PromptMessage),
    WindowSnapshotCaptured {
        window_id: window::Id,
        host: Result<WindowSnapshot, String>,
        parent: Result<WindowSnapshot, String>,
    },
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    WindowFocused(window::Id),
    KeyPressed(KeyPress),
    TrackRenameFocusChanged(bool),
    ModifiersChanged(keyboard::Modifiers),
    PrimaryMousePressed(bool),
    Tick,
    Frame(Instant),
    WindowResized {
        window_id: window::Id,
        size: Size,
    },
    WindowCloseRequested(window::Id),
}

#[derive(Debug, Clone)]
pub(super) struct KeyPress {
    pub(super) status: event::Status,
    pub(super) key: keyboard::Key,
    pub(super) physical_key: keyboard::key::Physical,
    pub(super) modifiers: keyboard::Modifiers,
}

#[derive(Debug, Clone)]
pub(super) struct ScorePreviewReady {
    pub(super) page_index: usize,
    pub(super) zoom: f32,
    pub(super) tier: super::ScoreZoomPreviewTier,
    pub(super) handle: image::Handle,
}

#[derive(Debug, Clone)]
pub(super) struct CompileOutputsReady {
    pub(super) generation: u64,
    pub(super) result: Result<super::LoadedCompileOutputs, String>,
}

#[derive(Debug, Clone)]
pub(super) enum PaneMessage {
    WorkspaceResized(pane_grid::ResizeEvent),
    WorkspaceTabPressed(WorkspacePaneKind),
    FocusWorkspacePane(WorkspacePaneKind),
    WorkspaceTabHovered(Option<WorkspacePaneKind>),
    OpenHeaderOverflowMenu(u64),
    SetEditorHeaderMenuSection(Option<EditorHeaderMenuSection>),
    HoverEditorFileMenuSection {
        section: Option<EditorFileMenuSection>,
        expanded: bool,
    },
    ToggleProjectMenu,
    CloseProjectMenu,
    SetProjectMenuSection(Option<ProjectMenuSection>),
    SetProjectRecentOpen(bool),
    TooltipHovered(Option<String>),
    CloseHeaderOverflowMenu,
    ToggleWorkspacePane(WorkspacePaneKind),
    WorkspaceDragMoved(iced::Point),
    WorkspaceDragReleased,
    WorkspaceDragExited,
}

#[derive(Debug, Clone)]
pub(super) enum FileMessage {
    RequestOpen,
    Picked(Option<PathBuf>),
    RequestCreateProject,
    RequestSaveProject,
    RequestLoadProject,
    CreateProjectPicked(Option<PathBuf>),
    LoadProjectPicked(Option<PathBuf>),
    OpenRecentProject(PathBuf),
}

#[derive(Debug, Clone)]
pub(super) enum EditorMessage {
    Widget {
        tab_id: u64,
        message: EditorWidgetMessage,
    },
    ActiveWidgetMessage(EditorWidgetMessage),
    NewRequested,
    TabPressed(u64),
    TabMoved {
        tab_id: u64,
        position: iced::Point,
    },
    TabGlobalMoved(iced::Point),
    TabHovered(Option<u64>),
    TabBarMoved(iced::Point),
    TabBarScrolled(iced::widget::scrollable::Viewport),
    TabBarEmptyMoved,
    StartRename(u64),
    RenameInputChanged(String),
    CommitRename,
    CancelRename,
    TabDragReleased,
    TabDragExited,
    CloseTabRequested(u64),
    RenameRequested,
    RenamePicked(Option<PathBuf>),
    OpenRequested,
    OpenPicked(Option<Vec<PathBuf>>),
    ToggleFileBrowser,
    FileBrowserFocused,
    FileBrowserToggleHiddenRequested,
    FileBrowserCutRequested,
    FileBrowserCopyRequested,
    FileBrowserPasteRequested,
    FileBrowserNewFileRequested,
    FileBrowserNewDirectoryRequested,
    FileBrowserRenameRequested,
    FileBrowserInlineEditChanged(String),
    CommitFileBrowserInlineEdit,
    CancelFileBrowserInlineEdit,
    FileBrowserTrashRequested,
    FileBrowserScrolled(iced::widget::scrollable::Viewport),
    FileBrowserColumnScrolled {
        column_index: usize,
        viewport: iced::widget::scrollable::Viewport,
    },
    FileBrowserEntryPressed {
        column_index: usize,
        path: PathBuf,
        is_dir: bool,
    },
    FileBrowserEntryHovered {
        column_index: usize,
        path: PathBuf,
        is_dir: bool,
    },
    FileBrowserEntryDragReleased {
        path: PathBuf,
        is_dir: bool,
    },
    FileBrowserEntryDoublePressed {
        column_index: usize,
        path: PathBuf,
        is_dir: bool,
    },
    FileBrowserDragMoved(iced::Point),
    FileBrowserDragReleased,
    OpenRecent(PathBuf),
    SaveRequested,
    SaveAsRequested,
    SaveAsPicked(Option<PathBuf>),
    SetCenterCursor(bool),
    ZoomIn,
    ZoomOut,
    ResetZoom,
    SetThemeHueOffsetDegrees(f32),
    SetThemeSaturation(f32),
    SetThemeWarmth(f32),
    SetThemeBrightness(f32),
    SetThemeTextDim(f32),
    SetThemeCommentDim(f32),
}

#[derive(Debug, Clone)]
pub(super) enum LoggerMessage {
    RequestClear,
    TextAction(text_editor::Action),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ViewerMessage {
    ScrollUp,
    ScrollDown,
    ScrollPositionChanged { x: f32, y: f32 },
    ViewportCursorMoved(iced::Point),
    ViewportCursorLeft,
    OpenPointAndClick,
    PrevPage,
    NextPage,
    ZoomIn,
    ZoomOut,
    SmoothZoom(mouse::ScrollDelta),
    DecreasePageBrightness,
    IncreasePageBrightness,
    ResetZoom,
    ResetPageBrightness,
}

#[derive(Debug, Clone)]
pub(super) enum PianoRollMessage {
    ViewportCursorMoved(iced::Point),
    ViewportCursorLeft,
    RollScrolled { x: f32, y: f32 },
    SetCursorTicks(u64),
    SetRewindFlagTicks(u64),
    ZoomIn,
    ZoomOut,
    SmoothZoom(mouse::ScrollDelta),
    ResetZoom,
    BeatSubdivisionSliderChanged(u8),
    BeatSubdivisionInputChanged(String),
    FilePrevious,
    FileNext,
    SelectTrack(usize),
    StartTrackRename(usize),
    OpenTrackColorPickerForTrack(usize),
    TrackRenameInputChanged(String),
    OpenTrackColorPicker,
    SubmitTrackColor(Color),
    PreviewTrackColor(Color),
    CommitTrackRename,
    CancelTrackRename,
    TrackPanelToggle,
    TrackPanelResizedBy(f32),
    TrackMuteToggled(usize),
    TrackSoloToggled(usize),
    TransportSeekNormalized(f32),
    TransportSeekReleased,
    TransportPlayPause,
    TransportRewind,
    TransportToggleMetronome,
    TransportOpenMetronomeMenu,
    TransportCloseMetronomeMenu,
    TransportMetronomeGainChanged(f32),
    TransportMetronomePitchChanged(f32),
}

#[derive(Debug, Clone)]
pub(super) enum MixerMessage {
    AddBus,
    RemoveBus(u16),
    InstrumentViewportScrolled(iced::widget::scrollable::Viewport),
    BusViewportScrolled(iced::widget::scrollable::Viewport),
    EffectRackViewportScrolled {
        strip_index: usize,
        viewport: iced::widget::scrollable::Viewport,
    },
    ResetMasterMeter,
    SetMasterGain(f32),
    SetMasterPan(f32),
    ResetTrackMeter(usize),
    SetTrackGain(usize, f32),
    SetTrackPan(usize, f32),
    SetMainRoute(
        super::mixer::RoutingStrip,
        lilypalooza_audio::mixer::TrackRoute,
    ),
    AddSend(super::mixer::RoutingStrip, u16),
    SetSendDestination(super::mixer::RoutingStrip, usize, u16),
    SetSendGain(super::mixer::RoutingStrip, usize, f32),
    ToggleSendEnabled(super::mixer::RoutingStrip, usize),
    ToggleSendPreFader(super::mixer::RoutingStrip, usize),
    RemoveSend(super::mixer::RoutingStrip, usize),
    SelectTrack(usize),
    StartTrackRename(usize),
    StartBusRename(u16),
    TrackRenameInputChanged(String),
    OpenTrackColorPicker,
    SubmitTrackColor(Color),
    PreviewTrackColor(Color),
    CommitTrackRename,
    CancelTrackRename,
    ToggleTrackMute(usize),
    ToggleTrackSolo(usize),
    ToggleProcessorBrowser(super::processor_editor_windows::EditorTarget),
    CloseProcessorBrowser,
    ProcessorBrowserSearchChanged(String),
    ToggleProcessorBrowserSection(super::mixer::ProcessorBrowserSectionKey),
    ToggleMixerEffectRack(usize),
    SelectProcessor(
        super::processor_editor_windows::EditorTarget,
        super::mixer::ProcessorChoice,
    ),
    SetProcessorSlotHovered(
        Option<(
            super::processor_editor_windows::EditorTarget,
            mixer::ProcessorSlotSegment,
        )>,
    ),
    ToggleSlotBypass(super::processor_editor_windows::EditorTarget),
    StartTrackEffectDrag {
        strip_index: usize,
        effect_index: usize,
    },
    DropTrackEffect {
        strip_index: usize,
        effect_index: usize,
    },
    TrackEffectDragMoved {
        strip_index: usize,
        y: f32,
    },
    EffectRackCursorLeft(usize),
    MoveTrackEffect {
        strip_index: usize,
        from_effect_index: usize,
        to_effect_index: usize,
    },
    #[allow(dead_code)]
    ToggleTrackInstrumentBrowser(usize),
    #[allow(dead_code)]
    CloseTrackInstrumentBrowser,
    #[allow(dead_code)]
    InstrumentBrowserSearchChanged(String),
    #[allow(dead_code)]
    SelectTrackInstrument(usize, super::mixer::InstrumentChoice),
    #[allow(dead_code)]
    OpenEditor(super::processor_editor_windows::EditorTarget),
    ResetBusMeter(u16),
    SetBusGain(u16, f32),
    SetBusPan(u16, f32),
    ToggleBusMute(u16),
    ToggleBusSolo(u16),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PromptMessage {
    Acknowledge,
    Discard,
    Cancel,
}

#[derive(Debug, Clone)]
pub(super) enum ShortcutsMessage {
    OpenDialog,
    CloseDialog,
    SearchChanged(String),
    SelectNext,
    SelectPrevious,
    ActivateSelected,
    ActivateAction(crate::settings::ShortcutActionId),
}
