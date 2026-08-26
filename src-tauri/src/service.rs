use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core_service::CoreService;
use crate::error::{CommandError, ErrorCode};
use crate::game_settings::{
    GameSettingsApplyResult, GameSettingsProfile, GameSettingsProfileId, GameSettingsSnapshot,
    GameSettingsStore, GameSettingsValues,
};
use crate::models::*;

const MAX_SAVE_DIRECTORIES_PER_ROOT: usize = 512;
const MAX_ROOTS_CONFIG_BYTES: u64 = 1024 * 1024;

pub struct AppState {
    pub service: ApplicationService,
}

impl AppState {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, std::io::Error> {
        Ok(Self {
            service: ApplicationService::new(app_data_dir)?,
        })
    }
}

pub struct ApplicationService {
    roots_file: PathBuf,
    state: Mutex<ServiceState>,
    write_coordination: Mutex<()>,
    core: CoreService,
    game_settings: GameSettingsStore,
}

#[derive(Default)]
struct ServiceState {
    roots: HashMap<RootId, RootRecord>,
    saves: HashMap<SaveId, SaveRecord>,
}

#[derive(Clone)]
struct RootRecord {
    path: PathBuf,
    installation_root: Option<PathBuf>,
    source: RootSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    Automatic,
    Remembered,
}

#[derive(Clone)]
struct SaveRecord {
    path: PathBuf,
    installation_root: Option<PathBuf>,
    summary: SaveSummary,
}

#[derive(Clone)]
struct InstallationRecord {
    path: PathBuf,
    save_root: Option<PathBuf>,
}

struct NormalizedRoot {
    path: PathBuf,
    installation_root: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedRoots {
    roots: Vec<PersistedRoot>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
enum PersistedRoot {
    Detailed {
        path: PathBuf,
        #[serde(default)]
        installation_root: Option<PathBuf>,
        #[serde(default = "default_true")]
        manual: bool,
    },
    Legacy(PathBuf),
}

const fn default_true() -> bool {
    true
}

struct LoadedRoots {
    persisted: PersistedRoots,
    diagnostic: Option<&'static str>,
}

enum ConfigRead {
    Missing,
    Valid {
        persisted: PersistedRoots,
        bytes: Vec<u8>,
    },
    Malformed,
}

impl ApplicationService {
    fn new(app_data_dir: PathBuf) -> Result<Self, std::io::Error> {
        private_directory_io(&app_data_dir)?;
        private_storage_tree_io(&app_data_dir.join("backups"))?;
        private_storage_tree_io(&app_data_dir.join("transactions"))?;
        let roots_file = app_data_dir.join("roots.json");
        let loaded = load_roots_resilient(&roots_file)?;
        let installations = discovered_installations();
        let automatic = automatic_root_records(&installations);
        let mut roots: HashMap<RootId, RootRecord> = loaded
            .persisted
            .roots
            .into_iter()
            .filter_map(|persisted| {
                let (path, recorded_installation, manual) = match persisted {
                    PersistedRoot::Detailed {
                        path,
                        installation_root,
                        manual,
                    } => (path, installation_root, manual),
                    PersistedRoot::Legacy(path) => (path, None, true),
                };
                if !manual {
                    return None;
                }
                let canonical = canonical_registered_root(&path).ok()?;
                let installation_root = recorded_installation
                    .and_then(|installation| canonical_installation_root(&installation))
                    .filter(|installation| {
                        save_core::resolve_starsector_save_root(installation)
                            .ok()
                            .is_some_and(|configured| platform_path_eq(&configured, &canonical))
                    })
                    .or_else(|| matching_installation_for_save_root(&canonical, &installations));
                let source = if automatic
                    .iter()
                    .any(|record| platform_path_eq(&record.path, &canonical))
                {
                    RootSource::Automatic
                } else {
                    RootSource::Remembered
                };
                let record = RootRecord {
                    path: canonical,
                    installation_root,
                    source,
                };
                Some((RootId::new(stable_id("root", &record.path)), record))
            })
            .collect();
        for record in automatic {
            roots
                .entry(RootId::new(stable_id("root", &record.path)))
                .or_insert(record);
        }
        let core = CoreService::new(app_data_dir.clone())?;
        let game_settings = GameSettingsStore::new(&app_data_dir)?;
        if let Some(message) = loaded.diagnostic {
            core.record_diagnostic(message);
        }

        Ok(Self {
            roots_file,
            state: Mutex::new(ServiceState {
                roots,
                saves: HashMap::new(),
            }),
            write_coordination: Mutex::new(()),
            core,
            game_settings,
        })
    }

    pub fn discover_installations(&self) -> Result<DiscoveryResult, CommandError> {
        let candidates = self
            .known_installations()?
            .into_iter()
            .map(|installation| installation_info(&installation))
            .collect();
        let registered_roots = self
            .lock_state()?
            .roots
            .iter()
            .filter(|(_, root)| root.source == RootSource::Remembered)
            .map(|(id, root)| save_root_view(id.clone(), &root.path))
            .collect();
        self.core
            .record_diagnostic("installation discovery completed");
        Ok(DiscoveryResult {
            installations: candidates,
            registered_roots,
        })
    }

    pub fn list_game_settings_profiles(&self) -> Result<Vec<GameSettingsProfile>, CommandError> {
        self.game_settings.list_profiles()
    }

    pub fn load_game_settings(
        &self,
        installation_id: &InstallationId,
    ) -> Result<GameSettingsSnapshot, CommandError> {
        let installation = self.verified_installation(installation_id)?;
        let info = installation_info(&installation);
        self.game_settings.read_snapshot(
            info.installation_id,
            &installation.path,
            info.display_name,
        )
    }

    pub fn save_game_settings_profile(
        &self,
        profile_id: Option<&GameSettingsProfileId>,
        name: &str,
        values: GameSettingsValues,
    ) -> Result<GameSettingsProfile, CommandError> {
        self.game_settings.save_profile(profile_id, name, values)
    }

    pub fn delete_game_settings_profile(
        &self,
        profile_id: &GameSettingsProfileId,
    ) -> Result<(), CommandError> {
        self.game_settings.delete_profile(profile_id)
    }

    pub fn apply_game_settings(
        &self,
        installation_id: &InstallationId,
        expected_revision: &str,
        values: GameSettingsValues,
    ) -> Result<GameSettingsApplyResult, CommandError> {
        let _write_guard = self.write_coordination.lock().map_err(|_| {
            CommandError::internal("Save and game-settings write coordination is unavailable")
        })?;
        let installation = self.verified_installation(installation_id)?;
        let info = installation_info(&installation);
        self.game_settings.apply(
            info.installation_id,
            &installation.path,
            info.display_name,
            expected_revision,
            values,
        )
    }

    fn known_installations(&self) -> Result<Vec<InstallationRecord>, CommandError> {
        let mut installations = discovered_installations();
        let state = self.lock_state()?;
        for root in state.roots.values() {
            let Some(path) = verified_root_installation(root) else {
                continue;
            };
            let save_root = save_core::resolve_starsector_save_root(&path).ok();
            if let Some(existing) = installations
                .iter_mut()
                .find(|installation| platform_path_eq(&installation.path, &path))
            {
                if existing.save_root.is_none() {
                    existing.save_root = save_root;
                }
            } else {
                installations.push(InstallationRecord { path, save_root });
            }
        }
        installations.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(installations)
    }

    fn verified_installation(
        &self,
        installation_id: &InstallationId,
    ) -> Result<InstallationRecord, CommandError> {
        let mut matches = self
            .known_installations()?
            .into_iter()
            .filter(|installation| {
                stable_id("installation", &installation.path) == installation_id.0
            });
        let installation = matches
            .next()
            .ok_or_else(|| CommandError::not_found("Verified Starsector installation"))?;
        if matches.next().is_some() {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "The installation selector is ambiguous",
            ));
        }
        Ok(installation)
    }

    pub fn register_root(&self, raw_path: &str) -> Result<SaveRoot, CommandError> {
        if raw_path.trim().is_empty() {
            return Err(CommandError::invalid_argument(
                "A save root path is required",
            ));
        }
        let normalized = normalize_selected_root(Path::new(raw_path))?;
        let root_id = RootId::new(stable_id("root", &normalized.path));
        {
            let mut state = self.lock_state()?;
            let mut roots = state.roots.clone();
            roots.insert(
                root_id.clone(),
                RootRecord {
                    path: normalized.path.clone(),
                    installation_root: normalized.installation_root,
                    source: RootSource::Remembered,
                },
            );
            self.persist_roots(&roots)?;
            state.roots = roots;
        }
        self.core
            .record_diagnostic("a manual save root was registered");
        Ok(save_root_view(root_id, &normalized.path))
    }

    pub fn forget_root(&self, root_id: &RootId) -> Result<(), CommandError> {
        let mut state = self.lock_state()?;
        if !state
            .roots
            .get(root_id)
            .is_some_and(|root| root.source == RootSource::Remembered)
        {
            return Err(CommandError::not_found("Save root"));
        }
        let mut roots = state.roots.clone();
        roots.remove(root_id);
        self.persist_roots(&roots)?;
        state.roots = roots;
        state
            .saves
            .retain(|_, save| save.summary.root_id.as_ref() != Some(root_id));
        self.core
            .record_diagnostic("a registered save root was forgotten");
        Ok(())
    }

    pub fn scan_saves(&self, root_id: Option<&RootId>) -> Result<Vec<SaveSummary>, CommandError> {
        if root_id.is_none() {
            self.refresh_automatic_roots()?;
        }
        let roots: Vec<(RootId, RootRecord)> = {
            let state = self.lock_state()?;
            match root_id {
                Some(id) => vec![(
                    id.clone(),
                    state
                        .roots
                        .get(id)
                        .cloned()
                        .ok_or_else(|| CommandError::not_found("Save root"))?,
                )],
                None => state
                    .roots
                    .iter()
                    .map(|(id, root)| (id.clone(), root.clone()))
                    .collect(),
            }
        };

        let mut found = Vec::new();
        let mut records = Vec::new();
        let mut scanned_roots = HashSet::new();
        for (id, root) in roots {
            let installation_root = verified_root_installation(&root);
            let directories = match bounded_save_directories(&root.path) {
                Ok(directories) => directories,
                Err(error) if root_id.is_none() => {
                    self.core.record_diagnostic(&format!(
                        "a save root was unavailable during scanning ({:?})",
                        error.code
                    ));
                    continue;
                }
                Err(error) => return Err(error),
            };
            scanned_roots.insert(id.clone());
            for path in directories {
                let save_id = SaveId::new(stable_id("save", &path));
                match self.core.inspect_save(&path, id.clone(), save_id.clone()) {
                    Ok(summary) => {
                        let registry_id = summary.id.clone();
                        found.push(summary.clone());
                        records.push((
                            registry_id,
                            SaveRecord {
                                path,
                                installation_root: installation_root.clone(),
                                summary,
                            },
                        ));
                    }
                    Err(error) => {
                        let summary =
                            unreadable_save_summary(&path, id.clone(), save_id.clone(), error.code);
                        found.push(summary.clone());
                        records.push((
                            save_id,
                            SaveRecord {
                                path,
                                installation_root: installation_root.clone(),
                                summary,
                            },
                        ));
                        self.core.record_diagnostic(&format!(
                            "a save was classified unreadable during scanning ({:?})",
                            error.code
                        ));
                    }
                }
            }
        }
        sort_save_summaries(&mut found);
        let mut state = self.lock_state()?;
        state.saves.retain(|_, save| {
            !save
                .summary
                .root_id
                .as_ref()
                .is_some_and(|id| scanned_roots.contains(id))
        });
        for (id, record) in records {
            state.saves.insert(id, record);
        }
        self.core.record_diagnostic(&format!(
            "save scan completed with {} result(s)",
            found.len()
        ));
        Ok(found)
    }

    pub fn open_save(&self, save_id: &SaveId) -> Result<SaveSnapshot, CommandError> {
        let record = self.save_record(save_id)?;
        self.core.open_save(
            &record.path,
            record.installation_root.as_deref(),
            record.summary,
        )
    }

    pub fn load_portrait(
        &self,
        session_id: &SessionId,
        portrait_id: &PortraitId,
    ) -> Result<PortraitPayload, CommandError> {
        self.core.load_portrait(session_id, portrait_id)
    }

    pub fn close_session(&self, session_id: &SessionId) -> Result<(), CommandError> {
        self.core.close_session(session_id)
    }

    pub fn discard_review(&self, review_id: &ReviewId) -> Result<(), CommandError> {
        self.core.discard_review(review_id)
    }

    pub fn unlock_protected_save(
        &self,
        session_id: &SessionId,
        acknowledgement: bool,
    ) -> Result<SaveSnapshot, CommandError> {
        let _write_guard = self.write_coordination.lock().map_err(|_| {
            CommandError::internal("Save and game-settings write coordination is unavailable")
        })?;
        self.core.unlock_protected_save(session_id, acknowledgement)
    }

    pub fn prepare_review(
        &self,
        session_id: &SessionId,
        edits: Vec<Edit>,
    ) -> Result<Review, CommandError> {
        if edits.is_empty() {
            return Err(CommandError::invalid_argument(
                "At least one semantic edit is required",
            ));
        }
        self.core.prepare_review(session_id, edits)
    }

    pub fn apply_review(
        &self,
        review_id: &ReviewId,
        mode: ApplyMode,
        acknowledgement: bool,
    ) -> Result<ApplyResult, CommandError> {
        let _write_guard = self.write_coordination.lock().map_err(|_| {
            CommandError::internal("Save and game-settings write coordination is unavailable")
        })?;
        self.ensure_no_unresolved_recovery()?;
        let result = self.core.apply_review(review_id, mode, acknowledgement)?;
        self.remember_applied_location(&result.target_path);
        Ok(result)
    }

    pub fn list_backups(&self, save_id: &SaveId) -> Result<Vec<BackupSummary>, CommandError> {
        self.save_record(save_id)?;
        self.core.list_backups(save_id)
    }

    pub fn prepare_restore(
        &self,
        session_id: &SessionId,
        backup_id: &BackupId,
    ) -> Result<Review, CommandError> {
        self.core.prepare_restore(session_id, backup_id)
    }

    pub fn apply_restore(
        &self,
        review_id: &ReviewId,
        acknowledgement: bool,
    ) -> Result<ApplyResult, CommandError> {
        let _write_guard = self.write_coordination.lock().map_err(|_| {
            CommandError::internal("Save and game-settings write coordination is unavailable")
        })?;
        // A restore whose backup matches the pending journal is the recovery
        // action itself. CoreService rejects unrelated restores while any
        // recovery remains unresolved.
        let result = self.core.apply_restore(review_id, acknowledgement)?;
        self.remember_applied_location(&result.target_path);
        Ok(result)
    }

    pub fn startup_recovery_state(&self) -> Result<RecoveryState, CommandError> {
        self.core.startup_recovery_state()
    }

    pub fn export_diagnostics(&self) -> Result<Diagnostics, CommandError> {
        self.core.export_diagnostics()
    }

    fn save_record(&self, save_id: &SaveId) -> Result<SaveRecord, CommandError> {
        self.lock_state()?
            .saves
            .get(save_id)
            .cloned()
            .ok_or_else(|| CommandError::not_found("Save"))
    }

    fn ensure_no_unresolved_recovery(&self) -> Result<(), CommandError> {
        let recovery = self.core.startup_recovery_state()?;
        if recovery.status == RecoveryStatus::RecoveryRequired {
            return Err(CommandError::new(
                ErrorCode::RecoveryRequired,
                "Resolve the interrupted save transaction before writing another save",
            ));
        }
        Ok(())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ServiceState>, CommandError> {
        self.state
            .lock()
            .map_err(|_| CommandError::internal("Application state is unavailable"))
    }

    fn persist_roots(&self, roots: &HashMap<RootId, RootRecord>) -> Result<(), CommandError> {
        let mut persisted = roots
            .values()
            .filter(|root| root.source == RootSource::Remembered)
            .map(|root| PersistedRoot::Detailed {
                path: root.path.clone(),
                installation_root: root.installation_root.clone(),
                manual: true,
            })
            .collect::<Vec<_>>();
        persisted.sort_by_key(|entry| {
            match entry {
                PersistedRoot::Detailed { path, .. } | PersistedRoot::Legacy(path) => path,
            }
            .to_string_lossy()
            .into_owned()
        });
        let payload = serde_json::to_vec_pretty(&PersistedRoots { roots: persisted })
            .map_err(|_| CommandError::internal("Could not encode registered save roots"))?;
        persist_roots_atomically(&self.roots_file, &payload)?;
        Ok(())
    }

    fn refresh_automatic_roots(&self) -> Result<(), CommandError> {
        let installations = discovered_installations();
        let records = automatic_root_records(&installations);
        let mut state = self.lock_state()?;
        let removed_root_ids = state
            .roots
            .iter()
            .filter(|(_, root)| root.source == RootSource::Automatic)
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        state
            .roots
            .retain(|_, root| root.source == RootSource::Remembered);
        state.saves.retain(|_, save| {
            !save
                .summary
                .root_id
                .as_ref()
                .is_some_and(|id| removed_root_ids.contains(id))
        });
        for record in records {
            match state
                .roots
                .entry(RootId::new(stable_id("root", &record.path)))
            {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(record);
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().installation_root.is_none() =>
                {
                    entry.get_mut().installation_root = record.installation_root;
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        Ok(())
    }

    fn remember_applied_location(&self, display_path: &str) {
        let save_path = Path::new(display_path);
        let Some(root_path) = save_path.parent() else {
            return;
        };
        let Ok(root_path) = canonical_registered_root(root_path) else {
            return;
        };
        let root_id = RootId::new(stable_id("root", &root_path));
        let installations = discovered_installations();
        let root = root_record(root_path, &installations, RootSource::Remembered);
        if let Ok(mut state) = self.state.lock() {
            let mut roots = state.roots.clone();
            roots.insert(root_id.clone(), root);
            if self.persist_roots(&roots).is_ok() {
                state.roots = roots;
            } else {
                self.core.record_diagnostic(
                    "the applied save location could not be remembered because configuration persistence failed",
                );
                return;
            }
        }
        let _ = self.scan_saves(Some(&root_id));
    }
}

fn load_roots_resilient(path: &Path) -> Result<LoadedRoots, std::io::Error> {
    let previous = roots_previous_path(path);
    match read_roots_config(path)? {
        ConfigRead::Valid { persisted, .. } => {
            harden_optional_private_file(&previous)?;
            Ok(LoadedRoots {
                persisted,
                diagnostic: None,
            })
        }
        ConfigRead::Missing => recover_previous_roots(path, &previous, false),
        ConfigRead::Malformed => {
            quarantine_config(path, "roots")?;
            recover_previous_roots(path, &previous, true)
        }
    }
}

fn recover_previous_roots(
    primary: &Path,
    previous: &Path,
    primary_was_malformed: bool,
) -> Result<LoadedRoots, std::io::Error> {
    match read_roots_config(previous)? {
        ConfigRead::Valid { persisted, bytes } => {
            atomic_write_private_file(primary, &bytes)?;
            Ok(LoadedRoots {
                persisted,
                diagnostic: Some(
                    "registered save roots were recovered from the previous durable configuration",
                ),
            })
        }
        ConfigRead::Missing => Ok(LoadedRoots {
            persisted: PersistedRoots::default(),
            diagnostic: primary_was_malformed.then_some(
                "registered save-root configuration was malformed; the unreadable file was preserved",
            ),
        }),
        ConfigRead::Malformed => {
            quarantine_config(previous, "roots-previous")?;
            Ok(LoadedRoots {
                persisted: PersistedRoots::default(),
                diagnostic: Some(
                    "registered save-root configurations were malformed; unreadable files were preserved",
                ),
            })
        }
    }
}

fn read_roots_config(path: &Path) -> Result<ConfigRead, std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigRead::Missing);
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "save-root configuration is not a regular file",
        ));
    }
    private_file_io(path)?;
    if metadata.len() > MAX_ROOTS_CONFIG_BYTES {
        return Ok(ConfigRead::Malformed);
    }
    let bytes = fs::read(path)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_ROOTS_CONFIG_BYTES) {
        return Ok(ConfigRead::Malformed);
    }
    match serde_json::from_slice(&bytes) {
        Ok(persisted) => Ok(ConfigRead::Valid { persisted, bytes }),
        Err(_) => Ok(ConfigRead::Malformed),
    }
}

fn persist_roots_atomically(path: &Path, payload: &[u8]) -> Result<(), CommandError> {
    if u64::try_from(payload.len()).map_or(true, |length| length > MAX_ROOTS_CONFIG_BYTES) {
        return Err(CommandError::internal(
            "Registered save roots exceed the configuration size limit",
        ));
    }
    match read_roots_config(path)? {
        ConfigRead::Valid { bytes, .. } => {
            atomic_write_private_file(&roots_previous_path(path), &bytes)?;
        }
        ConfigRead::Missing => {}
        ConfigRead::Malformed => {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Remembered save-root configuration changed and is unreadable; restart the app to recover it safely",
            ));
        }
    }
    atomic_write_private_file(path, payload)?;
    Ok(())
}

fn roots_previous_path(path: &Path) -> PathBuf {
    path.with_file_name("roots.previous.json")
}

fn quarantine_config(path: &Path, label: &str) -> Result<(), std::io::Error> {
    private_file_io(path)?;
    let quarantine =
        path.with_file_name(format!("{label}.corrupt-{}.json", Uuid::new_v4().simple()));
    replace_private_file_io(path, &quarantine)
}

fn atomic_write_private_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration file has no parent directory",
        )
    })?;
    private_directory_io(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "configuration destination is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> Result<(), std::io::Error> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_private_file_io(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn replace_private_file_io(replacement: &Path, destination: &Path) -> Result<(), std::io::Error> {
    save_core::replace_file_atomically(replacement, destination).map_err(core_error_io)
}

fn private_directory_io(path: &Path) -> Result<(), std::io::Error> {
    save_core::ensure_private_directory(path).map_err(core_error_io)
}

fn private_storage_tree_io(path: &Path) -> Result<(), std::io::Error> {
    save_core::harden_private_storage_tree(path).map_err(core_error_io)
}

fn private_file_io(path: &Path) -> Result<(), std::io::Error> {
    save_core::harden_private_file(path).map_err(core_error_io)
}

fn harden_optional_private_file(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private configuration is not a regular file",
            ))
        }
        Ok(_) => private_file_io(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn core_error_io(error: save_core::CoreError) -> std::io::Error {
    std::io::Error::other(error.message)
}

fn normalize_selected_root(selected: &Path) -> Result<NormalizedRoot, CommandError> {
    reject_symlink(selected)?;
    #[cfg(target_os = "macos")]
    let installation_candidate = if selected
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        selected.join("Contents").join("Resources").join("Java")
    } else {
        selected.to_path_buf()
    };
    #[cfg(not(target_os = "macos"))]
    let installation_candidate = selected.to_path_buf();

    if let Some(installation_root) = canonical_installation_root(&installation_candidate) {
        let path = save_core::resolve_starsector_save_root(&installation_root).map_err(|_| {
            CommandError::new(
                ErrorCode::ValidationFailed,
                "The selected Starsector installation has an unreadable or ambiguous configured save folder",
            )
        })?;
        return Ok(NormalizedRoot {
            path,
            installation_root: Some(installation_root),
        });
    }

    let candidate = if selected.is_file() {
        let file_name = selected.file_name().and_then(|name| name.to_str());
        if !matches!(file_name, Some("campaign.xml" | "descriptor.xml")) {
            return Err(CommandError::invalid_argument(
                "Select a Starsector installation, saves folder, save folder, campaign.xml, or descriptor.xml",
            ));
        }
        selected
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| CommandError::invalid_argument("The selected save has no saves folder"))?
            .to_path_buf()
    } else if looks_like_save_directory(selected) {
        selected
            .parent()
            .ok_or_else(|| CommandError::invalid_argument("The selected save has no saves folder"))?
            .to_path_buf()
    } else if selected.join("saves").is_dir() {
        selected.join("saves")
    } else {
        selected.to_path_buf()
    };
    let path = canonical_registered_root(&candidate)?;
    let installations = discovered_installations();
    Ok(NormalizedRoot {
        installation_root: matching_installation_for_save_root(&path, &installations),
        path,
    })
}

fn root_record(
    path: PathBuf,
    installations: &[InstallationRecord],
    source: RootSource,
) -> RootRecord {
    RootRecord {
        installation_root: matching_installation_for_save_root(&path, installations),
        path,
        source,
    }
}

fn verified_root_installation(root: &RootRecord) -> Option<PathBuf> {
    let installation = canonical_installation_root(root.installation_root.as_ref()?)?;
    let configured = save_core::resolve_starsector_save_root(&installation).ok()?;
    platform_path_eq(&configured, &root.path).then_some(installation)
}

fn matching_installation_for_save_root(
    save_root: &Path,
    installations: &[InstallationRecord],
) -> Option<PathBuf> {
    let mut matching = installations
        .iter()
        .filter(|installation| {
            installation
                .save_root
                .as_ref()
                .is_some_and(|candidate| platform_path_eq(candidate, save_root))
        })
        .map(|installation| installation.path.clone());
    let installation = matching.next()?;
    matching.next().is_none().then_some(installation)
}

fn discovered_installations() -> Vec<InstallationRecord> {
    let mut installations = Vec::new();
    for candidate in platform_installation_candidates() {
        let Some(path) = canonical_installation_root(&candidate) else {
            continue;
        };
        if installations
            .iter()
            .any(|installation: &InstallationRecord| platform_path_eq(&installation.path, &path))
        {
            continue;
        }
        let save_root = save_core::resolve_starsector_save_root(&path).ok();
        installations.push(InstallationRecord { path, save_root });
    }
    installations
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeInstallationLayout {
    Windows,
    MacOs,
    Linux,
}

#[cfg(windows)]
const HOST_INSTALLATION_LAYOUT: NativeInstallationLayout = NativeInstallationLayout::Windows;
#[cfg(target_os = "macos")]
const HOST_INSTALLATION_LAYOUT: NativeInstallationLayout = NativeInstallationLayout::MacOs;
#[cfg(all(unix, not(target_os = "macos")))]
const HOST_INSTALLATION_LAYOUT: NativeInstallationLayout = NativeInstallationLayout::Linux;

fn canonical_installation_root(path: &Path) -> Option<PathBuf> {
    canonical_installation_root_for_layout(path, HOST_INSTALLATION_LAYOUT)
}

fn canonical_installation_root_for_layout(
    path: &Path,
    layout: NativeInstallationLayout,
) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    let asset_root = match layout {
        NativeInstallationLayout::Windows => canonical.join("starsector-core"),
        NativeInstallationLayout::MacOs | NativeInstallationLayout::Linux => canonical.clone(),
    };
    if !is_directory_non_symlink(&asset_root)
        || !is_directory_non_symlink(&asset_root.join("data").join("config"))
        || !is_regular_non_symlink(&asset_root.join("data").join("config").join("settings.json"))
        || !is_regular_non_symlink(&asset_root.join("starfarer.api.jar"))
        || !is_regular_non_symlink(&asset_root.join("starfarer_obf.jar"))
    {
        return None;
    }

    match layout {
        NativeInstallationLayout::Windows => {
            if !is_regular_non_symlink(&canonical.join("starsector.exe"))
                || !is_regular_non_symlink(&canonical.join("jre").join("bin").join("java.exe"))
            {
                return None;
            }
        }
        NativeInstallationLayout::Linux => {
            if !is_regular_non_symlink(&canonical.join("starsector.sh"))
                || !is_regular_non_symlink(&canonical.join("jre_linux").join("bin").join("java"))
                || !is_directory_non_symlink(&canonical.join("native").join("linux"))
            {
                return None;
            }
        }
        NativeInstallationLayout::MacOs => {
            let resources = canonical.parent()?;
            let contents = resources.parent()?;
            let app = contents.parent()?;
            if !path_file_name_eq(&canonical, "Java")
                || !path_file_name_eq(resources, "Resources")
                || !path_file_name_eq(contents, "Contents")
                || !app
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
                || !is_regular_non_symlink(&contents.join("MacOS").join("starsector_mac.sh"))
                || !is_directory_non_symlink(&canonical.join("native").join("macosx"))
            {
                return None;
            }
        }
    }
    Some(canonical)
}

fn is_directory_non_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn path_file_name_eq(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn installation_display_root(installation: &Path) -> &Path {
    if path_file_name_eq(installation, "Java") {
        if let Some(resources) = installation.parent() {
            if path_file_name_eq(resources, "Resources") {
                if let Some(contents) = resources.parent() {
                    if path_file_name_eq(contents, "Contents") {
                        if let Some(app) = contents.parent() {
                            if app
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
                            {
                                return app;
                            }
                        }
                    }
                }
            }
        }
    }
    installation
}

fn automatic_root_records(installations: &[InstallationRecord]) -> Vec<RootRecord> {
    let mut records = Vec::new();
    for installation in installations {
        if let Some(path) = installation.save_root.clone() {
            push_unique_root(
                &mut records,
                RootRecord {
                    path,
                    installation_root: Some(installation.path.clone()),
                    source: RootSource::Automatic,
                },
            );
        }
    }
    for candidate in platform_standalone_save_root_candidates(installations) {
        let candidate = if candidate.join("saves").is_dir() {
            candidate.join("saves")
        } else {
            candidate
        };
        let Ok(path) = canonical_registered_root(&candidate) else {
            continue;
        };
        push_unique_root(
            &mut records,
            root_record(path, installations, RootSource::Automatic),
        );
    }
    records
}

fn push_unique_root(records: &mut Vec<RootRecord>, candidate: RootRecord) {
    if let Some(existing) = records
        .iter_mut()
        .find(|existing| platform_path_eq(&existing.path, &candidate.path))
    {
        if existing.installation_root.is_none() {
            existing.installation_root = candidate.installation_root;
        }
    } else {
        records.push(candidate);
    }
}

#[cfg(windows)]
fn platform_path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn platform_path_eq(left: &Path, right: &Path) -> bool {
    left == right
}

fn canonical_registered_root(path: &Path) -> Result<PathBuf, CommandError> {
    reject_symlink(path)?;
    let canonical = fs::canonicalize(path)?;
    reject_symlink(&canonical)?;
    if !canonical.is_dir() {
        return Err(CommandError::invalid_argument(
            "The save root must be a directory",
        ));
    }
    Ok(canonical)
}

fn reject_symlink(path: &Path) -> Result<(), CommandError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Symbolic links are not accepted as save inputs",
        ));
    }
    Ok(())
}

fn is_save_directory(path: &Path) -> bool {
    is_regular_non_symlink(&path.join("campaign.xml"))
        && is_regular_non_symlink(&path.join("descriptor.xml"))
}

fn looks_like_save_directory(path: &Path) -> bool {
    is_save_directory(path)
}

fn is_regular_non_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn bounded_save_directories(root: &Path) -> Result<Vec<PathBuf>, CommandError> {
    reject_symlink(root)?;
    let mut saves = Vec::new();
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        if index >= MAX_SAVE_DIRECTORIES_PER_ROOT {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "The selected root exceeds the bounded directory-entry limit",
            ));
        }
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let path = entry.path();
        if looks_like_save_directory(&path) {
            saves.push(path);
        }
    }
    Ok(saves)
}

fn stable_id(namespace: &str, path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(namespace.as_bytes());
    digest.update([0]);
    digest.update(path.as_os_str().to_string_lossy().as_bytes());
    let hash = digest.finalize();
    format!("{namespace}-{}", hex::encode(&hash[..16]))
}

fn sort_save_summaries(summaries: &mut [SaveSummary]) {
    summaries.sort_by(|left, right| {
        save_error_rank(left)
            .cmp(&save_error_rank(right))
            .then_with(|| save_date_missing(left).cmp(&save_date_missing(right)))
            .then_with(|| right.save_date.cmp(&left.save_date))
            .then_with(|| left.character_name.cmp(&right.character_name))
            .then_with(|| left.path.cmp(&right.path))
    });
}

const fn save_error_rank(summary: &SaveSummary) -> u8 {
    if matches!(summary.compatibility, CompatibilityState::Unreadable) {
        1
    } else {
        0
    }
}

fn save_date_missing(summary: &SaveSummary) -> bool {
    summary.save_date.trim().is_empty() || summary.save_date.eq_ignore_ascii_case("unknown")
}

fn save_root_view(root_id: RootId, path: &Path) -> SaveRoot {
    SaveRoot {
        root_id,
        display_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Starsector saves")
            .to_owned(),
        display_path: path.to_string_lossy().into_owned(),
        available: path.is_dir(),
        writable: fs::metadata(path)
            .map(|metadata| !metadata.permissions().readonly())
            .unwrap_or(false),
    }
}

fn installation_info(installation: &InstallationRecord) -> InstallationInfo {
    let display_root = installation_display_root(&installation.path);
    InstallationInfo {
        installation_id: InstallationId::new(stable_id("installation", &installation.path)),
        display_name: display_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Starsector")
            .to_owned(),
        display_path: display_root.to_string_lossy().into_owned(),
        detected_version: None,
        saves_root_available: installation
            .save_root
            .as_ref()
            .is_some_and(|path| path.is_dir()),
    }
}

fn unreadable_save_summary(
    path: &Path,
    root_id: RootId,
    save_id: SaveId,
    code: ErrorCode,
) -> SaveSummary {
    SaveSummary {
        id: save_id,
        root_id: Some(root_id),
        path: path.to_string_lossy().into_owned(),
        character_name: "Unreadable save".into(),
        character_level: 0,
        game_version: "Unknown".into(),
        save_file_version: "Unknown".into(),
        save_date: "Unknown".into(),
        location: "Unknown".into(),
        iron_mode: false,
        autosave: false,
        compressed: false,
        enabled_mods: Vec::new(),
        compatibility: CompatibilityState::Unreadable,
        compatibility_reason: Some(format!(
            "The save could not be inspected safely ({}).",
            code.as_str()
        )),
    }
}

#[cfg(target_os = "windows")]
fn platform_installation_candidates() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    let mut candidates = Vec::new();
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Software\\Fractal Softworks\\Starsector", KEY_READ | view)
        {
            if let Ok(value) = key.get_value::<String, _>("") {
                if let Some(path) = registry_installation_path(&value) {
                    candidates.push(path);
                }
            }
        }
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Starsector",
                KEY_READ | view,
            ) else {
                continue;
            };
            for name in ["InstallLocation", "UninstallString", "DisplayIcon"] {
                if let Ok(value) = key.get_value::<String, _>(name) {
                    if let Some(path) = registry_installation_path(&value) {
                        candidates.push(path);
                    }
                }
            }
        }
    }
    candidates.extend(
        ["ProgramFiles(x86)", "ProgramFiles"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .map(|base| base.join("Fractal Softworks").join("Starsector")),
    );
    candidates.extend(windows_document_starsector_candidates());
    candidates
}

#[cfg(target_os = "windows")]
fn registry_installation_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let candidate = if let Some(quoted) = value.strip_prefix('"') {
        let closing = quoted.find('"')?;
        &quoted[..closing]
    } else if let Some(executable_end) = value.to_ascii_lowercase().find(".exe") {
        &value[..executable_end + 4]
    } else if let Some((path, icon_index)) = value.rsplit_once(',') {
        if icon_index.trim().parse::<i32>().is_ok() {
            path
        } else {
            value
        }
    } else {
        value
    };
    let path = PathBuf::from(candidate.trim().trim_matches('"'));
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        path.parent().map(Path::to_path_buf)
    } else if path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("starsector-core"))
    {
        path.parent().and_then(Path::parent).map(Path::to_path_buf)
    } else {
        Some(path)
    }
}

#[cfg(target_os = "windows")]
fn platform_standalone_save_root_candidates(installations: &[InstallationRecord]) -> Vec<PathBuf> {
    let mut candidates = windows_document_starsector_candidates();
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            let Some(program_files) = std::env::var_os(variable).map(PathBuf::from) else {
                continue;
            };
            let Some(folder_name) = program_files.file_name() else {
                continue;
            };
            let virtual_program_files = local_app_data.join("VirtualStore").join(folder_name);
            candidates.push(
                virtual_program_files
                    .join("Fractal Softworks")
                    .join("Starsector")
                    .join("saves"),
            );
            for installation in installations {
                let Ok(relative) = installation.path.strip_prefix(&program_files) else {
                    continue;
                };
                candidates.push(virtual_program_files.join(relative).join("saves"));
            }
        }
    }
    candidates
}

#[cfg(target_os = "windows")]
fn windows_document_starsector_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(documents) = dirs::document_dir() {
        candidates.push(documents.join("Starsector"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        candidates.push(profile.join("Documents").join("Starsector"));
    }
    for variable in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(one_drive) = std::env::var_os(variable).map(PathBuf::from) {
            candidates.push(one_drive.join("Documents").join("Starsector"));
        }
    }
    candidates
}

#[cfg(target_os = "macos")]
fn platform_installation_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/Applications/Starsector.app/Contents/Resources/Java"),
        PathBuf::from("/Applications/Games/Starsector.app/Contents/Resources/Java"),
    ];
    if let Some(home) = dirs::home_dir() {
        candidates.push(
            home.join("Applications")
                .join("Starsector.app")
                .join("Contents")
                .join("Resources")
                .join("Java"),
        );
    }
    candidates
}

#[cfg(target_os = "macos")]
fn platform_standalone_save_root_candidates(_installations: &[InstallationRecord]) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_installation_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/starsector"),
        PathBuf::from("/usr/local/games/starsector"),
        PathBuf::from("/usr/local/share/starsector"),
        PathBuf::from("/usr/share/games/starsector"),
        PathBuf::from("/usr/share/starsector"),
    ];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("starsector"));
        candidates.push(home.join("Starsector"));
        candidates.push(home.join("Games").join("starsector"));
        candidates.push(home.join("Games").join("Starsector"));
        candidates.push(home.join("games").join("starsector"));
    }
    if let Some(data_home) = linux_data_home() {
        candidates.push(data_home.join("starsector"));
    }
    candidates
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_standalone_save_root_candidates(_installations: &[InstallationRecord]) -> Vec<PathBuf> {
    linux_data_home()
        .map(|data_home| vec![data_home.join("starsector").join("saves")])
        .unwrap_or_default()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn test_summary(id: &str, save_date: &str, compatibility: CompatibilityState) -> SaveSummary {
        SaveSummary {
            id: SaveId::new(id),
            root_id: None,
            path: format!(r"C:\saves\{id}"),
            character_name: id.to_owned(),
            character_level: 1,
            game_version: "test".to_owned(),
            save_file_version: "test".to_owned(),
            save_date: save_date.to_owned(),
            location: String::new(),
            iron_mode: false,
            autosave: false,
            compressed: false,
            enabled_mods: Vec::new(),
            compatibility,
            compatibility_reason: None,
        }
    }

    fn create_asset_root(asset_root: &Path) {
        fs::create_dir_all(asset_root.join("data/config")).unwrap();
        fs::write(
            asset_root.join("data/config/settings.json"),
            br#"{
                "playerMaxLevel": 15,
                "skillPointsPerLevel": 1,
                "storyPointsPerLevel": 4,
                "bonusXPUseMultAtMaxLevel": 3,
                "officerXPRequiredMult": 4,
                "officerMaxLevel": 5,
                "officerMaxEliteSkills": 1
            }"#,
        )
        .unwrap();
        fs::write(asset_root.join("starfarer.api.jar"), b"api").unwrap();
        fs::write(asset_root.join("starfarer_obf.jar"), b"game").unwrap();
    }

    #[test]
    fn stable_ids_do_not_reveal_the_path() {
        let id = stable_id("save", Path::new("/private/a/save"));
        assert!(id.starts_with("save-"));
        assert!(!id.contains("private"));
    }

    #[test]
    fn game_settings_reject_forged_installation_selectors_and_manage_local_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let service = ApplicationService::new(temp.path().join("app-data")).unwrap();
        let forged = InstallationId::new("installation-forged");
        assert_eq!(
            service.load_game_settings(&forged).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(
            service
                .apply_game_settings(&forged, &"0".repeat(64), GameSettingsValues::VANILLA_RC8,)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );

        let profile = service
            .save_game_settings_profile(
                None,
                "Long campaign",
                GameSettingsValues {
                    player_max_level: 30,
                    ..GameSettingsValues::VANILLA_RC8
                },
            )
            .unwrap();
        assert_eq!(service.list_game_settings_profiles().unwrap().len(), 2);
        service
            .delete_game_settings_profile(&profile.profile_id)
            .unwrap();
        assert_eq!(service.list_game_settings_profiles().unwrap().len(), 1);
    }

    #[test]
    fn usable_dated_saves_sort_ahead_of_unreadable_archives() {
        let mut saves = vec![
            test_summary("unknown", "Unknown", CompatibilityState::Unreadable),
            test_summary("old-preview", "2022-07-30", CompatibilityState::Preview),
            test_summary("current", "2026-08-23", CompatibilityState::Editable),
            test_summary(
                "broken-new-date",
                "2099-01-01",
                CompatibilityState::Unreadable,
            ),
        ];

        sort_save_summaries(&mut saves);

        assert_eq!(
            saves
                .iter()
                .map(|summary| summary.id.0.as_str())
                .collect::<Vec<_>>(),
            ["current", "old-preview", "broken-new-date", "unknown"]
        );
    }

    #[test]
    fn selected_save_folder_normalizes_to_its_parent_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("saves");
        let save = root.join("save_Test_123");
        fs::create_dir_all(&save).unwrap();
        File::create(save.join("campaign.xml")).unwrap();
        File::create(save.join("descriptor.xml")).unwrap();

        assert_eq!(
            normalize_selected_root(&save).unwrap().path,
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn documents_starsector_folder_normalizes_to_its_saves_child() {
        let temp = tempfile::tempdir().unwrap();
        let starsector = temp.path().join("Documents").join("Starsector");
        let saves = starsector.join("saves");
        fs::create_dir_all(&saves).unwrap();

        let normalized = normalize_selected_root(&starsector).unwrap();
        assert_eq!(normalized.path, saves.canonicalize().unwrap());
        assert!(normalized.installation_root.is_none());
    }

    #[cfg(windows)]
    fn create_windows_install(installation: &Path, save_root: &Path) {
        fs::create_dir_all(installation.join("starsector-core")).unwrap();
        create_asset_root(&installation.join("starsector-core"));
        fs::create_dir_all(installation.join("jre/bin")).unwrap();
        fs::create_dir_all(save_root).unwrap();
        fs::write(installation.join("starsector.exe"), b"wrapper").unwrap();
        fs::write(installation.join("jre/bin/java.exe"), b"jvm").unwrap();
        fs::write(
            installation.join("vmparams"),
            format!(
                "java \"-Dcom.fs.starfarer.settings.paths.saves={}\" Game",
                save_root.display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn native_installation_layouts_require_platform_launchers_and_game_assets() {
        let temp = tempfile::tempdir().unwrap();

        let windows = temp.path().join("windows");
        create_asset_root(&windows.join("starsector-core"));
        fs::create_dir_all(windows.join("jre/bin")).unwrap();
        fs::write(windows.join("starsector.exe"), b"wrapper").unwrap();
        fs::write(windows.join("jre/bin/java.exe"), b"jvm").unwrap();
        assert!(canonical_installation_root_for_layout(
            &windows,
            NativeInstallationLayout::Windows
        )
        .is_some());

        let linux = temp.path().join("linux");
        create_asset_root(&linux);
        fs::create_dir_all(linux.join("jre_linux/bin")).unwrap();
        fs::create_dir_all(linux.join("native/linux")).unwrap();
        fs::write(linux.join("starsector.sh"), b"#!/bin/sh").unwrap();
        fs::write(linux.join("jre_linux/bin/java"), b"jvm").unwrap();
        assert!(
            canonical_installation_root_for_layout(&linux, NativeInstallationLayout::Linux)
                .is_some()
        );

        let app = temp.path().join("Starsector.app");
        let java = app.join("Contents/Resources/Java");
        create_asset_root(&java);
        fs::create_dir_all(java.join("native/macosx")).unwrap();
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/starsector_mac.sh"), b"#!/bin/sh").unwrap();
        let canonical_java =
            canonical_installation_root_for_layout(&java, NativeInstallationLayout::MacOs).unwrap();
        assert_eq!(
            installation_display_root(&canonical_java),
            app.canonicalize().unwrap()
        );

        fs::remove_file(java.join("starfarer_obf.jar")).unwrap();
        assert!(
            canonical_installation_root_for_layout(&java, NativeInstallationLayout::MacOs)
                .is_none()
        );
    }

    #[cfg(windows)]
    fn create_arbitrary_host_install(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let installation = base.join("Arbitrary Starsector");
        let saves = installation.join("saves");
        create_windows_install(&installation, &saves);
        let asset_root = installation.join("starsector-core");
        (installation.clone(), installation, asset_root)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn create_arbitrary_host_install(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let installation = base.join("Arbitrary Starsector");
        create_asset_root(&installation);
        fs::create_dir_all(installation.join("saves")).unwrap();
        fs::create_dir_all(installation.join("jre_linux/bin")).unwrap();
        fs::create_dir_all(installation.join("native/linux")).unwrap();
        fs::write(installation.join("starsector.sh"), b"#!/bin/sh").unwrap();
        fs::write(installation.join("jre_linux/bin/java"), b"jvm").unwrap();
        (installation.clone(), installation.clone(), installation)
    }

    #[cfg(target_os = "macos")]
    fn create_arbitrary_host_install(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let app = base.join("Renamed Campaign Tools.app");
        let java = app.join("Contents/Resources/Java");
        create_asset_root(&java);
        fs::create_dir_all(java.join("saves")).unwrap();
        fs::create_dir_all(java.join("native/macosx")).unwrap();
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/starsector_mac.sh"), b"#!/bin/sh").unwrap();
        (app, java.clone(), java)
    }

    #[test]
    fn remembered_arbitrary_install_is_exposed_and_revalidated_for_settings() {
        let temp = tempfile::tempdir().unwrap();
        let (selected, installation_candidate, asset_root) =
            create_arbitrary_host_install(temp.path());
        let service = ApplicationService::new(temp.path().join("app-data")).unwrap();
        service
            .register_root(selected.to_str().expect("test path is UTF-8"))
            .unwrap();

        let installation = canonical_installation_root(&installation_candidate).unwrap();
        let installation_id = InstallationId::new(stable_id("installation", &installation));
        let discovery = service.discover_installations().unwrap();
        assert!(discovery
            .installations
            .iter()
            .any(|candidate| candidate.installation_id == installation_id));
        assert_eq!(
            service
                .load_game_settings(&installation_id)
                .unwrap()
                .installation_id,
            installation_id
        );

        fs::remove_file(asset_root.join("starfarer_obf.jar")).unwrap();
        assert_eq!(
            service
                .load_game_settings(&installation_id)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[cfg(windows)]
    #[test]
    fn selected_installation_uses_and_remembers_its_configured_external_root() {
        let temp = tempfile::tempdir().unwrap();
        let installation = temp.path().join("Custom Starsector");
        let save_root = temp
            .path()
            .join("Documents")
            .join("Starsector")
            .join("saves");
        create_windows_install(&installation, &save_root);
        let save = save_root.join("save_External_123");
        fs::create_dir(&save).unwrap();
        fs::write(save.join("campaign.xml"), b"<campaign></campaign>").unwrap();
        fs::write(save.join("descriptor.xml"), b"<descriptor></descriptor>").unwrap();

        let normalized = normalize_selected_root(&installation).unwrap();
        assert_eq!(normalized.path, save_root.canonicalize().unwrap());
        assert_eq!(
            normalized.installation_root.as_deref(),
            Some(installation.canonicalize().unwrap().as_path())
        );

        let app_data = temp.path().join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        fs::write(
            app_data.join("roots.json"),
            serde_json::to_vec_pretty(&PersistedRoots {
                roots: vec![PersistedRoot::Detailed {
                    path: normalized.path.clone(),
                    installation_root: normalized.installation_root.clone(),
                    manual: true,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let service = ApplicationService::new(app_data).unwrap();
        let root_id = RootId::new(stable_id("root", &normalized.path));
        let state = service.lock_state().unwrap();
        let root = state.roots.get(&root_id).unwrap();
        assert_eq!(
            verified_root_installation(root).as_deref(),
            normalized.installation_root.as_deref()
        );
        drop(state);

        let summaries = service.scan_saves(Some(&root_id)).unwrap();
        assert_eq!(summaries.len(), 1);
        let state = service.lock_state().unwrap();
        let save = state.saves.get(&summaries[0].id).unwrap();
        assert_eq!(
            save.installation_root.as_deref(),
            normalized.installation_root.as_deref()
        );
    }

    #[cfg(windows)]
    #[test]
    fn registry_values_resolve_install_uninstaller_and_icon_paths() {
        assert_eq!(
            registry_installation_path(r#"D:\Games\Starsector"#).unwrap(),
            PathBuf::from(r"D:\Games\Starsector")
        );
        assert_eq!(
            registry_installation_path(
                r#""C:\Program Files (x86)\Fractal Softworks\Starsector\UninstallStarsector.exe""#
            )
            .unwrap(),
            PathBuf::from(r"C:\Program Files (x86)\Fractal Softworks\Starsector")
        );
        assert_eq!(
            registry_installation_path(
                r#"C:\Program Files (x86)\Fractal Softworks\Starsector\starsector-core\starsector.ico,0"#
            )
            .unwrap(),
            PathBuf::from(r"C:\Program Files (x86)\Fractal Softworks\Starsector")
        );
    }

    #[test]
    fn persisted_roots_accept_the_legacy_string_schema() {
        let persisted: PersistedRoots =
            serde_json::from_str(r#"{"roots":["C:\\Games\\Starsector\\saves"]}"#).unwrap();
        assert!(matches!(
            persisted.roots.as_slice(),
            [PersistedRoot::Legacy(path)] if path == Path::new(r"C:\Games\Starsector\saves")
        ));
    }

    #[test]
    fn malformed_roots_recover_the_previous_durable_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let root = temp.path().join("manual-saves");
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir(&root).unwrap();
        let canonical = root.canonicalize().unwrap();
        let previous = PersistedRoots {
            roots: vec![PersistedRoot::Detailed {
                path: canonical.clone(),
                installation_root: None,
                manual: true,
            }],
        };
        fs::write(app_data.join("roots.json"), b"{ definitely not json").unwrap();
        fs::write(
            app_data.join("roots.previous.json"),
            serde_json::to_vec_pretty(&previous).unwrap(),
        )
        .unwrap();

        let service = ApplicationService::new(app_data.clone()).unwrap();
        let root_id = RootId::new(stable_id("root", &canonical));
        assert_eq!(
            service.lock_state().unwrap().roots[&root_id].source,
            RootSource::Remembered
        );
        assert!(matches!(
            read_roots_config(&app_data.join("roots.json")).unwrap(),
            ConfigRead::Valid { .. }
        ));
        assert!(fs::read_dir(&app_data).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("roots.corrupt-")
        }));
        let diagnostics = service.export_diagnostics().unwrap();
        assert!(diagnostics
            .entries
            .iter()
            .any(|entry| entry.contains("recovered from the previous durable configuration")));
    }

    #[test]
    fn malformed_roots_are_preserved_and_reported_without_a_previous_copy() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        fs::write(app_data.join("roots.json"), b"not json").unwrap();

        let service = ApplicationService::new(app_data.clone()).unwrap();

        assert!(!app_data.join("roots.json").exists());
        assert!(fs::read_dir(&app_data).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("roots.corrupt-")
        }));
        assert!(service
            .export_diagnostics()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.contains("configuration was malformed")));
    }

    #[test]
    fn failed_registration_keeps_the_previous_memory_and_disk_state() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let first = temp.path().join("first-saves");
        let second = temp.path().join("second-saves");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let service = ApplicationService::new(app_data.clone()).unwrap();
        service.register_root(&first.to_string_lossy()).unwrap();
        let original = fs::read(app_data.join("roots.json")).unwrap();
        fs::create_dir(app_data.join("roots.previous.json")).unwrap();

        assert!(service.register_root(&second.to_string_lossy()).is_err());

        let first_id = RootId::new(stable_id("root", &first.canonicalize().unwrap()));
        let second_id = RootId::new(stable_id("root", &second.canonicalize().unwrap()));
        let state = service.lock_state().unwrap();
        assert!(state.roots.contains_key(&first_id));
        assert!(!state.roots.contains_key(&second_id));
        drop(state);
        assert_eq!(fs::read(app_data.join("roots.json")).unwrap(), original);
    }

    #[test]
    fn successful_root_updates_keep_the_last_committed_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let first = temp.path().join("first-saves");
        let second = temp.path().join("second-saves");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let service = ApplicationService::new(app_data.clone()).unwrap();
        service.register_root(&first.to_string_lossy()).unwrap();
        service.register_root(&second.to_string_lossy()).unwrap();

        let current = match read_roots_config(&app_data.join("roots.json")).unwrap() {
            ConfigRead::Valid { persisted, .. } => persisted,
            _ => panic!("expected current roots configuration"),
        };
        let previous = match read_roots_config(&app_data.join("roots.previous.json")).unwrap() {
            ConfigRead::Valid { persisted, .. } => persisted,
            _ => panic!("expected previous roots configuration"),
        };
        assert_eq!(current.roots.len(), 2);
        assert_eq!(previous.roots.len(), 1);
        assert!(matches!(
            &previous.roots[0],
            PersistedRoot::Detailed { path, manual: true, .. }
                if path == &first.canonicalize().unwrap()
        ));
    }

    #[test]
    fn runtime_configuration_corruption_is_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let first = temp.path().join("first-saves");
        let second = temp.path().join("second-saves");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let service = ApplicationService::new(app_data.clone()).unwrap();
        service.register_root(&first.to_string_lossy()).unwrap();
        let malformed = b"externally corrupted roots";
        fs::write(app_data.join("roots.json"), malformed).unwrap();

        let error = service
            .register_root(&second.to_string_lossy())
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::ValidationFailed);
        let second_id = RootId::new(stable_id("root", &second.canonicalize().unwrap()));
        assert!(!service.lock_state().unwrap().roots.contains_key(&second_id));
        assert_eq!(fs::read(app_data.join("roots.json")).unwrap(), malformed);
    }

    #[test]
    fn failed_forget_keeps_the_root_in_memory_and_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let root = temp.path().join("manual-saves");
        fs::create_dir(&root).unwrap();
        let service = ApplicationService::new(app_data.clone()).unwrap();
        let registered = service.register_root(&root.to_string_lossy()).unwrap();
        let original = fs::read(app_data.join("roots.json")).unwrap();
        fs::create_dir(app_data.join("roots.previous.json")).unwrap();

        assert!(service.forget_root(&registered.root_id).is_err());

        assert!(service
            .lock_state()
            .unwrap()
            .roots
            .contains_key(&registered.root_id));
        assert_eq!(fs::read(app_data.join("roots.json")).unwrap(), original);
    }

    #[test]
    fn automatic_roots_are_not_persisted_listed_or_forgettable() {
        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let automatic_path = temp.path().join("automatic-saves");
        fs::create_dir(&automatic_path).unwrap();
        let automatic_path = automatic_path.canonicalize().unwrap();
        let automatic_id = RootId::new(stable_id("root", &automatic_path));
        let service = ApplicationService::new(app_data.clone()).unwrap();
        service.lock_state().unwrap().roots.insert(
            automatic_id.clone(),
            RootRecord {
                path: automatic_path,
                installation_root: None,
                source: RootSource::Automatic,
            },
        );
        {
            let state = service.lock_state().unwrap();
            service.persist_roots(&state.roots).unwrap();
        }

        let persisted = match read_roots_config(&app_data.join("roots.json")).unwrap() {
            ConfigRead::Valid { persisted, .. } => persisted,
            _ => panic!("expected a valid roots configuration"),
        };
        assert!(persisted.roots.is_empty());
        assert!(service
            .discover_installations()
            .unwrap()
            .registered_roots
            .iter()
            .all(|root| root.root_id != automatic_id));
        assert!(service.forget_root(&automatic_id).is_err());
        assert!(service
            .lock_state()
            .unwrap()
            .roots
            .contains_key(&automatic_id));
    }

    #[cfg(unix)]
    #[test]
    fn app_configuration_and_storage_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let first = temp.path().join("first-saves");
        let second = temp.path().join("second-saves");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let service = ApplicationService::new(app_data.clone()).unwrap();
        service.register_root(&first.to_string_lossy()).unwrap();
        service.register_root(&second.to_string_lossy()).unwrap();
        let backups = app_data.join("backups");
        let transactions = app_data.join("transactions");

        for directory in [
            app_data.as_path(),
            backups.as_path(),
            transactions.as_path(),
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for file in [
            app_data.join("roots.json"),
            app_data.join("roots.previous.json"),
        ] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_app_configuration_and_storage_permissions_are_hardened() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let app_data = temp.path().join("app-data");
        let backups = app_data.join("backups");
        let transactions = app_data.join("transactions");
        fs::create_dir_all(&backups).unwrap();
        fs::create_dir(&transactions).unwrap();
        let roots_payload = serde_json::to_vec(&PersistedRoots::default()).unwrap();
        let roots = app_data.join("roots.json");
        let previous = app_data.join("roots.previous.json");
        let old_backup = backups.join("old-backup.bin");
        let old_journal = transactions.join("old-journal.json");
        for file in [&roots, &previous] {
            fs::write(file, &roots_payload).unwrap();
        }
        fs::write(&old_backup, b"backup").unwrap();
        fs::write(&old_journal, b"journal").unwrap();
        for directory in [&app_data, &backups, &transactions] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for file in [&roots, &previous, &old_backup, &old_journal] {
            fs::set_permissions(file, fs::Permissions::from_mode(0o644)).unwrap();
        }

        let _service = ApplicationService::new(app_data.clone()).unwrap();

        for directory in [&app_data, &backups, &transactions] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for file in [&roots, &previous, &old_backup, &old_journal] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn scan_does_not_accept_a_symlinked_save_pair() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("saves");
        let save = root.join("save_Test_123");
        fs::create_dir_all(&save).unwrap();
        File::create(save.join("campaign.xml")).unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(save.join("campaign.xml"), save.join("descriptor.xml")).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(
            save.join("campaign.xml"),
            save.join("descriptor.xml"),
        )
        .is_err()
        {
            return;
        }

        assert!(!is_save_directory(&save));
    }
}
