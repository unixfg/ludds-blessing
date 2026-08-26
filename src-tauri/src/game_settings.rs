use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use crate::error::{CommandError, ErrorCode};
use crate::models::InstallationId;

const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROFILE_BYTES: u64 = 512 * 1024;
const MAX_CUSTOM_PROFILES: usize = 64;

const PLAYER_MAX_LEVEL: &str = "playerMaxLevel";
const SKILL_POINTS_PER_LEVEL: &str = "skillPointsPerLevel";
const STORY_POINTS_PER_LEVEL: &str = "storyPointsPerLevel";
const BONUS_XP_USE_MULT_AT_MAX_LEVEL: &str = "bonusXPUseMultAtMaxLevel";
const OFFICER_XP_REQUIRED_MULT: &str = "officerXPRequiredMult";
const OFFICER_MAX_LEVEL: &str = "officerMaxLevel";
const OFFICER_MAX_ELITE_SKILLS: &str = "officerMaxEliteSkills";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressionSettingsCompatibility {
    pub player: bool,
    pub officer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct GameSettingsProfileId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct GameSettingsValues {
    pub player_max_level: u32,
    pub skill_points_per_level: u32,
    pub story_points_per_level: u32,
    pub officer_max_level: u32,
    pub officer_max_elite_skills: u32,
}

impl GameSettingsValues {
    pub const VANILLA_RC8: Self = Self {
        player_max_level: 15,
        skill_points_per_level: 1,
        story_points_per_level: 4,
        officer_max_level: 5,
        officer_max_elite_skills: 1,
    };

    fn validate(self) -> Result<Self, CommandError> {
        if !(1..=100).contains(&self.player_max_level) {
            return Err(CommandError::invalid_argument(
                "Player maximum level must be between 1 and 100",
            ));
        }
        if self.skill_points_per_level > 10 {
            return Err(CommandError::invalid_argument(
                "Skill points per level must be between 0 and 10",
            ));
        }
        if self.story_points_per_level > 100 {
            return Err(CommandError::invalid_argument(
                "Story points per level must be between 0 and 100",
            ));
        }
        if !(1..=100).contains(&self.officer_max_level) {
            return Err(CommandError::invalid_argument(
                "Officer maximum level must be between 1 and 100",
            ));
        }
        if self.officer_max_elite_skills > self.officer_max_level {
            return Err(CommandError::invalid_argument(
                "Officer elite skills cannot exceed the officer maximum level",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct GameSettingsSnapshot {
    pub installation_id: InstallationId,
    pub display_name: String,
    pub display_path: String,
    pub values: GameSettingsValues,
    pub revision: String,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct GameSettingsProfile {
    pub profile_id: GameSettingsProfileId,
    pub name: String,
    pub values: GameSettingsValues,
    pub built_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct GameSettingsApplyResult {
    pub snapshot: GameSettingsSnapshot,
    pub backup_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedProfiles {
    schema: u32,
    profiles: Vec<GameSettingsProfile>,
}

impl Default for PersistedProfiles {
    fn default() -> Self {
        Self {
            schema: 1,
            profiles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsBackupManifest {
    schema: u32,
    backup_id: String,
    installation_fingerprint: String,
    source_revision: String,
}

#[derive(Debug, Clone)]
struct IntegerSpan {
    range: std::ops::Range<usize>,
    value: u32,
}

#[derive(Debug, Clone)]
struct SettingsDocument {
    bytes: Vec<u8>,
    spans: HashMap<&'static str, IntegerSpan>,
    values: GameSettingsValues,
    bonus_xp_use_mult_at_max_level: u32,
    officer_xp_required_mult: u32,
    revision: String,
}

impl SettingsDocument {
    fn parse(bytes: Vec<u8>) -> Result<Self, CommandError> {
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_SETTINGS_BYTES) {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Starsector settings.json exceeds the supported size limit",
            ));
        }
        std::str::from_utf8(&bytes).map_err(|_| {
            CommandError::new(
                ErrorCode::ValidationFailed,
                "Starsector settings.json is not valid UTF-8",
            )
        })?;

        let mut cursor = Cursor::new(&bytes);
        cursor.skip_trivia()?;
        cursor.expect_byte(b'{', "settings.json must contain one root object")?;
        let mut spans = HashMap::new();

        loop {
            cursor.skip_trivia()?;
            if cursor.consume_byte(b'}') {
                break;
            }
            let key = cursor.parse_string()?;
            cursor.skip_trivia()?;
            cursor.expect_byte(b':', "settings.json property is missing ':'")?;
            cursor.skip_trivia()?;

            if let Some(canonical) = recognized_key(&key) {
                let span = cursor.parse_u32()?;
                if spans.insert(canonical, span).is_some() {
                    return Err(CommandError::new(
                        ErrorCode::ValidationFailed,
                        format!("settings.json contains duplicate {canonical} entries"),
                    ));
                }
            } else {
                cursor.skip_value()?;
            }

            cursor.skip_trivia()?;
            if cursor.consume_byte(b',') {
                continue;
            }
            if cursor.consume_byte(b'}') {
                break;
            }
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "settings.json root object is malformed",
            ));
        }

        cursor.skip_trivia()?;
        if !cursor.is_eof() {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "settings.json contains data after the root object",
            ));
        }

        let values = GameSettingsValues {
            player_max_level: require_value(&spans, PLAYER_MAX_LEVEL)?,
            skill_points_per_level: require_value(&spans, SKILL_POINTS_PER_LEVEL)?,
            story_points_per_level: require_value(&spans, STORY_POINTS_PER_LEVEL)?,
            officer_max_level: require_value(&spans, OFFICER_MAX_LEVEL)?,
            officer_max_elite_skills: require_value(&spans, OFFICER_MAX_ELITE_SKILLS)?,
        }
        .validate()?;
        let bonus_xp_use_mult_at_max_level = require_value(&spans, BONUS_XP_USE_MULT_AT_MAX_LEVEL)?;
        let officer_xp_required_mult = require_value(&spans, OFFICER_XP_REQUIRED_MULT)?;
        let revision = fingerprint(&bytes);
        Ok(Self {
            bytes,
            spans,
            values,
            bonus_xp_use_mult_at_max_level,
            officer_xp_required_mult,
            revision,
        })
    }

    fn progression_compatibility(&self) -> ProgressionSettingsCompatibility {
        let vanilla = GameSettingsValues::VANILLA_RC8;
        let simulator = save_core::Rc8Progression::default();
        ProgressionSettingsCompatibility {
            player: self.values.player_max_level == vanilla.player_max_level
                && self.values.skill_points_per_level == vanilla.skill_points_per_level
                && self.values.story_points_per_level == vanilla.story_points_per_level
                && self.bonus_xp_use_mult_at_max_level == simulator.bonus_xp_use_mult_at_max_level,
            officer: self.values.officer_max_level == vanilla.officer_max_level
                && self.officer_xp_required_mult == simulator.officer_xp_required_mult,
        }
    }

    fn patched(&self, values: GameSettingsValues) -> Result<Vec<u8>, CommandError> {
        let values = values.validate()?;
        let replacements = [
            (PLAYER_MAX_LEVEL, values.player_max_level),
            (SKILL_POINTS_PER_LEVEL, values.skill_points_per_level),
            (STORY_POINTS_PER_LEVEL, values.story_points_per_level),
            (OFFICER_MAX_LEVEL, values.officer_max_level),
            (OFFICER_MAX_ELITE_SKILLS, values.officer_max_elite_skills),
        ];
        let mut patches = replacements
            .into_iter()
            .map(|(key, value)| {
                let span = self.spans.get(key).ok_or_else(|| {
                    CommandError::new(
                        ErrorCode::ValidationFailed,
                        format!("settings.json is missing {key}"),
                    )
                })?;
                Ok((span.range.clone(), value.to_string().into_bytes()))
            })
            .collect::<Result<Vec<_>, CommandError>>()?;
        patches.sort_by_key(|(range, _)| range.start);

        let mut output = Vec::with_capacity(self.bytes.len() + 32);
        let mut position = 0;
        for (range, replacement) in patches {
            if range.start < position || range.end > self.bytes.len() {
                return Err(CommandError::internal(
                    "Game settings patch spans are invalid",
                ));
            }
            output.extend_from_slice(&self.bytes[position..range.start]);
            output.extend_from_slice(&replacement);
            position = range.end;
        }
        output.extend_from_slice(&self.bytes[position..]);
        let reparsed = Self::parse(output.clone())?;
        if reparsed.values != values {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Patched game settings did not preserve the requested values",
            ));
        }
        Ok(output)
    }
}

pub(crate) fn progression_settings_compatibility_rc8(
    installation_root: &Path,
) -> Result<ProgressionSettingsCompatibility, CommandError> {
    let path = settings_path(installation_root)?;
    let document = SettingsDocument::parse(read_regular_file(&path, MAX_SETTINGS_BYTES)?)?;
    Ok(document.progression_compatibility())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn is_eof(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.bytes.get(self.position) == Some(&expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expect_byte(&mut self, expected: u8, message: &str) -> Result<(), CommandError> {
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(CommandError::new(ErrorCode::ValidationFailed, message))
        }
    }

    fn skip_trivia(&mut self) -> Result<(), CommandError> {
        loop {
            while self
                .bytes
                .get(self.position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.position += 1;
            }
            if self.bytes.get(self.position) == Some(&b'#') {
                self.position += 1;
                while self
                    .bytes
                    .get(self.position)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.position += 1;
                }
                continue;
            }
            if self.bytes.get(self.position..self.position + 2) == Some(b"//") {
                self.position += 2;
                while self
                    .bytes
                    .get(self.position)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.position += 1;
                }
                continue;
            }
            if self.bytes.get(self.position..self.position + 2) == Some(b"/*") {
                self.position += 2;
                let Some(relative_end) = self.bytes[self.position..]
                    .windows(2)
                    .position(|window| window == b"*/")
                else {
                    return Err(CommandError::new(
                        ErrorCode::ValidationFailed,
                        "settings.json contains an unterminated block comment",
                    ));
                };
                self.position += relative_end + 2;
                continue;
            }
            return Ok(());
        }
    }

    fn parse_string(&mut self) -> Result<String, CommandError> {
        let start = self.position;
        self.expect_byte(b'"', "settings.json property name must be a string")?;
        let mut escaped = false;
        while let Some(byte) = self.bytes.get(self.position).copied() {
            self.position += 1;
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => {
                    return serde_json::from_slice(&self.bytes[start..self.position]).map_err(
                        |_| {
                            CommandError::new(
                                ErrorCode::ValidationFailed,
                                "settings.json contains an invalid string",
                            )
                        },
                    );
                }
                _ => {}
            }
        }
        Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "settings.json contains an unterminated string",
        ))
    }

    fn parse_u32(&mut self) -> Result<IntegerSpan, CommandError> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        if self.position == start
            || self
                .bytes
                .get(self.position)
                .is_some_and(|byte| matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Supported Starsector game settings must use nonnegative integer values",
            ));
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| {
            CommandError::new(ErrorCode::ValidationFailed, "Invalid game setting integer")
        })?;
        let value = raw.parse::<u32>().map_err(|_| {
            CommandError::new(
                ErrorCode::ValidationFailed,
                "Game setting integer exceeds the supported range",
            )
        })?;
        Ok(IntegerSpan {
            range: start..self.position,
            value,
        })
    }

    fn skip_value(&mut self) -> Result<(), CommandError> {
        let start = self.position;
        let mut object_depth = 0usize;
        let mut array_depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;

        while let Some(byte) = self.bytes.get(self.position).copied() {
            if in_string {
                self.position += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }

            if byte == b'"' {
                in_string = true;
                self.position += 1;
                continue;
            }
            if byte == b'#'
                || self.bytes.get(self.position..self.position + 2) == Some(b"//")
                || self.bytes.get(self.position..self.position + 2) == Some(b"/*")
            {
                self.skip_trivia()?;
                continue;
            }
            match byte {
                b'{' => object_depth += 1,
                b'}' if object_depth > 0 => object_depth -= 1,
                b'[' => array_depth += 1,
                b']' if array_depth > 0 => array_depth -= 1,
                b',' | b'}' if object_depth == 0 && array_depth == 0 => break,
                _ => {}
            }
            self.position += 1;
        }
        if self.position == start || in_string || object_depth != 0 || array_depth != 0 {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "settings.json contains a malformed value",
            ));
        }
        Ok(())
    }
}

fn recognized_key(key: &str) -> Option<&'static str> {
    match key {
        PLAYER_MAX_LEVEL => Some(PLAYER_MAX_LEVEL),
        SKILL_POINTS_PER_LEVEL => Some(SKILL_POINTS_PER_LEVEL),
        STORY_POINTS_PER_LEVEL => Some(STORY_POINTS_PER_LEVEL),
        BONUS_XP_USE_MULT_AT_MAX_LEVEL => Some(BONUS_XP_USE_MULT_AT_MAX_LEVEL),
        OFFICER_XP_REQUIRED_MULT => Some(OFFICER_XP_REQUIRED_MULT),
        OFFICER_MAX_LEVEL => Some(OFFICER_MAX_LEVEL),
        OFFICER_MAX_ELITE_SKILLS => Some(OFFICER_MAX_ELITE_SKILLS),
        _ => None,
    }
}

fn require_value(
    spans: &HashMap<&'static str, IntegerSpan>,
    key: &'static str,
) -> Result<u32, CommandError> {
    spans.get(key).map(|span| span.value).ok_or_else(|| {
        CommandError::new(
            ErrorCode::ValidationFailed,
            format!("settings.json is missing required {key}"),
        )
    })
}

fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, CommandError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings must be a regular non-symbolic-link file",
        ));
    }
    if metadata.len() > max_bytes {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings file exceeds the supported size limit",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > max_bytes) {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings file grew beyond the supported size limit",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity(u64, u64);

#[cfg(unix)]
fn file_identity(file: &File) -> Result<FileIdentity, CommandError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(FileIdentity(metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn file_identity(file: &File) -> Result<FileIdentity, CommandError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(FileIdentity(
        u64::from(information.dwVolumeSerialNumber),
        index,
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> Result<FileIdentity, CommandError> {
    Err(CommandError::new(
        ErrorCode::ValidationFailed,
        "Could not establish the identity of the game settings file on this platform",
    ))
}

fn ensure_regular_settings_path(path: &Path) -> Result<(), CommandError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings must be a regular non-symbolic-link file",
        ));
    }
    Ok(())
}

fn regular_settings_identity(path: &Path) -> Result<FileIdentity, CommandError> {
    ensure_regular_settings_path(path)?;
    let current_file = File::open(path)?;
    let identity = file_identity(&current_file)?;
    ensure_regular_settings_path(path)?;
    Ok(identity)
}

fn stale_settings_path() -> CommandError {
    CommandError::new(
        ErrorCode::StaleSave,
        "Game settings changed while they were being opened; reload before applying",
    )
    .disk_changed()
    .retryable()
}

fn verify_opened_settings_path(
    path: &Path,
    expected_identity: FileIdentity,
    file: &File,
) -> Result<(), CommandError> {
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings must be a regular file",
        ));
    }
    let opened_identity = file_identity(file)?;
    let current_identity = regular_settings_identity(path)?;
    if opened_identity != expected_identity || current_identity != opened_identity {
        return Err(stale_settings_path());
    }
    Ok(())
}

fn open_regular_settings_for_update(path: &Path) -> Result<(File, FileIdentity), CommandError> {
    // Keep this path check immediately beside the open. The identity comparison
    // catches a replacement between checking the path and opening the handle.
    ensure_regular_settings_path(path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let expected_identity = file_identity(&file)?;
    verify_opened_settings_path(path, expected_identity, &file)?;
    Ok((file, expected_identity))
}

fn validate_profile_name(name: &str) -> Result<String, CommandError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 64 {
        return Err(CommandError::invalid_argument(
            "Profile name must contain between 1 and 64 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(CommandError::invalid_argument(
            "Profile name cannot contain control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_custom_profile_id(profile_id: &GameSettingsProfileId) -> Result<(), CommandError> {
    if !profile_id.0.starts_with("profile-")
        || profile_id.0.len() > 128
        || !profile_id
            .0
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(CommandError::invalid_argument(
            "Game settings profile selector is invalid",
        ));
    }
    Ok(())
}

pub struct GameSettingsStore {
    profiles_file: PathBuf,
    backup_root: PathBuf,
    ensure_game_closed: fn() -> save_core::Result<()>,
    profile_lock: Mutex<()>,
    apply_lock: Mutex<()>,
}

impl GameSettingsStore {
    pub fn new(app_data_dir: &Path) -> Result<Self, std::io::Error> {
        Self::new_with_process_check(app_data_dir, save_core::ensure_starsector_closed)
    }

    fn new_with_process_check(
        app_data_dir: &Path,
        ensure_game_closed: fn() -> save_core::Result<()>,
    ) -> Result<Self, std::io::Error> {
        secure_create_dir_all(app_data_dir)?;
        let backup_root = app_data_dir.join("game-settings-backups");
        secure_create_dir_all(&backup_root)?;
        Ok(Self {
            profiles_file: app_data_dir.join("game-settings-profiles.json"),
            backup_root,
            ensure_game_closed,
            profile_lock: Mutex::new(()),
            apply_lock: Mutex::new(()),
        })
    }

    pub fn list_profiles(&self) -> Result<Vec<GameSettingsProfile>, CommandError> {
        let _guard = self
            .profile_lock
            .lock()
            .map_err(|_| CommandError::internal("Game settings profile state is unavailable"))?;
        let mut result = vec![GameSettingsProfile {
            profile_id: GameSettingsProfileId("builtin-vanilla-rc8".into()),
            name: "Vanilla RC8".into(),
            values: GameSettingsValues::VANILLA_RC8,
            built_in: true,
        }];
        result.extend(self.load_custom_profiles()?.profiles);
        Ok(result)
    }

    pub fn save_profile(
        &self,
        profile_id: Option<&GameSettingsProfileId>,
        name: &str,
        values: GameSettingsValues,
    ) -> Result<GameSettingsProfile, CommandError> {
        let _guard = self
            .profile_lock
            .lock()
            .map_err(|_| CommandError::internal("Game settings profile state is unavailable"))?;
        let name = validate_profile_name(name)?;
        let values = values.validate()?;
        if profile_id.is_some_and(|id| id.0.starts_with("builtin-")) {
            return Err(CommandError::invalid_argument(
                "Built-in game settings profiles cannot be overwritten",
            ));
        }
        let mut persisted = self.load_custom_profiles()?;
        if let Some(id) = profile_id {
            validate_custom_profile_id(id)?;
        }
        let id = profile_id.cloned().unwrap_or_else(|| {
            GameSettingsProfileId(format!("profile-{}", Uuid::new_v4().simple()))
        });
        let profile = GameSettingsProfile {
            profile_id: id.clone(),
            name,
            values,
            built_in: false,
        };
        if let Some(existing) = persisted
            .profiles
            .iter_mut()
            .find(|existing| existing.profile_id == id)
        {
            *existing = profile.clone();
        } else {
            if profile_id.is_some() {
                return Err(CommandError::not_found("Game settings profile"));
            }
            if persisted.profiles.len() >= MAX_CUSTOM_PROFILES {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    "The game settings profile limit has been reached",
                ));
            }
            persisted.profiles.push(profile.clone());
        }
        persisted
            .profiles
            .sort_by_key(|profile| profile.name.to_lowercase());
        self.persist_profiles(&persisted)?;
        Ok(profile)
    }

    pub fn delete_profile(&self, profile_id: &GameSettingsProfileId) -> Result<(), CommandError> {
        let _guard = self
            .profile_lock
            .lock()
            .map_err(|_| CommandError::internal("Game settings profile state is unavailable"))?;
        if profile_id.0.starts_with("builtin-") {
            return Err(CommandError::invalid_argument(
                "Built-in game settings profiles cannot be deleted",
            ));
        }
        validate_custom_profile_id(profile_id)?;
        let mut persisted = self.load_custom_profiles()?;
        let before = persisted.profiles.len();
        persisted
            .profiles
            .retain(|profile| &profile.profile_id != profile_id);
        if persisted.profiles.len() == before {
            return Err(CommandError::not_found("Game settings profile"));
        }
        self.persist_profiles(&persisted)
    }

    pub fn read_snapshot(
        &self,
        installation_id: InstallationId,
        installation_root: &Path,
        display_name: String,
    ) -> Result<GameSettingsSnapshot, CommandError> {
        let path = settings_path(installation_root)?;
        let document = SettingsDocument::parse(read_regular_file(&path, MAX_SETTINGS_BYTES)?)?;
        let writable = OpenOptions::new().write(true).open(&path).is_ok();
        Ok(GameSettingsSnapshot {
            installation_id,
            display_name,
            display_path: path.to_string_lossy().into_owned(),
            values: document.values,
            revision: document.revision,
            writable,
        })
    }

    pub fn apply(
        &self,
        installation_id: InstallationId,
        installation_root: &Path,
        display_name: String,
        expected_revision: &str,
        values: GameSettingsValues,
    ) -> Result<GameSettingsApplyResult, CommandError> {
        let _guard = self
            .apply_lock
            .lock()
            .map_err(|_| CommandError::internal("Game settings writer state is unavailable"))?;
        if expected_revision.len() != 64
            || !expected_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CommandError::invalid_argument(
                "Game settings revision is invalid",
            ));
        }
        (self.ensure_game_closed)()?;
        let path = settings_path(installation_root)?;
        let (mut locked_file, settings_identity) = open_regular_settings_for_update(&path)?;
        locked_file.try_lock_exclusive().map_err(|_| {
            CommandError::new(
                ErrorCode::GameRunning,
                "Starsector settings are in use; close the game and try again",
            )
            .retryable()
        })?;
        let original = read_locked_file(&mut locked_file, MAX_SETTINGS_BYTES)?;
        let document = SettingsDocument::parse(original.clone())?;
        if document.revision != expected_revision {
            return Err(CommandError::new(
                ErrorCode::StaleSave,
                "Starsector settings changed after they were opened; reload before applying",
            )
            .disk_changed()
            .retryable());
        }
        let output = document.patched(values)?;
        if output == original {
            return Err(CommandError::invalid_argument(
                "The selected game settings already match the requested values",
            ));
        }

        let final_live = read_locked_file(&mut locked_file, MAX_SETTINGS_BYTES)?;
        if fingerprint(&final_live) != expected_revision {
            let _ = FileExt::unlock(&locked_file);
            return Err(CommandError::new(
                ErrorCode::StaleSave,
                "Starsector settings changed while the update was being prepared; reload before applying",
            )
            .disk_changed()
            .retryable());
        }
        let (backup_id, backup_bytes) = self.create_backup(installation_root, &original)?;
        (self.ensure_game_closed)()?;
        verify_opened_settings_path(&path, settings_identity, &locked_file)?;
        #[cfg(windows)]
        {
            let _ = FileExt::unlock(&locked_file);
            drop(locked_file);
        }
        let replace_result = replace_file_atomically(&path, &output);
        #[cfg(not(windows))]
        {
            let _ = FileExt::unlock(&locked_file);
            drop(locked_file);
        }
        replace_result?;
        let committed = read_regular_file(&path, MAX_SETTINGS_BYTES)?;
        let committed_document = match SettingsDocument::parse(committed) {
            Ok(document) if document.values == values => document,
            _ => {
                if let Err(rollback_error) = replace_file_atomically(&path, &backup_bytes) {
                    return Err(CommandError::new(
                        ErrorCode::RecoveryRequired,
                        "Game settings validation failed and automatic rollback also failed",
                    )
                    .with_detail(format!(
                        "Preserved backup {backup_id}; rollback error category: {:?}",
                        rollback_error.code
                    )));
                }
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Game settings validation failed; the original file was restored",
                ));
            }
        };

        Ok(GameSettingsApplyResult {
            snapshot: GameSettingsSnapshot {
                installation_id,
                display_name,
                display_path: path.to_string_lossy().into_owned(),
                values: committed_document.values,
                revision: committed_document.revision,
                writable: true,
            },
            backup_id,
            message: "Game settings were backed up and updated. Restart Starsector to use them."
                .into(),
        })
    }

    fn load_custom_profiles(&self) -> Result<PersistedProfiles, CommandError> {
        let bytes = match read_regular_file(&self.profiles_file, MAX_PROFILE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.code == ErrorCode::NotFound => {
                return Ok(PersistedProfiles::default())
            }
            Err(error) => return Err(error),
        };
        let persisted: PersistedProfiles = serde_json::from_slice(&bytes).map_err(|_| {
            CommandError::new(
                ErrorCode::ValidationFailed,
                "Saved game settings profiles are malformed",
            )
        })?;
        if persisted.schema != 1 || persisted.profiles.len() > MAX_CUSTOM_PROFILES {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Saved game settings profiles use an unsupported format",
            ));
        }
        let mut profile_ids = HashSet::with_capacity(persisted.profiles.len());
        for profile in &persisted.profiles {
            if profile.built_in
                || profile.profile_id.0.starts_with("builtin-")
                || !profile.profile_id.0.starts_with("profile-")
                || !profile_ids.insert(profile.profile_id.clone())
            {
                return Err(CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Saved game settings profile identity is invalid",
                ));
            }
            validate_custom_profile_id(&profile.profile_id).map_err(|_| {
                CommandError::new(
                    ErrorCode::ValidationFailed,
                    "Saved game settings profile identity is invalid",
                )
            })?;
            validate_profile_name(&profile.name)?;
            profile.values.validate()?;
        }
        Ok(persisted)
    }

    fn persist_profiles(&self, persisted: &PersistedProfiles) -> Result<(), CommandError> {
        let bytes = serde_json::to_vec_pretty(persisted)
            .map_err(|_| CommandError::internal("Could not encode game settings profiles"))?;
        write_atomic_private(&self.profiles_file, &bytes).map_err(CommandError::from)
    }

    fn create_backup(
        &self,
        installation_root: &Path,
        bytes: &[u8],
    ) -> Result<(String, Vec<u8>), CommandError> {
        let install_fingerprint = fingerprint(installation_root.to_string_lossy().as_bytes());
        let install_root = self.backup_root.join(&install_fingerprint[..24]);
        secure_create_dir_all(&install_root)?;
        let backup_id = format!("settings-backup-{}", Uuid::new_v4().simple());
        let backup_dir = install_root.join(&backup_id);
        secure_create_dir(&backup_dir)?;
        write_private_new(&backup_dir.join("settings.json"), bytes)?;
        let manifest = SettingsBackupManifest {
            schema: 1,
            backup_id: backup_id.clone(),
            installation_fingerprint: install_fingerprint,
            source_revision: fingerprint(bytes),
        };
        write_private_new(
            &backup_dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)
                .map_err(|_| CommandError::internal("Could not encode game settings backup"))?,
        )?;
        Ok((backup_id, bytes.to_vec()))
    }
}

fn settings_path(installation_root: &Path) -> Result<PathBuf, CommandError> {
    let metadata = fs::symlink_metadata(installation_root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Starsector installation root is not a regular directory",
        ));
    }
    let root = fs::canonicalize(installation_root)?;
    #[cfg(windows)]
    let mut directory = root.join("starsector-core");
    #[cfg(not(windows))]
    let mut directory = root;
    #[cfg(windows)]
    {
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Starsector settings path contains an unavailable or symbolic-link directory",
            ));
        }
    }
    for component in ["data", "config"] {
        directory = directory.join(component);
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CommandError::new(
                ErrorCode::ValidationFailed,
                "Starsector settings path contains an unavailable or symbolic-link directory",
            ));
        }
    }
    Ok(directory.join("settings.json"))
}

fn read_locked_file(file: &mut File, max_bytes: u64) -> Result<Vec<u8>, CommandError> {
    let length = file.metadata()?.len();
    if length > max_bytes {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings file exceeds the supported size limit",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(CommandError::new(
            ErrorCode::ValidationFailed,
            "Game settings file grew beyond the supported size limit",
        ));
    }
    Ok(bytes)
}

fn replace_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    let parent = path
        .parent()
        .ok_or_else(|| CommandError::internal("Game settings path has no parent directory"))?;
    let temp = parent.join(format!(
        ".settings.json.ludds-blessing-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let original_permissions = fs::metadata(path)?.permissions();
    let result = (|| -> Result<(), CommandError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&temp, original_permissions)?;
        native_replace(path, &temp)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn native_replace(destination: &Path, replacement: &Path) -> Result<(), CommandError> {
    fs::rename(replacement, destination)?;
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn native_replace(destination: &Path, replacement: &Path) -> Result<(), CommandError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        // ReplaceFileW may reject metadata merging on sandboxed ACLs and some
        // filesystems. A native write-through replacement rename preserves the
        // same-directory atomicity guarantee in that case.
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

#[cfg(not(any(unix, windows)))]
fn native_replace(_destination: &Path, _replacement: &Path) -> Result<(), CommandError> {
    Err(CommandError::new(
        ErrorCode::ValidationFailed,
        "Atomic game settings replacement is unsupported on this platform",
    ))
}

fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("private file has no parent"))?;
    secure_create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("private"),
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        write_private_new(&temp, bytes)?;
        #[cfg(unix)]
        fs::rename(&temp, path)?;
        #[cfg(windows)]
        {
            if path.exists() {
                native_replace(path, &temp).map_err(command_error_to_io)?;
            } else {
                fs::rename(&temp, path)?;
            }
        }
        sync_directory(parent)
    })();
    if temp.exists() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(windows)]
fn command_error_to_io(error: CommandError) -> std::io::Error {
    std::io::Error::other(error.message)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn secure_create_dir_all(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    secure_directory_permissions(path)
}

fn secure_create_dir(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir(path)?;
    secure_directory_permissions(path)
}

#[cfg(unix)]
fn secure_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    // Every file handle is flushed before replacement. Windows does not
    // reliably support FlushFileBuffers on directory handles; the rename is
    // recorded by the filesystem journal.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config_dir(installation: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            installation.join("starsector-core/data/config")
        }
        #[cfg(not(windows))]
        {
            installation.join("data/config")
        }
    }

    fn fixture() -> Vec<u8> {
        br#"{
  # comments and unrelated nested data must survive
  "unrelated": { "playerMaxLevel": 999, "text": "officerMaxLevel" },
  "playerMaxLevel":15,
  "skillPointsPerLevel":1,
  "storyPointsPerLevel":4,
  "bonusXPUseMultAtMaxLevel":3,
  "officerXPRequiredMult":4,
  "officerMaxLevel":5,
  "officerMaxEliteSkills":1,
  "tail": [1, 2, 3]
}"#
        .to_vec()
    }

    #[test]
    fn settings_parser_ignores_comments_strings_and_nested_keys() {
        let document = SettingsDocument::parse(fixture()).unwrap();
        assert_eq!(document.values, GameSettingsValues::VANILLA_RC8);
        assert_eq!(document.spans.len(), 7);
        assert_eq!(
            document.progression_compatibility(),
            ProgressionSettingsCompatibility {
                player: true,
                officer: true,
            }
        );
    }

    #[test]
    fn progression_compatibility_checks_simulator_inputs_by_domain() {
        let source = String::from_utf8(fixture()).unwrap();

        let player_multiplier = SettingsDocument::parse(
            source
                .replace(
                    "\"bonusXPUseMultAtMaxLevel\":3",
                    "\"bonusXPUseMultAtMaxLevel\":2",
                )
                .into_bytes(),
        )
        .unwrap();
        assert_eq!(
            player_multiplier.progression_compatibility(),
            ProgressionSettingsCompatibility {
                player: false,
                officer: true,
            }
        );

        let officer_multiplier = SettingsDocument::parse(
            source
                .replace("\"officerXPRequiredMult\":4", "\"officerXPRequiredMult\":3")
                .into_bytes(),
        )
        .unwrap();
        assert_eq!(
            officer_multiplier.progression_compatibility(),
            ProgressionSettingsCompatibility {
                player: true,
                officer: false,
            }
        );

        let elite_cap = SettingsDocument::parse(
            source
                .replace("\"officerMaxEliteSkills\":1", "\"officerMaxEliteSkills\":2")
                .into_bytes(),
        )
        .unwrap();
        assert_eq!(
            elite_cap.progression_compatibility(),
            ProgressionSettingsCompatibility {
                player: true,
                officer: true,
            }
        );
    }

    #[test]
    fn settings_patch_changes_only_the_five_integer_tokens() {
        let source = fixture();
        let document = SettingsDocument::parse(source.clone()).unwrap();
        let desired = GameSettingsValues {
            player_max_level: 40,
            skill_points_per_level: 2,
            story_points_per_level: 8,
            officer_max_level: 10,
            officer_max_elite_skills: 4,
        };
        let output = document.patched(desired).unwrap();
        let reparsed = SettingsDocument::parse(output.clone()).unwrap();
        assert_eq!(reparsed.values, desired);
        assert_eq!(reparsed.bonus_xp_use_mult_at_max_level, 3);
        assert_eq!(reparsed.officer_xp_required_mult, 4);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("\"playerMaxLevel\": 999"));
    }

    #[test]
    fn duplicate_missing_fractional_and_out_of_range_settings_fail_closed() {
        let duplicate = String::from_utf8(fixture()).unwrap().replace(
            "\"playerMaxLevel\":15,",
            "\"playerMaxLevel\":15,\n\"playerMaxLevel\":20,",
        );
        assert_eq!(
            SettingsDocument::parse(duplicate.into_bytes())
                .unwrap_err()
                .code,
            ErrorCode::ValidationFailed
        );
        let missing = String::from_utf8(fixture())
            .unwrap()
            .replace("\"storyPointsPerLevel\":4,", "");
        assert!(SettingsDocument::parse(missing.into_bytes()).is_err());
        let fractional = String::from_utf8(fixture())
            .unwrap()
            .replace("\"skillPointsPerLevel\":1", "\"skillPointsPerLevel\":1.5");
        assert!(SettingsDocument::parse(fractional.into_bytes()).is_err());
        assert!(GameSettingsValues {
            player_max_level: 101,
            ..GameSettingsValues::VANILLA_RC8
        }
        .validate()
        .is_err());
    }

    #[test]
    fn custom_profiles_round_trip_and_builtins_are_immutable() {
        let root = tempfile::tempdir().unwrap();
        let store = GameSettingsStore::new(root.path()).unwrap();
        let created = store
            .save_profile(None, "Long campaign", GameSettingsValues::VANILLA_RC8)
            .unwrap();
        let profiles = store.list_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        assert!(profiles[0].built_in);
        assert_eq!(profiles[1].profile_id, created.profile_id);
        store.delete_profile(&created.profile_id).unwrap();
        assert_eq!(store.list_profiles().unwrap().len(), 1);
        assert!(store
            .delete_profile(&GameSettingsProfileId("builtin-vanilla-rc8".into()))
            .is_err());
        assert_eq!(
            store
                .save_profile(
                    Some(&GameSettingsProfileId("profile-forged".into())),
                    "Forged",
                    GameSettingsValues::VANILLA_RC8,
                )
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn apply_is_revision_bound_backed_up_and_reparsed() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("Starsector");
        let config = fixture_config_dir(&installation);
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("settings.json"), fixture()).unwrap();
        let store =
            GameSettingsStore::new_with_process_check(&root.path().join("app-data"), || Ok(()))
                .unwrap();
        let id = InstallationId::new("installation-test");
        let snapshot = store
            .read_snapshot(id.clone(), &installation, "Test install".into())
            .unwrap();
        let desired = GameSettingsValues {
            player_max_level: 25,
            skill_points_per_level: 2,
            ..snapshot.values
        };
        let result = store
            .apply(
                id,
                &installation,
                "Test install".into(),
                &snapshot.revision,
                desired,
            )
            .unwrap();
        assert_eq!(result.snapshot.values, desired);
        assert!(result.backup_id.starts_with("settings-backup-"));
        let backups = fs::read_dir(root.path().join("app-data/game-settings-backups"))
            .unwrap()
            .flat_map(|entry| fs::read_dir(entry.unwrap().path()).unwrap())
            .count();
        assert_eq!(backups, 1);
        assert!(store
            .apply(
                result.snapshot.installation_id.clone(),
                &installation,
                "Test install".into(),
                &snapshot.revision,
                GameSettingsValues::VANILLA_RC8,
            )
            .unwrap_err()
            .disk_changed
            .unwrap_or(false));
    }

    #[test]
    fn apply_refuses_to_touch_settings_while_starsector_is_running() {
        fn report_running() -> save_core::Result<()> {
            Err(save_core::CoreError::new(
                save_core::ErrorCode::GameRunning,
                "test process is running",
            ))
        }

        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("Starsector");
        let config = fixture_config_dir(&installation);
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("settings.json"), fixture()).unwrap();
        let store = GameSettingsStore::new_with_process_check(
            &root.path().join("app-data"),
            report_running,
        )
        .unwrap();
        let id = InstallationId::new("installation-test");
        let snapshot = store
            .read_snapshot(id.clone(), &installation, "Test install".into())
            .unwrap();
        let error = store
            .apply(
                id,
                &installation,
                "Test install".into(),
                &snapshot.revision,
                GameSettingsValues {
                    player_max_level: 20,
                    ..snapshot.values
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::GameRunning);
        assert_eq!(fs::read(config.join("settings.json")).unwrap(), fixture());
        assert!(!root
            .path()
            .join("app-data/game-settings-backups")
            .read_dir()
            .unwrap()
            .any(|_| true));
    }

    #[test]
    fn settings_update_opens_only_the_checked_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        fs::write(&path, fixture()).unwrap();

        let (file, identity) = open_regular_settings_for_update(&path).unwrap();
        verify_opened_settings_path(&path, identity, &file).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn settings_update_rejects_a_final_file_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.json");
        let link = root.path().join("settings.json");
        fs::write(&target, fixture()).unwrap();
        symlink(&target, &link).unwrap();

        let error = open_regular_settings_for_update(&link).unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
    }

    #[cfg(unix)]
    #[test]
    fn settings_update_rejects_a_path_replaced_after_open() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        let replacement = root.path().join("replacement.json");
        fs::write(&path, fixture()).unwrap();
        fs::write(&replacement, fixture()).unwrap();

        let expected_identity = regular_settings_identity(&path).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs::rename(&replacement, &path).unwrap();

        let error = verify_opened_settings_path(&path, expected_identity, &file).unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleSave);
        assert_eq!(error.disk_changed, Some(true));
    }
}
