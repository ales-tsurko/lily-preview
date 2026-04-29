#![cfg_attr(test, allow(dead_code))]

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::settings::{PluginFormat, PluginSearchPath};

pub(crate) const PLUGIN_VALIDATOR_CONCURRENCY: usize = 1;

#[derive(Debug)]
pub(crate) enum PluginScanEvent {
    Log(String),
    ClapPlugins(Vec<lilypalooza_clap::ClapPluginMetadata>),
    Finished {
        summary: PluginScanSummary,
        cache: PluginScanCache,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PluginScanSummary {
    pub(crate) candidates: usize,
    pub(crate) valid_plugins: usize,
    pub(crate) invalid_candidates: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PluginScanState {
    receiver: Option<mpsc::Receiver<PluginScanEvent>>,
    active: bool,
}

impl PluginScanState {
    pub(crate) fn start(&mut self, search_paths: Vec<PluginSearchPath>, cache: PluginScanCache) {
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.active = true;
        thread::spawn(move || scan_worker(search_paths, cache, sender));
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn drain_events(&mut self) -> Vec<PluginScanEvent> {
        let mut events = Vec::new();
        let Some(receiver) = &self.receiver else {
            return events;
        };
        while let Ok(event) = receiver.try_recv() {
            if matches!(event, PluginScanEvent::Finished { .. }) {
                self.active = false;
            }
            events.push(event);
        }
        if !self.active {
            self.receiver = None;
        }
        events
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PluginScanCache {
    entries: HashMap<PathBuf, CachedPluginCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedPluginCandidate {
    fingerprint: PluginCandidateFingerprint,
    valid: bool,
    clap_plugins: Vec<lilypalooza_clap::ClapPluginMetadata>,
}

impl PluginScanCache {
    pub(crate) fn load() -> Self {
        let Ok(path) = cache_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(contents) => ron::from_str(&contents).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self::default(),
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let path = cache_path()?;
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
        fs::write(&path, contents)
            .map_err(|error| format!("Failed to write plugin cache {}: {error}", path.display()))
    }

    pub(crate) fn is_stale(&self, path: &Path, fingerprint: PluginCandidateFingerprint) -> bool {
        self.entries
            .get(path)
            .is_none_or(|entry| entry.fingerprint != fingerprint)
    }

    pub(crate) fn mark_checked(
        &mut self,
        path: PathBuf,
        fingerprint: PluginCandidateFingerprint,
        valid: bool,
        clap_plugins: Vec<lilypalooza_clap::ClapPluginMetadata>,
    ) {
        self.entries.insert(
            path,
            CachedPluginCandidate {
                fingerprint,
                valid,
                clap_plugins,
            },
        );
    }

    fn cached_clap_plugins(
        &self,
        path: &Path,
        fingerprint: PluginCandidateFingerprint,
    ) -> Option<Vec<lilypalooza_clap::ClapPluginMetadata>> {
        self.entries.get(path).and_then(|entry| {
            (entry.valid && entry.fingerprint == fingerprint).then(|| entry.clap_plugins.clone())
        })
    }
}

fn cache_path() -> Result<PathBuf, String> {
    let project_dirs = directories::ProjectDirs::from("", "", "lilypalooza")
        .ok_or_else(|| "Failed to resolve user config directory".to_string())?;
    Ok(project_dirs.config_dir().join("plugin-cache.ron"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PluginCandidateFingerprint {
    modified_millis: u128,
    len: u64,
}

impl PluginCandidateFingerprint {
    pub(crate) fn from_path(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let modified_millis = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Ok(Self {
            modified_millis,
            len: metadata.len(),
        })
    }
}

fn scan_worker(
    search_paths: Vec<PluginSearchPath>,
    mut cache: PluginScanCache,
    sender: mpsc::Sender<PluginScanEvent>,
) {
    let _ = sender.send(PluginScanEvent::Log(format!(
        "Scanning plugins with {PLUGIN_VALIDATOR_CONCURRENCY} validator process"
    )));
    let validator = validator_path();
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
                if let Some(plugins) = cache.cached_clap_plugins(&candidate, fingerprint)
                    && !plugins.is_empty()
                {
                    summary.valid_plugins += plugins.len();
                    let _ = sender.send(PluginScanEvent::ClapPlugins(plugins));
                }
                continue;
            }
            match validate_candidate(root.format, &candidate, &validator) {
                Ok(ValidatedPlugins::Clap(plugins)) => {
                    summary.valid_plugins += plugins.len();
                    cache.mark_checked(candidate.clone(), fingerprint, true, plugins.clone());
                    let _ = sender.send(PluginScanEvent::Log(format!(
                        "Validated {} CLAP plugin(s) from {}",
                        plugins.len(),
                        candidate.display()
                    )));
                    let _ = sender.send(PluginScanEvent::ClapPlugins(plugins));
                }
                Err(error) => {
                    summary.invalid_candidates += 1;
                    cache.mark_checked(candidate.clone(), fingerprint, false, Vec::new());
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

fn candidates_for_root(root: &PluginSearchPath) -> Result<Vec<PathBuf>, String> {
    match root.format {
        PluginFormat::Clap => {
            lilypalooza_clap::candidate_paths(&root.path).map_err(|error| error.to_string())
        }
        PluginFormat::Vst3 => Ok(Vec::new()),
    }
}

enum ValidatedPlugins {
    Clap(Vec<lilypalooza_clap::ClapPluginMetadata>),
}

fn validate_candidate(
    format: PluginFormat,
    path: &Path,
    validator: &Path,
) -> Result<ValidatedPlugins, String> {
    match format {
        PluginFormat::Clap => validate_clap_candidate(path, validator),
        PluginFormat::Vst3 => Err("VST3 adapter is not implemented yet".to_string()),
    }
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

fn validator_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lilypalooza"));
    path.set_file_name(format!(
        "lilypalooza-plugin-validator{}",
        std::env::consts::EXE_SUFFIX
    ));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

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
        cache.mark_checked(path.clone(), old, true, Vec::new());
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
}
