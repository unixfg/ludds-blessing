use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        #[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }
    };
}

opaque_id!(InstallationId);
opaque_id!(RootId);
opaque_id!(SaveId);
opaque_id!(SessionId);
opaque_id!(ReviewId);
opaque_id!(BackupId);
opaque_id!(PortraitId);
opaque_id!(OfficerId);
opaque_id!(InventoryStackId);
opaque_id!(StorageStackId);
opaque_id!(ColonyResourceStackId);
opaque_id!(ColonyId);
opaque_id!(CatalogItemId);

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct InstallationInfo {
    pub installation_id: InstallationId,
    pub display_name: String,
    pub display_path: String,
    pub detected_version: Option<String>,
    pub saves_root_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct SaveRoot {
    pub root_id: RootId,
    pub display_name: String,
    pub display_path: String,
    pub available: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct DiscoveryResult {
    pub installations: Vec<InstallationInfo>,
    pub registered_roots: Vec<SaveRoot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum CompatibilityState {
    Editable,
    Preview,
    Locked,
    Unreadable,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct SaveSummary {
    pub id: SaveId,
    pub root_id: Option<RootId>,
    /// Display-only path. It cannot be fed to any command except explicit root registration.
    pub path: String,
    pub character_name: String,
    pub character_level: u32,
    pub game_version: String,
    pub save_file_version: String,
    pub save_date: String,
    pub location: String,
    pub iron_mode: bool,
    pub autosave: bool,
    pub compressed: bool,
    pub enabled_mods: Vec<String>,
    pub compatibility: CompatibilityState,
    pub compatibility_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct FieldCapability {
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct SkillView {
    pub id: String,
    pub name: String,
    pub group: String,
    pub rank: u8,
    pub max_rank: u8,
    pub editable: bool,
    pub reason: Option<String>,
    pub icon_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct PortraitView {
    pub id: PortraitId,
    pub relative_path: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct CharacterView {
    pub first_name: String,
    pub last_name: String,
    pub portrait_id: Option<PortraitId>,
    pub portrait_path: String,
    /// Decimal text validated and rounded by the backend as a finite Java float.
    pub credits: String,
    pub level: u32,
    /// Decimal string to avoid JavaScript integer precision loss.
    pub xp: String,
    pub skill_points: u32,
    pub story_points: u32,
    pub skills: Vec<SkillView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct RelationView {
    pub faction_id: String,
    pub display_name: String,
    pub value: f32,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct OfficerView {
    pub id: OfficerId,
    pub name: String,
    pub portrait_path: Option<String>,
    pub personality: String,
    pub assignment: Option<String>,
    pub level: u32,
    pub xp: String,
    pub skill_points: u32,
    pub skills: Vec<SkillView>,
    pub progression_editable: bool,
    pub progression_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum InventoryKind {
    Resources,
    Weapons,
    FighterWing,
    Special,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct InventoryStackView {
    pub id: InventoryStackId,
    pub item_id: String,
    pub special_data: Option<String>,
    pub name: String,
    pub kind: InventoryKind,
    /// Decimal text rounded and validated by the backend as a finite Java float.
    pub quantity: String,
    /// The maximum size already stored on this existing stack.
    pub max_quantity: String,
    pub cargo_space_per_unit: String,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct InventoryView {
    pub stacks: Vec<InventoryStackView>,
    pub used_space: String,
    pub max_space: Option<String>,
    pub overloaded: bool,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct StorageStackView {
    pub id: StorageStackId,
    pub item_id: String,
    pub special_data: Option<String>,
    pub name: String,
    pub kind: InventoryKind,
    pub quantity: String,
    pub max_quantity: String,
    pub cargo_space_per_unit: String,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct StorageView {
    pub stacks: Vec<StorageStackView>,
    pub used_space: String,
    pub max_space: Option<String>,
    pub overloaded: bool,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct ColonyResourceStackView {
    pub id: ColonyResourceStackId,
    pub item_id: String,
    pub special_data: Option<String>,
    pub name: String,
    pub kind: InventoryKind,
    pub quantity: String,
    pub max_quantity: String,
    pub cargo_space_per_unit: String,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct ColonyResourcesView {
    pub stacks: Vec<ColonyResourceStackView>,
    pub used_space: String,
    pub max_space: Option<String>,
    pub overloaded: bool,
    pub editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct ColonyView {
    pub id: ColonyId,
    pub name: String,
    pub location_context: Option<String>,
    pub storage: Option<StorageView>,
    pub local_resources: Option<ColonyResourcesView>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum AddableItemKind {
    Commodity,
    Weapon,
    FighterWing,
    ShipBlueprint,
    WeaponBlueprint,
    FighterBlueprint,
}

/// A catalog-backed item constructor. The opaque id is the only value the
/// frontend may submit when requesting a new saved stack.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct AddableItemView {
    pub id: CatalogItemId,
    pub item_id: String,
    pub name: String,
    pub kind: AddableItemKind,
    pub cargo_space_per_unit: String,
    pub max_quantity: String,
    pub local_resources_eligible: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct CatalogView {
    pub portraits: Vec<PortraitView>,
    pub addable_items: Vec<AddableItemView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct SaveSnapshot {
    pub session_id: SessionId,
    pub save_id: SaveId,
    /// Opaque digest of both source files; never XML or patch coordinates.
    pub revision: String,
    pub summary: SaveSummary,
    pub protected_locked: bool,
    pub write_capability: FieldCapability,
    pub progression_capability: FieldCapability,
    pub character: CharacterView,
    pub relations: Vec<RelationView>,
    pub officers: Vec<OfficerView>,
    pub inventory: Option<InventoryView>,
    pub colonies: Vec<ColonyView>,
    pub catalog: CatalogView,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct PortraitPayload {
    pub portrait_id: PortraitId,
    pub mime_type: String,
    pub data_base64: String,
}

/// The only semantic changes accepted across IPC. Raw XML/XPath edits are intentionally absent.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum Edit {
    #[serde(rename = "set_player_name")]
    SetPlayerName {
        first_name: String,
        last_name: String,
    },
    #[serde(rename = "set_player_portrait")]
    SetPlayerPortrait { portrait_id: PortraitId },
    #[serde(rename = "set_credits")]
    SetCredits { value: String },
    #[serde(rename = "grant_player_xp")]
    GrantPlayerXp { amount: String },
    #[serde(rename = "raise_player_to_level")]
    RaisePlayerToLevel { level: u32 },
    #[serde(rename = "set_player_points")]
    SetPlayerPoints {
        skill_points: u32,
        story_points: u32,
    },
    #[serde(rename = "set_player_skill")]
    SetPlayerSkill { skill_id: String, rank: u8 },
    #[serde(rename = "set_relation")]
    SetRelation { faction_id: String, value: f32 },
    #[serde(rename = "grant_officer_xp")]
    GrantOfficerXp {
        officer_id: OfficerId,
        amount: String,
    },
    #[serde(rename = "raise_officer_to_level")]
    RaiseOfficerToLevel { officer_id: OfficerId, level: u32 },
    #[serde(rename = "set_officer_points")]
    SetOfficerPoints {
        officer_id: OfficerId,
        skill_points: u32,
    },
    #[serde(rename = "set_officer_skill")]
    SetOfficerSkill {
        officer_id: OfficerId,
        skill_id: String,
        rank: u8,
    },
    #[serde(rename = "set_inventory_quantity")]
    SetInventoryQuantity {
        stack_id: InventoryStackId,
        quantity: String,
    },
    #[serde(rename = "set_storage_stack_quantity")]
    SetStorageStackQuantity {
        colony_id: ColonyId,
        stack_id: StorageStackId,
        quantity: String,
    },
    #[serde(rename = "set_colony_resource_quantity")]
    SetColonyResourceQuantity {
        colony_id: ColonyId,
        stack_id: ColonyResourceStackId,
        quantity: String,
    },
    #[serde(rename = "add_storage_item")]
    AddStorageItem {
        colony_id: ColonyId,
        catalog_item_id: CatalogItemId,
        quantity: String,
    },
    #[serde(rename = "add_colony_resource")]
    AddColonyResource {
        colony_id: ColonyId,
        catalog_item_id: CatalogItemId,
        quantity: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum ReviewSection {
    Character,
    Reputation,
    Officers,
    Inventory,
    Colonies,
    Save,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct ReviewChange {
    pub key: String,
    pub section: ReviewSection,
    pub label: String,
    pub before: String,
    pub after: String,
    pub derived: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct Review {
    pub review_id: ReviewId,
    pub revision: String,
    pub changes: Vec<ReviewChange>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub can_apply: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum ApplyMode {
    #[serde(rename = "replace_original")]
    ReplaceOriginal,
    #[serde(rename = "save_copy")]
    SaveCopy { target_root: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct ApplyResult {
    pub save_id: SaveId,
    pub backup_id: Option<BackupId>,
    pub target_path: String,
    pub campaign_hash: String,
    pub descriptor_hash: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct BackupSummary {
    pub id: BackupId,
    pub save_id: SaveId,
    pub created_at: String,
    pub reason: String,
    pub size_bytes: String,
    pub game_version: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub enum RecoveryStatus {
    Clear,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct RecoveryItem {
    pub transaction_id: String,
    pub save_id: Option<SaveId>,
    pub summary: String,
    pub last_completed_phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct RecoveryState {
    pub status: RecoveryStatus,
    pub items: Vec<RecoveryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings/"))]
pub struct Diagnostics {
    pub app_version: String,
    pub os: String,
    /// Redacted operational events only; save content and full paths are excluded.
    pub entries: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_match_the_frontend_discriminated_union() {
        let value = serde_json::to_value(Edit::RaiseOfficerToLevel {
            officer_id: OfficerId::new("officer-opaque"),
            level: 7,
        })
        .unwrap();
        assert_eq!(value["type"], "raise_officer_to_level");
        assert_eq!(value["officerId"], "officer-opaque");
        assert_eq!(value["level"], 7);
    }

    #[test]
    fn save_copy_mode_is_tagged_and_camel_case() {
        let value = serde_json::to_value(ApplyMode::SaveCopy {
            target_root: "opaque-display-path".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "save_copy");
        assert_eq!(value["targetRoot"], "opaque-display-path");
    }

    #[test]
    fn cargo_edits_use_distinct_opaque_stack_types() {
        let inventory = serde_json::to_value(Edit::SetInventoryQuantity {
            stack_id: InventoryStackId::new("inventory-stack"),
            quantity: "12".into(),
        })
        .unwrap();
        assert_eq!(inventory["type"], "set_inventory_quantity");
        assert_eq!(inventory["stackId"], "inventory-stack");
        assert_eq!(inventory["quantity"], "12");

        let storage = serde_json::to_value(Edit::SetStorageStackQuantity {
            colony_id: ColonyId::new("colony"),
            stack_id: StorageStackId::new("storage-stack"),
            quantity: "3".into(),
        })
        .unwrap();
        assert_eq!(storage["type"], "set_storage_stack_quantity");
        assert_eq!(storage["colonyId"], "colony");
        assert_eq!(storage["stackId"], "storage-stack");

        let resources = serde_json::to_value(Edit::SetColonyResourceQuantity {
            colony_id: ColonyId::new("colony"),
            stack_id: ColonyResourceStackId::new("resource-stack"),
            quantity: "42.5".into(),
        })
        .unwrap();
        assert_eq!(resources["type"], "set_colony_resource_quantity");
        assert_eq!(resources["colonyId"], "colony");
        assert_eq!(resources["stackId"], "resource-stack");
    }
}
