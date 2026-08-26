//! The only adapter between Tauri IPC state and `save-core`.
//!
//! Keeping conversions and core-owned sessions here prevents XML, span patches, filesystem
//! handles, or transaction journals from crossing the frontend boundary.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::error::{CommandError, ErrorCode};
use crate::models::*;

const MAX_DIAGNOSTIC_ENTRIES: usize = 100;
const MAX_PORTRAIT_ENTRIES: usize = 2_048;
const MAX_PORTRAIT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SKILL_CATALOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SKILL_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MOD_DIRECTORIES: usize = 512;
const MAX_FACTION_ENTRIES: usize = 2_048;
const MAX_DATA_CATALOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DATA_CATALOG_ROWS: usize = 32_768;
const MAX_SHIP_SPEC_BYTES: u64 = 4 * 1024 * 1024;
// The desktop shell exposes one active save at a time. A second slot permits an
// already-open session to remain usable until a replacement has opened
// successfully, without letting repeated refreshes retain whole campaign
// object graphs indefinitely.
const MAX_OPEN_SESSIONS: usize = 2;
// At most one review is useful per session. The additional two slots cover
// recovery reviews, which are small and can always be prepared again.
const MAX_PENDING_REVIEWS: usize = MAX_OPEN_SESSIONS + 2;

pub struct CoreService {
    app_data_dir: PathBuf,
    backup_store: save_core::BackupStore,
    state: Mutex<CoreState>,
}

#[derive(Default)]
struct CoreState {
    sessions: HashMap<SessionId, Arc<SessionRecord>>,
    session_order: VecDeque<SessionId>,
    reviews: HashMap<ReviewId, ReviewRecord>,
    review_order: VecDeque<ReviewId>,
    recovery_tokens: HashMap<SessionId, RecoveryTarget>,
    diagnostics: VecDeque<String>,
}

struct SessionRecord {
    opened: save_core::OpenedSave,
    installation_root: Option<PathBuf>,
    snapshot: SaveSnapshot,
    portrait_files: HashMap<PortraitId, PathBuf>,
    skill_catalog: HashMap<String, ValidatedSkill>,
    faction_names: HashMap<String, String>,
    data_catalogs: LocalCatalogs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CatalogItemKind {
    Resources,
    Weapons,
    FighterWing,
    Special,
}

#[derive(Debug, Clone)]
struct ValidatedCatalogItem {
    name: String,
    /// Exact RC8 CargoStack space contribution. `None` keeps existing saved
    /// stacks recognizable while withholding construction authorization.
    cargo_space_per_unit: Option<f32>,
    local_resources_eligible: bool,
}

#[derive(Debug, Clone)]
struct ValidatedShipHull {
    name: String,
    hull_size: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LocalCatalogs {
    inventory: HashMap<(CatalogItemKind, String), ValidatedCatalogItem>,
    ships: HashMap<String, ValidatedShipHull>,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AddableItemKey {
    kind: CatalogItemKind,
    item_id: String,
    special_data: Option<String>,
}

#[derive(Debug, Clone)]
struct AddableItemDefinition {
    key: AddableItemKey,
    name: String,
    cargo_space_per_unit: f32,
    max_quantity: f32,
    local_resources_eligible: bool,
}

#[derive(Clone)]
struct ValidatedSkill {
    name: String,
    group: String,
    max_rank: u8,
    icon_id: Option<String>,
    player_allowed: bool,
    officer_allowed: bool,
}

#[derive(Clone, Copy)]
enum SkillOwner {
    Player,
    Officer,
}

#[derive(Debug)]
enum ReviewRecord {
    Edit {
        prepared: Box<save_core::PreparedReview>,
        session_id: SessionId,
        catalog_fingerprint: Option<String>,
        progression_requirements: ProgressionRequirements,
        acknowledgement_required: bool,
    },
    Restore {
        session_id: SessionId,
        backup_id: BackupId,
        acknowledgement_required: bool,
    },
    Recovery {
        save_id: String,
        backup_id: String,
        acknowledgement_required: bool,
    },
}

impl ReviewRecord {
    fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Edit { session_id, .. } | Self::Restore { session_id, .. } => Some(session_id),
            Self::Recovery { .. } => None,
        }
    }

    fn same_recovery_target(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (
                Self::Recovery {
                    save_id: left_save,
                    backup_id: left_backup,
                    ..
                },
                Self::Recovery {
                    save_id: right_save,
                    backup_id: right_backup,
                    ..
                }
            ) if left_save == right_save && left_backup == right_backup
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RecoveryTarget {
    save_id: String,
    backup_id: String,
}

impl CoreService {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, std::io::Error> {
        let backup_root = app_data_dir.join("backups");
        std::fs::create_dir_all(&backup_root)?;
        std::fs::create_dir_all(app_data_dir.join("transactions"))?;
        Ok(Self {
            app_data_dir,
            backup_store: save_core::BackupStore::new(backup_root),
            state: Mutex::new(CoreState::default()),
        })
    }

    pub fn inspect_save(
        &self,
        path: &Path,
        root_id: RootId,
        _save_id: SaveId,
    ) -> Result<SaveSummary, CommandError> {
        let summary = save_core::inspect_save_dir(
            path,
            save_core::ScanOptions {
                max_entries: 512,
                ..save_core::ScanOptions::default()
            },
        )?;
        Ok(summary_from_core(summary, root_id))
    }

    pub fn open_save(
        &self,
        path: &Path,
        installation_root: Option<&Path>,
        summary: SaveSummary,
    ) -> Result<SaveSnapshot, CommandError> {
        let opened = save_core::OpenedSave::open(
            save_core::SaveLocation::from_save_dir(path),
            save_core::OpenOptions::default(),
        )?;
        let session_id = SessionId::new(format!("session-{}", uuid::Uuid::new_v4()));
        let installation_root = installation_root.map(Path::to_path_buf);
        let (catalog, portrait_files, portrait_ids_by_path) = discover_portraits(
            installation_root.as_deref(),
            &opened.snapshot().metadata.enabled_mods,
        );
        let skill_catalog = discover_skills(
            installation_root.as_deref(),
            &opened.snapshot().metadata.enabled_mods,
        );
        let faction_names = discover_faction_names(
            installation_root.as_deref(),
            &opened.snapshot().metadata.enabled_mods,
        );
        let data_catalogs = discover_data_catalogs(
            installation_root.as_deref(),
            &opened.snapshot().metadata.enabled_mods,
        );
        let progression_settings_issues = progression_settings_issues(installation_root.as_deref());
        let snapshot = snapshot_from_core(
            session_id.clone(),
            summary,
            opened.snapshot(),
            catalog,
            progression_settings_issues,
            SnapshotCatalogs {
                portrait_ids_by_path: &portrait_ids_by_path,
                skills: &skill_catalog,
                faction_names: &faction_names,
                data: &data_catalogs,
            },
        );
        self.insert_session(
            &session_id,
            SessionRecord {
                opened,
                installation_root,
                snapshot: snapshot.clone(),
                portrait_files,
                skill_catalog,
                faction_names,
                data_catalogs,
            },
        )?;
        Ok(snapshot)
    }

    pub fn load_portrait(
        &self,
        session_id: &SessionId,
        portrait_id: &PortraitId,
    ) -> Result<PortraitPayload, CommandError> {
        let session = self.require_session(session_id)?;
        let path = session
            .portrait_files
            .get(portrait_id)
            .ok_or_else(|| CommandError::not_found("Portrait"))?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_PORTRAIT_BYTES
        {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "The portrait is not a permitted regular image file",
            ));
        }
        let bytes = fs::read(path)?;
        if !portrait_read_size_is_allowed(bytes.len()) {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "The portrait is not a permitted regular image file",
            ));
        }
        Ok(PortraitPayload {
            portrait_id: portrait_id.clone(),
            mime_type: portrait_mime(path)
                .ok_or_else(|| CommandError::not_found("Supported portrait"))?
                .into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    pub fn unlock_protected_save(
        &self,
        session_id: &SessionId,
        acknowledgement: bool,
    ) -> Result<SaveSnapshot, CommandError> {
        if !acknowledgement {
            return Err(CommandError::new(
                ErrorCode::ProtectedSave,
                "Unlocking an Iron Mode save requires explicit acknowledgement",
            ));
        }
        let session = self.require_session(session_id)?;
        if !session.snapshot.protected_locked {
            return Ok(session.snapshot.clone());
        }
        let backup = self.backup_store.backup_current(
            &session.opened.snapshot().save_id,
            session.opened.location().clone(),
            "protected-save unlock safety point",
            true,
        )?;
        let reopened = save_core::OpenedSave::open(
            session.opened.location().clone(),
            save_core::OpenOptions {
                allow_protected: true,
                ..save_core::OpenOptions::default()
            },
        )?;
        if reopened.snapshot().revision != backup.revision {
            return Err(CommandError::new(
                ErrorCode::StaleSave,
                "The save changed while the protected-save backup was created",
            )
            .disk_changed()
            .retryable());
        }
        let mut unlocked_summary = session.snapshot.summary.clone();
        unlocked_summary.compatibility = CompatibilityState::Editable;
        unlocked_summary.compatibility_reason = None;
        let data_catalogs = discover_data_catalogs(
            session.installation_root.as_deref(),
            &reopened.snapshot().metadata.enabled_mods,
        );
        let progression_settings_issues =
            progression_settings_issues(session.installation_root.as_deref());
        let snapshot = snapshot_from_core(
            session_id.clone(),
            unlocked_summary,
            reopened.snapshot(),
            session.snapshot.catalog.clone(),
            progression_settings_issues,
            SnapshotCatalogs {
                portrait_ids_by_path: &portrait_ids_by_path(&session.snapshot.catalog),
                skills: &session.skill_catalog,
                faction_names: &session.faction_names,
                data: &data_catalogs,
            },
        );
        self.insert_session(
            session_id,
            SessionRecord {
                opened: reopened,
                installation_root: session.installation_root.clone(),
                snapshot: snapshot.clone(),
                portrait_files: session.portrait_files.clone(),
                skill_catalog: session.skill_catalog.clone(),
                faction_names: session.faction_names.clone(),
                data_catalogs,
            },
        )?;
        self.record_diagnostic("a protected save was unlocked after a pinned backup");
        Ok(snapshot)
    }

    pub fn prepare_review(
        &self,
        session_id: &SessionId,
        edits: Vec<Edit>,
    ) -> Result<Review, CommandError> {
        let session = self.require_session(session_id)?;
        let progression_requirements = progression_requirements(&edits);
        if progression_requirements.any() {
            ensure_progression_edit_authorized(&session, progression_requirements)?;
        }
        let catalog_sensitive = edits.iter().any(|edit| {
            matches!(
                edit,
                Edit::SetInventoryQuantity { .. }
                    | Edit::SetStorageStackQuantity { .. }
                    | Edit::SetColonyResourceQuantity { .. }
                    | Edit::AddStorageItem { .. }
                    | Edit::AddColonyResource { .. }
            )
        });
        if catalog_sensitive {
            ensure_data_catalog_current(&session)?;
        }
        let core_edits = edits
            .into_iter()
            .map(|edit| {
                edit_to_core(
                    edit,
                    &session.snapshot,
                    &session.skill_catalog,
                    &session.data_catalogs,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let trusted_skill_ids: HashSet<String> = session
            .skill_catalog
            .iter()
            .filter(|(_, definition)| definition.player_allowed || definition.officer_allowed)
            .map(|(id, _)| id.clone())
            .collect();
        let mut trusted_stack_ids: HashSet<String> = session
            .snapshot
            .inventory
            .iter()
            .flat_map(|inventory| inventory.stacks.iter())
            .filter(|stack| stack.editable)
            .map(|stack| stack.id.0.clone())
            .collect();
        trusted_stack_ids.extend(
            session
                .snapshot
                .colonies
                .iter()
                .filter_map(|colony| colony.storage.as_ref())
                .flat_map(|storage| storage.stacks.iter())
                .filter(|stack| stack.editable)
                .map(|stack| stack.id.0.clone()),
        );
        trusted_stack_ids.extend(
            session
                .snapshot
                .colonies
                .iter()
                .filter_map(|colony| colony.local_resources.as_ref())
                .flat_map(|resources| resources.stacks.iter())
                .filter(|stack| stack.editable)
                .map(|stack| stack.id.0.clone()),
        );
        let trusted_additions = trusted_addition_specs(&session.data_catalogs);
        let prepared = session.opened.prepare_review_with_catalogs_and_additions(
            &core_edits,
            &trusted_skill_ids,
            &trusted_stack_ids,
            &trusted_additions,
        )?;
        let acknowledgement_required = prepared
            .summary()
            .warnings
            .iter()
            .any(|warning| warning.acknowledgement_required);
        let review = review_from_core(prepared.summary(), &session.snapshot);
        self.insert_review(
            review.review_id.clone(),
            ReviewRecord::Edit {
                prepared: Box::new(prepared),
                session_id: session_id.clone(),
                catalog_fingerprint: catalog_sensitive
                    .then(|| session.data_catalogs.fingerprint.clone()),
                progression_requirements,
                acknowledgement_required,
            },
        )?;
        Ok(review)
    }

    pub fn apply_review(
        &self,
        review_id: &ReviewId,
        mode: ApplyMode,
        acknowledgement: bool,
    ) -> Result<ApplyResult, CommandError> {
        let record = self.take_edit_review(review_id, acknowledgement)?;
        let ReviewRecord::Edit {
            prepared,
            session_id,
            catalog_fingerprint,
            progression_requirements,
            ..
        } = record
        else {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "A restore review cannot be applied as an edit review",
            ));
        };
        let session = self.require_session(&session_id)?;
        if progression_requirements.any() {
            ensure_progression_edit_authorized(&session, progression_requirements)?;
        }
        if let Some(expected) = catalog_fingerprint.as_deref() {
            ensure_data_catalog_matches(&session, expected)?;
        }
        let (outcome, message, invalidates_session) = match mode {
            ApplyMode::ReplaceOriginal => (
                self.backup_store.apply_replace(*prepared, false)?,
                "The save was backed up and updated successfully.".to_owned(),
                true,
            ),
            ApplyMode::SaveCopy { target_root } => {
                let target = validated_destination_root(&target_root)?;
                let display_name = session.snapshot.summary.character_name.clone();
                (
                    self.backup_store
                        .save_copy(*prepared, &target, &display_name)?,
                    "A new, independently named save copy was created.".to_owned(),
                    false,
                )
            }
        };
        if invalidates_session {
            self.remove_session(&session_id)?;
        }
        self.record_diagnostic("a single-use edit review was applied");
        apply_result_from_core(outcome, message)
    }

    pub fn list_backups(&self, save_id: &SaveId) -> Result<Vec<BackupSummary>, CommandError> {
        self.backup_store
            .list(&save_id.0)?
            .into_iter()
            .map(|backup| {
                let game_version = self.backup_game_version(&backup);
                backup_from_core(backup, &game_version)
            })
            .collect()
    }

    pub fn prepare_restore(
        &self,
        session_id: &SessionId,
        backup_id: &BackupId,
    ) -> Result<Review, CommandError> {
        let session = match self.require_session(session_id) {
            Ok(session) => session,
            Err(error) if error.code == ErrorCode::NotFound => {
                return self.prepare_pending_recovery(session_id, backup_id);
            }
            Err(error) => return Err(error),
        };
        authorize_restore(&session)?;
        let backup = self
            .backup_store
            .list(&session.opened.snapshot().save_id)?
            .into_iter()
            .find(|backup| backup.backup_id == backup_id.0)
            .ok_or_else(|| CommandError::not_found("Backup"))?;
        let pending = self.backup_store.pending_recoveries()?;
        let is_recovery = pending.iter().any(|record| {
            record.save_id == session.opened.snapshot().save_id && record.backup_id == backup_id.0
        });
        if !pending.is_empty() && !is_recovery {
            return Err(CommandError::new(
                ErrorCode::RecoveryRequired,
                "Restore the backup identified by the interrupted transaction before writing another save",
            ));
        }
        let review_id = ReviewId::new(format!("restore-{}", uuid::Uuid::new_v4()));
        let review = Review {
            review_id: review_id.clone(),
            revision: opaque_revision(&session.opened.snapshot().revision),
            changes: vec![ReviewChange {
                key: "save.restore".into(),
                section: ReviewSection::Save,
                label: "Restore backup".into(),
                before: opaque_revision(&session.opened.snapshot().revision),
                after: opaque_revision(&backup.revision),
                derived: None,
            }],
            warnings: vec![if is_recovery {
                "This restore resolves an interrupted transaction and first preserves the current live pair when it is readable.".into()
            } else {
                "Restoring creates a pinned safety backup of the current save first.".into()
            }],
            errors: Vec::new(),
            can_apply: true,
        };
        self.insert_review(
            review_id,
            ReviewRecord::Restore {
                session_id: session_id.clone(),
                backup_id: backup_id.clone(),
                acknowledgement_required: true,
            },
        )?;
        Ok(review)
    }

    pub fn apply_restore(
        &self,
        review_id: &ReviewId,
        acknowledgement: bool,
    ) -> Result<ApplyResult, CommandError> {
        let record = self.take_restore_review(review_id, acknowledgement)?;
        let (session_id, backup_id) = match record {
            ReviewRecord::Recovery {
                save_id, backup_id, ..
            } => {
                let outcome = self.backup_store.recover_pending(&save_id, &backup_id)?;
                self.lock_state()?
                    .recovery_tokens
                    .retain(|_, target| target.save_id != save_id || target.backup_id != backup_id);
                self.record_diagnostic(
                    "an interrupted transaction was recovered from its verified backup",
                );
                return apply_result_from_core(
                    outcome,
                    "The interrupted transaction was recovered from its verified external backup."
                        .into(),
                );
            }
            ReviewRecord::Restore {
                session_id,
                backup_id,
                ..
            } => (session_id, backup_id),
            ReviewRecord::Edit { .. } => {
                unreachable!("review kind was checked before removal")
            }
        };
        let session = self.require_session(&session_id)?;
        authorize_restore(&session)?;
        let pending = self.backup_store.pending_recoveries()?;
        let is_recovery = pending.iter().any(|record| {
            record.save_id == session.opened.snapshot().save_id && record.backup_id == backup_id.0
        });
        if !pending.is_empty() && !is_recovery {
            return Err(CommandError::new(
                ErrorCode::RecoveryRequired,
                "The selected backup does not resolve the interrupted transaction",
            ));
        }
        let allow_protected = session.opened.snapshot().capabilities.protected_save
            && !session.snapshot.protected_locked
            && session.opened.snapshot().capabilities.basic_character;
        let outcome = self.backup_store.restore_authorized(
            &session.opened.snapshot().save_id,
            &backup_id.0,
            session.opened.location().clone(),
            &session.opened.snapshot().revision,
            allow_protected,
        )?;
        self.remove_session(&session_id)?;
        self.record_diagnostic("a single-use restore review was applied");
        apply_result_from_core(
            outcome,
            "The backup was restored after preserving the previous live pair.".into(),
        )
    }

    pub fn startup_recovery_state(&self) -> Result<RecoveryState, CommandError> {
        let recoveries = self.backup_store.pending_recoveries()?;
        let status = if recoveries.is_empty() {
            RecoveryStatus::Clear
        } else {
            RecoveryStatus::RecoveryRequired
        };
        let targets: Vec<(RecoveryTarget, String)> = recoveries
            .into_iter()
            .map(|record| {
                (
                    RecoveryTarget {
                        save_id: record.save_id,
                        backup_id: record.backup_id,
                    },
                    recovery_phase_label(&record.phase).to_owned(),
                )
            })
            .collect();
        let mut state = self.lock_state()?;
        state
            .recovery_tokens
            .retain(|_, target| targets.iter().any(|(pending, _)| pending == target));
        let mut items = Vec::with_capacity(targets.len());
        for (target, phase) in targets {
            let token = state
                .recovery_tokens
                .iter()
                .find_map(|(token, existing)| (existing == &target).then(|| token.clone()))
                .unwrap_or_else(|| {
                    let token =
                        SessionId::new(format!("recovery-{}", uuid::Uuid::new_v4().simple()));
                    state.recovery_tokens.insert(token.clone(), target.clone());
                    token
                });
            items.push(RecoveryItem {
                // This opaque token is accepted as both arguments to
                // prepare_restore; filesystem paths and journal IDs remain private.
                transaction_id: token.0,
                save_id: Some(SaveId::new(target.save_id)),
                summary: "An interrupted save replacement requires recovery.".into(),
                last_completed_phase: phase,
            });
        }
        Ok(RecoveryState { status, items })
    }

    pub fn export_diagnostics(&self) -> Result<Diagnostics, CommandError> {
        let entries = self.lock_state()?.diagnostics.iter().cloned().collect();
        Ok(Diagnostics {
            app_version: env!("CARGO_PKG_VERSION").into(),
            os: std::env::consts::OS.into(),
            entries,
        })
    }

    /// Release an opened save and every review derived from it.
    ///
    /// This is intentionally idempotent so frontend cleanup can race with a
    /// successful apply, which already invalidates replace/restore sessions.
    pub fn close_session(&self, session_id: &SessionId) -> Result<(), CommandError> {
        if self.remove_session(session_id)? {
            self.record_diagnostic("an abandoned save session was released");
        }
        Ok(())
    }

    /// Release a prepared review without applying it. Missing reviews are
    /// already discarded or consumed and therefore count as success.
    pub fn discard_review(&self, review_id: &ReviewId) -> Result<(), CommandError> {
        let removed = {
            let mut state = self.lock_state()?;
            Self::remove_review_locked(&mut state, review_id).is_some()
        };
        if removed {
            self.record_diagnostic("an abandoned review was released");
        }
        Ok(())
    }

    pub fn record_diagnostic(&self, message: &str) {
        if let Ok(mut state) = self.state.lock() {
            if state.diagnostics.len() == MAX_DIAGNOSTIC_ENTRIES {
                state.diagnostics.pop_front();
            }
            state.diagnostics.push_back(message.to_owned());
        }
    }

    fn insert_session(
        &self,
        session_id: &SessionId,
        session: SessionRecord,
    ) -> Result<(), CommandError> {
        let mut state = self.lock_state()?;
        // Replacing a protected-save session invalidates anything derived from
        // its previous authorization state.
        Self::remove_session_locked(&mut state, session_id);
        state.sessions.insert(session_id.clone(), Arc::new(session));
        Self::touch_session_locked(&mut state, session_id);

        while state.sessions.len() > MAX_OPEN_SESSIONS {
            let Some(oldest) = state.session_order.pop_front() else {
                break;
            };
            if state.sessions.remove(&oldest).is_some() {
                Self::remove_reviews_for_session_locked(&mut state, &oldest);
            }
        }
        Ok(())
    }

    fn remove_session(&self, session_id: &SessionId) -> Result<bool, CommandError> {
        let mut state = self.lock_state()?;
        Ok(Self::remove_session_locked(&mut state, session_id))
    }

    fn remove_session_locked(state: &mut CoreState, session_id: &SessionId) -> bool {
        state
            .session_order
            .retain(|candidate| candidate != session_id);
        let removed = state.sessions.remove(session_id).is_some();
        Self::remove_reviews_for_session_locked(state, session_id);
        removed
    }

    fn touch_session_locked(state: &mut CoreState, session_id: &SessionId) {
        state
            .session_order
            .retain(|candidate| candidate != session_id);
        state.session_order.push_back(session_id.clone());
    }

    fn require_session(&self, session_id: &SessionId) -> Result<Arc<SessionRecord>, CommandError> {
        let mut state = self.lock_state()?;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| CommandError::not_found("Save session"))?;
        Self::touch_session_locked(&mut state, session_id);
        Ok(session)
    }

    fn insert_review(&self, review_id: ReviewId, record: ReviewRecord) -> Result<(), CommandError> {
        let mut state = self.lock_state()?;
        if let Some(session_id) = record.session_id() {
            if !state.sessions.contains_key(session_id) {
                return Err(CommandError::not_found("Save session"));
            }
        }

        // Only the newest immutable review for a session (or recovery target)
        // can be acted on. Superseded outputs are dropped immediately.
        let superseded: Vec<ReviewId> = state
            .reviews
            .iter()
            .filter_map(|(candidate_id, candidate)| {
                let same_session = record
                    .session_id()
                    .is_some_and(|session_id| candidate.session_id() == Some(session_id));
                (same_session || record.same_recovery_target(candidate))
                    .then(|| candidate_id.clone())
            })
            .collect();
        for candidate_id in superseded {
            Self::remove_review_locked(&mut state, &candidate_id);
        }

        Self::remove_review_locked(&mut state, &review_id);
        state.reviews.insert(review_id.clone(), record);
        state.review_order.push_back(review_id);
        while state.reviews.len() > MAX_PENDING_REVIEWS {
            let Some(oldest) = state.review_order.pop_front() else {
                break;
            };
            state.reviews.remove(&oldest);
        }
        Ok(())
    }

    fn remove_reviews_for_session_locked(state: &mut CoreState, session_id: &SessionId) {
        let review_ids: Vec<ReviewId> = state
            .reviews
            .iter()
            .filter(|(_, record)| record.session_id() == Some(session_id))
            .map(|(review_id, _)| review_id.clone())
            .collect();
        for review_id in review_ids {
            Self::remove_review_locked(state, &review_id);
        }
    }

    fn remove_review_locked(state: &mut CoreState, review_id: &ReviewId) -> Option<ReviewRecord> {
        state
            .review_order
            .retain(|candidate| candidate != review_id);
        state.reviews.remove(review_id)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, CoreState>, CommandError> {
        self.state
            .lock()
            .map_err(|_| CommandError::internal("Save-core session state is unavailable"))
    }

    fn take_edit_review(
        &self,
        review_id: &ReviewId,
        acknowledgement: bool,
    ) -> Result<ReviewRecord, CommandError> {
        self.take_review_kind(review_id, acknowledgement, false)
    }

    fn take_restore_review(
        &self,
        review_id: &ReviewId,
        acknowledgement: bool,
    ) -> Result<ReviewRecord, CommandError> {
        self.take_review_kind(review_id, acknowledgement, true)
    }

    fn take_review_kind(
        &self,
        review_id: &ReviewId,
        acknowledgement: bool,
        expect_restore: bool,
    ) -> Result<ReviewRecord, CommandError> {
        let mut state = self.lock_state()?;
        let record = state.reviews.get(review_id).ok_or_else(|| {
            CommandError::new(
                ErrorCode::ReviewConsumed,
                "The review was not found or has already been consumed",
            )
        })?;
        let (is_restore, acknowledgement_required) = match record {
            ReviewRecord::Edit {
                acknowledgement_required,
                ..
            } => (false, *acknowledgement_required),
            ReviewRecord::Restore {
                acknowledgement_required,
                ..
            } => (true, *acknowledgement_required),
            ReviewRecord::Recovery {
                acknowledgement_required,
                ..
            } => (true, *acknowledgement_required),
        };
        if is_restore != expect_restore {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                if expect_restore {
                    "An edit review cannot be applied as a restore review"
                } else {
                    "A restore review cannot be applied as an edit review"
                },
            ));
        }
        if acknowledgement_required && !acknowledgement {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Review warnings must be explicitly acknowledged before apply",
            ));
        }
        Self::remove_review_locked(&mut state, review_id).ok_or_else(|| {
            CommandError::new(
                ErrorCode::ReviewConsumed,
                "The review was not found or has already been consumed",
            )
        })
    }

    fn prepare_pending_recovery(
        &self,
        recovery_token: &SessionId,
        repeated_token: &BackupId,
    ) -> Result<Review, CommandError> {
        if recovery_token.0 != repeated_token.0 {
            return Err(CommandError::not_found("Save session"));
        }
        let target = self
            .lock_state()?
            .recovery_tokens
            .get(recovery_token)
            .cloned()
            .ok_or_else(|| CommandError::not_found("Recovery transaction"))?;
        let still_pending = self
            .backup_store
            .pending_recoveries()?
            .iter()
            .any(|record| record.save_id == target.save_id && record.backup_id == target.backup_id);
        if !still_pending {
            self.lock_state()?.recovery_tokens.remove(recovery_token);
            return Err(CommandError::new(
                ErrorCode::ReviewConsumed,
                "The recovery transaction is no longer pending",
            ));
        }
        let review_id = ReviewId::new(format!("recovery-review-{}", uuid::Uuid::new_v4()));
        let review = Review {
            review_id: review_id.clone(),
            revision: "pending-recovery".into(),
            changes: vec![ReviewChange {
                key: "save.recovery".into(),
                section: ReviewSection::Save,
                label: "Recover interrupted transaction".into(),
                before: "Interrupted live save pair".into(),
                after: "Last verified external backup".into(),
                derived: None,
            }],
            warnings: vec![
                "Recovery replaces the interrupted live pair after preserving its current raw bytes as an emergency backup."
                    .into(),
            ],
            errors: Vec::new(),
            can_apply: true,
        };
        self.insert_review(
            review_id,
            ReviewRecord::Recovery {
                save_id: target.save_id,
                backup_id: target.backup_id,
                acknowledgement_required: true,
            },
        )?;
        Ok(review)
    }

    fn backup_game_version(&self, backup: &save_core::BackupSummary) -> String {
        let unknown = || "Unknown".to_owned();
        if !valid_backup_component(&backup.save_id)
            || !valid_backup_component(&backup.backup_id)
            || backup.revision.descriptor.byte_len.get() > 4 * 1024 * 1024
        {
            return unknown();
        }
        let path = self
            .app_data_dir
            .join("backups")
            .join(&backup.save_id)
            .join(&backup.backup_id)
            .join("descriptor.xml");
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return unknown();
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != backup.revision.descriptor.byte_len.get()
        {
            return unknown();
        }
        let Ok(bytes) = fs::read(path) else {
            return unknown();
        };
        if hex::encode(Sha256::digest(&bytes)) != backup.revision.descriptor.sha256 {
            return unknown();
        }
        save_core::parse_descriptor(
            bytes,
            save_core::XmlLimits {
                max_bytes: 4 * 1024 * 1024,
                max_elements: 100_000,
                ..save_core::XmlLimits::default()
            },
        )
        .map(|descriptor| descriptor.metadata.game_version)
        .unwrap_or_else(|_| unknown())
    }
}

fn valid_backup_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn recovery_phase_label(phase: &str) -> &'static str {
    match phase {
        "prepared" | "prepared_restore" => "backup complete; replacement not confirmed",
        "campaign_replaced" => "campaign replaced; descriptor not confirmed",
        "descriptor_replaced" => "both files replaced; validation not confirmed",
        "recovery_prepared" => "recovery backup complete; replacement not confirmed",
        "recovery_campaign_replaced" => "recovery campaign replaced; descriptor not confirmed",
        "recovery_descriptor_replaced" => "recovery files replaced; validation not confirmed",
        _ => "interrupted transaction requires verified recovery",
    }
}

const CUSTOM_PROGRESSION_SETTINGS_REASON: &str =
    "Progression simulation is disabled because the associated installation uses customized progression settings. Restore the vanilla RC8 progression values or make XP changes in-game.";
const UNVERIFIED_PROGRESSION_SETTINGS_REASON: &str =
    "Progression simulation is disabled because the associated installation's progression settings could not be verified as vanilla RC8.";
const UNASSOCIATED_PROGRESSION_SETTINGS_REASON: &str =
    "Progression simulation is disabled because this save is not uniquely associated with a verified Starsector installation.";

#[derive(Debug, Clone, Copy, Default)]
struct ProgressionSettingsIssues {
    player: Option<&'static str>,
    officer: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ProgressionRequirements {
    player: bool,
    officer: bool,
}

impl ProgressionRequirements {
    const fn any(self) -> bool {
        self.player || self.officer
    }
}

fn progression_settings_issues(installation_root: Option<&Path>) -> ProgressionSettingsIssues {
    let Some(installation_root) = installation_root else {
        return ProgressionSettingsIssues {
            player: Some(UNASSOCIATED_PROGRESSION_SETTINGS_REASON),
            officer: Some(UNASSOCIATED_PROGRESSION_SETTINGS_REASON),
        };
    };
    match crate::game_settings::progression_settings_compatibility_rc8(installation_root) {
        Ok(compatibility) => ProgressionSettingsIssues {
            player: (!compatibility.player).then_some(CUSTOM_PROGRESSION_SETTINGS_REASON),
            officer: (!compatibility.officer).then_some(CUSTOM_PROGRESSION_SETTINGS_REASON),
        },
        Err(_) => ProgressionSettingsIssues {
            player: Some(UNVERIFIED_PROGRESSION_SETTINGS_REASON),
            officer: Some(UNVERIFIED_PROGRESSION_SETTINGS_REASON),
        },
    }
}

fn progression_requirements(edits: &[Edit]) -> ProgressionRequirements {
    let mut requirements = ProgressionRequirements::default();
    for edit in edits {
        match edit {
            Edit::GrantPlayerXp { .. } | Edit::RaisePlayerToLevel { .. } => {
                requirements.player = true;
            }
            Edit::GrantOfficerXp { .. } | Edit::RaiseOfficerToLevel { .. } => {
                requirements.officer = true;
            }
            _ => {}
        }
    }
    requirements
}

fn ensure_progression_edit_authorized(
    session: &SessionRecord,
    requirements: ProgressionRequirements,
) -> Result<(), CommandError> {
    let core = session.opened.snapshot();
    if !core.capabilities.progression {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            session
                .snapshot
                .progression_capability
                .reason
                .clone()
                .unwrap_or_else(|| "Progression editing is unavailable for this save.".into()),
        ));
    }
    if requirements.officer && !core.capabilities.officers {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Officer progression editing is unavailable for this save.",
        ));
    }
    let issues = progression_settings_issues(session.installation_root.as_deref());
    if let Some(reason) = requirements
        .player
        .then_some(issues.player)
        .flatten()
        .or_else(|| requirements.officer.then_some(issues.officer).flatten())
    {
        return Err(CommandError::new(ErrorCode::ValidationFailed, reason));
    }
    Ok(())
}

fn authorize_restore(session: &SessionRecord) -> Result<(), CommandError> {
    let metadata = &session.opened.snapshot().metadata;
    if metadata.compressed {
        return Err(CommandError::new(
            ErrorCode::UnsupportedCompression,
            "Compressed saves are read-only",
        ));
    }
    if metadata.game_version != save_core::SUPPORTED_GAME_VERSION
        || metadata.save_format != save_core::SUPPORTED_SAVE_FORMAT
    {
        return Err(CommandError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "Writing requires {} / save format {}",
                save_core::SUPPORTED_GAME_VERSION,
                save_core::SUPPORTED_SAVE_FORMAT
            ),
        ));
    }
    if session.snapshot.protected_locked {
        return Err(CommandError::new(
            ErrorCode::ProtectedSave,
            "Unlock the protected save for this session before restoring it",
        ));
    }
    if !session.snapshot.write_capability.editable
        || !session.opened.snapshot().capabilities.basic_character
    {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "This save session is not authorized for writes",
        ));
    }
    Ok(())
}

struct SnapshotCatalogs<'a> {
    portrait_ids_by_path: &'a HashMap<String, PortraitId>,
    skills: &'a HashMap<String, ValidatedSkill>,
    faction_names: &'a HashMap<String, String>,
    data: &'a LocalCatalogs,
}

fn snapshot_from_core(
    session_id: SessionId,
    summary: SaveSummary,
    core: &save_core::SaveSnapshot,
    catalog: CatalogView,
    progression_settings_issues: ProgressionSettingsIssues,
    catalogs: SnapshotCatalogs<'_>,
) -> SaveSnapshot {
    let mut catalog = catalog;
    catalog.addable_items = addable_item_views(catalogs.data);
    let capabilities = &core.capabilities;
    let write_reason = capabilities
        .reason
        .clone()
        .or_else(|| (!capabilities.basic_character).then(|| "This save is read-only".into()));
    let player_progression_editable =
        capabilities.progression && progression_settings_issues.player.is_none();
    let player_progression_reason = (!player_progression_editable).then(|| {
        if !core.metadata.enabled_mods.is_empty() {
            "Progression simulation is disabled when mods are enabled.".into()
        } else if let Some(reason) = progression_settings_issues.player {
            reason.into()
        } else {
            capabilities
                .reason
                .clone()
                .unwrap_or_else(|| "Progression editing is unavailable.".into())
        }
    });
    let officer_progression_editable = capabilities.progression
        && capabilities.officers
        && progression_settings_issues.officer.is_none();
    let officer_progression_reason = (!officer_progression_editable).then(|| {
        if !core.metadata.enabled_mods.is_empty() {
            "Progression simulation is disabled when mods are enabled.".into()
        } else if !capabilities.officers {
            "Officer progression editing is unavailable for this save.".into()
        } else if let Some(reason) = progression_settings_issues.officer {
            reason.into()
        } else {
            capabilities
                .reason
                .clone()
                .unwrap_or_else(|| "Officer progression editing is unavailable.".into())
        }
    });
    SaveSnapshot {
        session_id,
        save_id: summary.id.clone(),
        revision: opaque_revision(&core.revision),
        summary,
        protected_locked: capabilities.protected_save && !capabilities.basic_character,
        write_capability: FieldCapability {
            editable: capabilities.basic_character,
            reason: write_reason,
        },
        progression_capability: FieldCapability {
            editable: player_progression_editable,
            reason: player_progression_reason,
        },
        character: CharacterView {
            first_name: core.character.first_name.clone(),
            last_name: core.character.last_name.clone(),
            portrait_id: catalogs
                .portrait_ids_by_path
                .get(&normalize_asset_path(&core.character.portrait))
                .cloned(),
            portrait_path: core.character.portrait.clone(),
            credits: core.character.credits.to_string(),
            level: core.character.progression.level,
            xp: core.character.progression.xp.to_string(),
            skill_points: core.character.progression.skill_points,
            story_points: core.character.progression.story_points,
            skills: skill_views(
                &core.character.skills,
                capabilities.skills,
                catalogs.skills,
                SkillOwner::Player,
            ),
        },
        relations: core
            .reputation
            .iter()
            .map(|relation| RelationView {
                faction_id: relation.faction_id.clone(),
                display_name: catalogs
                    .faction_names
                    .get(&relation.faction_id)
                    .cloned()
                    .unwrap_or_else(|| humanize_id(&relation.faction_id)),
                value: relation.value_percent,
                editable: capabilities.reputation,
                reason: (!capabilities.reputation)
                    .then(|| "Reputation editing is unavailable for this save.".into()),
            })
            .collect(),
        officers: core
            .officers
            .iter()
            .map(|officer| OfficerView {
                id: OfficerId::new(officer.officer_id.clone()),
                name: [officer.first_name.as_str(), officer.last_name.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
                portrait_path: Some(officer.portrait.clone()),
                personality: humanize_id(&officer.personality),
                assignment: officer.assigned.then(|| "Assigned to player fleet".into()),
                level: officer.progression.level,
                xp: officer.progression.xp.to_string(),
                skill_points: officer.progression.skill_points,
                skills: skill_views(
                    &officer.skills,
                    capabilities.officers,
                    catalogs.skills,
                    SkillOwner::Officer,
                ),
                progression_editable: officer_progression_editable,
                progression_reason: officer_progression_reason.clone(),
            })
            .collect(),
        inventory: Some(inventory_view_from_core(
            &core.inventory,
            capabilities.inventory,
            catalogs.data,
        )),
        colonies: core
            .colonies
            .iter()
            .map(|colony| colony_view_from_core(colony, capabilities, catalogs.data))
            .collect(),
        catalog,
        warnings: core
            .warnings
            .iter()
            .map(|warning| warning.message.clone())
            .collect(),
    }
}

fn inventory_view_from_core(
    saved: &save_core::InventoryView,
    capability: bool,
    catalogs: &LocalCatalogs,
) -> InventoryView {
    let stacks = saved
        .stacks
        .iter()
        .map(|stack| {
            let presentation = inventory_stack_presentation(stack, capability, catalogs);
            InventoryStackView {
                id: InventoryStackId::new(stack.stack_id.clone()),
                item_id: stack.item_id.clone(),
                special_data: stack.special_data.clone(),
                name: presentation.name,
                kind: presentation.kind,
                quantity: stack.quantity.to_string(),
                max_quantity: stack.max_quantity.to_string(),
                cargo_space_per_unit: stack.cargo_space_per_unit.to_string(),
                editable: presentation.editable,
                reason: presentation.reason,
            }
        })
        .collect();
    InventoryView {
        stacks,
        used_space: saved.used_space.to_string(),
        max_space: saved.max_space.map(|value| value.to_string()),
        overloaded: saved
            .max_space
            .is_some_and(|maximum| saved.used_space > maximum),
        editable: capability,
        reason: (!capability).then(|| "Inventory editing is unavailable for this save.".into()),
    }
}

fn storage_view_from_core(
    saved: &save_core::InventoryView,
    capability: bool,
    catalogs: &LocalCatalogs,
) -> StorageView {
    let stacks = saved
        .stacks
        .iter()
        .map(|stack| {
            let presentation = inventory_stack_presentation(stack, capability, catalogs);
            StorageStackView {
                id: StorageStackId::new(stack.stack_id.clone()),
                item_id: stack.item_id.clone(),
                special_data: stack.special_data.clone(),
                name: presentation.name,
                kind: presentation.kind,
                quantity: stack.quantity.to_string(),
                max_quantity: stack.max_quantity.to_string(),
                cargo_space_per_unit: stack.cargo_space_per_unit.to_string(),
                editable: presentation.editable,
                reason: presentation.reason,
            }
        })
        .collect();
    StorageView {
        stacks,
        used_space: saved.used_space.to_string(),
        max_space: saved.max_space.map(|value| value.to_string()),
        overloaded: saved
            .max_space
            .is_some_and(|maximum| saved.used_space > maximum),
        editable: capability,
        reason: (!capability)
            .then(|| "Colony storage editing is unavailable for this save.".into()),
    }
}

fn colony_resources_view_from_core(
    saved: &save_core::InventoryView,
    capability: bool,
    catalogs: &LocalCatalogs,
) -> ColonyResourcesView {
    let stacks = saved
        .stacks
        .iter()
        .map(|stack| {
            let presentation = inventory_stack_presentation(stack, capability, catalogs);
            ColonyResourceStackView {
                id: ColonyResourceStackId::new(stack.stack_id.clone()),
                item_id: stack.item_id.clone(),
                special_data: stack.special_data.clone(),
                name: presentation.name,
                kind: presentation.kind,
                quantity: stack.quantity.to_string(),
                max_quantity: stack.max_quantity.to_string(),
                cargo_space_per_unit: stack.cargo_space_per_unit.to_string(),
                editable: presentation.editable,
                reason: presentation.reason,
            }
        })
        .collect();
    ColonyResourcesView {
        stacks,
        used_space: saved.used_space.to_string(),
        // The RC8 plugin does not enforce CargoData.mC as a stockpile cap.
        max_space: None,
        overloaded: false,
        editable: capability,
        reason: (!capability)
            .then(|| "Colony Local Resources editing is unavailable for this save.".into()),
    }
}

struct StackPresentation {
    name: String,
    kind: InventoryKind,
    editable: bool,
    reason: Option<String>,
}

fn inventory_stack_presentation(
    stack: &save_core::InventoryStack,
    capability: bool,
    catalogs: &LocalCatalogs,
) -> StackPresentation {
    let (kind, catalog_kind) = inventory_kind_from_core(stack.kind);
    let definition = catalog_kind.and_then(|catalog_kind| {
        catalogs
            .inventory
            .get(&(catalog_kind, stack.item_id.clone()))
    });
    let editable = capability && stack.structurally_editable && definition.is_some();
    let reason = (!editable).then(|| {
        if !capability {
            "Inventory editing is unavailable for this save.".into()
        } else if !stack.structurally_editable {
            stack
                .reason
                .clone()
                .unwrap_or_else(|| "This saved stack has an unsupported structure.".into())
        } else {
            "No unique trusted local catalog definition exists for this item.".into()
        }
    });
    StackPresentation {
        name: definition
            .map(|definition| definition.name.clone())
            .unwrap_or_else(|| humanize_id(&stack.item_id)),
        kind,
        editable,
        reason,
    }
}

fn inventory_kind_from_core(
    kind: save_core::InventoryKind,
) -> (InventoryKind, Option<CatalogItemKind>) {
    match kind {
        save_core::InventoryKind::Resources => {
            (InventoryKind::Resources, Some(CatalogItemKind::Resources))
        }
        save_core::InventoryKind::Weapons => {
            (InventoryKind::Weapons, Some(CatalogItemKind::Weapons))
        }
        save_core::InventoryKind::FighterChip => (
            InventoryKind::FighterWing,
            Some(CatalogItemKind::FighterWing),
        ),
        save_core::InventoryKind::Special => {
            (InventoryKind::Special, Some(CatalogItemKind::Special))
        }
        save_core::InventoryKind::Unknown => (InventoryKind::Unknown, None),
    }
}

fn colony_view_from_core(
    colony: &save_core::Colony,
    capabilities: &save_core::FieldCapabilities,
    catalogs: &LocalCatalogs,
) -> ColonyView {
    ColonyView {
        id: ColonyId::new(colony.colony_id.clone()),
        name: colony.name.clone(),
        location_context: colony.location_context.clone(),
        storage: colony
            .storage
            .as_ref()
            .map(|storage| storage_view_from_core(storage, capabilities.colony_storage, catalogs)),
        local_resources: colony.local_resources.as_ref().map(|resources| {
            colony_resources_view_from_core(resources, capabilities.colony_resources, catalogs)
        }),
        warnings: colony
            .warnings
            .iter()
            .map(|warning| warning.message.clone())
            .collect(),
    }
}

fn edit_to_core(
    edit: Edit,
    snapshot: &SaveSnapshot,
    skill_catalog: &HashMap<String, ValidatedSkill>,
    data_catalogs: &LocalCatalogs,
) -> Result<save_core::Edit, CommandError> {
    let edit = match edit {
        Edit::SetPlayerName {
            first_name,
            last_name,
        } => save_core::Edit::SetName {
            first_name,
            last_name,
        },
        Edit::SetPlayerPortrait { portrait_id } => {
            let relative = snapshot
                .catalog
                .portraits
                .iter()
                .find(|portrait| portrait.id == portrait_id)
                .map(|portrait| portrait.relative_path.clone())
                .ok_or_else(|| CommandError::invalid_argument("Unknown portrait selection"))?;
            save_core::Edit::SetPortrait {
                portrait_id: relative,
            }
        }
        Edit::SetCredits { value } => {
            let value = parse_finite_nonnegative_float(&value, "credits")?;
            save_core::Edit::SetCredits { value }
        }
        Edit::GrantPlayerXp { amount } => save_core::Edit::GrantPlayerXp {
            amount: save_core::DecimalU64::new(parse_decimal_u64(&amount, "XP amount")?),
        },
        Edit::RaisePlayerToLevel { level } => save_core::Edit::RaisePlayerToLevel { level },
        Edit::SetPlayerPoints {
            skill_points,
            story_points,
        } => save_core::Edit::SetPlayerPoints {
            skill_points,
            story_points,
        },
        Edit::SetPlayerSkill { skill_id, rank } => {
            validate_skill_view(
                snapshot
                    .character
                    .skills
                    .iter()
                    .find(|skill| skill.id == skill_id),
                rank,
            )?;
            validate_skill_edit(skill_catalog, &skill_id, rank, SkillOwner::Player)?;
            save_core::Edit::SetPlayerSkill {
                skill_id,
                rank: save_core::SkillRank::from_numeric(rank)?,
            }
        }
        Edit::SetRelation { faction_id, value } => save_core::Edit::SetFactionRelation {
            faction_id,
            value_percent: value,
        },
        Edit::GrantOfficerXp { officer_id, amount } => save_core::Edit::GrantOfficerXp {
            officer_id: officer_id.0,
            amount: save_core::DecimalU64::new(parse_decimal_u64(&amount, "XP amount")?),
        },
        Edit::RaiseOfficerToLevel { officer_id, level } => save_core::Edit::RaiseOfficerToLevel {
            officer_id: officer_id.0,
            level,
        },
        Edit::SetOfficerPoints {
            officer_id,
            skill_points,
        } => save_core::Edit::SetOfficerPoints {
            officer_id: officer_id.0,
            skill_points,
        },
        Edit::SetOfficerSkill {
            officer_id,
            skill_id,
            rank,
        } => {
            let officer = snapshot
                .officers
                .iter()
                .find(|officer| officer.id == officer_id)
                .ok_or_else(|| CommandError::not_found("Officer"))?;
            validate_skill_view(
                officer.skills.iter().find(|skill| skill.id == skill_id),
                rank,
            )?;
            validate_skill_edit(skill_catalog, &skill_id, rank, SkillOwner::Officer)?;
            save_core::Edit::SetOfficerSkill {
                officer_id: officer_id.0,
                skill_id,
                rank: save_core::SkillRank::from_numeric(rank)?,
            }
        }
        Edit::SetInventoryQuantity { stack_id, quantity } => {
            let quantity = parse_positive_finite_float(&quantity, "inventory quantity")?;
            let inventory = snapshot.inventory.as_ref().ok_or_else(|| {
                CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Player inventory is unavailable in this save session",
                )
            })?;
            let stack = inventory
                .stacks
                .iter()
                .find(|stack| stack.id == stack_id)
                .ok_or_else(|| CommandError::not_found("Inventory stack"))?;
            authorize_stack_quantity(
                stack.editable,
                stack.kind,
                &stack.max_quantity,
                quantity,
                "inventory stack",
            )?;
            save_core::Edit::SetInventoryStackQuantity {
                stack_id: stack_id.0,
                value: quantity,
            }
        }
        Edit::SetStorageStackQuantity {
            colony_id,
            stack_id,
            quantity,
        } => {
            let quantity = parse_positive_finite_float(&quantity, "storage quantity")?;
            let colony = snapshot
                .colonies
                .iter()
                .find(|colony| colony.id == colony_id)
                .ok_or_else(|| CommandError::not_found("Colony"))?;
            let storage = colony
                .storage
                .as_ref()
                .ok_or_else(|| CommandError::not_found("Colony storage"))?;
            let stack = storage
                .stacks
                .iter()
                .find(|stack| stack.id == stack_id)
                .ok_or_else(|| CommandError::not_found("Storage stack"))?;
            authorize_stack_quantity(
                stack.editable,
                stack.kind,
                &stack.max_quantity,
                quantity,
                "storage stack",
            )?;
            save_core::Edit::SetStorageStackQuantity {
                colony_id: colony_id.0,
                stack_id: stack_id.0,
                value: quantity,
            }
        }
        Edit::SetColonyResourceQuantity {
            colony_id,
            stack_id,
            quantity,
        } => {
            let quantity = parse_positive_finite_float(&quantity, "Local Resources quantity")?;
            let colony = snapshot
                .colonies
                .iter()
                .find(|colony| colony.id == colony_id)
                .ok_or_else(|| CommandError::not_found("Colony"))?;
            let resources = colony
                .local_resources
                .as_ref()
                .ok_or_else(|| CommandError::not_found("Colony Local Resources"))?;
            let stack = resources
                .stacks
                .iter()
                .find(|stack| stack.id == stack_id)
                .ok_or_else(|| CommandError::not_found("Local Resources stack"))?;
            if stack.kind != InventoryKind::Resources {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Local Resources edits are limited to commodity resource stacks",
                ));
            }
            authorize_stack_quantity(
                stack.editable,
                stack.kind,
                &stack.max_quantity,
                quantity,
                "Local Resources stack",
            )?;
            save_core::Edit::SetColonyResourceQuantity {
                colony_id: colony_id.0,
                stack_id: stack_id.0,
                value: quantity,
            }
        }
        Edit::AddStorageItem {
            colony_id,
            catalog_item_id,
            quantity,
        } => {
            let quantity = parse_positive_finite_float(&quantity, "item quantity")?;
            let definition = find_addable_item(data_catalogs, &catalog_item_id)
                .ok_or_else(|| CommandError::not_found("Catalog item"))?;
            validate_addition_quantity(&definition, quantity)?;
            let colony = snapshot
                .colonies
                .iter()
                .find(|colony| colony.id == colony_id)
                .ok_or_else(|| CommandError::not_found("Colony"))?;
            let storage = colony
                .storage
                .as_ref()
                .ok_or_else(|| CommandError::not_found("Colony storage"))?;
            if !storage.editable {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    storage
                        .reason
                        .clone()
                        .unwrap_or_else(|| "Colony storage is read-only".into()),
                ));
            }
            let existing_matches: Vec<_> = storage
                .stacks
                .iter()
                .filter(|stack| {
                    stack_matches_addable(
                        stack.kind,
                        &stack.item_id,
                        stack.special_data.as_deref(),
                        &definition.key,
                    )
                })
                .collect();
            if existing_matches.len() > 1 {
                return Err(CommandError::new(
                    ErrorCode::AmbiguousStructure,
                    "Multiple saved storage stacks match this catalog item",
                ));
            }
            if let Some(existing) = existing_matches.first().copied() {
                let current =
                    parse_positive_finite_float(&existing.quantity, "saved storage quantity")?;
                let next = checked_added_quantity(current, quantity, &existing.max_quantity)?;
                save_core::Edit::SetStorageStackQuantity {
                    colony_id: colony_id.0,
                    stack_id: existing.id.0.clone(),
                    value: next,
                }
            } else {
                save_core::Edit::AddStorageStack {
                    colony_id: colony_id.0,
                    item: addable_key_to_core(&definition.key),
                    quantity,
                }
            }
        }
        Edit::AddColonyResource {
            colony_id,
            catalog_item_id,
            quantity,
        } => {
            let quantity = parse_positive_finite_float(&quantity, "resource quantity")?;
            let definition = find_addable_item(data_catalogs, &catalog_item_id)
                .ok_or_else(|| CommandError::not_found("Catalog item"))?;
            if !definition.local_resources_eligible
                || definition.key.kind != CatalogItemKind::Resources
                || definition.key.special_data.is_some()
            {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Only recognized commodities may be added to Local Resources",
                ));
            }
            validate_addition_quantity(&definition, quantity)?;
            let colony = snapshot
                .colonies
                .iter()
                .find(|colony| colony.id == colony_id)
                .ok_or_else(|| CommandError::not_found("Colony"))?;
            let resources = colony
                .local_resources
                .as_ref()
                .ok_or_else(|| CommandError::not_found("Colony Local Resources"))?;
            if !resources.editable {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    resources
                        .reason
                        .clone()
                        .unwrap_or_else(|| "Colony Local Resources are read-only".into()),
                ));
            }
            let existing_matches: Vec<_> = resources
                .stacks
                .iter()
                .filter(|stack| {
                    stack_matches_addable(
                        stack.kind,
                        &stack.item_id,
                        stack.special_data.as_deref(),
                        &definition.key,
                    )
                })
                .collect();
            if existing_matches.len() > 1 {
                return Err(CommandError::new(
                    ErrorCode::AmbiguousStructure,
                    "Multiple Local Resources stacks match this commodity",
                ));
            }
            if let Some(existing) = existing_matches.first().copied() {
                let current = parse_positive_finite_float(
                    &existing.quantity,
                    "saved Local Resources quantity",
                )?;
                let next = checked_added_quantity(current, quantity, &existing.max_quantity)?;
                save_core::Edit::SetColonyResourceQuantity {
                    colony_id: colony_id.0,
                    stack_id: existing.id.0.clone(),
                    value: next,
                }
            } else {
                save_core::Edit::AddColonyResourceStack {
                    colony_id: colony_id.0,
                    commodity_id: definition.key.item_id,
                    quantity,
                }
            }
        }
    };
    Ok(edit)
}

fn addable_key_to_core(key: &AddableItemKey) -> save_core::CargoItemKey {
    save_core::CargoItemKey {
        kind: match key.kind {
            CatalogItemKind::Resources => save_core::InventoryKind::Resources,
            CatalogItemKind::Weapons => save_core::InventoryKind::Weapons,
            CatalogItemKind::FighterWing => save_core::InventoryKind::FighterChip,
            CatalogItemKind::Special => save_core::InventoryKind::Special,
        },
        item_id: key.item_id.clone(),
        special_data: key.special_data.clone(),
    }
}

fn stack_matches_addable(
    kind: InventoryKind,
    item_id: &str,
    special_data: Option<&str>,
    key: &AddableItemKey,
) -> bool {
    let expected = match key.kind {
        CatalogItemKind::Resources => InventoryKind::Resources,
        CatalogItemKind::Weapons => InventoryKind::Weapons,
        CatalogItemKind::FighterWing => InventoryKind::FighterWing,
        CatalogItemKind::Special => InventoryKind::Special,
    };
    kind == expected && item_id == key.item_id && special_data == key.special_data.as_deref()
}

fn validate_addition_quantity(
    definition: &AddableItemDefinition,
    quantity: f32,
) -> Result<(), CommandError> {
    if !quantity.is_finite() || quantity < 1.0 || quantity > definition.max_quantity {
        return Err(CommandError::invalid_argument(format!(
            "New stack quantities must be between 1 and {}",
            definition.max_quantity
        )));
    }
    if !matches!(definition.key.kind, CatalogItemKind::Resources) && quantity.fract() != 0.0 {
        return Err(CommandError::invalid_argument(
            "Weapons, fighter LPCs, and blueprints require whole quantities",
        ));
    }
    Ok(())
}

fn checked_added_quantity(current: f32, amount: f32, saved_max: &str) -> Result<f32, CommandError> {
    let saved_max = parse_positive_finite_float(saved_max, "saved stack maximum")?;
    let next = current + amount;
    if !next.is_finite() || next <= current || next > saved_max {
        return Err(CommandError::invalid_argument(format!(
            "The resulting quantity must not exceed the saved maximum {saved_max}"
        )));
    }
    Ok(next)
}

fn review_from_core(summary: &save_core::ReviewSummary, snapshot: &SaveSnapshot) -> Review {
    Review {
        review_id: ReviewId::new(summary.review_id.clone()),
        revision: opaque_revision(&summary.source_revision),
        changes: summary
            .changes
            .iter()
            .map(|change| ReviewChange {
                key: change.field.clone(),
                section: review_section(&change.field),
                label: review_label(&change.field, snapshot),
                before: change.old_value.clone(),
                after: change.new_value.clone(),
                derived: change.derived.then_some(true),
            })
            .collect(),
        warnings: summary
            .warnings
            .iter()
            .map(|warning| warning.message.clone())
            .collect(),
        errors: Vec::new(),
        can_apply: !summary.changes.is_empty(),
    }
}

fn review_label(field: &str, snapshot: &SaveSnapshot) -> String {
    cargo_addition_review_label(field, snapshot)
        .or_else(|| cargo_review_label(field, snapshot.inventory.as_ref(), &snapshot.colonies))
        .unwrap_or_else(|| humanize_id(&field.replace('.', "_")))
}

fn cargo_addition_review_label(field: &str, snapshot: &SaveSnapshot) -> Option<String> {
    for colony in &snapshot.colonies {
        for item in &snapshot.catalog.addable_items {
            let suffix = match item.kind {
                AddableItemKind::Commodity => format!("resources.{}", item.item_id),
                AddableItemKind::Weapon => format!("weapons.{}", item.item_id),
                AddableItemKind::FighterWing => format!("fighter_wing.{}", item.item_id),
                AddableItemKind::ShipBlueprint => format!("special.ship_bp.{}", item.item_id),
                AddableItemKind::WeaponBlueprint => {
                    format!("special.weapon_bp.{}", item.item_id)
                }
                AddableItemKind::FighterBlueprint => {
                    format!("special.fighter_bp.{}", item.item_id)
                }
            };
            if field == format!("colonies.{}.storage.add.{suffix}", colony.id.0) {
                return Some(format!(
                    "{} · Add {} [{}] to storage",
                    colony.name, item.name, item.item_id
                ));
            }
            if item.local_resources_eligible
                && field
                    == format!(
                        "colonies.{}.local_resources.add.resources.{}",
                        colony.id.0, item.item_id
                    )
            {
                return Some(format!(
                    "{} · Add {} [{}] to Local Resources",
                    colony.name, item.name, item.item_id
                ));
            }
        }
    }
    None
}

fn cargo_review_label(
    field: &str,
    inventory: Option<&InventoryView>,
    colonies: &[ColonyView],
) -> Option<String> {
    if field == "inventory.used_space" {
        return Some("Player cargo space".into());
    }
    if let Some(inventory) = inventory {
        if let Some(stack) = inventory
            .stacks
            .iter()
            .find(|stack| field == format!("inventory.{}.quantity", stack.id.0))
        {
            return Some(format!(
                "{} quantity",
                stack_review_name(&stack.name, &stack.item_id, stack.special_data.as_deref(),)
            ));
        }
    }
    for colony in colonies {
        let used_space_field = format!("colonies.{}.storage.used_space", colony.id.0);
        if field == used_space_field {
            return Some(format!("{} storage space", colony.name));
        }
        if let Some(storage) = &colony.storage {
            if let Some(stack) = storage.stacks.iter().find(|stack| {
                field == format!("colonies.{}.storage.{}.quantity", colony.id.0, stack.id.0)
            }) {
                return Some(format!(
                    "{} · {} quantity",
                    colony.name,
                    stack_review_name(&stack.name, &stack.item_id, stack.special_data.as_deref(),)
                ));
            }
        }
        let resources_used_space = format!("colonies.{}.local_resources.used_space", colony.id.0);
        if field == resources_used_space {
            return Some(format!("{} Local Resources stockpile size", colony.name));
        }
        if let Some(resources) = &colony.local_resources {
            if let Some(stack) = resources.stacks.iter().find(|stack| {
                field
                    == format!(
                        "colonies.{}.local_resources.{}.quantity",
                        colony.id.0, stack.id.0
                    )
            }) {
                return Some(format!(
                    "{} · {} local resource quantity",
                    colony.name,
                    stack_review_name(&stack.name, &stack.item_id, stack.special_data.as_deref(),)
                ));
            }
        }
    }
    None
}

fn stack_review_name(name: &str, item_id: &str, special_data: Option<&str>) -> String {
    match special_data {
        Some(data) => format!("{name} [{item_id}: {data}]"),
        None => format!("{name} [{item_id}]"),
    }
}

fn review_section(field: &str) -> ReviewSection {
    if field.starts_with("officer") {
        ReviewSection::Officers
    } else if field.starts_with("inventory") || field.starts_with("cargo") {
        ReviewSection::Inventory
    } else if field.starts_with("colony")
        || field.starts_with("colonies")
        || field.starts_with("storage")
    {
        ReviewSection::Colonies
    } else if field.starts_with("relation") || field.starts_with("reputation") {
        ReviewSection::Reputation
    } else if field.starts_with("player")
        || field.starts_with("character")
        || field.starts_with("credits")
    {
        ReviewSection::Character
    } else {
        ReviewSection::Save
    }
}

fn parse_decimal_u64(value: &str, label: &str) -> Result<u64, CommandError> {
    value.trim().parse().map_err(|_| {
        CommandError::invalid_argument(format!("{label} must be a nonnegative whole number"))
    })
}

fn parse_finite_nonnegative_float(value: &str, label: &str) -> Result<f32, CommandError> {
    let parsed: f32 = value
        .trim()
        .parse()
        .map_err(|_| CommandError::invalid_argument(format!("{label} must be a number")))?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(CommandError::invalid_argument(format!(
            "{label} must be finite and nonnegative"
        )));
    }
    Ok(parsed)
}

fn parse_positive_finite_float(value: &str, label: &str) -> Result<f32, CommandError> {
    let parsed = parse_finite_nonnegative_float(value, label)?;
    if parsed <= 0.0 {
        return Err(CommandError::invalid_argument(format!(
            "{label} must be greater than zero; removing stacks is not supported"
        )));
    }
    Ok(parsed)
}

fn authorize_stack_quantity(
    editable: bool,
    kind: InventoryKind,
    max_quantity: &str,
    quantity: f32,
    label: &str,
) -> Result<(), CommandError> {
    if !editable {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            format!("The selected {label} is read-only for this save session"),
        ));
    }
    let maximum = parse_finite_nonnegative_float(max_quantity, "saved stack maximum")?;
    if quantity > maximum {
        return Err(CommandError::invalid_argument(format!(
            "The {label} quantity exceeds its saved maximum"
        )));
    }
    if matches!(
        kind,
        InventoryKind::Weapons | InventoryKind::FighterWing | InventoryKind::Special
    ) && quantity.fract() != 0.0
    {
        return Err(CommandError::invalid_argument(format!(
            "The {label} quantity must be a whole number for this item type"
        )));
    }
    Ok(())
}

fn skill_views(
    saved: &[save_core::SkillState],
    capability: bool,
    catalog: &HashMap<String, ValidatedSkill>,
    owner: SkillOwner,
) -> Vec<SkillView> {
    let saved_by_id: HashMap<&str, &save_core::SkillState> = saved
        .iter()
        .map(|skill| (skill.id.as_str(), skill))
        .collect();
    let mut views: Vec<SkillView> = catalog
        .iter()
        .filter(|(_, definition)| skill_allowed(definition, owner))
        .map(|(id, definition)| {
            let rank = saved_by_id
                .get(id.as_str())
                .map_or(0, |skill| skill.rank.numeric());
            let rank_valid = rank <= definition.max_rank;
            let editable = capability && rank_valid;
            SkillView {
                id: id.clone(),
                name: definition.name.clone(),
                group: definition.group.clone(),
                rank,
                max_rank: definition.max_rank.max(rank),
                editable,
                reason: (!editable).then(|| {
                    if !rank_valid {
                        "Saved rank exceeds the validated local skill definition.".into()
                    } else {
                        "Skill editing is unavailable for this save.".into()
                    }
                }),
                icon_id: definition.icon_id.clone(),
            }
        })
        .collect();
    views.extend(
        saved
            .iter()
            .filter(|skill| {
                catalog
                    .get(&skill.id)
                    .is_none_or(|definition| !skill_allowed(definition, owner))
            })
            .map(|skill| {
                let definition = catalog.get(&skill.id);
                SkillView {
                    id: skill.id.clone(),
                    name: definition
                        .map(|definition| definition.name.clone())
                        .unwrap_or_else(|| skill.id.clone()),
                    group: definition
                        .map(|definition| definition.group.clone())
                        .unwrap_or_else(|| "Unknown mod".into()),
                    rank: skill.rank.numeric(),
                    max_rank: definition.map_or_else(
                        || skill.rank.numeric().max(1),
                        |definition| definition.max_rank.max(skill.rank.numeric()),
                    ),
                    editable: false,
                    reason: Some(if definition.is_some() {
                        "The local definition does not allow this skill for this character type."
                            .into()
                    } else {
                        "No trusted local skill definition.".into()
                    }),
                    icon_id: definition.and_then(|definition| definition.icon_id.clone()),
                }
            }),
    );
    views.sort_by(|left, right| {
        left.group
            .cmp(&right.group)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    views
}

fn opaque_revision(revision: &save_core::ContentRevision) -> String {
    let mut digest = Sha256::new();
    digest.update(revision.campaign.sha256.as_bytes());
    digest.update([0]);
    digest.update(revision.descriptor.sha256.as_bytes());
    format!("revision-{}", hex::encode(&digest.finalize()[..16]))
}

fn humanize_id(id: &str) -> String {
    id.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn humanize_enum_id(id: &str) -> String {
    humanize_id(&id.to_ascii_lowercase())
}

fn validate_skill_edit(
    catalog: &HashMap<String, ValidatedSkill>,
    skill_id: &str,
    rank: u8,
    owner: SkillOwner,
) -> Result<(), CommandError> {
    let definition = catalog.get(skill_id).ok_or_else(|| {
        CommandError::new(
            ErrorCode::ValidationFailed,
            "The skill is not present in the validated local catalog",
        )
    })?;
    if !skill_allowed(definition, owner) {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "The skill is not valid for this character type",
        ));
    }
    if rank > definition.max_rank {
        return Err(CommandError::invalid_argument(
            "The selected rank is not supported by the local skill definition",
        ));
    }
    Ok(())
}

fn validate_skill_view(skill: Option<&SkillView>, rank: u8) -> Result<(), CommandError> {
    let skill = skill.ok_or_else(|| {
        CommandError::new(
            ErrorCode::ValidationFailed,
            "The skill is not authorized by this save session",
        )
    })?;
    if !skill.editable {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "The skill is read-only for this save session",
        ));
    }
    if rank > skill.max_rank {
        return Err(CommandError::invalid_argument(
            "The selected rank exceeds the session-authorized maximum",
        ));
    }
    Ok(())
}

fn skill_allowed(definition: &ValidatedSkill, owner: SkillOwner) -> bool {
    match owner {
        SkillOwner::Player => definition.player_allowed,
        SkillOwner::Officer => definition.officer_allowed,
    }
}

fn csv_true(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn restricted_skill_tags(value: &str) -> bool {
    value.split(',').map(str::trim).any(|tag| {
        ["npc_only", "deprecated", "admin_only", "ai_core_only"]
            .into_iter()
            .any(|restricted| tag.eq_ignore_ascii_case(restricted))
    })
}

/// Returns the directory that contains Starsector's vanilla `data` and
/// `graphics` trees for the host's native distribution layout. Windows keeps
/// those assets below `starsector-core`; the Linux archive and the macOS app's
/// `Contents/Resources/Java` directory are themselves the asset root.
fn installation_asset_root(installation: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        installation.join("starsector-core")
    }
    #[cfg(not(windows))]
    {
        installation.to_path_buf()
    }
}

fn discover_skills(
    installation: Option<&Path>,
    enabled_mods: &[String],
) -> HashMap<String, ValidatedSkill> {
    let Some(installation) = installation else {
        return HashMap::new();
    };
    let mut merged: HashMap<String, Option<ValidatedSkill>> = HashMap::new();
    let base_root = installation_asset_root(installation);
    load_skill_directory(
        &base_root.join("data").join("characters").join("skills"),
        &base_root,
        &mut merged,
    );

    for mod_root in enabled_mod_roots(installation, enabled_mods) {
        load_skill_directory(
            &mod_root.join("data").join("characters").join("skills"),
            &mod_root,
            &mut merged,
        );
    }
    merged
        .into_iter()
        .filter_map(|(id, definition)| definition.map(|definition| (id, definition)))
        .collect()
}

fn load_skill_directory(
    directory: &Path,
    asset_root: &Path,
    merged: &mut HashMap<String, Option<ValidatedSkill>>,
) {
    let Ok(metadata) = fs::symlink_metadata(directory) else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return;
    }
    let (Ok(canonical_root), Ok(canonical_directory)) =
        (fs::canonicalize(asset_root), fs::canonicalize(directory))
    else {
        return;
    };
    if !canonical_directory.starts_with(canonical_root) {
        return;
    }
    let csv_path = directory.join("skill_data.csv");
    let Ok(metadata) = fs::symlink_metadata(&csv_path) else {
        return;
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_SKILL_CATALOG_BYTES
    {
        return;
    }
    let Ok(bytes) = fs::read(&csv_path) else {
        return;
    };
    if bytes.len() as u64 > MAX_SKILL_CATALOG_BYTES {
        return;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(decoded.as_bytes());
    let Ok(headers) = reader.headers().cloned() else {
        return;
    };
    let Ok(Some(id_index)) = unique_header_index(&headers, "id") else {
        return;
    };
    let Ok(Some(name_index)) = unique_header_index(&headers, "name") else {
        return;
    };
    let Ok(icon_index) = unique_header_index(&headers, "icon") else {
        return;
    };
    let Ok(officer_index) = unique_header_index(&headers, "combat officer") else {
        return;
    };
    let Ok(admin_index) = unique_header_index(&headers, "admin") else {
        return;
    };
    let Ok(tags_index) = unique_header_index(&headers, "tags") else {
        return;
    };
    let mut local: HashMap<String, Option<ValidatedSkill>> = HashMap::new();
    for (index, record) in reader.records().enumerate() {
        if index >= 4_096 {
            return;
        }
        let Ok(record) = record else { return };
        let Some(id) = record.get(id_index).map(str::trim) else {
            continue;
        };
        if !valid_catalog_id(id) {
            continue;
        }
        let skill_path = directory.join(format!("{id}.skill"));
        let Some(skill_text) = read_bounded_regular_text(&skill_path, MAX_SKILL_FILE_BYTES) else {
            continue;
        };
        let Some(skill_object) = parse_jsonish_object(&skill_text) else {
            continue;
        };
        if json_string(&skill_object, "id") != Some(id) {
            continue;
        }
        let Some(group) = json_string(&skill_object, "governingAptitude") else {
            continue;
        };
        if !valid_catalog_id(group) {
            continue;
        }
        let max_rank = match skill_object.get("elite") {
            None | Some(serde_json::Value::Bool(false)) => 1,
            Some(serde_json::Value::Bool(true)) => 2,
            Some(_) => continue,
        };
        let name = record
            .get(name_index)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 256)
            .map(str::to_owned)
            .unwrap_or_else(|| humanize_id(id));
        let icon_id = icon_index
            .and_then(|index| record.get(index))
            .map(str::trim)
            .filter(|value| valid_relative_asset_path(value))
            .map(normalize_asset_path);
        let restricted = id.starts_with("aptitude_")
            || admin_index
                .and_then(|index| record.get(index))
                .is_some_and(csv_true)
            || tags_index
                .and_then(|index| record.get(index))
                .is_some_and(restricted_skill_tags);
        let definition = ValidatedSkill {
            name,
            group: humanize_id(group),
            max_rank,
            icon_id,
            player_allowed: !restricted,
            officer_allowed: officer_index
                .and_then(|index| record.get(index))
                .is_some_and(csv_true)
                && !restricted,
        };
        match local.entry(id.to_owned()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(definition));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                // Multiple definitions are ambiguous and therefore read-only.
                entry.insert(None);
            }
        }
    }
    for (id, definition) in local {
        match merged.entry(id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(definition);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
}

fn unique_header_index(headers: &csv::StringRecord, expected: &str) -> Result<Option<usize>, ()> {
    let mut matches = headers
        .iter()
        .enumerate()
        .filter_map(|(index, header)| (header == expected).then_some(index));
    let first = matches.next();
    if matches.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

fn portrait_read_size_is_allowed(bytes_len: usize) -> bool {
    bytes_len <= MAX_PORTRAIT_BYTES as usize
}

fn read_jsonish_string_file(path: &Path, key: &str) -> Option<String> {
    let text = read_bounded_regular_text(path, MAX_SKILL_FILE_BYTES)?;
    parse_jsonish_object(&text).and_then(|object| json_string(&object, key).map(str::to_owned))
}

fn read_bounded_regular_text(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    Some(decoded.into_owned())
}

#[derive(Debug)]
struct UniqueJsonObject(HashMap<String, serde_json::Value>);

impl<'de> Deserialize<'de> for UniqueJsonObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueObjectVisitor;

        impl<'de> Visitor<'de> for UniqueObjectVisitor {
            type Value = UniqueJsonObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with unique top-level keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = HashMap::new();
                while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate top-level key"));
                    }
                }
                Ok(UniqueJsonObject(values))
            }
        }

        deserializer.deserialize_map(UniqueObjectVisitor)
    }
}

fn parse_jsonish_object(input: &str) -> Option<HashMap<String, serde_json::Value>> {
    let normalized = normalize_jsonish(input)?;
    let mut deserializer = serde_json::Deserializer::from_str(&normalized);
    let object = UniqueJsonObject::deserialize(&mut deserializer).ok()?;
    deserializer.end().ok()?;
    Some(object.0)
}

fn json_string<'a>(object: &'a HashMap<String, serde_json::Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// Starsector data files allow hash/C-style comments and trailing commas.
/// Normalize only those extensions, then use serde_json for complete structure
/// validation. Authorization values are never found with substring searches.
fn normalize_jsonish(input: &str) -> Option<String> {
    let mut without_comments = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if in_string {
            without_comments.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => {
                in_string = true;
                without_comments.push(character);
            }
            '#' => {
                for comment in characters.by_ref() {
                    if comment == '\n' {
                        without_comments.push('\n');
                        break;
                    }
                }
            }
            '/' if characters.peek() == Some(&'/') => {
                characters.next();
                for comment in characters.by_ref() {
                    if comment == '\n' {
                        without_comments.push('\n');
                        break;
                    }
                }
            }
            '/' if characters.peek() == Some(&'*') => {
                characters.next();
                let mut closed = false;
                let mut previous = '\0';
                for comment in characters.by_ref() {
                    if previous == '*' && comment == '/' {
                        closed = true;
                        break;
                    }
                    previous = comment;
                }
                if !closed {
                    return None;
                }
                without_comments.push(' ');
            }
            _ => without_comments.push(character),
        }
    }
    if in_string || escaped {
        return None;
    }

    let characters: Vec<char> = without_comments.chars().collect();
    let mut normalized = String::with_capacity(without_comments.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if in_string {
            normalized.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if character == '"' {
            in_string = true;
            normalized.push(character);
        } else if character == ',' {
            let mut next = index + 1;
            while next < characters.len() && characters[next].is_whitespace() {
                next += 1;
            }
            if next >= characters.len() || !matches!(characters[next], '}' | ']') {
                normalized.push(character);
            }
        } else {
            normalized.push(character);
        }
        index += 1;
    }
    Some(normalized)
}

fn valid_catalog_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_relative_asset_path(value: &str) -> bool {
    let normalized = normalize_asset_path(value);
    !normalized.is_empty()
        && !Path::new(&normalized).is_absolute()
        && !Path::new(&normalized)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn discover_data_catalogs(installation: Option<&Path>, enabled_mods: &[String]) -> LocalCatalogs {
    let Some(installation) = installation else {
        return LocalCatalogs::default();
    };
    let mut roots = vec![installation_asset_root(installation)];
    roots.extend(enabled_mod_roots(installation, enabled_mods));

    let mut inventory: HashMap<(CatalogItemKind, String), Option<ValidatedCatalogItem>> =
        HashMap::new();
    let mut ships: HashMap<String, Option<ValidatedShipHull>> = HashMap::new();
    for root in roots {
        merge_inventory_catalog(
            &mut inventory,
            CatalogItemKind::Resources,
            read_inventory_csv_catalog(
                &root,
                &root.join("data").join("campaign").join("commodities.csv"),
                CatalogSpaceSource::CsvCommodity,
            ),
        );
        merge_inventory_catalog(
            &mut inventory,
            CatalogItemKind::Weapons,
            read_inventory_csv_catalog(
                &root,
                &root.join("data").join("weapons").join("weapon_data.csv"),
                CatalogSpaceSource::WeaponSpec,
            ),
        );
        merge_inventory_catalog(
            &mut inventory,
            CatalogItemKind::FighterWing,
            read_inventory_csv_catalog(
                &root,
                &root.join("data").join("hulls").join("wing_data.csv"),
                CatalogSpaceSource::Fixed(1.0),
            ),
        );
        merge_inventory_catalog(
            &mut inventory,
            CatalogItemKind::Special,
            read_inventory_csv_catalog(
                &root,
                &root.join("data").join("campaign").join("special_items.csv"),
                CatalogSpaceSource::CsvSpecial,
            ),
        );
        merge_ship_catalog(
            &mut ships,
            read_ship_catalog(
                &root,
                &root.join("data").join("hulls").join("ship_data.csv"),
            ),
        );
    }

    let mut catalogs = LocalCatalogs {
        inventory: inventory
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect(),
        ships: ships
            .into_iter()
            .filter_map(|(id, definition)| definition.map(|definition| (id, definition)))
            .collect(),
        fingerprint: String::new(),
    };
    catalogs.fingerprint = data_catalog_fingerprint(&catalogs);
    catalogs
}

fn data_catalog_fingerprint(catalogs: &LocalCatalogs) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ludds-blessing-data-catalog-v1\0");

    let mut inventory: Vec<_> = catalogs.inventory.iter().collect();
    inventory.sort_by(|((left_kind, left_id), _), ((right_kind, right_id), _)| {
        catalog_kind_sort_key(*left_kind)
            .cmp(&catalog_kind_sort_key(*right_kind))
            .then_with(|| left_id.cmp(right_id))
    });
    for ((kind, id), definition) in inventory {
        digest.update([catalog_kind_sort_key(*kind)]);
        digest.update(id.as_bytes());
        digest.update([0]);
        digest.update(definition.name.as_bytes());
        digest.update([0]);
        match definition.cargo_space_per_unit {
            Some(value) => digest.update(value.to_bits().to_le_bytes()),
            None => digest.update(u32::MAX.to_le_bytes()),
        }
        digest.update([u8::from(definition.local_resources_eligible)]);
        digest.update([0xff]);
    }
    let mut ships: Vec<_> = catalogs.ships.iter().collect();
    ships.sort_by_key(|(id, _)| (*id).clone());
    for (id, definition) in ships {
        digest.update(b"ship\0");
        digest.update(id.as_bytes());
        digest.update([0]);
        digest.update(definition.name.as_bytes());
        digest.update([0]);
        if let Some(value) = definition.hull_size.as_deref() {
            digest.update(value.as_bytes());
        }
        digest.update([0]);
        digest.update([0xfe]);
    }
    format!("catalog-{}", hex::encode(&digest.finalize()[..16]))
}

const fn catalog_kind_sort_key(kind: CatalogItemKind) -> u8 {
    match kind {
        CatalogItemKind::Resources => 0,
        CatalogItemKind::Weapons => 1,
        CatalogItemKind::FighterWing => 2,
        CatalogItemKind::Special => 3,
    }
}

fn catalog_item_id(catalogs: &LocalCatalogs, key: &AddableItemKey) -> CatalogItemId {
    let mut digest = Sha256::new();
    digest.update(b"ludds-blessing-addable-item-v1\0");
    digest.update(catalogs.fingerprint.as_bytes());
    digest.update([catalog_kind_sort_key(key.kind), 0]);
    digest.update(key.item_id.as_bytes());
    digest.update([0]);
    if let Some(data) = &key.special_data {
        digest.update(data.as_bytes());
    }
    CatalogItemId::new(format!(
        "catalog-item-{}",
        hex::encode(&digest.finalize()[..16])
    ))
}

fn addable_item_definitions(
    catalogs: &LocalCatalogs,
) -> Vec<(CatalogItemId, AddableItemDefinition, AddableItemKind)> {
    const MAX_SAVED_STACK_QUANTITY: f32 = 1_000_000.0;
    let mut result = Vec::new();
    for ((kind, item_id), item) in &catalogs.inventory {
        let Some(cargo_space_per_unit) = item.cargo_space_per_unit else {
            continue;
        };
        let view_kind = match kind {
            CatalogItemKind::Resources => AddableItemKind::Commodity,
            CatalogItemKind::Weapons => AddableItemKind::Weapon,
            CatalogItemKind::FighterWing => AddableItemKind::FighterWing,
            CatalogItemKind::Special => continue,
        };
        let key = AddableItemKey {
            kind: *kind,
            item_id: item_id.clone(),
            special_data: None,
        };
        let definition = AddableItemDefinition {
            key: key.clone(),
            name: item.name.clone(),
            cargo_space_per_unit,
            max_quantity: MAX_SAVED_STACK_QUANTITY,
            local_resources_eligible: item.local_resources_eligible,
        };
        result.push((catalog_item_id(catalogs, &key), definition, view_kind));
    }

    let blueprint_groups: [(&str, AddableItemKind, CatalogItemKind); 3] = [
        (
            "ship_bp",
            AddableItemKind::ShipBlueprint,
            CatalogItemKind::Special,
        ),
        (
            "weapon_bp",
            AddableItemKind::WeaponBlueprint,
            CatalogItemKind::Special,
        ),
        (
            "fighter_bp",
            AddableItemKind::FighterBlueprint,
            CatalogItemKind::Special,
        ),
    ];
    for (blueprint_id, view_kind, special_kind) in blueprint_groups {
        let Some(base) = catalogs
            .inventory
            .get(&(CatalogItemKind::Special, blueprint_id.to_owned()))
        else {
            continue;
        };
        let Some(cargo_space_per_unit) = base.cargo_space_per_unit else {
            continue;
        };
        let targets: Vec<(&String, &str)> = match view_kind {
            AddableItemKind::ShipBlueprint => catalogs
                .ships
                .iter()
                .filter(|(_, ship)| supported_ship_hull_size(ship.hull_size.as_deref()))
                .map(|(id, ship)| (id, ship.name.as_str()))
                .collect(),
            AddableItemKind::WeaponBlueprint => catalogs
                .inventory
                .iter()
                .filter_map(|((kind, id), item)| {
                    (*kind == CatalogItemKind::Weapons).then_some((id, item.name.as_str()))
                })
                .collect(),
            AddableItemKind::FighterBlueprint => catalogs
                .inventory
                .iter()
                .filter_map(|((kind, id), item)| {
                    (*kind == CatalogItemKind::FighterWing).then_some((id, item.name.as_str()))
                })
                .collect(),
            _ => Vec::new(),
        };
        for (target_id, target_name) in targets {
            let key = AddableItemKey {
                kind: special_kind,
                item_id: blueprint_id.to_owned(),
                special_data: Some(target_id.clone()),
            };
            let definition = AddableItemDefinition {
                key: key.clone(),
                name: format!("{target_name} blueprint"),
                cargo_space_per_unit,
                max_quantity: MAX_SAVED_STACK_QUANTITY,
                local_resources_eligible: false,
            };
            result.push((catalog_item_id(catalogs, &key), definition, view_kind));
        }
    }
    result.sort_by(|left, right| {
        left.1
            .name
            .to_ascii_lowercase()
            .cmp(&right.1.name.to_ascii_lowercase())
            .then_with(|| left.1.key.item_id.cmp(&right.1.key.item_id))
    });
    result.truncate(MAX_DATA_CATALOG_ROWS);
    result
}

fn addable_item_views(catalogs: &LocalCatalogs) -> Vec<AddableItemView> {
    addable_item_definitions(catalogs)
        .into_iter()
        .map(|(id, definition, kind)| AddableItemView {
            id,
            item_id: definition
                .key
                .special_data
                .clone()
                .unwrap_or_else(|| definition.key.item_id.clone()),
            name: definition.name,
            kind,
            cargo_space_per_unit: definition.cargo_space_per_unit.to_string(),
            max_quantity: definition.max_quantity.to_string(),
            local_resources_eligible: definition.local_resources_eligible,
        })
        .collect()
}

fn supported_ship_hull_size(size: Option<&str>) -> bool {
    matches!(
        size,
        Some("Frigate" | "Destroyer" | "Cruiser" | "Capital Ship")
    )
}

fn find_addable_item(
    catalogs: &LocalCatalogs,
    id: &CatalogItemId,
) -> Option<AddableItemDefinition> {
    addable_item_definitions(catalogs)
        .into_iter()
        .find_map(|(candidate_id, definition, _)| (candidate_id == *id).then_some(definition))
}

fn trusted_addition_specs(
    catalogs: &LocalCatalogs,
) -> HashMap<save_core::CargoItemKey, save_core::CargoAdditionSpec> {
    addable_item_definitions(catalogs)
        .into_iter()
        .map(|(_, definition, _)| {
            let key = addable_key_to_core(&definition.key);
            (
                key.clone(),
                save_core::CargoAdditionSpec {
                    key,
                    cargo_space_per_unit: definition.cargo_space_per_unit,
                    local_resources_eligible: definition.local_resources_eligible,
                },
            )
        })
        .collect()
}

fn ensure_data_catalog_current(session: &SessionRecord) -> Result<(), CommandError> {
    ensure_data_catalog_matches(session, &session.data_catalogs.fingerprint)
}

fn ensure_data_catalog_matches(
    session: &SessionRecord,
    expected_fingerprint: &str,
) -> Result<(), CommandError> {
    let current = discover_data_catalogs(
        session.installation_root.as_deref(),
        &session.opened.snapshot().metadata.enabled_mods,
    );
    verify_data_catalog_fingerprint(expected_fingerprint, &current.fingerprint)
}

fn verify_data_catalog_fingerprint(
    expected_fingerprint: &str,
    current_fingerprint: &str,
) -> Result<(), CommandError> {
    if current_fingerprint == expected_fingerprint {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorCode::StaleSave,
            "The local item catalog changed; reopen the save before editing inventory",
        )
        .retryable())
    }
}

#[derive(Debug, Clone, Copy)]
enum CatalogSpaceSource {
    CsvCommodity,
    CsvSpecial,
    WeaponSpec,
    Fixed(f32),
}

fn read_inventory_csv_catalog(
    asset_root: &Path,
    path: &Path,
    space_source: CatalogSpaceSource,
) -> Option<HashMap<String, Option<ValidatedCatalogItem>>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_DATA_CATALOG_BYTES
    {
        return None;
    }
    let canonical_root = fs::canonicalize(asset_root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    if !canonical_path.starts_with(&canonical_root) {
        return None;
    }
    let bytes = fs::read(canonical_path).ok()?;
    if bytes.len() as u64 > MAX_DATA_CATALOG_BYTES {
        return None;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(decoded.as_bytes());
    let headers = reader.headers().ok()?.clone();
    let id_index = unique_header_index(&headers, "id").ok()??;
    let name_index = unique_header_index(&headers, "name").ok()?;
    let cargo_space_index = match space_source {
        CatalogSpaceSource::CsvCommodity | CatalogSpaceSource::CsvSpecial => {
            Some(unique_header_index(&headers, "cargo space").ok()??)
        }
        CatalogSpaceSource::WeaponSpec | CatalogSpaceSource::Fixed(_) => None,
    };
    let tags_index = match space_source {
        CatalogSpaceSource::CsvCommodity => Some(unique_header_index(&headers, "tags").ok()??),
        _ => None,
    };
    let mut result = HashMap::new();
    for (index, row) in reader.records().enumerate() {
        if index >= MAX_DATA_CATALOG_ROWS {
            return None;
        }
        let row = row.ok()?;
        let Some(id) = row.get(id_index).map(str::trim) else {
            continue;
        };
        if !valid_catalog_id(id) {
            continue;
        }
        let name = name_index
            .and_then(|index| row.get(index))
            .map(str::trim)
            .filter(|value| valid_catalog_display(value))
            .map_or_else(|| humanize_id(id), str::to_owned);
        let cargo_space_per_unit = match space_source {
            CatalogSpaceSource::CsvCommodity | CatalogSpaceSource::CsvSpecial => cargo_space_index
                .and_then(|index| row.get(index))
                .and_then(parse_catalog_nonnegative_f32),
            CatalogSpaceSource::WeaponSpec => read_weapon_cargo_space(asset_root, id),
            CatalogSpaceSource::Fixed(value) => Some(value),
        };
        let definition = ValidatedCatalogItem {
            name,
            cargo_space_per_unit,
            local_resources_eligible: matches!(space_source, CatalogSpaceSource::CsvCommodity)
                && tags_index
                    .and_then(|index| row.get(index))
                    .is_some_and(economic_commodity_tags),
        };
        match result.entry(id.to_owned()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(definition));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    Some(result)
}

fn economic_commodity_tags(tags: &str) -> bool {
    !tags
        .split(',')
        .map(str::trim)
        .any(|tag| matches!(tag, "nonecon" | "meta"))
}

fn parse_catalog_nonnegative_f32(value: &str) -> Option<f32> {
    let value = value.trim().parse::<f32>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn read_weapon_cargo_space(asset_root: &Path, weapon_id: &str) -> Option<f32> {
    let path = asset_root
        .join("data")
        .join("weapons")
        .join(format!("{weapon_id}.wpn"));
    let metadata = fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_SHIP_SPEC_BYTES
    {
        return None;
    }
    let canonical_root = fs::canonicalize(asset_root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    if !canonical_path.starts_with(canonical_root) {
        return None;
    }
    let bytes = fs::read(canonical_path).ok()?;
    if bytes.len() as u64 > MAX_SHIP_SPEC_BYTES {
        return None;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let object = parse_jsonish_object(&decoded)?;
    if json_string(&object, "id") != Some(weapon_id) {
        return None;
    }
    match json_string(&object, "size")? {
        "SMALL" => Some(2.0),
        "MEDIUM" => Some(4.0),
        "LARGE" => Some(8.0),
        _ => None,
    }
}

fn read_ship_catalog(
    asset_root: &Path,
    path: &Path,
) -> Option<HashMap<String, Option<ValidatedShipHull>>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_DATA_CATALOG_BYTES
    {
        return None;
    }
    let canonical_root = fs::canonicalize(asset_root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    if !canonical_path.starts_with(&canonical_root) {
        return None;
    }
    let bytes = fs::read(canonical_path).ok()?;
    if bytes.len() as u64 > MAX_DATA_CATALOG_BYTES {
        return None;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(decoded.as_bytes());
    let headers = reader.headers().ok()?.clone();
    let id_index = unique_header_index(&headers, "id").ok()??;
    let name_index = unique_header_index(&headers, "name").ok()??;
    let mut result = HashMap::new();
    for (index, row) in reader.records().enumerate() {
        if index >= MAX_DATA_CATALOG_ROWS {
            return None;
        }
        let row = row.ok()?;
        let Some(id) = row.get(id_index).map(str::trim) else {
            continue;
        };
        if !valid_catalog_id(id) {
            continue;
        }
        let name = row
            .get(name_index)
            .map(str::trim)
            .filter(|value| valid_catalog_display(value))
            .map_or_else(|| humanize_id(id), str::to_owned);
        let hull_size = read_ship_hull_size(asset_root, id);
        let definition = ValidatedShipHull { name, hull_size };
        match result.entry(id.to_owned()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(definition));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    Some(result)
}

fn read_ship_hull_size(asset_root: &Path, hull_id: &str) -> Option<String> {
    let path = asset_root
        .join("data")
        .join("hulls")
        .join(format!("{hull_id}.ship"));
    let metadata = fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_SHIP_SPEC_BYTES
    {
        return None;
    }
    let canonical_root = fs::canonicalize(asset_root).ok()?;
    let canonical_path = fs::canonicalize(path).ok()?;
    if !canonical_path.starts_with(canonical_root) {
        return None;
    }
    let bytes = fs::read(canonical_path).ok()?;
    if bytes.len() as u64 > MAX_SHIP_SPEC_BYTES {
        return None;
    }
    let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let object = parse_jsonish_object(&decoded)?;
    if json_string(&object, "hullId") != Some(hull_id) {
        return None;
    }
    let hull_size = json_string(&object, "hullSize")?;
    valid_catalog_id(hull_size).then(|| humanize_enum_id(hull_size))
}

fn merge_inventory_catalog(
    merged: &mut HashMap<(CatalogItemKind, String), Option<ValidatedCatalogItem>>,
    kind: CatalogItemKind,
    local: Option<HashMap<String, Option<ValidatedCatalogItem>>>,
) {
    let Some(local) = local else { return };
    for (id, definition) in local {
        match merged.entry((kind, id)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(definition);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
}

fn merge_ship_catalog(
    merged: &mut HashMap<String, Option<ValidatedShipHull>>,
    local: Option<HashMap<String, Option<ValidatedShipHull>>>,
) {
    let Some(local) = local else { return };
    for (id, definition) in local {
        match merged.entry(id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(definition);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
}

fn valid_catalog_display(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn discover_faction_names(
    installation: Option<&Path>,
    enabled_mods: &[String],
) -> HashMap<String, String> {
    let Some(installation) = installation else {
        return HashMap::new();
    };
    let mut roots = vec![installation_asset_root(installation)];
    roots.extend(enabled_mod_roots(installation, enabled_mods));
    let mut merged: HashMap<String, Option<String>> = HashMap::new();
    for asset_root in roots {
        let directory = asset_root.join("data").join("world").join("factions");
        let Ok(metadata) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let (Ok(canonical_root), Ok(canonical_directory)) =
            (fs::canonicalize(&asset_root), fs::canonicalize(&directory))
        else {
            continue;
        };
        if !canonical_directory.starts_with(canonical_root) {
            continue;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for (index, entry) in entries.enumerate() {
            if index >= MAX_FACTION_ENTRIES {
                break;
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("faction") {
                continue;
            }
            let Some(text) = read_bounded_regular_text(&path, MAX_SKILL_FILE_BYTES) else {
                continue;
            };
            let Some(object) = parse_jsonish_object(&text) else {
                continue;
            };
            let Some(id) = json_string(&object, "id") else {
                continue;
            };
            let Some(display_name) = json_string(&object, "displayName") else {
                continue;
            };
            if !valid_catalog_id(id)
                || display_name.is_empty()
                || display_name.len() > 256
                || display_name.chars().any(char::is_control)
            {
                continue;
            }
            match merged.entry(id.to_owned()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some(display_name.to_owned()));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    // Conflicting providers do not get to label a relation in the trusted view.
                    entry.insert(None);
                }
            }
        }
    }
    merged
        .into_iter()
        .filter_map(|(id, name)| name.map(|name| (id, name)))
        .collect()
}

fn discover_portraits(
    installation: Option<&Path>,
    enabled_mods: &[String],
) -> (
    CatalogView,
    HashMap<PortraitId, PathBuf>,
    HashMap<String, PortraitId>,
) {
    let Some(installation) = installation else {
        return (CatalogView::default(), HashMap::new(), HashMap::new());
    };
    let base_root = installation_asset_root(installation);
    let mut roots = vec![base_root.clone()];
    if base_root != installation {
        roots.push(installation.to_path_buf());
    }
    roots.extend(enabled_mod_roots(installation, enabled_mods));
    let mut by_relative: HashMap<String, Option<(PortraitId, PathBuf, String)>> = HashMap::new();
    for asset_root in roots {
        let portrait_dir = asset_root.join("graphics").join("portraits");
        let Ok(entries) = fs::read_dir(&portrait_dir) else {
            continue;
        };
        let entries: Vec<_> = entries.take(MAX_PORTRAIT_ENTRIES + 1).collect();
        if entries.len() > MAX_PORTRAIT_ENTRIES {
            // A truncated catalog cannot prove that a relative path is unique.
            continue;
        }
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_PORTRAIT_BYTES
                || portrait_mime(&path).is_none()
            {
                continue;
            }
            let Ok(canonical) = fs::canonicalize(&path) else {
                continue;
            };
            let Ok(relative) = canonical.strip_prefix(&asset_root) else {
                continue;
            };
            let relative = normalize_asset_path(&relative.to_string_lossy());
            let portrait_id = PortraitId::new(opaque_path_id("portrait", &canonical));
            let label = canonical
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(humanize_id)
                .unwrap_or_else(|| "Portrait".into());
            match by_relative.entry(relative) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(Some((portrait_id, canonical, label)));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    // Relative paths are what the save stores. Multiple providers are ambiguous.
                    entry.insert(None);
                }
            }
        }
    }
    let mut files = HashMap::new();
    let mut ids_by_path = HashMap::new();
    let mut portraits = Vec::new();
    for (relative, candidate) in by_relative {
        let Some((portrait_id, canonical, label)) = candidate else {
            continue;
        };
        files.insert(portrait_id.clone(), canonical);
        ids_by_path.insert(relative.clone(), portrait_id.clone());
        portraits.push(PortraitView {
            id: portrait_id,
            relative_path: relative,
            label,
        });
    }
    portraits.sort_by(|left, right| left.label.cmp(&right.label));
    (
        CatalogView {
            portraits,
            addable_items: Vec::new(),
        },
        files,
        ids_by_path,
    )
}

fn enabled_mod_roots(installation: &Path, enabled_mods: &[String]) -> Vec<PathBuf> {
    let enabled: HashSet<&str> = enabled_mods.iter().map(String::as_str).collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let mods_root = installation.join("mods");
    let Ok(metadata) = fs::symlink_metadata(&mods_root) else {
        return Vec::new();
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&mods_root) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.take(MAX_MOD_DIRECTORIES + 1).collect();
    if entries.len() > MAX_MOD_DIRECTORIES {
        return Vec::new();
    }
    let mut resolved: HashMap<String, Option<PathBuf>> = HashMap::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let mod_root = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&mod_root) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(mod_id) = read_jsonish_string_file(&mod_root.join("mod_info.json"), "id") else {
            continue;
        };
        if !valid_catalog_id(&mod_id) || !enabled.contains(mod_id.as_str()) {
            continue;
        }
        let Ok(canonical) = fs::canonicalize(&mod_root) else {
            continue;
        };
        match resolved.entry(mod_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(canonical));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                // Duplicate enabled mod IDs make all of that mod's assets ambiguous.
                entry.insert(None);
            }
        }
    }
    resolved.into_values().flatten().collect()
}

fn portrait_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn normalize_asset_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_owned()
}

fn opaque_path_id(namespace: &str, path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(namespace.as_bytes());
    digest.update([0]);
    digest.update(path.as_os_str().to_string_lossy().as_bytes());
    format!("{namespace}-{}", hex::encode(&digest.finalize()[..16]))
}

fn portrait_ids_by_path(catalog: &CatalogView) -> HashMap<String, PortraitId> {
    catalog
        .portraits
        .iter()
        .map(|portrait| {
            (
                normalize_asset_path(&portrait.relative_path),
                portrait.id.clone(),
            )
        })
        .collect()
}

fn validated_destination_root(raw_path: &str) -> Result<PathBuf, CommandError> {
    if raw_path.trim().is_empty() {
        return Err(CommandError::invalid_argument(
            "A destination saves folder is required",
        ));
    }
    let path = Path::new(raw_path);
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "The copy destination must be a regular, non-symlink directory",
        ));
    }
    Ok(fs::canonicalize(path)?)
}

fn apply_result_from_core(
    outcome: save_core::ApplyOutcome,
    message: String,
) -> Result<ApplyResult, CommandError> {
    let summary = save_core::inspect_save_dir(
        &outcome.location.save_dir,
        save_core::ScanOptions::default(),
    )?;
    Ok(ApplyResult {
        save_id: SaveId::new(summary.save_id),
        backup_id: outcome.backup.map(|backup| BackupId::new(backup.backup_id)),
        target_path: outcome.location.save_dir.to_string_lossy().into_owned(),
        campaign_hash: outcome.revision.campaign.sha256,
        descriptor_hash: outcome.revision.descriptor.sha256,
        message,
    })
}

fn backup_from_core(
    backup: save_core::BackupSummary,
    game_version: &str,
) -> Result<BackupSummary, CommandError> {
    let total_bytes = backup
        .revision
        .campaign
        .byte_len
        .get()
        .checked_add(backup.revision.descriptor.byte_len.get())
        .ok_or_else(|| CommandError::new(ErrorCode::ValidationFailed, "Backup size overflow"))?;
    Ok(BackupSummary {
        id: BackupId::new(backup.backup_id),
        save_id: SaveId::new(backup.save_id),
        created_at: format_epoch_millis(backup.created_at_millis.get()),
        reason: backup.reason,
        size_bytes: total_bytes.to_string(),
        game_version: game_version.to_owned(),
        pinned: backup.pinned,
    })
}

fn format_epoch_millis(value: i64) -> String {
    let nanos = i128::from(value).saturating_mul(1_000_000);
    match time::OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(timestamp) => timestamp
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| value.to_string()),
        Err(_) => value.to_string(),
    }
}

fn summary_from_core(summary: save_core::SaveSummary, root_id: RootId) -> SaveSummary {
    let metadata = summary.metadata;
    let protected = metadata
        .as_ref()
        .is_some_and(|metadata| metadata.iron_mode || metadata.autosave);
    let (compatibility, compatibility_reason) = match summary.compatibility {
        save_core::Compatibility::Editable if protected => (
            CompatibilityState::Locked,
            Some("Iron Mode and autosave slots require a per-session unlock.".into()),
        ),
        save_core::Compatibility::Editable => (CompatibilityState::Editable, None),
        save_core::Compatibility::ReadOnly { reason, .. } => {
            (CompatibilityState::Preview, Some(reason))
        }
        save_core::Compatibility::Invalid { reason, .. } => {
            (CompatibilityState::Unreadable, Some(reason))
        }
    };
    SaveSummary {
        id: SaveId::new(summary.save_id),
        root_id: Some(root_id),
        path: summary.location.save_dir.to_string_lossy().into_owned(),
        character_name: metadata
            .as_ref()
            .map(|metadata| metadata.character_name.clone())
            .unwrap_or_else(|| "Unreadable save".into()),
        character_level: metadata
            .as_ref()
            .map(|metadata| metadata.character_level)
            .unwrap_or(0),
        game_version: metadata
            .as_ref()
            .map(|metadata| metadata.game_version.clone())
            .unwrap_or_else(|| "Unknown".into()),
        save_file_version: metadata
            .as_ref()
            .map(|metadata| metadata.save_format.clone())
            .unwrap_or_else(|| "Unknown".into()),
        save_date: metadata
            .as_ref()
            .map(|metadata| metadata.save_date.clone())
            .unwrap_or_else(|| "Unknown".into()),
        location: metadata
            .as_ref()
            .map(|metadata| metadata.location_description.clone())
            .unwrap_or_else(|| "Unknown".into()),
        iron_mode: metadata.as_ref().is_some_and(|metadata| metadata.iron_mode),
        autosave: metadata.as_ref().is_some_and(|metadata| metadata.autosave),
        compressed: metadata
            .as_ref()
            .is_some_and(|metadata| metadata.compressed),
        enabled_mods: metadata
            .map(|metadata| metadata.enabled_mods)
            .unwrap_or_default(),
        compatibility,
        compatibility_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle_campaign_fixture() -> &'static str {
        r#"<?xml version="1.0" ?>
<CampaignEngine z="1">
<playerFleet ref="10"></playerFleet>
<characterData z="70"><name>Ada Vale</name><portraitName>graphics/portraits/portrait_a.png</portraitName><person ref="20"></person><isIronMode>false</isIronMode><skillsEverMadeElite z="71"></skillsEverMadeElite></characterData>
<clock z="75"><timestamp>-1000</timestamp></clock>
<factionManager z="76"><playerFaction ref="80"></playerFaction><relations z="90"></relations></factionManager>
<modAndPluginData z="100"><persistentData z="101"></persistentData></modAndPluginData>
<saveDirName>save_Ada_1</saveDirName>
<Flt z="10"><fD z="11"><m z="12"><FMmbr z="13"><c z="20" id="player-person" pid="steady" spr="graphics/portraits/portrait_a.png"><n z="21" f="Ada" l="Vale" g="FEMALE"></n><stats z="22" x2="0" xp="0" bx="0" db="0" l="1" pt="0" sp="0"><s>{}</s></stats></c></FMmbr></m><cargo z="30"><c z="31"><value>1000.0</value></c></cargo><c ref="20"></c><o z="40"></o></fD></Flt>
<f z="80"><id>player</id></f>
</CampaignEngine>"#
    }

    fn lifecycle_descriptor_fixture() -> &'static str {
        r#"<?xml version="1.0" ?>
<SaveGameData z="1"><portraitName>graphics/portraits/portrait_a.png</portraitName><characterName>Ada Vale</characterName><saveFileVersion>0.6</saveFileVersion><gameVersion>0.98a-RC8</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><difficulty>normal</difficulty><locDesc>Corvus</locDesc><saveDate>date</saveDate><slotCreationTimestamp>1</slotCreationTimestamp><enabledMods z="2"></enabledMods><autosave>false</autosave></SaveGameData>"#
    }

    fn lifecycle_summary(path: &Path) -> SaveSummary {
        SaveSummary {
            id: SaveId::new("save-lifecycle-test"),
            root_id: Some(RootId::new("root-lifecycle-test")),
            path: path.to_string_lossy().into_owned(),
            character_name: "Ada Vale".into(),
            character_level: 1,
            game_version: "0.98a-RC8".into(),
            save_file_version: "0.6".into(),
            save_date: "date".into(),
            location: "Corvus".into(),
            iron_mode: false,
            autosave: false,
            compressed: false,
            enabled_mods: Vec::new(),
            compatibility: CompatibilityState::Editable,
            compatibility_reason: None,
        }
    }

    fn open_lifecycle_session(service: &CoreService, save_dir: &Path) -> SaveSnapshot {
        service
            .open_save(save_dir, None, lifecycle_summary(save_dir))
            .unwrap()
    }

    fn lifecycle_service() -> (tempfile::TempDir, CoreService) {
        let temporary = tempfile::tempdir().unwrap();
        let save_dir = temporary.path().join("save_Ada_1");
        fs::create_dir(&save_dir).unwrap();
        fs::write(save_dir.join("campaign.xml"), lifecycle_campaign_fixture()).unwrap();
        fs::write(
            save_dir.join("descriptor.xml"),
            lifecycle_descriptor_fixture(),
        )
        .unwrap();
        let service = CoreService::new(temporary.path().join("app-data")).unwrap();
        (temporary, service)
    }

    fn progression_settings_fixture(player_max_level: u32) -> String {
        format!(
            r#"{{
  "playerMaxLevel": {player_max_level},
  "skillPointsPerLevel": 1,
  "storyPointsPerLevel": 4,
  "bonusXPUseMultAtMaxLevel": 3,
  "officerXPRequiredMult": 4,
  "officerMaxLevel": 5,
  "officerMaxEliteSkills": 1
}}"#
        )
    }

    fn progression_installation(root: &Path, player_max_level: u32) -> PathBuf {
        let installation = root.join("installation");
        #[cfg(windows)]
        let config = installation.join("starsector-core/data/config");
        #[cfg(not(windows))]
        let config = installation.join("data/config");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("settings.json"),
            progression_settings_fixture(player_max_level),
        )
        .unwrap();
        installation
    }

    fn progression_settings_path(installation: &Path) -> PathBuf {
        #[cfg(windows)]
        let path = installation.join("starsector-core/data/config/settings.json");
        #[cfg(not(windows))]
        let path = installation.join("data/config/settings.json");
        path
    }

    fn overwrite_progression_settings(installation: &Path, player_max_level: u32) {
        fs::write(
            progression_settings_path(installation),
            progression_settings_fixture(player_max_level),
        )
        .unwrap();
    }

    #[test]
    fn customized_installation_settings_disable_progression_and_direct_ipc() {
        let (temporary, service) = lifecycle_service();
        let save_dir = temporary.path().join("save_Ada_1");
        let installation = progression_installation(temporary.path(), 30);
        let snapshot = service
            .open_save(&save_dir, Some(&installation), lifecycle_summary(&save_dir))
            .unwrap();

        assert!(!snapshot.progression_capability.editable);
        assert!(snapshot
            .progression_capability
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("customized progression settings")));
        let error = service
            .prepare_review(
                &snapshot.session_id,
                vec![Edit::GrantPlayerXp { amount: "1".into() }],
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
    }

    #[test]
    fn unassociated_and_malformed_settings_fail_closed_for_progression_only() {
        let (temporary, service) = lifecycle_service();
        let save_dir = temporary.path().join("save_Ada_1");
        let unassociated = service
            .open_save(&save_dir, None, lifecycle_summary(&save_dir))
            .unwrap();
        assert!(!unassociated.progression_capability.editable);
        assert!(unassociated.write_capability.editable);
        assert!(unassociated
            .progression_capability
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not uniquely associated")));
        assert_eq!(
            service
                .prepare_review(
                    &unassociated.session_id,
                    vec![Edit::GrantPlayerXp { amount: "1".into() }],
                )
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );

        let installation = progression_installation(temporary.path(), 15);
        fs::write(progression_settings_path(&installation), b"{ malformed").unwrap();
        let malformed = service
            .open_save(&save_dir, Some(&installation), lifecycle_summary(&save_dir))
            .unwrap();
        assert!(!malformed.progression_capability.editable);
        assert!(malformed.write_capability.editable);
        assert!(malformed
            .progression_capability
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be verified")));
    }

    #[test]
    fn player_and_officer_settings_compatibility_are_independent() {
        let temporary = tempfile::tempdir().unwrap();
        let installation = progression_installation(temporary.path(), 15);

        let player_custom = progression_settings_fixture(15).replace(
            "\"bonusXPUseMultAtMaxLevel\": 3",
            "\"bonusXPUseMultAtMaxLevel\": 2",
        );
        fs::write(progression_settings_path(&installation), player_custom).unwrap();
        let issues = progression_settings_issues(Some(&installation));
        assert!(issues.player.is_some());
        assert!(issues.officer.is_none());

        let officer_custom = progression_settings_fixture(15).replace(
            "\"officerXPRequiredMult\": 4",
            "\"officerXPRequiredMult\": 3",
        );
        fs::write(progression_settings_path(&installation), officer_custom).unwrap();
        let issues = progression_settings_issues(Some(&installation));
        assert!(issues.player.is_none());
        assert!(issues.officer.is_some());

        let elite_cap_only = progression_settings_fixture(15).replace(
            "\"officerMaxEliteSkills\": 1",
            "\"officerMaxEliteSkills\": 2",
        );
        fs::write(progression_settings_path(&installation), elite_cap_only).unwrap();
        let issues = progression_settings_issues(Some(&installation));
        assert!(issues.player.is_none());
        assert!(issues.officer.is_none());
    }

    #[test]
    fn progression_settings_are_rechecked_when_a_review_is_applied() {
        let (temporary, service) = lifecycle_service();
        let save_dir = temporary.path().join("save_Ada_1");
        let installation = progression_installation(temporary.path(), 15);
        let snapshot = service
            .open_save(&save_dir, Some(&installation), lifecycle_summary(&save_dir))
            .unwrap();
        assert!(snapshot.progression_capability.editable);
        let review = service
            .prepare_review(
                &snapshot.session_id,
                vec![Edit::GrantPlayerXp { amount: "1".into() }],
            )
            .unwrap();
        let original = fs::read(save_dir.join("campaign.xml")).unwrap();

        overwrite_progression_settings(&installation, 30);
        let error = service
            .apply_review(&review.review_id, ApplyMode::ReplaceOriginal, true)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert_eq!(fs::read(save_dir.join("campaign.xml")).unwrap(), original);
    }

    #[test]
    fn hidden_player_setting_is_rechecked_before_save_copy() {
        let (temporary, service) = lifecycle_service();
        let save_dir = temporary.path().join("save_Ada_1");
        let installation = progression_installation(temporary.path(), 15);
        let snapshot = service
            .open_save(&save_dir, Some(&installation), lifecycle_summary(&save_dir))
            .unwrap();
        let review = service
            .prepare_review(
                &snapshot.session_id,
                vec![Edit::GrantPlayerXp { amount: "1".into() }],
            )
            .unwrap();
        let custom = progression_settings_fixture(15).replace(
            "\"bonusXPUseMultAtMaxLevel\": 3",
            "\"bonusXPUseMultAtMaxLevel\": 2",
        );
        fs::write(progression_settings_path(&installation), custom).unwrap();
        let copy_root = temporary.path().join("copies");
        fs::create_dir(&copy_root).unwrap();

        let error = service
            .apply_review(
                &review.review_id,
                ApplyMode::SaveCopy {
                    target_root: copy_root.to_string_lossy().into_owned(),
                },
                true,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert_eq!(fs::read_dir(copy_root).unwrap().count(), 0);
    }

    #[test]
    fn session_lookup_clones_only_the_arc_handle() {
        let (temporary, service) = lifecycle_service();
        let snapshot = open_lifecycle_session(&service, &temporary.path().join("save_Ada_1"));

        let first = service.require_session(&snapshot.session_id).unwrap();
        let second = service.require_session(&snapshot.session_id).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn opened_sessions_are_lru_bounded() {
        let (temporary, service) = lifecycle_service();
        let save_dir = temporary.path().join("save_Ada_1");
        let mut session_ids = Vec::new();
        for _ in 0..=MAX_OPEN_SESSIONS {
            session_ids.push(open_lifecycle_session(&service, &save_dir).session_id);
        }

        let state = service.lock_state().unwrap();
        assert_eq!(state.sessions.len(), MAX_OPEN_SESSIONS);
        assert!(!state.sessions.contains_key(&session_ids[0]));
        assert!(state.sessions.contains_key(session_ids.last().unwrap()));
    }

    #[test]
    fn newer_session_review_supersedes_and_releases_the_old_review() {
        let (temporary, service) = lifecycle_service();
        let snapshot = open_lifecycle_session(&service, &temporary.path().join("save_Ada_1"));
        let first = ReviewId::new("review-first");
        let second = ReviewId::new("review-second");

        for review_id in [&first, &second] {
            service
                .insert_review(
                    review_id.clone(),
                    ReviewRecord::Restore {
                        session_id: snapshot.session_id.clone(),
                        backup_id: BackupId::new("backup-test"),
                        acknowledgement_required: false,
                    },
                )
                .unwrap();
        }

        let state = service.lock_state().unwrap();
        assert_eq!(state.reviews.len(), 1);
        assert!(!state.reviews.contains_key(&first));
        assert!(state.reviews.contains_key(&second));
        drop(state);
        assert_eq!(
            service.take_restore_review(&first, true).unwrap_err().code,
            ErrorCode::ReviewConsumed
        );
    }

    #[test]
    fn close_session_and_discard_review_are_idempotent_and_release_state() {
        let (temporary, service) = lifecycle_service();
        let snapshot = open_lifecycle_session(&service, &temporary.path().join("save_Ada_1"));
        let review_id = ReviewId::new("review-abandoned");
        service
            .insert_review(
                review_id.clone(),
                ReviewRecord::Restore {
                    session_id: snapshot.session_id.clone(),
                    backup_id: BackupId::new("backup-test"),
                    acknowledgement_required: false,
                },
            )
            .unwrap();

        service.discard_review(&review_id).unwrap();
        service.discard_review(&review_id).unwrap();
        assert!(!service
            .lock_state()
            .unwrap()
            .reviews
            .contains_key(&review_id));

        let second_review = ReviewId::new("review-closed-with-session");
        service
            .insert_review(
                second_review.clone(),
                ReviewRecord::Restore {
                    session_id: snapshot.session_id.clone(),
                    backup_id: BackupId::new("backup-test"),
                    acknowledgement_required: false,
                },
            )
            .unwrap();
        service.close_session(&snapshot.session_id).unwrap();
        service.close_session(&snapshot.session_id).unwrap();
        let state = service.lock_state().unwrap();
        assert!(!state.sessions.contains_key(&snapshot.session_id));
        assert!(!state.reviews.contains_key(&second_review));
    }

    #[test]
    fn recovery_reviews_are_bounded_and_latest_target_can_be_reprepared() {
        let temporary = tempfile::tempdir().unwrap();
        let service = CoreService::new(temporary.path().to_path_buf()).unwrap();
        let mut ids = Vec::new();
        for index in 0..=MAX_PENDING_REVIEWS {
            let review_id = ReviewId::new(format!("recovery-review-{index}"));
            service
                .insert_review(
                    review_id.clone(),
                    ReviewRecord::Recovery {
                        save_id: format!("save-{index}"),
                        backup_id: format!("backup-{index}"),
                        acknowledgement_required: true,
                    },
                )
                .unwrap();
            ids.push(review_id);
        }

        let state = service.lock_state().unwrap();
        assert_eq!(state.reviews.len(), MAX_PENDING_REVIEWS);
        assert!(!state.reviews.contains_key(&ids[0]));
        assert!(state.reviews.contains_key(ids.last().unwrap()));
    }

    #[test]
    fn relaxed_json_parser_ignores_comments_and_string_contents() {
        let parsed = parse_jsonish_object(
            r##"{
                # "id":"from-comment"
                "note":"a string containing # and \"id\":\"from-string\"",
                "id":"trusted-id",
                "governingAptitude":"combat",
                "elite":true,
            }"##,
        )
        .unwrap();
        assert_eq!(json_string(&parsed, "id"), Some("trusted-id"));
        assert_eq!(parsed.get("elite"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn relaxed_json_parser_rejects_duplicate_or_malformed_objects() {
        assert!(parse_jsonish_object(r#"{"id":"first","id":"second"}"#).is_none());
        assert!(parse_jsonish_object(r#"{"id":"unterminated}"#).is_none());
        assert!(parse_jsonish_object("{ /* unterminated }").is_none());
    }

    #[test]
    fn restricted_catalog_tags_fail_closed() {
        assert!(restricted_skill_tags("npc_only, deprecated"));
        assert!(restricted_skill_tags("AI_CORE_ONLY"));
        assert!(!restricted_skill_tags("active_defenses, player_only"));
    }

    #[test]
    fn security_relevant_skill_headers_must_be_unique() {
        for header in ["id", "name", "icon", "combat officer", "admin", "tags"] {
            let headers = csv::StringRecord::from(vec![header, "unrelated", header]);
            assert_eq!(unique_header_index(&headers, header), Err(()), "{header}");
        }

        let headers = csv::StringRecord::from(vec!["id", "name", "icon"]);
        assert_eq!(unique_header_index(&headers, "id"), Ok(Some(0)));
        assert_eq!(unique_header_index(&headers, "admin"), Ok(None));
    }

    #[test]
    fn portrait_size_is_rechecked_against_the_post_read_length() {
        assert!(portrait_read_size_is_allowed(MAX_PORTRAIT_BYTES as usize));
        assert!(!portrait_read_size_is_allowed(
            MAX_PORTRAIT_BYTES as usize + 1
        ));
    }

    #[test]
    fn read_only_skill_view_cannot_be_changed_by_direct_ipc() {
        let skill = SkillView {
            id: "unknown_mod_skill".into(),
            name: "Unknown".into(),
            group: "Unknown mod".into(),
            rank: 2,
            max_rank: 2,
            editable: false,
            reason: Some("No trusted local skill definition".into()),
            icon_id: None,
        };
        assert_eq!(
            validate_skill_view(Some(&skill), 1).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
    }

    #[test]
    fn missing_warning_acknowledgement_does_not_consume_review() {
        let temporary = tempfile::tempdir().unwrap();
        let service = CoreService::new(temporary.path().to_path_buf()).unwrap();
        let review_id = ReviewId::new("restore-test");
        service.lock_state().unwrap().reviews.insert(
            review_id.clone(),
            ReviewRecord::Restore {
                session_id: SessionId::new("session-test"),
                backup_id: BackupId::new("backup-test"),
                acknowledgement_required: true,
            },
        );

        let error = match service.take_restore_review(&review_id, false) {
            Ok(_) => panic!("review unexpectedly bypassed acknowledgement"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert!(service
            .lock_state()
            .unwrap()
            .reviews
            .contains_key(&review_id));
        assert!(matches!(
            service.take_restore_review(&review_id, true).unwrap(),
            ReviewRecord::Restore { .. }
        ));
    }

    #[test]
    fn backup_components_reject_path_traversal() {
        assert!(valid_backup_component("backup-0123"));
        assert!(!valid_backup_component(".."));
        assert!(!valid_backup_component("folder/backup"));
    }

    #[test]
    fn ship_blueprint_catalog_rejects_duplicate_identity_headers() {
        let root = tempfile::tempdir().unwrap();
        let duplicate_headers = root.path().join("ship-data.csv");
        fs::write(
            &duplicate_headers,
            "name,id,id,designation,tech/manufacturer\nWolf,wolf,wolf,Frigate,Tri-Tachyon\n",
        )
        .unwrap();
        assert!(read_ship_catalog(root.path(), &duplicate_headers).is_none());
    }

    #[test]
    fn uppercase_rc8_hull_sizes_normalize_for_ship_blueprint_authorization() {
        let root = tempfile::tempdir().unwrap();
        let hulls = root.path().join("data").join("hulls");
        fs::create_dir_all(&hulls).unwrap();
        fs::write(
            hulls.join("wolf.ship"),
            r#"{"hullId":"wolf","hullSize":"FRIGATE"}"#,
        )
        .unwrap();
        let csv = hulls.join("ship_data.csv");
        fs::write(
            &csv,
            "name,id,designation,tech/manufacturer\nWolf,wolf,Combat frigate,Tri-Tachyon\n",
        )
        .unwrap();

        let ships = read_ship_catalog(root.path(), &csv).unwrap();
        assert_eq!(
            ships
                .get("wolf")
                .and_then(Option::as_ref)
                .and_then(|ship| ship.hull_size.as_deref()),
            Some("Frigate")
        );
        let mut catalogs = LocalCatalogs::default();
        catalogs.ships = ships
            .into_iter()
            .filter_map(|(id, ship)| ship.map(|ship| (id, ship)))
            .collect();
        catalogs.fingerprint = data_catalog_fingerprint(&catalogs);
    }

    #[test]
    fn quantity_authorization_rejects_removal_fractional_discrete_and_spoofed_stacks() {
        assert!(parse_positive_finite_float("0", "quantity").is_err());
        assert!(
            authorize_stack_quantity(false, InventoryKind::Resources, "100", 2.0, "stack").is_err()
        );
        assert!(
            authorize_stack_quantity(true, InventoryKind::Weapons, "100", 2.5, "stack").is_err()
        );
        assert!(
            authorize_stack_quantity(true, InventoryKind::Resources, "100", 101.0, "stack")
                .is_err()
        );
        assert!(
            authorize_stack_quantity(true, InventoryKind::Resources, "100", 2.5, "stack").is_ok()
        );
    }

    #[test]
    fn inventory_review_rejects_a_stale_catalog_fingerprint() {
        assert!(verify_data_catalog_fingerprint("catalog-a", "catalog-a").is_ok());
        let error = verify_data_catalog_fingerprint("catalog-a", "catalog-b").unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleSave);
        assert!(error.retryable);
    }

    #[test]
    fn cargo_review_fields_route_to_their_typed_sections() {
        assert_eq!(
            review_section("inventory.stack-1.quantity"),
            ReviewSection::Inventory
        );
        assert_eq!(
            review_section("colonies.colony-1.storage.stack-1.quantity"),
            ReviewSection::Colonies
        );
        assert_eq!(
            review_section("colonies.colony-1.local_resources.stack-1.quantity"),
            ReviewSection::Colonies
        );
    }

    #[test]
    fn cargo_review_labels_resolve_opaque_selectors_to_semantic_names() {
        let inventory = InventoryView {
            stacks: vec![InventoryStackView {
                id: InventoryStackId::new("opaque-inventory-stack"),
                item_id: "fuel".into(),
                special_data: None,
                name: "Fuel".into(),
                kind: InventoryKind::Resources,
                quantity: "100".into(),
                max_quantity: "1000".into(),
                cargo_space_per_unit: "0.25".into(),
                editable: true,
                reason: None,
            }],
            used_space: "25".into(),
            max_space: Some("100".into()),
            overloaded: false,
            editable: true,
            reason: None,
        };
        let colonies = vec![ColonyView {
            id: ColonyId::new("opaque-colony"),
            name: "New Dawn".into(),
            location_context: None,
            storage: Some(StorageView {
                stacks: vec![StorageStackView {
                    id: StorageStackId::new("opaque-storage-stack"),
                    item_id: "supplies".into(),
                    special_data: None,
                    name: "Supplies".into(),
                    kind: InventoryKind::Resources,
                    quantity: "50".into(),
                    max_quantity: "1000".into(),
                    cargo_space_per_unit: "1".into(),
                    editable: true,
                    reason: None,
                }],
                used_space: "50".into(),
                max_space: None,
                overloaded: false,
                editable: true,
                reason: None,
            }),
            local_resources: Some(ColonyResourcesView {
                stacks: vec![ColonyResourceStackView {
                    id: ColonyResourceStackId::new("opaque-resource-stack"),
                    item_id: "metals".into(),
                    special_data: None,
                    name: "Metals".into(),
                    kind: InventoryKind::Resources,
                    quantity: "250".into(),
                    max_quantity: "1000000".into(),
                    cargo_space_per_unit: "1".into(),
                    editable: true,
                    reason: None,
                }],
                used_space: "250".into(),
                max_space: None,
                overloaded: false,
                editable: true,
                reason: None,
            }),
            warnings: Vec::new(),
        }];

        assert_eq!(
            cargo_review_label(
                "inventory.opaque-inventory-stack.quantity",
                Some(&inventory),
                &colonies,
            ),
            Some("Fuel [fuel] quantity".into())
        );
        assert_eq!(
            cargo_review_label(
                "colonies.opaque-colony.storage.opaque-storage-stack.quantity",
                Some(&inventory),
                &colonies,
            ),
            Some("New Dawn · Supplies [supplies] quantity".into())
        );
        assert_eq!(
            cargo_review_label(
                "colonies.opaque-colony.storage.used_space",
                Some(&inventory),
                &colonies,
            ),
            Some("New Dawn storage space".into())
        );
        assert_eq!(
            cargo_review_label(
                "colonies.opaque-colony.local_resources.opaque-resource-stack.quantity",
                Some(&inventory),
                &colonies,
            ),
            Some("New Dawn · Metals [metals] local resource quantity".into())
        );
        assert_eq!(
            cargo_review_label(
                "colonies.opaque-colony.local_resources.used_space",
                Some(&inventory),
                &colonies,
            ),
            Some("New Dawn Local Resources stockpile size".into())
        );
    }

    #[test]
    fn catalog_fingerprint_is_order_independent_but_authorization_sensitive() {
        let mut left = LocalCatalogs::default();
        left.inventory.insert(
            (CatalogItemKind::Resources, "supplies".into()),
            ValidatedCatalogItem {
                name: "Supplies".into(),
                cargo_space_per_unit: Some(1.0),
                local_resources_eligible: true,
            },
        );
        left.inventory.insert(
            (CatalogItemKind::Weapons, "vulcan".into()),
            ValidatedCatalogItem {
                name: "Vulcan Cannon".into(),
                cargo_space_per_unit: Some(1.0),
                local_resources_eligible: false,
            },
        );
        let mut right = LocalCatalogs::default();
        right.inventory.insert(
            (CatalogItemKind::Weapons, "vulcan".into()),
            ValidatedCatalogItem {
                name: "Vulcan Cannon".into(),
                cargo_space_per_unit: Some(1.0),
                local_resources_eligible: false,
            },
        );
        right.inventory.insert(
            (CatalogItemKind::Resources, "supplies".into()),
            ValidatedCatalogItem {
                name: "Supplies".into(),
                cargo_space_per_unit: Some(1.0),
                local_resources_eligible: true,
            },
        );
        assert_eq!(
            data_catalog_fingerprint(&left),
            data_catalog_fingerprint(&right)
        );
        right
            .inventory
            .remove(&(CatalogItemKind::Weapons, "vulcan".into()));
        assert_ne!(
            data_catalog_fingerprint(&left),
            data_catalog_fingerprint(&right)
        );
    }

    #[test]
    fn stack_editability_requires_both_structure_and_a_trusted_catalog_entry() {
        let stack = save_core::InventoryStack {
            stack_id: "opaque-stack".into(),
            kind: save_core::InventoryKind::Resources,
            item_id: "supplies".into(),
            special_data: None,
            quantity: 5.0,
            max_quantity: 1_000_000.0,
            cargo_space_per_unit: 1.0,
            structurally_editable: true,
            reason: None,
        };
        let empty = LocalCatalogs::default();
        assert!(!inventory_stack_presentation(&stack, true, &empty).editable);

        let mut trusted = LocalCatalogs::default();
        trusted.inventory.insert(
            (CatalogItemKind::Resources, "supplies".into()),
            ValidatedCatalogItem {
                name: "Supplies".into(),
                cargo_space_per_unit: Some(1.0),
                local_resources_eligible: true,
            },
        );
        assert!(inventory_stack_presentation(&stack, true, &trusted).editable);

        let mut malformed = stack;
        malformed.structurally_editable = false;
        assert!(!inventory_stack_presentation(&malformed, true, &trusted).editable);
    }

    #[test]
    #[ignore = "requires STARSECTOR_SAVE_FIXTURE and reads a private local save without writing it"]
    fn opt_in_local_catalog_smoke_test() {
        let save_dir = PathBuf::from(
            std::env::var_os("STARSECTOR_SAVE_FIXTURE")
                .expect("set STARSECTOR_SAVE_FIXTURE to a local save directory"),
        );
        let opened = save_core::OpenedSave::open(
            save_core::SaveLocation::from_save_dir(&save_dir),
            save_core::OpenOptions::default(),
        )
        .unwrap();
        let installation = save_dir
            .parent()
            .and_then(Path::parent)
            .expect("fixture must remain inside an installation saves directory");
        let catalogs =
            discover_data_catalogs(Some(installation), &opened.snapshot().metadata.enabled_mods);
        assert!(catalogs
            .inventory
            .contains_key(&(CatalogItemKind::Resources, "supplies".into())));
        assert!(!catalogs.ships.is_empty());
        let additions = addable_item_views(&catalogs);
        assert!(additions
            .iter()
            .any(|item| matches!(item.kind, AddableItemKind::Weapon)));
        assert!(additions.iter().any(|item| matches!(
            item.kind,
            AddableItemKind::ShipBlueprint
                | AddableItemKind::WeaponBlueprint
                | AddableItemKind::FighterBlueprint
        )));
        let inventory = inventory_view_from_core(
            &opened.snapshot().inventory,
            opened.snapshot().capabilities.inventory,
            &catalogs,
        );
        assert_eq!(
            inventory.stacks.len(),
            opened.snapshot().inventory.stacks.len()
        );
        assert!(!inventory.stacks.is_empty());

        let colonies: Vec<_> = opened
            .snapshot()
            .colonies
            .iter()
            .map(|colony| colony_view_from_core(colony, &opened.snapshot().capabilities, &catalogs))
            .collect();
        assert_eq!(colonies.len(), opened.snapshot().colonies.len());
        assert!(colonies.iter().any(|colony| {
            colony.local_resources.as_ref().is_some_and(|resources| {
                !resources.stacks.is_empty()
                    && resources
                        .stacks
                        .iter()
                        .any(|stack| stack.kind == InventoryKind::Resources && stack.editable)
            })
        }));
    }
}
