use crate::descriptor::parse_descriptor;
use crate::error::{CoreError, ErrorCode, Result};
use crate::file_util::{ensure_regular_directory, opaque_id, read_regular_file};
use crate::model::{Compatibility, SaveLocation, SaveSummary, Warning};
use crate::xml::XmlLimits;
use crate::{SUPPORTED_GAME_VERSION, SUPPORTED_SAVE_FORMAT};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub max_entries: usize,
    pub max_descriptor_bytes: u64,
    pub max_campaign_bytes: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_entries: 4096,
            max_descriptor_bytes: 4 * 1024 * 1024,
            max_campaign_bytes: XmlLimits::default().max_bytes,
        }
    }
}

pub fn scan_save_root(root: &Path, options: ScanOptions) -> Result<Vec<SaveSummary>> {
    ensure_regular_directory(root)?;
    let mut summaries = Vec::new();
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        if index >= options.max_entries {
            return Err(CoreError::new(
                ErrorCode::ResourceLimit,
                "save root entry limit exceeded",
            ));
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".ludds-blessing-")
        {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            continue;
        }
        if !entry.path().join("descriptor.xml").exists() {
            continue;
        }
        if let Ok(summary) = inspect_save_dir(&entry.path(), options) {
            summaries.push(summary);
        }
    }
    summaries.sort_by(|left, right| {
        right
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.slot_creation_timestamp)
            .cmp(
                &left
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.slot_creation_timestamp),
            )
    });
    Ok(summaries)
}

pub fn inspect_save_dir(save_dir: &Path, options: ScanOptions) -> Result<SaveSummary> {
    ensure_regular_directory(save_dir)?;
    let location = SaveLocation::from_save_dir(save_dir);
    let save_id = opaque_id("save", save_dir.to_string_lossy().as_bytes());
    let descriptor_bytes =
        match read_regular_file(&location.descriptor_path, options.max_descriptor_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(invalid_summary(save_id, location, error));
            }
        };
    let descriptor = match parse_descriptor(
        descriptor_bytes,
        XmlLimits {
            max_bytes: options.max_descriptor_bytes,
            max_elements: 100_000,
            ..XmlLimits::default()
        },
    ) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            return Ok(invalid_summary(save_id, location, error));
        }
    };

    let campaign_ok = fs::symlink_metadata(&location.campaign_path)
        .map(|metadata| {
            metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= options.max_campaign_bytes
        })
        .unwrap_or(false);
    let compatibility = if !campaign_ok {
        Compatibility::Invalid {
            code: ErrorCode::InvalidPath,
            reason: "campaign.xml is missing, symlinked, non-regular, or too large".to_owned(),
        }
    } else if descriptor.metadata.compressed {
        Compatibility::ReadOnly {
            code: ErrorCode::UnsupportedCompression,
            reason: "compressed saves are read-only".to_owned(),
        }
    } else if descriptor.metadata.game_version != SUPPORTED_GAME_VERSION
        || descriptor.metadata.save_format != SUPPORTED_SAVE_FORMAT
    {
        Compatibility::ReadOnly {
            code: ErrorCode::UnsupportedVersion,
            reason: format!(
                "editing requires {SUPPORTED_GAME_VERSION} / format {SUPPORTED_SAVE_FORMAT}"
            ),
        }
    } else if !descriptor.has_complete_write_shape() {
        Compatibility::ReadOnly {
            code: ErrorCode::AmbiguousStructure,
            reason: "the descriptor is missing required RC8 write metadata".to_owned(),
        }
    } else {
        Compatibility::Editable
    };
    let mut warnings = Vec::new();
    if descriptor.metadata.iron_mode || descriptor.metadata.autosave {
        warnings.push(Warning {
            code: "PROTECTED_SAVE".to_owned(),
            message: "Iron Mode and autosave slots require an explicit per-session unlock"
                .to_owned(),
            acknowledgement_required: true,
        });
    }
    if !descriptor.metadata.enabled_mods.is_empty() {
        warnings.push(Warning {
            code: "MODDED_SAVE".to_owned(),
            message: "Unknown mod data will be preserved; progression simulation is disabled"
                .to_owned(),
            acknowledgement_required: false,
        });
    }

    Ok(SaveSummary {
        save_id,
        location,
        metadata: Some(descriptor.metadata),
        compatibility,
        warnings,
    })
}

fn invalid_summary(save_id: String, location: SaveLocation, error: CoreError) -> SaveSummary {
    SaveSummary {
        save_id,
        location,
        metadata: None,
        compatibility: Compatibility::Invalid {
            code: error.code,
            reason: error.message,
        },
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn descriptor_only_scan_classifies_versions() {
        let root = tempdir().unwrap();
        let save = root.path().join("save_one");
        fs::create_dir(&save).unwrap();
        fs::write(
            save.join("campaign.xml"),
            "<CampaignEngine z=\"1\"></CampaignEngine>",
        )
        .unwrap();
        fs::write(
            save.join("descriptor.xml"),
            "<SaveGameData z=\"1\"><portraitName>p.png</portraitName><characterName>A</characterName><saveFileVersion>0.5</saveFileVersion><gameVersion>old</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><slotCreationTimestamp>1</slotCreationTimestamp></SaveGameData>",
        )
        .unwrap();
        let saves = scan_save_root(root.path(), ScanOptions::default()).unwrap();
        assert_eq!(saves.len(), 1);
        assert!(matches!(
            saves[0].compatibility,
            Compatibility::ReadOnly { .. }
        ));
    }

    #[test]
    fn legacy_missing_descriptor_fields_are_metadata_not_parse_failures() {
        let root = tempdir().unwrap();
        let save = root.path().join("save_legacy");
        fs::create_dir(&save).unwrap();
        fs::write(save.join("campaign.xml"), "<CampaignEngine z=\"1\" />").unwrap();
        fs::write(
            save.join("descriptor.xml"),
            "<SaveGameData z=\"1\"><portraitName>p.png</portraitName><characterName>Legacy Pilot</characterName><saveFileVersion>0.5</saveFileVersion><characterLevel>7</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><saveDate>2022-07-30</saveDate></SaveGameData>",
        )
        .unwrap();

        let summary = inspect_save_dir(&save, ScanOptions::default()).unwrap();
        let metadata = summary.metadata.unwrap();
        assert_eq!(metadata.character_name, "Legacy Pilot");
        assert_eq!(metadata.game_version, "Unknown");
        assert_eq!(metadata.slot_creation_timestamp, None);
        assert!(matches!(
            summary.compatibility,
            Compatibility::ReadOnly {
                code: ErrorCode::UnsupportedVersion,
                ..
            }
        ));
    }

    #[test]
    fn supported_version_missing_write_metadata_stays_read_only() {
        let root = tempdir().unwrap();
        let save = root.path().join("save_incomplete");
        fs::create_dir(&save).unwrap();
        fs::write(save.join("campaign.xml"), "<CampaignEngine z=\"1\" />").unwrap();
        fs::write(
            save.join("descriptor.xml"),
            "<SaveGameData z=\"1\"><portraitName>p.png</portraitName><characterName>Incomplete Pilot</characterName><saveFileVersion>0.6</saveFileVersion><gameVersion>0.98a-RC8</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode></SaveGameData>",
        )
        .unwrap();

        let summary = inspect_save_dir(&save, ScanOptions::default()).unwrap();
        assert!(matches!(
            summary.compatibility,
            Compatibility::ReadOnly {
                code: ErrorCode::AmbiguousStructure,
                ..
            }
        ));
    }

    #[test]
    #[ignore = "requires SAVE_CORE_DISCOVERY_FIXTURE_DIR and reads a private local save without writing it"]
    fn inspects_real_descriptor_fixture_from_environment_read_only() {
        let directory = std::env::var_os("SAVE_CORE_DISCOVERY_FIXTURE_DIR")
            .expect("set SAVE_CORE_DISCOVERY_FIXTURE_DIR to a local save directory");
        let summary = inspect_save_dir(Path::new(&directory), ScanOptions::default()).unwrap();
        let metadata = summary.metadata.expect("descriptor metadata should parse");
        assert!(!metadata.character_name.is_empty());
        assert!(!metadata.save_format.is_empty());
        assert!(!matches!(
            summary.compatibility,
            Compatibility::Invalid { .. }
        ));
    }
}
