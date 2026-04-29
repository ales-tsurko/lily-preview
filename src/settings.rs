use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub(crate) enum WorkspacePane {
    #[default]
    Score,
    PianoRoll,
    Mixer,
    Editor,
    Logger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum DockAxis {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum FoldedPaneRestoreSettings {
    Tab {
        anchor: WorkspacePane,
    },
    Standalone,
    Split {
        anchor: WorkspacePane,
        axis: DockAxis,
        ratio: f32,
        insert_first: bool,
        #[serde(default)]
        sibling_panes: Vec<WorkspacePane>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FoldedPaneSettings {
    pub(crate) pane: WorkspacePane,
    pub(crate) restore: FoldedPaneRestoreSettings,
}

impl Default for FoldedPaneSettings {
    fn default() -> Self {
        Self {
            pane: WorkspacePane::PianoRoll,
            restore: FoldedPaneRestoreSettings::Standalone,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct DockGroupSettings {
    pub(crate) tabs: Vec<WorkspacePane>,
    pub(crate) active: WorkspacePane,
}

impl Default for DockGroupSettings {
    fn default() -> Self {
        Self {
            tabs: vec![WorkspacePane::Score],
            active: WorkspacePane::Score,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum DockNodeSettings {
    Group(DockGroupSettings),
    Split {
        axis: DockAxis,
        ratio: f32,
        first: Box<DockNodeSettings>,
        second: Box<DockNodeSettings>,
    },
}

impl Default for DockNodeSettings {
    fn default() -> Self {
        Self::Split {
            axis: DockAxis::Horizontal,
            ratio: 0.74,
            first: Box::new(Self::Split {
                axis: DockAxis::Vertical,
                ratio: 0.38,
                first: Box::new(Self::Group(DockGroupSettings {
                    tabs: vec![WorkspacePane::Editor],
                    active: WorkspacePane::Editor,
                })),
                second: Box::new(Self::Group(DockGroupSettings {
                    tabs: vec![WorkspacePane::Score, WorkspacePane::PianoRoll],
                    active: WorkspacePane::Score,
                })),
            }),
            second: Box::new(Self::Group(DockGroupSettings {
                tabs: vec![WorkspacePane::Logger],
                active: WorkspacePane::Logger,
            })),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct WorkspaceLayoutSettings {
    pub(crate) root: Option<DockNodeSettings>,
    pub(crate) folded_panes: Vec<FoldedPaneSettings>,
    pub(crate) piano_visible: bool,
}

impl Default for WorkspaceLayoutSettings {
    fn default() -> Self {
        Self {
            root: Some(DockNodeSettings::default()),
            folded_panes: Vec::new(),
            piano_visible: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct ScoreViewSettings {
    pub(crate) zoom: f32,
    pub(crate) page_brightness: u8,
}

impl Default for ScoreViewSettings {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            page_brightness: 70,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct PianoRollViewSettings {
    pub(crate) zoom_x: f32,
    pub(crate) beat_subdivision: u8,
}

impl Default for PianoRollViewSettings {
    fn default() -> Self {
        Self {
            zoom_x: 1.0,
            beat_subdivision: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct EditorThemeSettings {
    pub(crate) hue_offset_degrees: f32,
    pub(crate) saturation: f32,
    pub(crate) warmth: f32,
    pub(crate) brightness: f32,
    pub(crate) text_dim: f32,
    pub(crate) comment_dim: f32,
}

impl Default for EditorThemeSettings {
    fn default() -> Self {
        Self {
            hue_offset_degrees: 0.0,
            saturation: 1.0,
            warmth: 0.0,
            brightness: 1.0,
            text_dim: 1.0,
            comment_dim: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct EditorViewSettings {
    pub(crate) font_size: f32,
    pub(crate) center_cursor: bool,
}

impl Default for EditorViewSettings {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            center_cursor: false,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct PlaybackSettings {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) soundfonts: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sample_rate: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) block_size: Option<usize>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) chase_notes_on_seek: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PluginFormat {
    Clap,
    Vst3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PluginSearchPath {
    pub(crate) format: PluginFormat,
    pub(crate) path: PathBuf,
    pub(crate) enabled: bool,
}

impl Default for PluginSearchPath {
    fn default() -> Self {
        Self {
            format: PluginFormat::Clap,
            path: PathBuf::new(),
            enabled: true,
        }
    }
}

pub(crate) fn default_plugin_search_paths() -> Vec<PluginSearchPath> {
    default_plugin_search_path_specs()
        .into_iter()
        .map(|(format, path)| PluginSearchPath {
            format,
            path: expand_home(path),
            enabled: true,
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn default_plugin_search_path_specs() -> Vec<(PluginFormat, &'static str)> {
    vec![
        (PluginFormat::Clap, "/Library/Audio/Plug-Ins/CLAP"),
        (PluginFormat::Clap, "~/Library/Audio/Plug-Ins/CLAP"),
        (PluginFormat::Vst3, "/Library/Audio/Plug-Ins/VST3"),
        (PluginFormat::Vst3, "~/Library/Audio/Plug-Ins/VST3"),
    ]
}

#[cfg(target_os = "windows")]
fn default_plugin_search_path_specs() -> Vec<(PluginFormat, &'static str)> {
    let common = std::env::var("COMMONPROGRAMFILES")
        .unwrap_or_else(|_| "C:\\Program Files\\Common Files".to_string());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "~\\AppData\\Local".to_string());
    vec![
        (
            PluginFormat::Clap,
            Box::leak(format!("{common}\\CLAP").into_boxed_str()),
        ),
        (
            PluginFormat::Clap,
            Box::leak(format!("{local}\\Programs\\Common\\CLAP").into_boxed_str()),
        ),
        (
            PluginFormat::Vst3,
            Box::leak(format!("{common}\\VST3").into_boxed_str()),
        ),
        (
            PluginFormat::Vst3,
            Box::leak(format!("{local}\\Programs\\Common\\VST3").into_boxed_str()),
        ),
    ]
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn default_plugin_search_path_specs() -> Vec<(PluginFormat, &'static str)> {
    vec![
        (PluginFormat::Clap, "/usr/lib/clap"),
        (PluginFormat::Clap, "/usr/local/lib/clap"),
        (PluginFormat::Clap, "~/.clap"),
        (PluginFormat::Vst3, "/usr/lib/vst3"),
        (PluginFormat::Vst3, "/usr/local/lib/vst3"),
        (PluginFormat::Vst3, "~/.vst3"),
    ]
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn plugin_search_paths_is_default(paths: &[PluginSearchPath]) -> bool {
    paths == default_plugin_search_paths()
}

fn plugin_format_key(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    }
}

fn default_editor_recent_files_limit() -> usize {
    7
}

fn is_default_editor_recent_files_limit(value: &usize) -> bool {
    *value == default_editor_recent_files_limit()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ShortcutKeyCode {
    KeyA,
    KeyC,
    Comma,
    KeyF,
    KeyG,
    KeyH,
    KeyJ,
    KeyK,
    KeyL,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyS,
    KeyX,
    KeyV,
    KeyW,
    KeyY,
    KeyZ,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Slash,
    Backslash,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Backspace,
    Delete,
    Home,
    End,
    Insert,
    F3,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Equal,
    Minus,
    Digit0,
    NumpadAdd,
    NumpadSubtract,
    Numpad0,
    BracketLeft,
    BracketRight,
    NumpadEnter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ShortcutNamedKey {
    Space,
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShortcutKey {
    Code(ShortcutKeyCode),
    Named(ShortcutNamedKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ShortcutBinding {
    pub(crate) key: ShortcutKey,
    pub(crate) primary: bool,
    pub(crate) control: bool,
    pub(crate) alt: bool,
    pub(crate) shift: bool,
}

impl Default for ShortcutBinding {
    fn default() -> Self {
        Self {
            key: ShortcutKey::Code(ShortcutKeyCode::KeyS),
            primary: false,
            control: false,
            alt: false,
            shift: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShortcutBindingOverride {
    Assigned(ShortcutBinding),
    Unassigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ShortcutActionId {
    QuitApp,
    OpenActions,
    OpenSettingsFile,
    NewEditor,
    OpenEditorFile,
    ToggleFileBrowser,
    FileBrowserUndo,
    FileBrowserRedo,
    FileBrowserCut,
    FileBrowserCopy,
    FileBrowserPaste,
    FileBrowserRename,
    FileBrowserDelete,
    SaveEditor,
    CloseEditorTab,
    EditorUndo,
    EditorRedo,
    EditorCopy,
    EditorPaste,
    EditorOpenSearch,
    EditorOpenSearchReplace,
    EditorOpenGotoLine,
    EditorTriggerCompletion,
    EditorFindNext,
    EditorFindPrevious,
    EditorWordLeft,
    EditorWordRight,
    EditorWordLeftSelect,
    EditorWordRightSelect,
    EditorDeleteWordBackward,
    EditorDeleteWordForward,
    EditorDeleteToLineStart,
    EditorDeleteToLineEnd,
    EditorLineStart,
    EditorLineEnd,
    EditorLineStartSelect,
    EditorLineEndSelect,
    EditorDocumentStart,
    EditorDocumentEnd,
    EditorDocumentStartSelect,
    EditorDocumentEndSelect,
    EditorDeleteSelection,
    EditorSelectAll,
    EditorInsertLineBelow,
    EditorInsertLineAbove,
    EditorDeleteLine,
    EditorMoveLineUp,
    EditorMoveLineDown,
    EditorCopyLineUp,
    EditorCopyLineDown,
    EditorJoinLines,
    EditorIndent,
    EditorOutdent,
    EditorToggleLineComment,
    EditorToggleBlockComment,
    EditorSelectLine,
    EditorJumpToMatchingBracket,
    ToggleEditorPane,
    ToggleScorePane,
    TogglePianoRollPane,
    ToggleMixerPane,
    ToggleLoggerPane,
    PreviousTab,
    NextTab,
    PreviousEditorTab,
    NextEditorTab,
    PreviousPane,
    NextPane,
    ScoreZoomIn,
    ScoreZoomOut,
    ScoreZoomReset,
    EditorZoomIn,
    EditorZoomOut,
    EditorZoomReset,
    PianoRollZoomIn,
    PianoRollZoomOut,
    PianoRollZoomReset,
    TransportPlayPause,
    TransportRewind,
    ToggleMetronome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ShortcutOverride {
    pub(crate) action: ShortcutActionId,
    pub(crate) binding: ShortcutBindingOverride,
}

impl Default for ShortcutOverride {
    fn default() -> Self {
        Self {
            action: ShortcutActionId::SaveEditor,
            binding: ShortcutBindingOverride::Unassigned,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ShortcutSettings {
    pub(crate) overrides: Vec<ShortcutOverride>,
}

impl ShortcutSettings {
    pub(crate) fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

impl Serialize for ShortcutSettings {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.overrides.len()))?;
        for override_entry in &self.overrides {
            map.serialize_entry(
                &shortcut_action_id_key(override_entry.action),
                &format_shortcut_binding_override(&override_entry.binding),
            )?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ShortcutSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct LegacyShortcutSettings {
            overrides: Vec<ShortcutOverride>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ShortcutConfigValue {
            Text(String),
            Legacy(ShortcutBindingOverride),
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ShortcutSettingsRepr {
            Legacy(LegacyShortcutSettings),
            Flat(std::collections::BTreeMap<String, ShortcutConfigValue>),
        }

        match ShortcutSettingsRepr::deserialize(deserializer)? {
            ShortcutSettingsRepr::Legacy(legacy) => Ok(Self {
                overrides: legacy.overrides,
            }),
            ShortcutSettingsRepr::Flat(values) => {
                let mut overrides = Vec::with_capacity(values.len());

                for (key, value) in values {
                    let action = parse_shortcut_action_id_key(&key).ok_or_else(|| {
                        de::Error::custom(format!("Unknown shortcut action id: {key}"))
                    })?;
                    let binding = match value {
                        ShortcutConfigValue::Text(value) => {
                            parse_shortcut_binding_override(&value).map_err(de::Error::custom)?
                        }
                        ShortcutConfigValue::Legacy(value) => value,
                    };
                    overrides.push(ShortcutOverride { action, binding });
                }

                Ok(Self { overrides })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AppSettings {
    pub(crate) editor_view: EditorViewSettings,
    pub(crate) editor_theme: EditorThemeSettings,
    #[serde(
        default = "default_editor_recent_files_limit",
        skip_serializing_if = "is_default_editor_recent_files_limit"
    )]
    pub(crate) editor_recent_files_limit: usize,
    pub(crate) playback: PlaybackSettings,
    #[serde(default = "default_plugin_search_paths")]
    pub(crate) plugin_search_paths: Vec<PluginSearchPath>,
    #[serde(skip_serializing_if = "ShortcutSettings::is_empty")]
    pub(crate) shortcuts: ShortcutSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            editor_view: EditorViewSettings::default(),
            editor_theme: EditorThemeSettings::default(),
            editor_recent_files_limit: default_editor_recent_files_limit(),
            playback: PlaybackSettings::default(),
            plugin_search_paths: default_plugin_search_paths(),
            shortcuts: ShortcutSettings::default(),
        }
    }
}

pub(crate) fn load() -> Result<AppSettings, String> {
    let path = settings_load_path()?;

    load_from_path(&path)
}

pub(crate) fn load_from_path(path: &std::path::Path) -> Result<AppSettings, String> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|error| format!("Failed to parse settings {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(error) => Err(format!(
            "Failed to read settings {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn save(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path()?;
    let Some(parent) = path.parent() else {
        return Err(format!("Settings path has no parent: {}", path.display()));
    };

    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create settings directory {}: {error}",
            parent.display()
        )
    })?;

    let contents = render_settings_file(settings)
        .map_err(|error| format!("Failed to serialize settings: {error}"))?;

    fs::write(&path, contents)
        .map_err(|error| format!("Failed to write settings {}: {error}", path.display()))
}

fn render_settings_file(settings: &AppSettings) -> Result<String, toml::ser::Error> {
    let defaults = AppSettings::default();
    let mut out = String::new();

    out.push_str("# Lilypalooza settings\n");
    out.push_str("#\n");
    out.push_str("# Defaults are shown as commented lines.\n");
    out.push_str("# Uncomment and change a line to override it.\n\n");

    out.push_str("[editor_view]\n");
    push_documented_value(
        &mut out,
        "Editor font size in points.",
        "font_size",
        &format_f32(settings.editor_view.font_size),
        &format_f32(defaults.editor_view.font_size),
    );
    push_documented_value(
        &mut out,
        "Keep the cursor centered vertically while navigating.",
        "center_cursor",
        &settings.editor_view.center_cursor.to_string(),
        &defaults.editor_view.center_cursor.to_string(),
    );

    out.push_str("\n[editor_theme]\n");
    push_documented_value(
        &mut out,
        "Shift the editor theme hue in degrees.",
        "hue_offset_degrees",
        &format_f32(settings.editor_theme.hue_offset_degrees),
        &format_f32(defaults.editor_theme.hue_offset_degrees),
    );
    push_documented_value(
        &mut out,
        "Overall syntax saturation multiplier.",
        "saturation",
        &format_f32(settings.editor_theme.saturation),
        &format_f32(defaults.editor_theme.saturation),
    );
    push_documented_value(
        &mut out,
        "Warm or cool the editor colors.",
        "warmth",
        &format_f32(settings.editor_theme.warmth),
        &format_f32(defaults.editor_theme.warmth),
    );
    push_documented_value(
        &mut out,
        "Overall editor brightness multiplier.",
        "brightness",
        &format_f32(settings.editor_theme.brightness),
        &format_f32(defaults.editor_theme.brightness),
    );
    push_documented_value(
        &mut out,
        "Main text brightness multiplier.",
        "text_dim",
        &format_f32(settings.editor_theme.text_dim),
        &format_f32(defaults.editor_theme.text_dim),
    );
    push_documented_value(
        &mut out,
        "Comment text brightness multiplier.",
        "comment_dim",
        &format_f32(settings.editor_theme.comment_dim),
        &format_f32(defaults.editor_theme.comment_dim),
    );

    out.push('\n');
    push_documented_value(
        &mut out,
        "How many recent files to keep in menus.",
        "editor_recent_files_limit",
        &settings.editor_recent_files_limit.to_string(),
        &defaults.editor_recent_files_limit.to_string(),
    );

    out.push_str("\n[playback]\n");
    out.push_str("# Default startup SoundFont files.\n");
    out.push_str("# Example:\n");
    if settings.playback.soundfonts.is_empty() {
        out.push_str("# soundfonts = [\"/absolute/path/to/file.sf2\"]\n");
    } else {
        out.push_str("soundfonts = [\n");
        for soundfont in &settings.playback.soundfonts {
            out.push_str("    ");
            out.push_str(&format!("{:?}", soundfont.display().to_string()));
            out.push_str(",\n");
        }
        out.push_str("]\n");
    }
    out.push('\n');
    out.push_str(
        "# Preferred output device name. Use \"default\" to follow the system default device.\n",
    );
    if let Some(device) = &settings.playback.device {
        out.push_str("device = ");
        out.push_str(&format!("{device:?}\n\n"));
    } else {
        out.push_str("# device = \"default\"\n\n");
    }
    out.push_str("# Preferred output sample rate in Hz.\n");
    if let Some(sample_rate) = settings.playback.sample_rate {
        out.push_str(&format!("sample_rate = {sample_rate}\n\n"));
    } else {
        out.push_str("# sample_rate = 48000\n\n");
    }
    out.push_str("# Preferred backend block size in frames.\n");
    if let Some(block_size) = settings.playback.block_size {
        out.push_str(&format!("block_size = {block_size}\n\n"));
    } else {
        out.push_str("# block_size = 64\n\n");
    }
    push_documented_value(
        &mut out,
        "Chase already-held notes into the new position after seeking.",
        "chase_notes_on_seek",
        &settings.playback.chase_notes_on_seek.to_string(),
        &defaults.playback.chase_notes_on_seek.to_string(),
    );

    out.push_str("\n# Plugin scan roots. The scanner runs in the background and validates candidates in an isolated helper process.\n");
    if plugin_search_paths_is_default(&settings.plugin_search_paths) {
        for path in &defaults.plugin_search_paths {
            out.push_str("# [[plugin_search_paths]]\n");
            out.push_str("# format = ");
            out.push_str(&format!("{:?}\n", plugin_format_key(path.format)));
            out.push_str("# path = ");
            out.push_str(&format!("{:?}\n", path.path.display().to_string()));
            out.push_str("# enabled = true\n\n");
        }
    } else {
        for path in &settings.plugin_search_paths {
            out.push_str("[[plugin_search_paths]]\n");
            out.push_str("format = ");
            out.push_str(&format!("{:?}\n", plugin_format_key(path.format)));
            out.push_str("path = ");
            out.push_str(&format!("{:?}\n", path.path.display().to_string()));
            out.push_str("enabled = ");
            out.push_str(&path.enabled.to_string());
            out.push_str("\n\n");
        }
    }

    out.push_str("\n[shortcuts]\n");
    out.push_str("# Shortcut overrides. Use View > Actions to discover action ids.\n");
    out.push_str("# Use a single shortcut string or \"Unassigned\".\n");
    out.push_str("# Examples: \"Cmd+Shift+P\", \"Ctrl+,\", \"Alt+Up\", \"F3\", \"Space\"\n");
    if settings.shortcuts.overrides.is_empty() {
        out.push_str("# Example override:\n");
        out.push_str("# open-actions = \"Cmd+Shift+P\"\n");
        out.push_str("# open-settings-file = \"Cmd+,\"\n");
    } else {
        let mut overrides = settings.shortcuts.overrides.clone();
        overrides.sort_by_key(|entry| shortcut_action_id_key(entry.action));
        for override_entry in overrides {
            out.push_str(&shortcut_action_id_key(override_entry.action));
            out.push_str(" = ");
            out.push_str(&toml::to_string(&format_shortcut_binding_override(
                &override_entry.binding,
            ))?);
        }
    }

    Ok(out)
}

fn push_documented_value(out: &mut String, comment: &str, key: &str, value: &str, default: &str) {
    out.push_str("# ");
    out.push_str(comment);
    out.push('\n');
    if value == default {
        out.push_str("# ");
    }
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(value);
    out.push_str("\n\n");
}

fn format_f32(value: f32) -> String {
    let mut s = format!("{value:.3}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.push('0');
    }
    s
}

pub(crate) fn path() -> Result<PathBuf, String> {
    settings_path()
}

fn settings_path() -> Result<PathBuf, String> {
    let project_dirs = ProjectDirs::from("", "", "lilypalooza")
        .ok_or_else(|| "Failed to resolve user config directory".to_string())?;

    Ok(project_dirs.config_dir().join("settings.toml"))
}

fn legacy_settings_path() -> Result<PathBuf, String> {
    let project_dirs = ProjectDirs::from("by", "alestsurko", "lilypalooza")
        .ok_or_else(|| "Failed to resolve user config directory".to_string())?;

    Ok(project_dirs.config_dir().join("settings.toml"))
}

fn settings_load_path() -> Result<PathBuf, String> {
    let path = settings_path()?;
    if path.is_file() {
        return Ok(path);
    }

    let legacy = legacy_settings_path()?;
    if legacy.is_file() {
        return Ok(legacy);
    }

    Ok(path)
}

pub(crate) fn shortcut_action_id_key(action_id: ShortcutActionId) -> String {
    let debug = format!("{action_id:?}");
    let mut out = String::with_capacity(debug.len() + 8);

    for (index, ch) in debug.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }

    out
}

fn parse_shortcut_action_id_key(key: &str) -> Option<ShortcutActionId> {
    #[derive(Deserialize)]
    struct ActionIdWrapper {
        action: ShortcutActionId,
    }

    let source = format!("action = {key:?}");
    toml::from_str::<ActionIdWrapper>(&source)
        .ok()
        .map(|wrapper| wrapper.action)
}

fn format_shortcut_binding_override(binding: &ShortcutBindingOverride) -> String {
    match binding {
        ShortcutBindingOverride::Assigned(binding) => format_shortcut_binding(binding),
        ShortcutBindingOverride::Unassigned => "Unassigned".to_string(),
    }
}

fn format_shortcut_binding(binding: &ShortcutBinding) -> String {
    let mut parts = Vec::new();

    if binding.primary {
        parts.push("Cmd");
    }
    if binding.control {
        parts.push("Ctrl");
    }
    if binding.alt {
        parts.push("Alt");
    }
    if binding.shift {
        parts.push("Shift");
    }

    parts.push(match binding.key {
        ShortcutKey::Code(code) => shortcut_key_code_string(code),
        ShortcutKey::Named(named) => shortcut_named_key_string(named),
    });

    parts.join("+")
}

fn parse_shortcut_binding_override(value: &str) -> Result<ShortcutBindingOverride, String> {
    if value.trim().eq_ignore_ascii_case("unassigned") {
        return Ok(ShortcutBindingOverride::Unassigned);
    }

    parse_shortcut_binding(value).map(ShortcutBindingOverride::Assigned)
}

fn parse_shortcut_binding(value: &str) -> Result<ShortcutBinding, String> {
    let mut binding = ShortcutBinding {
        key: ShortcutKey::Code(ShortcutKeyCode::KeyS),
        primary: false,
        control: false,
        alt: false,
        shift: false,
    };

    let mut tokens: Vec<_> = value
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();

    let key_token = tokens
        .pop()
        .ok_or_else(|| "Shortcut must include a key".to_string())?;

    for token in tokens {
        if token.eq_ignore_ascii_case("cmd")
            || token.eq_ignore_ascii_case("primary")
            || (!cfg!(target_os = "macos") && token.eq_ignore_ascii_case("ctrl"))
        {
            binding.primary = true;
        } else if token.eq_ignore_ascii_case("ctrl") || token.eq_ignore_ascii_case("control") {
            binding.control = true;
        } else if token.eq_ignore_ascii_case("alt") || token.eq_ignore_ascii_case("option") {
            binding.alt = true;
        } else if token.eq_ignore_ascii_case("shift") {
            binding.shift = true;
        } else {
            return Err(format!("Unknown shortcut modifier: {token}"));
        }
    }

    binding.key = parse_shortcut_key(key_token)?;
    Ok(binding)
}

fn parse_shortcut_key(value: &str) -> Result<ShortcutKey, String> {
    let key = if value.len() == 1 {
        match value.chars().next().unwrap_or_default() {
            'A' | 'a' => ShortcutKey::Code(ShortcutKeyCode::KeyA),
            'C' | 'c' => ShortcutKey::Code(ShortcutKeyCode::KeyC),
            ',' => ShortcutKey::Code(ShortcutKeyCode::Comma),
            'F' | 'f' => ShortcutKey::Code(ShortcutKeyCode::KeyF),
            'G' | 'g' => ShortcutKey::Code(ShortcutKeyCode::KeyG),
            'H' | 'h' => ShortcutKey::Code(ShortcutKeyCode::KeyH),
            'J' | 'j' => ShortcutKey::Code(ShortcutKeyCode::KeyJ),
            'K' | 'k' => ShortcutKey::Code(ShortcutKeyCode::KeyK),
            'L' | 'l' => ShortcutKey::Code(ShortcutKeyCode::KeyL),
            'N' | 'n' => ShortcutKey::Code(ShortcutKeyCode::KeyN),
            'O' | 'o' => ShortcutKey::Code(ShortcutKeyCode::KeyO),
            'P' | 'p' => ShortcutKey::Code(ShortcutKeyCode::KeyP),
            'Q' | 'q' => ShortcutKey::Code(ShortcutKeyCode::KeyQ),
            'S' | 's' => ShortcutKey::Code(ShortcutKeyCode::KeyS),
            'X' | 'x' => ShortcutKey::Code(ShortcutKeyCode::KeyX),
            'V' | 'v' => ShortcutKey::Code(ShortcutKeyCode::KeyV),
            'W' | 'w' => ShortcutKey::Code(ShortcutKeyCode::KeyW),
            'Y' | 'y' => ShortcutKey::Code(ShortcutKeyCode::KeyY),
            'Z' | 'z' => ShortcutKey::Code(ShortcutKeyCode::KeyZ),
            '0' => ShortcutKey::Code(ShortcutKeyCode::Digit0),
            '1' => ShortcutKey::Code(ShortcutKeyCode::Digit1),
            '2' => ShortcutKey::Code(ShortcutKeyCode::Digit2),
            '3' => ShortcutKey::Code(ShortcutKeyCode::Digit3),
            '4' => ShortcutKey::Code(ShortcutKeyCode::Digit4),
            '/' => ShortcutKey::Code(ShortcutKeyCode::Slash),
            '\\' => ShortcutKey::Code(ShortcutKeyCode::Backslash),
            '[' => ShortcutKey::Code(ShortcutKeyCode::BracketLeft),
            ']' => ShortcutKey::Code(ShortcutKeyCode::BracketRight),
            '-' => ShortcutKey::Code(ShortcutKeyCode::Minus),
            _ => return Err(format!("Unknown shortcut key: {value}")),
        }
    } else if value.eq_ignore_ascii_case("left") {
        ShortcutKey::Code(ShortcutKeyCode::ArrowLeft)
    } else if value.eq_ignore_ascii_case("right") {
        ShortcutKey::Code(ShortcutKeyCode::ArrowRight)
    } else if value.eq_ignore_ascii_case("up") {
        ShortcutKey::Code(ShortcutKeyCode::ArrowUp)
    } else if value.eq_ignore_ascii_case("down") {
        ShortcutKey::Code(ShortcutKeyCode::ArrowDown)
    } else if value.eq_ignore_ascii_case("backspace") {
        ShortcutKey::Code(ShortcutKeyCode::Backspace)
    } else if value.eq_ignore_ascii_case("delete") {
        ShortcutKey::Code(ShortcutKeyCode::Delete)
    } else if value.eq_ignore_ascii_case("home") {
        ShortcutKey::Code(ShortcutKeyCode::Home)
    } else if value.eq_ignore_ascii_case("end") {
        ShortcutKey::Code(ShortcutKeyCode::End)
    } else if value.eq_ignore_ascii_case("insert") {
        ShortcutKey::Code(ShortcutKeyCode::Insert)
    } else if value.eq_ignore_ascii_case("f3") {
        ShortcutKey::Code(ShortcutKeyCode::F3)
    } else if value.eq_ignore_ascii_case("plus") {
        ShortcutKey::Code(ShortcutKeyCode::Equal)
    } else if value.eq_ignore_ascii_case("space") {
        ShortcutKey::Named(ShortcutNamedKey::Space)
    } else if value.eq_ignore_ascii_case("enter") {
        ShortcutKey::Named(ShortcutNamedKey::Enter)
    } else {
        return Err(format!("Unknown shortcut key: {value}"));
    };

    Ok(key)
}

fn shortcut_key_code_string(code: ShortcutKeyCode) -> &'static str {
    match code {
        ShortcutKeyCode::KeyA => "A",
        ShortcutKeyCode::KeyC => "C",
        ShortcutKeyCode::Comma => ",",
        ShortcutKeyCode::KeyF => "F",
        ShortcutKeyCode::KeyG => "G",
        ShortcutKeyCode::KeyH => "H",
        ShortcutKeyCode::KeyJ => "J",
        ShortcutKeyCode::KeyK => "K",
        ShortcutKeyCode::KeyL => "L",
        ShortcutKeyCode::KeyN => "N",
        ShortcutKeyCode::KeyO => "O",
        ShortcutKeyCode::KeyP => "P",
        ShortcutKeyCode::KeyQ => "Q",
        ShortcutKeyCode::KeyS => "S",
        ShortcutKeyCode::KeyX => "X",
        ShortcutKeyCode::KeyV => "V",
        ShortcutKeyCode::KeyW => "W",
        ShortcutKeyCode::KeyY => "Y",
        ShortcutKeyCode::KeyZ => "Z",
        ShortcutKeyCode::Digit0 | ShortcutKeyCode::Numpad0 => "0",
        ShortcutKeyCode::Digit1 | ShortcutKeyCode::Numpad1 => "1",
        ShortcutKeyCode::Digit2 | ShortcutKeyCode::Numpad2 => "2",
        ShortcutKeyCode::Digit3 | ShortcutKeyCode::Numpad3 => "3",
        ShortcutKeyCode::Digit4 | ShortcutKeyCode::Numpad4 => "4",
        ShortcutKeyCode::Slash => "/",
        ShortcutKeyCode::Backslash => "\\",
        ShortcutKeyCode::ArrowLeft => "Left",
        ShortcutKeyCode::ArrowRight => "Right",
        ShortcutKeyCode::ArrowUp => "Up",
        ShortcutKeyCode::ArrowDown => "Down",
        ShortcutKeyCode::Backspace => "Backspace",
        ShortcutKeyCode::Delete => "Delete",
        ShortcutKeyCode::Home => "Home",
        ShortcutKeyCode::End => "End",
        ShortcutKeyCode::Insert => "Insert",
        ShortcutKeyCode::F3 => "F3",
        ShortcutKeyCode::Equal | ShortcutKeyCode::NumpadAdd => "Plus",
        ShortcutKeyCode::Minus | ShortcutKeyCode::NumpadSubtract => "-",
        ShortcutKeyCode::BracketLeft => "[",
        ShortcutKeyCode::BracketRight => "]",
        ShortcutKeyCode::NumpadEnter => "Enter",
    }
}

fn shortcut_named_key_string(named: ShortcutNamedKey) -> &'static str {
    match named {
        ShortcutNamedKey::Space => "Space",
        ShortcutNamedKey::Enter => "Enter",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, PlaybackSettings, PluginFormat, PluginSearchPath, default_plugin_search_paths,
        render_settings_file,
    };
    use std::path::PathBuf;

    #[test]
    fn settings_template_contains_playback_section() {
        let contents =
            render_settings_file(&AppSettings::default()).expect("default settings should render");

        assert!(contents.contains("[playback]"));
        assert!(contents.contains("# soundfonts = [\"/absolute/path/to/file.sf2\"]"));
        assert!(contents.contains("# device = \"default\""));
        assert!(contents.contains("# sample_rate = 48000"));
        assert!(contents.contains("# block_size = 64"));
        assert!(contents.contains("# chase_notes_on_seek = false"));
    }

    #[test]
    fn settings_roundtrip_parses_playback_settings() {
        let settings = AppSettings {
            playback: PlaybackSettings {
                soundfonts: vec![
                    PathBuf::from("/tmp/test.sf2"),
                    PathBuf::from("/tmp/other.sf2"),
                ],
                device: Some("Built-in Output".into()),
                sample_rate: Some(48_000),
                block_size: Some(128),
                chase_notes_on_seek: true,
            },
            ..AppSettings::default()
        };

        let contents = render_settings_file(&settings)
            .expect("settings with playback soundfont should render");
        let parsed: AppSettings =
            toml::from_str(&contents).expect("rendered settings should parse back");

        assert_eq!(
            parsed.playback.soundfonts,
            vec![
                PathBuf::from("/tmp/test.sf2"),
                PathBuf::from("/tmp/other.sf2")
            ]
        );
        assert_eq!(parsed.playback.device.as_deref(), Some("Built-in Output"));
        assert_eq!(parsed.playback.sample_rate, Some(48_000));
        assert_eq!(parsed.playback.block_size, Some(128));
        assert!(parsed.playback.chase_notes_on_seek);
    }

    #[test]
    fn default_plugin_search_paths_include_all_platform_formats() {
        let paths = default_plugin_search_paths();

        assert!(paths.iter().any(|path| path.format == PluginFormat::Clap));
        assert!(paths.iter().any(|path| path.format == PluginFormat::Vst3));
        assert!(paths.iter().all(|path| path.enabled));
        assert!(paths.iter().any(|path| {
            path.path
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("clap")
        }));
        assert!(paths.iter().any(|path| {
            path.path
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("vst3")
        }));
    }

    #[test]
    fn settings_roundtrip_parses_plugin_search_paths() {
        let settings = AppSettings {
            plugin_search_paths: vec![
                PluginSearchPath {
                    format: PluginFormat::Clap,
                    path: PathBuf::from("/plugins/clap"),
                    enabled: true,
                },
                PluginSearchPath {
                    format: PluginFormat::Vst3,
                    path: PathBuf::from("/plugins/vst3"),
                    enabled: false,
                },
            ],
            ..AppSettings::default()
        };

        let contents =
            render_settings_file(&settings).expect("settings with plugin paths should render");
        let parsed: AppSettings =
            toml::from_str(&contents).expect("rendered settings should parse back");

        assert_eq!(parsed.plugin_search_paths, settings.plugin_search_paths);
    }
}
