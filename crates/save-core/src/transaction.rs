use crate::descriptor::parse_descriptor;
use crate::error::{CoreError, ErrorCode, Result};
use crate::file_util::{ensure_regular_directory, fingerprint, opaque_id, read_regular_file};
use crate::model::{ContentRevision, DecimalI64, SaveLocation};
use crate::patch::apply_patches;
use crate::review::PreparedReview;
use crate::xml::{XmlDocument, XmlLimits};
use crate::{SUPPORTED_GAME_VERSION, SUPPORTED_SAVE_FORMAT};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BackupStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupSummary {
    pub backup_id: String,
    pub save_id: String,
    pub created_at_millis: DecimalI64,
    pub pinned: bool,
    pub reason: String,
    pub revision: ContentRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub location: SaveLocation,
    pub revision: ContentRevision,
    pub backup: Option<BackupSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub save_id: String,
    pub backup_id: String,
    /// Last transaction phase durably recorded before interruption.
    pub phase: String,
    pub journal_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupManifest {
    schema: u32,
    summary: BackupSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    schema: u32,
    save_id: String,
    backup_id: String,
    phase: String,
    destination: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy: Option<CopyJournal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CopyJournal {
    source: PathBuf,
    staging: PathBuf,
    expected_revision: ContentRevision,
}

const MAX_RECOVERY_SCAN_ENTRIES: usize = 100_000;
#[cfg(unix)]
const MAX_PRIVATE_STORAGE_ENTRIES: usize = 100_000;

#[cfg(any(windows, unix))]
const MAX_STARSECTOR_LOG_FILE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(any(windows, unix))]
const MAX_STARSECTOR_LOG_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

impl BackupStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a durable byte-identical backup without mutating the live save.
    /// This is used by the protected-save unlock flow before edit capabilities
    /// are exposed to the session.
    pub fn backup_current(
        &self,
        save_id: &str,
        source: SaveLocation,
        reason: &str,
        pinned: bool,
    ) -> Result<BackupSummary> {
        ensure_regular_directory(&source.save_dir)?;
        if opaque_id("save", source.save_dir.to_string_lossy().as_bytes()) != save_id {
            return Err(CoreError::new(
                ErrorCode::InvalidPath,
                "save identity does not match the backup source directory",
            ));
        }
        ensure_save_inactive(&source)?;
        let mut campaign_file = lock_for_write(&source.campaign_path)?;
        let mut descriptor_file = match lock_for_write(&source.descriptor_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = FileExt::unlock(&campaign_file);
                return Err(error);
            }
        };
        let campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
        let descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
        // Full parse before accepting a backup as the unlock safety point.
        XmlDocument::parse(campaign.clone(), XmlLimits::default())?;
        parse_descriptor(
            descriptor.clone(),
            XmlLimits {
                max_bytes: 4 * 1024 * 1024,
                max_elements: 100_000,
                ..XmlLimits::default()
            },
        )?;
        let backup = self.create_backup(save_id, &campaign, &descriptor, pinned, reason)?;
        let final_campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
        let final_descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
        if campaign != final_campaign || descriptor != final_descriptor {
            return Err(CoreError::new(
                ErrorCode::StaleSave,
                "save changed while its safety backup was being created",
            ));
        }
        let _ = FileExt::unlock(&campaign_file);
        let _ = FileExt::unlock(&descriptor_file);
        Ok(backup)
    }

    pub fn list(&self, save_id: &str) -> Result<Vec<BackupSummary>> {
        validate_opaque_component(save_id)?;
        let directory = self.root.join(save_id);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        ensure_regular_directory(&directory)?;
        let mut backups = Vec::new();
        for (index, entry) in fs::read_dir(&directory)?.enumerate() {
            if index >= 100_000 {
                return Err(CoreError::new(
                    ErrorCode::ResourceLimit,
                    "backup entry limit exceeded",
                ));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            let bytes = match read_regular_file(&manifest_path, 1024 * 1024) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let manifest: BackupManifest = match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(_) => continue,
            };
            if manifest.schema == 1 && manifest.summary.save_id == save_id {
                backups.push(manifest.summary);
            }
        }
        backups.sort_by_key(|backup| std::cmp::Reverse(backup.created_at_millis));
        Ok(backups)
    }

    pub fn pending_recoveries(&self) -> Result<Vec<RecoveryRecord>> {
        self.pending_recoveries_bounded(MAX_RECOVERY_SCAN_ENTRIES)
    }

    fn pending_recoveries_bounded(&self, max_entries: usize) -> Result<Vec<RecoveryRecord>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        ensure_regular_directory(&self.root)?;
        let mut result = Vec::new();
        let mut visited = 0_usize;
        for save_entry in fs::read_dir(&self.root)? {
            visited = visited.checked_add(1).ok_or_else(|| {
                CoreError::new(ErrorCode::ResourceLimit, "recovery scan counter overflow")
            })?;
            if visited > max_entries {
                return Err(CoreError::new(
                    ErrorCode::ResourceLimit,
                    "recovery entry limit exceeded",
                ));
            }
            let save_entry = save_entry?;
            let metadata = fs::symlink_metadata(save_entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                continue;
            }
            for backup_entry in fs::read_dir(save_entry.path())? {
                visited = visited.checked_add(1).ok_or_else(|| {
                    CoreError::new(ErrorCode::ResourceLimit, "recovery scan counter overflow")
                })?;
                if visited > max_entries {
                    return Err(CoreError::new(
                        ErrorCode::ResourceLimit,
                        "recovery entry limit exceeded",
                    ));
                }
                let backup_entry = backup_entry?;
                let metadata = fs::symlink_metadata(backup_entry.path())?;
                if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                    continue;
                }
                let started = backup_entry.path().join("transaction-started.json");
                let complete = backup_entry.path().join("transaction-complete.json");
                if started.exists() && !complete.exists() {
                    let record: JournalRecord =
                        serde_json::from_slice(&read_regular_file(&started, 1024 * 1024)?)?;
                    result.push(RecoveryRecord {
                        save_id: record.save_id,
                        backup_id: record.backup_id,
                        phase: record.phase,
                        journal_path: started,
                    });
                }
            }
        }
        Ok(result)
    }

    /// Consumes a review so it cannot be accidentally applied twice by the caller.
    pub fn apply_replace(
        &self,
        review: PreparedReview,
        pinned_backup: bool,
    ) -> Result<ApplyOutcome> {
        ensure_regular_directory(&review.location.save_dir)?;
        ensure_save_inactive(&review.location)?;
        let mut campaign_file = lock_for_write(&review.location.campaign_path)?;
        let mut descriptor_file = match lock_for_write(&review.location.descriptor_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = FileExt::unlock(&campaign_file);
                return Err(error);
            }
        };
        let campaign_live =
            read_locked(&mut campaign_file, review.campaign_source.len() as u64 + 1)?;
        let descriptor_live = read_locked(
            &mut descriptor_file,
            review.descriptor_source.len() as u64 + 1,
        )?;
        review.validate_against(&campaign_live, &descriptor_live)?;
        validate_supported_write_pair(
            &campaign_live,
            &descriptor_live,
            review.protected_write_authorized,
        )?;

        let backup = self.create_backup(
            &review.summary.save_id,
            &campaign_live,
            &descriptor_live,
            pinned_backup,
            "before editor apply",
        )?;
        let backup_dir = self.root.join(&backup.save_id).join(&backup.backup_id);
        let mut journal = JournalRecord {
            schema: 1,
            save_id: backup.save_id.clone(),
            backup_id: backup.backup_id.clone(),
            phase: "prepared".to_owned(),
            destination: review.location.save_dir.clone(),
            copy: None,
        };
        let campaign_temp = sibling_temp(&review.location.campaign_path, "campaign")?;
        let descriptor_temp = sibling_temp(&review.location.descriptor_path, "descriptor")?;
        write_new_synced(
            &backup_dir.join("transaction-started.json"),
            &serde_json::to_vec_pretty(&journal)?,
        )?;
        let preparation = (|| -> Result<()> {
            write_new_synced(&campaign_temp, &review.campaign_output)?;
            write_new_synced(&descriptor_temp, &review.descriptor_output)?;

            // Keep both locks through the final stale validation. Windows
            // ReplaceFileW cannot replace a byte-range-locked destination, so
            // handles are released only for the native replacement window and
            // the newly installed pair is immediately reopened and relocked.
            let final_campaign =
                read_locked(&mut campaign_file, review.campaign_source.len() as u64 + 1)?;
            let final_descriptor = read_locked(
                &mut descriptor_file,
                review.descriptor_source.len() as u64 + 1,
            )?;
            review.validate_against(&final_campaign, &final_descriptor)
        })();
        if let Err(error) = preparation {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            complete_or_recovery(&backup_dir, &journal, "aborted_before_replace")?;
            return Err(error);
        }
        // Unix renames can proceed while the old inodes remain locked. The
        // Windows replacement APIs reject byte-range-locked destinations, so
        // that platform has a necessarily narrow unlock window; final
        // validation immediately reopens and locks the installed pair.
        #[cfg(windows)]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        let replacement = replace_pair(
            &campaign_temp,
            &descriptor_temp,
            &review.location,
            &campaign_live,
            &descriptor_live,
            ReplacementJournal {
                directory: &backup_dir,
                record: &mut journal,
                phase_prefix: "apply",
            },
        );
        #[cfg(not(windows))]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        if let Err(error) = replacement {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
            if error.code != ErrorCode::RecoveryRequired {
                complete_or_recovery(&backup_dir, &journal, "rolled_back_or_aborted")?;
            }
            return Err(error);
        }

        let committed = validate_committed_pair(
            &review.location,
            &review.campaign_output,
            &review.descriptor_output,
        );
        let (committed_campaign, committed_descriptor) = match committed {
            Ok(pair) => pair,
            Err(validation_error) => {
                if validation_error.code == ErrorCode::GameRunning {
                    return Err(CoreError::new(
                        ErrorCode::RecoveryRequired,
                        "The selected save became active or unavailable during final validation; recover it from the durable backup before retrying",
                    ));
                }
                if rollback_pair(
                    &review.location,
                    &campaign_live,
                    &descriptor_live,
                    &review.campaign_output,
                    &review.descriptor_output,
                    ReplacementJournal {
                        directory: &backup_dir,
                        record: &mut journal,
                        phase_prefix: "apply_validation_rollback",
                    },
                )
                .is_err()
                {
                    return Err(CoreError::new(
                        ErrorCode::RecoveryRequired,
                        format!("post-commit validation and rollback failed: {validation_error}"),
                    ));
                }
                complete_or_recovery(&backup_dir, &journal, "rolled_back")?;
                return Err(validation_error);
            }
        };
        complete_or_recovery(&backup_dir, &journal, "complete")?;
        Ok(ApplyOutcome {
            location: review.location,
            revision: ContentRevision {
                campaign: fingerprint(&committed_campaign),
                descriptor: fingerprint(&committed_descriptor),
            },
            backup: Some(backup),
        })
    }

    pub fn save_copy(
        &self,
        review: PreparedReview,
        destination_parent: &Path,
        display_name: &str,
    ) -> Result<ApplyOutcome> {
        ensure_regular_directory(destination_parent)?;
        ensure_regular_directory(&review.location.save_dir)?;
        ensure_save_inactive(&review.location)?;
        let mut campaign_file = lock_for_write(&review.location.campaign_path)?;
        let mut descriptor_file = match lock_for_write(&review.location.descriptor_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = FileExt::unlock(&campaign_file);
                return Err(error);
            }
        };
        let campaign_limit = u64::try_from(review.campaign_source.len())
            .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "campaign size overflow"))?;
        let descriptor_limit = u64::try_from(review.descriptor_source.len())
            .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "descriptor size overflow"))?;
        let current_campaign = read_locked(&mut campaign_file, campaign_limit)?;
        let current_descriptor = read_locked(&mut descriptor_file, descriptor_limit)?;
        review.validate_against(&current_campaign, &current_descriptor)?;
        validate_supported_write_pair(
            &current_campaign,
            &current_descriptor,
            review.protected_write_authorized,
        )?;

        let safe_name = sanitize_copy_name(display_name);
        let suffix = Uuid::new_v4().simple().to_string();
        let directory_name = format!("save_{safe_name}_{}", &suffix[..12]);
        let destination = destination_parent.join(&directory_name);
        let location = SaveLocation::from_save_dir(&destination);
        let staging = destination_parent.join(format!(".ludds-blessing-copy-{suffix}.tmp"));
        if destination.exists() || staging.exists() {
            return Err(CoreError::new(
                ErrorCode::InvalidPath,
                "generated save-copy path already exists",
            ));
        }
        let staging_location = SaveLocation::from_save_dir(&staging);

        let campaign_doc =
            XmlDocument::parse(review.campaign_output.clone(), XmlLimits::default())?;
        let save_dir_name = campaign_doc.unique_direct_child(campaign_doc.root(), "saveDirName")?;
        let campaign_patch =
            campaign_doc.text_patch(save_dir_name, &directory_name, "save-copy directory name")?;
        let campaign_output = apply_patches(campaign_doc.bytes(), &[campaign_patch])?;
        let descriptor_doc = parse_descriptor(
            review.descriptor_output.clone(),
            XmlLimits {
                max_bytes: 4 * 1024 * 1024,
                max_elements: 100_000,
                ..XmlLimits::default()
            },
        )?;
        let now = now_millis()?;
        let descriptor_patch = descriptor_doc.slot_creation_patch(now)?;
        let descriptor_output = apply_patches(descriptor_doc.xml.bytes(), &[descriptor_patch])?;
        validate_supported_write_pair(
            &campaign_output,
            &descriptor_output,
            review.protected_write_authorized,
        )?;
        let expected_revision = ContentRevision {
            campaign: fingerprint(&campaign_output),
            descriptor: fingerprint(&descriptor_output),
        };

        let backup = self.create_backup(
            &review.summary.save_id,
            &current_campaign,
            &current_descriptor,
            false,
            "before save copy",
        )?;
        let backup_dir = self.root.join(&backup.save_id).join(&backup.backup_id);
        let mut journal = JournalRecord {
            schema: 1,
            save_id: backup.save_id.clone(),
            backup_id: backup.backup_id.clone(),
            phase: "copy_prepared".to_owned(),
            destination: destination.clone(),
            copy: Some(CopyJournal {
                source: review.location.save_dir.clone(),
                staging: staging.clone(),
                expected_revision: expected_revision.clone(),
            }),
        };
        write_new_synced(
            &backup_dir.join("transaction-started.json"),
            &serde_json::to_vec_pretty(&journal)?,
        )?;

        let mut published = false;
        let result = (|| -> Result<(Vec<u8>, Vec<u8>)> {
            create_private_directory(&staging)?;
            sync_directory(destination_parent)?;
            update_journal_phase(&backup_dir, &mut journal, "copy_staging_created")?;
            write_new_synced(&staging_location.campaign_path, &campaign_output)?;
            write_new_synced(&staging_location.descriptor_path, &descriptor_output)?;
            let staged_campaign = read_regular_file(
                &staging_location.campaign_path,
                expected_revision.campaign.byte_len.get(),
            )?;
            let staged_descriptor = read_regular_file(
                &staging_location.descriptor_path,
                expected_revision.descriptor.byte_len.get(),
            )?;
            if staged_campaign != campaign_output || staged_descriptor != descriptor_output {
                return Err(CoreError::validation(
                    "staged save-copy bytes differ from the validated output",
                ));
            }
            validate_supported_write_pair(
                &staged_campaign,
                &staged_descriptor,
                review.protected_write_authorized,
            )?;
            sync_directory(&staging)?;
            update_journal_phase(&backup_dir, &mut journal, "copy_staged_validated")?;

            let source_campaign = read_locked(&mut campaign_file, campaign_limit)?;
            let source_descriptor = read_locked(&mut descriptor_file, descriptor_limit)?;
            review.validate_against(&source_campaign, &source_descriptor)?;
            ensure_save_inactive(&review.location)?;
            ensure_save_inactive(&location)?;
            fs::rename(&staging, &destination)?;
            published = true;
            sync_directory(destination_parent)?;
            update_journal_phase(&backup_dir, &mut journal, "copy_published")?;

            let committed =
                validate_committed_pair(&location, &campaign_output, &descriptor_output)?;
            let final_source_campaign = read_locked(&mut campaign_file, campaign_limit)?;
            let final_source_descriptor = read_locked(&mut descriptor_file, descriptor_limit)?;
            review.validate_against(&final_source_campaign, &final_source_descriptor)?;
            complete_or_recovery(&backup_dir, &journal, "copy_complete")?;
            Ok(committed)
        })();
        let _ = FileExt::unlock(&campaign_file);
        let _ = FileExt::unlock(&descriptor_file);
        match result {
            Ok((committed_campaign, committed_descriptor)) => Ok(ApplyOutcome {
                location,
                revision: ContentRevision {
                    campaign: fingerprint(&committed_campaign),
                    descriptor: fingerprint(&committed_descriptor),
                },
                backup: Some(backup),
            }),
            Err(error) if published => Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!("save copy was published but final validation did not complete: {error}"),
            )),
            Err(error) => {
                if cleanup_copy_staging(&staging).is_err() {
                    return Err(CoreError::new(
                        ErrorCode::RecoveryRequired,
                        format!("save-copy staging cleanup failed after: {error}"),
                    ));
                }
                complete_or_recovery(&backup_dir, &journal, "copy_aborted")?;
                Err(error)
            }
        }
    }

    pub fn restore(
        &self,
        save_id: &str,
        backup_id: &str,
        destination: SaveLocation,
        expected_current: &ContentRevision,
    ) -> Result<ApplyOutcome> {
        self.restore_authorized(save_id, backup_id, destination, expected_current, false)
    }

    /// Restores a supported backup. `allow_protected` must only be set after
    /// the caller's session has completed the explicit protected-save
    /// acknowledgement and immediate pinned-backup flow.
    pub fn restore_authorized(
        &self,
        save_id: &str,
        backup_id: &str,
        destination: SaveLocation,
        expected_current: &ContentRevision,
        allow_protected: bool,
    ) -> Result<ApplyOutcome> {
        ensure_save_inactive(&destination)?;
        ensure_regular_directory(&destination.save_dir)?;
        validate_opaque_component(save_id)?;
        validate_opaque_component(backup_id)?;
        if opaque_id("save", destination.save_dir.to_string_lossy().as_bytes()) != save_id {
            return Err(CoreError::new(
                ErrorCode::InvalidPath,
                "save identity does not match the restore destination",
            ));
        }
        let backup_dir = self.root.join(save_id).join(backup_id);
        ensure_regular_directory(&backup_dir)?;
        let manifest: BackupManifest = serde_json::from_slice(&read_regular_file(
            &backup_dir.join("manifest.json"),
            1024 * 1024,
        )?)?;
        if manifest.schema != 1
            || manifest.summary.save_id != save_id
            || manifest.summary.backup_id != backup_id
        {
            return Err(CoreError::validation("backup manifest identity mismatch"));
        }
        validate_backup_manifest_lengths(&manifest)?;
        let restore_campaign = read_regular_file(
            &backup_dir.join("campaign.xml"),
            manifest.summary.revision.campaign.byte_len.get(),
        )?;
        let restore_descriptor = read_regular_file(
            &backup_dir.join("descriptor.xml"),
            manifest.summary.revision.descriptor.byte_len.get(),
        )?;
        if fingerprint(&restore_campaign) != manifest.summary.revision.campaign
            || fingerprint(&restore_descriptor) != manifest.summary.revision.descriptor
        {
            return Err(CoreError::validation("backup hash validation failed"));
        }
        validate_supported_write_pair(&restore_campaign, &restore_descriptor, allow_protected)?;

        let mut campaign_file = lock_for_write(&destination.campaign_path)?;
        let mut descriptor_file = match lock_for_write(&destination.descriptor_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = FileExt::unlock(&campaign_file);
                return Err(error);
            }
        };
        let current_campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
        let current_descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
        let actual = ContentRevision {
            campaign: fingerprint(&current_campaign),
            descriptor: fingerprint(&current_descriptor),
        };
        if &actual != expected_current {
            return Err(CoreError::new(
                ErrorCode::StaleSave,
                "save changed after restore review",
            ));
        }
        validate_supported_write_pair(&current_campaign, &current_descriptor, allow_protected)?;
        let safety_backup = self.create_backup(
            save_id,
            &current_campaign,
            &current_descriptor,
            true,
            "before backup restore",
        )?;
        let safety_backup_dir = self
            .root
            .join(&safety_backup.save_id)
            .join(&safety_backup.backup_id);
        let mut journal = JournalRecord {
            schema: 1,
            save_id: safety_backup.save_id.clone(),
            backup_id: safety_backup.backup_id.clone(),
            phase: "prepared_restore".to_owned(),
            destination: destination.save_dir.clone(),
            copy: None,
        };
        let campaign_temp = sibling_temp(&destination.campaign_path, "restore-campaign")?;
        let descriptor_temp = sibling_temp(&destination.descriptor_path, "restore-descriptor")?;
        write_new_synced(
            &safety_backup_dir.join("transaction-started.json"),
            &serde_json::to_vec_pretty(&journal)?,
        )?;
        let preparation = (|| -> Result<()> {
            write_new_synced(&campaign_temp, &restore_campaign)?;
            write_new_synced(&descriptor_temp, &restore_descriptor)?;
            let final_campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
            let final_descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
            if final_campaign != current_campaign || final_descriptor != current_descriptor {
                return Err(CoreError::new(
                    ErrorCode::StaleSave,
                    "save changed while restore was being prepared",
                ));
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            complete_or_recovery(
                &safety_backup_dir,
                &journal,
                "restore_aborted_before_replace",
            )?;
            return Err(error);
        }
        #[cfg(windows)]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        let replacement = replace_pair(
            &campaign_temp,
            &descriptor_temp,
            &destination,
            &current_campaign,
            &current_descriptor,
            ReplacementJournal {
                directory: &safety_backup_dir,
                record: &mut journal,
                phase_prefix: "restore",
            },
        );
        #[cfg(not(windows))]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        if let Err(error) = replacement {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
            if error.code != ErrorCode::RecoveryRequired {
                complete_or_recovery(
                    &safety_backup_dir,
                    &journal,
                    "restore_rolled_back_or_aborted",
                )?;
            }
            return Err(error);
        }
        let committed =
            validate_committed_pair(&destination, &restore_campaign, &restore_descriptor);
        let (committed_campaign, committed_descriptor) = match committed {
            Ok(pair) => pair,
            Err(validation_error) => {
                if validation_error.code == ErrorCode::GameRunning {
                    return Err(CoreError::new(
                        ErrorCode::RecoveryRequired,
                        "The selected save became active or unavailable during restore validation; recover it from the safety backup before retrying",
                    ));
                }
                if rollback_pair(
                    &destination,
                    &current_campaign,
                    &current_descriptor,
                    &restore_campaign,
                    &restore_descriptor,
                    ReplacementJournal {
                        directory: &safety_backup_dir,
                        record: &mut journal,
                        phase_prefix: "restore_validation_rollback",
                    },
                )
                .is_err()
                {
                    return Err(CoreError::new(
                        ErrorCode::RecoveryRequired,
                        format!("restore validation and rollback failed: {validation_error}"),
                    ));
                }
                complete_or_recovery(&safety_backup_dir, &journal, "restore_rolled_back")?;
                return Err(validation_error);
            }
        };
        complete_or_recovery(&safety_backup_dir, &journal, "restore_complete")?;
        resolve_recovery_after_restore(&backup_dir, save_id, backup_id)?;
        Ok(ApplyOutcome {
            location: destination,
            revision: ContentRevision {
                campaign: fingerprint(&committed_campaign),
                descriptor: fingerprint(&committed_descriptor),
            },
            backup: Some(safety_backup),
        })
    }

    /// Resolves an interrupted transaction from its exact durable backup even
    /// when the current XML pair is no longer parseable. Both current files
    /// must still be regular files so they can be locked and preserved as a raw
    /// pinned emergency backup before recovery.
    pub fn recover_pending(&self, save_id: &str, backup_id: &str) -> Result<ApplyOutcome> {
        validate_opaque_component(save_id)?;
        validate_opaque_component(backup_id)?;
        let recovery_dir = self.root.join(save_id).join(backup_id);
        ensure_regular_directory(&recovery_dir)?;
        let started = recovery_dir.join("transaction-started.json");
        let complete = recovery_dir.join("transaction-complete.json");
        if complete.exists() || !started.exists() {
            return Err(CoreError::new(
                ErrorCode::NotFound,
                "the selected backup has no pending recovery",
            ));
        }
        let mut journal: JournalRecord =
            serde_json::from_slice(&read_regular_file(&started, 1024 * 1024)?)?;
        if journal.schema != 1 || journal.save_id != save_id || journal.backup_id != backup_id {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "pending recovery journal identity mismatch",
            ));
        }
        let manifest: BackupManifest = serde_json::from_slice(&read_regular_file(
            &recovery_dir.join("manifest.json"),
            1024 * 1024,
        )?)?;
        if manifest.schema != 1
            || manifest.summary.save_id != save_id
            || manifest.summary.backup_id != backup_id
        {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "pending recovery manifest identity mismatch",
            ));
        }
        validate_backup_manifest_lengths(&manifest)?;
        let restore_campaign = read_regular_file(
            &recovery_dir.join("campaign.xml"),
            manifest.summary.revision.campaign.byte_len.get(),
        )?;
        let restore_descriptor = read_regular_file(
            &recovery_dir.join("descriptor.xml"),
            manifest.summary.revision.descriptor.byte_len.get(),
        )?;
        if fingerprint(&restore_campaign) != manifest.summary.revision.campaign
            || fingerprint(&restore_descriptor) != manifest.summary.revision.descriptor
        {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "pending recovery backup hash validation failed",
            ));
        }
        validate_supported_write_pair(&restore_campaign, &restore_descriptor, true)?;

        if journal.copy.is_some() {
            return self.recover_pending_copy(&recovery_dir, &mut journal, manifest.summary);
        }
        let expected_save_id = opaque_id("save", journal.destination.to_string_lossy().as_bytes());
        if expected_save_id != save_id {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "pending recovery destination does not match its save identity",
            ));
        }

        let destination = SaveLocation::from_save_dir(journal.destination.clone());
        ensure_save_inactive(&destination)?;
        ensure_regular_directory(&destination.save_dir)?;
        let mut campaign_file = lock_for_write(&destination.campaign_path)?;
        let mut descriptor_file = match lock_for_write(&destination.descriptor_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = FileExt::unlock(&campaign_file);
                return Err(error);
            }
        };
        let current_campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
        let current_descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
        let emergency_backup = self.create_backup(
            save_id,
            &current_campaign,
            &current_descriptor,
            true,
            "raw emergency backup before recovery",
        )?;

        let campaign_temp = sibling_temp(&destination.campaign_path, "recovery-campaign")?;
        let descriptor_temp = sibling_temp(&destination.descriptor_path, "recovery-descriptor")?;
        write_new_synced(&campaign_temp, &restore_campaign)?;
        write_new_synced(&descriptor_temp, &restore_descriptor)?;
        let final_campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
        let final_descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
        if final_campaign != current_campaign || final_descriptor != current_descriptor {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
            return Err(CoreError::new(
                ErrorCode::StaleSave,
                "live files changed while recovery was being prepared",
            ));
        }
        update_journal_phase(&recovery_dir, &mut journal, "recovery_prepared")?;
        #[cfg(windows)]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        let replacement = replace_pair(
            &campaign_temp,
            &descriptor_temp,
            &destination,
            &current_campaign,
            &current_descriptor,
            ReplacementJournal {
                directory: &recovery_dir,
                record: &mut journal,
                phase_prefix: "recovery",
            },
        );
        #[cfg(not(windows))]
        {
            let _ = FileExt::unlock(&campaign_file);
            let _ = FileExt::unlock(&descriptor_file);
            drop(campaign_file);
            drop(descriptor_file);
        }
        if replacement.is_err() {
            let _ = fs::remove_file(&campaign_temp);
            let _ = fs::remove_file(&descriptor_temp);
        }
        replacement.map_err(|error| {
            if error.code == ErrorCode::RecoveryRequired {
                error
            } else {
                CoreError::new(
                    ErrorCode::RecoveryRequired,
                    format!("recovery replacement did not complete: {error}"),
                )
            }
        })?;
        let (committed_campaign, committed_descriptor) =
            validate_committed_pair(&destination, &restore_campaign, &restore_descriptor).map_err(
                |error| {
                    CoreError::new(
                        ErrorCode::RecoveryRequired,
                        format!("recovered files failed final validation: {error}"),
                    )
                },
            )?;
        complete_or_recovery(&recovery_dir, &journal, "recovered_at_startup")?;
        Ok(ApplyOutcome {
            location: destination,
            revision: ContentRevision {
                campaign: fingerprint(&committed_campaign),
                descriptor: fingerprint(&committed_descriptor),
            },
            backup: Some(emergency_backup),
        })
    }

    fn recover_pending_copy(
        &self,
        recovery_dir: &Path,
        journal: &mut JournalRecord,
        backup: BackupSummary,
    ) -> Result<ApplyOutcome> {
        let copy = journal.copy.clone().ok_or_else(|| {
            CoreError::new(
                ErrorCode::RecoveryRequired,
                "copy recovery journal is incomplete",
            )
        })?;
        if opaque_id("save", copy.source.to_string_lossy().as_bytes()) != journal.save_id {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "copy recovery source does not match its save identity",
            ));
        }
        let destination_parent = journal
            .destination
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::RecoveryRequired,
                    "copy recovery destination has no parent",
                )
            })?;
        if copy.staging.parent() != Some(destination_parent.as_path())
            || copy.source == journal.destination
            || !copy
                .staging
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".ludds-blessing-copy-"))
            || !journal
                .destination
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("save_"))
        {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "copy recovery paths fail scope validation",
            ));
        }
        ensure_regular_directory(&destination_parent)?;
        let source = SaveLocation::from_save_dir(copy.source.clone());
        ensure_save_inactive(&source)?;
        let destination = SaveLocation::from_save_dir(journal.destination.clone());
        ensure_save_inactive(&destination)?;
        let staging = SaveLocation::from_save_dir(copy.staging.clone());
        let destination_exists = journal.destination.exists();
        let staging_exists = copy.staging.exists();
        if destination_exists && staging_exists {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "both staged and published save-copy directories exist",
            ));
        }

        let (location, committed_campaign, committed_descriptor) = if destination_exists {
            let (campaign, descriptor) =
                read_pair_at_revision(&destination, &copy.expected_revision)?;
            let committed = validate_committed_pair(&destination, &campaign, &descriptor)?;
            update_journal_phase(recovery_dir, journal, "copy_recovery_validated_published")?;
            (destination, committed.0, committed.1)
        } else if staging_exists {
            match read_pair_at_revision(&staging, &copy.expected_revision) {
                Ok((campaign, descriptor)) => {
                    update_journal_phase(recovery_dir, journal, "copy_recovery_staged_validated")?;
                    ensure_save_inactive(&source)?;
                    ensure_save_inactive(&destination)?;
                    fs::rename(&copy.staging, &journal.destination)?;
                    sync_directory(&destination_parent)?;
                    update_journal_phase(recovery_dir, journal, "copy_recovery_published")?;
                    let committed = validate_committed_pair(&destination, &campaign, &descriptor)?;
                    (destination, committed.0, committed.1)
                }
                Err(_) => {
                    // A crash before both staged files were durably written is
                    // safely abortable because Save Copy never mutates its
                    // source. Only the exact, scoped staging directory and its
                    // two allowed regular files may be removed.
                    let (campaign, descriptor) = read_locked_supported_pair(&source)?;
                    cleanup_copy_staging(&copy.staging)?;
                    update_journal_phase(
                        recovery_dir,
                        journal,
                        "copy_recovery_discarded_incomplete_staging",
                    )?;
                    (source, campaign, descriptor)
                }
            }
        } else {
            let (campaign, descriptor) = read_locked_supported_pair(&source)?;
            update_journal_phase(recovery_dir, journal, "copy_recovery_aborted")?;
            (source, campaign, descriptor)
        };
        complete_or_recovery(recovery_dir, journal, "copy_recovered_at_startup")?;
        Ok(ApplyOutcome {
            location,
            revision: ContentRevision {
                campaign: fingerprint(&committed_campaign),
                descriptor: fingerprint(&committed_descriptor),
            },
            backup: Some(backup),
        })
    }

    fn create_backup(
        &self,
        save_id: &str,
        campaign: &[u8],
        descriptor: &[u8],
        pinned: bool,
        reason: &str,
    ) -> Result<BackupSummary> {
        validate_opaque_component(save_id)?;
        ensure_store_root(&self.root)?;
        let save_root = self.root.join(save_id);
        if !save_root.exists() {
            create_private_directory(&save_root)?;
            sync_directory(&self.root)?;
        }
        ensure_regular_directory(&save_root)?;
        harden_private_directory(&save_root)?;
        let backup_id = format!("backup-{}", Uuid::new_v4().simple());
        let backup_dir = save_root.join(&backup_id);
        create_private_directory(&backup_dir)?;
        sync_directory(&save_root)?;
        let summary = BackupSummary {
            backup_id,
            save_id: save_id.to_owned(),
            created_at_millis: DecimalI64::new(now_millis()?),
            pinned,
            reason: reason.to_owned(),
            revision: ContentRevision {
                campaign: fingerprint(campaign),
                descriptor: fingerprint(descriptor),
            },
        };
        write_new_synced(&backup_dir.join("campaign.xml"), campaign)?;
        write_new_synced(&backup_dir.join("descriptor.xml"), descriptor)?;
        let manifest = BackupManifest {
            schema: 1,
            summary: summary.clone(),
        };
        write_new_synced(
            &backup_dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(summary)
    }
}

/// Creates (or validates) an application-owned directory and restricts it to
/// the current user on Unix. Windows keeps its inherited user-profile ACLs.
///
/// Only the requested directory and directories newly created below it are
/// changed; existing ancestors are never chmodded.
///
/// # Errors
///
/// Returns an error when the path cannot be created, is not a regular
/// directory, or its owner-only permissions cannot be applied.
pub fn ensure_private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;

            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700).create(path)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path)?;
    }
    ensure_regular_directory(path)?;
    harden_private_directory(path)
}

/// Restricts an existing application storage tree without following symlinks.
/// This migrates data written by older releases that relied on the process
/// umask. The bounded walk fails closed on links, special files, or excessive
/// entries rather than changing anything outside the application directory.
///
/// # Errors
///
/// Returns an error when the tree cannot be read, contains a link or special
/// file, exceeds the bounded walk, or owner-only permissions cannot be applied.
pub fn harden_private_storage_tree(root: &Path) -> Result<()> {
    ensure_private_directory(root)?;

    #[cfg(unix)]
    {
        let mut pending = vec![root.to_path_buf()];
        let mut visited = 0_usize;
        while let Some(directory) = pending.pop() {
            harden_private_directory(&directory)?;
            for entry in fs::read_dir(&directory)? {
                visited = visited.checked_add(1).ok_or_else(|| {
                    CoreError::new(
                        ErrorCode::ResourceLimit,
                        "private storage entry counter overflow",
                    )
                })?;
                if visited > MAX_PRIVATE_STORAGE_ENTRIES {
                    return Err(CoreError::new(
                        ErrorCode::ResourceLimit,
                        "private storage entry limit exceeded",
                    ));
                }
                let path = entry?.path();
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() {
                    return Err(CoreError::new(
                        ErrorCode::InvalidPath,
                        "private storage contains a symbolic link",
                    ));
                }
                if metadata.is_dir() {
                    pending.push(path);
                } else if metadata.is_file() {
                    harden_private_file(&path)?;
                } else {
                    return Err(CoreError::new(
                        ErrorCode::InvalidPath,
                        "private storage contains a special file",
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Restricts a regular application-owned file to the current user on Unix.
///
/// # Errors
///
/// Returns an error when the path is not a regular non-symlink file or its
/// permissions cannot be inspected or changed.
pub fn harden_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            "private storage file is not regular",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn harden_private_directory(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(_path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path)?;
    ensure_regular_directory(path)?;
    harden_private_directory(path)
}

fn ensure_store_root(root: &Path) -> Result<()> {
    let created = !root.exists();
    ensure_private_directory(root)?;
    if created {
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn validate_supported_write_pair(
    campaign: &[u8],
    descriptor: &[u8],
    allow_protected: bool,
) -> Result<()> {
    XmlDocument::parse(campaign.to_vec(), XmlLimits::default())?;
    let descriptor = parse_descriptor(
        descriptor.to_vec(),
        XmlLimits {
            max_bytes: 4 * 1024 * 1024,
            max_elements: 100_000,
            ..XmlLimits::default()
        },
    )?;
    if descriptor.metadata.compressed {
        return Err(CoreError::new(
            ErrorCode::UnsupportedCompression,
            "compressed saves cannot be written",
        ));
    }
    if descriptor.metadata.game_version != SUPPORTED_GAME_VERSION
        || descriptor.metadata.save_format != SUPPORTED_SAVE_FORMAT
    {
        return Err(CoreError::new(
            ErrorCode::UnsupportedVersion,
            format!("writes require {SUPPORTED_GAME_VERSION} / format {SUPPORTED_SAVE_FORMAT}"),
        ));
    }
    descriptor.require_complete_write_shape()?;
    if (descriptor.metadata.iron_mode || descriptor.metadata.autosave) && !allow_protected {
        return Err(CoreError::invalid_edit(
            "protected save restore/apply is not authorized for this session",
        ));
    }
    Ok(())
}

fn validate_backup_manifest_lengths(manifest: &BackupManifest) -> Result<()> {
    validate_revision_lengths(&manifest.summary.revision, "backup")
}

fn validate_revision_lengths(revision: &ContentRevision, label: &str) -> Result<()> {
    if revision.campaign.byte_len.get() > XmlLimits::default().max_bytes {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            format!("{label} campaign length exceeds the parser safety limit"),
        ));
    }
    if revision.descriptor.byte_len.get() > 4 * 1024 * 1024 {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            format!("{label} descriptor length exceeds the parser safety limit"),
        ));
    }
    Ok(())
}

fn read_pair_at_revision(
    location: &SaveLocation,
    revision: &ContentRevision,
) -> Result<(Vec<u8>, Vec<u8>)> {
    validate_revision_lengths(revision, "copy journal")?;
    ensure_regular_directory(&location.save_dir)?;
    let campaign = read_regular_file(&location.campaign_path, revision.campaign.byte_len.get())?;
    let descriptor = read_regular_file(
        &location.descriptor_path,
        revision.descriptor.byte_len.get(),
    )?;
    if fingerprint(&campaign) != revision.campaign
        || fingerprint(&descriptor) != revision.descriptor
    {
        return Err(CoreError::new(
            ErrorCode::RecoveryRequired,
            "save-copy bytes do not match the durable transaction journal",
        ));
    }
    validate_supported_write_pair(&campaign, &descriptor, true)?;
    Ok((campaign, descriptor))
}

fn read_locked_supported_pair(location: &SaveLocation) -> Result<(Vec<u8>, Vec<u8>)> {
    ensure_regular_directory(&location.save_dir)?;
    let mut campaign_file = lock_for_write(&location.campaign_path)?;
    let mut descriptor_file = match lock_for_write(&location.descriptor_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = FileExt::unlock(&campaign_file);
            return Err(error);
        }
    };
    let campaign = read_locked(&mut campaign_file, XmlLimits::default().max_bytes)?;
    let descriptor = read_locked(&mut descriptor_file, 4 * 1024 * 1024)?;
    validate_supported_write_pair(&campaign, &descriptor, true)?;
    let _ = FileExt::unlock(&campaign_file);
    let _ = FileExt::unlock(&descriptor_file);
    Ok((campaign, descriptor))
}

fn resolve_recovery_after_restore(
    restored_backup_dir: &Path,
    save_id: &str,
    backup_id: &str,
) -> Result<()> {
    let started = restored_backup_dir.join("transaction-started.json");
    let complete = restored_backup_dir.join("transaction-complete.json");
    if !started.exists() || complete.exists() {
        return Ok(());
    }
    let journal: JournalRecord =
        serde_json::from_slice(&read_regular_file(&started, 1024 * 1024)?)?;
    if journal.schema != 1 || journal.save_id != save_id || journal.backup_id != backup_id {
        return Err(CoreError::new(
            ErrorCode::RecoveryRequired,
            "pending recovery journal does not match the restored backup",
        ));
    }
    complete_or_recovery(restored_backup_dir, &journal, "recovered_by_restore")
}

fn ensure_save_inactive(location: &SaveLocation) -> Result<()> {
    ensure_no_starsector_inprogress_files(location)?;

    #[cfg(windows)]
    {
        ensure_save_inactive_windows(location)
    }

    #[cfg(unix)]
    {
        ensure_save_inactive_unix(location)
    }

    #[cfg(not(any(windows, unix)))]
    {
        ensure_starsector_closed()
    }
}

fn ensure_no_starsector_inprogress_files(location: &SaveLocation) -> Result<()> {
    for file_name in ["campaign.xml.inprogress", "descriptor.xml.inprogress"] {
        let path = location.save_dir.join(file_name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(CoreError::new(
                    ErrorCode::GameRunning,
                    "The selected save has an in-progress Starsector write; wait for it to finish",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CoreError::new(
                    ErrorCode::GameRunning,
                    "The selected save's in-progress write state could not be verified",
                ));
            }
        }
    }
    Ok(())
}

/// Fails closed when any Starsector process is running. This is intended for
/// writes to game-global files, where per-save activity detection is not
/// sufficient.
///
/// # Errors
///
/// Returns `GAME_RUNNING` when Starsector is detected, or an inspection error
/// when process activity cannot be established safely.
pub fn ensure_starsector_closed() -> Result<()> {
    #[cfg(windows)]
    let running = running_starsector_processes()?.found;
    #[cfg(unix)]
    let running = running_starsector_processes_unix()?.found;
    #[cfg(not(any(windows, unix)))]
    let running = starsector_is_running()?;

    if running {
        return Err(CoreError::new(
            ErrorCode::GameRunning,
            "Starsector is running; close the game before changing files it uses",
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn process_looks_like_starsector(executable_name: &str, image_or_command: &str) -> bool {
    let executable_name = executable_name.to_ascii_lowercase();
    let image_or_command = image_or_command.to_ascii_lowercase();
    executable_name.contains("starsector")
        || image_or_command.contains("starsector-core")
        || image_or_command.contains("starsector")
}

#[cfg(windows)]
#[derive(Debug, Default)]
struct WindowsStarsectorProcesses {
    found: bool,
    unresolved: bool,
    install_roots: Vec<PathBuf>,
    topology_valid: Vec<(PathBuf, bool)>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeStarsectorProcessKind {
    Wrapper,
    Jvm,
}

#[cfg(windows)]
#[derive(Debug)]
struct ResolvedStarsectorProcess {
    process_id: u32,
    parent_process_id: u32,
    created_at_ticks: u64,
    install_root: PathBuf,
    kind: NativeStarsectorProcessKind,
}

#[cfg(any(windows, unix))]
#[derive(Debug, Default, PartialEq, Eq)]
struct StarsectorLogActivity {
    saw_start: bool,
    ambiguous: bool,
    possibly_active: Vec<PathBuf>,
    loading: Option<PathBuf>,
    saving: Option<PathBuf>,
}

#[cfg(any(windows, unix))]
impl StarsectorLogActivity {
    fn reset_for_start(&mut self) {
        *self = Self {
            saw_start: true,
            ..Self::default()
        };
    }

    fn references(&self, target: &Path) -> bool {
        self.possibly_active
            .iter()
            .chain(self.loading.iter())
            .chain(self.saving.iter())
            .any(|path| native_path_eq(path, target))
    }

    fn retain_possible_active(&mut self, path: PathBuf) {
        if !self
            .possibly_active
            .iter()
            .any(|existing| native_path_eq(existing, &path))
        {
            self.possibly_active.push(path);
        }
    }
}

#[cfg(any(windows, unix))]
fn game_running_error(message: impl Into<String>) -> CoreError {
    CoreError::new(ErrorCode::GameRunning, message)
}

#[cfg(windows)]
fn ensure_save_inactive_windows(location: &SaveLocation) -> Result<()> {
    let processes = running_starsector_processes()?;
    if !processes.found {
        return Ok(());
    }
    if processes.unresolved || processes.install_roots.is_empty() {
        return Err(game_running_error(
            "Starsector is running, but its installation could not be resolved safely",
        ));
    }

    let target = canonicalize_save_candidate(&location.save_dir).map_err(|_| {
        game_running_error("The selected save path could not be matched safely to Starsector")
    })?;
    if windows_virtual_store_target(&target) {
        return Err(game_running_error(
            "Legacy VirtualStore saves cannot be edited safely while Starsector is running",
        ));
    }
    let mut matching_install = None;
    for install_root in &processes.install_roots {
        let saves_root = crate::resolve_starsector_save_root(install_root).map_err(|_| {
            game_running_error(
                "A running Starsector installation has an unreadable or ambiguous configured save folder",
            )
        })?;
        if target
            .parent()
            .is_some_and(|parent| windows_path_eq(parent, &saves_root))
        {
            if matching_install.is_some() {
                return Err(game_running_error(
                    "The selected save matches more than one running Starsector installation",
                ));
            }
            matching_install = Some(install_root);
        }
    }

    let Some(install_root) = matching_install else {
        // Every Starsector process was resolved and the target is not a direct
        // child of any running installation's saves directory.
        return Ok(());
    };
    let topology_valid = processes
        .topology_valid
        .iter()
        .find(|(root, _)| windows_path_eq(root, install_root))
        .is_some_and(|(_, valid)| *valid);
    if !topology_valid {
        return Err(game_running_error(
            "The running Starsector process topology is not a single verified native session",
        ));
    }
    let activity = read_starsector_log_activity(install_root)?;
    ensure_log_activity_allows_target(&activity, &target)
}

#[cfg(any(windows, unix))]
fn ensure_log_activity_allows_target(
    activity: &StarsectorLogActivity,
    target: &Path,
) -> Result<()> {
    if !activity.saw_start || activity.ambiguous {
        return Err(game_running_error(
            "The running Starsector session's active save could not be determined safely",
        ));
    }
    if activity.loading.is_some() || activity.saving.is_some() {
        return Err(game_running_error(
            "Starsector is currently loading or saving; wait for that operation to finish",
        ));
    }
    if activity.possibly_active.is_empty() {
        return Err(game_running_error(
            "The running Starsector session has not established an active save safely",
        ));
    }
    if activity.references(target) {
        return Err(game_running_error(
            "The selected save is loaded, loading, or being saved by Starsector",
        ));
    }
    Ok(())
}

#[cfg(any(windows, unix))]
fn canonicalize_save_candidate(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "save path has no parent")
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "save path has no name")
    })?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}

#[cfg(windows)]
fn native_path_eq(left: &Path, right: &Path) -> bool {
    windows_path_eq(left, right)
}

#[cfg(unix)]
fn native_path_eq(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn windows_path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(windows)]
fn windows_path_is_within(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !path_component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&root_component.as_os_str().to_string_lossy())
        {
            return false;
        }
    }
    true
}

#[cfg(windows)]
fn windows_virtual_store_target(target: &Path) -> bool {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return false;
    };
    let root = local_app_data.join("VirtualStore");
    let root = fs::canonicalize(&root).unwrap_or(root);
    windows_path_is_within(target, &root)
}

#[cfg(windows)]
fn starsector_install_root_from_image(image: &Path) -> Option<PathBuf> {
    image.parent()?.ancestors().take(12).find_map(|ancestor| {
        let core = ancestor.join("starsector-core");
        let wrapper = ancestor.join("starsector.exe");
        let jvm = ancestor.join("jre/bin/java.exe");
        if core.is_dir() && wrapper.is_file() && jvm.is_file() {
            fs::canonicalize(ancestor).ok()
        } else {
            None
        }
    })
}

#[cfg(windows)]
fn push_unique_windows_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths
        .iter()
        .any(|existing| windows_path_eq(existing, &candidate))
    {
        paths.push(candidate);
    }
}

#[cfg(windows)]
fn classify_native_starsector_process(
    install_root: &Path,
    image: &Path,
) -> Option<NativeStarsectorProcessKind> {
    let image = fs::canonicalize(image).ok()?;
    let wrapper = fs::canonicalize(install_root.join("starsector.exe")).ok()?;
    if windows_path_eq(&image, &wrapper) {
        return Some(NativeStarsectorProcessKind::Wrapper);
    }
    let jvm = fs::canonicalize(install_root.join("jre/bin/java.exe")).ok()?;
    windows_path_eq(&image, &jvm).then_some(NativeStarsectorProcessKind::Jvm)
}

#[cfg(windows)]
fn has_exact_native_starsector_topology(
    processes: &[ResolvedStarsectorProcess],
    install_root: &Path,
) -> bool {
    let members = processes
        .iter()
        .filter(|process| windows_path_eq(&process.install_root, install_root))
        .collect::<Vec<_>>();
    let wrappers = members
        .iter()
        .filter(|process| process.kind == NativeStarsectorProcessKind::Wrapper)
        .copied()
        .collect::<Vec<_>>();
    let jvms = members
        .iter()
        .filter(|process| process.kind == NativeStarsectorProcessKind::Jvm)
        .copied()
        .collect::<Vec<_>>();
    wrappers.len() == 1
        && jvms.len() == 1
        && jvms[0].parent_process_id == wrappers[0].process_id
        && jvms[0].created_at_ticks >= wrappers[0].created_at_ticks
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn running_starsector_processes() -> Result<WindowsStarsectorProcesses> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: Toolhelp receives a correctly sized, zero-initialized structure.
    // Every non-null snapshot/process handle is closed before this function
    // returns, and all UTF-16 buffers remain valid for their respective calls.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut result = WindowsStarsectorProcesses::default();
        let mut resolved = Vec::new();
        let mut present = Process32FirstW(snapshot, &raw mut entry) != 0;
        while present {
            let name_end = entry
                .szExeFile
                .iter()
                .position(|code_unit| *code_unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..name_end]);
            let mut image = String::new();
            let mut created_at_ticks = None;
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
            if !process.is_null() {
                let mut image_buffer = vec![0_u16; 32_768];
                let mut image_len = image_buffer.len() as u32;
                if QueryFullProcessImageNameW(
                    process,
                    0,
                    image_buffer.as_mut_ptr(),
                    &raw mut image_len,
                ) != 0
                {
                    image = String::from_utf16_lossy(&image_buffer[..image_len as usize]);
                }
                let mut creation: FILETIME = zeroed();
                let mut exit: FILETIME = zeroed();
                let mut kernel: FILETIME = zeroed();
                let mut user: FILETIME = zeroed();
                if GetProcessTimes(
                    process,
                    &raw mut creation,
                    &raw mut exit,
                    &raw mut kernel,
                    &raw mut user,
                ) != 0
                {
                    created_at_ticks = Some(
                        (u64::from(creation.dwHighDateTime) << 32)
                            | u64::from(creation.dwLowDateTime),
                    );
                }
                CloseHandle(process);
            }
            if process_looks_like_starsector(&name, &image) {
                result.found = true;
                if image.is_empty() {
                    result.unresolved = true;
                } else if let Some(root) = starsector_install_root_from_image(Path::new(&image)) {
                    if let (Some(kind), Some(created_at_ticks)) = (
                        classify_native_starsector_process(&root, Path::new(&image)),
                        created_at_ticks,
                    ) {
                        push_unique_windows_path(&mut result.install_roots, root.clone());
                        resolved.push(ResolvedStarsectorProcess {
                            process_id: entry.th32ProcessID,
                            parent_process_id: entry.th32ParentProcessID,
                            created_at_ticks,
                            install_root: root,
                            kind,
                        });
                    } else {
                        result.unresolved = true;
                    }
                } else {
                    result.unresolved = true;
                }
            }
            present = Process32NextW(snapshot, &raw mut entry) != 0;
        }
        CloseHandle(snapshot);
        result.topology_valid = result
            .install_roots
            .iter()
            .map(|root| {
                (
                    root.clone(),
                    has_exact_native_starsector_topology(&resolved, root),
                )
            })
            .collect();
        if result.topology_valid.iter().any(|(_, valid)| !valid) {
            result.unresolved = true;
        }
        Ok(result)
    }
}

#[cfg(any(windows, unix))]
fn read_starsector_log_activity(install_root: &Path) -> Result<StarsectorLogActivity> {
    let log_root = starsector_log_root(install_root)?;
    let paths = [
        log_root.join("starsector.log.3"),
        log_root.join("starsector.log.2"),
        log_root.join("starsector.log.1"),
        log_root.join("starsector.log"),
    ];
    if !paths[3].exists() {
        return Err(game_running_error(
            "The running Starsector installation has no current activity log",
        ));
    }
    if (!paths[2].exists() && (paths[1].exists() || paths[0].exists()))
        || (!paths[1].exists() && paths[0].exists())
    {
        return Err(game_running_error(
            "The Starsector activity log rotation chain has a gap",
        ));
    }

    let mut total = 0_u64;
    let mut snapshots = Vec::new();
    // Open the current file first. If a rotation occurs while the older files
    // are being opened, this handle still contains every activity event that
    // existed at the start of the probe instead of silently losing the newest
    // segment to a renamed path.
    for index in [3_usize, 2, 1, 0] {
        let path = &paths[index];
        if !path.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            game_running_error("A Starsector activity log could not be inspected safely")
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(game_running_error(
                "A Starsector activity log is not a regular file",
            ));
        }
        if metadata.len() > MAX_STARSECTOR_LOG_FILE_BYTES {
            return Err(game_running_error(
                "A Starsector activity log exceeds the safety limit",
            ));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| game_running_error("Starsector activity log size overflowed"))?;
        if total > MAX_STARSECTOR_LOG_TOTAL_BYTES {
            return Err(game_running_error(
                "The Starsector activity log chain exceeds the safety limit",
            ));
        }
        let file = File::open(path)
            .map_err(|_| game_running_error("A Starsector activity log could not be opened"))?;
        snapshots.push((index, file, metadata.len()));
    }
    snapshots.sort_by_key(|(index, _, _)| *index);

    let mut activity = StarsectorLogActivity::default();
    for (_, file, length) in snapshots {
        let mut bytes = Vec::with_capacity(usize::try_from(length).map_err(|_| {
            game_running_error("A Starsector activity log cannot fit in memory safely")
        })?);
        file.take(MAX_STARSECTOR_LOG_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| game_running_error("A Starsector activity log could not be read"))?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_STARSECTOR_LOG_FILE_BYTES)
        {
            return Err(game_running_error(
                "A Starsector activity log grew beyond the safety limit",
            ));
        }
        parse_starsector_log_snapshot(&bytes, install_root, &mut activity)?;
    }
    if !activity.saw_start {
        return Err(game_running_error(
            "The current Starsector launch marker is missing from the bounded log chain",
        ));
    }
    Ok(activity)
}

#[cfg(windows)]
fn starsector_log_root(install_root: &Path) -> Result<PathBuf> {
    Ok(install_root.join("starsector-core"))
}

#[cfg(target_os = "linux")]
fn starsector_log_root(install_root: &Path) -> Result<PathBuf> {
    Ok(install_root.to_path_buf())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn starsector_log_root(install_root: &Path) -> Result<PathBuf> {
    let direct = install_root.join("starsector.log");
    let nested_root = install_root.join("logs");
    let nested = nested_root.join("starsector.log");
    match (direct.exists(), nested.exists()) {
        (true, true) => Err(game_running_error(
            "The running Starsector installation has ambiguous activity-log locations",
        )),
        (true, false) => Ok(install_root.to_path_buf()),
        (false, _) => Ok(nested_root),
    }
}

#[cfg(any(windows, unix))]
fn parse_starsector_log_snapshot(
    bytes: &[u8],
    install_root: &Path,
    activity: &mut StarsectorLogActivity,
) -> Result<()> {
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let trailing = &bytes[complete_len..];
    if trailing_fragment_may_be_activity(trailing) {
        return Err(game_running_error(
            "A Starsector save-activity log line is incomplete",
        ));
    }
    let complete = std::str::from_utf8(&bytes[..complete_len])
        .map_err(|_| game_running_error("A Starsector activity log contains malformed UTF-8"))?;
    for line in complete.lines() {
        parse_starsector_log_line(line.trim_end_matches('\r'), install_root, activity)?;
    }
    Ok(())
}

#[cfg(any(windows, unix))]
fn trailing_fragment_may_be_activity(fragment: &[u8]) -> bool {
    !fragment.is_empty()
}

#[cfg(any(windows, unix))]
struct StarsectorLogRecord<'a> {
    logger: &'a str,
    message: &'a str,
}

#[cfg(any(windows, unix))]
fn parse_starsector_log_record(line: &str) -> Option<StarsectorLogRecord<'_>> {
    let timestamp_end = line.find(char::is_whitespace)?;
    if timestamp_end == 0
        || !line[..timestamp_end]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let after_timestamp = line[timestamp_end..].trim_start_matches(char::is_whitespace);
    let after_open = after_timestamp.strip_prefix('[')?;
    let thread_end = after_open.find(']')?;
    if thread_end == 0 {
        return None;
    }
    let after_thread = after_open[thread_end + 1..].strip_prefix(' ')?;
    let level_end = after_thread.find(char::is_whitespace)?;
    let level = &after_thread[..level_end];
    if !matches!(
        level,
        "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR" | "FATAL"
    ) {
        return None;
    }
    let after_level = after_thread[level_end..].strip_prefix("  ")?;
    let logger_end = after_level.find("  - ")?;
    let logger = &after_level[..logger_end];
    if logger.is_empty() || logger.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    Some(StarsectorLogRecord {
        logger,
        message: &after_level[logger_end + 4..],
    })
}

#[cfg(any(windows, unix))]
fn parse_starsector_log_line(
    line: &str,
    install_root: &Path,
    activity: &mut StarsectorLogActivity,
) -> Result<()> {
    const LAUNCHER: &str = "com.fs.starfarer.StarfarerLauncher";
    const CAMPAIGN: &str = "com.fs.starfarer.campaign.save.CampaignGameManager";

    let Some(record) = parse_starsector_log_record(line) else {
        if line.contains(LAUNCHER) || line.contains(CAMPAIGN) {
            activity.ambiguous = true;
        }
        return Ok(());
    };
    if record.logger == LAUNCHER {
        let message = record.message;
        if message.starts_with("Starting Starsector ") && message.ends_with(" launcher") {
            activity.reset_for_start();
        } else if message.starts_with("Starting Starsector") {
            activity.ambiguous = true;
        }
        return Ok(());
    }
    if record.logger != CAMPAIGN {
        return Ok(());
    }
    let message = record.message;
    if !activity.saw_start {
        return Ok(());
    }

    if let Some(raw_path) = exact_ellipsis_path(message, "Loading ") {
        if activity.loading.is_some() || activity.saving.is_some() {
            activity.ambiguous = true;
            return Ok(());
        }
        activity.loading = resolve_logged_save_path(install_root, raw_path);
        if activity.loading.is_none() {
            activity.ambiguous = true;
        }
    } else if message == "Loading stage 39 - last" {
        if activity.saving.is_some() {
            activity.ambiguous = true;
        }
        match activity.loading.take() {
            Some(path) => activity.retain_possible_active(path),
            None => activity.ambiguous = true,
        }
    } else if message.starts_with("Loading ") && !message.starts_with("Loading stage ") {
        activity.ambiguous = true;
    } else if let Some(raw_path) = exact_ellipsis_path(message, "Saving to ") {
        if activity.saving.is_some() || activity.loading.is_some() {
            activity.ambiguous = true;
            return Ok(());
        }
        activity.saving = resolve_logged_save_path(install_root, raw_path);
        if activity.saving.is_none() {
            activity.ambiguous = true;
        }
    } else if message.starts_with("Saving to ") {
        activity.ambiguous = true;
    } else if message == "Finished saving" {
        match activity.saving.take() {
            Some(path) => activity.retain_possible_active(path),
            None => activity.ambiguous = true,
        }
    }
    Ok(())
}

#[cfg(any(windows, unix))]
fn exact_ellipsis_path<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    let path = message.strip_prefix(prefix)?.strip_suffix("...")?;
    (!path.is_empty()).then_some(path)
}

#[cfg(any(windows, unix))]
fn resolve_logged_save_path(install_root: &Path, raw_path: &str) -> Option<PathBuf> {
    if raw_path.contains('\0') {
        return None;
    }
    #[cfg(windows)]
    let platform_path = raw_path.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);
    #[cfg(unix)]
    let platform_path = {
        if raw_path.contains('\\') {
            return None;
        }
        raw_path.to_owned()
    };
    let logged = PathBuf::from(platform_path);
    let candidate = if logged.is_absolute() {
        logged
    } else {
        starsector_process_working_root(install_root).join(logged)
    };
    let candidate = canonicalize_save_candidate(&candidate).ok()?;
    let saves_root = crate::resolve_starsector_save_root(install_root).ok()?;
    candidate
        .parent()
        .is_some_and(|parent| native_path_eq(parent, &saves_root))
        .then_some(candidate)
}

#[cfg(windows)]
fn starsector_process_working_root(install_root: &Path) -> PathBuf {
    install_root.join("starsector-core")
}

#[cfg(unix)]
fn starsector_process_working_root(install_root: &Path) -> PathBuf {
    install_root.to_path_buf()
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct UnixStarsectorProcesses {
    found: bool,
    unresolved: bool,
    install_roots: Vec<PathBuf>,
    session_counts: Vec<(PathBuf, usize)>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnixProcessEvidence {
    Launcher,
    NamedWrapper,
}

#[cfg(unix)]
#[derive(Debug)]
struct ObservedUnixProcess {
    evidence: UnixProcessEvidence,
    install_root: Option<PathBuf>,
}

#[cfg(unix)]
fn ensure_save_inactive_unix(location: &SaveLocation) -> Result<()> {
    let processes = running_starsector_processes_unix()?;
    if !processes.found {
        return Ok(());
    }
    if processes.unresolved || processes.install_roots.is_empty() {
        return Err(game_running_error(
            "Starsector is running, but its native installation could not be resolved safely",
        ));
    }

    let target = canonicalize_save_candidate(&location.save_dir).map_err(|_| {
        game_running_error("The selected save path could not be matched safely to Starsector")
    })?;
    let mut matching_install = None;
    for install_root in &processes.install_roots {
        let saves_root = crate::resolve_starsector_save_root(install_root).map_err(|_| {
            game_running_error(
                "A running Starsector installation has an unreadable configured save folder",
            )
        })?;
        if target.parent().is_some_and(|parent| parent == saves_root) {
            if matching_install.is_some() {
                return Err(game_running_error(
                    "The selected save matches more than one running Starsector installation",
                ));
            }
            matching_install = Some(install_root);
        }
    }

    let Some(install_root) = matching_install else {
        // All observed native sessions were resolved and the target is not a
        // direct child of any running installation's save root.
        return Ok(());
    };
    let exact_session = processes
        .session_counts
        .iter()
        .find(|(root, _)| root == install_root)
        .is_some_and(|(_, count)| *count == 1);
    if !exact_session {
        return Err(game_running_error(
            "The running Starsector process topology is not a single verified native session",
        ));
    }
    let activity = read_starsector_log_activity(install_root)?;
    ensure_log_activity_allows_target(&activity, &target)
}

#[cfg(unix)]
fn classify_unix_process(executable_name: &str, arguments: &[&str]) -> Option<UnixProcessEvidence> {
    const LAUNCHER: &str = "com.fs.starfarer.StarfarerLauncher";
    if arguments.contains(&LAUNCHER) {
        return Some(UnixProcessEvidence::Launcher);
    }
    let executable = executable_name.to_ascii_lowercase();
    matches!(
        executable.as_str(),
        "starsector" | "starsector.sh" | "starsector_mac.sh"
    )
    .then_some(UnixProcessEvidence::NamedWrapper)
}

#[cfg(unix)]
fn unix_installation_root_from_working_directory(path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    let root = fs::canonicalize(path).ok()?;
    for regular in [
        root.join("data/config/settings.json"),
        root.join("starfarer.api.jar"),
        root.join("starfarer_obf.jar"),
    ] {
        let metadata = fs::symlink_metadata(regular).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return None;
        }
    }
    crate::resolve_starsector_save_root(&root).ok()?;
    Some(root)
}

#[cfg(unix)]
fn unix_launcher_save_root_matches(arguments: &[&str], installation_root: &Path) -> bool {
    const PROPERTY: &str = "-Dcom.fs.starfarer.settings.paths.saves=";
    let mut configured = None;
    for argument in arguments {
        if let Some(value) = argument.strip_prefix(PROPERTY) {
            if value.is_empty() || value.contains(['\0', '\\']) || configured.is_some() {
                return false;
            }
            configured = Some(value);
        } else if argument.starts_with("-Dcom.fs.starfarer.settings.paths.saves") {
            return false;
        }
    }
    let Some(configured) = configured else {
        return false;
    };
    let configured = PathBuf::from(configured);
    let candidate = if configured.is_absolute() {
        configured
    } else {
        installation_root.join(configured)
    };
    let Ok(candidate) = fs::canonicalize(candidate) else {
        return false;
    };
    crate::resolve_starsector_save_root(installation_root)
        .is_ok_and(|expected| candidate == expected)
}

#[cfg(unix)]
fn assemble_unix_processes(observed: Vec<ObservedUnixProcess>) -> UnixStarsectorProcesses {
    let mut result = UnixStarsectorProcesses {
        found: !observed.is_empty(),
        ..UnixStarsectorProcesses::default()
    };
    for process in observed
        .iter()
        .filter(|process| process.evidence == UnixProcessEvidence::Launcher)
    {
        let Some(root) = process.install_root.as_ref() else {
            result.unresolved = true;
            continue;
        };
        if !result.install_roots.iter().any(|existing| existing == root) {
            result.install_roots.push(root.clone());
            result.session_counts.push((root.clone(), 1));
        } else if let Some((_, count)) = result
            .session_counts
            .iter_mut()
            .find(|(existing, _)| existing == root)
        {
            *count = count.saturating_add(1);
            result.unresolved = true;
        }
    }
    if result.install_roots.is_empty() && result.found {
        result.unresolved = true;
    }
    for wrapper in observed
        .iter()
        .filter(|process| process.evidence == UnixProcessEvidence::NamedWrapper)
    {
        if !wrapper.install_root.as_ref().is_some_and(|root| {
            result
                .install_roots
                .iter()
                .any(|installation| installation == root)
        }) {
            result.unresolved = true;
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn running_starsector_processes_unix() -> Result<UnixStarsectorProcesses> {
    const MAX_PROCESS_COMMAND_BYTES: u64 = 64 * 1024;
    let mut observed = Vec::new();
    for (index, entry) in fs::read_dir("/proc")?.enumerate() {
        if index >= 262_144 {
            return Err(CoreError::new(
                ErrorCode::ResourceLimit,
                "process scan limit exceeded",
            ));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }

        let command_path = entry.path().join("cmdline");
        let command = match File::open(&command_path) {
            Ok(file) => {
                let mut command = Vec::new();
                file.take(MAX_PROCESS_COMMAND_BYTES + 1)
                    .read_to_end(&mut command)?;
                if u64::try_from(command.len())
                    .map_or(true, |length| length > MAX_PROCESS_COMMAND_BYTES)
                {
                    Vec::new()
                } else {
                    command
                }
            }
            Err(_) => Vec::new(),
        };
        let arguments = command
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>();
        let argument_refs = arguments.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
        let command_name = argument_refs
            .first()
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let comm = fs::read_to_string(entry.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let executable_name = if command_name.is_empty() {
            comm.as_str()
        } else {
            command_name
        };
        let Some(evidence) = classify_unix_process(executable_name, &argument_refs) else {
            continue;
        };
        let install_root = fs::canonicalize(entry.path().join("cwd"))
            .ok()
            .and_then(|cwd| unix_installation_root_from_working_directory(&cwd));
        let install_root = if evidence == UnixProcessEvidence::Launcher {
            install_root.filter(|root| unix_launcher_save_root_matches(&argument_refs, root))
        } else {
            install_root
        };
        observed.push(ObservedUnixProcess {
            evidence,
            install_root,
        });
    }
    Ok(assemble_unix_processes(observed))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn running_starsector_processes_unix() -> Result<UnixStarsectorProcesses> {
    use std::process::Command;

    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()?;
    if !output.status.success() {
        return Err(CoreError::new(
            ErrorCode::Io,
            "could not inspect running processes",
        ));
    }
    if output.stdout.len() > 16 * 1024 * 1024 {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            "process listing exceeds safety limit",
        ));
    }
    let listing = std::str::from_utf8(&output.stdout).map_err(|_| {
        CoreError::new(
            ErrorCode::ValidationFailed,
            "process listing contains malformed UTF-8",
        )
    })?;
    let mut observed = Vec::new();
    for line in listing.lines() {
        let line = line.trim_start();
        let Some(command_start) = line.find(char::is_whitespace) else {
            continue;
        };
        if !line[..command_start]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let command = line[command_start..].trim_start();
        let arguments = command.split_ascii_whitespace().collect::<Vec<_>>();
        let executable_name = arguments
            .first()
            .map(|value| value.trim_matches(['\'', '"']))
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let Some(evidence) = classify_unix_process(executable_name, &arguments) else {
            continue;
        };
        let install_root = macos_installation_root_from_command(command);
        let install_root = if evidence == UnixProcessEvidence::Launcher {
            install_root.filter(|root| unix_launcher_save_root_matches(&arguments, root))
        } else {
            install_root
        };
        observed.push(ObservedUnixProcess {
            evidence,
            install_root,
        });
    }
    Ok(assemble_unix_processes(observed))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn macos_installation_root_from_command(command: &str) -> Option<PathBuf> {
    const MARKER: &str = ".app/Contents/";
    let mut candidates = Vec::new();
    let mut search_from = 0;
    while let Some(relative) = command[search_from..].find(MARKER) {
        let app_end = search_from + relative + ".app".len();
        for start in std::iter::once(0).chain(command[..app_end].char_indices().filter_map(
            |(index, character)| {
                (character.is_whitespace() || matches!(character, '\'' | '"'))
                    .then_some(index + character.len_utf8())
            },
        )) {
            let raw = command[start..app_end].trim_matches(['\'', '"']);
            if raw.is_empty() {
                continue;
            }
            let java = PathBuf::from(raw)
                .join("Contents")
                .join("Resources")
                .join("Java");
            if let Some(root) = unix_installation_root_from_working_directory(&java) {
                if !candidates.iter().any(|existing| existing == &root) {
                    candidates.push(root);
                }
            }
        }
        search_from = app_end;
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

#[cfg(not(any(unix, windows)))]
fn starsector_is_running() -> Result<bool> {
    Err(CoreError::new(
        ErrorCode::ValidationFailed,
        "in-place writes are unavailable because process detection is unsupported",
    ))
}

fn lock_for_write(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            "save file is not regular",
        ));
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock_exclusive().map_err(|_| {
        CoreError::new(
            ErrorCode::GameRunning,
            "the selected save file is in use; wait for Starsector to finish with this save",
        )
    })?;
    Ok(file)
}

fn read_locked(file: &mut File, max_bytes: u64) -> Result<Vec<u8>> {
    let length = file.metadata()?.len();
    if length > max_bytes {
        return Err(CoreError::new(
            ErrorCode::StaleSave,
            "locked save file size changed",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let capacity = usize::try_from(length).map_err(|_| {
        CoreError::new(
            ErrorCode::ResourceLimit,
            "save file length exceeds this platform's address space",
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let read_limit = max_bytes.saturating_add(1);
    (&mut *file).take(read_limit).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > max_bytes) {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            "locked save file grew beyond the configured size limit while reading",
        ));
    }
    Ok(bytes)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn cleanup_copy_staging(staging: &Path) -> Result<()> {
    if !staging.exists() {
        return Ok(());
    }
    ensure_regular_directory(staging)?;
    let mut entries = Vec::new();
    for (index, entry) in fs::read_dir(staging)?.enumerate() {
        if index >= 3 {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "save-copy staging contains unexpected files",
            ));
        }
        let entry = entry?;
        let name = entry.file_name();
        if name != "campaign.xml" && name != "descriptor.xml" {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "save-copy staging contains an unexpected file",
            ));
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                "save-copy staging member is not a regular file",
            ));
        }
        entries.push(entry.path());
    }
    for entry in entries {
        fs::remove_file(entry)?;
    }
    fs::remove_dir(staging)?;
    if let Some(parent) = staging.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // FlushFileBuffers is already issued for every file. Windows directory
    // handles require special open flags; NTFS journals directory operations.
    Ok(())
}

fn sibling_temp(target: &Path, kind: &str) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| CoreError::new(ErrorCode::InvalidPath, "save has no parent directory"))?;
    Ok(parent.join(format!(
        ".ludds-blessing-{kind}-{}.tmp",
        Uuid::new_v4().simple()
    )))
}

fn validate_committed_pair(
    location: &SaveLocation,
    expected_campaign: &[u8],
    expected_descriptor: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    ensure_save_inactive(location)?;
    let mut campaign_file = lock_for_write(&location.campaign_path)?;
    let mut descriptor_file = match lock_for_write(&location.descriptor_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = FileExt::unlock(&campaign_file);
            return Err(error);
        }
    };
    let campaign = read_locked(
        &mut campaign_file,
        u64::try_from(expected_campaign.len())
            .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "campaign size overflow"))?,
    )?;
    let descriptor = read_locked(
        &mut descriptor_file,
        u64::try_from(expected_descriptor.len())
            .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "descriptor size overflow"))?,
    )?;
    if campaign != expected_campaign || descriptor != expected_descriptor {
        return Err(CoreError::validation(
            "committed bytes differ from the validated output",
        ));
    }
    XmlDocument::parse(campaign.clone(), XmlLimits::default())?;
    parse_descriptor(
        descriptor.clone(),
        XmlLimits {
            max_bytes: 4 * 1024 * 1024,
            max_elements: 100_000,
            ..XmlLimits::default()
        },
    )?;
    let _ = FileExt::unlock(&campaign_file);
    let _ = FileExt::unlock(&descriptor_file);
    Ok((campaign, descriptor))
}

struct ReplacementJournal<'a> {
    directory: &'a Path,
    record: &'a mut JournalRecord,
    phase_prefix: &'a str,
}

fn rollback_pair(
    destination: &SaveLocation,
    desired_campaign: &[u8],
    desired_descriptor: &[u8],
    fallback_campaign: &[u8],
    fallback_descriptor: &[u8],
    journal: ReplacementJournal<'_>,
) -> Result<()> {
    let campaign_temp = sibling_temp(&destination.campaign_path, "rollback-campaign")?;
    let descriptor_temp = sibling_temp(&destination.descriptor_path, "rollback-descriptor")?;
    write_new_synced(&campaign_temp, desired_campaign)?;
    write_new_synced(&descriptor_temp, desired_descriptor)?;
    replace_pair(
        &campaign_temp,
        &descriptor_temp,
        destination,
        fallback_campaign,
        fallback_descriptor,
        journal,
    )
}

fn rewrite_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            "transaction journal is not a regular file",
        ));
    }
    let replacement = sibling_temp(path, "journal")?;
    let result = (|| -> Result<()> {
        write_new_synced(&replacement, bytes)?;
        replace_file_atomically(&replacement, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&replacement);
    }
    result
}

fn update_journal_phase(
    backup_dir: &Path,
    journal: &mut JournalRecord,
    phase: impl Into<String>,
) -> Result<()> {
    let mut updated = journal.clone();
    updated.phase = phase.into();
    rewrite_synced(
        &backup_dir.join("transaction-started.json"),
        &serde_json::to_vec_pretty(&updated)?,
    )?;
    *journal = updated;
    Ok(())
}

fn record_replacement_phase(
    backup_dir: &Path,
    journal: &mut JournalRecord,
    phase: String,
) -> Result<()> {
    update_journal_phase(backup_dir, journal, phase).map_err(|error| {
        CoreError::new(
            ErrorCode::RecoveryRequired,
            format!("live files changed but the transaction phase could not be recorded: {error}"),
        )
    })
}

fn mark_transaction_complete(
    backup_dir: &Path,
    journal: &JournalRecord,
    phase: &str,
) -> Result<()> {
    let complete = JournalRecord {
        phase: phase.to_owned(),
        ..journal.clone()
    };
    write_new_synced(
        &backup_dir.join("transaction-complete.json"),
        &serde_json::to_vec_pretty(&complete)?,
    )
}

fn complete_or_recovery(backup_dir: &Path, journal: &JournalRecord, phase: &str) -> Result<()> {
    mark_transaction_complete(backup_dir, journal, phase).map_err(|error| {
        CoreError::new(
            ErrorCode::RecoveryRequired,
            format!("transaction state is known but its durable completion marker failed: {error}"),
        )
    })
}

fn replace_pair(
    campaign_temp: &Path,
    descriptor_temp: &Path,
    destination: &SaveLocation,
    rollback_campaign: &[u8],
    rollback_descriptor: &[u8],
    journal: ReplacementJournal<'_>,
) -> Result<()> {
    ensure_save_inactive(destination)?;
    replace_file_atomically(campaign_temp, &destination.campaign_path)?;
    record_replacement_phase(
        journal.directory,
        journal.record,
        format!("{}_campaign_replaced", journal.phase_prefix),
    )?;
    if let Err(activity_error) = ensure_save_inactive(destination) {
        return Err(CoreError::new(
            ErrorCode::RecoveryRequired,
            format!(
                "the selected save became active after campaign.xml was replaced: {activity_error}"
            ),
        ));
    }
    if let Err(descriptor_error) =
        replace_file_atomically(descriptor_temp, &destination.descriptor_path)
    {
        if let Err(activity_error) = ensure_save_inactive(destination) {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!(
                    "the selected save became active during partial replacement: {activity_error}"
                ),
            ));
        }
        let rollback_campaign_temp = sibling_temp(&destination.campaign_path, "rollback-campaign")?;
        let rollback_descriptor_temp =
            sibling_temp(&destination.descriptor_path, "rollback-descriptor")?;
        write_new_synced(&rollback_campaign_temp, rollback_campaign)?;
        write_new_synced(&rollback_descriptor_temp, rollback_descriptor)?;
        if let Err(activity_error) = ensure_save_inactive(destination) {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!(
                    "the selected save became active before partial-replacement rollback: {activity_error}"
                ),
            ));
        }
        let campaign_result =
            replace_file_atomically(&rollback_campaign_temp, &destination.campaign_path);
        let campaign_phase_result = if campaign_result.is_ok() {
            record_replacement_phase(
                journal.directory,
                journal.record,
                format!("{}_rollback_campaign_replaced", journal.phase_prefix),
            )
        } else {
            Ok(())
        };
        if let Err(activity_error) = ensure_save_inactive(destination) {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!(
                    "the selected save became active between rollback replacements: {activity_error}"
                ),
            ));
        }
        let descriptor_result =
            replace_file_atomically(&rollback_descriptor_temp, &destination.descriptor_path);
        let descriptor_phase_result = if descriptor_result.is_ok() {
            record_replacement_phase(
                journal.directory,
                journal.record,
                format!("{}_rollback_descriptor_replaced", journal.phase_prefix),
            )
        } else {
            Ok(())
        };
        if let Err(activity_error) = ensure_save_inactive(destination) {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!(
                    "the selected save became active while rollback was being validated: {activity_error}"
                ),
            ));
        }
        if campaign_result.is_err()
            || descriptor_result.is_err()
            || campaign_phase_result.is_err()
            || descriptor_phase_result.is_err()
        {
            return Err(CoreError::new(
                ErrorCode::RecoveryRequired,
                format!(
                    "partial replacement, rollback, or durable phase update failed: {descriptor_error}"
                ),
            ));
        }
        return Err(descriptor_error);
    }
    record_replacement_phase(
        journal.directory,
        journal.record,
        format!("{}_descriptor_replaced", journal.phase_prefix),
    )?;
    Ok(())
}

#[cfg(unix)]
/// Durably replaces a file with a synced same-directory staging file.
///
/// # Errors
///
/// Returns an error when the atomic rename or parent-directory sync fails.
pub fn replace_file_atomically(replacement: &Path, destination: &Path) -> Result<()> {
    fs::rename(replacement, destination)?;
    if let Some(parent) = destination.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
/// Durably replaces a file with a synced same-directory staging file.
///
/// # Errors
///
/// Returns an error when both native atomic replacement mechanisms fail.
pub fn replace_file_atomically(replacement: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replacement_wide: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both path buffers are NUL-terminated and remain alive for the
    // call; optional pointer parameters are null as permitted by ReplaceFileW.
    let result = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if result == 0 {
        // Some Windows filesystems and sandboxed ACL configurations reject
        // ReplaceFileW's metadata merge even when atomic replacement itself is
        // permitted. Retain ReplaceFileW as the primary path and use the native
        // write-through replacement rename as the compatibility fallback.
        let fallback = unsafe {
            MoveFileExW(
                replacement_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if fallback == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn now_millis() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CoreError::validation("system clock is before Unix epoch"))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| CoreError::validation("system clock exceeds timestamp range"))
}

fn validate_opaque_component(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            "invalid opaque identifier",
        ));
    }
    Ok(())
}

fn sanitize_copy_name(value: &str) -> String {
    let mut result: String = value
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character)
            } else if character.is_whitespace() || matches!(character, '-' | '_') {
                Some('_')
            } else {
                None
            }
        })
        .take(32)
        .collect();
    while result.contains("__") {
        result = result.replace("__", "_");
    }
    let result = result.trim_matches('_').to_owned();
    if result.is_empty() {
        "copy".to_owned()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DecimalU64, Edit};
    use crate::semantic::{OpenOptions as SaveOpenOptions, OpenedSave};
    use tempfile::tempdir;

    fn valid_campaign() -> Vec<u8> {
        br#"<?xml version="1.0"?><CampaignEngine z="1"></CampaignEngine>"#.to_vec()
    }

    fn valid_descriptor() -> Vec<u8> {
        br#"<?xml version="1.0"?><SaveGameData z="1"><portraitName>graphics/portraits/a.png</portraitName><characterName>Ada Vale</characterName><saveFileVersion>0.6</saveFileVersion><gameVersion>0.98a-RC8</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><difficulty>normal</difficulty><locDesc>Corvus</locDesc><saveDate>date</saveDate><slotCreationTimestamp>1</slotCreationTimestamp><enabledMods z="2"></enabledMods><autosave>false</autosave></SaveGameData>"#.to_vec()
    }

    fn editable_campaign() -> Vec<u8> {
        br#"<?xml version="1.0" ?>
<CampaignEngine z="1">
<playerFleet ref="10"></playerFleet>
<characterData z="70"><name>Ada Vale</name><portraitName>graphics/portraits/portrait_a.png</portraitName><person ref="20"></person><isIronMode>false</isIronMode><skillsEverMadeElite z="71"></skillsEverMadeElite></characterData>
<clock z="75"><timestamp>-1000</timestamp></clock>
<factionManager z="76"><playerFaction ref="80"></playerFaction><relations z="90"><e><st>player_hegemony</st><FMRelation z="91"><factionIdOne>player</factionIdOne><factionIdTwo>hegemony</factionIdTwo><value>0.2</value></FMRelation></e></relations></factionManager>
<saveDirName>save_Ada_1</saveDirName>
<Flt z="10"><fD z="11"><m z="12"><FMmbr z="13"><c z="20" id="player-person" pid="steady" spr="graphics/portraits/portrait_a.png"><n z="21" f="Ada" l="Vale" g="FEMALE"></n><stats z="22" x2="0" xp="0" bx="0" db="0" l="1" pt="0" sp="0"><s>{"alpha":0}</s></stats></c></FMmbr></m><cargo z="30"><c z="31"><value>1000.0</value></c></cargo><c ref="20"></c><o z="40"></o></fD></Flt>
<f z="80"><id>player</id></f>
</CampaignEngine>"#
            .to_vec()
    }

    fn editable_descriptor() -> Vec<u8> {
        br#"<?xml version="1.0" ?><SaveGameData z="1"><portraitName>graphics/portraits/portrait_a.png</portraitName><characterName>Ada Vale</characterName><saveFileVersion>0.6</saveFileVersion><gameVersion>0.98a-RC8</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><difficulty>normal</difficulty><locDesc>Corvus</locDesc><saveDate>date</saveDate><slotCreationTimestamp>1</slotCreationTimestamp><enabledMods z="2"></enabledMods><autosave>false</autosave></SaveGameData>"#.to_vec()
    }

    fn open_editable_fixture(save_dir: &Path) -> OpenedSave {
        fs::create_dir(save_dir).unwrap();
        fs::write(save_dir.join("campaign.xml"), editable_campaign()).unwrap();
        fs::write(save_dir.join("descriptor.xml"), editable_descriptor()).unwrap();
        OpenedSave::open(
            SaveLocation::from_save_dir(save_dir),
            SaveOpenOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn backup_round_trip_is_byte_identical_and_ignores_game_bak_files() {
        let root = tempdir().unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let campaign = b"campaign bytes";
        let descriptor = b"descriptor bytes";
        let backup = store
            .create_backup("save-abc", campaign, descriptor, true, "test")
            .unwrap();
        let listed = store.list("save-abc").unwrap();
        assert_eq!(listed, vec![backup.clone()]);
        let directory = store.root.join("save-abc").join(backup.backup_id);
        assert_eq!(fs::read(directory.join("campaign.xml")).unwrap(), campaign);
        assert_eq!(
            fs::read(directory.join("descriptor.xml")).unwrap(),
            descriptor
        );
        assert!(!directory.join("campaign.xml.bak").exists());
    }

    #[cfg(unix)]
    #[test]
    fn backup_storage_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let backup = store
            .create_backup("save-abc", b"campaign", b"descriptor", true, "test")
            .unwrap();
        let save_root = store.root.join("save-abc");
        let backup_root = save_root.join(backup.backup_id);

        for directory in [&store.root, &save_root, &backup_root] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for file in ["campaign.xml", "descriptor.xml", "manifest.json"] {
            assert_eq!(
                fs::metadata(backup_root.join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_private_storage_is_hardened_without_following_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempdir().unwrap();
        let backups = root.path().join("backups");
        let save_root = backups.join("save");
        let nested = save_root.join("backup");
        fs::create_dir_all(&nested).unwrap();
        let manifest = nested.join("manifest.json");
        fs::write(&manifest, b"{}").unwrap();
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&save_root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();

        harden_private_storage_tree(&backups).unwrap();

        for directory in [&backups, &save_root, &nested] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(&manifest).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let outside = root.path().join("outside");
        fs::write(&outside, b"private").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&outside, nested.join("linked-file")).unwrap();
        let error = harden_private_storage_tree(&backups).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPath);
        assert_eq!(
            fs::metadata(outside).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn save_copy_locks_rechecks_backs_up_journals_and_publishes_atomically() {
        let root = tempdir().unwrap();
        let source_dir = root.path().join("source-save");
        let opened = open_editable_fixture(&source_dir);
        let source_campaign = fs::read(source_dir.join("campaign.xml")).unwrap();
        let source_descriptor = fs::read(source_dir.join("descriptor.xml")).unwrap();
        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 2345.5 }])
            .unwrap();
        let copies = root.path().join("copies");
        fs::create_dir(&copies).unwrap();
        let store = BackupStore::new(root.path().join("backups"));

        let outcome = store.save_copy(review, &copies, "Ada Copy").unwrap();
        let backup = outcome
            .backup
            .clone()
            .expect("save copy must return backup");
        assert_eq!(
            fs::read(source_dir.join("campaign.xml")).unwrap(),
            source_campaign
        );
        assert_eq!(
            fs::read(source_dir.join("descriptor.xml")).unwrap(),
            source_descriptor
        );
        let backup_dir = store.root.join(&backup.save_id).join(&backup.backup_id);
        assert_eq!(
            fs::read(backup_dir.join("campaign.xml")).unwrap(),
            source_campaign
        );
        assert_eq!(
            fs::read(backup_dir.join("descriptor.xml")).unwrap(),
            source_descriptor
        );
        assert!(!backup_dir.join("campaign.xml.bak").exists());
        let started: JournalRecord =
            serde_json::from_slice(&fs::read(backup_dir.join("transaction-started.json")).unwrap())
                .unwrap();
        assert_eq!(started.phase, "copy_published");
        let complete: JournalRecord = serde_json::from_slice(
            &fs::read(backup_dir.join("transaction-complete.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(complete.phase, "copy_complete");
        let members = fs::read_dir(&outcome.location.save_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 2);
        assert!(outcome.location.campaign_path.exists());
        assert!(outcome.location.descriptor_path.exists());
        assert!(fs::read_dir(&copies).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ludds-blessing-copy-")
        }));
        let campaign =
            String::from_utf8(fs::read(&outcome.location.campaign_path).unwrap()).unwrap();
        assert!(campaign.contains("<value>2345.5</value>"));
        assert!(campaign.contains(
            outcome
                .location
                .save_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ));
        assert!(store.pending_recoveries().unwrap().is_empty());
    }

    #[test]
    fn save_copy_rejects_a_locked_source_before_creating_a_backup() {
        let root = tempdir().unwrap();
        let source_dir = root.path().join("source-save");
        let opened = open_editable_fixture(&source_dir);
        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 1200.0 }])
            .unwrap();
        let copies = root.path().join("copies");
        fs::create_dir(&copies).unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let locked = lock_for_write(&source_dir.join("campaign.xml")).unwrap();
        let error = store.save_copy(review, &copies, "locked").unwrap_err();
        let _ = FileExt::unlock(&locked);
        assert_eq!(error.code, ErrorCode::GameRunning);
        assert!(!store.root.exists());
    }

    #[test]
    fn pending_save_copy_recovery_finishes_validated_staging() {
        let root = tempdir().unwrap();
        let source_dir = root.path().join("source-save");
        let opened = open_editable_fixture(&source_dir);
        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 1500.0 }])
            .unwrap();
        let copies = root.path().join("copies");
        fs::create_dir(&copies).unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let applied = store.save_copy(review, &copies, "recoverable").unwrap();
        let backup = applied.backup.unwrap();
        let backup_dir = store.root.join(&backup.save_id).join(&backup.backup_id);
        let started_path = backup_dir.join("transaction-started.json");
        let mut journal: JournalRecord =
            serde_json::from_slice(&fs::read(&started_path).unwrap()).unwrap();
        let staging = journal.copy.as_ref().unwrap().staging.clone();

        fs::remove_file(backup_dir.join("transaction-complete.json")).unwrap();
        fs::rename(&applied.location.save_dir, &staging).unwrap();
        update_journal_phase(&backup_dir, &mut journal, "copy_staged_validated").unwrap();
        assert_eq!(store.pending_recoveries().unwrap().len(), 1);

        let recovered = store
            .recover_pending(&backup.save_id, &backup.backup_id)
            .unwrap();
        assert_eq!(recovered.location, applied.location);
        assert!(recovered.location.campaign_path.exists());
        assert!(!staging.exists());
        assert_eq!(recovered.backup, Some(backup));
        assert!(store.pending_recoveries().unwrap().is_empty());
    }

    #[test]
    fn pending_save_copy_recovery_discards_only_incomplete_staging() {
        let root = tempdir().unwrap();
        let source_dir = root.path().join("source-save");
        let opened = open_editable_fixture(&source_dir);
        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 1600.0 }])
            .unwrap();
        let copies = root.path().join("copies");
        fs::create_dir(&copies).unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let applied = store.save_copy(review, &copies, "partial").unwrap();
        let backup = applied.backup.unwrap();
        let backup_dir = store.root.join(&backup.save_id).join(&backup.backup_id);
        let mut journal: JournalRecord =
            serde_json::from_slice(&fs::read(backup_dir.join("transaction-started.json")).unwrap())
                .unwrap();
        let staging = journal.copy.as_ref().unwrap().staging.clone();

        fs::remove_file(backup_dir.join("transaction-complete.json")).unwrap();
        fs::rename(&applied.location.save_dir, &staging).unwrap();
        fs::remove_file(staging.join("descriptor.xml")).unwrap();
        update_journal_phase(&backup_dir, &mut journal, "copy_staging_created").unwrap();

        let recovered = store
            .recover_pending(&backup.save_id, &backup.backup_id)
            .unwrap();
        assert_eq!(recovered.location.save_dir, source_dir);
        assert!(!applied.location.save_dir.exists());
        assert!(!staging.exists());
        assert!(source_dir.join("campaign.xml").exists());
        assert!(source_dir.join("descriptor.xml").exists());
        assert!(store.pending_recoveries().unwrap().is_empty());
    }

    #[test]
    fn recovery_scans_and_manifest_lengths_are_bounded() {
        let root = tempdir().unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        fs::create_dir_all(store.root.join("save-one").join("backup-one")).unwrap();
        let error = store.pending_recoveries_bounded(1).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceLimit);

        let mut revision = ContentRevision {
            campaign: fingerprint(b"campaign"),
            descriptor: fingerprint(b"descriptor"),
        };
        revision.campaign.byte_len = DecimalU64::new(XmlLimits::default().max_bytes + 1);
        let manifest = BackupManifest {
            schema: 1,
            summary: BackupSummary {
                backup_id: "backup-one".to_owned(),
                save_id: "save-one".to_owned(),
                created_at_millis: DecimalI64::new(1),
                pinned: true,
                reason: "test".to_owned(),
                revision,
            },
        };
        let error = validate_backup_manifest_lengths(&manifest).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceLimit);

        let path = root.path().join("bounded-read.xml");
        fs::write(&path, b"four").unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let error = read_locked(&mut file, 3).unwrap_err();
        assert!(matches!(
            error.code,
            ErrorCode::StaleSave | ErrorCode::ResourceLimit
        ));
    }

    #[test]
    fn copy_names_are_bounded_and_safe() {
        assert_eq!(sanitize_copy_name("../../My Captain"), "My_Captain");
        assert_eq!(sanitize_copy_name("***"), "copy");
    }

    #[test]
    fn native_replace_overwrites_an_existing_regular_file() {
        let root = tempdir().unwrap();
        let destination = root.path().join("destination.xml");
        let replacement = root.path().join("replacement.tmp");
        fs::write(&destination, b"old").unwrap();
        fs::write(&replacement, b"new").unwrap();
        replace_file_atomically(&replacement, &destination).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"new");
    }

    #[test]
    fn starsector_process_names_are_recognized_without_blocking_unrelated_java() {
        assert!(process_looks_like_starsector("starsector.exe", ""));
        assert!(process_looks_like_starsector(
            "java.exe",
            r"C:\Games\Starsector\jre\bin\java.exe"
        ));
        assert!(process_looks_like_starsector(
            "java",
            "java -jar starsector-core.jar"
        ));
        assert!(!process_looks_like_starsector(
            "java.exe",
            r"C:\Tools\Java\bin\java.exe"
        ));
    }

    #[test]
    fn starsector_inprogress_files_block_before_a_transaction_backup() {
        let root = tempdir().unwrap();
        let source_dir = root.path().join("source-save");
        let opened = open_editable_fixture(&source_dir);
        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 1200.0 }])
            .unwrap();
        fs::write(source_dir.join("campaign.xml.inprogress"), b"partial").unwrap();
        let store = BackupStore::new(root.path().join("backups"));

        let error = store.apply_replace(review, false).unwrap_err();

        assert_eq!(error.code, ErrorCode::GameRunning);
        assert!(!store.root.exists());
    }

    #[cfg(windows)]
    fn create_windows_test_install(root: &Path, save_names: &[&str]) {
        fs::create_dir_all(root.join("starsector-core")).unwrap();
        fs::create_dir_all(root.join("saves")).unwrap();
        fs::create_dir_all(root.join("jre/bin")).unwrap();
        fs::write(root.join("starsector.exe"), b"test wrapper").unwrap();
        fs::write(root.join("jre/bin/java.exe"), b"test jvm").unwrap();
        fs::write(
            root.join("vmparams"),
            b"java -Dcom.fs.starfarer.settings.paths.saves=..\\saves Game",
        )
        .unwrap();
        for name in save_names {
            fs::create_dir(root.join("saves").join(name)).unwrap();
        }
    }

    #[cfg(any(windows, unix))]
    fn launcher_log_line() -> String {
        "0    [main] INFO  com.fs.starfarer.StarfarerLauncher  - Starting Starsector 0.98a-RC8 launcher".to_owned()
    }

    #[cfg(any(windows, unix))]
    fn campaign_log_line(message: &str) -> String {
        format!(
            "100 [Thread-2] INFO  com.fs.starfarer.campaign.save.CampaignGameManager  - {message}"
        )
    }

    #[cfg(windows)]
    fn parse_windows_test_lines(root: &Path, lines: &[String]) -> StarsectorLogActivity {
        let mut bytes = lines.join("\n").into_bytes();
        bytes.push(b'\n');
        let mut activity = StarsectorLogActivity::default();
        parse_starsector_log_snapshot(&bytes, root, &mut activity).unwrap();
        activity
    }

    #[cfg(unix)]
    fn create_unix_test_install(root: &Path, save_names: &[&str]) {
        fs::create_dir_all(root.join("data/config")).unwrap();
        fs::create_dir_all(root.join("saves")).unwrap();
        fs::write(root.join("data/config/settings.json"), b"{}").unwrap();
        fs::write(root.join("starfarer.api.jar"), b"api").unwrap();
        fs::write(root.join("starfarer_obf.jar"), b"game").unwrap();
        for name in save_names {
            fs::create_dir(root.join("saves").join(name)).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_evidence_and_topology_fail_closed() {
        assert_eq!(
            classify_unix_process("java", &["java", "com.fs.starfarer.StarfarerLauncher"]),
            Some(UnixProcessEvidence::Launcher)
        );
        assert_eq!(
            classify_unix_process("java", &["java", "example.UnrelatedLauncher"]),
            None
        );
        assert_eq!(
            classify_unix_process("starsector.sh", &["./starsector.sh"]),
            Some(UnixProcessEvidence::NamedWrapper)
        );

        let root = PathBuf::from("/verified/starsector");
        let valid = assemble_unix_processes(vec![
            ObservedUnixProcess {
                evidence: UnixProcessEvidence::Launcher,
                install_root: Some(root.clone()),
            },
            ObservedUnixProcess {
                evidence: UnixProcessEvidence::NamedWrapper,
                install_root: Some(root.clone()),
            },
        ]);
        assert!(valid.found);
        assert!(!valid.unresolved);
        assert_eq!(valid.session_counts, vec![(root.clone(), 1)]);

        let duplicate = assemble_unix_processes(vec![
            ObservedUnixProcess {
                evidence: UnixProcessEvidence::Launcher,
                install_root: Some(root.clone()),
            },
            ObservedUnixProcess {
                evidence: UnixProcessEvidence::Launcher,
                install_root: Some(root),
            },
        ]);
        assert!(duplicate.unresolved);

        let unresolved = assemble_unix_processes(vec![ObservedUnixProcess {
            evidence: UnixProcessEvidence::Launcher,
            install_root: None,
        }]);
        assert!(unresolved.unresolved);
        assert!(unresolved.install_roots.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_log_activity_permits_only_a_different_verified_save() {
        let root = tempdir().unwrap();
        create_unix_test_install(root.path(), &["save_active", "save_inactive"]);
        assert!(unix_launcher_save_root_matches(
            &[
                "java",
                "-Dcom.fs.starfarer.settings.paths.saves=./saves",
                "com.fs.starfarer.StarfarerLauncher",
            ],
            root.path()
        ));
        assert!(!unix_launcher_save_root_matches(
            &[
                "java",
                "-Dcom.fs.starfarer.settings.paths.saves=/tmp/other-saves",
                "com.fs.starfarer.StarfarerLauncher",
            ],
            root.path()
        ));
        assert!(!unix_launcher_save_root_matches(
            &["java", "com.fs.starfarer.StarfarerLauncher"],
            root.path()
        ));
        let active = fs::canonicalize(root.path().join("saves/save_active")).unwrap();
        let inactive = fs::canonicalize(root.path().join("saves/save_inactive")).unwrap();
        let mut bytes = [
            launcher_log_line(),
            campaign_log_line("Loading ./saves/save_active..."),
            campaign_log_line("Loading stage 39 - last"),
        ]
        .join("\n")
        .into_bytes();
        bytes.push(b'\n');
        let mut activity = StarsectorLogActivity::default();
        parse_starsector_log_snapshot(&bytes, root.path(), &mut activity).unwrap();

        assert_eq!(activity.possibly_active, vec![active.clone()]);
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &active)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );
        ensure_log_activity_allows_target(&activity, &inactive).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_command_resolves_an_app_path_with_spaces() {
        let root = tempdir().unwrap();
        let java = root
            .path()
            .join("My Games/Starsector.app/Contents/Resources/Java");
        create_unix_test_install(&java, &[]);
        let executable = root
            .path()
            .join("My Games/Starsector.app/Contents/Home/bin/java");
        let command = format!(
            "{} -cp game.jar com.fs.starfarer.StarfarerLauncher",
            executable.display()
        );
        assert_eq!(
            macos_installation_root_from_command(&command),
            Some(java.canonicalize().unwrap())
        );
    }

    #[cfg(windows)]
    #[test]
    fn latest_launch_marker_resets_historical_save_activity() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_old"]);
        let lines = vec![
            launcher_log_line(),
            campaign_log_line(r"Loading ..\saves/save_old..."),
            campaign_log_line("Loading stage 39 - last"),
            launcher_log_line(),
            campaign_log_line(r"Reading save data from [..\saves\save_old\descriptor.xml]"),
        ];

        let activity = parse_windows_test_lines(root.path(), &lines);

        assert!(activity.saw_start);
        assert!(!activity.ambiguous);
        assert!(activity.possibly_active.is_empty());
        assert!(activity.loading.is_none());
        assert!(activity.saving.is_none());
        let target = fs::canonicalize(root.path().join("saves/save_old")).unwrap();
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &target)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );
    }

    #[cfg(windows)]
    #[test]
    fn embedded_logger_markers_cannot_spoof_a_launch_or_save_event() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_active", "save_spoofed"]);
        let active = fs::canonicalize(root.path().join("saves/save_active")).unwrap();
        let mut activity = parse_windows_test_lines(
            root.path(),
            &[
                launcher_log_line(),
                campaign_log_line(r"Loading ..\saves/save_active..."),
                campaign_log_line("Loading stage 39 - last"),
            ],
        );
        let spoofed_launch = "200 [worker] INFO  example.Mod  - embedded com.fs.starfarer.StarfarerLauncher  - Starting Starsector 0.98a-RC8 launcher";
        let spoofed_load = "201 [worker] INFO  example.Mod  - embedded com.fs.starfarer.campaign.save.CampaignGameManager  - Loading ..\\saves/save_spoofed...";

        parse_starsector_log_line(spoofed_launch, root.path(), &mut activity).unwrap();
        parse_starsector_log_line(spoofed_load, root.path(), &mut activity).unwrap();

        assert!(!activity.ambiguous);
        assert_eq!(activity.possibly_active, vec![active]);
        assert!(activity.loading.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn load_and_save_transitions_block_globally_then_only_the_active_save() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_active", "save_inactive", "save_copy"]);
        let active = fs::canonicalize(root.path().join("saves/save_active")).unwrap();
        let inactive = fs::canonicalize(root.path().join("saves/save_inactive")).unwrap();
        let mut activity = StarsectorLogActivity::default();

        for line in [
            launcher_log_line(),
            campaign_log_line(r"Loading ..\\saves/save_active..."),
        ] {
            parse_starsector_log_line(&line, root.path(), &mut activity).unwrap();
        }
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &inactive)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );

        parse_starsector_log_line(
            &campaign_log_line("Loading stage 39 - last"),
            root.path(),
            &mut activity,
        )
        .unwrap();
        assert_eq!(activity.possibly_active, vec![active.clone()]);
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &active)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );
        ensure_log_activity_allows_target(&activity, &inactive).unwrap();

        parse_starsector_log_line(
            &campaign_log_line(r"Saving to ..\saves/save_copy..."),
            root.path(),
            &mut activity,
        )
        .unwrap();
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &inactive)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );
        parse_starsector_log_line(
            &campaign_log_line("Finished saving"),
            root.path(),
            &mut activity,
        )
        .unwrap();

        assert!(activity.references(&active));
        assert!(
            activity.references(&fs::canonicalize(root.path().join("saves/save_copy")).unwrap())
        );
        ensure_log_activity_allows_target(&activity, &inactive).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn every_distinct_completed_save_remains_possibly_active() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_first", "save_copy"]);
        let first = fs::canonicalize(root.path().join("saves/save_first")).unwrap();
        let activity = parse_windows_test_lines(
            root.path(),
            &[
                launcher_log_line(),
                campaign_log_line(r"Saving to ..\saves/save_first..."),
                campaign_log_line("Finished saving"),
                campaign_log_line(r"Saving to ..\saves/save_copy..."),
                campaign_log_line("Finished saving"),
            ],
        );

        assert!(!activity.ambiguous);
        let copy = fs::canonicalize(root.path().join("saves/save_copy")).unwrap();
        assert_eq!(activity.possibly_active, vec![first, copy]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_containment_is_component_bounded_and_case_insensitive() {
        let root = Path::new(r"C:\Users\Pilot\AppData\Local\VirtualStore");
        assert!(windows_path_is_within(
            Path::new(r"c:\users\pilot\appdata\local\virtualstore\Program Files (x86)\Starsector"),
            root
        ));
        assert!(!windows_path_is_within(
            Path::new(r"C:\Users\Pilot\AppData\Local\VirtualStoreOther\Starsector"),
            root
        ));
        assert!(!windows_path_is_within(
            Path::new(r"C:\Users\Pilot\AppData\Local"),
            root
        ));
    }

    #[cfg(windows)]
    #[test]
    fn logged_save_paths_are_normalized_and_confined_to_direct_save_children() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_one"]);
        fs::create_dir(root.path().join("outside")).unwrap();
        let expected = fs::canonicalize(root.path().join("saves/save_one")).unwrap();

        assert_eq!(
            resolve_logged_save_path(root.path(), r"..\\saves/save_one").as_deref(),
            Some(expected.as_path())
        );
        assert_eq!(
            resolve_logged_save_path(root.path(), expected.to_string_lossy().as_ref()).as_deref(),
            Some(expected.as_path())
        );
        assert!(resolve_logged_save_path(root.path(), r"..\outside").is_none());
        assert!(resolve_logged_save_path(root.path(), r"..\saves\save_one\nested").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn configured_external_save_root_controls_log_path_authorization() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_wrong_root"]);
        let external = root.path().join("External Campaigns");
        let active = external.join("save_active");
        let inactive = external.join("save_inactive");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir(&inactive).unwrap();
        fs::write(
            root.path().join("vmparams"),
            format!(
                "java \"-Dcom.fs.starfarer.settings.paths.saves={}\" Game",
                external.display()
            ),
        )
        .unwrap();
        let active = fs::canonicalize(active).unwrap();
        let inactive = fs::canonicalize(inactive).unwrap();

        assert_eq!(
            resolve_logged_save_path(root.path(), active.to_string_lossy().as_ref()).as_deref(),
            Some(active.as_path())
        );
        assert!(resolve_logged_save_path(
            root.path(),
            root.path()
                .join("saves/save_wrong_root")
                .to_string_lossy()
                .as_ref()
        )
        .is_none());

        let activity = parse_windows_test_lines(
            root.path(),
            &[
                launcher_log_line(),
                campaign_log_line(&format!("Loading {}...", active.display())),
                campaign_log_line("Loading stage 39 - last"),
            ],
        );
        assert_eq!(activity.possibly_active, vec![active.clone()]);
        assert_eq!(
            ensure_log_activity_allows_target(&activity, &active)
                .unwrap_err()
                .code,
            ErrorCode::GameRunning
        );
        ensure_log_activity_allows_target(&activity, &inactive).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn malformed_or_incomplete_relevant_log_activity_fails_closed() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_one"]);
        let activity = parse_windows_test_lines(
            root.path(),
            &[
                launcher_log_line(),
                campaign_log_line(r"Loading ..\saves/save_one"),
            ],
        );
        assert!(activity.ambiguous);

        let mut activity = StarsectorLogActivity::default();
        let relevant_fragment = campaign_log_line(r"Saving to ..\saves/save_one...");
        let error =
            parse_starsector_log_snapshot(relevant_fragment.as_bytes(), root.path(), &mut activity)
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::GameRunning);

        let error = parse_starsector_log_snapshot(
            b"100 [worker] INFO unrelated fragment",
            root.path(),
            &mut activity,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::GameRunning);
    }

    #[cfg(windows)]
    #[test]
    fn rotated_log_chain_carries_the_latest_launch_into_the_current_log() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &["save_one"]);
        let core = root.path().join("starsector-core");
        fs::write(
            core.join("starsector.log.1"),
            format!("{}\n", launcher_log_line()),
        )
        .unwrap();
        fs::write(
            core.join("starsector.log"),
            format!(
                "{}\n{}\n",
                campaign_log_line(r"Loading ..\saves/save_one..."),
                campaign_log_line("Loading stage 39 - last")
            ),
        )
        .unwrap();

        let activity = read_starsector_log_activity(root.path()).unwrap();
        let expected = fs::canonicalize(root.path().join("saves/save_one")).unwrap();
        assert_eq!(activity.possibly_active, vec![expected]);
        assert!(!activity.ambiguous);
    }

    #[cfg(windows)]
    #[test]
    fn missing_or_oversized_current_log_fails_closed() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &[]);
        assert_eq!(
            read_starsector_log_activity(root.path()).unwrap_err().code,
            ErrorCode::GameRunning
        );

        let log = File::create(root.path().join("starsector-core/starsector.log")).unwrap();
        log.set_len(MAX_STARSECTOR_LOG_FILE_BYTES + 1).unwrap();
        assert_eq!(
            read_starsector_log_activity(root.path()).unwrap_err().code,
            ErrorCode::GameRunning
        );
    }

    #[cfg(windows)]
    #[test]
    fn process_image_resolves_only_a_valid_starsector_install_root() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &[]);
        let image = root.path().join("jre/bin/java.exe");

        assert_eq!(
            starsector_install_root_from_image(&image).as_deref(),
            Some(fs::canonicalize(root.path()).unwrap().as_path())
        );
        assert!(
            starsector_install_root_from_image(Path::new(r"C:\Tools\Java\bin\java.exe")).is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    fn exact_native_topology_requires_one_wrapper_and_its_child_jvm() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &[]);
        let install_root = fs::canonicalize(root.path()).unwrap();
        let process =
            |process_id, parent_process_id, created_at_ticks, kind| ResolvedStarsectorProcess {
                process_id,
                parent_process_id,
                created_at_ticks,
                install_root: install_root.clone(),
                kind,
            };
        let one_session = vec![
            process(10, 1, 100, NativeStarsectorProcessKind::Wrapper),
            process(11, 10, 200, NativeStarsectorProcessKind::Jvm),
        ];
        assert!(has_exact_native_starsector_topology(
            &one_session,
            &install_root
        ));

        let two_sessions = vec![
            process(10, 1, 100, NativeStarsectorProcessKind::Wrapper),
            process(11, 10, 200, NativeStarsectorProcessKind::Jvm),
            process(20, 1, 300, NativeStarsectorProcessKind::Wrapper),
            process(21, 20, 400, NativeStarsectorProcessKind::Jvm),
        ];
        assert!(!has_exact_native_starsector_topology(
            &two_sessions,
            &install_root
        ));
        let unpaired = vec![
            process(10, 1, 100, NativeStarsectorProcessKind::Wrapper),
            process(11, 99, 200, NativeStarsectorProcessKind::Jvm),
        ];
        assert!(!has_exact_native_starsector_topology(
            &unpaired,
            &install_root
        ));
        let java_only = vec![process(11, 10, 200, NativeStarsectorProcessKind::Jvm)];
        assert!(!has_exact_native_starsector_topology(
            &java_only,
            &install_root
        ));
        let impossible_creation_order = vec![
            process(10, 1, 200, NativeStarsectorProcessKind::Wrapper),
            process(11, 10, 100, NativeStarsectorProcessKind::Jvm),
        ];
        assert!(!has_exact_native_starsector_topology(
            &impossible_creation_order,
            &install_root
        ));
    }

    #[cfg(windows)]
    #[test]
    fn only_exact_native_executable_paths_are_classified() {
        let root = tempdir().unwrap();
        create_windows_test_install(root.path(), &[]);
        let install_root = fs::canonicalize(root.path()).unwrap();
        let helper = root.path().join("UninstallStarsector.exe");
        fs::write(&helper, b"helper").unwrap();

        assert_eq!(
            classify_native_starsector_process(&install_root, &root.path().join("starsector.exe")),
            Some(NativeStarsectorProcessKind::Wrapper)
        );
        assert_eq!(
            classify_native_starsector_process(
                &install_root,
                &root.path().join("jre/bin/java.exe")
            ),
            Some(NativeStarsectorProcessKind::Jvm)
        );
        assert_eq!(
            classify_native_starsector_process(&install_root, &helper),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "developer-only live Starsector activity probe"]
    fn developer_live_guard_distinguishes_active_and_inactive_saves() {
        let active = PathBuf::from(
            std::env::var_os("LUDDS_BLESSING_LIVE_ACTIVE_SAVE")
                .expect("set LUDDS_BLESSING_LIVE_ACTIVE_SAVE"),
        );
        let inactive = PathBuf::from(
            std::env::var_os("LUDDS_BLESSING_LIVE_INACTIVE_SAVE")
                .expect("set LUDDS_BLESSING_LIVE_INACTIVE_SAVE"),
        );
        let active_parent = fs::canonicalize(active.parent().unwrap()).unwrap();
        let inactive_parent = fs::canonicalize(inactive.parent().unwrap()).unwrap();
        assert!(windows_path_eq(&active_parent, &inactive_parent));

        let error = ensure_save_inactive(&SaveLocation::from_save_dir(active)).unwrap_err();
        assert_eq!(error.code, ErrorCode::GameRunning);
        ensure_save_inactive(&SaveLocation::from_save_dir(inactive)).unwrap();
    }

    #[test]
    fn opaque_ids_reject_traversal() {
        assert!(validate_opaque_component("../bad").is_err());
        assert!(validate_opaque_component(&crate::file_util::opaque_id("save", b"ok")).is_ok());
    }

    #[test]
    fn completed_rollback_marker_clears_recovery_state() {
        let root = tempdir().unwrap();
        let store = BackupStore::new(root.path().join("backups"));
        let backup = store
            .create_backup("save-abc", b"campaign", b"descriptor", true, "test")
            .unwrap();
        let backup_dir = store.root.join(&backup.save_id).join(&backup.backup_id);
        let mut journal = JournalRecord {
            schema: 1,
            save_id: backup.save_id,
            backup_id: backup.backup_id,
            phase: "prepared".to_owned(),
            destination: root.path().join("save"),
            copy: None,
        };
        write_new_synced(
            &backup_dir.join("transaction-started.json"),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let pending = store.pending_recoveries().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].phase, "prepared");
        update_journal_phase(&backup_dir, &mut journal, "apply_campaign_replaced").unwrap();
        let pending = store.pending_recoveries().unwrap();
        assert_eq!(pending[0].phase, "apply_campaign_replaced");
        mark_transaction_complete(&backup_dir, &journal, "rolled_back").unwrap();
        assert!(store.pending_recoveries().unwrap().is_empty());
    }

    #[test]
    fn pending_recovery_restores_validated_pair_and_preserves_raw_current_bytes() {
        let root = tempdir().unwrap();
        let save_dir = root.path().join("save");
        fs::create_dir(&save_dir).unwrap();
        let location = SaveLocation::from_save_dir(&save_dir);
        fs::write(&location.campaign_path, b"broken campaign").unwrap();
        fs::write(&location.descriptor_path, b"broken descriptor").unwrap();
        let save_id = opaque_id("save", save_dir.to_string_lossy().as_bytes());
        let store = BackupStore::new(root.path().join("backups"));
        let recovery = store
            .create_backup(
                &save_id,
                &valid_campaign(),
                &valid_descriptor(),
                true,
                "transaction source",
            )
            .unwrap();
        let recovery_dir = store.root.join(&save_id).join(&recovery.backup_id);
        let journal = JournalRecord {
            schema: 1,
            save_id: save_id.clone(),
            backup_id: recovery.backup_id.clone(),
            phase: "prepared".to_owned(),
            destination: save_dir,
            copy: None,
        };
        write_new_synced(
            &recovery_dir.join("transaction-started.json"),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let outcome = store
            .recover_pending(&save_id, &recovery.backup_id)
            .unwrap();
        assert_eq!(fs::read(location.campaign_path).unwrap(), valid_campaign());
        assert_eq!(
            fs::read(location.descriptor_path).unwrap(),
            valid_descriptor()
        );
        let emergency = outcome.backup.unwrap();
        let emergency_dir = store.root.join(&save_id).join(emergency.backup_id);
        assert_eq!(
            fs::read(emergency_dir.join("campaign.xml")).unwrap(),
            b"broken campaign"
        );
        let durable_journal: JournalRecord = serde_json::from_slice(
            &fs::read(recovery_dir.join("transaction-started.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(durable_journal.phase, "recovery_descriptor_replaced");
        assert!(store.pending_recoveries().unwrap().is_empty());
    }
}
