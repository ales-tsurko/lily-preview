//! Reusable background plugin scanner.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

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
        self.entries
            .get(path)
            .is_none_or(|entry| entry.fingerprint != fingerprint)
    }

    /// Stores a checked candidate.
    pub fn mark_checked(
        &mut self,
        path: PathBuf,
        fingerprint: PluginCandidateFingerprint,
        valid: bool,
        clap_plugins: Vec<lilypalooza_clap::ClapPluginMetadata>,
        vst3_plugins: Vec<lilypalooza_vst3::Vst3PluginMetadata>,
    ) {
        self.entries.insert(
            path,
            CachedPluginCandidate {
                fingerprint,
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
            if !cache.is_stale(&candidate, fingerprint) {
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
                        summary.invalid_candidates += 1;
                        cache.mark_checked(
                            candidate.clone(),
                            fingerprint,
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
                        summary.invalid_candidates += 1;
                        cache.mark_checked(
                            candidate.clone(),
                            fingerprint,
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
                    summary.invalid_candidates += 1;
                    cache.mark_checked(
                        candidate.clone(),
                        fingerprint,
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
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    let report: lilypalooza_vst3::ValidationReport =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
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
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    let report: lilypalooza_clap::ValidationReport =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    report
        .result
        .map(ValidatedPlugins::Clap)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn cache_marks_changed_candidate_as_stale() {
        let path = PathBuf::from("/tmp/test.clap");
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
        cache.mark_checked(path.clone(), old, true, Vec::new(), Vec::new());
        assert!(!cache.is_stale(&path, old));
        assert!(cache.is_stale(&path, new));
    }

    #[test]
    fn clap_root_collects_only_clap_candidates() {
        let dir = tempfile::tempdir().expect("temp dir");
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
        let dir = tempfile::tempdir().expect("temp dir");
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
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("plugin-cache.ron");
        let candidate = PathBuf::from("/tmp/test.clap");
        let fingerprint = PluginCandidateFingerprint {
            modified_millis: 7,
            len: 9,
        };
        let mut cache = PluginScanCache::default();
        cache.mark_checked(candidate.clone(), fingerprint, true, Vec::new(), Vec::new());

        cache.save_to(&path).expect("cache should save");
        let loaded = PluginScanCache::load_from(&path);

        assert!(!loaded.is_stale(&candidate, fingerprint));
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
    fn empty_clap_validation_result_counts_as_invalid_candidate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let candidate = dir.path().join("empty.clap");
        std::fs::write(&candidate, "").expect("clap file");
        let validator = fake_empty_clap_validator();
        let mut state = PluginScanState::default();

        state.start(
            vec![PluginSearchPath {
                format: PluginFormat::Clap,
                path: dir.path().to_path_buf(),
                enabled: true,
            }],
            PluginScanCache::default(),
            validator,
        );

        let (summary, logs) = drain_scan_until_finished(&mut state);

        assert_eq!(
            summary,
            PluginScanSummary {
                candidates: 1,
                valid_plugins: 0,
                invalid_candidates: 1,
            }
        );
        assert!(logs.iter().any(|log| log.contains("No CLAP plugins found")));
    }

    fn drain_scan_until_finished(state: &mut PluginScanState) -> (PluginScanSummary, Vec<String>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut logs = Vec::new();
        while Instant::now() < deadline {
            for event in state.drain_events() {
                match event {
                    PluginScanEvent::Log(log) => logs.push(log),
                    PluginScanEvent::Finished { summary, .. } => return (summary, logs),
                    PluginScanEvent::ClapPlugins(_) | PluginScanEvent::Vst3Plugins(_) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("scan did not finish");
    }

    fn fake_empty_clap_validator() -> PathBuf {
        let dir = tempfile::tempdir().expect("validator temp dir").keep();
        let path = dir.join("validator");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '{\"format\":\"clap\",\"path\":\"%s\",\"result\":{\"Ok\":[]}}' \"$4\"\n",
        )
        .expect("validator script");
        make_executable(&path);
        path
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path)
            .expect("validator metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("validator permissions");
    }
}
