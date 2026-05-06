//! Reusable background plugin scanner.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Maximum number of concurrent validator subprocesses.
pub const PLUGIN_VALIDATOR_CONCURRENCY: usize = 1;
/// Maximum scanner events the app should process in one UI update.
pub const PLUGIN_SCAN_UI_EVENT_BUDGET: usize = 16;

/// Plugin binary format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginFormat {
    /// CLAP plugin.
    Clap,
    /// VST3 plugin.
    Vst3,
}

/// One plugin search root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSearchPath {
    /// Plugin format to search for.
    pub format: PluginFormat,
    /// Filesystem search root.
    pub path: PathBuf,
    /// Whether this root participates in scans.
    pub enabled: bool,
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

/// Background scanner event.
#[derive(Debug)]
pub enum PluginScanEvent {
    /// Human-readable progress line.
    Log(String),
    /// Validated CLAP plugin metadata.
    ClapPlugins(Vec<lilypalooza_clap::ClapPluginMetadata>),
    /// Validated VST3 plugin metadata.
    Vst3Plugins(Vec<lilypalooza_vst3::Vst3PluginMetadata>),
    /// Scan completion.
    Finished {
        /// Scan summary.
        summary: PluginScanSummary,
        /// Updated cache.
        cache: PluginScanCache,
    },
}

/// Scan counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PluginScanSummary {
    /// Candidate binaries seen.
    pub candidates: usize,
    /// Valid plugin descriptors found.
    pub valid_plugins: usize,
    /// Invalid candidates.
    pub invalid_candidates: usize,
}

/// Background plugin scan state.
#[derive(Debug, Default)]
pub struct PluginScanState {
    receiver: Option<mpsc::Receiver<PluginScanEvent>>,
    active: bool,
}

impl PluginScanState {
    /// Starts a background scan.
    pub fn start(
        &mut self,
        search_paths: Vec<PluginSearchPath>,
        cache: PluginScanCache,
        validator: PathBuf,
    ) {
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.active = true;
        thread::spawn(move || scan_worker(search_paths, cache, validator, sender));
    }

    /// Returns whether a scan is currently running.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Drains pending scan events.
    pub fn drain_events(&mut self) -> Vec<PluginScanEvent> {
        self.drain_events_with_limit(usize::MAX)
    }

    /// Drains up to `limit` pending scan events.
    pub fn drain_events_with_limit(&mut self, limit: usize) -> Vec<PluginScanEvent> {
        let mut events = Vec::new();
        if limit == 0 {
            return events;
        }
        let Some(receiver) = &self.receiver else {
            return events;
        };
        for _ in 0..limit {
            match receiver.try_recv() {
                Ok(event) => {
                    if matches!(event, PluginScanEvent::Finished { .. }) {
                        self.active = false;
                    }
                    events.push(event);
                    if !self.active {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.active = false;
                    break;
                }
            }
        }
        if !self.active {
            self.receiver = None;
        }
        events
    }
}

/// Persistent scan cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginScanCache {
    entries: HashMap<PathBuf, CachedPluginCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedPluginCandidate {
    fingerprint: PluginCandidateFingerprint,
    #[serde(default)]
    validator_fingerprint: Option<PluginCandidateFingerprint>,
    valid: bool,
    #[serde(default)]
    clap_plugins: Vec<lilypalooza_clap::ClapPluginMetadata>,
    #[serde(default)]
    vst3_plugins: Vec<lilypalooza_vst3::Vst3PluginMetadata>,
}

impl PluginScanCache {
    /// Loads a scan cache from `path`.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => ron::from_str(&contents).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self::default(),
        }
    }

    /// Saves a scan cache to `path`.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let Some(parent) = path.parent() else {
            return Err(format!(
                "Plugin cache path has no parent: {}",
                path.display()
            ));
        };
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create plugin cache directory {}: {error}",
                parent.display()
            )
        })?;
        let contents = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::new())
            .map_err(|error| format!("Failed to serialize plugin cache: {error}"))?;
        fs::write(path, contents)
            .map_err(|error| format!("Failed to write plugin cache {}: {error}", path.display()))
    }

    /// Returns whether a candidate has changed since the cached scan.
    #[must_use]
    pub fn is_stale(&self, path: &Path, fingerprint: PluginCandidateFingerprint) -> bool {
        self.is_stale_for_validator(path, fingerprint, None)
    }

    /// Returns whether a candidate has changed since the cached scan for this validator.
    #[must_use]
    pub fn is_stale_for_validator(
        &self,
        path: &Path,
        fingerprint: PluginCandidateFingerprint,
        validator_fingerprint: Option<PluginCandidateFingerprint>,
    ) -> bool {
        self.entries.get(path).is_none_or(|entry| {
            entry.fingerprint != fingerprint || entry.validator_fingerprint != validator_fingerprint
        })
    }

    /// Stores a checked candidate.
    pub fn mark_checked(
        &mut self,
        path: PathBuf,
        fingerprint: PluginCandidateFingerprint,
        validator_fingerprint: Option<PluginCandidateFingerprint>,
        valid: bool,
        clap_plugins: Vec<lilypalooza_clap::ClapPluginMetadata>,
        vst3_plugins: Vec<lilypalooza_vst3::Vst3PluginMetadata>,
    ) {
        self.entries.insert(
            path,
            CachedPluginCandidate {
                fingerprint,
                validator_fingerprint,
                valid,
                clap_plugins,
                vst3_plugins,
            },
        );
    }

    fn cached_candidate(
        &self,
        path: &Path,
        fingerprint: PluginCandidateFingerprint,
    ) -> Option<CachedCandidateResult> {
        self.entries.get(path).and_then(|entry| {
            (entry.fingerprint == fingerprint).then(|| {
                if entry.valid && !entry.clap_plugins.is_empty() {
                    CachedCandidateResult::ValidClapPlugins(entry.clap_plugins.clone())
                } else if entry.valid && !entry.vst3_plugins.is_empty() {
                    CachedCandidateResult::ValidVst3Plugins(entry.vst3_plugins.clone())
                } else {
                    CachedCandidateResult::Invalid
                }
            })
        })
    }
}

enum CachedCandidateResult {
    ValidClapPlugins(Vec<lilypalooza_clap::ClapPluginMetadata>),
    ValidVst3Plugins(Vec<lilypalooza_vst3::Vst3PluginMetadata>),
    Invalid,
}

/// Candidate file fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCandidateFingerprint {
    modified_millis: u64,
    len: u64,
}

impl PluginCandidateFingerprint {
    /// Builds a fingerprint from filesystem metadata.
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let modified_millis = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        Ok(Self {
            modified_millis,
            len: metadata.len(),
        })
    }
}

fn scan_worker(
    search_paths: Vec<PluginSearchPath>,
    mut cache: PluginScanCache,
    validator: PathBuf,
    sender: mpsc::Sender<PluginScanEvent>,
) {
    let _ = sender.send(PluginScanEvent::Log(format!(
        "Scanning plugins with {PLUGIN_VALIDATOR_CONCURRENCY} validator process"
    )));
    let mut summary = PluginScanSummary::default();
    let validator_fingerprint = PluginCandidateFingerprint::from_path(&validator).ok();

    for root in search_paths.into_iter().filter(|path| path.enabled) {
        let candidates = match candidates_for_root(&root) {
            Ok(candidates) => candidates,
            Err(error) => {
                let _ = sender.send(PluginScanEvent::Log(format!(
                    "Plugin scan skipped {}: {error}",
                    root.path.display()
                )));
                continue;
            }
        };
        summary.candidates += candidates.len();

        for candidate in candidates {
            let fingerprint = match PluginCandidateFingerprint::from_path(&candidate) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    summary.invalid_candidates += 1;
                    let _ = sender.send(PluginScanEvent::Log(format!(
                        "Plugin scan skipped {}: {error}",
                        candidate.display()
                    )));
                    continue;
                }
            };
            if !cache.is_stale_for_validator(&candidate, fingerprint, validator_fingerprint) {
                match cache.cached_candidate(&candidate, fingerprint) {
                    Some(CachedCandidateResult::ValidClapPlugins(plugins)) => {
                        summary.valid_plugins += plugins.len();
                        let _ = sender.send(PluginScanEvent::ClapPlugins(plugins));
                    }
                    Some(CachedCandidateResult::ValidVst3Plugins(plugins)) => {
                        summary.valid_plugins += plugins.len();
                        let _ = sender.send(PluginScanEvent::Vst3Plugins(plugins));
                    }
                    Some(CachedCandidateResult::Invalid) | None => {
                        summary.invalid_candidates += 1;
                    }
                }
                continue;
            }
            match validate_candidate(root.format, &candidate, &validator) {
                Ok(ValidatedPlugins::Clap(plugins)) => {
                    if plugins.is_empty() {
                        if reuse_cached_valid_candidate(
                            &cache,
                            &candidate,
                            fingerprint,
                            &mut summary,
                            &sender,
                            "validator returned no CLAP plugins",
                        ) {
                            continue;
                        }
                        summary.invalid_candidates += 1;
                        cache.mark_checked(
                            candidate.clone(),
                            fingerprint,
                            validator_fingerprint,
                            false,
                            Vec::new(),
                            Vec::new(),
                        );
                        let _ = sender.send(PluginScanEvent::Log(format!(
                            "No CLAP plugins found in {}",
                            candidate.display()
                        )));
                        continue;
                    }
                    summary.valid_plugins += plugins.len();
                    cache.mark_checked(
                        candidate.clone(),
                        fingerprint,
                        validator_fingerprint,
                        true,
                        plugins.clone(),
                        Vec::new(),
                    );
                    let _ = sender.send(PluginScanEvent::Log(format!(
                        "Validated {} CLAP plugin(s) from {}",
                        plugins.len(),
                        candidate.display()
                    )));
                    let _ = sender.send(PluginScanEvent::ClapPlugins(plugins));
                }
                Ok(ValidatedPlugins::Vst3(plugins)) => {
                    if plugins.is_empty() {
                        if reuse_cached_valid_candidate(
                            &cache,
                            &candidate,
                            fingerprint,
                            &mut summary,
                            &sender,
                            "validator returned no VST3 plugins",
                        ) {
                            continue;
                        }
                        summary.invalid_candidates += 1;
                        cache.mark_checked(
                            candidate.clone(),
                            fingerprint,
                            validator_fingerprint,
                            false,
                            Vec::new(),
                            Vec::new(),
                        );
                        let _ = sender.send(PluginScanEvent::Log(format!(
                            "No VST3 plugins found in {}",
                            candidate.display()
                        )));
                        continue;
                    }
                    summary.valid_plugins += plugins.len();
                    cache.mark_checked(
                        candidate.clone(),
                        fingerprint,
                        validator_fingerprint,
                        true,
                        Vec::new(),
                        plugins.clone(),
                    );
                    let _ = sender.send(PluginScanEvent::Log(format!(
                        "Validated {} VST3 plugin(s) from {}",
                        plugins.len(),
                        candidate.display()
                    )));
                    let _ = sender.send(PluginScanEvent::Vst3Plugins(plugins));
                }
                Err(error) => {
                    if reuse_cached_valid_candidate(
                        &cache,
                        &candidate,
                        fingerprint,
                        &mut summary,
                        &sender,
                        &format!("validation failed: {error}"),
                    ) {
                        continue;
                    }
                    summary.invalid_candidates += 1;
                    cache.mark_checked(
                        candidate.clone(),
                        fingerprint,
                        validator_fingerprint,
                        false,
                        Vec::new(),
                        Vec::new(),
                    );
                    let _ = sender.send(PluginScanEvent::Log(format!(
                        "Invalid plugin {}: {error}",
                        candidate.display()
                    )));
                }
            }
        }
    }

    let _ = sender.send(PluginScanEvent::Log(format!(
        "Plugin scan finished: {} candidate(s), {} plugin(s), {} invalid",
        summary.candidates, summary.valid_plugins, summary.invalid_candidates
    )));
    let _ = sender.send(PluginScanEvent::Finished { summary, cache });
}

fn reuse_cached_valid_candidate(
    cache: &PluginScanCache,
    candidate: &Path,
    fingerprint: PluginCandidateFingerprint,
    summary: &mut PluginScanSummary,
    sender: &mpsc::Sender<PluginScanEvent>,
    reason: &str,
) -> bool {
    match cache.cached_candidate(candidate, fingerprint) {
        Some(CachedCandidateResult::ValidClapPlugins(plugins)) => {
            summary.valid_plugins += plugins.len();
            let _ = sender.send(PluginScanEvent::Log(format!(
                "Reusing cached CLAP plugin metadata for {} ({reason})",
                candidate.display()
            )));
            let _ = sender.send(PluginScanEvent::ClapPlugins(plugins));
            true
        }
        Some(CachedCandidateResult::ValidVst3Plugins(plugins)) => {
            summary.valid_plugins += plugins.len();
            let _ = sender.send(PluginScanEvent::Log(format!(
                "Reusing cached VST3 plugin metadata for {} ({reason})",
                candidate.display()
            )));
            let _ = sender.send(PluginScanEvent::Vst3Plugins(plugins));
            true
        }
        Some(CachedCandidateResult::Invalid) | None => false,
    }
}

/// Returns plugin candidates directly under one search root.
pub fn candidates_for_root(root: &PluginSearchPath) -> Result<Vec<PathBuf>, String> {
    match root.format {
        PluginFormat::Clap => {
            lilypalooza_clap::candidate_paths(&root.path).map_err(|error| error.to_string())
        }
        PluginFormat::Vst3 => {
            lilypalooza_vst3::candidate_paths(&root.path).map_err(|error| error.to_string())
        }
    }
}

enum ValidatedPlugins {
    Clap(Vec<lilypalooza_clap::ClapPluginMetadata>),
    Vst3(Vec<lilypalooza_vst3::Vst3PluginMetadata>),
}

fn validate_candidate(
    format: PluginFormat,
    path: &Path,
    validator: &Path,
) -> Result<ValidatedPlugins, String> {
    match format {
        PluginFormat::Clap => validate_clap_candidate(path, validator),
        PluginFormat::Vst3 => validate_vst3_candidate(path, validator),
    }
}

fn validate_vst3_candidate(path: &Path, validator: &Path) -> Result<ValidatedPlugins, String> {
    let output = Command::new(validator)
        .arg("--format")
        .arg(lilypalooza_vst3::FORMAT)
        .arg("--path")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to run validator {}: {error}", validator.display()))?;
    parse_vst3_validator_output(output.status.success(), &output.stdout, &output.stderr)
}

fn parse_vst3_validator_output(
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ValidatedPlugins, String> {
    let report = parse_validator_stdout::<lilypalooza_vst3::ValidationReport>(stdout);
    if !success {
        return match report {
            Ok(report) => report
                .result
                .map(ValidatedPlugins::Vst3)
                .map_err(|error| error.to_string()),
            Err(_) => Err(String::from_utf8_lossy(stderr).trim().to_string()),
        };
    }
    let report = report.map_err(|error| error.to_string())?;
    report
        .result
        .map(ValidatedPlugins::Vst3)
        .map_err(|error| error.to_string())
}

fn validate_clap_candidate(path: &Path, validator: &Path) -> Result<ValidatedPlugins, String> {
    let output = Command::new(validator)
        .arg("--format")
        .arg(lilypalooza_clap::FORMAT)
        .arg("--path")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to run validator {}: {error}", validator.display()))?;
    parse_clap_validator_output(output.status.success(), &output.stdout, &output.stderr)
}

fn parse_clap_validator_output(
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ValidatedPlugins, String> {
    let report = parse_validator_stdout::<lilypalooza_clap::ValidationReport>(stdout);
    if !success {
        return match report {
            Ok(report) => report
                .result
                .map(ValidatedPlugins::Clap)
                .map_err(|error| error.to_string()),
            Err(_) => Err(String::from_utf8_lossy(stderr).trim().to_string()),
        };
    }
    let report = report.map_err(|error| error.to_string())?;
    report
        .result
        .map(ValidatedPlugins::Clap)
        .map_err(|error| error.to_string())
}

fn parse_validator_stdout<T>(stdout: &[u8]) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned,
{
    if let Ok(report) = serde_json::from_slice(stdout) {
        return Ok(report);
    }

    let mut last_error = None;
    for (index, byte) in stdout.iter().enumerate() {
        if *byte != b'{' {
            continue;
        }
        let mut deserializer = serde_json::Deserializer::from_slice(&stdout[index..]);
        match T::deserialize(&mut deserializer) {
            Ok(report) => return Ok(report),
            Err(error) => last_error = Some(error),
        }
    }

    match last_error {
        Some(error) => Err(error),
        None => serde_json::from_slice(stdout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("lilypalooza-plugin-scan-")
            .tempdir()
            .expect("temp dir")
    }

    fn test_path(file: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = test_dir();
        let path = dir.path().join(file);
        (dir, path)
    }

    fn vst3_report_stdout(path: &Path, role: &str) -> Vec<u8> {
        serde_json::json!({
            "format": "vst3",
            "path": path,
            "result": {
                "Ok": [{
                    "processor_id": format!("vst3:{}#00112233445566778899aabbccddeeff", path.display()),
                    "class_id": "00112233445566778899aabbccddeeff",
                    "name": "Plugin",
                    "vendor": "Vendor",
                    "version": null,
                    "category": null,
                    "role": role,
                    "path": path,
                    "library_path": path,
                }]
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn cache_marks_changed_candidate_as_stale() {
        let (_dir, path) = test_path("test.clap");
        let mut cache = PluginScanCache::default();
        let old = PluginCandidateFingerprint {
            modified_millis: 10,
            len: 20,
        };
        let new = PluginCandidateFingerprint {
            modified_millis: 11,
            len: 20,
        };

        assert!(cache.is_stale(&path, old));
        cache.mark_checked(path.clone(), old, None, true, Vec::new(), Vec::new());
        assert!(!cache.is_stale(&path, old));
        assert!(cache.is_stale(&path, new));
    }

    #[test]
    fn cache_marks_changed_validator_as_stale() {
        let (_dir, path) = test_path("test.clap");
        let candidate = PluginCandidateFingerprint {
            modified_millis: 10,
            len: 20,
        };
        let old_validator = Some(PluginCandidateFingerprint {
            modified_millis: 1,
            len: 2,
        });
        let new_validator = Some(PluginCandidateFingerprint {
            modified_millis: 2,
            len: 2,
        });
        let mut cache = PluginScanCache::default();

        cache.mark_checked(
            path.clone(),
            candidate,
            old_validator,
            true,
            Vec::new(),
            Vec::new(),
        );

        assert!(!cache.is_stale_for_validator(&path, candidate, old_validator));
        assert!(cache.is_stale_for_validator(&path, candidate, new_validator));
    }

    #[test]
    fn clap_root_collects_only_clap_candidates() {
        let dir = test_dir();
        std::fs::write(dir.path().join("a.clap"), "").expect("clap file");
        std::fs::write(dir.path().join("b.vst3"), "").expect("vst3 file");
        let root = PluginSearchPath {
            format: PluginFormat::Clap,
            path: dir.path().to_path_buf(),
            enabled: true,
        };

        let candidates = candidates_for_root(&root).expect("scan root");

        assert_eq!(candidates, vec![dir.path().join("a.clap")]);
    }

    #[test]
    fn vst3_root_collects_vst3_candidates_recursively() {
        let dir = test_dir();
        std::fs::write(dir.path().join("a.clap"), "").expect("clap file");
        let nested = dir.path().join("Vendor").join("b.vst3");
        std::fs::create_dir_all(&nested).expect("vst3 bundle");
        let root = PluginSearchPath {
            format: PluginFormat::Vst3,
            path: dir.path().to_path_buf(),
            enabled: true,
        };

        let candidates = candidates_for_root(&root).expect("scan root");

        assert_eq!(candidates, vec![nested]);
    }

    #[test]
    fn cache_roundtrips_from_explicit_path() {
        let (_cache_dir, path) = test_path("plugin-cache.ron");
        let (_candidate_dir, candidate) = test_path("test.clap");
        let fingerprint = PluginCandidateFingerprint {
            modified_millis: 7,
            len: 9,
        };
        let mut cache = PluginScanCache::default();
        cache.mark_checked(
            candidate.clone(),
            fingerprint,
            None,
            true,
            Vec::new(),
            Vec::new(),
        );

        cache.save_to(&path).expect("cache should save");
        let loaded = PluginScanCache::load_from(&path);

        assert!(!loaded.is_stale(&candidate, fingerprint));
    }

    #[test]
    fn unchanged_valid_plugin_is_reused_when_revalidation_fails() {
        let (_dir, path) = test_path("plugin.vst3");
        let fingerprint = PluginCandidateFingerprint {
            modified_millis: 7,
            len: 9,
        };
        let stdout = vst3_report_stdout(&path, "instrument");
        let ValidatedPlugins::Vst3(plugins) =
            parse_vst3_validator_output(true, &stdout, b"").expect("valid plugin metadata")
        else {
            panic!("expected VST3 plugins");
        };
        let mut cache = PluginScanCache::default();
        cache.mark_checked(
            path.clone(),
            fingerprint,
            Some(PluginCandidateFingerprint {
                modified_millis: 1,
                len: 2,
            }),
            true,
            Vec::new(),
            plugins,
        );
        let (sender, receiver) = mpsc::channel();
        let mut summary = PluginScanSummary::default();

        assert!(reuse_cached_valid_candidate(
            &cache,
            &path,
            fingerprint,
            &mut summary,
            &sender,
            "validation failed"
        ));

        assert_eq!(summary.valid_plugins, 1);
        assert!(matches!(
            receiver.try_iter().last(),
            Some(PluginScanEvent::Vst3Plugins(plugins)) if plugins[0].name == "Plugin"
        ));
    }

    #[test]
    fn drain_events_with_limit_keeps_scan_active_when_budget_is_exhausted() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(PluginScanEvent::Log("one".to_string()))
            .expect("send one");
        sender
            .send(PluginScanEvent::Log("two".to_string()))
            .expect("send two");
        sender
            .send(PluginScanEvent::Log("three".to_string()))
            .expect("send three");
        let mut state = PluginScanState {
            receiver: Some(receiver),
            active: true,
        };

        let events = state.drain_events_with_limit(2);

        assert_eq!(events.len(), 2);
        assert!(state.is_active());
        assert_eq!(state.drain_events().len(), 1);
    }

    #[test]
    fn drain_events_with_limit_zero_does_not_drain() {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(PluginScanEvent::Log("one".to_string()))
            .expect("send one");
        let mut state = PluginScanState {
            receiver: Some(receiver),
            active: true,
        };

        assert!(state.drain_events_with_limit(0).is_empty());
        assert_eq!(state.drain_events().len(), 1);
    }

    #[test]
    fn empty_clap_validation_result_parses_as_empty_plugin_list() {
        let (_dir, path) = test_path("empty.clap");
        let stdout = serde_json::json!({
            "format": "clap",
            "path": path,
            "result": { "Ok": [] },
        })
        .to_string()
        .into_bytes();
        let plugins = parse_clap_validator_output(true, &stdout, b"")
            .expect("empty valid report should parse");

        match plugins {
            ValidatedPlugins::Clap(plugins) => assert!(plugins.is_empty()),
            ValidatedPlugins::Vst3(_) => panic!("expected CLAP plugins"),
        }
    }

    #[test]
    fn non_success_validator_with_valid_report_is_accepted() {
        let (_dir, path) = test_path("plugin.vst3");
        let stdout = vst3_report_stdout(&path, "effect");
        let plugins = parse_vst3_validator_output(false, &stdout, b"process exited non-zero")
            .expect("valid stdout should parse");

        match plugins {
            ValidatedPlugins::Vst3(plugins) => assert_eq!(plugins.len(), 1),
            ValidatedPlugins::Clap(_) => panic!("expected VST3 plugins"),
        }
    }

    #[test]
    fn validator_stdout_prefix_noise_is_ignored() {
        let (_dir, path) = test_path("plugin.vst3");
        let mut stdout = b"[info] initializing\n[info] ready\n".to_vec();
        stdout.extend(vst3_report_stdout(&path, "effect"));
        let plugins = parse_vst3_validator_output(true, &stdout, b"")
            .expect("valid report after log lines should parse");

        match plugins {
            ValidatedPlugins::Vst3(plugins) => assert_eq!(plugins[0].name, "Plugin"),
            ValidatedPlugins::Clap(_) => panic!("expected VST3 plugins"),
        }
    }
}
