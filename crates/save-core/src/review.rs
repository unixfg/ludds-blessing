use crate::descriptor::parse_descriptor;
use crate::error::{CoreError, ErrorCode, Result};
use crate::file_util::fingerprint;
use crate::model::*;
use crate::patch::{apply_patches, PatchBuilder, SpanPatch};
use crate::progression::{
    grant_officer_xp, grant_player_xp, raise_officer_to_level, raise_player_to_level,
    Rc8Progression,
};
use crate::semantic::{
    cargo_stack_selector, extract_campaign, join_name, CargoAnchors, CargoStackState, CargoState,
    InternalStats, OpenedSave, SemanticState, StatsAnchors,
};
use crate::skill_json::SkillJsonDocument;
use crate::xml::{escape_xml_attribute, escape_xml_text, XmlDocument};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use uuid::Uuid;

#[derive(Debug)]
pub struct PreparedReview {
    pub(crate) summary: ReviewSummary,
    pub(crate) location: SaveLocation,
    pub(crate) campaign_source: Vec<u8>,
    pub(crate) descriptor_source: Vec<u8>,
    pub(crate) campaign_patches: Vec<SpanPatch>,
    pub(crate) descriptor_patches: Vec<SpanPatch>,
    pub(crate) campaign_output: Vec<u8>,
    pub(crate) descriptor_output: Vec<u8>,
    pub(crate) protected_write_authorized: bool,
}

impl PreparedReview {
    pub fn summary(&self) -> &ReviewSummary {
        &self.summary
    }

    pub fn campaign_output_fingerprint(&self) -> FileFingerprint {
        fingerprint(&self.campaign_output)
    }

    pub fn descriptor_output_fingerprint(&self) -> FileFingerprint {
        fingerprint(&self.descriptor_output)
    }

    pub fn campaign_patch_count(&self) -> usize {
        self.campaign_patches.len()
    }

    pub fn descriptor_patch_count(&self) -> usize {
        self.descriptor_patches.len()
    }

    pub fn validate_against(&self, campaign: &[u8], descriptor: &[u8]) -> Result<()> {
        let actual = ContentRevision {
            campaign: fingerprint(campaign),
            descriptor: fingerprint(descriptor),
        };
        if actual != self.summary.source_revision {
            return Err(CoreError::new(
                ErrorCode::StaleSave,
                "save contents changed after review was prepared",
            ));
        }
        Ok(())
    }
}

impl OpenedSave {
    /// Prepares a review using only skill IDs already present on the edited
    /// character as authorization. This intentionally cannot introduce a new
    /// skill member; callers with a validated local catalog should use
    /// [`Self::prepare_review_with_skill_catalog`].
    pub fn prepare_review(&self, edits: &[Edit]) -> Result<PreparedReview> {
        self.prepare_review_authorized(
            edits,
            SkillAuthorization::ExistingOnly,
            StackAuthorization::Denied,
            AdditionAuthorization::Denied,
        )
    }

    /// Prepares a review using skill IDs from a caller-validated local game/mod
    /// catalog. Only IDs in `trusted_skill_ids` may be edited, including IDs
    /// absent from the current JSON skill maps.
    ///
    /// The core never treats IDs found in the save as catalog authorization:
    /// unknown or malformed mod skill data therefore remains byte-preserved and
    /// read-only.
    pub fn prepare_review_with_skill_catalog(
        &self,
        edits: &[Edit],
        trusted_skill_ids: &HashSet<String>,
    ) -> Result<PreparedReview> {
        self.prepare_review_authorized(
            edits,
            SkillAuthorization::Trusted(trusted_skill_ids),
            StackAuthorization::Denied,
            AdditionAuthorization::Denied,
        )
    }

    /// Prepares a review using caller-validated skill and cargo catalogs.
    ///
    /// Cargo authorization is expressed as opaque stack selectors returned by
    /// this exact save snapshot. A selector is still rechecked against the
    /// authoritative cargo graph and structural capability in the core.
    pub fn prepare_review_with_catalogs(
        &self,
        edits: &[Edit],
        trusted_skill_ids: &HashSet<String>,
        trusted_stack_ids: &HashSet<String>,
    ) -> Result<PreparedReview> {
        self.prepare_review_authorized(
            edits,
            SkillAuthorization::Trusted(trusted_skill_ids),
            StackAuthorization::Trusted(trusted_stack_ids),
            AdditionAuthorization::Denied,
        )
    }

    /// Prepares a review with explicit authorization for creating new RC8
    /// cargo stacks. Addition specs must be derived from a validated local
    /// installation/mod catalog; the core rechecks their shape, destination,
    /// quantities, graph ownership, and serialized cache values.
    pub fn prepare_review_with_catalogs_and_additions(
        &self,
        edits: &[Edit],
        trusted_skill_ids: &HashSet<String>,
        trusted_stack_ids: &HashSet<String>,
        trusted_additions: &HashMap<CargoItemKey, CargoAdditionSpec>,
    ) -> Result<PreparedReview> {
        self.prepare_review_authorized(
            edits,
            SkillAuthorization::Trusted(trusted_skill_ids),
            StackAuthorization::Trusted(trusted_stack_ids),
            AdditionAuthorization::Trusted(trusted_additions),
        )
    }

    fn prepare_review_authorized(
        &self,
        edits: &[Edit],
        skill_authorization: SkillAuthorization<'_>,
        stack_authorization: StackAuthorization<'_>,
        addition_authorization: AdditionAuthorization<'_>,
    ) -> Result<PreparedReview> {
        if edits.is_empty() {
            return Err(CoreError::invalid_edit("at least one edit is required"));
        }
        reject_duplicate_cargo_edit_targets(edits)?;
        let mut desired = self.state.clone();
        let mut invalidated_officers = HashSet::new();
        let mut player_progression_requested = false;
        let mut officer_progression_requested = HashSet::new();
        let mut additions = Vec::new();
        let mut next_identity = None;

        for edit in edits {
            match edit {
                Edit::SetName {
                    first_name,
                    last_name,
                } => {
                    require_capability(
                        self.snapshot.capabilities.basic_character,
                        &self.snapshot.compatibility,
                        "character editing",
                    )?;
                    validate_name(first_name, "first name", false)?;
                    validate_name(last_name, "last name", true)?;
                    desired.player.first_name = first_name.trim().to_owned();
                    desired.player.last_name = last_name.trim().to_owned();
                    desired.character_summary_name =
                        join_name(&desired.player.first_name, &desired.player.last_name);
                }
                Edit::SetPortrait { portrait_id } => {
                    require_capability(
                        self.snapshot.capabilities.basic_character,
                        &self.snapshot.compatibility,
                        "portrait editing",
                    )?;
                    validate_portrait_id(portrait_id)?;
                    desired.player.portrait = portrait_id.clone();
                    desired.character_summary_portrait = portrait_id.clone();
                }
                Edit::SetCredits { value } => {
                    require_capability(
                        self.snapshot.capabilities.basic_character,
                        &self.snapshot.compatibility,
                        "credit editing",
                    )?;
                    if !value.is_finite() || *value < 0.0 {
                        return Err(CoreError::invalid_edit(
                            "credits must be a finite, nonnegative Java float",
                        ));
                    }
                    desired.credits = *value;
                }
                Edit::GrantPlayerXp { amount } => {
                    require_capability(
                        self.snapshot.capabilities.progression,
                        &self.snapshot.compatibility,
                        "player XP simulation",
                    )?;
                    let progress =
                        grant_player_xp(&desired.player.stats.player_progress(), amount.get())?;
                    desired.player.stats.update_player(progress);
                    player_progression_requested = true;
                }
                Edit::RaisePlayerToLevel { level } => {
                    require_capability(
                        self.snapshot.capabilities.progression,
                        &self.snapshot.compatibility,
                        "player XP simulation",
                    )?;
                    let progress =
                        raise_player_to_level(&desired.player.stats.player_progress(), *level)?;
                    desired.player.stats.update_player(progress);
                    player_progression_requested = true;
                }
                Edit::SetPlayerPoints {
                    skill_points,
                    story_points,
                } => {
                    require_capability(
                        self.snapshot.capabilities.skills,
                        &self.snapshot.compatibility,
                        "player point editing",
                    )?;
                    validate_point_total(*skill_points)?;
                    validate_point_total(*story_points)?;
                    desired.player.stats.skill_points = *skill_points;
                    desired.player.stats.story_points = *story_points;
                }
                Edit::SetPlayerSkill { skill_id, rank } => {
                    require_capability(
                        self.snapshot.capabilities.skills,
                        &self.snapshot.compatibility,
                        "player skill editing",
                    )?;
                    skill_authorization.require(skill_id, &self.state.player.skills)?;
                    if *rank == SkillRank::Unlearned
                        && !desired.player.skills.contains_key(skill_id)
                    {
                        // Do not add a zero-valued member for an absent skill.
                    } else {
                        desired.player.skills.insert(skill_id.clone(), *rank);
                    }
                    if *rank == SkillRank::Elite
                        && !desired.skills_ever_elite.iter().any(|id| id == skill_id)
                    {
                        desired.skills_ever_elite.push(skill_id.clone());
                    }
                }
                Edit::SetFactionRelation {
                    faction_id,
                    value_percent,
                } => {
                    require_capability(
                        self.snapshot.capabilities.reputation,
                        &self.snapshot.compatibility,
                        "reputation editing",
                    )?;
                    if !value_percent.is_finite() || !(-100.0..=100.0).contains(value_percent) {
                        return Err(CoreError::invalid_edit(
                            "reputation must be finite and within -100..100",
                        ));
                    }
                    let relation = desired.relations.get_mut(faction_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "no existing player relation for faction '{faction_id}'"
                        ))
                    })?;
                    let game_value = *value_percent / 100.0;
                    relation.value_percent = game_value * 100.0;
                }
                Edit::GrantOfficerXp { officer_id, amount } => {
                    require_capability(
                        self.snapshot.capabilities.progression,
                        &self.snapshot.compatibility,
                        "officer XP simulation",
                    )?;
                    let officer = desired.officers.get_mut(officer_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown officer '{officer_id}'"))
                    })?;
                    let progress = grant_officer_xp(
                        &officer.person.stats.officer_progress(),
                        amount.get(),
                        Rc8Progression::default().officer_max_level,
                    )?;
                    officer.person.stats.update_officer(progress);
                    officer.pending_skill_picks.clear();
                    officer.made_picks = false;
                    invalidated_officers.insert(officer_id.clone());
                    officer_progression_requested.insert(officer_id.clone());
                }
                Edit::RaiseOfficerToLevel { officer_id, level } => {
                    require_capability(
                        self.snapshot.capabilities.progression,
                        &self.snapshot.compatibility,
                        "officer progression editing",
                    )?;
                    let officer = desired.officers.get_mut(officer_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown officer '{officer_id}'"))
                    })?;
                    let progress = raise_officer_to_level(
                        &officer.person.stats.officer_progress(),
                        *level,
                        Rc8Progression::default().officer_max_level,
                    )?;
                    officer.person.stats.update_officer(progress);
                    officer.pending_skill_picks.clear();
                    officer.made_picks = false;
                    invalidated_officers.insert(officer_id.clone());
                    officer_progression_requested.insert(officer_id.clone());
                }
                Edit::SetOfficerPoints {
                    officer_id,
                    skill_points,
                } => {
                    require_capability(
                        self.snapshot.capabilities.officers,
                        &self.snapshot.compatibility,
                        "officer point editing",
                    )?;
                    validate_point_total(*skill_points)?;
                    let officer = desired.officers.get_mut(officer_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown officer '{officer_id}'"))
                    })?;
                    officer.person.stats.skill_points = *skill_points;
                    officer.pending_skill_picks.clear();
                    officer.made_picks = false;
                    invalidated_officers.insert(officer_id.clone());
                }
                Edit::SetOfficerSkill {
                    officer_id,
                    skill_id,
                    rank,
                } => {
                    require_capability(
                        self.snapshot.capabilities.officers,
                        &self.snapshot.compatibility,
                        "officer skill editing",
                    )?;
                    let officer = desired.officers.get_mut(officer_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown officer '{officer_id}'"))
                    })?;
                    let original_officer =
                        self.state.officers.get(officer_id).ok_or_else(|| {
                            CoreError::invalid_edit(format!("unknown officer '{officer_id}'"))
                        })?;
                    skill_authorization.require(skill_id, &original_officer.person.skills)?;
                    if *rank == SkillRank::Unlearned
                        && !officer.person.skills.contains_key(skill_id)
                    {
                        // A no-op should not introduce a zero-valued member.
                    } else {
                        officer.person.skills.insert(skill_id.clone(), *rank);
                    }
                    officer.pending_skill_picks.clear();
                    officer.made_picks = false;
                    invalidated_officers.insert(officer_id.clone());
                }
                Edit::SetInventoryStackQuantity { stack_id, value } => {
                    require_capability(
                        self.snapshot.capabilities.inventory,
                        &self.snapshot.compatibility,
                        "player inventory editing",
                    )?;
                    stack_authorization.require(stack_id)?;
                    let original = self.state.inventory.stacks.get(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown player inventory stack '{stack_id}'"
                        ))
                    })?;
                    validate_stack_quantity(original, *value)?;
                    let stack = desired.inventory.stacks.get_mut(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown player inventory stack '{stack_id}'"
                        ))
                    })?;
                    stack.quantity = *value;
                    desired.inventory.used_space = desired.inventory.recompute_used_space()?;
                }
                Edit::SetStorageStackQuantity {
                    colony_id,
                    stack_id,
                    value,
                } => {
                    require_capability(
                        self.snapshot.capabilities.colony_storage,
                        &self.snapshot.compatibility,
                        "colony storage editing",
                    )?;
                    stack_authorization.require(stack_id)?;
                    let original_colony = self.state.colonies.get(colony_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown colony '{colony_id}'"))
                    })?;
                    let original_storage = original_colony.storage.as_ref().ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "colony '{colony_id}' has no editable storage"
                        ))
                    })?;
                    let original = original_storage.stacks.get(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown storage stack '{stack_id}' for colony '{colony_id}'"
                        ))
                    })?;
                    validate_stack_quantity(original, *value)?;
                    let colony = desired.colonies.get_mut(colony_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown colony '{colony_id}'"))
                    })?;
                    let storage = colony.storage.as_mut().ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "colony '{colony_id}' has no editable storage"
                        ))
                    })?;
                    let stack = storage.stacks.get_mut(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown storage stack '{stack_id}' for colony '{colony_id}'"
                        ))
                    })?;
                    stack.quantity = *value;
                    storage.used_space = storage.recompute_used_space()?;
                }
                Edit::SetColonyResourceQuantity {
                    colony_id,
                    stack_id,
                    value,
                } => {
                    require_capability(
                        self.snapshot.capabilities.colony_resources,
                        &self.snapshot.compatibility,
                        "colony Local Resources editing",
                    )?;
                    stack_authorization.require(stack_id)?;
                    let original_colony = self.state.colonies.get(colony_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown colony '{colony_id}'"))
                    })?;
                    let original_resources =
                        original_colony.local_resources.as_ref().ok_or_else(|| {
                            CoreError::invalid_edit(format!(
                                "colony '{colony_id}' has no editable Local Resources"
                            ))
                        })?;
                    let original = original_resources.stacks.get(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown Local Resources stack '{stack_id}' for colony '{colony_id}'"
                        ))
                    })?;
                    if original.kind != InventoryKind::Resources {
                        return Err(CoreError::invalid_edit(
                            "Local Resources edits are limited to commodity resource stacks",
                        ));
                    }
                    validate_stack_quantity(original, *value)?;
                    let colony = desired.colonies.get_mut(colony_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!("unknown colony '{colony_id}'"))
                    })?;
                    let resources = colony.local_resources.as_mut().ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "colony '{colony_id}' has no editable Local Resources"
                        ))
                    })?;
                    let stack = resources.stacks.get_mut(stack_id).ok_or_else(|| {
                        CoreError::invalid_edit(format!(
                            "unknown Local Resources stack '{stack_id}' for colony '{colony_id}'"
                        ))
                    })?;
                    stack.quantity = *value;
                    resources.used_space = resources.recompute_used_space()?;
                }
                Edit::AddStorageStack {
                    colony_id,
                    item,
                    quantity,
                } => {
                    require_capability(
                        self.snapshot.capabilities.colony_storage,
                        &self.snapshot.compatibility,
                        "colony storage stack creation",
                    )?;
                    stage_colony_cargo_addition(
                        self,
                        &mut desired,
                        addition_authorization,
                        &mut additions,
                        &mut next_identity,
                        colony_id,
                        CargoAdditionTarget::Storage,
                        item,
                        *quantity,
                    )?;
                }
                Edit::AddColonyResourceStack {
                    colony_id,
                    commodity_id,
                    quantity,
                } => {
                    require_capability(
                        self.snapshot.capabilities.colony_resources,
                        &self.snapshot.compatibility,
                        "colony Local Resources stack creation",
                    )?;
                    let item = CargoItemKey {
                        kind: InventoryKind::Resources,
                        item_id: commodity_id.clone(),
                        special_data: None,
                    };
                    stage_colony_cargo_addition(
                        self,
                        &mut desired,
                        addition_authorization,
                        &mut additions,
                        &mut next_identity,
                        colony_id,
                        CargoAdditionTarget::LocalResources,
                        &item,
                        *quantity,
                    )?;
                }
            }
        }

        let mut campaign_builder = PatchBuilder::default();
        let mut descriptor_builder = PatchBuilder::default();
        build_player_patches(
            self,
            &desired,
            &mut campaign_builder,
            &mut descriptor_builder,
        )?;
        build_relation_patches(self, &desired, &mut campaign_builder)?;
        build_officer_patches(self, &desired, &invalidated_officers, &mut campaign_builder)?;
        build_inventory_patches(self, &desired, &mut campaign_builder)?;
        build_colony_storage_patches(self, &desired, &additions, &mut campaign_builder)?;
        build_colony_resource_patches(self, &desired, &additions, &mut campaign_builder)?;
        let campaign_patches = campaign_builder.finish();
        let descriptor_patches = descriptor_builder.finish();
        if campaign_patches.is_empty() && descriptor_patches.is_empty() {
            return Err(CoreError::invalid_edit(
                "the requested edits make no changes",
            ));
        }
        let campaign_output = apply_patches(self.campaign.bytes(), &campaign_patches)?;
        let descriptor_output = apply_patches(self.descriptor.xml.bytes(), &descriptor_patches)?;
        validate_candidate(self, &desired, &campaign_output, &descriptor_output)?;

        let mut changes = semantic_changes(&self.state, &desired, &additions);
        mark_derived_changes(
            &mut changes,
            player_progression_requested,
            &officer_progression_requested,
        );
        let summary = ReviewSummary {
            review_id: Uuid::new_v4().to_string(),
            save_id: self.snapshot.save_id.clone(),
            source_revision: self.snapshot.revision.clone(),
            changes,
            warnings: review_warnings(&self.state, &desired),
        };
        Ok(PreparedReview {
            summary,
            location: self.location.clone(),
            campaign_source: self.campaign.bytes().to_vec(),
            descriptor_source: self.descriptor.xml.bytes().to_vec(),
            campaign_patches,
            descriptor_patches,
            campaign_output,
            descriptor_output,
            protected_write_authorized: !self.snapshot.capabilities.protected_save
                || self.options.allow_protected,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum CargoEditTarget {
    Inventory(String),
    Storage(String, String),
    ColonyResources(String, String),
    NewStorage(String, CargoItemKey),
    NewColonyResource(String, String),
}

fn reject_duplicate_cargo_edit_targets(edits: &[Edit]) -> Result<()> {
    let mut targets = HashSet::new();
    for edit in edits {
        let target = match edit {
            Edit::SetInventoryStackQuantity { stack_id, .. } => {
                Some(CargoEditTarget::Inventory(stack_id.clone()))
            }
            Edit::SetStorageStackQuantity {
                colony_id,
                stack_id,
                ..
            } => Some(CargoEditTarget::Storage(
                colony_id.clone(),
                stack_id.clone(),
            )),
            Edit::SetColonyResourceQuantity {
                colony_id,
                stack_id,
                ..
            } => Some(CargoEditTarget::ColonyResources(
                colony_id.clone(),
                stack_id.clone(),
            )),
            Edit::AddStorageStack {
                colony_id, item, ..
            } => Some(CargoEditTarget::NewStorage(colony_id.clone(), item.clone())),
            Edit::AddColonyResourceStack {
                colony_id,
                commodity_id,
                ..
            } => Some(CargoEditTarget::NewColonyResource(
                colony_id.clone(),
                commodity_id.clone(),
            )),
            _ => None,
        };
        if target.is_some_and(|target| !targets.insert(target)) {
            return Err(CoreError::invalid_edit(
                "multiple staged cargo edits target the same saved stack or catalog item",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum SkillAuthorization<'a> {
    ExistingOnly,
    Trusted(&'a HashSet<String>),
}

impl SkillAuthorization<'_> {
    fn require(self, skill_id: &str, existing: &IndexMap<String, SkillRank>) -> Result<()> {
        let authorized = match self {
            Self::ExistingOnly => existing.contains_key(skill_id),
            Self::Trusted(ids) => ids.contains(skill_id),
        };
        if !authorized {
            return Err(CoreError::invalid_edit(format!(
                "skill '{skill_id}' is not authorized by the validated local catalog"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum StackAuthorization<'a> {
    Denied,
    Trusted(&'a HashSet<String>),
}

impl StackAuthorization<'_> {
    fn require(self, stack_id: &str) -> Result<()> {
        if matches!(self, Self::Trusted(ids) if ids.contains(stack_id)) {
            return Ok(());
        }
        Err(CoreError::invalid_edit(format!(
            "cargo stack '{stack_id}' is not authorized by the validated local catalog"
        )))
    }
}

#[derive(Debug, Clone, Copy)]
enum AdditionAuthorization<'a> {
    Denied,
    Trusted(&'a HashMap<CargoItemKey, CargoAdditionSpec>),
}

impl<'a> AdditionAuthorization<'a> {
    fn require(self, item: &CargoItemKey) -> Result<&'a CargoAdditionSpec> {
        let spec = match self {
            Self::Trusted(items) => items.get(item),
            Self::Denied => None,
        }
        .ok_or_else(|| {
            CoreError::invalid_edit(format!(
                "cargo item '{}' is not authorized for creation by the validated local catalog",
                item.item_id
            ))
        })?;
        if spec.key != *item {
            return Err(CoreError::invalid_edit(
                "cargo addition catalog key does not match its authorized definition",
            ));
        }
        if !spec.cargo_space_per_unit.is_finite() || spec.cargo_space_per_unit.is_sign_negative() {
            return Err(CoreError::invalid_edit(
                "cargo addition catalog has an invalid cargo-space value",
            ));
        }
        Ok(spec)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CargoAdditionTarget {
    Storage,
    LocalResources,
}

#[derive(Debug, Clone)]
struct PendingCargoAddition {
    colony_id: String,
    target: CargoAdditionTarget,
    stack_id: String,
    stack_identity: String,
    data_identity: Option<String>,
    item: CargoItemKey,
    quantity: f32,
    cargo_space_per_unit: f32,
}

const RC8_UNLIMITED_STACK_MAX: f32 = 1_000_000.0;
const MAX_CARGO_ADDITIONS_PER_REVIEW: usize = 4_096;

#[allow(clippy::too_many_arguments)]
fn stage_colony_cargo_addition(
    opened: &OpenedSave,
    desired: &mut SemanticState,
    authorization: AdditionAuthorization<'_>,
    additions: &mut Vec<PendingCargoAddition>,
    next_identity: &mut Option<u64>,
    colony_id: &str,
    target: CargoAdditionTarget,
    item: &CargoItemKey,
    quantity: f32,
) -> Result<()> {
    if additions.len() >= MAX_CARGO_ADDITIONS_PER_REVIEW {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            "cargo addition count exceeds the per-review safety limit",
        ));
    }
    validate_cargo_item_key(item)?;
    let spec = authorization.require(item)?;
    if target == CargoAdditionTarget::LocalResources
        && (item.kind != InventoryKind::Resources || !spec.local_resources_eligible)
    {
        return Err(CoreError::invalid_edit(
            "Local Resources accepts only catalog-authorized economic commodity resources",
        ));
    }
    validate_new_stack_quantity(item.kind, quantity)?;

    let anchors = colony_cargo_anchors(opened, colony_id, target)?.clone();
    let original = colony_cargo(&opened.state, colony_id, target)?;
    if !original.recompute_safe || !original.scope_editable {
        return Err(CoreError::invalid_edit(
            "the selected cargo container is not structurally editable",
        ));
    }
    if !anchors.unlimited_stacks {
        return Err(CoreError::invalid_edit(
            "new cargo stacks require the exact RC8 unlimited-stack container shape",
        ));
    }
    if opened.campaign.name(anchors.stack_container) != "s"
        || opened.campaign.parent(anchors.stack_container) != Some(anchors.cargo)
    {
        return Err(CoreError::ambiguous(
            "cargo stack list is aliased or is not owned by the selected cargo container",
        ));
    }
    let stack_list_identity = opened
        .campaign
        .require_attribute(anchors.stack_container, "z")?;
    if opened.campaign.reference_count(stack_list_identity) != 0 {
        return Err(CoreError::ambiguous(
            "cargo stack list is shared by reference and cannot accept new members",
        ));
    }
    opened.campaign.require_attribute(anchors.cargo, "z")?;
    if !opened
        .campaign
        .direct_children_named(anchors.cargo, "partials")
        .is_empty()
    {
        return Err(CoreError::ambiguous(
            "cargo has partial-quantity accounting; stack creation is unavailable",
        ));
    }
    if original
        .stacks
        .values()
        .any(|stack| cargo_stack_matches(stack, item))
        || additions.iter().any(|addition| {
            addition.colony_id == colony_id && addition.target == target && addition.item == *item
        })
    {
        return Err(CoreError::invalid_edit(
            "an exact cargo stack already exists; edit its quantity instead of creating a duplicate",
        ));
    }

    let addition_ordinal = additions
        .iter()
        .filter(|addition| addition.colony_id == colony_id && addition.target == target)
        .count();
    let ordinal = anchors
        .slot_count
        .checked_add(addition_ordinal)
        .ok_or_else(|| CoreError::new(ErrorCode::ResourceLimit, "cargo slot index overflow"))?;
    let stack_identity = allocate_fresh_identity(&opened.campaign, next_identity)?;
    let data_identity = (item.kind == InventoryKind::Special)
        .then(|| allocate_fresh_identity(&opened.campaign, next_identity))
        .transpose()?;
    let stack_id = cargo_stack_selector(
        &anchors.scope_key,
        &anchors.stack_prefix,
        ordinal,
        &stack_identity,
        raw_inventory_kind(item.kind)?,
        &item.item_id,
    );

    let cargo = colony_cargo_mut(desired, colony_id, target)?;
    if cargo.stacks.contains_key(&stack_id) {
        return Err(CoreError::ambiguous(
            "new cargo stack selector collides with an existing selector",
        ));
    }
    cargo.stacks.insert(
        stack_id.clone(),
        CargoStackState {
            kind: item.kind,
            item_id: item.item_id.clone(),
            special_data: item.special_data.clone(),
            quantity,
            max_quantity: RC8_UNLIMITED_STACK_MAX,
            cargo_space_per_unit: spec.cargo_space_per_unit,
            structurally_editable: true,
            reason: None,
        },
    );
    cargo.slot_order.push(stack_id.clone());
    cargo.used_space = cargo.recompute_used_space()?;
    additions.push(PendingCargoAddition {
        colony_id: colony_id.to_owned(),
        target,
        stack_id,
        stack_identity,
        data_identity,
        item: item.clone(),
        quantity,
        cargo_space_per_unit: spec.cargo_space_per_unit,
    });
    Ok(())
}

fn colony_cargo_anchors<'a>(
    opened: &'a OpenedSave,
    colony_id: &str,
    target: CargoAdditionTarget,
) -> Result<&'a CargoAnchors> {
    let colony = opened
        .anchors
        .colonies
        .get(colony_id)
        .ok_or_else(|| CoreError::invalid_edit(format!("unknown colony '{colony_id}'")))?;
    let anchors = match target {
        CargoAdditionTarget::Storage => colony.storage.as_ref(),
        CargoAdditionTarget::LocalResources => colony.local_resources.as_ref(),
    };
    anchors.ok_or_else(|| {
        CoreError::invalid_edit(format!(
            "colony '{colony_id}' has no safely anchored {}",
            cargo_target_name(target)
        ))
    })
}

fn colony_cargo<'a>(
    state: &'a SemanticState,
    colony_id: &str,
    target: CargoAdditionTarget,
) -> Result<&'a CargoState> {
    let colony = state
        .colonies
        .get(colony_id)
        .ok_or_else(|| CoreError::invalid_edit(format!("unknown colony '{colony_id}'")))?;
    let cargo = match target {
        CargoAdditionTarget::Storage => colony.storage.as_ref(),
        CargoAdditionTarget::LocalResources => colony.local_resources.as_ref(),
    };
    cargo.ok_or_else(|| {
        CoreError::invalid_edit(format!(
            "colony '{colony_id}' has no editable {}",
            cargo_target_name(target)
        ))
    })
}

fn colony_cargo_mut<'a>(
    state: &'a mut SemanticState,
    colony_id: &str,
    target: CargoAdditionTarget,
) -> Result<&'a mut CargoState> {
    let colony = state
        .colonies
        .get_mut(colony_id)
        .ok_or_else(|| CoreError::invalid_edit(format!("unknown colony '{colony_id}'")))?;
    let cargo = match target {
        CargoAdditionTarget::Storage => colony.storage.as_mut(),
        CargoAdditionTarget::LocalResources => colony.local_resources.as_mut(),
    };
    cargo.ok_or_else(|| {
        CoreError::invalid_edit(format!(
            "colony '{colony_id}' has no editable {}",
            cargo_target_name(target)
        ))
    })
}

fn cargo_target_name(target: CargoAdditionTarget) -> &'static str {
    match target {
        CargoAdditionTarget::Storage => "storage",
        CargoAdditionTarget::LocalResources => "Local Resources",
    }
}

fn cargo_stack_matches(stack: &CargoStackState, item: &CargoItemKey) -> bool {
    stack.kind == item.kind
        && stack.item_id == item.item_id
        && stack.special_data == item.special_data
}

fn validate_cargo_item_key(item: &CargoItemKey) -> Result<()> {
    if item.kind == InventoryKind::Unknown {
        return Err(CoreError::invalid_edit(
            "unknown cargo kinds cannot be created",
        ));
    }
    validate_catalog_identifier(&item.item_id, "cargo item ID")?;
    if item.kind != InventoryKind::Special && item.special_data.is_some() {
        return Err(CoreError::invalid_edit(
            "only special cargo items may carry special data",
        ));
    }
    if item.kind == InventoryKind::Special
        && matches!(
            item.item_id.as_str(),
            "ship_bp" | "weapon_bp" | "fighter_bp"
        )
        && item.special_data.is_none()
    {
        return Err(CoreError::invalid_edit(
            "individual blueprint special items require a catalog target ID",
        ));
    }
    if let Some(data) = &item.special_data {
        validate_catalog_identifier(data, "special-item data ID")?;
    }
    Ok(())
}

fn validate_catalog_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(CoreError::invalid_edit(format!(
            "{label} is not a supported local catalog identifier"
        )));
    }
    Ok(())
}

fn validate_new_stack_quantity(kind: InventoryKind, quantity: f32) -> Result<()> {
    if !quantity.is_finite() || quantity < 1.0 {
        return Err(CoreError::invalid_edit(
            "new cargo stack quantities must be finite and at least one",
        ));
    }
    if quantity > RC8_UNLIMITED_STACK_MAX {
        return Err(CoreError::invalid_edit(format!(
            "new cargo stack quantity exceeds the RC8 unlimited-stack maximum {RC8_UNLIMITED_STACK_MAX}"
        )));
    }
    if kind.requires_whole_quantity() && quantity.fract() != 0.0 {
        return Err(CoreError::invalid_edit(
            "new weapon, fighter-wing, and special-item quantities must be whole numbers",
        ));
    }
    Ok(())
}

fn allocate_fresh_identity(xml: &XmlDocument, next: &mut Option<u64>) -> Result<String> {
    let value = match *next {
        Some(value) => value,
        None => xml.next_numeric_identity()?,
    };
    *next = Some(value.checked_add(1).ok_or_else(|| {
        CoreError::new(
            ErrorCode::ResourceLimit,
            "campaign z identity sequence is exhausted",
        )
    })?);
    Ok(value.to_string())
}

fn raw_inventory_kind(kind: InventoryKind) -> Result<&'static str> {
    match kind {
        InventoryKind::Resources => Ok("RESOURCES"),
        InventoryKind::Weapons => Ok("WEAPONS"),
        InventoryKind::FighterChip => Ok("FIGHTER_CHIP"),
        InventoryKind::Special => Ok("SPECIAL"),
        InventoryKind::Unknown => Err(CoreError::invalid_edit(
            "unknown cargo kinds cannot be serialized",
        )),
    }
}

fn validate_stack_quantity(stack: &crate::semantic::CargoStackState, value: f32) -> Result<()> {
    if !stack.structurally_editable {
        return Err(CoreError::invalid_edit(
            stack
                .reason
                .clone()
                .unwrap_or_else(|| "cargo stack is structurally read-only".into()),
        ));
    }
    if !value.is_finite() || value <= 0.0 {
        return Err(CoreError::invalid_edit(
            "cargo quantities must be finite and greater than zero",
        ));
    }
    if value > stack.max_quantity {
        return Err(CoreError::invalid_edit(format!(
            "cargo quantity exceeds the serialized maximum {}",
            stack.max_quantity
        )));
    }
    if stack.kind.requires_whole_quantity() && value.fract() != 0.0 {
        return Err(CoreError::invalid_edit(
            "weapon, fighter-wing, and special-item quantities must be whole numbers",
        ));
    }
    Ok(())
}

fn build_player_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    campaign: &mut PatchBuilder,
    descriptor: &mut PatchBuilder,
) -> Result<()> {
    let old = &opened.state;
    let anchors = &opened.anchors;
    if old.player.first_name != desired.player.first_name {
        campaign.push(opened.campaign.attribute_patch(
            anchors.player_person.name,
            "f",
            &desired.player.first_name,
            "player first name",
        )?)?;
    }
    if old.player.last_name != desired.player.last_name {
        campaign.push(opened.campaign.attribute_patch(
            anchors.player_person.name,
            "l",
            &desired.player.last_name,
            "player last name",
        )?)?;
    }
    let desired_name = join_name(&desired.player.first_name, &desired.player.last_name);
    if old.character_summary_name != desired_name
        || old.player.first_name != desired.player.first_name
        || old.player.last_name != desired.player.last_name
    {
        campaign.push(opened.campaign.text_patch(
            anchors.character_name,
            &desired_name,
            "campaign character name mirror",
        )?)?;
        descriptor.push(opened.descriptor.name_patch(&desired_name)?)?;
    }
    if old.player.portrait != desired.player.portrait {
        campaign.push(opened.campaign.attribute_patch(
            anchors.player_person.person,
            "spr",
            &desired.player.portrait,
            "player portrait",
        )?)?;
        campaign.push(opened.campaign.text_patch(
            anchors.character_portrait,
            &desired.player.portrait,
            "campaign portrait mirror",
        )?)?;
        descriptor.push(opened.descriptor.portrait_patch(&desired.player.portrait)?)?;
    }
    if old.credits.to_bits() != desired.credits.to_bits() {
        campaign.push(opened.campaign.text_patch(
            anchors.credits_value,
            &desired.credits.to_string(),
            "credits",
        )?)?;
    }
    build_stats_patches(
        &opened.campaign,
        &anchors.player_person.stats,
        &old.player.stats,
        &desired.player.stats,
        "player",
        campaign,
    )?;
    if old.player.stats.level != desired.player.stats.level {
        descriptor.push(opened.descriptor.level_patch(desired.player.stats.level)?)?;
    }
    if old.player.skills != desired.player.skills {
        for patch in skill_patches(
            &opened.campaign,
            anchors.player_person.stats.skills,
            &old.player.skills,
            &desired.player.skills,
            "player skills",
        )? {
            campaign.push(patch)?;
        }
    }
    if old.skills_ever_elite != desired.skills_ever_elite {
        campaign.push(elite_history_patch(opened, desired)?)?;
    }
    Ok(())
}

fn build_inventory_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    campaign: &mut PatchBuilder,
) -> Result<()> {
    if cargo_values_equal(&opened.state.inventory, &desired.inventory) {
        return Ok(());
    }
    let anchors = opened
        .anchors
        .inventory
        .as_ref()
        .ok_or_else(|| CoreError::validation("player inventory anchors are unavailable"))?;
    build_cargo_patches(
        &opened.campaign,
        anchors,
        &opened.state.inventory,
        &desired.inventory,
        &[],
        "player inventory",
        campaign,
    )
}

fn build_colony_storage_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    additions: &[PendingCargoAddition],
    campaign: &mut PatchBuilder,
) -> Result<()> {
    for (colony_id, next_colony) in &desired.colonies {
        let old_colony = opened.state.colonies.get(colony_id).ok_or_else(|| {
            CoreError::validation(format!("missing original colony state for {colony_id}"))
        })?;
        let (Some(old), Some(next)) = (&old_colony.storage, &next_colony.storage) else {
            if old_colony.storage.is_some() != next_colony.storage.is_some() {
                return Err(CoreError::validation(
                    "colony storage presence may not be changed",
                ));
            }
            continue;
        };
        if cargo_values_equal(old, next) {
            continue;
        }
        let anchors = opened
            .anchors
            .colonies
            .get(colony_id)
            .and_then(|anchors| anchors.storage.as_ref())
            .ok_or_else(|| {
                CoreError::validation(format!("missing storage anchors for colony {colony_id}"))
            })?;
        let cargo_additions: Vec<_> = additions
            .iter()
            .filter(|addition| {
                addition.colony_id == *colony_id && addition.target == CargoAdditionTarget::Storage
            })
            .collect();
        build_cargo_patches(
            &opened.campaign,
            anchors,
            old,
            next,
            &cargo_additions,
            &format!("colony {colony_id} storage"),
            campaign,
        )?;
    }
    Ok(())
}

fn build_colony_resource_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    additions: &[PendingCargoAddition],
    campaign: &mut PatchBuilder,
) -> Result<()> {
    for (colony_id, next_colony) in &desired.colonies {
        let old_colony = opened.state.colonies.get(colony_id).ok_or_else(|| {
            CoreError::validation(format!("missing original colony state for {colony_id}"))
        })?;
        let (Some(old), Some(next)) = (&old_colony.local_resources, &next_colony.local_resources)
        else {
            if old_colony.local_resources.is_some() != next_colony.local_resources.is_some() {
                return Err(CoreError::validation(
                    "colony Local Resources presence may not be changed",
                ));
            }
            continue;
        };
        if cargo_values_equal(old, next) {
            continue;
        }
        let anchors = opened
            .anchors
            .colonies
            .get(colony_id)
            .and_then(|anchors| anchors.local_resources.as_ref())
            .ok_or_else(|| {
                CoreError::validation(format!(
                    "missing Local Resources anchors for colony {colony_id}"
                ))
            })?;
        let cargo_additions: Vec<_> = additions
            .iter()
            .filter(|addition| {
                addition.colony_id == *colony_id
                    && addition.target == CargoAdditionTarget::LocalResources
            })
            .collect();
        build_cargo_patches(
            &opened.campaign,
            anchors,
            old,
            next,
            &cargo_additions,
            &format!("colony {colony_id} Local Resources"),
            campaign,
        )?;
    }
    Ok(())
}

fn build_cargo_patches(
    xml: &XmlDocument,
    anchors: &crate::semantic::CargoAnchors,
    old: &crate::semantic::CargoState,
    next: &crate::semantic::CargoState,
    additions: &[&PendingCargoAddition],
    label: &str,
    patches: &mut PatchBuilder,
) -> Result<()> {
    let expected_len = old
        .stacks
        .len()
        .checked_add(additions.len())
        .ok_or_else(|| CoreError::new(ErrorCode::ResourceLimit, "cargo stack count overflow"))?;
    let mut expected_order = old.slot_order.clone();
    expected_order.extend(additions.iter().map(|addition| addition.stack_id.clone()));
    if expected_len != next.stacks.len() || expected_order != next.slot_order {
        return Err(CoreError::validation(
            "cargo graph membership or ordering differs from the authorized additions",
        ));
    }
    for (stack_id, stack) in &next.stacks {
        let Some(original) = old.stacks.get(stack_id) else {
            let addition = additions
                .iter()
                .find(|addition| addition.stack_id == *stack_id)
                .ok_or_else(|| {
                    CoreError::validation(format!(
                        "cargo stack {stack_id} was introduced without authorization"
                    ))
                })?;
            validate_pending_addition_state(addition, stack)?;
            continue;
        };
        if original.quantity.to_bits() == stack.quantity.to_bits() {
            continue;
        }
        let anchor = anchors.stacks.get(stack_id).copied().ok_or_else(|| {
            CoreError::validation(format!("missing cargo stack anchor for {stack_id}"))
        })?;
        patches.push(xml.attribute_patch(
            anchor,
            "s",
            &stack.quantity.to_string(),
            format!("{label} stack {stack_id}"),
        )?)?;
    }
    if !additions.is_empty() {
        let insertion = build_cargo_addition_insertion(xml, anchors, additions)?;
        patches.push(xml.prepend_to_closing_tag_patch(
            anchors.stack_container,
            insertion,
            format!("{label} add cargo stacks"),
        )?)?;
    }
    if old.used_space.to_bits() != next.used_space.to_bits() {
        patches.push(xml.attribute_patch(
            anchors.cargo,
            "sU",
            &next.used_space.to_string(),
            format!("{label} used space"),
        )?)?;
    }
    Ok(())
}

fn validate_pending_addition_state(
    addition: &PendingCargoAddition,
    stack: &CargoStackState,
) -> Result<()> {
    if !cargo_stack_matches(stack, &addition.item)
        || stack.quantity.to_bits() != addition.quantity.to_bits()
        || stack.max_quantity.to_bits() != RC8_UNLIMITED_STACK_MAX.to_bits()
        || stack.cargo_space_per_unit.to_bits() != addition.cargo_space_per_unit.to_bits()
    {
        return Err(CoreError::validation(
            "staged cargo addition no longer matches its authorized catalog definition",
        ));
    }
    Ok(())
}

fn build_cargo_addition_insertion(
    xml: &XmlDocument,
    anchors: &CargoAnchors,
    additions: &[&PendingCargoAddition],
) -> Result<Vec<u8>> {
    let cargo_identity = xml.require_attribute(anchors.cargo, "z")?;
    let newline = if xml.bytes().windows(2).any(|window| window == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let inner = xml.inner_range(anchors.stack_container)?;
    let source_inner = xml.raw_bytes(inner);
    let needs_leading_newline =
        !source_inner.is_empty() && !source_inner.ends_with(b"\n") || source_inner.is_empty();
    let mut insertion = String::new();
    if needs_leading_newline {
        insertion.push_str(newline);
    }
    for addition in additions {
        serialize_cargo_addition(&mut insertion, addition, cargo_identity, newline)?;
    }
    Ok(insertion.into_bytes())
}

fn serialize_cargo_addition(
    output: &mut String,
    addition: &PendingCargoAddition,
    cargo_identity: &str,
    newline: &str,
) -> Result<()> {
    let raw_kind = raw_inventory_kind(addition.item.kind)?;
    let round_size = if addition.item.kind.requires_whole_quantity() {
        "true"
    } else {
        "false"
    };
    output.push_str("<CIStack z=\"");
    output.push_str(&escape_xml_attribute(&addition.stack_identity));
    output.push_str("\" rS=\"");
    output.push_str(round_size);
    output.push_str("\" s=\"");
    output.push_str(&canonical_f32(addition.quantity));
    output.push_str("\" mS=\"1000000.0\" cSPU=\"");
    output.push_str(&canonical_f32(addition.cargo_space_per_unit));
    output.push_str("\" t=\"");
    output.push_str(raw_kind);
    output.push_str("\">");
    output.push_str(newline);
    if addition.item.kind == InventoryKind::Special {
        let data_identity = addition.data_identity.as_ref().ok_or_else(|| {
            CoreError::validation("special cargo addition has no data-object identity")
        })?;
        output.push_str("<d cl=\"SpID\" z=\"");
        output.push_str(&escape_xml_attribute(data_identity));
        output.push_str("\" i=\"");
        output.push_str(&escape_xml_attribute(&addition.item.item_id));
        output.push('"');
        if let Some(data) = &addition.item.special_data {
            output.push_str(" d=\"");
            output.push_str(&escape_xml_attribute(data));
            output.push('"');
        }
        output.push_str("></d>");
    } else {
        output.push_str("<d cl=\"st\">");
        output.push_str(&escape_xml_text(&addition.item.item_id));
        output.push_str("</d>");
    }
    output.push_str(newline);
    output.push_str("<c ref=\"");
    output.push_str(&escape_xml_attribute(cargo_identity));
    output.push_str("\"></c>");
    output.push_str(newline);
    output.push_str("</CIStack>");
    output.push_str(newline);
    Ok(())
}

fn canonical_f32(value: f32) -> String {
    let mut result = value.to_string();
    if !result.contains('.') && !result.contains('e') && !result.contains('E') {
        result.push_str(".0");
    }
    result
}

fn build_relation_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    campaign: &mut PatchBuilder,
) -> Result<()> {
    for (faction_id, next) in &desired.relations {
        let old = &opened.state.relations[faction_id];
        if old.value_percent.to_bits() == next.value_percent.to_bits() {
            continue;
        }
        let anchors = opened.anchors.relations.get(faction_id).ok_or_else(|| {
            CoreError::ambiguous(format!("missing relation anchor for {faction_id}"))
        })?;
        let game_value = next.value_percent / 100.0;
        campaign.push(opened.campaign.text_patch(
            anchors.value,
            &game_value.to_string(),
            format!("relation {faction_id}"),
        )?)?;
        if let Some(last_value) = anchors.history_last_value {
            campaign.push(opened.campaign.text_patch(
                last_value,
                &game_value.to_string(),
                format!("relation history {faction_id}"),
            )?)?;
            let timestamp_anchor = if next.value_percent > old.value_percent {
                anchors.history_positive_timestamp
            } else {
                anchors.history_negative_timestamp
            };
            if let Some(timestamp_anchor) = timestamp_anchor {
                campaign.push(opened.campaign.text_patch(
                    timestamp_anchor,
                    &opened.state.timestamp.to_string(),
                    format!("relation timestamp {faction_id}"),
                )?)?;
            }
        }
    }
    Ok(())
}

fn build_officer_patches(
    opened: &OpenedSave,
    desired: &SemanticState,
    invalidated: &HashSet<String>,
    campaign: &mut PatchBuilder,
) -> Result<()> {
    for (officer_id, next) in &desired.officers {
        let old = &opened.state.officers[officer_id];
        let anchors = opened.anchors.officers.get(officer_id).ok_or_else(|| {
            CoreError::ambiguous(format!("missing officer anchor for {officer_id}"))
        })?;
        build_stats_patches(
            &opened.campaign,
            &anchors.person.stats,
            &old.person.stats,
            &next.person.stats,
            &format!("officer {officer_id}"),
            campaign,
        )?;
        if old.person.skills != next.person.skills {
            for patch in skill_patches(
                &opened.campaign,
                anchors.person.stats.skills,
                &old.person.skills,
                &next.person.skills,
                format!("officer skills {officer_id}"),
            )? {
                campaign.push(patch)?;
            }
        }
        if invalidated.contains(officer_id) {
            for child in opened.campaign.children(anchors.skill_picks) {
                if opened.campaign.name(*child) != "st"
                    || opened.campaign.attribute(*child, "z").is_some()
                    || opened.campaign.attribute(*child, "ref").is_some()
                {
                    return Err(CoreError::validation(format!(
                        "refusing to clear structured skill picks for {officer_id}"
                    )));
                }
                let range = opened.campaign.full_range(*child);
                campaign.push(SpanPatch::new(
                    range.clone(),
                    opened.campaign.raw_bytes(range).to_vec(),
                    Vec::new(),
                    format!("clear officer skill pick {officer_id}"),
                ))?;
            }
            if old.made_picks {
                campaign.push(opened.campaign.text_patch(
                    anchors.made_picks,
                    "false",
                    format!("reset officer picks {officer_id}"),
                )?)?;
            }
        }
    }
    Ok(())
}

fn build_stats_patches(
    xml: &XmlDocument,
    anchors: &StatsAnchors,
    old: &InternalStats,
    next: &InternalStats,
    label: &str,
    patches: &mut PatchBuilder,
) -> Result<()> {
    let fields = [
        (
            "x2",
            old.story_checkpoint_xp.to_string(),
            next.story_checkpoint_xp.to_string(),
        ),
        ("xp", old.xp.to_string(), next.xp.to_string()),
        ("bx", old.bonus_xp.to_string(), next.bonus_xp.to_string()),
        (
            "db",
            old.deferred_bonus_xp.to_string(),
            next.deferred_bonus_xp.to_string(),
        ),
        ("l", old.level.to_string(), next.level.to_string()),
        (
            "pt",
            old.skill_points.to_string(),
            next.skill_points.to_string(),
        ),
        (
            "sp",
            old.story_points.to_string(),
            next.story_points.to_string(),
        ),
    ];
    for (attribute, old_value, new_value) in fields {
        if old_value != new_value {
            patches.push(xml.attribute_patch(
                anchors.stats,
                attribute,
                &new_value,
                format!("{label} stats {attribute}"),
            )?)?;
        }
    }
    Ok(())
}

fn skill_patches(
    xml: &XmlDocument,
    element: usize,
    old: &IndexMap<String, SkillRank>,
    desired: &IndexMap<String, SkillRank>,
    label: impl Into<String>,
) -> Result<Vec<SpanPatch>> {
    let label = label.into();
    let inner = xml.inner_range(element)?;
    let source = xml.raw_bytes(inner.clone());
    let document = SkillJsonDocument::parse(source)?;
    if &document.to_rank_map() != old {
        return Err(CoreError::validation(format!(
            "{label} anchor no longer matches extracted semantics"
        )));
    }
    if old.keys().any(|skill_id| !desired.contains_key(skill_id)) {
        return Err(CoreError::validation(
            "skill members may not be removed from the saved JSON object",
        ));
    }

    let mut patches = Vec::new();
    let mut insertion = String::new();
    let mut needs_comma = !document.entries().is_empty();
    for (skill_id, rank) in desired {
        if let Some(entry) = document.entries().get(skill_id) {
            if entry.rank != *rank {
                let range =
                    (inner.start + entry.value_range.start)..(inner.start + entry.value_range.end);
                patches.push(SpanPatch::new(
                    range.clone(),
                    xml.raw_bytes(range).to_vec(),
                    rank.numeric().to_string().into_bytes(),
                    format!("{label}: {skill_id}"),
                ));
            }
        } else if *rank != SkillRank::Unlearned {
            if needs_comma {
                insertion.push(',');
            }
            insertion.push_str(&xml_safe_json_string(skill_id)?);
            insertion.push(':');
            insertion.push(char::from(b'0' + rank.numeric()));
            needs_comma = true;
        }
    }
    if !insertion.is_empty() {
        let offset = inner.start + document.insertion_offset();
        patches.push(SpanPatch::new(
            offset..offset,
            Vec::new(),
            insertion.into_bytes(),
            format!("{label}: insert authorized members"),
        ));
    }
    Ok(patches)
}

fn xml_safe_json_string(value: &str) -> Result<String> {
    let encoded = serde_json::to_string(value)?;
    Ok(encoded
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e"))
}

fn elite_history_patch(opened: &OpenedSave, desired: &SemanticState) -> Result<SpanPatch> {
    let element = opened.anchors.skills_ever_elite;
    let mut extracted = Vec::new();
    for child in opened.campaign.children(element) {
        if opened.campaign.name(*child) != "st"
            || opened.campaign.attribute(*child, "z").is_some()
            || opened.campaign.attribute(*child, "ref").is_some()
        {
            return Err(CoreError::validation(
                "refusing to append to structured elite-skill history",
            ));
        }
        extracted.push(opened.campaign.simple_text(*child)?);
    }
    if extracted != opened.state.skills_ever_elite {
        return Err(CoreError::validation(
            "elite-skill history anchor no longer matches extracted semantics",
        ));
    }
    let existing = &opened.state.skills_ever_elite;
    if desired.skills_ever_elite.len() <= existing.len()
        || !desired.skills_ever_elite.starts_with(existing)
    {
        return Err(CoreError::validation(
            "elite-skill history edits must be append-only",
        ));
    }
    let newline = if opened
        .campaign
        .bytes()
        .windows(2)
        .any(|window| window == b"\r\n")
    {
        "\r\n"
    } else {
        "\n"
    };
    let inner = opened.campaign.inner_range(element)?;
    let source = opened.campaign.raw_bytes(inner.clone());
    let trailing_whitespace = source
        .iter()
        .rev()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let offset = inner.end - trailing_whitespace;
    let mut insertion = String::new();
    for (index, skill_id) in desired.skills_ever_elite[existing.len()..]
        .iter()
        .enumerate()
    {
        if index > 0 || !existing.is_empty() {
            insertion.push_str(newline);
        }
        insertion.push_str("<st>");
        insertion.push_str(&escape_xml_text(skill_id));
        insertion.push_str("</st>");
    }
    Ok(SpanPatch::new(
        offset..offset,
        Vec::new(),
        insertion.into_bytes(),
        "append skills ever made elite",
    ))
}

fn validate_candidate(
    opened: &OpenedSave,
    desired: &SemanticState,
    campaign_output: &[u8],
    descriptor_output: &[u8],
) -> Result<()> {
    let campaign = XmlDocument::parse(campaign_output.to_vec(), opened.options.campaign_limits)?;
    let descriptor =
        parse_descriptor(descriptor_output.to_vec(), opened.options.descriptor_limits)?;
    let (_, reparsed) = extract_campaign(&campaign, &opened.snapshot.save_id)?;
    if !semantic_values_equal(desired, &reparsed) {
        return Err(CoreError::validation(
            "candidate save did not reproduce the requested semantic values",
        ));
    }
    let expected_name = join_name(&desired.player.first_name, &desired.player.last_name);
    if descriptor.metadata.character_name != expected_name
        || descriptor.metadata.portrait != desired.player.portrait
        || descriptor.metadata.character_level != desired.player.stats.level
    {
        return Err(CoreError::validation(
            "descriptor mirrors failed candidate validation",
        ));
    }
    Ok(())
}

fn semantic_values_equal(left: &SemanticState, right: &SemanticState) -> bool {
    left.player.first_name == right.player.first_name
        && left.player.last_name == right.player.last_name
        && left.player.portrait == right.player.portrait
        && left.player.stats == right.player.stats
        && left.player.skills == right.player.skills
        && left.character_summary_name == right.character_summary_name
        && left.character_summary_portrait == right.character_summary_portrait
        && left.credits.to_bits() == right.credits.to_bits()
        && left.inventory_available == right.inventory_available
        && cargo_values_equal(&left.inventory, &right.inventory)
        && left.skills_ever_elite == right.skills_ever_elite
        && relation_values_equal(&left.relations, &right.relations)
        && officer_values_equal(&left.officers, &right.officers)
        && left.colony_storage_available == right.colony_storage_available
        && left.colony_resources_available == right.colony_resources_available
        && colony_values_equal(&left.colonies, &right.colonies)
}

fn cargo_values_equal(
    left: &crate::semantic::CargoState,
    right: &crate::semantic::CargoState,
) -> bool {
    left.used_space.to_bits() == right.used_space.to_bits()
        && left.max_space.map(f32::to_bits) == right.max_space.map(f32::to_bits)
        && left.slot_order == right.slot_order
        && left.recompute_safe == right.recompute_safe
        && left.scope_editable == right.scope_editable
        && left.stacks.len() == right.stacks.len()
        && left.stacks.iter().all(|(id, stack)| {
            right.stacks.get(id).is_some_and(|other| {
                stack.kind == other.kind
                    && stack.item_id == other.item_id
                    && stack.special_data == other.special_data
                    && stack.quantity.to_bits() == other.quantity.to_bits()
                    && stack.max_quantity.to_bits() == other.max_quantity.to_bits()
                    && stack.cargo_space_per_unit.to_bits() == other.cargo_space_per_unit.to_bits()
                    && stack.structurally_editable == other.structurally_editable
            })
        })
}

fn colony_values_equal(
    left: &IndexMap<String, crate::semantic::ColonyState>,
    right: &IndexMap<String, crate::semantic::ColonyState>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, colony)| {
            right.get(id).is_some_and(|other| {
                colony.name == other.name
                    && colony.faction_id == other.faction_id
                    && colony.location_context == other.location_context
                    && match (&colony.storage, &other.storage) {
                        (Some(left), Some(right)) => cargo_values_equal(left, right),
                        (None, None) => true,
                        _ => false,
                    }
                    && match (&colony.local_resources, &other.local_resources) {
                        (Some(left), Some(right)) => cargo_values_equal(left, right),
                        (None, None) => true,
                        _ => false,
                    }
            })
        })
}

fn relation_values_equal(
    left: &IndexMap<String, crate::semantic::RelationState>,
    right: &IndexMap<String, crate::semantic::RelationState>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, value)| {
            right.get(id).is_some_and(|other| {
                value.value_percent.to_bits() == other.value_percent.to_bits()
                    && value.has_history == other.has_history
            })
        })
}

fn officer_values_equal(
    left: &IndexMap<String, crate::semantic::OfficerState>,
    right: &IndexMap<String, crate::semantic::OfficerState>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, value)| {
            right.get(id).is_some_and(|other| {
                value.person.first_name == other.person.first_name
                    && value.person.last_name == other.person.last_name
                    && value.person.portrait == other.person.portrait
                    && value.person.stats == other.person.stats
                    && value.person.skills == other.person.skills
                    && value.pending_skill_picks == other.pending_skill_picks
                    && value.made_picks == other.made_picks
            })
        })
}

fn semantic_changes(
    old: &SemanticState,
    next: &SemanticState,
    additions: &[PendingCargoAddition],
) -> Vec<ReviewChange> {
    let mut changes = Vec::new();
    push_change(
        &mut changes,
        "character.first_name",
        &old.player.first_name,
        &next.player.first_name,
    );
    push_change(
        &mut changes,
        "character.last_name",
        &old.player.last_name,
        &next.player.last_name,
    );
    push_change(
        &mut changes,
        "character.portrait",
        &old.player.portrait,
        &next.player.portrait,
    );
    push_change(&mut changes, "character.credits", old.credits, next.credits);
    cargo_changes(&mut changes, "inventory", &old.inventory, &next.inventory);
    stats_changes(
        &mut changes,
        "character",
        &old.player.stats,
        &next.player.stats,
    );
    for (id, rank) in &next.player.skills {
        if old.player.skills.get(id) != Some(rank) {
            push_change(
                &mut changes,
                &format!("character.skills.{id}"),
                format!(
                    "{:?}",
                    old.player
                        .skills
                        .get(id)
                        .copied()
                        .unwrap_or(SkillRank::Unlearned)
                ),
                format!("{rank:?}"),
            );
        }
    }
    for (id, relation) in &next.relations {
        let old_value = old.relations[id].value_percent;
        push_change(
            &mut changes,
            &format!("reputation.{id}"),
            old_value,
            relation.value_percent,
        );
    }
    for (id, officer) in &next.officers {
        let old_officer = &old.officers[id];
        stats_changes(
            &mut changes,
            &format!("officers.{id}"),
            &old_officer.person.stats,
            &officer.person.stats,
        );
        for (skill_id, rank) in &officer.person.skills {
            if old_officer.person.skills.get(skill_id) != Some(rank) {
                push_change(
                    &mut changes,
                    &format!("officers.{id}.skills.{skill_id}"),
                    format!(
                        "{:?}",
                        old_officer
                            .person
                            .skills
                            .get(skill_id)
                            .copied()
                            .unwrap_or(SkillRank::Unlearned)
                    ),
                    format!("{rank:?}"),
                );
            }
        }
    }
    for (colony_id, colony) in &next.colonies {
        if let (Some(old_storage), Some(next_storage)) = (
            old.colonies
                .get(colony_id)
                .and_then(|colony| colony.storage.as_ref()),
            colony.storage.as_ref(),
        ) {
            cargo_changes(
                &mut changes,
                &format!("colonies.{colony_id}.storage"),
                old_storage,
                next_storage,
            );
        }
        if let (Some(old_resources), Some(next_resources)) = (
            old.colonies
                .get(colony_id)
                .and_then(|colony| colony.local_resources.as_ref()),
            colony.local_resources.as_ref(),
        ) {
            cargo_changes(
                &mut changes,
                &format!("colonies.{colony_id}.local_resources"),
                old_resources,
                next_resources,
            );
        }
    }
    for addition in additions {
        let destination = match addition.target {
            CargoAdditionTarget::Storage => "storage",
            CargoAdditionTarget::LocalResources => "local_resources",
        };
        let mut field = format!(
            "colonies.{}.{}.add.{}.{}",
            addition.colony_id,
            destination,
            inventory_kind_token(addition.item.kind),
            addition.item.item_id
        );
        if let Some(data) = &addition.item.special_data {
            field.push('.');
            field.push_str(data);
        }
        push_change(
            &mut changes,
            &field,
            "Not present",
            canonical_f32(addition.quantity),
        );
    }
    changes.retain(|change| change.old_value != change.new_value);
    for change in &mut changes {
        if change.field.ends_with(".used_space") {
            change.derived = true;
        }
    }
    changes
}

fn inventory_kind_token(kind: InventoryKind) -> &'static str {
    match kind {
        InventoryKind::Resources => "resources",
        InventoryKind::Weapons => "weapons",
        InventoryKind::FighterChip => "fighter_wing",
        InventoryKind::Special => "special",
        InventoryKind::Unknown => "unknown",
    }
}

fn cargo_changes(
    changes: &mut Vec<ReviewChange>,
    prefix: &str,
    old: &crate::semantic::CargoState,
    next: &crate::semantic::CargoState,
) {
    for (stack_id, stack) in &next.stacks {
        let Some(original) = old.stacks.get(stack_id) else {
            continue;
        };
        if original.quantity.to_bits() != stack.quantity.to_bits() {
            push_change(
                changes,
                &format!("{prefix}.{stack_id}.quantity"),
                original.quantity,
                stack.quantity,
            );
        }
    }
    push_change(
        changes,
        &format!("{prefix}.used_space"),
        old.used_space,
        next.used_space,
    );
}

fn stats_changes(
    changes: &mut Vec<ReviewChange>,
    prefix: &str,
    old: &InternalStats,
    next: &InternalStats,
) {
    push_change(changes, &format!("{prefix}.xp"), old.xp, next.xp);
    push_change(changes, &format!("{prefix}.level"), old.level, next.level);
    push_change(
        changes,
        &format!("{prefix}.story_checkpoint_xp"),
        old.story_checkpoint_xp,
        next.story_checkpoint_xp,
    );
    push_change(
        changes,
        &format!("{prefix}.bonus_xp"),
        old.bonus_xp,
        next.bonus_xp,
    );
    push_change(
        changes,
        &format!("{prefix}.deferred_bonus_xp"),
        old.deferred_bonus_xp,
        next.deferred_bonus_xp,
    );
    push_change(
        changes,
        &format!("{prefix}.skill_points"),
        old.skill_points,
        next.skill_points,
    );
    push_change(
        changes,
        &format!("{prefix}.story_points"),
        old.story_points,
        next.story_points,
    );
}

fn push_change(
    changes: &mut Vec<ReviewChange>,
    field: &str,
    old_value: impl ToString,
    new_value: impl ToString,
) {
    let old_value = old_value.to_string();
    let new_value = new_value.to_string();
    if old_value != new_value {
        changes.push(ReviewChange {
            field: field.to_owned(),
            old_value,
            new_value,
            derived: false,
        });
    }
}

fn mark_derived_changes(
    changes: &mut [ReviewChange],
    player_progression: bool,
    officer_progression: &HashSet<String>,
) {
    for change in changes {
        if player_progression
            && change.field.starts_with("character.")
            && change.field != "character.xp"
        {
            change.derived = true;
        }
        for officer_id in officer_progression {
            if change.field.starts_with(&format!("officers.{officer_id}."))
                && !change.field.ends_with(".xp")
            {
                change.derived = true;
            }
        }
    }
}

fn review_warnings(old: &SemanticState, state: &SemanticState) -> Vec<Warning> {
    let mut warnings = Vec::new();
    if !cargo_values_equal(&old.inventory, &state.inventory)
        && state
            .inventory
            .max_space
            .is_some_and(|capacity| state.inventory.used_space > capacity)
    {
        warnings.push(Warning {
            code: "PLAYER_CARGO_OVER_CAPACITY".to_owned(),
            message: "The staged inventory exceeds the player fleet cargo capacity".to_owned(),
            acknowledgement_required: true,
        });
    }
    for (colony_id, colony) in &state.colonies {
        let Some(storage) = &colony.storage else {
            continue;
        };
        let Some(original) = old
            .colonies
            .get(colony_id)
            .and_then(|colony| colony.storage.as_ref())
        else {
            continue;
        };
        if !cargo_values_equal(original, storage)
            && storage
                .max_space
                .is_some_and(|capacity| storage.used_space > capacity)
        {
            warnings.push(Warning {
                code: "COLONY_STORAGE_OVER_CAPACITY".to_owned(),
                message: format!(
                    "The staged storage for {} exceeds its serialized capacity",
                    colony.name
                ),
                acknowledgement_required: true,
            });
        }
    }
    let learned = state
        .player
        .skills
        .values()
        .filter(|rank| **rank != SkillRank::Unlearned)
        .count() as u32;
    if learned
        > state
            .player
            .stats
            .level
            .saturating_add(state.player.stats.skill_points)
    {
        warnings.push(Warning {
            code: "PLAYER_SKILL_TOTAL_INCONSISTENT".to_owned(),
            message: "Learned player skills exceed level plus unspent points".to_owned(),
            acknowledgement_required: true,
        });
    }
    for (id, officer) in &state.officers {
        let learned = officer
            .person
            .skills
            .values()
            .filter(|rank| **rank != SkillRank::Unlearned)
            .count() as u32;
        if learned
            > officer
                .person
                .stats
                .level
                .saturating_add(officer.person.stats.skill_points)
        {
            warnings.push(Warning {
                code: "OFFICER_SKILL_TOTAL_INCONSISTENT".to_owned(),
                message: format!("Officer {id} has an unusual level/skill total"),
                acknowledgement_required: true,
            });
        }
    }
    warnings
}

fn validate_name(value: &str, field: &str, allow_empty: bool) -> Result<()> {
    let value = value.trim();
    if (!allow_empty && value.is_empty()) || value.chars().count() > 100 {
        return Err(CoreError::invalid_edit(format!("invalid {field}")));
    }
    if value.chars().any(char::is_control) {
        return Err(CoreError::invalid_edit(format!(
            "{field} contains control characters"
        )));
    }
    Ok(())
}

fn validate_portrait_id(value: &str) -> Result<()> {
    if value.len() > 512 || value.contains('\\') || value.contains(':') {
        return Err(CoreError::invalid_edit("invalid portrait path"));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || !value.starts_with("graphics/")
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CoreError::invalid_edit(
            "portrait must be a validated relative graphics path",
        ));
    }
    Ok(())
}

fn validate_point_total(value: u32) -> Result<()> {
    if value > 1_000_000 {
        return Err(CoreError::invalid_edit("point total exceeds safety limit"));
    }
    Ok(())
}

fn require_capability(enabled: bool, compatibility: &Compatibility, feature: &str) -> Result<()> {
    if enabled {
        return Ok(());
    }
    match compatibility {
        Compatibility::ReadOnly { code, reason } | Compatibility::Invalid { code, reason } => {
            Err(CoreError::new(*code, reason.clone()))
        }
        Compatibility::Editable => Err(CoreError::invalid_edit(format!(
            "{feature} is disabled for this save"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::OpenOptions;
    use crate::xml::XmlLimits;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn duplicate_cargo_targets_are_rejected_without_conflating_distinct_scopes() {
        for edits in [
            vec![
                Edit::SetStorageStackQuantity {
                    colony_id: "colony-a".into(),
                    stack_id: "stack-a".into(),
                    value: 12.0,
                },
                Edit::SetStorageStackQuantity {
                    colony_id: "colony-a".into(),
                    stack_id: "stack-a".into(),
                    value: 15.0,
                },
            ],
            vec![
                Edit::SetStorageStackQuantity {
                    colony_id: "colony-a".into(),
                    stack_id: "stack-a".into(),
                    value: 15.0,
                },
                Edit::SetStorageStackQuantity {
                    colony_id: "colony-a".into(),
                    stack_id: "stack-a".into(),
                    value: 12.0,
                },
            ],
        ] {
            let error = reject_duplicate_cargo_edit_targets(&edits).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidEdit);
            assert!(error.message.contains("same saved stack"));
        }

        let item = CargoItemKey {
            kind: InventoryKind::Weapons,
            item_id: "vulcan".into(),
            special_data: None,
        };
        assert!(reject_duplicate_cargo_edit_targets(&[
            Edit::AddStorageStack {
                colony_id: "colony-a".into(),
                item: item.clone(),
                quantity: 1.0,
            },
            Edit::AddStorageStack {
                colony_id: "colony-a".into(),
                item,
                quantity: 2.0,
            },
        ])
        .is_err());

        assert!(reject_duplicate_cargo_edit_targets(&[
            Edit::SetStorageStackQuantity {
                colony_id: "colony-a".into(),
                stack_id: "shared-selector".into(),
                value: 12.0,
            },
            Edit::SetStorageStackQuantity {
                colony_id: "colony-b".into(),
                stack_id: "shared-selector".into(),
                value: 15.0,
            },
            Edit::SetColonyResourceQuantity {
                colony_id: "colony-a".into(),
                stack_id: "shared-selector".into(),
                value: 18.0,
            },
        ])
        .is_ok());
    }

    fn campaign_fixture() -> String {
        r#"<?xml version="1.0" ?>
<CampaignEngine z="1">
<playerFleet ref="10"></playerFleet>
<characterData z="70">
<name>Ada Vale</name>
<portraitName>graphics/portraits/portrait_a.png</portraitName>
<person ref="20"></person>
<isIronMode>false</isIronMode>
<skillsEverMadeElite z="71"><!--preserve--><st>beta</st>{TRAILING_SPACE}
</skillsEverMadeElite>
</characterData>
<clock z="75"><timestamp>-1000</timestamp></clock>
<factionManager z="76">
<playerFaction ref="80"></playerFaction>
<relations z="90"><e><st>player_hegemony</st><FMRelation z="91"><factionIdOne>player</factionIdOne><factionIdTwo>hegemony</factionIdTwo><value>0.2</value></FMRelation></e></relations>
</factionManager>
<modAndPluginData z="100"><persistentData z="101"><e><st>core_CEFSSharedDataKey</st><SharedData z="102"><playerActivityTracker z="103"><repChangeTracker z="104"><repData z="105"><e><st>hegemony</st><ReputationChangeData z="106"><lastPositiveChange>-2000</lastPositiveChange><lastNegativeChange>-3000</lastNegativeChange><lastValue>0.2</lastValue></ReputationChangeData></e></repData></repChangeTracker></playerActivityTracker></SharedData></e></persistentData></modAndPluginData>
<saveDirName>save_Ada_1</saveDirName>
<Flt z="10">
<fD z="11">
<m z="12">
<FMmbr z="13"><c z="20" id="player-person" pid="steady" spr="graphics/portraits/portrait_a.png"><n z="21" f="Ada" l="Vale" g="FEMALE"></n><stats z="22" x2="0" xp="0" bx="0" db="0" l="1" pt="0" sp="0"><s>{"alpha":0,"beta":1}</s></stats></c></FMmbr>
<FMmbr z="14"><c z="50" id="officer-person" pid="aggressive" spr="graphics/portraits/officer.png"><n z="51" f="Juno" l="Reed" g="FEMALE"></n><stats z="52" x2="0" xp="0" bx="0" db="0" l="1" pt="0" sp="0"><s>{"alpha":1}</s></stats></c></FMmbr>
</m>
<cargo z="30"><c z="31"><value>1000.0</value></c></cargo>
<c ref="20"></c>
<o z="40"><OfficerData z="41"><person ref="50"></person><skillPicks z="42"><st>beta</st></skillPicks><madePicks>true</madePicks></OfficerData></o>
</fD>
</Flt>
<f z="80"><id>player</id></f>
</CampaignEngine>
"#
        .replace("{TRAILING_SPACE}", " ")
    }

    fn inventory_colony_fixture() -> String {
        campaign_fixture()
            .replace(
                r#"<cargo z="30"><c z="31"><value>1000.0</value></c></cargo>"#,
                r#"<cargo z="30" uS="true" mC="20.0" sU="21.0" mF="100.0" mP="100.0">
<s z="32">
<slot ref="33"></slot>
<CIStack z="34" rS="true" s="2.0" mS="50.0" cSPU="4.0" t="WEAPONS"><d cl="st">laser</d><c ref="30"></c></CIStack>
<CIStack z="35" rS="true" s="1.0" mS="50.0" cSPU="1.0" t="FIGHTER_CHIP"><d cl="st">talon_wing</d><c ref="30"></c></CIStack>
<CIStack z="36" rS="true" s="1.0" mS="50.0" cSPU="1.0" t="SPECIAL"><d cl="SpID" z="37" i="ship_bp" d="wolf"></d><c ref="30"></c></CIStack>
<CIStack z="38" rS="true" s="3.0" mS="50.0" cSPU="2.0" t="MOD_CARGO"><d cl="st">mod_item</d><c ref="30"></c></CIStack>
<CIStack z="39" rS="false" s="0.0" mS="100.0" cSPU="0.0" t="NULL"></CIStack>
</s>
<c z="31"><value>1000.0</value></c>
<partials z="43"><e><st>RESOURCESsupplies</st><fp>0.25</fp></e></partials>
<item cl="CIStack" z="33" rS="false" s="5.0" mS="50.0" cSPU="1.0" t="RESOURCES"><d cl="st">supplies</d><c ref="30"></c></item>
</cargo>"#,
            )
            .replace(
                "<saveDirName>save_Ada_1</saveDirName>",
                r#"<economy z="200"><econ z="201"><markets z="202"><Market ref="210"></Market></markets></econ></economy>
<Market z="210">
<id>player_colony</id><name>New Dawn</name><size>5</size><factionId>player</factionId><playerOwned>true</playerOwned>
<prevStability>8.0</prevStability><hazard z="211" m="1.5"></hazard><accessibilityMod z="212" fB="1.2" m="1.0" pM="0.0"></accessibilityMod>
<useStockpilesForShortages>true</useStockpilesForShortages><isFreePort>true</isFreePort>
<admin ref="220"></admin><primaryEntity ref="230"></primaryEntity>
<industries z="240"><Industry z="241" id="population"></Industry><Industry z="242" id="spaceport"></Industry></industries>
<conditions z="243"><MCon z="244" i="habitable"></MCon><MCon z="245" i="population_5"></MCon></conditions>
<submarkets z="250"><Submarket z="270" s="local_resources"><m ref="210"></m><p cl="LocalResourcesSubmarketPlugin" z="271"><m ref="210"></m><s ref="270"></s>
<c cl="CargoData" z="272" uS="true" mC="1000.0" sU="15.0" mF="100.0" mP="100.0"><s z="273">
<CIStack z="274" rS="false" s="10.0" mS="1000000.0" cSPU="1.0" t="RESOURCES"><d cl="st">metals</d><c ref="272"></c></CIStack>
<CIStack z="275" rS="false" s="5.0" mS="1000000.0" cSPU="1.0" t="RESOURCES"><d cl="st">supplies</d><c ref="272"></c></CIStack>
</s><c z="276"><value>0.0</value></c></c>
<taken cl="CargoData" z="277" uS="true" mC="1000.0" sU="0.0"><s z="278"></s><c z="279"><value>0.0</value></c></taken>
<left cl="CargoData" z="280" uS="true" mC="1000.0" sU="0.0"><s z="281"></s><c z="282"><value>0.0</value></c></left>
<stockpilingBonus z="283"></stockpilingBonus>
</p></Submarket><Submarket z="251" s="storage"><m ref="210"></m><p cl="StoragePlugin" z="252" paid="true"><m ref="210"></m><s ref="251"></s>
<c cl="CargoData" z="253" uS="true" mC="30.0" sU="20.0" mF="100.0" mP="100.0"><s z="254">
<CIStack z="255" rS="false" s="10.0" mS="100.0" cSPU="1.0" t="RESOURCES"><d cl="st">metals</d><c ref="253"></c></CIStack>
<CIStack z="256" rS="true" s="2.0" mS="100.0" cSPU="4.0" t="WEAPONS"><d cl="st">mortar</d><c ref="253"></c></CIStack>
<CIStack z="257" rS="true" s="1.0" mS="100.0" cSPU="1.0" t="FIGHTER_CHIP"><d cl="st">broadsword_wing</d><c ref="253"></c></CIStack>
<CIStack z="258" rS="true" s="1.0" mS="100.0" cSPU="1.0" t="SPECIAL"><d cl="SpID" z="259" i="modspec" d="heavyarmor"></d><c ref="253"></c></CIStack>
</s><c z="260"><value>0.0</value></c><mS z="261"><m z="262"><FMmbr z="263"></FMmbr><FMmbr z="264"></FMmbr></m></mS></c>
</p></Submarket></submarkets>
</Market>
<Person z="220"><n z="221" f="Iris" l="Sato"></n></Person>
<Plnt z="230"><cL ref="231"></cL></Plnt><Sstm z="231" dN="Dawn System"></Sstm>
<saveDirName>save_Ada_1</saveDirName>"#,
            )
    }

    fn open_campaign_fixture(campaign: String) -> OpenedSave {
        open_campaign_fixture_with_mods(campaign, "")
    }

    fn cargo_item(kind: InventoryKind, item_id: &str, special_data: Option<&str>) -> CargoItemKey {
        CargoItemKey {
            kind,
            item_id: item_id.to_owned(),
            special_data: special_data.map(str::to_owned),
        }
    }

    fn cargo_addition(
        key: CargoItemKey,
        cargo_space_per_unit: f32,
        local_resources_eligible: bool,
    ) -> (CargoItemKey, CargoAdditionSpec) {
        (
            key.clone(),
            CargoAdditionSpec {
                key,
                cargo_space_per_unit,
                local_resources_eligible,
            },
        )
    }

    fn open_campaign_fixture_with_mods(campaign: String, enabled_mods: &str) -> OpenedSave {
        open_campaign_fixture_result(campaign, enabled_mods).unwrap()
    }

    fn open_campaign_fixture_result(campaign: String, enabled_mods: &str) -> Result<OpenedSave> {
        let root = tempdir().unwrap().keep();
        fs::write(root.join("campaign.xml"), campaign).unwrap();
        fs::write(
            root.join("descriptor.xml"),
            descriptor_fixture(enabled_mods, false),
        )
        .unwrap();
        OpenedSave::open(SaveLocation::from_save_dir(root), OpenOptions::default())
    }

    fn two_colony_fixture() -> String {
        let mut fixture = inventory_colony_fixture();
        let market_start = fixture.find(r#"<Market z="210">"#).unwrap();
        let market_end =
            market_start + fixture[market_start..].find("</Market>").unwrap() + "</Market>".len();
        let mut second_market = fixture[market_start..market_end].to_owned();
        for id in [
            210_u32, 211, 212, 240, 241, 242, 243, 244, 245, 250, 251, 252, 253, 254, 255, 256,
            257, 258, 259, 260, 261, 262, 263, 264, 270, 271, 272, 273, 274, 275, 276, 277, 278,
            279, 280, 281, 282, 283,
        ] {
            second_market = second_market
                .replace(&format!(r#"z="{id}""#), &format!(r#"z="{}""#, id + 100))
                .replace(&format!(r#"ref="{id}""#), &format!(r#"ref="{}""#, id + 100));
        }
        second_market = second_market
            .replace("<id>player_colony</id>", "<id>player_colony_2</id>")
            .replace("<name>New Dawn</name>", "<name>Second Dawn</name>");
        fixture.insert_str(market_end, &format!("\n{second_market}"));
        fixture.replace(
            r#"<markets z="202"><Market ref="210"></Market></markets>"#,
            r#"<markets z="202"><Market ref="210"></Market><Market ref="310"></Market></markets>"#,
        )
    }

    fn descriptor_fixture(enabled_mods: &str, autosave: bool) -> String {
        format!(
            r#"<?xml version="1.0" ?>
<SaveGameData z="1"><portraitName>graphics/portraits/portrait_a.png</portraitName><characterName>Ada Vale</characterName><saveFileVersion>0.6</saveFileVersion><gameVersion>0.98a-RC8</gameVersion><characterLevel>1</characterLevel><compressed>false</compressed><isIronMode>false</isIronMode><difficulty>normal</difficulty><locDesc>Corvus</locDesc><saveDate>date</saveDate><slotCreationTimestamp>1</slotCreationTimestamp><enabledMods z="2">{enabled_mods}</enabledMods><autosave>{autosave}</autosave></SaveGameData>"#
        )
    }

    fn open_fixture(enabled_mods: &str, autosave: bool, unlock: bool) -> OpenedSave {
        let root = tempdir().unwrap().keep();
        fs::write(root.join("campaign.xml"), campaign_fixture()).unwrap();
        fs::write(
            root.join("descriptor.xml"),
            descriptor_fixture(enabled_mods, autosave),
        )
        .unwrap();
        OpenedSave::open(
            SaveLocation::from_save_dir(root),
            OpenOptions {
                allow_protected: unlock,
                ..OpenOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn campaign_without_officer_list_opens_with_empty_roster() {
        let campaign = campaign_fixture().replace(
            r#"<o z="40"><OfficerData z="41"><person ref="50"></person><skillPicks z="42"><st>beta</st></skillPicks><madePicks>true</madePicks></OfficerData></o>"#,
            "",
        );

        let opened = open_campaign_fixture(campaign);

        assert!(opened.snapshot().officers.is_empty());
        assert!(opened.snapshot().capabilities.officers);

        let review = opened
            .prepare_review(&[Edit::SetCredits { value: 2_000.0 }])
            .unwrap();
        let output = XmlDocument::parse(
            review.campaign_output.clone(),
            opened.options.campaign_limits,
        )
        .unwrap();
        let player_fleet = output
            .resolve(
                output
                    .unique_direct_child(output.root(), "playerFleet")
                    .unwrap(),
            )
            .unwrap();
        let fleet_data = output
            .resolve(output.unique_direct_child(player_fleet, "fD").unwrap())
            .unwrap();
        assert!(output.direct_children_named(fleet_data, "o").is_empty());

        let (_, reparsed) =
            extract_campaign(&output, &opened.snapshot().save_id).expect("review output reparses");
        assert_eq!(reparsed.credits, 2_000.0);
        assert!(reparsed.officers.is_empty());
    }

    #[test]
    fn absent_officer_list_does_not_authorize_fabricated_officer_edit() {
        let campaign = campaign_fixture().replace(
            r#"<o z="40"><OfficerData z="41"><person ref="50"></person><skillPicks z="42"><st>beta</st></skillPicks><madePicks>true</madePicks></OfficerData></o>"#,
            "",
        );
        let opened = open_campaign_fixture(campaign);

        let error = opened
            .prepare_review(&[Edit::SetOfficerPoints {
                officer_id: "officer-forged".to_owned(),
                skill_points: 1,
            }])
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidEdit);
        assert_eq!(error.message, "unknown officer 'officer-forged'");
    }

    #[test]
    fn duplicate_officer_lists_remain_ambiguous() {
        let campaign = campaign_fixture().replace(
            r#"<o z="40"><OfficerData z="41">"#,
            r#"<o z="43"></o><o z="40"><OfficerData z="41">"#,
        );

        let error = open_campaign_fixture_result(campaign, "").unwrap_err();

        assert_eq!(error.code, ErrorCode::AmbiguousStructure);
        assert!(error
            .message
            .contains("expected at most one o child below fD"));
    }

    #[test]
    fn malformed_populated_officer_list_remains_ambiguous() {
        let campaign =
            campaign_fixture().replace(r#"<skillPicks z="42"><st>beta</st></skillPicks>"#, "");

        let error = open_campaign_fixture_result(campaign, "").unwrap_err();

        assert_eq!(error.code, ErrorCode::AmbiguousStructure);
        assert!(error
            .message
            .contains("expected one skillPicks child below OfficerData"));
    }

    #[test]
    fn semantic_review_updates_only_anchored_fields_and_revalidates() {
        let opened = open_fixture("", false, false);
        let officer_id = opened.snapshot().officers[0].officer_id.clone();
        let trusted_skill_ids = HashSet::from(["alpha".to_owned(), "beta".to_owned()]);
        let review = opened
            .prepare_review_with_skill_catalog(
                &[
                    Edit::SetName {
                        first_name: "A&da".to_owned(),
                        last_name: "V<ale".to_owned(),
                    },
                    Edit::SetCredits { value: 1234.5 },
                    Edit::GrantPlayerXp {
                        amount: DecimalU64::new(50_000),
                    },
                    Edit::SetPlayerSkill {
                        skill_id: "alpha".to_owned(),
                        rank: SkillRank::Elite,
                    },
                    Edit::SetFactionRelation {
                        faction_id: "hegemony".to_owned(),
                        value_percent: -50.0,
                    },
                    Edit::GrantOfficerXp {
                        officer_id: officer_id.clone(),
                        amount: DecimalU64::new(48_000),
                    },
                    Edit::SetOfficerSkill {
                        officer_id,
                        skill_id: "beta".to_owned(),
                        rank: SkillRank::Learned,
                    },
                ],
                &trusted_skill_ids,
            )
            .unwrap();
        assert!(review.campaign_patch_count() >= 12);
        assert!(review.descriptor_patch_count() >= 2);
        let campaign = String::from_utf8(review.campaign_output.clone()).unwrap();
        assert!(campaign.contains("f=\"A&amp;da\" l=\"V&lt;ale\""));
        assert!(campaign.contains("<value>1234.5</value>"));
        assert!(campaign.contains("xp=\"50000\""));
        assert!(campaign.contains("l=\"2\" pt=\"1\" sp=\"4\""));
        assert!(campaign.contains("<value>-0.5</value>"));
        assert!(campaign.contains("<lastValue>-0.5</lastValue>"));
        assert!(campaign.contains("<lastNegativeChange>-1000</lastNegativeChange>"));
        assert!(campaign.contains("<skillPicks z=\"42\"></skillPicks>"));
        assert!(campaign.contains("<madePicks>false</madePicks>"));
        assert!(campaign.contains("{\"alpha\":1,\"beta\":1}"));
        assert!(campaign
            .contains("<!--preserve--><st>beta</st>\n<st>alpha</st> \n</skillsEverMadeElite>"));
        XmlDocument::parse(review.campaign_output.clone(), XmlLimits::default()).unwrap();
    }

    #[test]
    fn skill_insertion_requires_the_validated_catalog() {
        let opened = open_fixture("", false, false);
        let officer_id = opened.snapshot().officers[0].officer_id.clone();
        let player_edit = Edit::SetPlayerSkill {
            skill_id: "new_skill".to_owned(),
            rank: SkillRank::Learned,
        };
        assert_eq!(
            opened
                .prepare_review(std::slice::from_ref(&player_edit))
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
        assert_eq!(
            opened
                .prepare_review_with_skill_catalog(
                    std::slice::from_ref(&player_edit),
                    &HashSet::new(),
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let trusted = HashSet::from(["new_skill".to_owned()]);
        let review = opened
            .prepare_review_with_skill_catalog(&[player_edit], &trusted)
            .unwrap();
        let campaign = String::from_utf8(review.campaign_output).unwrap();
        assert!(campaign.contains("{\"alpha\":0,\"beta\":1,\"new_skill\":1}"));

        let officer_review = opened
            .prepare_review_with_skill_catalog(
                &[Edit::SetOfficerSkill {
                    officer_id,
                    skill_id: "new_skill".to_owned(),
                    rank: SkillRank::Elite,
                }],
                &trusted,
            )
            .unwrap();
        let campaign = String::from_utf8(officer_review.campaign_output).unwrap();
        assert!(campaign.contains("{\"alpha\":1,\"new_skill\":2}"));
    }

    #[test]
    fn skill_member_patches_preserve_existing_json_bytes_and_handle_empty_objects() {
        let xml = XmlDocument::parse(
            br#"<root><s>{ "alpha" : 0 , "beta": 1 }</s><empty>{}</empty></root>"#.to_vec(),
            XmlLimits::default(),
        )
        .unwrap();
        let skills = xml.unique_direct_child(xml.root(), "s").unwrap();
        let old = IndexMap::from([
            ("alpha".to_owned(), SkillRank::Unlearned),
            ("beta".to_owned(), SkillRank::Learned),
        ]);
        let desired = IndexMap::from([
            ("alpha".to_owned(), SkillRank::Elite),
            ("beta".to_owned(), SkillRank::Learned),
            ("gamma".to_owned(), SkillRank::Learned),
        ]);
        let output = apply_patches(
            xml.bytes(),
            &skill_patches(&xml, skills, &old, &desired, "skills").unwrap(),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            r#"<root><s>{ "alpha" : 2 , "beta": 1,"gamma":1 }</s><empty>{}</empty></root>"#
        );

        let empty = xml.unique_direct_child(xml.root(), "empty").unwrap();
        let desired = IndexMap::from([("gamma".to_owned(), SkillRank::Learned)]);
        let output = apply_patches(
            xml.bytes(),
            &skill_patches(&xml, empty, &IndexMap::new(), &desired, "empty").unwrap(),
        )
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("<empty>{\"gamma\":1}</empty>"));
    }

    #[test]
    fn modded_and_locked_saves_disable_progression_fail_closed() {
        let modded = open_fixture("<st>example_mod</st>", false, false);
        assert!(!modded.snapshot().capabilities.progression);
        assert!(modded
            .prepare_review(&[Edit::GrantPlayerXp {
                amount: DecimalU64::new(1)
            }])
            .is_err());
        assert!(modded
            .prepare_review(&[Edit::SetCredits { value: 2.0 }])
            .is_ok());

        let modded_with_cargo =
            open_campaign_fixture_with_mods(inventory_colony_fixture(), "<st>example_mod</st>");
        assert!(!modded_with_cargo.snapshot().capabilities.progression);
        assert!(modded_with_cargo.snapshot().capabilities.inventory);
        assert!(modded_with_cargo.snapshot().capabilities.colony_storage);
        assert!(modded_with_cargo.snapshot().capabilities.colony_resources);

        let locked = open_fixture("", true, false);
        assert!(!locked.snapshot().capabilities.basic_character);
        assert!(locked
            .prepare_review(&[Edit::SetCredits { value: 2.0 }])
            .is_err());
        let unlocked = open_fixture("", true, true);
        assert!(unlocked.snapshot().capabilities.basic_character);
    }

    #[test]
    fn inventory_and_colony_cargo_follow_authoritative_graphs() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let snapshot = opened.snapshot();
        assert!(snapshot.capabilities.inventory);
        assert!(snapshot.capabilities.colony_storage);
        assert!(snapshot.capabilities.colony_resources);
        assert_eq!(snapshot.inventory.stacks.len(), 5);
        assert_eq!(snapshot.inventory.used_space, 21.0);
        assert_eq!(snapshot.inventory.max_space, Some(20.0));
        assert!(snapshot.inventory.stacks.iter().any(|stack| {
            stack.kind == InventoryKind::Special
                && stack.item_id == "ship_bp"
                && stack.special_data.as_deref() == Some("wolf")
        }));
        let unknown = snapshot
            .inventory
            .stacks
            .iter()
            .find(|stack| stack.kind == InventoryKind::Unknown)
            .unwrap();
        assert_eq!(unknown.item_id, "mod_item");
        assert!(!unknown.structurally_editable);

        let colony = &snapshot.colonies[0];
        assert_eq!(colony.name, "New Dawn");
        assert_eq!(colony.location_context.as_deref(), Some("Dawn System"));
        assert_eq!(colony.storage.as_ref().unwrap().stacks.len(), 4);
        let resources = colony.local_resources.as_ref().unwrap();
        assert_eq!(resources.stacks.len(), 2);
        assert_eq!(resources.used_space, 15.0);
        assert_eq!(resources.max_space, None);
        assert!(resources.stacks.iter().all(|stack| {
            stack.kind == InventoryKind::Resources && stack.structurally_editable
        }));
    }

    #[test]
    fn display_only_colony_fields_are_not_required_for_cargo_editing() {
        let fixture = inventory_colony_fixture()
            .replace("<size>5</size>", "<size>not-a-number</size>")
            .replace(
                "<prevStability>8.0</prevStability>",
                "<prevStability>not-a-number</prevStability>",
            )
            .replace(
                "<hazard z=\"211\" m=\"1.5\">",
                "<hazard z=\"211\" m=\"invalid\">",
            )
            .replace(
                "fB=\"1.2\" m=\"1.0\" pM=\"0.0\"",
                "fB=\"invalid\" m=\"invalid\" pM=\"invalid\"",
            )
            .replace(
                "<useStockpilesForShortages>true</useStockpilesForShortages>",
                "<useStockpilesForShortages>invalid</useStockpilesForShortages>",
            )
            .replace(
                "<isFreePort>true</isFreePort>",
                "<isFreePort>invalid</isFreePort>",
            )
            .replace(
                "<Industry z=\"241\" id=\"population\">",
                "<Industry z=\"241\">",
            )
            .replace("<MCon z=\"244\" i=\"habitable\">", "<MCon z=\"244\">")
            .replace(
                "<mS z=\"261\"><m z=\"262\"><FMmbr z=\"263\"></FMmbr><FMmbr z=\"264\"></FMmbr></m></mS>",
                "<mS z=\"261\"><unknown z=\"262\"><entry z=\"263\"></entry><entry z=\"264\"></entry></unknown></mS>",
            );
        let opened = open_campaign_fixture(fixture);
        let colony = &opened.snapshot().colonies[0];
        assert_eq!(colony.name, "New Dawn");
        assert_eq!(colony.location_context.as_deref(), Some("Dawn System"));
        assert!(colony.storage.is_some());
        assert!(colony.local_resources.is_some());
        assert!(opened.snapshot().capabilities.colony_storage);
        assert!(opened.snapshot().capabilities.colony_resources);

        let stack = &colony.storage.as_ref().unwrap().stacks[0];
        let trusted = HashSet::from([stack.stack_id.clone()]);
        let review = opened
            .prepare_review_with_catalogs(
                &[Edit::SetStorageStackQuantity {
                    colony_id: colony.colony_id.clone(),
                    stack_id: stack.stack_id.clone(),
                    value: 11.0,
                }],
                &HashSet::new(),
                &trusted,
            )
            .unwrap();
        let output = String::from_utf8(review.campaign_output).unwrap();
        for preserved in [
            "<size>not-a-number</size>",
            "<prevStability>not-a-number</prevStability>",
            "<hazard z=\"211\" m=\"invalid\">",
            "fB=\"invalid\" m=\"invalid\" pM=\"invalid\"",
            "<useStockpilesForShortages>invalid</useStockpilesForShortages>",
            "<isFreePort>invalid</isFreePort>",
            "<Industry z=\"241\">",
            "<MCon z=\"244\">",
            "<mS z=\"261\"><unknown z=\"262\"><entry z=\"263\"></entry><entry z=\"264\"></entry></unknown></mS>",
        ] {
            assert!(output.contains(preserved));
        }
    }

    #[test]
    fn authorized_inventory_storage_and_resource_edits_patch_only_sizes_and_used_space() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let resource = opened
            .snapshot()
            .inventory
            .stacks
            .iter()
            .find(|stack| stack.item_id == "supplies")
            .unwrap();
        let colony = &opened.snapshot().colonies[0];
        let storage_special = colony
            .storage
            .as_ref()
            .unwrap()
            .stacks
            .iter()
            .find(|stack| stack.kind == InventoryKind::Special)
            .unwrap();
        let local_resource = colony
            .local_resources
            .as_ref()
            .unwrap()
            .stacks
            .iter()
            .find(|stack| stack.item_id == "metals")
            .unwrap();
        let trusted = HashSet::from([
            resource.stack_id.clone(),
            storage_special.stack_id.clone(),
            local_resource.stack_id.clone(),
        ]);
        let edits = [
            Edit::SetInventoryStackQuantity {
                stack_id: resource.stack_id.clone(),
                value: 6.0,
            },
            Edit::SetStorageStackQuantity {
                colony_id: colony.colony_id.clone(),
                stack_id: storage_special.stack_id.clone(),
                value: 2.0,
            },
            Edit::SetColonyResourceQuantity {
                colony_id: colony.colony_id.clone(),
                stack_id: local_resource.stack_id.clone(),
                value: 12.0,
            },
        ];
        assert_eq!(
            opened
                .prepare_review_with_skill_catalog(&edits, &HashSet::new())
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
        let review = opened
            .prepare_review_with_catalogs(&edits, &HashSet::new(), &trusted)
            .unwrap();
        assert_eq!(review.campaign_patch_count(), 6);
        assert!(review
            .summary()
            .warnings
            .iter()
            .any(|warning| warning.code == "PLAYER_CARGO_OVER_CAPACITY"));
        assert!(review
            .summary()
            .changes
            .iter()
            .filter(|change| change.field.ends_with(".used_space"))
            .all(|change| change.derived));
        let output = String::from_utf8(review.campaign_output.clone()).unwrap();
        assert!(output.contains("z=\"33\" rS=\"false\" s=\"6\""));
        assert!(output.contains("z=\"258\" rS=\"true\" s=\"2\""));
        assert!(output.contains("z=\"274\" rS=\"false\" s=\"12\""));
        assert!(output.contains("mC=\"20.0\" sU=\"22\""));
        assert!(output.contains("mC=\"30.0\" sU=\"21\""));
        assert!(output.contains("z=\"272\" uS=\"true\" mC=\"1000.0\" sU=\"17\""));
        assert!(output.contains("<taken cl=\"CargoData\" z=\"277\" uS=\"true\" mC=\"1000.0\" sU=\"0.0\"><s z=\"278\"></s>"));
        assert!(output.contains("<left cl=\"CargoData\" z=\"280\" uS=\"true\" mC=\"1000.0\" sU=\"0.0\"><s z=\"281\"></s>"));
        assert!(output.contains("<fp>0.25</fp>"));
        assert!(output.contains(
            "<mS z=\"261\"><m z=\"262\"><FMmbr z=\"263\"></FMmbr><FMmbr z=\"264\"></FMmbr></m></mS>"
        ));

        let storage_overflow = opened
            .prepare_review_with_catalogs(
                &[Edit::SetStorageStackQuantity {
                    colony_id: colony.colony_id.clone(),
                    stack_id: storage_special.stack_id.clone(),
                    value: 20.0,
                }],
                &HashSet::new(),
                &trusted,
            )
            .unwrap();
        assert!(storage_overflow
            .summary()
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_STORAGE_OVER_CAPACITY"));
    }

    #[test]
    fn local_resource_float_noise_is_preserved_until_explicitly_edited() {
        let original_crew_stack = r#"<CIStack z="275" rS="false" s="5.0" mS="1000000.0" cSPU="1.0" t="RESOURCES"><d cl="st">supplies</d><c ref="272"></c></CIStack>"#;
        let noisy_crew_stack = r#"<CIStack z="275" rS="false" s="721.32007" mS="1000000.0" cSPU="1.0" t="RESOURCES"><d cl="st">crew</d><c ref="272"></c></CIStack>"#;
        let fixture = inventory_colony_fixture()
            .replacen(
                r#"mC="1000.0" sU="15.0""#,
                r#"mC="1000.0" sU="731.32007""#,
                1,
            )
            .replacen(original_crew_stack, noisy_crew_stack, 1);
        let opened = open_campaign_fixture(fixture);
        let colony = &opened.snapshot().colonies[0];
        let colony_id = colony.colony_id.clone();
        let crew = colony
            .local_resources
            .as_ref()
            .unwrap()
            .stacks
            .iter()
            .find(|stack| stack.item_id == "crew")
            .unwrap();
        let crew_stack_id = crew.stack_id.clone();
        assert_eq!(crew.quantity.to_bits(), 0x4434_547c);

        let player_supplies = opened
            .snapshot()
            .inventory
            .stacks
            .iter()
            .find(|stack| stack.item_id == "supplies")
            .unwrap();
        let unrelated_review = opened
            .prepare_review_with_catalogs(
                &[Edit::SetInventoryStackQuantity {
                    stack_id: player_supplies.stack_id.clone(),
                    value: 6.0,
                }],
                &HashSet::new(),
                &HashSet::from([player_supplies.stack_id.clone()]),
            )
            .unwrap();
        assert_eq!(unrelated_review.campaign_patch_count(), 2);
        assert!(unrelated_review
            .campaign_patches
            .iter()
            .all(|patch| patch.label.starts_with("player inventory")));
        let unrelated_output = String::from_utf8(unrelated_review.campaign_output).unwrap();
        assert!(unrelated_output.contains(noisy_crew_stack));
        assert!(unrelated_output.contains(r#"mC="1000.0" sU="731.32007""#));

        let explicit_review = opened
            .prepare_review_with_catalogs(
                &[Edit::SetColonyResourceQuantity {
                    colony_id: colony_id.clone(),
                    stack_id: crew_stack_id.clone(),
                    value: 721.32,
                }],
                &HashSet::new(),
                &HashSet::from([crew_stack_id.clone()]),
            )
            .unwrap();
        assert_eq!(explicit_review.campaign_patch_count(), 2);
        assert_eq!(explicit_review.descriptor_patch_count(), 0);
        assert!(explicit_review
            .campaign_patches
            .iter()
            .all(|patch| patch.label.contains("Local Resources")));
        assert!(explicit_review
            .campaign_patches
            .iter()
            .any(|patch| { patch.expected == b"721.32007" && patch.replacement == b"721.32" }));
        assert!(explicit_review
            .campaign_patches
            .iter()
            .any(|patch| { patch.expected == b"731.32007" && patch.replacement == b"731.32" }));

        let output = XmlDocument::parse(
            explicit_review.campaign_output,
            opened.options.campaign_limits,
        )
        .unwrap();
        let (_, reparsed) =
            extract_campaign(&output, &opened.snapshot().save_id).expect("review output reparses");
        let resources = reparsed
            .colonies
            .get(&colony_id)
            .unwrap()
            .local_resources
            .as_ref()
            .unwrap();
        let reparsed_crew = resources.stacks.get(&crew_stack_id).unwrap();
        assert_eq!(reparsed_crew.quantity.to_bits(), 721.32_f32.to_bits());
        assert_eq!(
            resources.used_space.to_bits(),
            (10.0_f32 + 721.32_f32).to_bits()
        );
    }

    #[test]
    fn authorized_storage_additions_use_exact_rc8_stack_shapes_and_fresh_identities() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        let ore = cargo_item(InventoryKind::Resources, "ore", None);
        let gauss = cargo_item(InventoryKind::Weapons, "gauss", None);
        let xyphos = cargo_item(InventoryKind::FighterChip, "xyphos_wing", None);
        let ship_blueprint = cargo_item(InventoryKind::Special, "ship_bp", Some("onslaught"));
        let tech_package = cargo_item(InventoryKind::Special, "high_tech_package", None);
        let additions = HashMap::from([
            cargo_addition(ore.clone(), 1.0, true),
            cargo_addition(gauss.clone(), 8.0, false),
            cargo_addition(xyphos.clone(), 1.0, false),
            cargo_addition(ship_blueprint.clone(), 1.0, false),
            cargo_addition(tech_package.clone(), 1.0, false),
        ]);
        let edits = vec![
            Edit::AddStorageStack {
                colony_id: colony_id.clone(),
                item: ore,
                quantity: 3.5,
            },
            Edit::AddStorageStack {
                colony_id: colony_id.clone(),
                item: gauss,
                quantity: 2.0,
            },
            Edit::AddStorageStack {
                colony_id: colony_id.clone(),
                item: xyphos,
                quantity: 1.0,
            },
            Edit::AddStorageStack {
                colony_id: colony_id.clone(),
                item: ship_blueprint,
                quantity: 1.0,
            },
            Edit::AddStorageStack {
                colony_id: colony_id.clone(),
                item: tech_package,
                quantity: 1.0,
            },
        ];

        let review = opened
            .prepare_review_with_catalogs_and_additions(
                &edits,
                &HashSet::new(),
                &HashSet::new(),
                &additions,
            )
            .unwrap();
        assert_eq!(review.campaign_patch_count(), 2);
        let output = String::from_utf8(review.campaign_output.clone()).unwrap();
        assert!(output.contains(
            "<CIStack z=\"284\" rS=\"false\" s=\"3.5\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"RESOURCES\">\n<d cl=\"st\">ore</d>\n<c ref=\"253\"></c>\n</CIStack>"
        ));
        assert!(output.contains(
            "<CIStack z=\"285\" rS=\"true\" s=\"2.0\" mS=\"1000000.0\" cSPU=\"8.0\" t=\"WEAPONS\">\n<d cl=\"st\">gauss</d>\n<c ref=\"253\"></c>\n</CIStack>"
        ));
        assert!(output.contains(
            "<CIStack z=\"286\" rS=\"true\" s=\"1.0\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"FIGHTER_CHIP\">\n<d cl=\"st\">xyphos_wing</d>\n<c ref=\"253\"></c>\n</CIStack>"
        ));
        assert!(output.contains(
            "<CIStack z=\"287\" rS=\"true\" s=\"1.0\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"SPECIAL\">\n<d cl=\"SpID\" z=\"288\" i=\"ship_bp\" d=\"onslaught\"></d>\n<c ref=\"253\"></c>\n</CIStack>"
        ));
        assert!(output.contains(
            "<CIStack z=\"289\" rS=\"true\" s=\"1.0\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"SPECIAL\">\n<d cl=\"SpID\" z=\"290\" i=\"high_tech_package\"></d>\n<c ref=\"253\"></c>\n</CIStack>"
        ));
        assert!(output.contains("z=\"253\" uS=\"true\" mC=\"30.0\" sU=\"42.5\""));
        XmlDocument::parse(review.campaign_output.clone(), XmlLimits::default()).unwrap();

        let fields: HashSet<_> = review
            .summary()
            .changes
            .iter()
            .map(|change| change.field.as_str())
            .collect();
        assert!(fields.contains(format!("colonies.{colony_id}.storage.add.resources.ore").as_str()));
        assert!(fields.contains(format!("colonies.{colony_id}.storage.add.weapons.gauss").as_str()));
        assert!(fields.contains(
            format!("colonies.{colony_id}.storage.add.fighter_wing.xyphos_wing").as_str()
        ));
        assert!(fields.contains(
            format!("colonies.{colony_id}.storage.add.special.ship_bp.onslaught").as_str()
        ));
        assert!(fields.contains(
            format!("colonies.{colony_id}.storage.add.special.high_tech_package").as_str()
        ));
        assert!(review
            .summary()
            .changes
            .iter()
            .filter(|change| { change.field.contains(".storage.add.") })
            .all(|change| change.old_value == "Not present"));
    }

    #[test]
    fn local_resource_addition_changes_only_main_cargo_and_used_space() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        let ore = cargo_item(InventoryKind::Resources, "ore", None);
        let additions = HashMap::from([cargo_addition(ore, 1.0, true)]);
        let review = opened
            .prepare_review_with_catalogs_and_additions(
                &[Edit::AddColonyResourceStack {
                    colony_id: colony_id.clone(),
                    commodity_id: "ore".to_owned(),
                    quantity: 7.25,
                }],
                &HashSet::new(),
                &HashSet::new(),
                &additions,
            )
            .unwrap();

        assert_eq!(review.campaign_patch_count(), 2);
        let output = String::from_utf8(review.campaign_output.clone()).unwrap();
        assert!(output.contains(
            "<CIStack z=\"284\" rS=\"false\" s=\"7.25\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"RESOURCES\">\n<d cl=\"st\">ore</d>\n<c ref=\"272\"></c>\n</CIStack>"
        ));
        assert!(output.contains("z=\"272\" uS=\"true\" mC=\"1000.0\" sU=\"22.25\""));
        assert!(output.contains("<taken cl=\"CargoData\" z=\"277\" uS=\"true\" mC=\"1000.0\" sU=\"0.0\"><s z=\"278\"></s><c z=\"279\"><value>0.0</value></c></taken>"));
        assert!(output.contains("<left cl=\"CargoData\" z=\"280\" uS=\"true\" mC=\"1000.0\" sU=\"0.0\"><s z=\"281\"></s><c z=\"282\"><value>0.0</value></c></left>"));
        assert!(output.contains("<stockpilingBonus z=\"283\"></stockpilingBonus>"));
        assert!(review.summary().changes.iter().any(|change| {
            change.field == format!("colonies.{colony_id}.local_resources.add.resources.ore")
                && change.old_value == "Not present"
                && change.new_value == "7.25"
        }));
        XmlDocument::parse(review.campaign_output, XmlLimits::default()).unwrap();
    }

    #[test]
    fn cargo_additions_require_catalog_authorization_and_reject_unsafe_shapes() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        let gauss = cargo_item(InventoryKind::Weapons, "gauss", None);
        let edit = Edit::AddStorageStack {
            colony_id: colony_id.clone(),
            item: gauss.clone(),
            quantity: 1.0,
        };
        assert_eq!(
            opened
                .prepare_review(std::slice::from_ref(&edit))
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        for cargo_space_per_unit in [-1.0, -0.0, f32::INFINITY] {
            let forged_catalog =
                HashMap::from([cargo_addition(gauss.clone(), cargo_space_per_unit, false)]);
            assert_eq!(
                opened
                    .prepare_review_with_catalogs_and_additions(
                        std::slice::from_ref(&edit),
                        &HashSet::new(),
                        &HashSet::new(),
                        &forged_catalog,
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidEdit
            );
        }
        let mismatched_catalog = HashMap::from([(
            gauss.clone(),
            CargoAdditionSpec {
                key: cargo_item(InventoryKind::Weapons, "laser", None),
                cargo_space_per_unit: 8.0,
                local_resources_eligible: false,
            },
        )]);
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    std::slice::from_ref(&edit),
                    &HashSet::new(),
                    &HashSet::new(),
                    &mismatched_catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let gauss_catalog = HashMap::from([cargo_addition(gauss.clone(), 8.0, false)]);
        for quantity in [0.0, 0.5, 1.5, 1_000_001.0, f32::INFINITY] {
            let error = opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id: colony_id.clone(),
                        item: gauss.clone(),
                        quantity,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &gauss_catalog,
                )
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidEdit);
        }

        let metals = cargo_item(InventoryKind::Resources, "metals", None);
        let duplicate_catalog = HashMap::from([cargo_addition(metals.clone(), 1.0, true)]);
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id: colony_id.clone(),
                        item: metals,
                        quantity: 2.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &duplicate_catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let ore = cargo_item(InventoryKind::Resources, "ore", None);
        let ore_catalog = HashMap::from([cargo_addition(ore.clone(), 1.0, true)]);
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[
                        Edit::AddStorageStack {
                            colony_id: colony_id.clone(),
                            item: ore.clone(),
                            quantity: 2.0,
                        },
                        Edit::AddStorageStack {
                            colony_id: colony_id.clone(),
                            item: ore.clone(),
                            quantity: 3.0,
                        },
                    ],
                    &HashSet::new(),
                    &HashSet::new(),
                    &ore_catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let storage_only_catalog = HashMap::from([cargo_addition(ore, 1.0, false)]);
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddColonyResourceStack {
                        colony_id: colony_id.clone(),
                        commodity_id: "ore".to_owned(),
                        quantity: 2.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &storage_only_catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let incomplete_blueprint = cargo_item(InventoryKind::Special, "ship_bp", None);
        let incomplete_catalog =
            HashMap::from([cargo_addition(incomplete_blueprint.clone(), 1.0, false)]);
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id,
                        item: incomplete_blueprint,
                        quantity: 1.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &incomplete_catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
    }

    #[test]
    fn cargo_additions_fail_closed_on_partials_noncanonical_ids_and_bounded_stacks() {
        let gauss = cargo_item(InventoryKind::Weapons, "gauss", None);
        let catalog = HashMap::from([cargo_addition(gauss.clone(), 8.0, false)]);

        let partials = inventory_colony_fixture().replace(
            "</s><c z=\"260\"><value>0.0</value></c><mS z=\"261\">",
            "</s><partials z=\"284\"></partials><c z=\"260\"><value>0.0</value></c><mS z=\"261\">",
        );
        let opened = open_campaign_fixture(partials);
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id,
                        item: gauss.clone(),
                        quantity: 1.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::AmbiguousStructure
        );

        let bounded = inventory_colony_fixture().replace(
            "<c cl=\"CargoData\" z=\"253\" uS=\"true\"",
            "<c cl=\"CargoData\" z=\"253\" uS=\"false\"",
        );
        let opened = open_campaign_fixture(bounded);
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id,
                        item: gauss.clone(),
                        quantity: 1.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );

        let noncanonical_identity = inventory_colony_fixture().replace(
            "<stockpilingBonus z=\"283\">",
            "<stockpilingBonus z=\"mod-identity\">",
        );
        let opened = open_campaign_fixture(noncanonical_identity);
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        assert_eq!(
            opened
                .prepare_review_with_catalogs_and_additions(
                    &[Edit::AddStorageStack {
                        colony_id,
                        item: gauss,
                        quantity: 1.0,
                    }],
                    &HashSet::new(),
                    &HashSet::new(),
                    &catalog,
                )
                .unwrap_err()
                .code,
            ErrorCode::AmbiguousStructure
        );
    }

    #[test]
    fn empty_unlimited_storage_remains_addition_capable() {
        let mut fixture = inventory_colony_fixture();
        let list_start = fixture.find("<s z=\"254\">").unwrap() + "<s z=\"254\">".len();
        let list_end = list_start + fixture[list_start..].find("</s>").unwrap();
        fixture.replace_range(list_start..list_end, "");
        fixture = fixture.replace("mC=\"30.0\" sU=\"20.0\"", "mC=\"30.0\" sU=\"0.0\"");

        let opened = open_campaign_fixture(fixture);
        assert!(opened.snapshot().capabilities.colony_storage);
        assert!(opened.snapshot().colonies[0]
            .storage
            .as_ref()
            .unwrap()
            .stacks
            .is_empty());
        let colony_id = opened.snapshot().colonies[0].colony_id.clone();
        let gauss = cargo_item(InventoryKind::Weapons, "gauss", None);
        let catalog = HashMap::from([cargo_addition(gauss.clone(), 8.0, false)]);
        let review = opened
            .prepare_review_with_catalogs_and_additions(
                &[Edit::AddStorageStack {
                    colony_id,
                    item: gauss,
                    quantity: 1.0,
                }],
                &HashSet::new(),
                &HashSet::new(),
                &catalog,
            )
            .unwrap();
        let output = String::from_utf8(review.campaign_output).unwrap();
        assert!(output.contains("<s z=\"254\">\n<CIStack z=\"284\""));
        assert!(output.contains("mC=\"30.0\" sU=\"8\""));
    }

    #[test]
    fn local_resources_exposes_non_resource_stacks_as_read_only() {
        let fixture = inventory_colony_fixture().replace(
            "z=\"275\" rS=\"false\" s=\"5.0\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"RESOURCES\"",
            "z=\"275\" rS=\"false\" s=\"5.0\" mS=\"1000000.0\" cSPU=\"1.0\" t=\"WEAPONS\"",
        );
        let opened = open_campaign_fixture(fixture);
        let resources = opened.snapshot().colonies[0]
            .local_resources
            .as_ref()
            .unwrap();
        let unsupported = resources
            .stacks
            .iter()
            .find(|stack| stack.item_id == "supplies")
            .unwrap();
        assert_eq!(unsupported.kind, InventoryKind::Weapons);
        assert!(!unsupported.structurally_editable);
        assert_eq!(
            unsupported.reason.as_deref(),
            Some("Local Resources only supports commodity resource stacks")
        );
    }

    #[test]
    fn quantity_rules_and_storage_scope_fail_closed() {
        let opened = open_campaign_fixture(inventory_colony_fixture());
        let weapon = opened
            .snapshot()
            .inventory
            .stacks
            .iter()
            .find(|stack| stack.kind == InventoryKind::Weapons)
            .unwrap();
        let unknown = opened
            .snapshot()
            .inventory
            .stacks
            .iter()
            .find(|stack| stack.kind == InventoryKind::Unknown)
            .unwrap();
        let trusted = HashSet::from([weapon.stack_id.clone(), unknown.stack_id.clone()]);
        for value in [0.0, -1.0, 1.5, 101.0, f32::INFINITY] {
            assert_eq!(
                opened
                    .prepare_review_with_catalogs(
                        &[Edit::SetInventoryStackQuantity {
                            stack_id: weapon.stack_id.clone(),
                            value,
                        }],
                        &HashSet::new(),
                        &trusted,
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidEdit
            );
        }
        assert_eq!(
            opened
                .prepare_review_with_catalogs(
                    &[Edit::SetInventoryStackQuantity {
                        stack_id: unknown.stack_id.clone(),
                        value: 4.0,
                    }],
                    &HashSet::new(),
                    &trusted,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
        let colony = &opened.snapshot().colonies[0];
        let resource = &colony.local_resources.as_ref().unwrap().stacks[0];
        let resource_trusted = HashSet::from([resource.stack_id.clone()]);
        for value in [0.0, -1.0, 1_000_001.0, f32::NAN] {
            assert_eq!(
                opened
                    .prepare_review_with_catalogs(
                        &[Edit::SetColonyResourceQuantity {
                            colony_id: colony.colony_id.clone(),
                            stack_id: resource.stack_id.clone(),
                            value,
                        }],
                        &HashSet::new(),
                        &resource_trusted,
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidEdit
            );
        }
        assert_eq!(
            opened
                .prepare_review_with_catalogs(
                    &[Edit::SetColonyResourceQuantity {
                        colony_id: colony.colony_id.clone(),
                        stack_id: weapon.stack_id.clone(),
                        value: 2.0,
                    }],
                    &HashSet::new(),
                    &trusted,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
        assert_eq!(
            opened
                .prepare_review_with_catalogs(
                    &[Edit::SetStorageStackQuantity {
                        colony_id: "colony-forged".into(),
                        stack_id: weapon.stack_id.clone(),
                        value: 2.0,
                    }],
                    &HashSet::new(),
                    &trusted,
                )
                .unwrap_err()
                .code,
            ErrorCode::InvalidEdit
        );
    }

    #[test]
    fn feature_local_structure_failures_do_not_block_other_editors() {
        let malformed_inventory = inventory_colony_fixture().replace(
            "s=\"5.0\" mS=\"50.0\" cSPU=\"1.0\" t=\"RESOURCES\"",
            "s=\"5.0\" cSPU=\"1.0\" t=\"RESOURCES\"",
        );
        let opened = open_campaign_fixture(malformed_inventory);
        assert!(!opened.snapshot().capabilities.inventory);
        assert!(opened.snapshot().inventory.stacks.is_empty());
        assert!(opened.snapshot().capabilities.basic_character);

        let malformed_resources = inventory_colony_fixture().replace(
            "<left cl=\"CargoData\" z=\"280\"",
            "<left cl=\"NotCargoData\" z=\"280\"",
        );
        let opened = open_campaign_fixture(malformed_resources);
        assert!(opened.snapshot().colonies[0].local_resources.is_none());
        assert!(!opened.snapshot().capabilities.colony_resources);
        assert!(opened.snapshot().colonies[0].storage.is_some());
        assert!(opened.snapshot().capabilities.colony_storage);
        assert!(opened.snapshot().colonies[0]
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_RESOURCES_CARGO_INVALID"));
        assert_eq!(opened.snapshot().colonies.len(), 1);

        let malformed_storage = inventory_colony_fixture()
            .replace("mC=\"30.0\" sU=\"20.0\"", "mC=\"30.0\" sU=\"not-a-number\"");
        let opened = open_campaign_fixture(malformed_storage);
        assert_eq!(opened.snapshot().colonies.len(), 1);
        assert!(opened.snapshot().colonies[0].storage.is_none());
        assert!(!opened.snapshot().capabilities.colony_storage);
        assert!(opened.snapshot().colonies[0]
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_STORAGE_UNAVAILABLE"));
        assert!(opened.snapshot().capabilities.basic_character);
    }

    #[test]
    fn malformed_storage_does_not_disable_another_colonys_safe_storage() {
        let fixture = two_colony_fixture().replacen(
            "mC=\"30.0\" sU=\"20.0\"",
            "mC=\"30.0\" sU=\"not-a-number\"",
            1,
        );
        let opened = open_campaign_fixture(fixture);
        assert_eq!(opened.snapshot().colonies.len(), 2);
        assert_eq!(
            opened
                .snapshot()
                .colonies
                .iter()
                .filter(|colony| colony.storage.is_some())
                .count(),
            1
        );
        assert!(opened.snapshot().capabilities.colony_storage);

        let colony = opened
            .snapshot()
            .colonies
            .iter()
            .find(|colony| colony.storage.is_some())
            .unwrap();
        let stack = &colony.storage.as_ref().unwrap().stacks[0];
        let trusted = HashSet::from([stack.stack_id.clone()]);
        opened
            .prepare_review_with_catalogs(
                &[Edit::SetStorageStackQuantity {
                    colony_id: colony.colony_id.clone(),
                    stack_id: stack.stack_id.clone(),
                    value: stack.quantity + 1.0,
                }],
                &HashSet::new(),
                &trusted,
            )
            .unwrap();
    }

    #[test]
    fn local_resources_ambiguity_backlinks_and_accounting_aliases_fail_closed() {
        let duplicate = inventory_colony_fixture().replace(
            "</p></Submarket><Submarket z=\"251\" s=\"storage\">",
            "</p></Submarket><Submarket z=\"284\" s=\"local_resources\"><m ref=\"210\"></m></Submarket><Submarket z=\"251\" s=\"storage\">",
        );
        let opened = open_campaign_fixture(duplicate);
        assert!(opened.snapshot().colonies[0].local_resources.is_none());
        assert!(opened.snapshot().colonies[0]
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_RESOURCES_AMBIGUOUS"));

        let wrong_backlink = inventory_colony_fixture().replace(
            "<Submarket z=\"270\" s=\"local_resources\"><m ref=\"210\"></m>",
            "<Submarket z=\"270\" s=\"local_resources\"><m ref=\"220\"></m>",
        );
        let opened = open_campaign_fixture(wrong_backlink);
        assert!(opened.snapshot().colonies[0].local_resources.is_none());
        assert!(opened.snapshot().colonies[0]
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_RESOURCES_BACKLINK_INVALID"));

        let shared_accounting = inventory_colony_fixture().replace(
            "<left cl=\"CargoData\" z=\"280\" uS=\"true\" mC=\"1000.0\" sU=\"0.0\"><s z=\"281\"></s><c z=\"282\"><value>0.0</value></c></left>",
            "<left ref=\"277\"></left>",
        );
        let opened = open_campaign_fixture(shared_accounting);
        assert!(opened.snapshot().colonies[0].local_resources.is_none());
        assert!(opened.snapshot().colonies[0]
            .warnings
            .iter()
            .any(|warning| warning.code == "COLONY_RESOURCES_CARGO_INVALID"));
    }

    #[test]
    fn malformed_resources_do_not_disable_another_colonys_safe_resources() {
        let fixture = two_colony_fixture().replacen(
            "cl=\"LocalResourcesSubmarketPlugin\"",
            "cl=\"UnsupportedResourcesPlugin\"",
            1,
        );
        let opened = open_campaign_fixture(fixture);
        assert_eq!(opened.snapshot().colonies.len(), 2);
        assert_eq!(
            opened
                .snapshot()
                .colonies
                .iter()
                .filter(|colony| colony.local_resources.is_some())
                .count(),
            1
        );
        assert!(opened.snapshot().capabilities.colony_resources);

        let colony = opened
            .snapshot()
            .colonies
            .iter()
            .find(|colony| colony.local_resources.is_some())
            .unwrap();
        let stack = &colony.local_resources.as_ref().unwrap().stacks[0];
        let trusted = HashSet::from([stack.stack_id.clone()]);
        opened
            .prepare_review_with_catalogs(
                &[Edit::SetColonyResourceQuantity {
                    colony_id: colony.colony_id.clone(),
                    stack_id: stack.stack_id.clone(),
                    value: stack.quantity + 1.0,
                }],
                &HashSet::new(),
                &trusted,
            )
            .unwrap();
    }
}
