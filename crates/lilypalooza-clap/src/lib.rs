//! CLAP adapter and probe helpers for Lilypalooza.

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};

use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::plugin::clap_plugin_descriptor;
use clap_sys::plugin_features::{
    CLAP_PLUGIN_FEATURE_AUDIO_EFFECT, CLAP_PLUGIN_FEATURE_DRUM, CLAP_PLUGIN_FEATURE_DRUM_MACHINE,
    CLAP_PLUGIN_FEATURE_INSTRUMENT, CLAP_PLUGIN_FEATURE_SAMPLER, CLAP_PLUGIN_FEATURE_SYNTHESIZER,
};
use clap_sys::version::clap_version_is_compatible;
use lilypalooza_audio::instrument::{
    EffectRuntimeSpec, InstrumentRuntimeContext, InstrumentRuntimeSpec, RuntimeFactoryError,
    registry,
};
use lilypalooza_audio::{ProcessorDescriptor, SlotState};
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

/// Finds CLAP candidates directly under one root.
pub fn candidate_paths(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut candidates = Vec::new();
    if !root.is_dir() {
        return Ok(candidates);
    }

    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if is_clap_candidate(&path) {
            candidates.push(path);
        }
    }
    candidates.sort();
    Ok(candidates)
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

/// Registers validated CLAP plugins in the shared audio registry.
pub fn register_plugins(plugins: impl IntoIterator<Item = ClapPluginMetadata>) {
    let entries = plugins.into_iter().map(registry_entry_for_plugin);
    registry::register(entries);
}

fn registry_entry_for_plugin(plugin: ClapPluginMetadata) -> registry::Entry {
    let descriptor = Box::leak(Box::new(ProcessorDescriptor {
        name: Box::leak(plugin.name.clone().into_boxed_str()),
        params: &[],
        editor: None,
    }));
    match plugin.role {
        registry::Role::Instrument => registry::Entry::plugin_instrument(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Clap,
            descriptor,
            create_clap_instrument_runtime,
        ),
        registry::Role::Effect => registry::Entry::plugin_effect(
            plugin.processor_id,
            plugin.name,
            registry::Backend::Clap,
            descriptor,
            create_clap_effect_runtime,
        ),
    }
}

fn create_clap_instrument_runtime(
    _slot: &SlotState,
    _context: &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
    Err(RuntimeFactoryError::Backend(
        "CLAP runtime instantiation is not implemented yet".to_string(),
    ))
}

fn create_clap_effect_runtime(
    _slot: &SlotState,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    Err(RuntimeFactoryError::Backend(
        "CLAP runtime instantiation is not implemented yet".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_candidate_detection_is_extension_based() {
        assert!(is_clap_candidate(Path::new("Plugin.clap")));
        assert!(is_clap_candidate(Path::new("Plugin.CLAP")));
        assert!(!is_clap_candidate(Path::new("Plugin.vst3")));
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
}
