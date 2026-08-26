use crate::error::{CoreError, ErrorCode, Result};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

macro_rules! decimal_string {
    ($name:ident, $inner:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub $inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }
            pub const fn get(self) -> $inner {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = <$inner as FromStr>::Err;
            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse::<$inner>().map(Self).map_err(de::Error::custom)
            }
        }
    };
}

decimal_string!(DecimalU64, u64);
decimal_string!(DecimalI64, i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveLocation {
    pub save_dir: PathBuf,
    pub campaign_path: PathBuf,
    pub descriptor_path: PathBuf,
}

impl SaveLocation {
    pub fn from_save_dir(save_dir: impl Into<PathBuf>) -> Self {
        let save_dir = save_dir.into();
        Self {
            campaign_path: save_dir.join("campaign.xml"),
            descriptor_path: save_dir.join("descriptor.xml"),
            save_dir,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub sha256: String,
    pub byte_len: DecimalU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRevision {
    pub campaign: FileFingerprint,
    pub descriptor: FileFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Compatibility {
    Editable,
    ReadOnly { code: ErrorCode, reason: String },
    Invalid { code: ErrorCode, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveMetadata {
    pub character_name: String,
    pub portrait: String,
    pub game_version: String,
    pub save_format: String,
    pub character_level: u32,
    pub compressed: bool,
    pub iron_mode: bool,
    pub autosave: bool,
    pub difficulty: String,
    pub location_description: String,
    pub save_date: String,
    pub slot_creation_timestamp: Option<DecimalI64>,
    pub enabled_mods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveSummary {
    pub save_id: String,
    pub location: SaveLocation,
    pub metadata: Option<SaveMetadata>,
    pub compatibility: Compatibility,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
    pub acknowledgement_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldCapabilities {
    pub basic_character: bool,
    pub progression: bool,
    pub skills: bool,
    pub reputation: bool,
    pub officers: bool,
    pub inventory: bool,
    pub colony_storage: bool,
    pub colony_resources: bool,
    pub protected_save: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryKind {
    Resources,
    Weapons,
    FighterChip,
    Special,
    Unknown,
}

impl InventoryKind {
    pub const fn requires_whole_quantity(self) -> bool {
        matches!(self, Self::Weapons | Self::FighterChip | Self::Special)
    }
}

/// A caller-visible cargo catalog key. The shell constructs these only from
/// the validated local Starsector installation and enabled-mod catalogs.
///
/// `special_data` is part of a special stack's identity: for example,
/// `ship_bp` plus a hull ID identifies one blueprint, while a blueprint
/// package has no payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CargoItemKey {
    pub kind: InventoryKind,
    pub item_id: String,
    pub special_data: Option<String>,
}

/// Serialization fields derived from a caller-validated local catalog.
/// Values supplied by an edit are never trusted for these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CargoAdditionSpec {
    pub key: CargoItemKey,
    pub cargo_space_per_unit: f32,
    /// True only for economic, non-meta commodities accepted by the exact
    /// RC8 Local Resources plugin.
    pub local_resources_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryStack {
    /// Opaque, save-bound selector. Never an XML identity or byte offset.
    pub stack_id: String,
    pub kind: InventoryKind,
    /// The local catalog ID when the stack has a supported RC8 representation.
    pub item_id: String,
    /// Optional payload that distinguishes special-item variants.
    pub special_data: Option<String>,
    pub quantity: f32,
    pub max_quantity: f32,
    pub cargo_space_per_unit: f32,
    /// Structural authorization only. The shell must additionally require a
    /// unique entry in its validated local game/mod catalog.
    pub structurally_editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryView {
    pub stacks: Vec<InventoryStack>,
    pub used_space: f32,
    pub max_space: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Colony {
    /// Opaque selector derived from the authoritative economy market roster.
    pub colony_id: String,
    pub name: String,
    pub location_context: Option<String>,
    pub storage: Option<InventoryView>,
    pub local_resources: Option<InventoryView>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRank {
    Unlearned,
    Learned,
    Elite,
}

impl SkillRank {
    pub const fn numeric(self) -> u8 {
        match self {
            Self::Unlearned => 0,
            Self::Learned => 1,
            Self::Elite => 2,
        }
    }

    pub fn from_numeric(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Unlearned),
            1 => Ok(Self::Learned),
            2 => Ok(Self::Elite),
            _ => Err(CoreError::validation(format!("invalid skill rank {value}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillState {
    pub id: String,
    pub rank: SkillRank,
    pub known: bool,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressionView {
    pub level: u32,
    pub xp: DecimalU64,
    pub story_checkpoint_xp: DecimalU64,
    pub bonus_xp: DecimalU64,
    pub deferred_bonus_xp: DecimalU64,
    pub skill_points: u32,
    pub story_points: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerCharacter {
    pub first_name: String,
    pub last_name: String,
    pub full_name: String,
    pub portrait: String,
    pub credits: f32,
    pub progression: ProgressionView,
    pub skills: Vec<SkillState>,
    pub skills_ever_elite: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Officer {
    pub officer_id: String,
    pub first_name: String,
    pub last_name: String,
    pub portrait: String,
    pub personality: String,
    pub assigned: bool,
    pub progression: ProgressionView,
    pub skills: Vec<SkillState>,
    pub pending_skill_picks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactionRelation {
    pub faction_id: String,
    pub value_percent: f32,
    pub has_history: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveSnapshot {
    pub save_id: String,
    pub revision: ContentRevision,
    pub metadata: SaveMetadata,
    pub compatibility: Compatibility,
    pub capabilities: FieldCapabilities,
    pub character: PlayerCharacter,
    pub inventory: InventoryView,
    pub reputation: Vec<FactionRelation>,
    pub officers: Vec<Officer>,
    pub colonies: Vec<Colony>,
    pub warnings: Vec<Warning>,
}

/// The complete set of semantic edits accepted by the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Edit {
    SetName {
        first_name: String,
        last_name: String,
    },
    SetPortrait {
        portrait_id: String,
    },
    SetCredits {
        value: f32,
    },
    GrantPlayerXp {
        amount: DecimalU64,
    },
    RaisePlayerToLevel {
        level: u32,
    },
    SetPlayerPoints {
        skill_points: u32,
        story_points: u32,
    },
    SetPlayerSkill {
        skill_id: String,
        rank: SkillRank,
    },
    SetFactionRelation {
        faction_id: String,
        value_percent: f32,
    },
    GrantOfficerXp {
        officer_id: String,
        amount: DecimalU64,
    },
    RaiseOfficerToLevel {
        officer_id: String,
        level: u32,
    },
    SetOfficerPoints {
        officer_id: String,
        skill_points: u32,
    },
    SetOfficerSkill {
        officer_id: String,
        skill_id: String,
        rank: SkillRank,
    },
    SetInventoryStackQuantity {
        stack_id: String,
        value: f32,
    },
    SetStorageStackQuantity {
        colony_id: String,
        stack_id: String,
        value: f32,
    },
    SetColonyResourceQuantity {
        colony_id: String,
        stack_id: String,
        value: f32,
    },
    AddStorageStack {
        colony_id: String,
        item: CargoItemKey,
        quantity: f32,
    },
    AddColonyResourceStack {
        colony_id: String,
        commodity_id: String,
        quantity: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    ReplaceOriginal,
    SaveCopy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewChange {
    pub field: String,
    pub old_value: String,
    pub new_value: String,
    pub derived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub review_id: String,
    pub save_id: String,
    pub source_revision: ContentRevision,
    pub changes: Vec<ReviewChange>,
    pub warnings: Vec<Warning>,
}
