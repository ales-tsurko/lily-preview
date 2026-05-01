//! CLAP adapter and probe helpers for Lilypalooza.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{
    Arc, Mutex, OnceLock, RwLock,
    atomic::{AtomicBool, Ordering},
};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_IS_LIVE, CLAP_EVENT_MIDI, clap_event_header,
    clap_event_midi, clap_input_events, clap_output_events,
};
use clap_sys::ext::gui::{
    CLAP_EXT_GUI, CLAP_WINDOW_API_COCOA, CLAP_WINDOW_API_WAYLAND, CLAP_WINDOW_API_WIN32,
    CLAP_WINDOW_API_X11, clap_host_gui, clap_plugin_gui, clap_window, clap_window_handle,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_host_params, clap_plugin_params};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::plugin_features::{
    CLAP_PLUGIN_FEATURE_AUDIO_EFFECT, CLAP_PLUGIN_FEATURE_DRUM, CLAP_PLUGIN_FEATURE_DRUM_MACHINE,
    CLAP_PLUGIN_FEATURE_INSTRUMENT, CLAP_PLUGIN_FEATURE_SAMPLER, CLAP_PLUGIN_FEATURE_SYNTHESIZER,
};
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use lilypalooza_audio::instrument::{
    Controller, ControllerError, EditorDescriptor, EditorError, EditorParent, EditorSession,
    EditorSize, EffectProcessor, EffectRuntimeContext, EffectRuntimeSpec, InstrumentProcessor,
    InstrumentRuntimeContext, InstrumentRuntimeSpec, MidiEvent, Processor, ProcessorState,
    ProcessorStateError, RuntimeBinding, RuntimeFactoryError, registry,
};
use lilypalooza_audio::{ProcessorDescriptor, SlotState};
use raw_window_handle::RawWindowHandle;
use serde::{Deserialize, Serialize};

/// Stable adapter backend format.
pub const FORMAT: &str = "clap";

/// One CLAP plugin discovered inside a CLAP binary or bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClapPluginMetadata {
    /// Stable host id used by persisted processor slots.
    pub processor_id: String,
    /// CLAP plugin id from the descriptor.
    pub clap_id: String,
    /// Display name.
    pub name: String,
    /// Optional vendor.
    pub vendor: Option<String>,
    /// Optional version.
    pub version: Option<String>,
    /// Descriptor feature strings.
    pub features: Vec<String>,
    /// Lilypalooza registry role.
    pub role: registry::Role,
    /// Original candidate path.
    pub path: PathBuf,
    /// Resolved dynamic library path.
    pub library_path: PathBuf,
}

/// Result returned by the validator process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Validated format.
    pub format: String,
    /// Candidate path.
    pub path: PathBuf,
    /// Probe outcome.
    pub result: Result<Vec<ClapPluginMetadata>, String>,
}

/// CLAP probe errors.
#[derive(Debug, thiserror::Error)]
pub enum ClapProbeError {
    /// Candidate path does not look like a CLAP plugin.
    #[error("not a CLAP candidate: {0}")]
    NotCandidate(String),
    /// Path could not be converted for CLAP entry initialization.
    #[error("plugin path contains an interior NUL byte: {0}")]
    InvalidPath(String),
    /// Dynamic library loading failed.
    #[error("failed to load CLAP library {path}: {error}")]
    Load {
        /// Dynamic library path.
        path: PathBuf,
        /// Loader error.
        error: String,
    },
    /// Required CLAP entry symbol is missing.
    #[error("CLAP entry symbol is missing in {0}")]
    MissingEntry(PathBuf),
    /// CLAP version is unsupported.
    #[error("CLAP version is not compatible")]
    IncompatibleVersion,
    /// CLAP entry initialization failed.
    #[error("CLAP entry initialization failed")]
    InitFailed,
    /// Required CLAP function pointer is missing.
    #[error("CLAP entry is missing required function: {0}")]
    MissingFunction(&'static str),
    /// Required CLAP plugin factory is missing.
    #[error("CLAP plugin factory is missing")]
    MissingFactory,
    /// CLAP plugin factory did not expose any plugin descriptors.
    #[error("CLAP plugin factory exposes no plugin descriptors")]
    NoPluginDescriptors,
    /// Plugin descriptor is invalid.
    #[error("CLAP plugin descriptor {index} is invalid")]
    InvalidDescriptor {
        /// Descriptor index reported by the plugin factory.
        index: u32,
    },
}

/// Returns true when a path is a CLAP candidate.
#[must_use]
pub fn is_clap_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("clap"))
}

/// Finds CLAP candidates under one root.
pub fn candidate_paths(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut candidates = Vec::new();
    if !root.is_dir() {
        return Ok(candidates);
    }

    collect_candidate_paths(root, &mut candidates)?;
    candidates.sort();
    Ok(candidates)
}

fn collect_candidate_paths(
    root: &Path,
    candidates: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if is_clap_candidate(&path) {
            candidates.push(path);
        } else if path.is_dir() {
            collect_candidate_paths(&path, candidates)?;
        }
    }
    Ok(())
}

/// Probes one CLAP candidate in-process. Call this from the validator subprocess, not the app.
pub fn probe(path: &Path) -> Result<Vec<ClapPluginMetadata>, ClapProbeError> {
    if !is_clap_candidate(path) {
        return Err(ClapProbeError::NotCandidate(path.display().to_string()));
    }
    let library_path = resolve_clap_library_path(path);
    let path_c = CString::new(path.display().to_string())
        .map_err(|_| ClapProbeError::InvalidPath(path.display().to_string()))?;

    // SAFETY: Loading a third-party dynamic library is inherently unsafe and is isolated by
    // `lilypalooza-plugin-validator`. We only keep function pointers while `library` is alive.
    let library = unsafe {
        libloading::Library::new(&library_path).map_err(|error| ClapProbeError::Load {
            path: library_path.clone(),
            error: error.to_string(),
        })?
    };

    // SAFETY: The symbol name is the CLAP ABI entry point. The returned pointer is checked before
    // dereference and the library stays loaded for the whole probe.
    let entry = unsafe {
        let symbol = library
            .get::<*const clap_sys::entry::clap_plugin_entry>(b"clap_entry\0")
            .map_err(|_| ClapProbeError::MissingEntry(library_path.clone()))?;
        let entry = *symbol;
        entry
            .as_ref()
            .ok_or(ClapProbeError::MissingEntry(library_path.clone()))?
    };

    if !clap_version_is_compatible(entry.clap_version) {
        return Err(ClapProbeError::IncompatibleVersion);
    }

    let init = entry.init.ok_or(ClapProbeError::MissingFunction("init"))?;
    let deinit = entry
        .deinit
        .ok_or(ClapProbeError::MissingFunction("deinit"))?;
    let get_factory = entry
        .get_factory
        .ok_or(ClapProbeError::MissingFunction("get_factory"))?;

    // SAFETY: Function pointer comes from the validated CLAP entry and receives a NUL-terminated
    // path string valid for the duration of the call.
    if unsafe { !init(path_c.as_ptr()) } {
        return Err(ClapProbeError::InitFailed);
    }

    let result = probe_initialized_factory(path, &library_path, get_factory);

    // SAFETY: `deinit` is paired with successful `init` for this CLAP entry.
    unsafe { deinit() };

    result
}

fn probe_initialized_factory(
    path: &Path,
    library_path: &Path,
    get_factory: unsafe extern "C" fn(*const std::ffi::c_char) -> *const c_void,
) -> Result<Vec<ClapPluginMetadata>, ClapProbeError> {
    // SAFETY: Function pointer comes from CLAP entry; factory id is a static C string.
    let factory = unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) };
    // SAFETY: Factory pointer comes from CLAP `get_factory` and is checked for null.
    let factory = unsafe { (factory as *const clap_plugin_factory).as_ref() }
        .ok_or(ClapProbeError::MissingFactory)?;
    let count = factory
        .get_plugin_count
        .ok_or(ClapProbeError::MissingFunction("get_plugin_count"))?;
    let descriptor = factory
        .get_plugin_descriptor
        .ok_or(ClapProbeError::MissingFunction("get_plugin_descriptor"))?;

    // SAFETY: CLAP factory function pointer is valid while the CLAP entry is initialized.
    let count = unsafe { count(factory) };
    if count == 0 {
        return Err(ClapProbeError::NoPluginDescriptors);
    }
    let mut plugins = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: Index is below the factory-reported count.
        let desc = unsafe { descriptor(factory, index) };
        let desc = unsafe_descriptor(desc).ok_or(ClapProbeError::InvalidDescriptor { index })?;
        plugins.push(metadata_from_descriptor(path, library_path, desc, index)?);
    }
    Ok(plugins)
}

fn unsafe_descriptor(
    descriptor: *const clap_plugin_descriptor,
) -> Option<&'static clap_plugin_descriptor> {
    // SAFETY: Caller passes a descriptor pointer returned by CLAP. Null is handled.
    unsafe { descriptor.as_ref() }
}

fn metadata_from_descriptor(
    path: &Path,
    library_path: &Path,
    descriptor: &clap_plugin_descriptor,
    index: u32,
) -> Result<ClapPluginMetadata, ClapProbeError> {
    if !clap_version_is_compatible(descriptor.clap_version) {
        return Err(ClapProbeError::InvalidDescriptor { index });
    }
    let clap_id = cstr_field(descriptor.id).ok_or(ClapProbeError::InvalidDescriptor { index })?;
    let name = cstr_field(descriptor.name).ok_or(ClapProbeError::InvalidDescriptor { index })?;
    let features = features_from_descriptor(descriptor);
    let role = role_from_features(&features);

    Ok(ClapPluginMetadata {
        processor_id: stable_processor_id(path, &clap_id),
        clap_id,
        name,
        vendor: cstr_field(descriptor.vendor),
        version: cstr_field(descriptor.version),
        features,
        role,
        path: path.to_path_buf(),
        library_path: library_path.to_path_buf(),
    })
}

fn features_from_descriptor(descriptor: &clap_plugin_descriptor) -> Vec<String> {
    let mut features = Vec::new();
    let mut cursor = descriptor.features;
    if cursor.is_null() {
        return features;
    }

    loop {
        // SAFETY: CLAP feature arrays are null-terminated. We stop at the first null pointer.
        let ptr = unsafe { *cursor };
        if ptr.is_null() {
            break;
        }
        if let Some(feature) = cstr_field(ptr) {
            features.push(feature);
        }
        // SAFETY: Advancing within a CLAP null-terminated feature pointer array.
        cursor = unsafe { cursor.add(1) };
    }
    features
}

fn role_from_features(features: &[String]) -> registry::Role {
    if has_feature(features, CLAP_PLUGIN_FEATURE_INSTRUMENT)
        || has_feature(features, CLAP_PLUGIN_FEATURE_SYNTHESIZER)
        || has_feature(features, CLAP_PLUGIN_FEATURE_SAMPLER)
        || has_feature(features, CLAP_PLUGIN_FEATURE_DRUM)
        || has_feature(features, CLAP_PLUGIN_FEATURE_DRUM_MACHINE)
    {
        registry::Role::Instrument
    } else {
        let _ = CLAP_PLUGIN_FEATURE_AUDIO_EFFECT;
        registry::Role::Effect
    }
}

fn has_feature(features: &[String], feature: &CStr) -> bool {
    let feature = feature.to_string_lossy();
    features
        .iter()
        .any(|candidate| candidate == feature.as_ref())
}

fn cstr_field(value: *const std::ffi::c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: CLAP descriptor string fields are expected to be valid NUL-terminated strings.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .ok()
        .map(str::to_string)
}

/// Builds the stable global processor id for one CLAP plugin.
#[must_use]
pub fn stable_processor_id(path: &Path, clap_id: &str) -> String {
    format!("{FORMAT}:{}#{clap_id}", path.display())
}

/// Resolves the actual dynamic library path for a CLAP candidate.
#[must_use]
pub fn resolve_clap_library_path(path: &Path) -> PathBuf {
    let macos_bundle = path.join("Contents").join("MacOS").join(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default(),
    );
    if macos_bundle.is_file() {
        macos_bundle
    } else {
        path.to_path_buf()
    }
}

/// Parsed components of a stable CLAP processor id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeKey {
    /// Candidate bundle/library path.
    pub path: PathBuf,
    /// CLAP descriptor id.
    pub clap_id: String,
}

impl RuntimeKey {
    /// Parses ids produced by [`stable_processor_id`].
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let rest = id.strip_prefix("clap:")?;
        let (path, clap_id) = rest.rsplit_once('#')?;
        (!path.is_empty() && !clap_id.is_empty()).then(|| Self {
            path: PathBuf::from(path),
            clap_id: clap_id.to_string(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum ClapRuntimeError {
    #[error("unknown CLAP plugin `{0}`")]
    UnknownPlugin(String),
    #[error("CLAP plugin id is invalid: {0}")]
    InvalidPluginId(String),
    #[error("failed to load CLAP library {path}: {error}")]
    Load { path: PathBuf, error: String },
    #[error("CLAP entry symbol is missing in {0}")]
    MissingEntry(PathBuf),
    #[error("CLAP version is not compatible")]
    IncompatibleVersion,
    #[error("CLAP entry initialization failed")]
    InitFailed,
    #[error("CLAP entry is missing required function: {0}")]
    MissingFunction(&'static str),
    #[error("CLAP plugin factory is missing")]
    MissingFactory,
    #[error("CLAP plugin creation failed")]
    CreatePluginFailed,
    #[error("CLAP plugin init failed")]
    PluginInitFailed,
    #[error("CLAP plugin activation failed")]
    ActivateFailed,
    #[error("CLAP plugin processing start failed")]
    StartProcessingFailed,
    #[error("CLAP plugin does not provide a process callback")]
    MissingProcess,
}

const DEFAULT_CLAP_EDITOR_DESCRIPTOR: EditorDescriptor = EditorDescriptor {
    default_size: EditorSize {
        width: 720,
        height: 480,
    },
    min_size: Some(EditorSize {
        width: 320,
        height: 220,
    }),
    resizable: false,
};

static PLUGIN_METADATA: OnceLock<RwLock<HashMap<String, ClapPluginMetadata>>> = OnceLock::new();
static LOADED_MODULES: OnceLock<Mutex<HashMap<PathBuf, Arc<LoadedModule>>>> = OnceLock::new();

fn metadata_store() -> &'static RwLock<HashMap<String, ClapPluginMetadata>> {
    PLUGIN_METADATA.get_or_init(|| RwLock::new(HashMap::new()))
}

fn loaded_modules() -> &'static Mutex<HashMap<PathBuf, Arc<LoadedModule>>> {
    LOADED_MODULES.get_or_init(|| Mutex::new(HashMap::new()))
}

struct LoadedModule {
    _library: libloading::Library,
    entry: NonNull<clap_sys::entry::clap_plugin_entry>,
}

// SAFETY: The dynamic library is pinned in `LOADED_MODULES`; `entry` points into that live library
// and is only used through CLAP function pointers that are themselves thread-safe by host contract.
unsafe impl Send for LoadedModule {}
// SAFETY: See the `Send` impl; shared access never mutates Rust-owned state inside `LoadedModule`.
unsafe impl Sync for LoadedModule {}

fn load_module(
    plugin_path: &Path,
    library_path: &Path,
) -> Result<Arc<LoadedModule>, ClapRuntimeError> {
    let mut modules = loaded_modules()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(module) = modules.get(library_path) {
        return Ok(module.clone());
    }

    // SAFETY: Loading third-party plugin code is inherently unsafe. The loaded library is kept
    // alive for the whole process lifetime through `LOADED_MODULES`.
    let library = unsafe {
        libloading::Library::new(library_path).map_err(|error| ClapRuntimeError::Load {
            path: library_path.to_path_buf(),
            error: error.to_string(),
        })?
    };
    // SAFETY: The symbol name is the CLAP ABI entry point and the pointer is checked for null.
    let entry = unsafe {
        let symbol = library
            .get::<*const clap_sys::entry::clap_plugin_entry>(b"clap_entry\0")
            .map_err(|_| ClapRuntimeError::MissingEntry(library_path.to_path_buf()))?;
        NonNull::new(*symbol as *mut clap_sys::entry::clap_plugin_entry)
            .ok_or_else(|| ClapRuntimeError::MissingEntry(library_path.to_path_buf()))?
    };
    // SAFETY: `entry` points into the loaded CLAP library and stays valid while `library` lives.
    let entry_ref = unsafe { entry.as_ref() };
    if !clap_version_is_compatible(entry_ref.clap_version) {
        return Err(ClapRuntimeError::IncompatibleVersion);
    }
    let init = entry_ref
        .init
        .ok_or(ClapRuntimeError::MissingFunction("init"))?;
    let path_c = CString::new(plugin_path.display().to_string())
        .map_err(|_| ClapRuntimeError::InvalidPluginId(plugin_path.display().to_string()))?;
    // SAFETY: CLAP entry function pointer and NUL-terminated path come from validated data.
    if unsafe { !init(path_c.as_ptr()) } {
        return Err(ClapRuntimeError::InitFailed);
    }

    let module = Arc::new(LoadedModule {
        _library: library,
        entry,
    });
    modules.insert(library_path.to_path_buf(), module.clone());
    Ok(module)
}

fn plugin_metadata(plugin_id: &str) -> Result<ClapPluginMetadata, ClapRuntimeError> {
    metadata_store()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(plugin_id)
        .cloned()
        .or_else(|| {
            let key = RuntimeKey::parse(plugin_id)?;
            Some(ClapPluginMetadata {
                processor_id: plugin_id.to_string(),
                clap_id: key.clap_id,
                name: plugin_id.to_string(),
                vendor: None,
                version: None,
                features: Vec::new(),
                role: registry::Role::Effect,
                path: key.path.clone(),
                library_path: resolve_clap_library_path(&key.path),
            })
        })
        .ok_or_else(|| ClapRuntimeError::UnknownPlugin(plugin_id.to_string()))
}

struct HostContext {
    name: CString,
    vendor: CString,
    url: CString,
    version: CString,
    host: clap_host,
    requested_gui_size: Mutex<Option<EditorSize>>,
    params_flush_requested: AtomicBool,
}

impl HostContext {
    fn new() -> Box<Self> {
        let mut context = Box::new(Self {
            name: CString::new("Lilypalooza").expect("static string has no NUL"),
            vendor: CString::new("Lilypalooza").expect("static string has no NUL"),
            url: CString::new("https://github.com/ales-tsurko/lilypalooza")
                .expect("static string has no NUL"),
            version: CString::new(env!("CARGO_PKG_VERSION")).expect("static string has no NUL"),
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: std::ptr::null_mut(),
                name: std::ptr::null(),
                vendor: std::ptr::null(),
                url: std::ptr::null(),
                version: std::ptr::null(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
            requested_gui_size: Mutex::new(None),
            params_flush_requested: AtomicBool::new(false),
        });
        let context_ptr = (&mut *context) as *mut Self;
        context.host.host_data = context_ptr.cast();
        context.host.name = context.name.as_ptr();
        context.host.vendor = context.vendor.as_ptr();
        context.host.url = context.url.as_ptr();
        context.host.version = context.version.as_ptr();
        context
    }

    fn as_ptr(&self) -> *const clap_host {
        &self.host
    }

    fn take_requested_gui_size(&self) -> Option<EditorSize> {
        self.requested_gui_size
            .lock()
            .map(|mut size| size.take())
            .unwrap_or_default()
    }

    fn set_requested_gui_size(&self, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        if let Ok(mut size) = self.requested_gui_size.lock() {
            *size = Some(EditorSize { width, height });
            true
        } else {
            false
        }
    }

    fn request_params_flush(&self) {
        self.params_flush_requested.store(true, Ordering::Release);
    }

    fn take_params_flush_request(&self) -> bool {
        self.params_flush_requested.swap(false, Ordering::AcqRel)
    }
}

unsafe extern "C" fn host_get_extension(
    _host: *const clap_host,
    extension_id: *const c_char,
) -> *const c_void {
    if extension_id.is_null() {
        return std::ptr::null();
    }
    // SAFETY: CLAP passes a NUL-terminated extension id pointer.
    let id = unsafe { CStr::from_ptr(extension_id) };
    if id == CLAP_EXT_GUI {
        return (&HOST_GUI as *const clap_host_gui).cast();
    }
    if id == CLAP_EXT_LATENCY {
        return (&HOST_LATENCY as *const clap_host_latency).cast();
    }
    if id == CLAP_EXT_STATE {
        return (&HOST_STATE as *const clap_host_state).cast();
    }
    if id == CLAP_EXT_PARAMS {
        return (&HOST_PARAMS as *const clap_host_params).cast();
    }
    std::ptr::null()
}

unsafe extern "C" fn host_request_restart(_host: *const clap_host) {}
unsafe extern "C" fn host_request_process(_host: *const clap_host) {}
unsafe extern "C" fn host_request_callback(_host: *const clap_host) {}

static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(host_gui_resize_hints_changed),
    request_resize: Some(host_gui_request_resize),
    request_show: Some(host_gui_request_show),
    request_hide: Some(host_gui_request_hide),
    closed: Some(host_gui_closed),
};

static HOST_LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(host_latency_changed),
};

static HOST_STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(host_state_mark_dirty),
};

static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(host_params_rescan),
    clear: Some(host_params_clear),
    request_flush: Some(host_params_request_flush),
};

unsafe extern "C" fn host_gui_resize_hints_changed(_host: *const clap_host) {}
unsafe extern "C" fn host_gui_request_resize(
    host: *const clap_host,
    width: u32,
    height: u32,
) -> bool {
    if host.is_null() {
        return false;
    }
    // SAFETY: CLAP passes back the host pointer we supplied; `host_data` stores `HostContext`.
    let context = unsafe { (*host).host_data.cast::<HostContext>().as_ref() };
    context.is_some_and(|context| context.set_requested_gui_size(width, height))
}
unsafe extern "C" fn host_gui_request_show(_host: *const clap_host) -> bool {
    true
}
unsafe extern "C" fn host_gui_request_hide(_host: *const clap_host) -> bool {
    true
}
unsafe extern "C" fn host_gui_closed(_host: *const clap_host, _was_destroyed: bool) {}
unsafe extern "C" fn host_latency_changed(_host: *const clap_host) {}
unsafe extern "C" fn host_state_mark_dirty(_host: *const clap_host) {}
unsafe extern "C" fn host_params_rescan(_host: *const clap_host, _flags: u32) {}
unsafe extern "C" fn host_params_clear(_host: *const clap_host, _param_id: u32, _flags: u32) {}
unsafe extern "C" fn host_params_request_flush(host: *const clap_host) {
    if host.is_null() {
        return;
    }
    // SAFETY: CLAP passes back the host pointer we supplied; `host_data` stores `HostContext`.
    if let Some(context) = unsafe { (*host).host_data.cast::<HostContext>().as_ref() } {
        context.request_params_flush();
    }
}

struct ClapRuntimeInner {
    _module: Arc<LoadedModule>,
    host: Box<HostContext>,
    plugin: NonNull<clap_plugin>,
    descriptor: &'static ProcessorDescriptor,
    process: unsafe extern "C" fn(*const clap_plugin, *const clap_process) -> i32,
    activated: bool,
    processing: bool,
    destroyed: bool,
}

// SAFETY: The runtime is always shared behind a `Mutex`; raw CLAP pointers are accessed only while
// holding that mutex and remain valid until the paired destroy call in `Drop`.
unsafe impl Send for ClapRuntimeInner {}

impl ClapRuntimeInner {
    fn instantiate(
        metadata: &ClapPluginMetadata,
        descriptor: &'static ProcessorDescriptor,
        sample_rate: usize,
        block_size: usize,
    ) -> Result<Self, ClapRuntimeError> {
        let module = load_module(&metadata.path, &metadata.library_path)?;
        let host = HostContext::new();
        let clap_id_c = CString::new(metadata.clap_id.clone())
            .map_err(|_| ClapRuntimeError::InvalidPluginId(metadata.clap_id.clone()))?;
        let factory = module.plugin_factory()?;
        // SAFETY: Factory pointer comes from the initialized CLAP entry and is checked for null.
        let factory_ref = unsafe { factory.as_ref() }.ok_or(ClapRuntimeError::MissingFactory)?;
        let create_plugin = factory_ref
            .create_plugin
            .ok_or(ClapRuntimeError::MissingFunction("create_plugin"))?;
        // SAFETY: Factory and host pointers are valid for this call. `clap_id_c` is NUL-terminated.
        let plugin = unsafe { create_plugin(factory, host.as_ptr(), clap_id_c.as_ptr()) };
        let plugin =
            NonNull::new(plugin as *mut clap_plugin).ok_or(ClapRuntimeError::CreatePluginFailed)?;
        // SAFETY: `plugin` is a live CLAP plugin pointer from the factory.
        let plugin_ref = unsafe { plugin.as_ref() };
        let init = plugin_ref
            .init
            .ok_or(ClapRuntimeError::MissingFunction("plugin.init"))?;
        // SAFETY: CLAP plugin init is called once before activation.
        if unsafe { !init(plugin.as_ptr()) } {
            return Err(ClapRuntimeError::PluginInitFailed);
        }
        let process = plugin_ref.process.ok_or(ClapRuntimeError::MissingProcess)?;

        let mut runtime = Self {
            _module: module,
            host,
            plugin,
            descriptor,
            process,
            activated: false,
            processing: false,
            destroyed: false,
        };
        runtime.activate(sample_rate, block_size)?;
        Ok(runtime)
    }

    fn activate(&mut self, sample_rate: usize, block_size: usize) -> Result<(), ClapRuntimeError> {
        if self.destroyed {
            return Err(ClapRuntimeError::PluginInitFailed);
        }
        // SAFETY: `plugin` is a live CLAP plugin pointer.
        let plugin = unsafe { self.plugin.as_ref() };
        if let Some(activate) = plugin.activate {
            // SAFETY: Activation uses the current engine settings and a valid plugin pointer.
            if unsafe {
                !activate(
                    self.plugin.as_ptr(),
                    sample_rate.max(1) as f64,
                    1,
                    block_size.max(1) as u32,
                )
            } {
                return Err(ClapRuntimeError::ActivateFailed);
            }
            self.activated = true;
        }
        if let Some(start_processing) = plugin.start_processing {
            // SAFETY: Called after successful initialization/activation.
            if unsafe { !start_processing(self.plugin.as_ptr()) } {
                return Err(ClapRuntimeError::StartProcessingFailed);
            }
            self.processing = true;
        }
        Ok(())
    }

    fn plugin_extension<T>(&self, id: &CStr) -> Option<&T> {
        if self.destroyed {
            return None;
        }
        // SAFETY: `plugin` is live and extension pointer is checked for null.
        let plugin = unsafe { self.plugin.as_ref() };
        let get_extension = plugin.get_extension?;
        // SAFETY: CLAP extension id is a static NUL-terminated string.
        let extension = unsafe { get_extension(self.plugin.as_ptr(), id.as_ptr()) };
        // SAFETY: CLAP returns an extension table matching the requested id.
        unsafe { (extension as *const T).as_ref() }
    }

    fn process_block(
        &mut self,
        input_left: Option<&[f32]>,
        input_right: Option<&[f32]>,
        output_left: &mut [f32],
        output_right: &mut [f32],
        events: &[clap_event_midi],
    ) -> bool {
        if self.destroyed {
            return false;
        }
        let _ = clap_flush_params_if_requested(
            &self.host,
            self.plugin.as_ptr(),
            self.plugin_extension::<clap_plugin_params>(CLAP_EXT_PARAMS),
        );
        let frames = output_left.len().min(output_right.len());
        let in_left = input_left
            .map(|input| input.as_ptr() as *mut f32)
            .unwrap_or(std::ptr::null_mut());
        let in_right = input_right
            .map(|input| input.as_ptr() as *mut f32)
            .unwrap_or(std::ptr::null_mut());
        let out_left = output_left.as_mut_ptr();
        let out_right = output_right.as_mut_ptr();
        let mut input_channels = [in_left, in_right];
        let mut output_channels = [out_left, out_right];
        let input_buffer = clap_audio_buffer {
            data32: input_channels.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut output_buffer = clap_audio_buffer {
            data32: output_channels.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let mut event_list = ClapInputEventList { events };
        let in_events = clap_input_events {
            ctx: (&mut event_list as *mut ClapInputEventList<'_>).cast(),
            size: Some(clap_input_events_size),
            get: Some(clap_input_events_get),
        };
        let out_events = clap_output_events {
            ctx: std::ptr::null_mut(),
            try_push: Some(clap_output_events_try_push),
        };
        let process = clap_process {
            steady_time: -1,
            frames_count: frames as u32,
            transport: std::ptr::null(),
            audio_inputs: input_left.map_or(std::ptr::null(), |_| &input_buffer),
            audio_outputs: &mut output_buffer,
            audio_inputs_count: u32::from(input_left.is_some()),
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };
        // SAFETY: The process struct and buffers remain valid for the duration of the call.
        unsafe { (self.process)(self.plugin.as_ptr(), &process) != CLAP_PROCESS_ERROR }
    }

    fn reset(&mut self) {
        if self.destroyed {
            return;
        }
        // SAFETY: `plugin` is live while the runtime exists.
        if let Some(reset) = unsafe { self.plugin.as_ref() }.reset {
            // SAFETY: Reset is a CLAP callback on a live plugin.
            unsafe { reset(self.plugin.as_ptr()) };
        }
    }

    fn latency_samples(&self) -> u32 {
        self.plugin_extension::<clap_plugin_latency>(CLAP_EXT_LATENCY)
            .and_then(|extension| extension.get)
            .map_or(0, |get| {
                // SAFETY: Latency extension table belongs to this live plugin.
                unsafe { get(self.plugin.as_ptr()) }
            })
    }

    fn save_state(&mut self) -> Result<ProcessorState, ControllerError> {
        let Some(state) = self.plugin_extension::<clap_plugin_state>(CLAP_EXT_STATE) else {
            return Ok(ProcessorState::default());
        };
        let Some(save) = state.save else {
            return Ok(ProcessorState::default());
        };
        let mut bytes = Vec::new();
        let stream = clap_ostream {
            ctx: (&mut bytes as *mut Vec<u8>).cast(),
            write: Some(ostream_write),
        };
        // SAFETY: Stream callback appends to `bytes`, which outlives the call.
        if unsafe { save(self.plugin.as_ptr(), &stream) } {
            Ok(ProcessorState(bytes))
        } else {
            Err(ControllerError::Backend(
                "CLAP state save failed".to_string(),
            ))
        }
    }

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        if state.0.is_empty() {
            return Ok(());
        }
        let Some(extension) = self.plugin_extension::<clap_plugin_state>(CLAP_EXT_STATE) else {
            return Ok(());
        };
        let Some(load) = extension.load else {
            return Ok(());
        };
        let mut input = InputStreamState {
            bytes: &state.0,
            offset: 0,
        };
        let stream = clap_istream {
            ctx: (&mut input as *mut InputStreamState<'_>).cast(),
            read: Some(istream_read),
        };
        // SAFETY: Stream callback reads from `state`, which outlives the call.
        if unsafe { load(self.plugin.as_ptr(), &stream) } {
            Ok(())
        } else {
            Err(ProcessorStateError::Decode(
                "CLAP state load failed".to_string(),
            ))
        }
    }

    fn prepare_destroy(&mut self) {
        if self.destroyed {
            return;
        }
        // SAFETY: `plugin` is live until `destroy` is called below.
        let plugin = unsafe { self.plugin.as_ref() };
        if self.processing
            && let Some(stop_processing) = plugin.stop_processing
        {
            // SAFETY: Called once before deactivation/destroy.
            unsafe { stop_processing(self.plugin.as_ptr()) };
        }
        self.processing = false;
        if self.activated
            && let Some(deactivate) = plugin.deactivate
        {
            // SAFETY: Paired with successful activation.
            unsafe { deactivate(self.plugin.as_ptr()) };
        }
        self.activated = false;
        if let Some(destroy) = plugin.destroy {
            // SAFETY: Final plugin lifecycle call. No further plugin access happens after this.
            unsafe { destroy(self.plugin.as_ptr()) };
        }
        self.destroyed = true;
    }
}

impl LoadedModule {
    fn plugin_factory(&self) -> Result<*const clap_plugin_factory, ClapRuntimeError> {
        // SAFETY: `entry` points into a loaded library kept alive by `self`.
        let entry = unsafe { self.entry.as_ref() };
        let get_factory = entry
            .get_factory
            .ok_or(ClapRuntimeError::MissingFunction("get_factory"))?;
        // SAFETY: Factory id is a static C string.
        let factory = unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) };
        if factory.is_null() {
            Err(ClapRuntimeError::MissingFactory)
        } else {
            Ok(factory.cast())
        }
    }
}

impl Drop for ClapRuntimeInner {
    fn drop(&mut self) {
        self.prepare_destroy();
    }
}

struct ClapInputEventList<'a> {
    events: &'a [clap_event_midi],
}

unsafe extern "C" fn clap_input_events_size(list: *const clap_input_events) -> u32 {
    if list.is_null() {
        return 0;
    }
    // SAFETY: `ctx` is set to a valid `ClapInputEventList` for the process call.
    let events = unsafe {
        &(*(list.as_ref().expect("checked above").ctx as *const ClapInputEventList<'_>)).events
    };
    events.len() as u32
}

unsafe extern "C" fn clap_input_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    if list.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `ctx` is set to a valid `ClapInputEventList` for the process call.
    let events = unsafe {
        &(*(list.as_ref().expect("checked above").ctx as *const ClapInputEventList<'_>)).events
    };
    events
        .get(index as usize)
        .map_or(std::ptr::null(), |event| &event.header)
}

unsafe extern "C" fn clap_output_events_try_push(
    _list: *const clap_output_events,
    _event: *const clap_event_header,
) -> bool {
    true
}

fn clap_flush_params_if_requested(
    host: &HostContext,
    plugin: *const clap_plugin,
    params: Option<&clap_plugin_params>,
) -> bool {
    if !host.take_params_flush_request() {
        return false;
    }
    let Some(flush) = params.and_then(|params| params.flush) else {
        return false;
    };
    let events: [clap_event_midi; 0] = [];
    let mut event_list = ClapInputEventList { events: &events };
    let in_events = clap_input_events {
        ctx: (&mut event_list as *mut ClapInputEventList<'_>).cast(),
        size: Some(clap_input_events_size),
        get: Some(clap_input_events_get),
    };
    let out_events = clap_output_events {
        ctx: std::ptr::null_mut(),
        try_push: Some(clap_output_events_try_push),
    };
    // SAFETY: The plugin pointer and event lists are valid for the duration of the call.
    unsafe { flush(plugin, &in_events, &out_events) };
    true
}

unsafe extern "C" fn ostream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    if stream.is_null() || buffer.is_null() {
        return -1;
    }
    // SAFETY: `ctx` points to the Vec owned by `save_state` for this callback.
    let bytes = unsafe { &mut *((*stream).ctx as *mut Vec<u8>) };
    // SAFETY: CLAP provides a readable buffer of `size` bytes for the callback.
    let input = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
    bytes.extend_from_slice(input);
    size as i64
}

struct InputStreamState<'a> {
    bytes: &'a [u8],
    offset: usize,
}

unsafe extern "C" fn istream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    if stream.is_null() || buffer.is_null() {
        return -1;
    }
    // SAFETY: `ctx` points to the input stream state owned by `load_state`.
    let input = unsafe { &mut *((*stream).ctx as *mut InputStreamState<'_>) };
    let remaining = input.bytes.len().saturating_sub(input.offset);
    let to_copy = remaining.min(size as usize);
    // SAFETY: CLAP provides a writable output buffer of `size` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(
            input.bytes[input.offset..].as_ptr(),
            buffer.cast::<u8>(),
            to_copy,
        );
    }
    input.offset += to_copy;
    to_copy as i64
}

#[derive(Clone)]
struct ClapBinding {
    shared: Arc<Mutex<ClapRuntimeInner>>,
}

impl RuntimeBinding for ClapBinding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(ClapController {
            shared: self.shared.clone(),
        })
    }

    fn latency_samples(&self) -> u32 {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latency_samples()
    }

    fn prepare_destroy(&self) {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prepare_destroy();
    }
}

struct ClapController {
    shared: Arc<Mutex<ClapRuntimeInner>>,
}

impl Controller for ClapController {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .descriptor
    }

    fn get_param(&self, id: &str) -> Result<f32, ControllerError> {
        Err(ControllerError::UnknownParameter(id.to_string()))
    }

    fn set_param(&self, id: &str, _normalized: f32) -> Result<(), ControllerError> {
        Err(ControllerError::UnknownParameter(id.to_string()))
    }

    fn save_state(&self) -> Result<ProcessorState, ControllerError> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .save_state()
    }

    fn load_state(&self, state: &ProcessorState) -> Result<(), ControllerError> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .load_state(state)
            .map_err(|error| ControllerError::Backend(error.to_string()))
    }

    fn create_editor_session(&self) -> Result<Option<Box<dyn EditorSession>>, EditorError> {
        let has_gui = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .plugin_extension::<clap_plugin_gui>(CLAP_EXT_GUI)
            .is_some();
        Ok(has_gui.then(|| {
            Box::new(ClapEditorSession {
                shared: self.shared.clone(),
                created: false,
                attached: false,
                initial_size: None,
            }) as Box<dyn EditorSession>
        }))
    }
}

struct ClapProcessor {
    shared: Arc<Mutex<ClapRuntimeInner>>,
    midi: ClapMidiEventQueue,
}

struct ClapMidiEventQueue {
    pending: Vec<clap_event_midi>,
    active_notes: [[bool; 128]; 16],
}

impl ClapMidiEventQueue {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            active_notes: [[false; 128]; 16],
        }
    }

    fn push(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                channel,
                note,
                velocity,
            } if velocity > 0 => {
                self.active_notes[(channel & 0x0f) as usize][note as usize] = true;
                self.pending
                    .push(clap_midi_event([0x90 | (channel & 0x0f), note, velocity]));
            }
            MidiEvent::NoteOn { channel, note, .. } => {
                self.active_notes[(channel & 0x0f) as usize][note as usize] = false;
                self.pending
                    .push(clap_midi_event([0x80 | (channel & 0x0f), note, 0]));
            }
            MidiEvent::NoteOff {
                channel,
                note,
                velocity,
            } => {
                self.active_notes[(channel & 0x0f) as usize][note as usize] = false;
                self.pending
                    .push(clap_midi_event([0x80 | (channel & 0x0f), note, velocity]));
            }
            MidiEvent::AllNotesOff { channel } => {
                self.push_active_note_offs(channel);
                self.pending
                    .push(clap_midi_event([0xb0 | (channel & 0x0f), 123, 0]));
            }
            MidiEvent::AllSoundOff { channel } => {
                self.push_active_note_offs(channel);
                self.pending
                    .push(clap_midi_event([0xb0 | (channel & 0x0f), 120, 0]));
            }
            event => {
                if let Some(event) = midi_event_to_clap(event) {
                    self.pending.push(event);
                }
            }
        }
    }

    fn push_active_note_offs(&mut self, channel: u8) {
        let channel = channel & 0x0f;
        for note in 0..128 {
            if self.active_notes[channel as usize][note] {
                self.pending
                    .push(clap_midi_event([0x80 | channel, note as u8, 0]));
                self.active_notes[channel as usize][note] = false;
            }
        }
    }

    fn push_panic(&mut self) {
        for channel in 0..16_u8 {
            self.push_active_note_offs(channel);
            self.pending.push(clap_midi_event([0xb0 | channel, 120, 0]));
            self.pending.push(clap_midi_event([0xb0 | channel, 123, 0]));
            self.pending.push(clap_midi_event([0xb0 | channel, 121, 0]));
        }
    }

    fn take(&mut self) -> Vec<clap_event_midi> {
        std::mem::take(&mut self.pending)
    }
}

impl Processor for ClapProcessor {
    fn descriptor(&self) -> &'static ProcessorDescriptor {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .descriptor
    }

    fn set_param(&mut self, _id: &str, _normalized: f32) -> bool {
        false
    }

    fn get_param(&self, _id: &str) -> Option<f32> {
        None
    }

    fn save_state(&self) -> ProcessorState {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .save_state()
            .unwrap_or_default()
    }

    fn load_state(&mut self, state: &ProcessorState) -> Result<(), ProcessorStateError> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .load_state(state)
    }

    fn reset(&mut self) {
        self.midi.push_panic();
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reset();
    }

    fn latency_samples(&self) -> u32 {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latency_samples()
    }

    fn create_editor_session(&self) -> Result<Option<Box<dyn EditorSession>>, EditorError> {
        ClapController {
            shared: self.shared.clone(),
        }
        .create_editor_session()
    }
}

impl InstrumentProcessor for ClapProcessor {
    fn handle_midi(&mut self, event: MidiEvent) {
        self.midi.push(event);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        left.fill(0.0);
        right.fill(0.0);
        let events = self.midi.take();
        let _ = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .process_block(None, None, left, right, &events);
    }
}

impl EffectProcessor for ClapProcessor {
    fn process(
        &mut self,
        in_left: &[f32],
        in_right: &[f32],
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        let events = self.midi.take();
        if !self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .process_block(Some(in_left), Some(in_right), out_left, out_right, &events)
        {
            out_left.copy_from_slice(in_left);
            out_right.copy_from_slice(in_right);
        }
    }
}

fn midi_event_to_clap(event: MidiEvent) -> Option<clap_event_midi> {
    let data = match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => [0x90 | (channel & 0x0f), note, velocity],
        MidiEvent::NoteOff {
            channel,
            note,
            velocity,
        } => [0x80 | (channel & 0x0f), note, velocity],
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => [0xb0 | (channel & 0x0f), controller, value],
        MidiEvent::ProgramChange { channel, program } => [0xc0 | (channel & 0x0f), program, 0],
        MidiEvent::ChannelPressure { channel, pressure } => [0xd0 | (channel & 0x0f), pressure, 0],
        MidiEvent::PolyPressure {
            channel,
            note,
            pressure,
        } => [0xa0 | (channel & 0x0f), note, pressure],
        MidiEvent::PitchBend { channel, value } => {
            let value = (i32::from(value) + 8192).clamp(0, 16_383) as u16;
            [
                0xe0 | (channel & 0x0f),
                (value & 0x7f) as u8,
                (value >> 7) as u8,
            ]
        }
        MidiEvent::AllNotesOff { channel } => [0xb0 | (channel & 0x0f), 123, 0],
        MidiEvent::AllSoundOff { channel } => [0xb0 | (channel & 0x0f), 120, 0],
        MidiEvent::ResetAllControllers { channel } => [0xb0 | (channel & 0x0f), 121, 0],
    };
    Some(clap_midi_event(data))
}

fn clap_midi_event(data: [u8; 3]) -> clap_event_midi {
    clap_event_midi {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_midi>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_MIDI,
            flags: CLAP_EVENT_IS_LIVE,
        },
        port_index: 0,
        data,
    }
}

struct ClapEditorSession {
    shared: Arc<Mutex<ClapRuntimeInner>>,
    created: bool,
    attached: bool,
    initial_size: Option<EditorSize>,
}

impl EditorSession for ClapEditorSession {
    fn initial_size(&mut self) -> Result<Option<EditorSize>, EditorError> {
        Ok(self.initial_size)
    }

    fn requested_size(&mut self) -> Result<Option<EditorSize>, EditorError> {
        let runtime = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(runtime.host.take_requested_gui_size())
    }

    fn attach(&mut self, parent: EditorParent) -> Result<(), EditorError> {
        let window = clap_window_for_parent(parent)?;
        let runtime = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gui = runtime
            .plugin_extension::<clap_plugin_gui>(CLAP_EXT_GUI)
            .ok_or(EditorError::Unsupported)?;
        if let Some(is_api_supported) = gui.is_api_supported {
            // SAFETY: GUI extension table and API string are valid for this live plugin.
            if unsafe { !is_api_supported(runtime.plugin.as_ptr(), window.api, false) } {
                return Err(EditorError::Unsupported);
            }
        }
        if !self.created {
            let create = gui.create.ok_or(EditorError::Unsupported)?;
            // SAFETY: GUI is created once with an API supported by the host window.
            if unsafe { !create(runtime.plugin.as_ptr(), window.api, false) } {
                return Err(EditorError::Backend("CLAP GUI creation failed".to_string()));
            }
            self.created = true;
        }
        let gui = runtime
            .plugin_extension::<clap_plugin_gui>(CLAP_EXT_GUI)
            .ok_or(EditorError::Unsupported)?;
        let set_parent = gui.set_parent.ok_or(EditorError::Unsupported)?;
        // SAFETY: The host parent window stays valid for the editor-host window lifetime.
        if unsafe { !set_parent(runtime.plugin.as_ptr(), &window) } {
            return Err(EditorError::Backend(
                "CLAP GUI parenting failed".to_string(),
            ));
        }
        self.initial_size = runtime
            .host
            .take_requested_gui_size()
            .or_else(|| clap_gui_reported_size(gui, runtime.plugin.as_ptr()));
        self.attached = true;
        Ok(())
    }

    fn detach(&mut self) -> Result<(), EditorError> {
        let runtime = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(gui) = runtime.plugin_extension::<clap_plugin_gui>(CLAP_EXT_GUI) {
            clap_gui_destroy_created(gui, runtime.plugin.as_ptr(), self.created);
        }
        self.created = false;
        self.attached = false;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), EditorError> {
        clap_gui_set_embedded_visible(visible)
    }

    fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
        let runtime = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(gui) = runtime.plugin_extension::<clap_plugin_gui>(CLAP_EXT_GUI) else {
            return Ok(size);
        };
        let mut size = size;
        if !clap_gui_adjusted_resize(gui, runtime.plugin.as_ptr(), &mut size) {
            return Ok(size);
        }
        self.initial_size = Some(size);
        Ok(size)
    }
}

fn clap_gui_destroy_created(gui: &clap_plugin_gui, plugin: *const clap_plugin, created: bool) {
    if created && let Some(destroy) = gui.destroy {
        // SAFETY: Destroy is paired with successful GUI creation.
        unsafe { destroy(plugin) };
    }
}

fn clap_gui_set_embedded_visible(_visible: bool) -> Result<(), EditorError> {
    Ok(())
}

impl Drop for ClapEditorSession {
    fn drop(&mut self) {
        if self.created || self.attached {
            let _ = self.detach();
        }
    }
}

fn clap_gui_reported_size(gui: &clap_plugin_gui, plugin: *const clap_plugin) -> Option<EditorSize> {
    let get_size = gui.get_size?;
    let mut width = 0;
    let mut height = 0;
    // SAFETY: The GUI extension belongs to the live plugin and receives valid out-pointers.
    if unsafe { !get_size(plugin, &mut width, &mut height) } || width == 0 || height == 0 {
        return None;
    }
    Some(EditorSize { width, height })
}

fn clap_gui_adjusted_resize(
    gui: &clap_plugin_gui,
    plugin: *const clap_plugin,
    size: &mut EditorSize,
) -> bool {
    let Some(can_resize) = gui.can_resize else {
        return false;
    };
    // SAFETY: The GUI extension belongs to the live plugin.
    if unsafe { !can_resize(plugin) } {
        return false;
    }

    let mut width = size.width;
    let mut height = size.height;
    if let Some(adjust_size) = gui.adjust_size {
        // SAFETY: The GUI extension belongs to the live plugin and receives valid out-pointers.
        if unsafe { !adjust_size(plugin, &mut width, &mut height) } || width == 0 || height == 0 {
            return false;
        }
    }

    let Some(set_size) = gui.set_size else {
        return false;
    };
    // SAFETY: The GUI extension belongs to the live plugin.
    if unsafe { !set_size(plugin, width, height) } {
        return false;
    }

    *size = EditorSize { width, height };
    true
}

fn clap_window_for_parent(parent: EditorParent) -> Result<clap_window, EditorError> {
    match parent.window {
        RawWindowHandle::AppKit(handle) => Ok(clap_window {
            api: CLAP_WINDOW_API_COCOA.as_ptr(),
            specific: clap_window_handle {
                cocoa: handle.ns_view.as_ptr(),
            },
        }),
        RawWindowHandle::Win32(handle) => Ok(clap_window {
            api: CLAP_WINDOW_API_WIN32.as_ptr(),
            specific: clap_window_handle {
                win32: handle.hwnd.get() as *mut c_void,
            },
        }),
        RawWindowHandle::Xlib(handle) => Ok(clap_window {
            api: CLAP_WINDOW_API_X11.as_ptr(),
            specific: clap_window_handle { x11: handle.window },
        }),
        RawWindowHandle::Xcb(handle) => Ok(clap_window {
            api: CLAP_WINDOW_API_X11.as_ptr(),
            specific: clap_window_handle {
                x11: handle.window.get().into(),
            },
        }),
        RawWindowHandle::Wayland(handle) => Ok(clap_window {
            api: CLAP_WINDOW_API_WAYLAND.as_ptr(),
            specific: clap_window_handle {
                ptr: handle.surface.as_ptr().cast(),
            },
        }),
        other => Err(EditorError::HostUnavailable(format!(
            "unsupported CLAP editor parent: {other:?}"
        ))),
    }
}

/// Registers validated CLAP plugins in the shared audio registry.
pub fn register_plugins(plugins: impl IntoIterator<Item = ClapPluginMetadata>) {
    let plugins = plugins.into_iter().collect::<Vec<_>>();
    {
        let mut metadata = metadata_store()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for plugin in &plugins {
            metadata.insert(plugin.processor_id.clone(), plugin.clone());
        }
    }
    let entries = plugins.into_iter().map(registry_entry_for_plugin);
    registry::register(entries);
}

fn registry_entry_for_plugin(plugin: ClapPluginMetadata) -> registry::Entry {
    let descriptor = Box::leak(Box::new(ProcessorDescriptor {
        name: Box::leak(plugin.name.clone().into_boxed_str()),
        params: &[],
        editor: Some(DEFAULT_CLAP_EDITOR_DESCRIPTOR),
    }));
    match plugin.role {
        registry::Role::Instrument => registry::Entry::plugin_instrument(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Clap,
            plugin.vendor,
            descriptor,
            create_clap_instrument_runtime,
        ),
        registry::Role::Effect => registry::Entry::plugin_effect(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Clap,
            plugin.vendor,
            descriptor,
            create_clap_effect_runtime,
        ),
    }
}

fn create_clap_instrument_runtime(
    slot: &SlotState,
    context: &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
    let Some((metadata, descriptor)) = metadata_and_descriptor(slot)? else {
        return Ok(None);
    };
    let shared = instantiate_shared(
        &metadata,
        descriptor,
        context.soundfont_settings.sample_rate.max(1) as usize,
        context.soundfont_settings.block_size.max(1),
        &slot.state,
    )?;
    Ok(Some(InstrumentRuntimeSpec {
        processor: Box::new(ClapProcessor {
            shared: shared.clone(),
            midi: ClapMidiEventQueue::new(),
        }),
        binding: Box::new(ClapBinding { shared }),
    }))
}

fn create_clap_effect_runtime(
    slot: &SlotState,
    context: &EffectRuntimeContext,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    let Some((metadata, descriptor)) = metadata_and_descriptor(slot)? else {
        return Ok(None);
    };
    let shared = instantiate_shared(
        &metadata,
        descriptor,
        context.sample_rate,
        context.block_size,
        &slot.state,
    )?;
    Ok(Some(EffectRuntimeSpec {
        processor: Box::new(ClapProcessor {
            shared: shared.clone(),
            midi: ClapMidiEventQueue::new(),
        }),
        binding: Some(Box::new(ClapBinding { shared })),
    }))
}

fn metadata_and_descriptor(
    slot: &SlotState,
) -> Result<Option<(ClapPluginMetadata, &'static ProcessorDescriptor)>, RuntimeFactoryError> {
    let lilypalooza_audio::ProcessorKind::Plugin { plugin_id } = &slot.kind else {
        return Ok(None);
    };
    let metadata = plugin_metadata(plugin_id)
        .map_err(|error| RuntimeFactoryError::Backend(error.to_string()))?;
    let descriptor = registry::entry(plugin_id)
        .map(|entry| entry.descriptor)
        .ok_or_else(|| {
            RuntimeFactoryError::Backend(format!("CLAP plugin `{plugin_id}` is not registered"))
        })?;
    Ok(Some((metadata, descriptor)))
}

fn instantiate_shared(
    metadata: &ClapPluginMetadata,
    descriptor: &'static ProcessorDescriptor,
    sample_rate: usize,
    block_size: usize,
    state: &ProcessorState,
) -> Result<Arc<Mutex<ClapRuntimeInner>>, RuntimeFactoryError> {
    let mut runtime = ClapRuntimeInner::instantiate(metadata, descriptor, sample_rate, block_size)
        .map_err(|error| RuntimeFactoryError::Backend(error.to_string()))?;
    runtime
        .load_state(state)
        .map_err(RuntimeFactoryError::State)?;
    Ok(Arc::new(Mutex::new(runtime)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;
    use std::sync::atomic::AtomicUsize;

    use raw_window_handle::{AppKitWindowHandle, RawWindowHandle};

    #[test]
    fn clap_candidate_detection_is_extension_based() {
        assert!(is_clap_candidate(Path::new("Plugin.clap")));
        assert!(is_clap_candidate(Path::new("Plugin.CLAP")));
        assert!(!is_clap_candidate(Path::new("Plugin.vst3")));
    }

    #[test]
    fn candidate_paths_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("Vendor").join("Plugin.clap");
        std::fs::create_dir_all(&nested).expect("nested clap dir");
        std::fs::write(dir.path().join("Root.clap"), "").expect("root clap file");

        let candidates = candidate_paths(dir.path()).expect("candidate paths");

        assert_eq!(candidates, vec![dir.path().join("Root.clap"), nested]);
    }

    #[test]
    fn stable_processor_id_includes_path_and_clap_id() {
        assert_eq!(
            stable_processor_id(Path::new("/Plug/Test.clap"), "org.test"),
            "clap:/Plug/Test.clap#org.test"
        );
    }

    #[test]
    fn validation_report_serializes_structured_success() {
        let report = ValidationReport {
            format: FORMAT.to_string(),
            path: PathBuf::from("/Plug/Test.clap"),
            result: Ok(vec![ClapPluginMetadata {
                processor_id: "clap:/Plug/Test.clap#org.test".to_string(),
                clap_id: "org.test".to_string(),
                name: "Test".to_string(),
                vendor: Some("Vendor".to_string()),
                version: Some("1.0".to_string()),
                features: vec!["audio-effect".to_string()],
                role: registry::Role::Effect,
                path: PathBuf::from("/Plug/Test.clap"),
                library_path: PathBuf::from("/Plug/Test.clap"),
            }]),
        };

        let json = serde_json::to_string(&report).expect("report should serialize");
        let parsed: ValidationReport =
            serde_json::from_str(&json).expect("report should deserialize");

        assert_eq!(parsed, report);
    }

    #[test]
    fn probe_rejects_factory_without_plugin_descriptors() {
        let error = probe_initialized_factory(
            Path::new("/Plug/Empty.clap"),
            Path::new("/Plug/Empty.clap"),
            empty_factory,
        )
        .expect_err("factory with zero plugins should be invalid");

        assert!(matches!(error, ClapProbeError::NoPluginDescriptors));
    }

    #[test]
    fn clap_gui_reported_size_reads_plugin_native_size() {
        let size = clap_gui_reported_size(
            &SIZED_PLUGIN_GUI,
            NonNull::<clap_plugin>::dangling().as_ptr(),
        )
        .expect("plugin GUI should report size");

        assert_eq!(
            size,
            EditorSize {
                width: 936,
                height: 612,
            }
        );
    }

    #[test]
    fn host_gui_request_resize_records_requested_editor_size() {
        let host = HostContext::new();

        // SAFETY: `host` owns a valid CLAP host pointer for this test.
        assert!(unsafe { host_gui_request_resize(host.as_ptr(), 880, 540) });

        assert_eq!(
            host.take_requested_gui_size(),
            Some(EditorSize {
                width: 880,
                height: 540,
            })
        );
        assert_eq!(host.take_requested_gui_size(), None);
    }

    #[test]
    fn host_params_request_flush_is_consumed_by_plugin_flush() {
        PARAM_FLUSH_COUNT.store(0, Ordering::Release);
        let host = HostContext::new();
        let params = clap_plugin_params {
            count: None,
            get_info: None,
            get_value: None,
            value_to_text: None,
            text_to_value: None,
            flush: Some(test_params_flush),
        };

        // SAFETY: `host` owns a valid CLAP host pointer for this test.
        unsafe { host_params_request_flush(host.as_ptr()) };

        assert!(clap_flush_params_if_requested(
            &host,
            NonNull::<clap_plugin>::dangling().as_ptr(),
            Some(&params),
        ));
        assert_eq!(PARAM_FLUSH_COUNT.load(Ordering::Acquire), 1);
        assert!(!clap_flush_params_if_requested(
            &host,
            NonNull::<clap_plugin>::dangling().as_ptr(),
            Some(&params),
        ));
    }

    #[test]
    fn clap_midi_queue_expands_all_notes_off_to_active_note_offs() {
        let mut queue = ClapMidiEventQueue::new();
        queue.push(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        queue.push(MidiEvent::NoteOn {
            channel: 0,
            note: 64,
            velocity: 100,
        });
        queue.push(MidiEvent::NoteOn {
            channel: 1,
            note: 67,
            velocity: 100,
        });
        let _ = queue.take();

        queue.push(MidiEvent::AllNotesOff { channel: 0 });
        let data: Vec<_> = queue.take().into_iter().map(|event| event.data).collect();

        assert_eq!(data, vec![[0x80, 60, 0], [0x80, 64, 0], [0xb0, 123, 0]]);

        queue.push(MidiEvent::AllNotesOff { channel: 1 });
        let data: Vec<_> = queue.take().into_iter().map(|event| event.data).collect();

        assert_eq!(data, vec![[0x81, 67, 0], [0xb1, 123, 0]]);
    }

    #[test]
    fn clap_midi_queue_panic_emits_active_note_offs_before_controller_panic() {
        let mut queue = ClapMidiEventQueue::new();
        queue.push(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        });
        queue.push(MidiEvent::NoteOn {
            channel: 1,
            note: 64,
            velocity: 100,
        });
        let _ = queue.take();

        queue.push_panic();
        let data: Vec<_> = queue.take().into_iter().map(|event| event.data).collect();

        assert_eq!(
            &data[..4],
            &[
                [0x80, 60, 0],
                [0xb0, 120, 0],
                [0xb0, 123, 0],
                [0xb0, 121, 0]
            ]
        );
        assert_eq!(
            &data[4..8],
            &[
                [0x81, 64, 0],
                [0xb1, 120, 0],
                [0xb1, 123, 0],
                [0xb1, 121, 0]
            ]
        );
        assert_eq!(data.len(), 50);
    }

    #[test]
    fn clap_gui_resize_returns_none_when_plugin_cannot_resize() {
        let mut requested = EditorSize {
            width: 936,
            height: 612,
        };

        let resized = clap_gui_adjusted_resize(
            &NON_RESIZABLE_PLUGIN_GUI,
            NonNull::<clap_plugin>::dangling().as_ptr(),
            &mut requested,
        );

        assert!(!resized);
        assert_eq!(
            requested,
            EditorSize {
                width: 936,
                height: 612,
            }
        );
    }

    #[test]
    fn clap_gui_resize_adjusts_before_setting_size() {
        let mut requested = EditorSize {
            width: 935,
            height: 611,
        };

        let resized = clap_gui_adjusted_resize(
            &RESIZABLE_PLUGIN_GUI,
            NonNull::<clap_plugin>::dangling().as_ptr(),
            &mut requested,
        );

        assert!(resized);
        assert_eq!(
            requested,
            EditorSize {
                width: 936,
                height: 612,
            }
        );
    }

    #[test]
    fn clap_gui_destroy_created_does_not_hide_embedded_gui() {
        GUI_DESTROY_COUNT.store(0, Ordering::Release);
        GUI_HIDE_COUNT.store(0, Ordering::Release);

        clap_gui_destroy_created(
            &DESTROY_TRACKING_PLUGIN_GUI,
            NonNull::<clap_plugin>::dangling().as_ptr(),
            true,
        );

        assert_eq!(GUI_DESTROY_COUNT.load(Ordering::Acquire), 1);
        assert_eq!(GUI_HIDE_COUNT.load(Ordering::Acquire), 0);
    }

    #[test]
    fn clap_embedded_visibility_is_host_window_only() {
        assert!(clap_gui_set_embedded_visible(false).is_ok());
        assert!(clap_gui_set_embedded_visible(true).is_ok());
    }

    static PARAM_FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);
    static GUI_DESTROY_COUNT: AtomicUsize = AtomicUsize::new(0);
    static GUI_HIDE_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn test_params_flush(
        _: *const clap_plugin,
        _: *const clap_input_events,
        _: *const clap_output_events,
    ) {
        PARAM_FLUSH_COUNT.fetch_add(1, Ordering::AcqRel);
    }

    static SIZED_PLUGIN_GUI: clap_plugin_gui = clap_plugin_gui {
        is_api_supported: None,
        get_preferred_api: None,
        create: None,
        destroy: None,
        set_scale: None,
        get_size: Some(sized_plugin_gui_get_size),
        can_resize: None,
        get_resize_hints: None,
        adjust_size: None,
        set_size: None,
        set_parent: None,
        set_transient: None,
        suggest_title: None,
        show: None,
        hide: None,
    };

    static NON_RESIZABLE_PLUGIN_GUI: clap_plugin_gui = clap_plugin_gui {
        can_resize: Some(non_resizable_plugin_gui_can_resize),
        set_size: Some(resizable_plugin_gui_set_size),
        ..SIZED_PLUGIN_GUI
    };

    static RESIZABLE_PLUGIN_GUI: clap_plugin_gui = clap_plugin_gui {
        can_resize: Some(resizable_plugin_gui_can_resize),
        adjust_size: Some(resizable_plugin_gui_adjust_size),
        set_size: Some(resizable_plugin_gui_set_size),
        ..SIZED_PLUGIN_GUI
    };

    static DESTROY_TRACKING_PLUGIN_GUI: clap_plugin_gui = clap_plugin_gui {
        destroy: Some(destroy_tracking_plugin_gui_destroy),
        hide: Some(destroy_tracking_plugin_gui_hide),
        ..SIZED_PLUGIN_GUI
    };

    unsafe extern "C" fn non_resizable_plugin_gui_can_resize(_: *const clap_plugin) -> bool {
        false
    }

    unsafe extern "C" fn resizable_plugin_gui_can_resize(_: *const clap_plugin) -> bool {
        true
    }

    unsafe extern "C" fn resizable_plugin_gui_adjust_size(
        _: *const clap_plugin,
        width: *mut u32,
        height: *mut u32,
    ) -> bool {
        // SAFETY: The test passes valid out-pointers.
        unsafe {
            *width += 1;
            *height += 1;
        }
        true
    }

    unsafe extern "C" fn resizable_plugin_gui_set_size(
        _: *const clap_plugin,
        width: u32,
        height: u32,
    ) -> bool {
        width == 936 && height == 612
    }

    unsafe extern "C" fn destroy_tracking_plugin_gui_destroy(_: *const clap_plugin) {
        GUI_DESTROY_COUNT.fetch_add(1, Ordering::AcqRel);
    }

    unsafe extern "C" fn destroy_tracking_plugin_gui_hide(_: *const clap_plugin) -> bool {
        GUI_HIDE_COUNT.fetch_add(1, Ordering::AcqRel);
        true
    }

    unsafe extern "C" fn sized_plugin_gui_get_size(
        _: *const clap_plugin,
        width: *mut u32,
        height: *mut u32,
    ) -> bool {
        // SAFETY: The test passes valid out-pointers.
        unsafe {
            *width = 936;
            *height = 612;
        }
        true
    }

    unsafe extern "C" fn empty_factory(factory_id: *const c_char) -> *const c_void {
        if factory_id.is_null() {
            return std::ptr::null();
        }
        // SAFETY: `factory_id` is checked for null and points to the static CLAP factory id.
        let factory_id = unsafe { CStr::from_ptr(factory_id) };
        if factory_id == CLAP_PLUGIN_FACTORY_ID {
            &raw const EMPTY_PLUGIN_FACTORY as *const c_void
        } else {
            std::ptr::null()
        }
    }

    static EMPTY_PLUGIN_FACTORY: clap_plugin_factory = clap_plugin_factory {
        get_plugin_count: Some(empty_plugin_count),
        get_plugin_descriptor: Some(empty_plugin_descriptor),
        create_plugin: Some(empty_create_plugin),
    };

    unsafe extern "C" fn empty_plugin_count(_: *const clap_plugin_factory) -> u32 {
        0
    }

    unsafe extern "C" fn empty_plugin_descriptor(
        _: *const clap_plugin_factory,
        _: u32,
    ) -> *const clap_plugin_descriptor {
        std::ptr::null()
    }

    unsafe extern "C" fn empty_create_plugin(
        _: *const clap_plugin_factory,
        _: *const clap_sys::host::clap_host,
        _: *const c_char,
    ) -> *const clap_sys::plugin::clap_plugin {
        std::ptr::null()
    }

    #[test]
    fn registered_clap_plugin_entry_exposes_editor_descriptor() {
        let plugin = ClapPluginMetadata {
            processor_id: "clap:/Plug/Editor.clap#org.test.editor".to_string(),
            clap_id: "org.test.editor".to_string(),
            name: "Editor Plugin".to_string(),
            vendor: None,
            version: None,
            features: vec!["audio-effect".to_string()],
            role: registry::Role::Effect,
            path: PathBuf::from("/Plug/Editor.clap"),
            library_path: PathBuf::from("/Plug/Editor.clap"),
        };

        register_plugins([plugin.clone()]);
        let entry = registry::entry(&plugin.processor_id).expect("plugin should be registered");

        assert_eq!(entry.backend, registry::Backend::Clap);
        assert_eq!(entry.descriptor.name, "Editor Plugin");
        assert_eq!(
            entry.descriptor.editor,
            Some(EditorDescriptor {
                resizable: false,
                ..DEFAULT_CLAP_EDITOR_DESCRIPTOR
            })
        );
    }

    #[test]
    fn clap_editor_default_size_uses_conservative_host_fallback() {
        assert_eq!(
            DEFAULT_CLAP_EDITOR_DESCRIPTOR.default_size,
            EditorSize {
                width: 720,
                height: 480,
            }
        );
    }

    #[test]
    fn runtime_key_parses_stable_processor_id() {
        let key = RuntimeKey::parse("clap:/Plug/Test.clap#org.test")
            .expect("stable CLAP processor id should parse");

        assert_eq!(key.path, PathBuf::from("/Plug/Test.clap"));
        assert_eq!(key.clap_id, "org.test");
    }

    #[test]
    fn appkit_parent_converts_to_cocoa_clap_window() {
        let ptr = NonNull::<std::ffi::c_void>::dangling();
        let parent = lilypalooza_audio::EditorParent {
            window: RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr)),
            display: None,
        };

        let window = clap_window_for_parent(parent).expect("appkit parent should be supported");

        // SAFETY: The tested conversion returns a static CLAP API C string.
        let api = unsafe { CStr::from_ptr(window.api) };
        // SAFETY: The window was constructed as a Cocoa CLAP window in this test.
        let cocoa = unsafe { window.specific.cocoa };
        assert_eq!(api, clap_sys::ext::gui::CLAP_WINDOW_API_COCOA);
        assert_eq!(cocoa, ptr.as_ptr());
    }
}
