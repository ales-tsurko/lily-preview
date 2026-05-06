//! VST3 plugin adapter.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use lilypalooza_audio::instrument::{
    Controller, ControllerError, EditorError, EditorParent, EditorResizeHandler, EditorSession,
    EditorSize, EffectProcessor, EffectRuntimeContext, EffectRuntimeSpec, InstrumentProcessor,
    InstrumentRuntimeContext, InstrumentRuntimeSpec, MidiEvent, Processor, ProcessorDescriptor,
    ProcessorState, ProcessorStateError, RuntimeBinding, RuntimeFactoryError, SlotState, registry,
};
use raw_window_handle::{RawWindowHandle, XlibWindowHandle};
use serde::{Deserialize, Serialize};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

/// VST3 plugin format id used by the validator.
pub const FORMAT: &str = "vst3";

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";
const EDITOR_VIEW_NAME: &[u8] = b"editor\0";
const DEFAULT_VST3_EDITOR_DESCRIPTOR: lilypalooza_audio::EditorDescriptor =
    lilypalooza_audio::EditorDescriptor {
        default_size: EditorSize {
            width: 640,
            height: 480,
        },
        min_size: None,
        resizable: true,
    };

/// Validated VST3 plugin metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vst3PluginMetadata {
    /// Stable processor id used by Lilypalooza.
    pub processor_id: String,
    /// VST3 processor class id as lowercase hex.
    pub class_id: String,
    /// User-visible plugin name.
    pub name: String,
    /// User-visible vendor, when reported by the plugin.
    pub vendor: Option<String>,
    /// User-visible version, when reported by the plugin.
    pub version: Option<String>,
    /// Raw VST3 subcategory string.
    pub category: Option<String>,
    /// Mixer processor role inferred from VST3 subcategories.
    pub role: registry::Role,
    /// Candidate path, usually the `.vst3` bundle or file.
    pub path: PathBuf,
    /// Native dynamic library path inside the candidate.
    pub library_path: PathBuf,
}

/// Structured validation report emitted by the validator helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Plugin format.
    pub format: String,
    /// Candidate path.
    pub path: PathBuf,
    /// Validation result.
    pub result: Result<Vec<Vst3PluginMetadata>, String>,
}

/// VST3 probing error.
#[derive(thiserror::Error, Debug)]
pub enum Vst3ProbeError {
    /// Candidate is not a VST3 file or bundle.
    #[error("not a VST3 candidate: {0}")]
    NotCandidate(PathBuf),
    /// VST3 binary path could not be resolved.
    #[error("VST3 binary not found for {0}")]
    MissingLibrary(PathBuf),
    /// Dynamic library could not be loaded.
    #[error("failed to load VST3 library {path}: {error}")]
    LoadLibrary {
        /// Library path.
        path: PathBuf,
        /// Loader error.
        error: libloading::Error,
    },
    /// macOS bundle object could not be created.
    #[error("failed to create VST3 bundle for {0}")]
    MissingBundle(PathBuf),
    /// macOS bundle executable could not be loaded.
    #[error("failed to load VST3 bundle executable for {0}")]
    BundleLoadFailed(PathBuf),
    /// Required exported symbol is missing.
    #[error("missing VST3 export `{0}`")]
    MissingExport(&'static str),
    /// Plugin factory export returned null.
    #[error("VST3 factory export returned null")]
    MissingFactory,
    /// Factory exposed no VST3 audio processor classes.
    #[error("no VST3 audio processor classes found")]
    NoPluginClasses,
}

#[derive(thiserror::Error, Debug)]
enum Vst3RuntimeError {
    #[error("VST3 plugin `{0}` is not registered")]
    MissingMetadata(String),
    #[error("invalid VST3 class id `{0}`")]
    InvalidClassId(String),
    #[error("VST3 factory failed to create processor")]
    CreateProcessorFailed,
    #[error("VST3 processor does not implement IAudioProcessor")]
    MissingAudioProcessor,
    #[error("VST3 processor initialize failed")]
    InitializeFailed,
    #[error("VST3 processor setup failed")]
    SetupFailed,
    #[error("VST3 processor activation failed")]
    ActivateFailed,
    #[error(transparent)]
    Probe(#[from] Vst3ProbeError),
}

/// Returns whether `path` looks like a VST3 candidate.
#[must_use]
pub fn is_vst3_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vst3"))
}

/// Recursively returns VST3 candidates under `root`.
pub fn candidate_paths(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_candidate_paths(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_candidate_paths(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if is_vst3_candidate(path) {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if !path.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        collect_candidate_paths(&entry?.path(), out)?;
    }
    Ok(())
}

/// Resolves the dynamic library path for a VST3 candidate.
#[must_use]
pub fn resolve_vst3_library_path(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let candidates = platform_vst3_binary_candidates(path, stem);
    candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

fn platform_vst3_binary_candidates(path: &Path, stem: &str) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![path.join("Contents").join("MacOS").join(stem)]
    }
    #[cfg(target_os = "windows")]
    {
        vec![
            path.join("Contents")
                .join("x86_64-win")
                .join(format!("{stem}.vst3")),
            path.join(format!("{stem}.vst3")),
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            path.join("Contents")
                .join("x86_64-linux")
                .join(format!("{stem}.so")),
            path.join(format!("{stem}.so")),
        ]
    }
}

/// Builds a stable Lilypalooza processor id from a VST3 path and class id.
#[must_use]
pub fn stable_processor_id(path: &Path, class_id: &str) -> String {
    format!("vst3:{}#{class_id}", path.display())
}

/// Probes a VST3 candidate in-process.
pub fn probe(path: &Path) -> Result<Vec<Vst3PluginMetadata>, Vst3ProbeError> {
    if !is_vst3_candidate(path) {
        return Err(Vst3ProbeError::NotCandidate(path.to_path_buf()));
    }
    let library_path = resolve_vst3_library_path(path);
    if !library_path.exists() {
        return Err(Vst3ProbeError::MissingLibrary(path.to_path_buf()));
    }
    let module = load_module(path, &library_path)?;
    let plugins = vst3_factory_plugins(&module.factory, path, &library_path);
    if plugins.is_empty() {
        Err(Vst3ProbeError::NoPluginClasses)
    } else {
        Ok(plugins)
    }
}

fn vst3_factory_plugins(
    factory: &ComPtr<IPluginFactory>,
    path: &Path,
    library_path: &Path,
) -> Vec<Vst3PluginMetadata> {
    // SAFETY: `factory` is a live COM factory kept alive by `LoadedModule`.
    let count = unsafe { factory.countClasses() }.max(0);
    let factory_vendor = vst3_factory_vendor(factory);
    (0..count)
        .filter_map(|index| {
            vst3_class_metadata(
                factory,
                index,
                path,
                library_path,
                factory_vendor.as_deref(),
            )
        })
        .collect()
}

fn vst3_factory_vendor(factory: &ComPtr<IPluginFactory>) -> Option<String> {
    let mut info = zeroed::<PFactoryInfo>();
    // SAFETY: Factory fills the provided `PFactoryInfo`.
    if unsafe { factory.getFactoryInfo(&mut info) } != kResultOk {
        return None;
    }
    non_empty_string(c_char_array_to_string(&info.vendor))
}

fn vst3_class_metadata(
    factory: &ComPtr<IPluginFactory>,
    index: i32,
    path: &Path,
    library_path: &Path,
    factory_vendor: Option<&str>,
) -> Option<Vst3PluginMetadata> {
    if let Some(factory2) = factory.cast::<IPluginFactory2>() {
        let mut info = zeroed::<PClassInfo2>();
        // SAFETY: Factory fills the provided `PClassInfo2` for a valid class index.
        if unsafe { factory2.getClassInfo2(index, &mut info) } == kResultOk
            && c_char_array_to_string(&info.category) == AUDIO_MODULE_CLASS
        {
            return Some(metadata_from_class_info2(
                info,
                path,
                library_path,
                factory_vendor,
            ));
        }
    }

    let mut info = zeroed::<PClassInfo>();
    // SAFETY: Factory fills the provided `PClassInfo` for a valid class index.
    if unsafe { factory.getClassInfo(index, &mut info) } != kResultOk
        || c_char_array_to_string(&info.category) != AUDIO_MODULE_CLASS
    {
        return None;
    }
    Some(metadata_from_class_info(
        info,
        path,
        library_path,
        factory_vendor,
    ))
}

fn metadata_from_class_info2(
    info: PClassInfo2,
    path: &Path,
    library_path: &Path,
    factory_vendor: Option<&str>,
) -> Vst3PluginMetadata {
    let class_id = tuid_to_hex(&info.cid);
    let category = non_empty_string(c_char_array_to_string(&info.subCategories));
    Vst3PluginMetadata {
        processor_id: stable_processor_id(path, &class_id),
        class_id,
        name: c_char_array_to_string(&info.name),
        vendor: metadata_vendor(
            non_empty_string(c_char_array_to_string(&info.vendor)),
            factory_vendor,
        ),
        version: non_empty_string(c_char_array_to_string(&info.version)),
        role: role_from_subcategories(category.as_deref()),
        category,
        path: path.to_path_buf(),
        library_path: library_path.to_path_buf(),
    }
}

fn metadata_from_class_info(
    info: PClassInfo,
    path: &Path,
    library_path: &Path,
    factory_vendor: Option<&str>,
) -> Vst3PluginMetadata {
    let class_id = tuid_to_hex(&info.cid);
    Vst3PluginMetadata {
        processor_id: stable_processor_id(path, &class_id),
        class_id,
        name: c_char_array_to_string(&info.name),
        vendor: metadata_vendor(None, factory_vendor),
        version: None,
        category: None,
        role: registry::Role::Effect,
        path: path.to_path_buf(),
        library_path: library_path.to_path_buf(),
    }
}

fn metadata_vendor(class_vendor: Option<String>, factory_vendor: Option<&str>) -> Option<String> {
    class_vendor.or_else(|| factory_vendor.map(str::to_owned))
}

fn role_from_subcategories(category: Option<&str>) -> registry::Role {
    let category = category.unwrap_or_default().to_ascii_lowercase();
    if category.contains("instrument")
        || category.contains("synth")
        || category.contains("sampler")
        || category.contains("drum")
    {
        registry::Role::Instrument
    } else {
        registry::Role::Effect
    }
}

fn non_empty_string(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn c_char_array_to_string(bytes: &[c_char]) -> String {
    let len = bytes
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(bytes.len());
    let bytes = bytes[..len]
        .iter()
        .map(|value| *value as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn copy_tchar_string(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn tuid_to_hex(tuid: &TUID) -> String {
    tuid.iter()
        .map(|byte| format!("{:02x}", *byte as u8))
        .collect()
}

fn hex_to_tuid(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0 as c_char; 16];
    for index in 0..16 {
        let byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
        out[index] = byte as c_char;
    }
    Some(out)
}

fn zeroed<T>() -> T {
    // SAFETY: VST3 POD structs are C structs where zero-initialization is valid before filling.
    unsafe { std::mem::zeroed() }
}

struct LoadedModule {
    factory: ComPtr<IPluginFactory>,
    #[cfg(target_os = "macos")]
    _bundle: core_foundation::bundle::CFBundle,
    #[cfg(not(target_os = "macos"))]
    _library: libloading::Library,
}

// SAFETY: The library and COM factory remain loaded for process lifetime through `LOADED_MODULES`.
unsafe impl Send for LoadedModule {}
// SAFETY: Access to plugin instances created from the module is synchronized by runtime mutexes.
unsafe impl Sync for LoadedModule {}

type GetPluginFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
#[cfg(target_os = "macos")]
type BundleEntry = unsafe extern "system" fn(*mut c_void) -> bool;
#[cfg(target_os = "windows")]
type InitDll = unsafe extern "system" fn() -> bool;
#[cfg(all(unix, not(target_os = "macos")))]
type ModuleEntry = unsafe extern "system" fn(*mut c_void) -> bool;

const GET_PLUGIN_FACTORY_SYMBOL: &str = "GetPluginFactory";
#[cfg(target_os = "macos")]
const BUNDLE_ENTRY_SYMBOL: &str = "bundleEntry";

fn loaded_modules() -> &'static RwLock<HashMap<PathBuf, Arc<LoadedModule>>> {
    static LOADED_MODULES: OnceLock<RwLock<HashMap<PathBuf, Arc<LoadedModule>>>> = OnceLock::new();
    LOADED_MODULES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn load_module(path: &Path, library_path: &Path) -> Result<Arc<LoadedModule>, Vst3ProbeError> {
    if let Some(module) = loaded_modules()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(library_path)
        .cloned()
    {
        return Ok(module);
    }
    let module = Arc::new(load_module_uncached(path, library_path)?);
    loaded_modules()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(library_path.to_path_buf(), module.clone());
    Ok(module)
}

fn load_module_uncached(path: &Path, _library_path: &Path) -> Result<LoadedModule, Vst3ProbeError> {
    #[cfg(target_os = "macos")]
    {
        load_macos_module(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        load_dynamic_module(path, _library_path)
    }
}

#[cfg(not(target_os = "macos"))]
fn load_dynamic_module(_path: &Path, library_path: &Path) -> Result<LoadedModule, Vst3ProbeError> {
    // SAFETY: Loading a plugin library is isolated by the validator subprocess during scanning.
    let library = unsafe { libloading::Library::new(library_path) }.map_err(|error| {
        Vst3ProbeError::LoadLibrary {
            path: library_path.to_path_buf(),
            error,
        }
    })?;

    #[cfg(target_os = "windows")]
    call_windows_init(&library)?;
    #[cfg(all(unix, not(target_os = "macos")))]
    call_module_entry(&library)?;

    // SAFETY: Symbol name is NUL-terminated and expected by VST3.
    let get_factory = unsafe { library.get::<GetPluginFactory>(b"GetPluginFactory\0") }
        .map_err(|_| Vst3ProbeError::MissingExport("GetPluginFactory"))?;
    // SAFETY: Factory export is called after module initialization.
    let factory = unsafe { get_factory() };
    // SAFETY: `GetPluginFactory` returns an owning COM pointer when non-null.
    let factory = unsafe { ComPtr::from_raw(factory) }.ok_or(Vst3ProbeError::MissingFactory)?;
    Ok(LoadedModule {
        factory,
        _library: library,
    })
}

#[cfg(target_os = "macos")]
fn load_macos_module(path: &Path) -> Result<LoadedModule, Vst3ProbeError> {
    use core_foundation::base::TCFType;
    use core_foundation::bundle::CFBundle;
    use core_foundation::bundle::CFBundleLoadExecutable;
    use core_foundation::string::CFString;
    use core_foundation::url::{CFURL, kCFURLPOSIXPathStyle};

    let bundle = CFBundle::new(CFURL::from_file_system_path(
        CFString::new(&path.display().to_string()),
        kCFURLPOSIXPathStyle,
        true,
    ))
    .ok_or_else(|| Vst3ProbeError::MissingBundle(path.to_path_buf()))?;

    // SAFETY: VST3 macOS modules must be loaded via CFBundleLoadExecutable before bundleEntry.
    if unsafe { CFBundleLoadExecutable(bundle.as_concrete_TypeRef()) } == 0 {
        return Err(Vst3ProbeError::BundleLoadFailed(path.to_path_buf()));
    }

    let entry = macos_bundle_symbol::<BundleEntry>(&bundle, BUNDLE_ENTRY_SYMBOL)
        .ok_or(Vst3ProbeError::MissingExport(BUNDLE_ENTRY_SYMBOL))?;
    let bundle_ref = bundle.as_concrete_TypeRef() as *mut c_void;
    // SAFETY: bundleEntry is the required VST3 macOS module initializer.
    if !unsafe { entry(bundle_ref) } {
        return Err(Vst3ProbeError::MissingExport(BUNDLE_ENTRY_SYMBOL));
    }

    let get_factory = macos_bundle_symbol::<GetPluginFactory>(&bundle, GET_PLUGIN_FACTORY_SYMBOL)
        .ok_or(Vst3ProbeError::MissingExport(GET_PLUGIN_FACTORY_SYMBOL))?;
    // SAFETY: Factory export is called after module initialization.
    let factory = unsafe { get_factory() };
    // SAFETY: `GetPluginFactory` returns an owning COM pointer when non-null.
    let factory = unsafe { ComPtr::from_raw(factory) }.ok_or(Vst3ProbeError::MissingFactory)?;
    Ok(LoadedModule {
        factory,
        _bundle: bundle,
    })
}

#[cfg(target_os = "macos")]
fn macos_bundle_symbol<T>(bundle: &core_foundation::bundle::CFBundle, symbol: &str) -> Option<T>
where
    T: Copy,
{
    use core_foundation::string::CFString;

    let pointer = bundle.function_pointer_for_name(CFString::new(symbol));
    if pointer.is_null() {
        return None;
    }
    // SAFETY: The caller chooses `T` to match the named C symbol signature.
    Some(unsafe { std::mem::transmute_copy::<*const c_void, T>(&pointer) })
}

#[cfg(target_os = "windows")]
fn call_windows_init(library: &libloading::Library) -> Result<(), Vst3ProbeError> {
    // SAFETY: Symbol lookup uses a static NUL-terminated export name.
    let entry = unsafe { library.get::<InitDll>(b"InitDll\0") };
    if let Ok(entry) = entry {
        // SAFETY: InitDll is the optional VST3 module initializer on Windows.
        if !unsafe { entry() } {
            return Err(Vst3ProbeError::MissingExport("InitDll"));
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn call_module_entry(library: &libloading::Library) -> Result<(), Vst3ProbeError> {
    if let Ok(entry) = unsafe { library.get::<ModuleEntry>(b"ModuleEntry\0") } {
        if !unsafe { entry(std::ptr::null_mut()) } {
            return Err(Vst3ProbeError::MissingExport("ModuleEntry"));
        }
    }
    Ok(())
}

fn metadata_store() -> &'static RwLock<HashMap<String, Vst3PluginMetadata>> {
    static PLUGIN_METADATA: OnceLock<RwLock<HashMap<String, Vst3PluginMetadata>>> = OnceLock::new();
    PLUGIN_METADATA.get_or_init(|| RwLock::new(HashMap::new()))
}

fn plugin_metadata(id: &str) -> Result<Vst3PluginMetadata, Vst3RuntimeError> {
    metadata_store()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .cloned()
        .ok_or_else(|| Vst3RuntimeError::MissingMetadata(id.to_string()))
}

struct Vst3Host {
    requested_size: Mutex<Option<EditorSize>>,
    resize_handler: Mutex<Option<Arc<dyn EditorResizeHandler>>>,
}

impl Vst3Host {
    fn new() -> Self {
        Self {
            requested_size: Mutex::new(None),
            resize_handler: Mutex::new(None),
        }
    }

    fn take_requested_size(&self) -> Option<EditorSize> {
        let requested = self
            .requested_size
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        trace_vst3_editor(|| format!("host take_requested_size {requested:?}"));
        requested
    }

    fn set_resize_handler(&self, handler: Option<Arc<dyn EditorResizeHandler>>) {
        *self
            .resize_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handler;
    }

    fn resize_handler(&self) -> Option<Arc<dyn EditorResizeHandler>> {
        self.resize_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn store_requested_size(&self, size: EditorSize) {
        *self
            .requested_size
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(size);
    }
}

impl Class for Vst3Host {
    type Interfaces = (IHostApplication, IPlugFrame, IComponentHandler);
}

impl IHostApplicationTrait for Vst3Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: VST3 provides a writable String128 pointer or null.
        if let Some(name) = unsafe { name.as_mut() } {
            copy_tchar_string("Lilypalooza", name);
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `obj` is a writable out pointer provided by the plugin.
        unsafe {
            *obj = std::ptr::null_mut();
        }
        // SAFETY: VST3 passes TUID pointers for `cid`/`iid`.
        unsafe {
            if tuid_ptr_eq(cid, &IMessage_iid) {
                return create_host_message(iid, obj);
            }
            if tuid_ptr_eq(cid, &IAttributeList_iid) {
                return create_host_attribute_list(iid, obj);
            }
        }
        kNotImplemented
    }
}

unsafe fn create_host_message(iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
    // SAFETY: `iid` is supplied by VST3 and points to a TUID when non-null.
    if unsafe { !tuid_ptr_eq(iid, &IMessage_iid) && !tuid_ptr_eq(iid, &FUnknown_iid) } {
        return kNoInterface;
    }
    let Some(instance) = ComWrapper::new(Vst3Message::new()).to_com_ptr::<IMessage>() else {
        return kNoInterface;
    };
    // SAFETY: `obj` was checked by the caller and receives ownership of the COM pointer.
    unsafe {
        *obj = instance.into_raw().cast();
    }
    kResultOk
}

unsafe fn create_host_attribute_list(iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
    // SAFETY: `iid` is supplied by VST3 and points to a TUID when non-null.
    if unsafe { !tuid_ptr_eq(iid, &IAttributeList_iid) && !tuid_ptr_eq(iid, &FUnknown_iid) } {
        return kNoInterface;
    }
    let Some(instance) =
        ComWrapper::new(Vst3AttributeList::default()).to_com_ptr::<IAttributeList>()
    else {
        return kNoInterface;
    };
    // SAFETY: `obj` was checked by the caller and receives ownership of the COM pointer.
    unsafe {
        *obj = instance.into_raw().cast();
    }
    kResultOk
}

unsafe fn tuid_ptr_eq(value: *mut TUID, expected: &TUID) -> bool {
    // SAFETY: Caller guarantees `value` is either null or a valid TUID pointer.
    unsafe { value.as_ref().is_some_and(|value| value == expected) }
}

#[derive(Default)]
struct Vst3AttributeList {
    values: Mutex<HashMap<String, Vst3AttributeValue>>,
}

enum Vst3AttributeValue {
    Int(i64),
    Float(f64),
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

impl Class for Vst3AttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for Vst3AttributeList {
    unsafe fn setInt(&self, id: IAttrID, value: int64) -> tresult {
        self.set_attr(id, Vst3AttributeValue::Int(value))
    }

    unsafe fn getInt(&self, id: IAttrID, value: *mut int64) -> tresult {
        if value.is_null() {
            return kInvalidArgument;
        }
        match self.get_attr(id) {
            Some(Vst3AttributeValue::Int(stored)) => {
                // SAFETY: `value` is checked non-null above.
                unsafe {
                    *value = stored;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttrID, value: f64) -> tresult {
        self.set_attr(id, Vst3AttributeValue::Float(value))
    }

    unsafe fn getFloat(&self, id: IAttrID, value: *mut f64) -> tresult {
        if value.is_null() {
            return kInvalidArgument;
        }
        match self.get_attr(id) {
            Some(Vst3AttributeValue::Float(stored)) => {
                // SAFETY: `value` is checked non-null above.
                unsafe {
                    *value = stored;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut stored = Vec::new();
        let mut offset = 0;
        loop {
            // SAFETY: VST3 strings are null-terminated TChar arrays.
            let ch = unsafe { *string.add(offset) };
            stored.push(ch);
            if ch == 0 {
                break;
            }
            offset += 1;
        }
        self.set_attr(id, Vst3AttributeValue::String(stored))
    }

    unsafe fn getString(&self, id: IAttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let capacity = (size_in_bytes as usize) / std::mem::size_of::<TChar>();
        if capacity == 0 {
            return kInvalidArgument;
        }
        match self.get_attr(id) {
            Some(Vst3AttributeValue::String(stored)) => {
                let copy_len = stored.len().min(capacity);
                // SAFETY: `string` points to `capacity` TChar slots by VST3 contract.
                unsafe {
                    std::ptr::copy_nonoverlapping(stored.as_ptr(), string, copy_len);
                    *string.add(copy_len.saturating_sub(1)) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(&self, id: IAttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        if data.is_null() && size_in_bytes > 0 {
            return kInvalidArgument;
        }
        // SAFETY: VST3 provides `size_in_bytes` readable bytes when non-zero.
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) };
        self.set_attr(id, Vst3AttributeValue::Binary(bytes.to_vec()))
    }

    unsafe fn getBinary(
        &self,
        id: IAttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        match self.get_attr(id) {
            Some(Vst3AttributeValue::Binary(stored)) => {
                // SAFETY: Out pointers are checked non-null above.
                unsafe {
                    *data = stored.as_ptr().cast();
                    *size_in_bytes = stored.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

impl Vst3AttributeList {
    fn set_attr(&self, id: IAttrID, value: Vst3AttributeValue) -> tresult {
        let Some(id) = attr_id_to_string(id) else {
            return kInvalidArgument;
        };
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, value);
        kResultOk
    }

    fn get_attr(&self, id: IAttrID) -> Option<Vst3AttributeValue> {
        let id = attr_id_to_string(id)?;
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .map(Vst3AttributeValue::clone_value)
    }
}

impl Vst3AttributeValue {
    fn clone_value(&self) -> Self {
        match self {
            Self::Int(value) => Self::Int(*value),
            Self::Float(value) => Self::Float(*value),
            Self::String(value) => Self::String(value.clone()),
            Self::Binary(value) => Self::Binary(value.clone()),
        }
    }
}

fn attr_id_to_string(id: IAttrID) -> Option<String> {
    if id.is_null() {
        return None;
    }
    // SAFETY: VST3 AttrID is a null-terminated C string.
    Some(unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned())
}

struct Vst3Message {
    message_id: Mutex<Option<CString>>,
    attributes: ComPtr<IAttributeList>,
}

impl Vst3Message {
    fn new() -> Self {
        let attributes = ComWrapper::new(Vst3AttributeList::default())
            .to_com_ptr::<IAttributeList>()
            .expect("Vst3AttributeList exposes IAttributeList");
        Self {
            message_id: Mutex::new(None),
            attributes,
        }
    }
}

impl Class for Vst3Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Vst3Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.message_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or(std::ptr::null(), |id| id.as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let message_id = if id.is_null() {
            None
        } else {
            // SAFETY: VST3 message IDs are null-terminated C strings.
            Some(unsafe { CStr::from_ptr(id) }.to_owned())
        };
        *self
            .message_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = message_id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes.as_ptr()
    }
}

impl IPlugFrameTrait for Vst3Host {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        // SAFETY: VST3 provides a readable ViewRect pointer or null.
        let Some(rect) = (unsafe { new_size.as_ref() }) else {
            return kInvalidArgument;
        };
        if let Some(size) = editor_size_from_rect(*rect) {
            if let Some(handler) = self.resize_handler() {
                let accepted = match handler.resize_editor(size) {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        trace_vst3_editor(|| {
                            format!(
                                "IPlugFrame::resizeView live resize failed view={view:p} rect={} size={size:?}: {error}",
                                format_view_rect(*rect)
                            )
                        });
                        return kResultFalse;
                    }
                };
                // SAFETY: `view` is supplied by the plugin for this resize callback.
                let on_size_result = unsafe { call_plug_view_on_size(view, accepted) };
                if on_size_result != kResultOk {
                    trace_vst3_editor(|| {
                        format!(
                            "IPlugFrame::resizeView onSize failed view={view:p} accepted={accepted:?} result={on_size_result}"
                        )
                    });
                    return on_size_result;
                }
                self.store_requested_size(accepted);
                trace_vst3_editor(|| {
                    format!(
                        "IPlugFrame::resizeView applied live resize view={view:p} rect={} requested={size:?} accepted={accepted:?}",
                        format_view_rect(*rect)
                    )
                });
                return kResultOk;
            }
            self.store_requested_size(size);
            trace_vst3_editor(|| {
                format!(
                    "IPlugFrame::resizeView queued deferred resize view={view:p} rect={} size={size:?}",
                    format_view_rect(*rect)
                )
            });
            return kResultOk;
        } else {
            trace_vst3_editor(|| {
                format!(
                    "IPlugFrame::resizeView ignored invalid rect={}",
                    format_view_rect(*rect)
                )
            });
        }
        kResultFalse
    }
}

impl IComponentHandlerTrait for Vst3Host {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value_normalized: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

struct Vst3EventList {
    events: Vec<Event>,
}

impl Class for Vst3EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for Vst3EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.len() as int32
    }

    unsafe fn getEvent(&self, index: int32, event: *mut Event) -> tresult {
        // SAFETY: VST3 provides a writable Event pointer or null.
        let Some(out) = (unsafe { event.as_mut() }) else {
            return kInvalidArgument;
        };
        let Some(input) = self.events.get(index as usize) else {
            return kInvalidArgument;
        };
        *out = *input;
        kResultOk
    }

    unsafe fn addEvent(&self, _event: *mut Event) -> tresult {
        kResultOk
    }
}

struct EmptyParameterChanges;

impl Class for EmptyParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for EmptyParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        0
    }

    unsafe fn getParameterData(&self, _index: int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        _index: *mut int32,
    ) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

struct Vst3RuntimeInner {
    _module: Arc<LoadedModule>,
    host: ComWrapper<Vst3Host>,
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    component_connection: Option<ComPtr<IConnectionPoint>>,
    controller_connection: Option<ComPtr<IConnectionPoint>>,
    descriptor: &'static ProcessorDescriptor,
    controller_lifecycle: Option<ControllerLifecycle>,
    active: bool,
    processing: bool,
    destroyed: bool,
    process_trace_remaining: u8,
}

struct CreatedController {
    controller: ComPtr<IEditController>,
    lifecycle: ControllerLifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControllerLifecycle {
    ComponentIntegrated,
    Separate,
}

impl ControllerLifecycle {
    fn initializes_controller(self) -> bool {
        matches!(self, Self::Separate)
    }

    fn connects_component(self) -> bool {
        matches!(self, Self::Separate)
    }

    fn terminates_controller(self) -> bool {
        matches!(self, Self::Separate)
    }
}

// SAFETY: The runtime is always shared behind a `Mutex`; raw VST3 pointers are accessed only
// while holding that mutex and remain valid until the paired terminate calls.
unsafe impl Send for Vst3RuntimeInner {}

impl Vst3RuntimeInner {
    fn instantiate(
        metadata: &Vst3PluginMetadata,
        descriptor: &'static ProcessorDescriptor,
        sample_rate: usize,
        block_size: usize,
        role: registry::Role,
    ) -> Result<Self, Vst3RuntimeError> {
        trace_vst3(|| {
            format!(
                "instantiate start name={} role={role:?} path={}",
                metadata.name,
                metadata.path.display()
            )
        });
        let module = load_module(&metadata.path, &metadata.library_path)?;
        let class_id = hex_to_tuid(&metadata.class_id)
            .ok_or_else(|| Vst3RuntimeError::InvalidClassId(metadata.class_id.clone()))?;
        let host = ComWrapper::new(Vst3Host::new());
        let host_application = host
            .to_com_ptr::<IHostApplication>()
            .ok_or(Vst3RuntimeError::InitializeFailed)?;
        let mut component_raw = std::ptr::null_mut::<c_void>();
        trace_vst3(|| "instantiate createInstance IComponent start".to_string());
        // SAFETY: Factory is live and writes the requested processor COM pointer.
        unsafe {
            module.factory.createInstance(
                class_id.as_ptr(),
                IComponent_iid.as_ptr(),
                &mut component_raw,
            )
        };
        trace_vst3(|| "instantiate createInstance IComponent done".to_string());
        // SAFETY: Successful factory creation returns an owning IComponent pointer.
        let component = unsafe { ComPtr::from_raw(component_raw.cast::<IComponent>()) }
            .ok_or(Vst3RuntimeError::CreateProcessorFailed)?;
        trace_vst3(|| "instantiate component.initialize start".to_string());
        // SAFETY: Component is initialized once with a live host application object.
        if unsafe { component.initialize(host_application.as_ptr().cast()) } != kResultOk {
            return Err(Vst3RuntimeError::InitializeFailed);
        }
        trace_vst3(|| "instantiate component.initialize done".to_string());
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or(Vst3RuntimeError::MissingAudioProcessor)?;
        trace_vst3(|| "instantiate create_controller start".to_string());
        let created_controller = create_controller(&module.factory, &component, &host)?;
        trace_vst3(|| "instantiate create_controller done".to_string());
        let controller_lifecycle = created_controller.as_ref().map(|created| created.lifecycle);
        let controller = created_controller.map(|created| created.controller);
        let (component_connection, controller_connection) =
            connect_component_and_controller(&component, controller.as_ref(), controller_lifecycle);
        if let Some(controller) = &controller {
            sync_controller_component_state(&component, controller);
        }

        let mut runtime = Self {
            _module: module,
            host,
            component,
            processor,
            controller,
            component_connection,
            controller_connection,
            descriptor,
            controller_lifecycle,
            active: false,
            processing: false,
            destroyed: false,
            process_trace_remaining: 8,
        };
        runtime.configure_audio(sample_rate, block_size, role)?;
        trace_vst3(|| "instantiate done".to_string());
        Ok(runtime)
    }

    fn configure_audio(
        &mut self,
        sample_rate: usize,
        block_size: usize,
        role: registry::Role,
    ) -> Result<(), Vst3RuntimeError> {
        // SAFETY: Component/processor are initialized and bus setup uses stack-owned values.
        unsafe {
            trace_vst3(|| "configure_audio setBusArrangements start".to_string());
            let mut input = SpeakerArr::kStereo;
            let mut output = SpeakerArr::kStereo;
            let input_count = i32::from(role == registry::Role::Effect);
            let set_bus_arrangements_result =
                self.processor
                    .setBusArrangements(&mut input, input_count, &mut output, 1);
            trace_vst3(|| {
                format!(
                    "configure_audio setBusArrangements done result={set_bus_arrangements_result} input_count={input_count}"
                )
            });
            trace_vst3(|| "configure_audio activate buses start".to_string());
            activate_buses(
                &self.component,
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kInput as BusDirection,
            );
            activate_buses(
                &self.component,
                MediaTypes_::kAudio as MediaType,
                BusDirections_::kOutput as BusDirection,
            );
            activate_buses(
                &self.component,
                MediaTypes_::kEvent as MediaType,
                BusDirections_::kInput as BusDirection,
            );

            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as int32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
                maxSamplesPerBlock: block_size.max(1) as int32,
                sampleRate: sample_rate.max(1) as SampleRate,
            };
            trace_vst3(|| "configure_audio setupProcessing start".to_string());
            let setup_processing_result = self.processor.setupProcessing(&mut setup);
            trace_vst3(|| {
                format!("configure_audio setupProcessing done result={setup_processing_result}")
            });
            if setup_processing_result != kResultOk {
                return Err(Vst3RuntimeError::SetupFailed);
            }
            trace_vst3(|| "configure_audio setActive(1) start".to_string());
            let set_active_result = self.component.setActive(1);
            trace_vst3(|| format!("configure_audio setActive(1) done result={set_active_result}"));
            if set_active_result != kResultOk {
                return Err(Vst3RuntimeError::ActivateFailed);
            }
            self.active = true;
            trace_vst3(|| "configure_audio setProcessing(1) start".to_string());
            let set_processing_result = self.processor.setProcessing(1);
            trace_vst3(|| {
                format!("configure_audio setProcessing(1) done result={set_processing_result}")
            });
            if set_processing_result == kResultOk {
                self.processing = true;
            } else if set_processing_result == kNotImplemented {
                trace_vst3(|| {
                    "configure_audio setProcessing(1) not implemented; continuing active"
                        .to_string()
                });
            } else {
                return Err(Vst3RuntimeError::ActivateFailed);
            }
            trace_vst3(|| "configure_audio done".to_string());
        }
        Ok(())
    }

    fn process_block(
        &mut self,
        input_left: Option<&[f32]>,
        input_right: Option<&[f32]>,
        output_left: &mut [f32],
        output_right: &mut [f32],
        events: &[Event],
    ) -> bool {
        if self.destroyed || !self.processing {
            return false;
        }
        let frames = output_left.len().min(output_right.len());
        let mut input_left_buffer = input_left.map(|input| input.to_vec());
        let mut input_right_buffer = input_right.map(|input| input.to_vec());
        let in_left = input_left_buffer
            .as_mut()
            .map_or(std::ptr::null_mut(), Vec::as_mut_ptr);
        let in_right = input_right_buffer
            .as_mut()
            .map_or(std::ptr::null_mut(), Vec::as_mut_ptr);
        let mut input_channels = [in_left, in_right];
        let mut output_channels = [output_left.as_mut_ptr(), output_right.as_mut_ptr()];
        let mut inputs = [AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_channels.as_mut_ptr(),
            },
        }];
        let mut outputs = [AudioBusBuffers {
            numChannels: 2,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: output_channels.as_mut_ptr(),
            },
        }];
        let event_list = ComWrapper::new(Vst3EventList {
            events: events.to_vec(),
        });
        let empty_events = ComWrapper::new(Vst3EventList { events: Vec::new() });
        let input_events = event_list.to_com_ptr::<IEventList>();
        let output_events = empty_events.to_com_ptr::<IEventList>();
        let input_params = ComWrapper::new(EmptyParameterChanges);
        let output_params = ComWrapper::new(EmptyParameterChanges);
        let input_params = input_params.to_com_ptr::<IParameterChanges>();
        let output_params = output_params.to_com_ptr::<IParameterChanges>();
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: frames as int32,
            numInputs: i32::from(input_left.is_some()),
            numOutputs: 1,
            inputs: if input_left.is_some() {
                inputs.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            outputs: outputs.as_mut_ptr(),
            inputParameterChanges: input_params
                .as_ref()
                .map_or(std::ptr::null_mut(), ComPtr::as_ptr),
            outputParameterChanges: output_params
                .as_ref()
                .map_or(std::ptr::null_mut(), ComPtr::as_ptr),
            inputEvents: input_events
                .as_ref()
                .map_or(std::ptr::null_mut(), ComPtr::as_ptr),
            outputEvents: output_events
                .as_ref()
                .map_or(std::ptr::null_mut(), ComPtr::as_ptr),
            processContext: std::ptr::null_mut(),
        };
        if self.process_trace_remaining > 0 {
            trace_vst3(|| {
                format!(
                    "process start frames={frames} inputs={} events={}",
                    data.numInputs,
                    events.len()
                )
            });
        }
        // SAFETY: ProcessData points to buffers and COM lists that outlive this process call.
        let result = unsafe { self.processor.process(&mut data) == kResultOk };
        if self.process_trace_remaining > 0 {
            self.process_trace_remaining -= 1;
            trace_vst3(|| format!("process done result={result}"));
        }
        result
    }

    fn reset(&mut self) {}

    fn latency_samples(&self) -> u32 {
        // SAFETY: Processor is live while runtime is not destroyed.
        unsafe { self.processor.getLatencySamples() }
    }

    fn save_state(&mut self) -> Result<ProcessorState, ControllerError> {
        Ok(ProcessorState::default())
    }

    fn load_state(&mut self, _state: &ProcessorState) -> Result<(), ProcessorStateError> {
        Ok(())
    }

    fn prepare_destroy(&mut self) {
        if self.destroyed {
            return;
        }
        trace_vst3(|| "prepare_destroy start".to_string());
        // SAFETY: Lifecycle calls are paired with successful initialization/activation.
        unsafe {
            if self.processing {
                trace_vst3(|| "prepare_destroy setProcessing(0) start".to_string());
                let _ = self.processor.setProcessing(0);
            }
            self.processing = false;
            if self.active {
                trace_vst3(|| "prepare_destroy setActive(0) start".to_string());
                let _ = self.component.setActive(0);
            }
            self.active = false;
            if self
                .controller_lifecycle
                .is_some_and(ControllerLifecycle::connects_component)
                && let (Some(component), Some(controller)) =
                    (&self.component_connection, &self.controller_connection)
            {
                trace_vst3(|| "prepare_destroy disconnect start".to_string());
                let _ = component.disconnect(controller.as_ptr());
                let _ = controller.disconnect(component.as_ptr());
            }
            if self
                .controller_lifecycle
                .is_some_and(ControllerLifecycle::terminates_controller)
                && let Some(controller) = &self.controller
            {
                trace_vst3(|| "prepare_destroy controller.terminate start".to_string());
                let _ = controller.terminate();
            }
            trace_vst3(|| "prepare_destroy component.terminate start".to_string());
            let _ = self.component.terminate();
        }
        self.destroyed = true;
        trace_vst3(|| "prepare_destroy done".to_string());
    }
}

fn connect_component_and_controller(
    component: &ComPtr<IComponent>,
    controller: Option<&ComPtr<IEditController>>,
    controller_lifecycle: Option<ControllerLifecycle>,
) -> (
    Option<ComPtr<IConnectionPoint>>,
    Option<ComPtr<IConnectionPoint>>,
) {
    if !controller_lifecycle.is_some_and(ControllerLifecycle::connects_component) {
        return (None, None);
    }
    let component_connection = component.cast::<IConnectionPoint>();
    let controller_connection =
        controller.and_then(|controller| controller.cast::<IConnectionPoint>());
    if let (Some(component_connection), Some(controller_connection)) =
        (&component_connection, &controller_connection)
    {
        // SAFETY: Both connection points are live and owned by this runtime.
        unsafe {
            trace_vst3(|| "connect_component_and_controller component.connect start".to_string());
            let _ = component_connection.connect(controller_connection.as_ptr());
            trace_vst3(|| "connect_component_and_controller component.connect done".to_string());
            trace_vst3(|| "connect_component_and_controller controller.connect start".to_string());
            let _ = controller_connection.connect(component_connection.as_ptr());
            trace_vst3(|| "connect_component_and_controller controller.connect done".to_string());
        }
    }
    (component_connection, controller_connection)
}

fn sync_controller_component_state(
    component: &ComPtr<IComponent>,
    controller: &ComPtr<IEditController>,
) {
    let stream_wrapper = ComWrapper::new(Vst3MemoryStream::default());
    let Some(stream) = stream_wrapper.to_com_ptr::<IBStream>() else {
        return;
    };
    // SAFETY: Component/controller are initialized and the stream stays live for both calls.
    unsafe {
        trace_vst3(|| "sync_controller_component_state getState start".to_string());
        if component.getState(stream.as_ptr()) != kResultOk {
            trace_vst3(|| "sync_controller_component_state getState skipped".to_string());
            return;
        }
        stream_wrapper.rewind();
        trace_vst3(|| "sync_controller_component_state setComponentState start".to_string());
        let result = controller.setComponentState(stream.as_ptr());
        trace_vst3(|| {
            format!("sync_controller_component_state setComponentState done result={result}")
        });
    }
}

#[derive(Default)]
struct Vst3MemoryStream {
    state: Mutex<Vst3MemoryStreamState>,
}

#[derive(Default)]
struct Vst3MemoryStreamState {
    data: Vec<u8>,
    position: usize,
}

impl Vst3MemoryStream {
    fn rewind(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .position = 0;
    }
}

impl Class for Vst3MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Vst3MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> tresult {
        if num_bytes < 0 || buffer.is_null() {
            return kInvalidArgument;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = num_bytes as usize;
        let available = state.data.len().saturating_sub(state.position);
        let count = requested.min(available);
        if count > 0 {
            // SAFETY: `buffer` is non-null and writable for `count` bytes by IBStream contract.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    state.data.as_ptr().add(state.position),
                    buffer.cast::<u8>(),
                    count,
                );
            }
        }
        state.position += count;
        // SAFETY: VST3 may pass null when it does not need the byte count.
        if let Some(num_bytes_read) = unsafe { num_bytes_read.as_mut() } {
            *num_bytes_read = count as int32;
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> tresult {
        if num_bytes < 0 || buffer.is_null() {
            return kInvalidArgument;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = num_bytes as usize;
        let end = state.position.saturating_add(count);
        if end > state.data.len() {
            state.data.resize(end, 0);
        }
        // SAFETY: `buffer` is non-null and readable for `count` bytes by IBStream contract.
        let input = unsafe { slice::from_raw_parts(buffer.cast::<u8>(), count) };
        let position = state.position;
        state.data[position..end].copy_from_slice(input);
        state.position = end;
        // SAFETY: VST3 may pass null when it does not need the byte count.
        if let Some(num_bytes_written) = unsafe { num_bytes_written.as_mut() } {
            *num_bytes_written = count as int32;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let base = match mode {
            mode if mode == IBStream_::IStreamSeekMode_::kIBSeekSet as int32 => 0,
            mode if mode == IBStream_::IStreamSeekMode_::kIBSeekCur as int32 => {
                state.position as int64
            }
            mode if mode == IBStream_::IStreamSeekMode_::kIBSeekEnd as int32 => {
                state.data.len() as int64
            }
            _ => return kInvalidArgument,
        };
        let Some(next) = base.checked_add(pos) else {
            return kInvalidArgument;
        };
        if next < 0 {
            return kInvalidArgument;
        }
        state.position = next as usize;
        // SAFETY: VST3 may pass null when it does not need the new position.
        if let Some(result) = unsafe { result.as_mut() } {
            *result = next;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        let position = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .position as int64;
        // SAFETY: VST3 provides a writable position pointer or null.
        let Some(pos) = (unsafe { pos.as_mut() }) else {
            return kInvalidArgument;
        };
        *pos = position;
        kResultOk
    }
}

impl Drop for Vst3RuntimeInner {
    fn drop(&mut self) {
        self.prepare_destroy();
    }
}

fn create_controller(
    factory: &ComPtr<IPluginFactory>,
    component: &ComPtr<IComponent>,
    host: &ComWrapper<Vst3Host>,
) -> Result<Option<CreatedController>, Vst3RuntimeError> {
    if let Some(controller) = component.cast::<IEditController>() {
        trace_vst3(|| "create_controller component has IEditController".to_string());
        set_controller_component_handler(&controller, host)?;
        return Ok(Some(CreatedController {
            controller,
            lifecycle: ControllerLifecycle::ComponentIntegrated,
        }));
    }
    let mut controller_id = [0 as c_char; 16];
    trace_vst3(|| "create_controller getControllerClassId start".to_string());
    // SAFETY: Component writes the controller class id into the provided TUID.
    if unsafe { component.getControllerClassId(&mut controller_id) } != kResultOk {
        trace_vst3(|| "create_controller no controller class id".to_string());
        return Ok(None);
    }
    let mut controller_raw = std::ptr::null_mut::<c_void>();
    trace_vst3(|| "create_controller factory.createInstance IEditController start".to_string());
    // SAFETY: Factory is live and writes an optional controller COM pointer.
    unsafe {
        factory.createInstance(
            controller_id.as_ptr(),
            IEditController_iid.as_ptr(),
            &mut controller_raw,
        )
    };
    trace_vst3(|| "create_controller factory.createInstance IEditController done".to_string());
    // SAFETY: Successful controller creation returns an owning IEditController pointer.
    let Some(controller) = (unsafe { ComPtr::from_raw(controller_raw.cast::<IEditController>()) })
    else {
        trace_vst3(|| "create_controller factory returned null".to_string());
        return Ok(None);
    };
    let lifecycle = ControllerLifecycle::Separate;
    if lifecycle.initializes_controller() {
        initialize_controller(&controller, host)?;
    }
    Ok(Some(CreatedController {
        controller,
        lifecycle,
    }))
}

fn initialize_controller(
    controller: &ComPtr<IEditController>,
    host: &ComWrapper<Vst3Host>,
) -> Result<(), Vst3RuntimeError> {
    let host_application = host
        .to_com_ptr::<IHostApplication>()
        .ok_or(Vst3RuntimeError::InitializeFailed)?;
    trace_vst3(|| "initialize_controller initialize start".to_string());
    // SAFETY: Controller is initialized once with a live host application object.
    if unsafe { controller.initialize(host_application.as_ptr().cast()) } != kResultOk {
        return Err(Vst3RuntimeError::InitializeFailed);
    }
    trace_vst3(|| "initialize_controller initialize done".to_string());
    set_controller_component_handler(controller, host)?;
    Ok(())
}

fn set_controller_component_handler(
    controller: &ComPtr<IEditController>,
    host: &ComWrapper<Vst3Host>,
) -> Result<(), Vst3RuntimeError> {
    if let Some(handler) = host.to_com_ptr::<IComponentHandler>() {
        trace_vst3(|| "initialize_controller setComponentHandler start".to_string());
        // SAFETY: Component handler COM pointer is owned by the host wrapper and remains live.
        unsafe {
            let _ = controller.setComponentHandler(handler.as_ptr());
        }
        trace_vst3(|| "initialize_controller setComponentHandler done".to_string());
    }
    Ok(())
}

fn activate_buses(component: &ComPtr<IComponent>, media_type: MediaType, direction: BusDirection) {
    // SAFETY: Component is live and queried with VST3 media/direction constants.
    let count = unsafe { component.getBusCount(media_type, direction) }.max(0);
    trace_vst3(|| {
        format!("activate_buses media_type={media_type} direction={direction} count={count}")
    });
    for index in 0..count {
        // SAFETY: Bus indices are bounded by `getBusCount`.
        let result = unsafe { component.activateBus(media_type, direction, index, 1) };
        trace_vst3(|| {
            format!(
                "activate_buses media_type={media_type} direction={direction} index={index} result={result}"
            )
        });
    }
}

#[derive(Clone)]
struct Vst3Binding {
    shared: Arc<Mutex<Vst3RuntimeInner>>,
}

impl RuntimeBinding for Vst3Binding {
    fn controller(&self) -> Box<dyn Controller> {
        Box::new(Vst3Controller {
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

struct Vst3Controller {
    shared: Arc<Mutex<Vst3RuntimeInner>>,
}

impl Controller for Vst3Controller {
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
        let has_editor = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .controller
            .is_some();
        Ok(has_editor.then(|| {
            Box::new(Vst3EditorSession {
                shared: self.shared.clone(),
                view: None,
                current_size: None,
            }) as Box<dyn EditorSession>
        }))
    }
}

struct Vst3Processor {
    shared: Arc<Mutex<Vst3RuntimeInner>>,
    midi: Vst3MidiEventQueue,
}

struct Vst3MidiEventQueue {
    pending: Vec<Event>,
    active_notes: [[bool; 128]; 16],
}

impl Vst3MidiEventQueue {
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
                    .push(vst3_note_on_event(channel, note, velocity));
            }
            MidiEvent::NoteOn { channel, note, .. } => {
                self.active_notes[(channel & 0x0f) as usize][note as usize] = false;
                self.pending.push(vst3_note_off_event(channel, note, 0));
            }
            MidiEvent::NoteOff {
                channel,
                note,
                velocity,
            } => {
                self.active_notes[(channel & 0x0f) as usize][note as usize] = false;
                self.pending
                    .push(vst3_note_off_event(channel, note, velocity));
            }
            MidiEvent::AllNotesOff { channel } | MidiEvent::AllSoundOff { channel } => {
                self.push_active_note_offs(channel);
            }
            MidiEvent::PolyPressure {
                channel,
                note,
                pressure,
            } => self
                .pending
                .push(vst3_poly_pressure_event(channel, note, pressure)),
            _ => {}
        }
    }

    fn push_active_note_offs(&mut self, channel: u8) {
        let channel = channel & 0x0f;
        for note in 0..128 {
            if self.active_notes[channel as usize][note] {
                self.pending
                    .push(vst3_note_off_event(channel, note as u8, 0));
                self.active_notes[channel as usize][note] = false;
            }
        }
    }

    fn push_panic(&mut self) {
        for channel in 0..16_u8 {
            self.push_active_note_offs(channel);
        }
    }

    fn take(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.pending)
    }
}

impl Processor for Vst3Processor {
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
        Vst3Controller {
            shared: self.shared.clone(),
        }
        .create_editor_session()
    }
}

impl InstrumentProcessor for Vst3Processor {
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

impl EffectProcessor for Vst3Processor {
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

fn vst3_note_on_event(channel: u8, note: u8, velocity: u8) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: 0,
        ppqPosition: 0.0,
        flags: Event_::EventFlags_::kIsLive as u16,
        r#type: Event_::EventTypes_::kNoteOnEvent as u16,
        __field0: Event__type0 {
            noteOn: NoteOnEvent {
                channel: (channel & 0x0f) as int16,
                pitch: note as int16,
                tuning: 0.0,
                velocity: f32::from(velocity) / 127.0,
                length: 0,
                noteId: -1,
            },
        },
    }
}

fn vst3_note_off_event(channel: u8, note: u8, velocity: u8) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: 0,
        ppqPosition: 0.0,
        flags: Event_::EventFlags_::kIsLive as u16,
        r#type: Event_::EventTypes_::kNoteOffEvent as u16,
        __field0: Event__type0 {
            noteOff: NoteOffEvent {
                channel: (channel & 0x0f) as int16,
                pitch: note as int16,
                velocity: f32::from(velocity) / 127.0,
                noteId: -1,
                tuning: 0.0,
            },
        },
    }
}

fn vst3_poly_pressure_event(channel: u8, note: u8, pressure: u8) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: 0,
        ppqPosition: 0.0,
        flags: Event_::EventFlags_::kIsLive as u16,
        r#type: Event_::EventTypes_::kPolyPressureEvent as u16,
        __field0: Event__type0 {
            polyPressure: PolyPressureEvent {
                channel: (channel & 0x0f) as int16,
                pitch: note as int16,
                pressure: f32::from(pressure) / 127.0,
                noteId: -1,
            },
        },
    }
}

struct Vst3EditorSession {
    shared: Arc<Mutex<Vst3RuntimeInner>>,
    view: Option<ComPtr<IPlugView>>,
    current_size: Option<EditorSize>,
}

impl EditorSession for Vst3EditorSession {
    fn resizable(&mut self) -> Result<Option<bool>, EditorError> {
        let Some(view) = &self.view else {
            return Ok(None);
        };
        // SAFETY: View is live while attached to this editor session.
        let resizable = unsafe { view.canResize() == kResultOk };
        trace_vst3_editor(|| format!("session resizable={resizable}"));
        Ok(Some(resizable))
    }

    fn initial_size(&mut self) -> Result<Option<EditorSize>, EditorError> {
        trace_vst3_editor(|| format!("session initial_size {:?}", self.current_size));
        Ok(self.current_size)
    }

    fn requested_size(&mut self) -> Result<Option<EditorSize>, EditorError> {
        let requested = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host
            .take_requested_size();
        let changed = changed_editor_size_request(&mut self.current_size, requested);
        trace_vst3_editor(|| {
            format!(
                "session requested_size requested={requested:?} changed={changed:?} current={:?}",
                self.current_size
            )
        });
        Ok(changed)
    }

    fn set_resize_handler(
        &mut self,
        handler: Option<Arc<dyn EditorResizeHandler>>,
    ) -> Result<(), EditorError> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host
            .set_resize_handler(handler);
        Ok(())
    }

    fn attach(&mut self, parent: EditorParent) -> Result<(), EditorError> {
        let (parent, platform) = vst3_parent_for_parent(parent)?;
        let runtime = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let controller = runtime
            .controller
            .as_ref()
            .ok_or(EditorError::Unsupported)?;
        trace_vst3(|| "editor attach createView start".to_string());
        // SAFETY: Controller is live and returns an owning view pointer or null.
        let view = unsafe { controller.createView(EDITOR_VIEW_NAME.as_ptr().cast()) };
        trace_vst3(|| "editor attach createView done".to_string());
        // SAFETY: Non-null view pointer is owned by the caller.
        let view = unsafe { ComPtr::from_raw(view) }.ok_or(EditorError::Unsupported)?;
        let frame = runtime
            .host
            .to_com_ptr::<IPlugFrame>()
            .ok_or(EditorError::Unsupported)?;
        // SAFETY: View, frame, and parent handle are live for the editor window lifetime.
        unsafe {
            trace_vst3(|| "editor attach setFrame start".to_string());
            let _ = view.setFrame(frame.as_ptr());
            trace_vst3(|| "editor attach setFrame done".to_string());
            trace_vst3(|| "editor attach isPlatformTypeSupported start".to_string());
            if view.isPlatformTypeSupported(platform) != kResultOk {
                return Err(EditorError::Unsupported);
            }
            trace_vst3(|| "editor attach isPlatformTypeSupported done".to_string());
            trace_vst3(|| "editor attach attached start".to_string());
            if view.attached(parent, platform) != kResultOk {
                return Err(EditorError::Backend(
                    "VST3 editor attach failed".to_string(),
                ));
            }
            trace_vst3(|| "editor attach attached done".to_string());
        }
        self.current_size = vst3_view_size(&view);
        trace_vst3_editor(|| format!("session attach current_size={:?}", self.current_size));
        self.view = Some(view);
        Ok(())
    }

    fn detach(&mut self) -> Result<(), EditorError> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .host
            .set_resize_handler(None);
        if let Some(view) = self.view.take() {
            trace_vst3_editor(|| format!("session detach current_size={:?}", self.current_size));
            // SAFETY: View was attached by this session and can be detached once.
            unsafe {
                trace_vst3(|| "editor detach removed start".to_string());
                let _ = view.removed();
                trace_vst3(|| "editor detach removed done".to_string());
                trace_vst3(|| "editor detach setFrame(null) start".to_string());
                let _ = view.setFrame(std::ptr::null_mut());
                trace_vst3(|| "editor detach setFrame(null) done".to_string());
            }
        }
        self.current_size = None;
        Ok(())
    }

    fn set_visible(&mut self, _visible: bool) -> Result<(), EditorError> {
        Ok(())
    }

    fn resize(&mut self, size: EditorSize) -> Result<EditorSize, EditorError> {
        let Some(view) = &self.view else {
            return Ok(size);
        };
        let mut rect = rect_from_editor_size(size);
        // SAFETY: View is live and receives a stack-owned size rectangle.
        unsafe {
            trace_vst3(|| format!("editor resize canResize start requested={size:?}"));
            if view.canResize() == kResultOk {
                trace_vst3(|| "editor resize checkSizeConstraint start".to_string());
                let _ = view.checkSizeConstraint(&mut rect);
                trace_vst3(|| {
                    format!(
                        "editor resize onSize start constrained_rect={}",
                        format_view_rect(rect)
                    )
                });
                let _ = view.onSize(&mut rect);
                trace_vst3(|| "editor resize onSize done".to_string());
            }
        }
        let accepted = editor_size_from_rect(rect).unwrap_or(size);
        trace_vst3_editor(|| {
            format!(
                "session resize requested={size:?} accepted={accepted:?} rect={}",
                format_view_rect(rect)
            )
        });
        self.current_size = Some(accepted);
        Ok(accepted)
    }
}

fn trace_vst3_editor(message: impl FnOnce() -> String) {
    trace_vst3_prefixed("vst3-editor", message);
}

fn trace_vst3(message: impl FnOnce() -> String) {
    trace_vst3_prefixed("vst3", message);
}

fn trace_vst3_prefixed(prefix: &str, message: impl FnOnce() -> String) {
    log::trace!(
        target: "lilypalooza_vst3",
        "{prefix} thread={:?} {}",
        std::thread::current().id(),
        message()
    );
}

fn changed_editor_size_request(
    current_size: &mut Option<EditorSize>,
    requested: Option<EditorSize>,
) -> Option<EditorSize> {
    let requested = requested?;
    if *current_size == Some(requested) {
        return None;
    }
    *current_size = Some(requested);
    Some(requested)
}

impl Drop for Vst3EditorSession {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}

fn vst3_view_size(view: &ComPtr<IPlugView>) -> Option<EditorSize> {
    let mut rect = zeroed::<ViewRect>();
    // SAFETY: View is live and writes its current size into `rect`.
    if unsafe { view.getSize(&mut rect) } != kResultOk {
        return None;
    }
    editor_size_from_rect(rect)
}

fn editor_size_from_rect(rect: ViewRect) -> Option<EditorSize> {
    let width = rect.right.saturating_sub(rect.left) as u32;
    let height = rect.bottom.saturating_sub(rect.top) as u32;
    (width > 0 && height > 0).then_some(EditorSize { width, height })
}

fn rect_from_editor_size(size: EditorSize) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: size.width as int32,
        bottom: size.height as int32,
    }
}

unsafe fn call_plug_view_on_size(view: *mut IPlugView, size: EditorSize) -> tresult {
    if view.is_null() {
        return kInvalidArgument;
    }
    let mut rect = rect_from_editor_size(size);
    // SAFETY: `view` is supplied by the plugin to `IPlugFrame::resizeView`, and the VST3
    // resize sequence requires the host to call `IPlugView::onSize` after resizing the frame.
    unsafe { ((*(*view).vtbl).onSize)(view, &mut rect) }
}

fn format_view_rect(rect: ViewRect) -> String {
    format!(
        "left={} top={} right={} bottom={} w={} h={}",
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        rect.right - rect.left,
        rect.bottom - rect.top
    )
}

fn vst3_parent_for_parent(parent: EditorParent) -> Result<(*mut c_void, FIDString), EditorError> {
    match parent.window {
        RawWindowHandle::AppKit(handle) => {
            Ok((handle.ns_view.as_ptr().cast(), kPlatformTypeNSView))
        }
        RawWindowHandle::Win32(handle) => Ok((handle.hwnd.get() as *mut c_void, kPlatformTypeHWND)),
        RawWindowHandle::Xlib(XlibWindowHandle { window, .. }) => Ok((
            window as usize as *mut c_void,
            kPlatformTypeX11EmbedWindowID,
        )),
        RawWindowHandle::Xcb(handle) => Ok((
            handle.window.get() as usize as *mut c_void,
            kPlatformTypeX11EmbedWindowID,
        )),
        RawWindowHandle::Wayland(handle) => Ok((
            handle.surface.as_ptr().cast(),
            kPlatformTypeWaylandSurfaceID,
        )),
        other => Err(EditorError::HostUnavailable(format!(
            "unsupported VST3 editor parent: {other:?}"
        ))),
    }
}

/// Registers validated VST3 plugins in the shared audio registry.
pub fn register_plugins(plugins: impl IntoIterator<Item = Vst3PluginMetadata>) {
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

fn registry_entry_for_plugin(plugin: Vst3PluginMetadata) -> registry::Entry {
    let descriptor = Box::leak(Box::new(ProcessorDescriptor {
        name: Box::leak(plugin.name.clone().into_boxed_str()),
        params: &[],
        editor: Some(DEFAULT_VST3_EDITOR_DESCRIPTOR),
    }));
    match plugin.role {
        registry::Role::Instrument => registry::Entry::plugin_instrument(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Vst3,
            plugin.vendor,
            descriptor,
            create_vst3_instrument_runtime,
        ),
        registry::Role::Effect => registry::Entry::plugin_effect(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Vst3,
            plugin.vendor,
            descriptor,
            create_vst3_effect_runtime,
        ),
    }
}

fn create_vst3_instrument_runtime(
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
        registry::Role::Instrument,
        &slot.state,
    )?;
    Ok(Some(InstrumentRuntimeSpec {
        processor: Box::new(Vst3Processor {
            shared: shared.clone(),
            midi: Vst3MidiEventQueue::new(),
        }),
        binding: Box::new(Vst3Binding { shared }),
    }))
}

fn create_vst3_effect_runtime(
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
        registry::Role::Effect,
        &slot.state,
    )?;
    Ok(Some(EffectRuntimeSpec {
        processor: Box::new(Vst3Processor {
            shared: shared.clone(),
            midi: Vst3MidiEventQueue::new(),
        }),
        binding: Some(Box::new(Vst3Binding { shared })),
    }))
}

fn metadata_and_descriptor(
    slot: &SlotState,
) -> Result<Option<(Vst3PluginMetadata, &'static ProcessorDescriptor)>, RuntimeFactoryError> {
    let lilypalooza_audio::ProcessorKind::Plugin { plugin_id } = &slot.kind else {
        return Ok(None);
    };
    let metadata = plugin_metadata(plugin_id)
        .map_err(|error| RuntimeFactoryError::Backend(error.to_string()))?;
    let descriptor = registry::entry(plugin_id)
        .map(|entry| entry.descriptor)
        .ok_or_else(|| {
            RuntimeFactoryError::Backend(format!("VST3 plugin `{plugin_id}` is not registered"))
        })?;
    Ok(Some((metadata, descriptor)))
}

fn instantiate_shared(
    metadata: &Vst3PluginMetadata,
    descriptor: &'static ProcessorDescriptor,
    sample_rate: usize,
    block_size: usize,
    role: registry::Role,
    state: &ProcessorState,
) -> Result<Arc<Mutex<Vst3RuntimeInner>>, RuntimeFactoryError> {
    let mut runtime =
        Vst3RuntimeInner::instantiate(metadata, descriptor, sample_rate, block_size, role)
            .map_err(|error| RuntimeFactoryError::Backend(error.to_string()))?;
    runtime
        .load_state(state)
        .map_err(RuntimeFactoryError::State)?;
    Ok(Arc::new(Mutex::new(runtime)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilypalooza_audio::instrument::EditorResizeHandler;

    #[test]
    fn vst3_candidate_detection_is_extension_based() {
        assert!(is_vst3_candidate(Path::new("Plugin.vst3")));
        assert!(is_vst3_candidate(Path::new("Plugin.VST3")));
        assert!(!is_vst3_candidate(Path::new("Plugin.clap")));
    }

    #[test]
    fn host_application_creates_vst3_messages() {
        let host = Vst3Host::new();
        let mut cid = IMessage_iid;
        let mut iid = IMessage_iid;
        let mut obj = std::ptr::null_mut();

        // SAFETY: The test passes valid TUID pointers and a writable out pointer.
        let result = unsafe { host.createInstance(&mut cid, &mut iid, &mut obj) };

        assert_eq!(result, kResultOk);
        // SAFETY: `createInstance` returned ownership of an IMessage pointer.
        let message = unsafe { ComPtr::from_raw(obj.cast::<IMessage>()) }
            .expect("host should create IMessage");
        let id = CString::new("hello").expect("static string has no interior nul");
        // SAFETY: The message pointer is live and `id` is a valid null-terminated string.
        unsafe {
            message.setMessageID(id.as_ptr());
            assert_eq!(CStr::from_ptr(message.getMessageID()).to_bytes(), b"hello");
            assert!(!message.getAttributes().is_null());
        }
    }

    #[test]
    fn host_application_creates_attribute_lists() {
        let host = Vst3Host::new();
        let mut cid = IAttributeList_iid;
        let mut iid = IAttributeList_iid;
        let mut obj = std::ptr::null_mut();

        // SAFETY: The test passes valid TUID pointers and a writable out pointer.
        let result = unsafe { host.createInstance(&mut cid, &mut iid, &mut obj) };

        assert_eq!(result, kResultOk);
        // SAFETY: `createInstance` returned ownership of an IAttributeList pointer.
        let attributes = unsafe { ComPtr::from_raw(obj.cast::<IAttributeList>()) }
            .expect("host should create IAttributeList");
        let key = CString::new("value").expect("static string has no interior nul");
        let mut value = 0;
        // SAFETY: The attribute list pointer is live and `key` is null-terminated.
        unsafe {
            assert_eq!(attributes.setInt(key.as_ptr(), 42), kResultOk);
            assert_eq!(attributes.getInt(key.as_ptr(), &mut value), kResultOk);
        }
        assert_eq!(value, 42);
    }

    #[test]
    fn vst3_candidate_paths_recurse() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("Vendor").join("Plugin.vst3");
        std::fs::create_dir_all(&nested).expect("nested vst3 dir");
        std::fs::write(dir.path().join("Root.vst3"), "").expect("root vst3 file");
        std::fs::write(dir.path().join("Other.clap"), "").expect("clap file");

        let candidates = candidate_paths(dir.path()).expect("candidate scan");

        assert_eq!(candidates, vec![dir.path().join("Root.vst3"), nested]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_vst3_loader_uses_sdk_entry_symbol_names() {
        assert_eq!(GET_PLUGIN_FACTORY_SYMBOL, "GetPluginFactory");
        assert_eq!(BUNDLE_ENTRY_SYMBOL, "bundleEntry");
    }

    #[test]
    fn stable_processor_id_includes_path_and_class_id() {
        assert_eq!(
            stable_processor_id(Path::new("/Plug/Test.vst3"), "001122"),
            "vst3:/Plug/Test.vst3#001122"
        );
    }

    #[test]
    fn validation_report_serializes() {
        let report = ValidationReport {
            format: FORMAT.to_string(),
            path: PathBuf::from("/Plug/Test.vst3"),
            result: Ok(vec![Vst3PluginMetadata {
                processor_id: "vst3:/Plug/Test.vst3#00112233445566778899aabbccddeeff".to_string(),
                class_id: "00112233445566778899aabbccddeeff".to_string(),
                name: "Test".to_string(),
                vendor: Some("Vendor".to_string()),
                version: Some("1.0".to_string()),
                category: Some("Instrument|Synth".to_string()),
                role: registry::Role::Instrument,
                path: PathBuf::from("/Plug/Test.vst3"),
                library_path: PathBuf::from("/Plug/Test.vst3"),
            }]),
        };

        let json = serde_json::to_string(&report).expect("report json");

        assert!(json.contains("\"format\":\"vst3\""));
        assert!(json.contains("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn role_detection_treats_instrument_subcategories_as_instruments() {
        assert_eq!(
            role_from_subcategories(Some("Instrument|Synth")),
            registry::Role::Instrument
        );
        assert_eq!(
            role_from_subcategories(Some("Fx|Delay")),
            registry::Role::Effect
        );
    }

    #[test]
    fn legacy_class_info_uses_factory_vendor() {
        let mut info = zeroed::<PClassInfo>();
        write_c_char_array(&mut info.name, "Legacy Plugin");

        let metadata = metadata_from_class_info(
            info,
            Path::new("/Plug/Legacy.vst3"),
            Path::new("/Plug/Legacy.vst3"),
            Some("Factory Vendor"),
        );

        assert_eq!(metadata.name, "Legacy Plugin");
        assert_eq!(metadata.vendor.as_deref(), Some("Factory Vendor"));
    }

    #[test]
    fn class_info2_prefers_class_vendor_over_factory_vendor() {
        let mut info = zeroed::<PClassInfo2>();
        write_c_char_array(&mut info.vendor, "Class Vendor");

        let metadata = metadata_from_class_info2(
            info,
            Path::new("/Plug/ClassVendor.vst3"),
            Path::new("/Plug/ClassVendor.vst3"),
            Some("Factory Vendor"),
        );

        assert_eq!(metadata.vendor.as_deref(), Some("Class Vendor"));
    }

    #[test]
    fn class_info2_empty_vendor_uses_factory_vendor() {
        let info = zeroed::<PClassInfo2>();

        let metadata = metadata_from_class_info2(
            info,
            Path::new("/Plug/FactoryVendor.vst3"),
            Path::new("/Plug/FactoryVendor.vst3"),
            Some("Factory Vendor"),
        );

        assert_eq!(metadata.vendor.as_deref(), Some("Factory Vendor"));
    }

    #[test]
    fn component_integrated_controller_uses_component_lifecycle_only() {
        let lifecycle = ControllerLifecycle::ComponentIntegrated;

        assert!(!lifecycle.initializes_controller());
        assert!(!lifecycle.connects_component());
        assert!(!lifecycle.terminates_controller());
    }

    #[test]
    fn separate_controller_uses_separate_controller_lifecycle() {
        let lifecycle = ControllerLifecycle::Separate;

        assert!(lifecycle.initializes_controller());
        assert!(lifecycle.connects_component());
        assert!(lifecycle.terminates_controller());
    }

    #[test]
    fn tuid_hex_roundtrips() {
        let hex = "00112233445566778899aabbccddeeff";
        assert_eq!(tuid_to_hex(&hex_to_tuid(hex).expect("tuid")), hex);
    }

    fn write_c_char_array<const N: usize>(dst: &mut [c_char; N], value: &str) {
        for (dst, src) in dst.iter_mut().zip(value.bytes().chain(std::iter::once(0))) {
            *dst = src as c_char;
        }
    }

    #[test]
    fn registered_vst3_plugin_entry_exposes_editor_descriptor() {
        let plugin = Vst3PluginMetadata {
            processor_id: "vst3:/Plug/Editor.vst3#00112233445566778899aabbccddeeff".to_string(),
            class_id: "00112233445566778899aabbccddeeff".to_string(),
            name: "Editor".to_string(),
            vendor: Some("Vendor".to_string()),
            version: None,
            category: Some("Fx".to_string()),
            role: registry::Role::Effect,
            path: PathBuf::from("/Plug/Editor.vst3"),
            library_path: PathBuf::from("/Plug/Editor.vst3"),
        };

        register_plugins([plugin.clone()]);
        let entry = registry::entry(&plugin.processor_id).expect("registered plugin");

        assert_eq!(entry.backend, registry::Backend::Vst3);
        assert_eq!(entry.role, registry::Role::Effect);
        assert!(entry.descriptor.editor.is_some());
    }

    #[test]
    fn unchanged_vst3_editor_size_request_is_ignored() {
        let mut current = Some(EditorSize {
            width: 640,
            height: 480,
        });

        assert_eq!(
            changed_editor_size_request(
                &mut current,
                Some(EditorSize {
                    width: 640,
                    height: 480,
                })
            ),
            None
        );
        assert_eq!(
            changed_editor_size_request(
                &mut current,
                Some(EditorSize {
                    width: 800,
                    height: 600,
                })
            ),
            Some(EditorSize {
                width: 800,
                height: 600,
            })
        );
        assert_eq!(
            current,
            Some(EditorSize {
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn vst3_resize_view_without_live_handler_records_deferred_resize_request() {
        let host = Vst3Host::new();
        let mut rect = rect_from_editor_size(EditorSize {
            width: 800,
            height: 600,
        });

        // SAFETY: The test passes a live stack-owned ViewRect pointer to the host callback.
        let result = unsafe { host.resizeView(std::ptr::null_mut(), &raw mut rect) };

        assert_eq!(result, kResultOk);
        assert_eq!(
            host.take_requested_size(),
            Some(EditorSize {
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn vst3_resize_view_resizes_host_and_calls_on_size_synchronously() {
        let host = Vst3Host::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(TestResizeHandler {
            events: Arc::clone(&events),
            ..TestResizeHandler::default()
        });
        host.set_resize_handler(Some(handler.clone()));
        let view = ComWrapper::new(TestPlugView {
            events: Arc::clone(&events),
            ..TestPlugView::default()
        });
        let view_ptr = view.to_com_ptr::<IPlugView>().expect("test view");
        let mut rect = rect_from_editor_size(EditorSize {
            width: 800,
            height: 600,
        });

        // SAFETY: The test passes live COM view and ViewRect pointers to the host callback.
        let result = unsafe { host.resizeView(view_ptr.as_ptr(), &raw mut rect) };

        assert_eq!(result, kResultOk);
        assert_eq!(
            handler
                .requested
                .lock()
                .expect("requested sizes")
                .as_slice(),
            &[EditorSize {
                width: 800,
                height: 600,
            }]
        );
        assert_eq!(
            view.on_size.lock().expect("onSize calls").as_slice(),
            &[EditorSize {
                width: 800,
                height: 600,
            }]
        );
        assert_eq!(
            host.take_requested_size(),
            Some(EditorSize {
                width: 800,
                height: 600,
            })
        );
        assert_eq!(
            events.lock().expect("resize events").as_slice(),
            &["resize_editor", "onSize"]
        );
    }

    #[derive(Default)]
    struct TestResizeHandler {
        requested: Mutex<Vec<EditorSize>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl EditorResizeHandler for TestResizeHandler {
        fn resize_editor(&self, size: EditorSize) -> Result<EditorSize, EditorError> {
            self.events
                .lock()
                .expect("resize events")
                .push("resize_editor");
            self.requested.lock().expect("requested sizes").push(size);
            Ok(size)
        }
    }

    #[derive(Default)]
    struct TestPlugView {
        on_size: Mutex<Vec<EditorSize>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Class for TestPlugView {
        type Interfaces = (IPlugView,);
    }

    impl IPlugViewTrait for TestPlugView {
        unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
            kResultOk
        }

        unsafe fn attached(&self, _parent: *mut c_void, _type: FIDString) -> tresult {
            kResultOk
        }

        unsafe fn removed(&self) -> tresult {
            kResultOk
        }

        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultFalse
        }

        unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultFalse
        }

        unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultFalse
        }

        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            // SAFETY: VST3 supplies a writable ViewRect pointer or null.
            let Some(size) = (unsafe { size.as_mut() }) else {
                return kInvalidArgument;
            };
            *size = rect_from_editor_size(EditorSize {
                width: 640,
                height: 480,
            });
            kResultOk
        }

        unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
            // SAFETY: VST3 supplies a readable ViewRect pointer or null.
            let rect = unsafe { new_size.as_ref() };
            let Some(size) = rect.and_then(|rect| editor_size_from_rect(*rect)) else {
                return kInvalidArgument;
            };
            self.events.lock().expect("resize events").push("onSize");
            self.on_size.lock().expect("onSize calls").push(size);
            kResultOk
        }

        unsafe fn onFocus(&self, _state: TBool) -> tresult {
            kResultOk
        }

        unsafe fn setFrame(&self, _frame: *mut IPlugFrame) -> tresult {
            kResultOk
        }

        unsafe fn canResize(&self) -> tresult {
            kResultOk
        }

        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultOk
        }
    }
}
