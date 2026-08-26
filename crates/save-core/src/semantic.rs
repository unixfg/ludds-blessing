use crate::descriptor::{parse_descriptor, DescriptorDocument};
use crate::error::{CoreError, ErrorCode, Result};
use crate::file_util::{fingerprint, opaque_id, read_regular_file};
use crate::model::*;
use crate::progression::{OfficerProgress, PlayerProgress};
use crate::skill_json::SkillJsonDocument;
use crate::xml::{ElementId, XmlDocument, XmlLimits};
use crate::{SUPPORTED_GAME_VERSION, SUPPORTED_SAVE_FORMAT};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
pub struct OpenOptions {
    pub campaign_limits: XmlLimits,
    pub descriptor_limits: XmlLimits,
    /// Set only after the UI has completed the explicit protected-save unlock.
    pub allow_protected: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            campaign_limits: XmlLimits::default(),
            descriptor_limits: XmlLimits {
                max_bytes: 4 * 1024 * 1024,
                max_elements: 100_000,
                ..XmlLimits::default()
            },
            allow_protected: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenedSave {
    pub(crate) location: SaveLocation,
    pub(crate) campaign: XmlDocument,
    pub(crate) descriptor: DescriptorDocument,
    pub(crate) anchors: CampaignAnchors,
    pub(crate) state: SemanticState,
    pub(crate) snapshot: SaveSnapshot,
    pub(crate) options: OpenOptions,
}

impl OpenedSave {
    pub fn open(location: SaveLocation, options: OpenOptions) -> Result<Self> {
        let descriptor_bytes = read_regular_file(
            &location.descriptor_path,
            options.descriptor_limits.max_bytes,
        )?;
        let descriptor = parse_descriptor(descriptor_bytes, options.descriptor_limits)?;
        if descriptor.metadata.compressed {
            return Err(CoreError::new(
                ErrorCode::UnsupportedCompression,
                "compressed campaign data cannot be opened",
            ));
        }
        let campaign_bytes =
            read_regular_file(&location.campaign_path, options.campaign_limits.max_bytes)?;
        let campaign = XmlDocument::parse(campaign_bytes, options.campaign_limits)?;
        Self::from_documents(location, campaign, descriptor, options)
    }

    pub(crate) fn from_documents(
        location: SaveLocation,
        campaign: XmlDocument,
        descriptor: DescriptorDocument,
        options: OpenOptions,
    ) -> Result<Self> {
        let save_id = opaque_id("save", location.save_dir.to_string_lossy().as_bytes());
        let (anchors, state) = extract_campaign(&campaign, &save_id)?;
        let revision = ContentRevision {
            campaign: fingerprint(campaign.bytes()),
            descriptor: fingerprint(descriptor.xml.bytes()),
        };
        let (compatibility, mut capabilities, mut warnings) = capabilities(&descriptor, options);
        capabilities.inventory &= state.inventory_available;
        capabilities.colony_storage &= state.colony_storage_available;
        capabilities.colony_resources &= state.colony_resources_available;
        warnings.extend(state.warnings.clone());
        add_mirror_warnings(&state, &descriptor, &mut warnings);
        let snapshot = state.to_snapshot(
            save_id,
            revision,
            descriptor.metadata.clone(),
            compatibility,
            capabilities,
            warnings,
        )?;
        Ok(Self {
            location,
            campaign,
            descriptor,
            anchors,
            state,
            snapshot,
            options,
        })
    }

    pub fn snapshot(&self) -> &SaveSnapshot {
        &self.snapshot
    }

    pub fn location(&self) -> &SaveLocation {
        &self.location
    }
}

fn capabilities(
    descriptor: &DescriptorDocument,
    options: OpenOptions,
) -> (Compatibility, FieldCapabilities, Vec<Warning>) {
    let metadata = &descriptor.metadata;
    let protected = metadata.iron_mode || metadata.autosave;
    let version_ok = metadata.game_version == SUPPORTED_GAME_VERSION
        && metadata.save_format == SUPPORTED_SAVE_FORMAT;
    let write_shape_ok = descriptor.has_complete_write_shape();
    let compatibility = if metadata.compressed {
        Compatibility::ReadOnly {
            code: ErrorCode::UnsupportedCompression,
            reason: "compressed saves are read-only".to_owned(),
        }
    } else if !version_ok {
        Compatibility::ReadOnly {
            code: ErrorCode::UnsupportedVersion,
            reason: format!(
                "editing requires {SUPPORTED_GAME_VERSION} / format {SUPPORTED_SAVE_FORMAT}"
            ),
        }
    } else if !write_shape_ok {
        Compatibility::ReadOnly {
            code: ErrorCode::AmbiguousStructure,
            reason: "the descriptor is missing required RC8 write metadata".to_owned(),
        }
    } else {
        Compatibility::Editable
    };
    let base_editable = version_ok
        && write_shape_ok
        && !metadata.compressed
        && (!protected || options.allow_protected);
    let progression = base_editable && metadata.enabled_mods.is_empty();
    let reason = if !version_ok {
        Some("unsupported game or save format".to_owned())
    } else if !write_shape_ok {
        Some("descriptor is missing required RC8 write metadata".to_owned())
    } else if protected && !options.allow_protected {
        Some("protected save is locked for this session".to_owned())
    } else {
        None
    };
    let capabilities = FieldCapabilities {
        basic_character: base_editable,
        progression,
        skills: base_editable,
        reputation: base_editable,
        officers: base_editable,
        inventory: base_editable,
        colony_storage: base_editable,
        colony_resources: base_editable,
        protected_save: protected,
        reason,
    };
    let mut warnings = Vec::new();
    if protected {
        warnings.push(Warning {
            code: "PROTECTED_SAVE".to_owned(),
            message: if options.allow_protected {
                "Protected save unlocked for this session; create an immediate pinned backup"
                    .to_owned()
            } else {
                "Protected save is locked until explicitly acknowledged".to_owned()
            },
            acknowledgement_required: !options.allow_protected,
        });
    }
    if !metadata.enabled_mods.is_empty() {
        warnings.push(Warning {
            code: "MODDED_PROGRESSION_DISABLED".to_owned(),
            message: "XP simulation is disabled because enabled mods may alter progression"
                .to_owned(),
            acknowledgement_required: false,
        });
    }
    (compatibility, capabilities, warnings)
}

fn add_mirror_warnings(
    state: &SemanticState,
    descriptor: &DescriptorDocument,
    warnings: &mut Vec<Warning>,
) {
    let full_name = join_name(&state.player.first_name, &state.player.last_name);
    if descriptor.metadata.character_name != full_name || state.character_summary_name != full_name
    {
        warnings.push(Warning {
            code: "NAME_MIRROR_MISMATCH".to_owned(),
            message: "Character-name mirrors disagree; editing the name will synchronize them"
                .to_owned(),
            acknowledgement_required: false,
        });
    }
    if descriptor.metadata.portrait != state.player.portrait
        || state.character_summary_portrait != state.player.portrait
    {
        warnings.push(Warning {
            code: "PORTRAIT_MIRROR_MISMATCH".to_owned(),
            message: "Portrait mirrors disagree; editing the portrait will synchronize them"
                .to_owned(),
            acknowledgement_required: false,
        });
    }
    if descriptor.metadata.character_level != state.player.stats.level {
        warnings.push(Warning {
            code: "LEVEL_MIRROR_MISMATCH".to_owned(),
            message: "Descriptor level does not match the player object".to_owned(),
            acknowledgement_required: true,
        });
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CampaignAnchors {
    pub character_name: ElementId,
    pub character_portrait: ElementId,
    pub player_person: PersonAnchors,
    pub credits_value: ElementId,
    pub inventory: Option<CargoAnchors>,
    pub skills_ever_elite: ElementId,
    pub relations: HashMap<String, RelationAnchors>,
    pub officers: HashMap<String, OfficerAnchors>,
    pub colonies: HashMap<String, ColonyAnchors>,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoAnchors {
    pub cargo: ElementId,
    pub stack_container: ElementId,
    pub stacks: HashMap<String, ElementId>,
    pub scope_key: String,
    pub stack_prefix: String,
    pub slot_count: usize,
    pub unlimited_stacks: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ColonyAnchors {
    pub storage: Option<CargoAnchors>,
    pub local_resources: Option<CargoAnchors>,
}

#[derive(Debug, Clone)]
pub(crate) struct PersonAnchors {
    pub person: ElementId,
    pub name: ElementId,
    pub stats: StatsAnchors,
}

#[derive(Debug, Clone)]
pub(crate) struct StatsAnchors {
    pub stats: ElementId,
    pub skills: ElementId,
}

#[derive(Debug, Clone)]
pub(crate) struct RelationAnchors {
    pub value: ElementId,
    pub history_last_value: Option<ElementId>,
    pub history_positive_timestamp: Option<ElementId>,
    pub history_negative_timestamp: Option<ElementId>,
}

#[derive(Debug, Clone)]
pub(crate) struct OfficerAnchors {
    pub person: PersonAnchors,
    pub skill_picks: ElementId,
    pub made_picks: ElementId,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticState {
    pub player: PersonState,
    pub character_summary_name: String,
    pub character_summary_portrait: String,
    pub credits: f32,
    pub inventory: CargoState,
    pub inventory_available: bool,
    pub skills_ever_elite: Vec<String>,
    pub relations: IndexMap<String, RelationState>,
    pub officers: IndexMap<String, OfficerState>,
    pub colonies: IndexMap<String, ColonyState>,
    pub colony_storage_available: bool,
    pub colony_resources_available: bool,
    pub timestamp: i64,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoState {
    pub stacks: IndexMap<String, CargoStackState>,
    pub slot_order: Vec<String>,
    pub used_space: f32,
    pub max_space: Option<f32>,
    pub warnings: Vec<Warning>,
    pub recompute_safe: bool,
    pub scope_editable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoStackState {
    pub kind: InventoryKind,
    pub item_id: String,
    pub special_data: Option<String>,
    pub quantity: f32,
    pub max_quantity: f32,
    pub cargo_space_per_unit: f32,
    pub structurally_editable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ColonyState {
    pub name: String,
    pub faction_id: String,
    pub location_context: Option<String>,
    pub storage: Option<CargoState>,
    pub local_resources: Option<CargoState>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone)]
pub(crate) struct PersonState {
    pub first_name: String,
    pub last_name: String,
    pub portrait: String,
    pub personality: String,
    pub stats: InternalStats,
    pub skills: IndexMap<String, SkillRank>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InternalStats {
    pub story_checkpoint_xp: u64,
    pub xp: u64,
    pub bonus_xp: u64,
    pub deferred_bonus_xp: u64,
    pub level: u32,
    pub skill_points: u32,
    pub story_points: u32,
}

impl InternalStats {
    pub fn player_progress(&self) -> PlayerProgress {
        PlayerProgress {
            story_checkpoint_xp: self.story_checkpoint_xp,
            xp: self.xp,
            bonus_xp: self.bonus_xp,
            deferred_bonus_xp: self.deferred_bonus_xp,
            level: self.level,
            skill_points: self.skill_points,
            story_points: self.story_points,
        }
    }

    pub fn update_player(&mut self, progress: PlayerProgress) {
        self.story_checkpoint_xp = progress.story_checkpoint_xp;
        self.xp = progress.xp;
        self.bonus_xp = progress.bonus_xp;
        self.deferred_bonus_xp = progress.deferred_bonus_xp;
        self.level = progress.level;
        self.skill_points = progress.skill_points;
        self.story_points = progress.story_points;
    }

    pub fn officer_progress(&self) -> OfficerProgress {
        OfficerProgress {
            xp: self.xp,
            bonus_xp: self.bonus_xp,
            level: self.level,
            skill_points: self.skill_points,
        }
    }

    pub fn update_officer(&mut self, progress: OfficerProgress) {
        self.xp = progress.xp;
        self.bonus_xp = progress.bonus_xp;
        self.level = progress.level;
        self.skill_points = progress.skill_points;
    }

    fn view(&self) -> ProgressionView {
        ProgressionView {
            level: self.level,
            xp: DecimalU64::new(self.xp),
            story_checkpoint_xp: DecimalU64::new(self.story_checkpoint_xp),
            bonus_xp: DecimalU64::new(self.bonus_xp),
            deferred_bonus_xp: DecimalU64::new(self.deferred_bonus_xp),
            skill_points: self.skill_points,
            story_points: self.story_points,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RelationState {
    pub value_percent: f32,
    pub has_history: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OfficerState {
    pub person: PersonState,
    pub assigned: bool,
    pub pending_skill_picks: Vec<String>,
    pub made_picks: bool,
}

impl SemanticState {
    fn to_snapshot(
        &self,
        save_id: String,
        revision: ContentRevision,
        metadata: SaveMetadata,
        compatibility: Compatibility,
        capabilities: FieldCapabilities,
        warnings: Vec<Warning>,
    ) -> Result<SaveSnapshot> {
        let known_skills: HashSet<&str> = self.player.skills.keys().map(String::as_str).collect();
        let character = PlayerCharacter {
            first_name: self.player.first_name.clone(),
            last_name: self.player.last_name.clone(),
            full_name: join_name(&self.player.first_name, &self.player.last_name),
            portrait: self.player.portrait.clone(),
            credits: self.credits,
            progression: self.player.stats.view(),
            skills: skill_views(&self.player.skills, &known_skills, capabilities.skills),
            skills_ever_elite: self.skills_ever_elite.clone(),
        };
        let reputation = self
            .relations
            .iter()
            .map(|(faction_id, relation)| FactionRelation {
                faction_id: faction_id.clone(),
                value_percent: relation.value_percent,
                has_history: relation.has_history,
            })
            .collect();
        let officers = self
            .officers
            .iter()
            .map(|(officer_id, officer)| Officer {
                officer_id: officer_id.clone(),
                first_name: officer.person.first_name.clone(),
                last_name: officer.person.last_name.clone(),
                portrait: officer.person.portrait.clone(),
                personality: officer.person.personality.clone(),
                assigned: officer.assigned,
                progression: officer.person.stats.view(),
                skills: skill_views(&officer.person.skills, &known_skills, capabilities.officers),
                pending_skill_picks: officer.pending_skill_picks.clone(),
            })
            .collect();
        let inventory = self.inventory.view();
        let colonies = self
            .colonies
            .iter()
            .map(|(colony_id, colony)| Colony {
                colony_id: colony_id.clone(),
                name: colony.name.clone(),
                location_context: colony.location_context.clone(),
                storage: colony.storage.as_ref().map(CargoState::view),
                local_resources: colony.local_resources.as_ref().map(CargoState::view),
                warnings: colony.warnings.clone(),
            })
            .collect();
        Ok(SaveSnapshot {
            save_id,
            revision,
            metadata,
            compatibility,
            capabilities,
            character,
            inventory,
            reputation,
            officers,
            colonies,
            warnings,
        })
    }
}

impl CargoState {
    fn unavailable() -> Self {
        Self {
            stacks: IndexMap::new(),
            slot_order: Vec::new(),
            used_space: 0.0,
            max_space: None,
            warnings: Vec::new(),
            recompute_safe: false,
            scope_editable: false,
        }
    }

    pub(crate) fn view(&self) -> InventoryView {
        InventoryView {
            stacks: self
                .stacks
                .iter()
                .map(|(stack_id, stack)| InventoryStack {
                    stack_id: stack_id.clone(),
                    kind: stack.kind,
                    item_id: stack.item_id.clone(),
                    special_data: stack.special_data.clone(),
                    quantity: stack.quantity,
                    max_quantity: stack.max_quantity,
                    cargo_space_per_unit: stack.cargo_space_per_unit,
                    structurally_editable: stack.structurally_editable,
                    reason: stack.reason.clone(),
                })
                .collect(),
            used_space: self.used_space,
            max_space: self.max_space,
        }
    }

    pub(crate) fn recompute_used_space(&self) -> Result<f32> {
        let mut total = 0.0_f32;
        for stack_id in &self.slot_order {
            let stack = self.stacks.get(stack_id).ok_or_else(|| {
                CoreError::validation("cargo slot order references a missing stack")
            })?;
            total += stack.quantity * stack.cargo_space_per_unit;
            if !total.is_finite() {
                return Err(CoreError::validation("cargo used space overflowed"));
            }
        }
        Ok(total)
    }
}

fn feature_warning(code: &str, prefix: &str, error: &CoreError) -> Warning {
    Warning {
        code: code.to_owned(),
        message: format!("{prefix}: {}", error.message),
        acknowledgement_required: false,
    }
}

fn skill_views(
    skills: &IndexMap<String, SkillRank>,
    known: &HashSet<&str>,
    section_editable: bool,
) -> Vec<SkillState> {
    skills
        .iter()
        .map(|(id, rank)| SkillState {
            id: id.clone(),
            rank: *rank,
            known: known.contains(id.as_str()),
            editable: section_editable && known.contains(id.as_str()),
        })
        .collect()
}

pub(crate) fn extract_campaign(
    xml: &XmlDocument,
    save_id: &str,
) -> Result<(CampaignAnchors, SemanticState)> {
    let root = xml.root();
    if xml.name(root) != "CampaignEngine" {
        return Err(CoreError::ambiguous(format!(
            "campaign root is {}, expected CampaignEngine",
            xml.name(root)
        )));
    }
    let player_fleet_ref = xml.unique_direct_child(root, "playerFleet")?;
    let player_fleet = xml.resolve(player_fleet_ref)?;
    let character_data = xml.unique_direct_child(root, "characterData")?;
    let person_ref = xml.unique_direct_child(character_data, "person")?;
    let player_person_id = xml.resolve(person_ref)?;
    let player_person = parse_person(xml, player_person_id)?;

    let character_name = xml.unique_direct_child(character_data, "name")?;
    let character_portrait = xml.unique_direct_child(character_data, "portraitName")?;
    let elite_container = xml.unique_direct_child(character_data, "skillsEverMadeElite")?;
    let skills_ever_elite = xml
        .direct_children_named(elite_container, "st")
        .into_iter()
        .map(|id| xml.simple_text(id))
        .collect::<Result<Vec<_>>>()?;

    let fleet_data_candidate = xml.unique_direct_child(player_fleet, "fD")?;
    let fleet_data = xml.resolve(fleet_data_candidate)?;
    let fleet_player_ref = xml.unique_direct_child(fleet_data, "c")?;
    if xml.resolve(fleet_player_ref)? != player_person_id {
        return Err(CoreError::ambiguous(
            "characterData person does not match the player fleet commander reference",
        ));
    }
    let cargo_candidate = xml.unique_direct_child(fleet_data, "cargo")?;
    let cargo = xml.resolve(cargo_candidate)?;
    let (inventory_anchors, inventory, inventory_available, inventory_warning) = match extract_cargo(
        xml,
        cargo,
        &format!("{save_id}:player"),
        "inventory-stack",
        true,
    ) {
        Ok((anchors, state)) => {
            let available = state.recompute_safe;
            (Some(anchors), state, available, None)
        }
        Err(error) => (
            None,
            CargoState::unavailable(),
            false,
            Some(feature_warning(
                "INVENTORY_UNAVAILABLE",
                "Player inventory could not be indexed safely",
                &error,
            )),
        ),
    };
    let mut credit_candidates = Vec::new();
    for candidate in xml.direct_children_named(cargo, "c") {
        if let Some(value) = xml.optional_unique_direct_child(candidate, "value")? {
            credit_candidates.push((candidate, value));
        }
    }
    if credit_candidates.len() != 1 {
        return Err(CoreError::ambiguous(format!(
            "expected one credit value in player cargo, found {}",
            credit_candidates.len()
        )));
    }
    let credits_value = credit_candidates[0].1;
    let credits = parse_f32(&xml.simple_text(credits_value)?, "credits")?;
    if credits < 0.0 {
        return Err(CoreError::validation("credits are negative"));
    }

    // XStream omits the officer list entirely when the authoritative roster is
    // empty (notably in a freshly-started campaign). Treat that one omission as
    // an empty roster, while `optional_unique_direct_child` still rejects a
    // duplicated list and the existing per-entry checks reject malformed
    // populated rosters.
    let officer_container = xml.optional_unique_direct_child(fleet_data, "o")?;
    let officer_entries = officer_container
        .map(|container| xml.direct_children_named(container, "OfficerData"))
        .unwrap_or_default();
    let mut officers = IndexMap::new();
    let mut officer_anchors = HashMap::new();
    let mut seen_people = HashSet::new();
    for (index, officer_data) in officer_entries.into_iter().enumerate() {
        let person_ref = xml.unique_direct_child(officer_data, "person")?;
        let person_id = xml.resolve(person_ref)?;
        if !seen_people.insert(person_id) {
            return Err(CoreError::ambiguous(
                "officer roster contains a duplicate person",
            ));
        }
        let (person_anchors, person) = parse_person(xml, person_id)?;
        let stable_person_key = xml.attribute(person_id, "id").unwrap_or("anonymous");
        let officer_id = opaque_id(
            "officer",
            format!("{save_id}:{index}:{stable_person_key}").as_bytes(),
        );
        let skill_picks = xml.unique_direct_child(officer_data, "skillPicks")?;
        let pending_skill_picks = xml
            .direct_children_named(skill_picks, "st")
            .into_iter()
            .map(|id| xml.simple_text(id))
            .collect::<Result<Vec<_>>>()?;
        let made_picks = xml.unique_direct_child(officer_data, "madePicks")?;
        let made_picks_value = parse_bool(&xml.simple_text(made_picks)?, "madePicks")?;
        let assigned = has_ancestor_named(xml, person_id, "FMmbr", fleet_data);
        officer_anchors.insert(
            officer_id.clone(),
            OfficerAnchors {
                person: person_anchors,
                skill_picks,
                made_picks,
            },
        );
        officers.insert(
            officer_id,
            OfficerState {
                person,
                assigned,
                pending_skill_picks,
                made_picks: made_picks_value,
            },
        );
    }

    let faction_manager = xml.unique_direct_child(root, "factionManager")?;
    let player_faction_ref = xml.unique_direct_child(faction_manager, "playerFaction")?;
    let player_faction = xml.resolve(player_faction_ref)?;
    let player_faction_id = xml.child_text(player_faction, "id")?;
    let player_faction_id_ref = player_faction_id.as_str();
    let (
        colony_anchors,
        colonies,
        colony_storage_available,
        colony_resources_available,
        colony_warning,
    ) = match extract_colonies(xml, root, save_id, player_faction_id_ref) {
        Ok((anchors, colonies)) => {
            // This is a section-level capability, not an all-colonies
            // invariant. One malformed storage must not make another
            // colony's independently anchored storage read-only. Missing
            // or unsafe storages have no anchors and are rejected again at
            // review time by colony and stack scope.
            let available = colonies.values().any(|colony| {
                colony
                    .storage
                    .as_ref()
                    .is_some_and(|storage| storage.recompute_safe && storage.scope_editable)
            });
            let resources_available = colonies.values().any(|colony| {
                colony
                    .local_resources
                    .as_ref()
                    .is_some_and(|resources| resources.recompute_safe && resources.scope_editable)
            });
            (anchors, colonies, available, resources_available, None)
        }
        Err(error) => (
            HashMap::new(),
            IndexMap::new(),
            false,
            false,
            Some(feature_warning(
                "COLONIES_UNAVAILABLE",
                "Colony details could not be indexed safely",
                &error,
            )),
        ),
    };
    let relations_container = xml.unique_direct_child(faction_manager, "relations")?;
    let histories = parse_reputation_histories(xml, root)?;
    let mut relations = IndexMap::new();
    let mut relation_anchors = HashMap::new();
    let mut relation_definitions = HashSet::new();
    for entry in xml.direct_children_named(relations_container, "e") {
        let candidates = xml.direct_children_named(entry, "FMRelation");
        if candidates.len() != 1 {
            continue;
        }
        let definition = xml.resolve(candidates[0])?;
        if !relation_definitions.insert(definition) {
            continue;
        }
        let one = xml.child_text(definition, "factionIdOne")?;
        let two = xml.child_text(definition, "factionIdTwo")?;
        let other = match (one.as_str(), two.as_str()) {
            (left, right) if left == player_faction_id_ref && right == player_faction_id_ref => {
                continue
            }
            (left, other) if left == player_faction_id_ref => other,
            (other, right) if right == player_faction_id_ref => other,
            _ => continue,
        };
        if relations.contains_key(other) {
            return Err(CoreError::ambiguous(format!(
                "multiple defining player relations for {other}"
            )));
        }
        let value = xml.unique_direct_child(definition, "value")?;
        let game_value = parse_f32(&xml.simple_text(value)?, "relation value")?;
        if !(-1.0..=1.0).contains(&game_value) {
            return Err(CoreError::validation(format!(
                "relation for {other} is outside -1..1"
            )));
        }
        let history = histories.get(other).cloned().unwrap_or_default();
        relations.insert(
            other.to_owned(),
            RelationState {
                value_percent: game_value * 100.0,
                has_history: history.last_value.is_some(),
            },
        );
        relation_anchors.insert(
            other.to_owned(),
            RelationAnchors {
                value,
                history_last_value: history.last_value,
                history_positive_timestamp: history.positive_timestamp,
                history_negative_timestamp: history.negative_timestamp,
            },
        );
    }

    let clock = xml.unique_direct_child(root, "clock")?;
    let timestamp = xml.unique_direct_child(clock, "timestamp")?;
    let timestamp_value = parse_i64(&xml.simple_text(timestamp)?, "campaign timestamp")?;
    xml.unique_direct_child(root, "saveDirName")?;
    let mut warnings = Vec::new();
    warnings.extend(inventory.warnings.clone());
    warnings.extend(inventory_warning);
    warnings.extend(colony_warning);
    for (id, officer) in &officers {
        let learned = officer
            .person
            .skills
            .values()
            .filter(|rank| **rank != SkillRank::Unlearned)
            .count() as u32;
        if learned > officer.person.stats.level {
            warnings.push(Warning {
                code: "OFFICER_SKILL_COUNT_UNUSUAL".to_owned(),
                message: format!("Officer {id} has more learned skills than levels"),
                acknowledgement_required: false,
            });
        }
    }
    Ok((
        CampaignAnchors {
            character_name,
            character_portrait,
            player_person: player_person.0,
            credits_value,
            inventory: inventory_anchors,
            skills_ever_elite: elite_container,
            relations: relation_anchors,
            officers: officer_anchors,
            colonies: colony_anchors,
        },
        SemanticState {
            player: player_person.1,
            character_summary_name: xml.simple_text(character_name)?,
            character_summary_portrait: xml.simple_text(character_portrait)?,
            credits,
            inventory,
            inventory_available,
            skills_ever_elite,
            relations,
            officers,
            colonies,
            colony_storage_available,
            colony_resources_available,
            timestamp: timestamp_value,
            warnings,
        },
    ))
}

fn extract_cargo(
    xml: &XmlDocument,
    cargo_candidate: ElementId,
    scope_key: &str,
    stack_prefix: &str,
    scope_editable: bool,
) -> Result<(CargoAnchors, CargoState)> {
    const MAX_CARGO_SLOTS: usize = 100_000;

    let cargo = xml.resolve(cargo_candidate)?;
    let stack_container = xml.resolve(xml.unique_direct_child(cargo, "s")?)?;
    let unlimited_stacks = xml
        .attribute(cargo, "uS")
        .map(|value| parse_bool(value, "cargo unlimited stacks"))
        .transpose()?
        .unwrap_or(false);
    let used_space =
        parse_nonnegative_f32(xml.require_attribute(cargo, "sU")?, "cargo used space")?;
    let max_space = Some(parse_nonnegative_f32(
        xml.require_attribute(cargo, "mC")?,
        "cargo capacity",
    )?);
    let slots = xml.children(stack_container);
    if slots.len() > MAX_CARGO_SLOTS {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            "cargo stack count exceeds the configured safety limit",
        ));
    }

    let mut anchors = HashMap::new();
    let mut stacks = IndexMap::<String, CargoStackState>::new();
    let mut definition_ids = HashMap::<ElementId, String>::new();
    let mut slot_order = Vec::new();
    let mut warnings = Vec::new();
    let mut recompute_safe = true;

    for (ordinal, slot) in slots.iter().copied().enumerate() {
        let definition = xml.resolve(slot)?;
        if xml.name(definition) != "CIStack" && xml.attribute(definition, "cl") != Some("CIStack") {
            recompute_safe = false;
            warnings.push(Warning {
                code: "CARGO_STACK_REFERENCE_WRONG_TYPE".to_owned(),
                message: "Cargo contains a stack reference with the wrong resolved type".to_owned(),
                acknowledgement_required: false,
            });
            continue;
        }
        let raw_kind = xml.require_attribute(definition, "t")?;
        if raw_kind == "NULL" {
            continue;
        }
        if let Some(existing_id) = definition_ids.get(&definition).cloned() {
            slot_order.push(existing_id.clone());
            if let Some(existing) = stacks.get_mut(&existing_id) {
                existing.structurally_editable = false;
                existing.reason =
                    Some("The same non-empty stack object occupies multiple slots".into());
            }
            recompute_safe = false;
            continue;
        }

        let quantity = parse_nonnegative_f32(
            xml.require_attribute(definition, "s")?,
            "cargo stack quantity",
        )?;
        let max_quantity = parse_nonnegative_f32(
            xml.require_attribute(definition, "mS")?,
            "cargo stack maximum",
        )?;
        let cargo_space_per_unit = parse_nonnegative_f32(
            xml.require_attribute(definition, "cSPU")?,
            "cargo space per unit",
        )?;
        if max_quantity <= 0.0 {
            return Err(CoreError::validation(
                "non-empty cargo stack has a nonpositive maximum",
            ));
        }

        let kind = match raw_kind {
            "RESOURCES" => InventoryKind::Resources,
            "WEAPONS" => InventoryKind::Weapons,
            "FIGHTER_CHIP" => InventoryKind::FighterChip,
            "SPECIAL" => InventoryKind::Special,
            _ => InventoryKind::Unknown,
        };
        let mut structurally_editable = scope_editable && kind != InventoryKind::Unknown;
        let mut reason = (!scope_editable).then(|| "This cargo container is not unlocked".into());
        if kind == InventoryKind::Unknown {
            structurally_editable = false;
            reason = Some(if raw_kind.len() <= 64 {
                format!("Unsupported RC8 cargo kind {raw_kind}")
            } else {
                "Unsupported RC8 cargo kind".into()
            });
        }

        let data_children = xml.direct_children_named(definition, "d");
        let (item_id, special_data) = if data_children.len() == 1 {
            let data = data_children[0];
            if kind == InventoryKind::Special {
                match xml.attribute(data, "i") {
                    Some(id) if !id.is_empty() => {
                        (id.to_owned(), xml.attribute(data, "d").map(str::to_owned))
                    }
                    _ => {
                        structurally_editable = false;
                        reason = Some("Special-item data has no item ID".into());
                        ("unknown".to_owned(), None)
                    }
                }
            } else {
                match xml.simple_text(data) {
                    Ok(id) if !id.is_empty() => (id, None),
                    _ => {
                        structurally_editable = false;
                        reason = Some("Cargo item data is not a scalar item ID".into());
                        ("unknown".to_owned(), None)
                    }
                }
            }
        } else {
            structurally_editable = false;
            reason = Some("Cargo stack does not contain exactly one item-data field".into());
            ("unknown".to_owned(), None)
        };

        let backlinks = xml.direct_children_named(definition, "c");
        if backlinks.len() != 1 || xml.resolve(backlinks[0]).ok() != Some(cargo) {
            structurally_editable = false;
            reason = Some("Cargo stack backlink does not identify its containing cargo".into());
        }
        let identity = xml.attribute(definition, "z");
        if identity.is_none() {
            structurally_editable = false;
            reason = Some("Cargo stack definition has no identity".into());
        }
        let mut item_id = item_id;
        let mut special_data = special_data;
        if !bounded_display_value(&item_id, 512) {
            item_id = "unknown".into();
            special_data = None;
            structurally_editable = false;
            reason = Some("Cargo item ID exceeds the display safety limit".into());
        }
        if special_data
            .as_ref()
            .is_some_and(|data| !bounded_display_value(data, 4096))
        {
            special_data = None;
            structurally_editable = false;
            reason = Some("Special-item data exceeds the display safety limit".into());
        }
        let stack_id = cargo_stack_selector(
            scope_key,
            stack_prefix,
            ordinal,
            identity.unwrap_or("anonymous"),
            raw_kind,
            &item_id,
        );
        if stacks.contains_key(&stack_id) {
            return Err(CoreError::ambiguous(
                "opaque cargo stack selector collision",
            ));
        }
        definition_ids.insert(definition, stack_id.clone());
        slot_order.push(stack_id.clone());
        anchors.insert(stack_id.clone(), definition);
        stacks.insert(
            stack_id,
            CargoStackState {
                kind,
                item_id,
                special_data,
                quantity,
                max_quantity,
                cargo_space_per_unit,
                structurally_editable,
                reason,
            },
        );
    }

    if !recompute_safe {
        for stack in stacks.values_mut() {
            stack.structurally_editable = false;
            stack.reason.get_or_insert_with(|| {
                "Cargo slot structure is ambiguous, so used space cannot be safely rewritten".into()
            });
        }
    }
    let state = CargoState {
        stacks,
        slot_order,
        used_space,
        max_space,
        warnings,
        recompute_safe,
        scope_editable,
    };
    if state.recompute_safe {
        let computed = state.recompute_used_space()?;
        let tolerance = 0.01_f32.max(used_space.abs() * 0.000_01);
        if (computed - used_space).abs() > tolerance {
            return Err(CoreError::validation(format!(
                "cargo used-space cache {used_space} does not match stack total {computed}"
            )));
        }
    }
    Ok((
        CargoAnchors {
            cargo,
            stack_container,
            stacks: anchors,
            scope_key: scope_key.to_owned(),
            stack_prefix: stack_prefix.to_owned(),
            slot_count: slots.len(),
            unlimited_stacks,
        },
        state,
    ))
}

pub(crate) fn cargo_stack_selector(
    scope_key: &str,
    stack_prefix: &str,
    ordinal: usize,
    identity: &str,
    raw_kind: &str,
    item_id: &str,
) -> String {
    let selector_material = format!("{scope_key}:{ordinal}:{identity}:{raw_kind}:{item_id}");
    opaque_id(stack_prefix, selector_material.as_bytes())
}

fn extract_colonies(
    xml: &XmlDocument,
    root: ElementId,
    save_id: &str,
    player_faction_id: &str,
) -> Result<(
    HashMap<String, ColonyAnchors>,
    IndexMap<String, ColonyState>,
)> {
    const MAX_MARKETS: usize = 20_000;

    let Some(economy_candidate) = xml.optional_unique_direct_child(root, "economy")? else {
        return Ok((HashMap::new(), IndexMap::new()));
    };
    let economy = xml.resolve(economy_candidate)?;
    let econ = xml.resolve(xml.unique_direct_child(economy, "econ")?)?;
    let markets = xml.resolve(xml.unique_direct_child(econ, "markets")?)?;
    if xml.children(markets).len() > MAX_MARKETS {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            "economy market count exceeds the configured safety limit",
        ));
    }

    let mut seen_definitions = HashSet::new();
    let mut seen_market_ids = HashSet::new();
    let mut anchors = HashMap::new();
    let mut colonies = IndexMap::new();
    for market_ref in xml.children(markets).iter().copied() {
        let market = xml.resolve(market_ref)?;
        if !seen_definitions.insert(market) {
            continue;
        }
        let owned = match xml.optional_unique_direct_child(market, "playerOwned")? {
            Some(value) => parse_bool(&xml.simple_text(value)?, "market playerOwned")?,
            None => false,
        };
        if !owned {
            continue;
        }
        let faction_id = bounded_child_text(xml, market, "factionId", 512, "faction ID")?;
        if faction_id != player_faction_id {
            continue;
        }
        let market_id = bounded_child_text(xml, market, "id", 512, "market ID")?;
        if !seen_market_ids.insert(market_id.clone()) {
            return Err(CoreError::ambiguous(format!(
                "multiple player-owned markets use ID {market_id}"
            )));
        }
        let colony_id = opaque_id("colony", format!("{save_id}:{market_id}").as_bytes());
        let (colony_anchors, colony) = extract_colony(xml, market, save_id, &colony_id)?;
        anchors.insert(colony_id.clone(), colony_anchors);
        colonies.insert(colony_id, colony);
    }
    Ok((anchors, colonies))
}

fn extract_colony(
    xml: &XmlDocument,
    market: ElementId,
    save_id: &str,
    colony_id: &str,
) -> Result<(ColonyAnchors, ColonyState)> {
    let mut warnings = Vec::new();
    let name = bounded_child_text(xml, market, "name", 512, "colony name")?;
    let faction_id = bounded_child_text(xml, market, "factionId", 512, "faction ID")?;
    let location_context = extract_location_context(xml, market, &mut warnings);
    let (storage_anchor, storage) =
        match extract_colony_storage(xml, market, save_id, colony_id, &mut warnings) {
            Ok(result) => result,
            Err(error) => {
                warnings.push(feature_warning(
                    "COLONY_STORAGE_UNAVAILABLE",
                    "Colony storage could not be indexed safely",
                    &error,
                ));
                (None, None)
            }
        };
    let (local_resources_anchor, local_resources) =
        match extract_colony_resources(xml, market, save_id, colony_id, &mut warnings) {
            Ok(result) => result,
            Err(error) => {
                warnings.push(feature_warning(
                    "COLONY_RESOURCES_UNAVAILABLE",
                    "Colony Local Resources could not be indexed safely",
                    &error,
                ));
                (None, None)
            }
        };
    Ok((
        ColonyAnchors {
            storage: storage_anchor,
            local_resources: local_resources_anchor,
        },
        ColonyState {
            name,
            faction_id,
            location_context,
            storage,
            local_resources,
            warnings,
        },
    ))
}

fn extract_colony_storage(
    xml: &XmlDocument,
    market: ElementId,
    save_id: &str,
    colony_id: &str,
    warnings: &mut Vec<Warning>,
) -> Result<(Option<CargoAnchors>, Option<CargoState>)> {
    let Some(submarkets) = xml.optional_unique_direct_child(market, "submarkets")? else {
        return Ok((None, None));
    };
    let submarkets = xml.resolve(submarkets)?;
    let mut storage_candidates = Vec::new();
    let mut seen = HashSet::new();
    for candidate in xml.children(submarkets).iter().copied() {
        let definition = xml.resolve(candidate)?;
        if seen.insert(definition) && xml.attribute(definition, "s") == Some("storage") {
            storage_candidates.push(definition);
        }
    }
    if storage_candidates.is_empty() {
        return Ok((None, None));
    }
    if storage_candidates.len() != 1 {
        warnings.push(Warning {
            code: "COLONY_STORAGE_AMBIGUOUS".into(),
            message: "Colony has multiple storage submarkets; storage is unavailable".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }
    let submarket = storage_candidates[0];
    if !unique_backlink_resolves(xml, submarket, "m", market) {
        warnings.push(Warning {
            code: "COLONY_STORAGE_BACKLINK_INVALID".into(),
            message: "Storage submarket does not link uniquely to its colony".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }
    let plugin = match xml.optional_unique_direct_child(submarket, "p")? {
        Some(plugin) => xml.resolve(plugin)?,
        None => return Ok((None, None)),
    };
    if xml.attribute(plugin, "cl") != Some("StoragePlugin")
        || !unique_backlink_resolves(xml, plugin, "m", market)
        || !unique_backlink_resolves(xml, plugin, "s", submarket)
    {
        warnings.push(Warning {
            code: "COLONY_STORAGE_PLUGIN_INVALID".into(),
            message: "Storage plugin structure is not the supported RC8 shape".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }
    let paid = xml
        .attribute(plugin, "paid")
        .map(|value| parse_bool(value, "storage paid"))
        .transpose()?
        .unwrap_or(false);
    let cargo_candidate = xml.unique_direct_child(plugin, "c")?;
    let cargo = xml.resolve(cargo_candidate)?;
    let (anchor, state) = extract_cargo(
        xml,
        cargo,
        &format!("{save_id}:{colony_id}:storage"),
        "storage-stack",
        paid,
    )?;
    Ok((Some(anchor), Some(state)))
}

fn extract_colony_resources(
    xml: &XmlDocument,
    market: ElementId,
    save_id: &str,
    colony_id: &str,
    warnings: &mut Vec<Warning>,
) -> Result<(Option<CargoAnchors>, Option<CargoState>)> {
    let Some(submarkets) = xml.optional_unique_direct_child(market, "submarkets")? else {
        return Ok((None, None));
    };
    let submarkets = xml.resolve(submarkets)?;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for candidate in xml.children(submarkets).iter().copied() {
        let definition = xml.resolve(candidate)?;
        if seen.insert(definition) && xml.attribute(definition, "s") == Some("local_resources") {
            candidates.push(definition);
        }
    }
    if candidates.is_empty() {
        return Ok((None, None));
    }
    if candidates.len() != 1 {
        warnings.push(Warning {
            code: "COLONY_RESOURCES_AMBIGUOUS".into(),
            message: "Colony has multiple Local Resources submarkets; resources are unavailable"
                .into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }

    let submarket = candidates[0];
    if !unique_backlink_resolves(xml, submarket, "m", market) {
        warnings.push(Warning {
            code: "COLONY_RESOURCES_BACKLINK_INVALID".into(),
            message: "Local Resources submarket does not link uniquely to its colony".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }
    let plugin = match xml.optional_unique_direct_child(submarket, "p")? {
        Some(plugin) => xml.resolve(plugin)?,
        None => return Ok((None, None)),
    };
    if xml.attribute(plugin, "cl") != Some("LocalResourcesSubmarketPlugin")
        || !unique_backlink_resolves(xml, plugin, "m", market)
        || !unique_backlink_resolves(xml, plugin, "s", submarket)
    {
        warnings.push(Warning {
            code: "COLONY_RESOURCES_PLUGIN_INVALID".into(),
            message: "Local Resources plugin structure is not the supported RC8 shape".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }

    let cargo = xml.resolve(xml.unique_direct_child(plugin, "c")?)?;
    let taken = xml.resolve(xml.unique_direct_child(plugin, "taken")?)?;
    let left = xml.resolve(xml.unique_direct_child(plugin, "left")?)?;
    if cargo == taken
        || cargo == left
        || taken == left
        || !is_cargo_data(xml, cargo)
        || !is_cargo_data(xml, taken)
        || !is_cargo_data(xml, left)
    {
        warnings.push(Warning {
            code: "COLONY_RESOURCES_CARGO_INVALID".into(),
            message: "Local Resources main and accounting cargo containers are not distinct RC8 CargoData objects".into(),
            acknowledgement_required: false,
        });
        return Ok((None, None));
    }

    let (anchor, mut state) = extract_cargo(
        xml,
        cargo,
        &format!("{save_id}:{colony_id}:local-resources"),
        "colony-resource-stack",
        true,
    )?;
    for stack in state.stacks.values_mut() {
        if stack.kind != InventoryKind::Resources {
            stack.structurally_editable = false;
            stack.reason = Some("Local Resources only supports commodity resource stacks".into());
        }
    }
    // CargoData carries a generic mC value, but the RC8 Local Resources plugin
    // never treats it as a player-facing capacity. Keep validating and
    // rewriting sU, while suppressing misleading overload semantics.
    state.max_space = None;
    Ok((Some(anchor), Some(state)))
}

fn is_cargo_data(xml: &XmlDocument, candidate: ElementId) -> bool {
    (xml.name(candidate) == "CargoData" || xml.attribute(candidate, "cl") == Some("CargoData"))
        && xml.direct_children_named(candidate, "s").len() == 1
}

fn extract_location_context(
    xml: &XmlDocument,
    market: ElementId,
    warnings: &mut Vec<Warning>,
) -> Option<String> {
    let primary_candidate = match xml.optional_unique_direct_child(market, "primaryEntity") {
        Ok(Some(value)) => value,
        Ok(None) => return colony_location_warning(warnings),
        Err(_) => return colony_location_warning(warnings),
    };
    let primary = match xml.resolve(primary_candidate) {
        Ok(value) => value,
        Err(_) => return colony_location_warning(warnings),
    };
    let location_candidate = match xml.optional_unique_direct_child(primary, "cL") {
        Ok(Some(value)) => value,
        Ok(None) | Err(_) => return colony_location_warning(warnings),
    };
    let location = match xml.resolve(location_candidate) {
        Ok(value) => value,
        Err(_) => return colony_location_warning(warnings),
    };
    if let Some(name) = xml
        .attribute(location, "dN")
        .or_else(|| xml.attribute(location, "bN"))
    {
        if bounded_display_value(name, 512) {
            return Some(name.to_owned());
        }
    }
    colony_location_warning(warnings)
}

fn colony_location_warning(warnings: &mut Vec<Warning>) -> Option<String> {
    warnings.push(Warning {
        code: "COLONY_LOCATION_UNAVAILABLE".into(),
        message: "Colony location name could not be read safely".into(),
        acknowledgement_required: false,
    });
    None
}

fn unique_backlink_resolves(
    xml: &XmlDocument,
    parent: ElementId,
    child_name: &str,
    expected: ElementId,
) -> bool {
    let candidates = xml.direct_children_named(parent, child_name);
    candidates.len() == 1 && xml.resolve(candidates[0]).ok() == Some(expected)
}

fn parse_nonnegative_f32(value: &str, field: &str) -> Result<f32> {
    let parsed = parse_f32(value, field)?;
    if parsed < 0.0 {
        return Err(CoreError::validation(format!("{field} is negative")));
    }
    Ok(parsed)
}

fn bounded_display_value(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn bounded_child_text(
    xml: &XmlDocument,
    parent: ElementId,
    child_name: &str,
    max_bytes: usize,
    label: &str,
) -> Result<String> {
    let value = xml.child_text(parent, child_name)?;
    if !bounded_display_value(&value, max_bytes) {
        return Err(CoreError::validation(format!(
            "{label} exceeds the display safety limit"
        )));
    }
    Ok(value)
}

fn parse_person(xml: &XmlDocument, person: ElementId) -> Result<(PersonAnchors, PersonState)> {
    let name = xml.unique_direct_child(person, "n")?;
    let stats = xml.unique_direct_child(person, "stats")?;
    let skills = xml.unique_direct_child(stats, "s")?;
    let stats_value = InternalStats {
        story_checkpoint_xp: parse_u64(xml.require_attribute(stats, "x2")?, "x2")?,
        xp: parse_u64(xml.require_attribute(stats, "xp")?, "xp")?,
        bonus_xp: parse_u64(xml.require_attribute(stats, "bx")?, "bx")?,
        deferred_bonus_xp: parse_u64(xml.require_attribute(stats, "db")?, "db")?,
        level: parse_u32(xml.require_attribute(stats, "l")?, "level")?,
        skill_points: parse_u32(xml.require_attribute(stats, "pt")?, "skill points")?,
        story_points: parse_u32(xml.require_attribute(stats, "sp")?, "story points")?,
    };
    if stats_value.level == 0 {
        return Err(CoreError::validation("person level must be at least one"));
    }
    let skill_range = xml.inner_range(skills)?;
    let skill_map = SkillJsonDocument::parse(xml.raw_bytes(skill_range))?.to_rank_map();
    Ok((
        PersonAnchors {
            person,
            name,
            stats: StatsAnchors { stats, skills },
        },
        PersonState {
            first_name: xml.require_attribute(name, "f")?.to_owned(),
            last_name: xml.require_attribute(name, "l")?.to_owned(),
            portrait: xml.require_attribute(person, "spr")?.to_owned(),
            personality: xml.attribute(person, "pid").unwrap_or("").to_owned(),
            stats: stats_value,
            skills: skill_map,
        },
    ))
}

#[derive(Debug, Clone, Default)]
struct HistoryAnchors {
    last_value: Option<ElementId>,
    positive_timestamp: Option<ElementId>,
    negative_timestamp: Option<ElementId>,
}

fn parse_reputation_histories(
    xml: &XmlDocument,
    root: ElementId,
) -> Result<HashMap<String, HistoryAnchors>> {
    let Some(mod_data) = xml.optional_unique_direct_child(root, "modAndPluginData")? else {
        return Ok(HashMap::new());
    };
    let Some(persistent) = xml.optional_unique_direct_child(mod_data, "persistentData")? else {
        return Ok(HashMap::new());
    };
    let mut shared_data = None;
    for entry in xml.direct_children_named(persistent, "e") {
        let keys = xml.direct_children_named(entry, "st");
        if keys.len() == 1 && xml.simple_text(keys[0])? == "core_CEFSSharedDataKey" {
            let candidates = xml.direct_children_named(entry, "SharedData");
            if candidates.len() != 1 || shared_data.replace(candidates[0]).is_some() {
                return Err(CoreError::ambiguous("ambiguous core shared-data entry"));
            }
        }
    }
    let Some(shared_data) = shared_data else {
        return Ok(HashMap::new());
    };
    let activity = xml.unique_direct_child(shared_data, "playerActivityTracker")?;
    let Some(tracker) = xml.optional_unique_direct_child(activity, "repChangeTracker")? else {
        return Ok(HashMap::new());
    };
    let data = xml.unique_direct_child(tracker, "repData")?;
    let mut histories = HashMap::new();
    for entry in xml.direct_children_named(data, "e") {
        let keys = xml.direct_children_named(entry, "st");
        let records = xml.direct_children_named(entry, "ReputationChangeData");
        if keys.len() != 1 || records.len() != 1 {
            return Err(CoreError::ambiguous("malformed reputation-history entry"));
        }
        let faction_id = xml.simple_text(keys[0])?;
        if histories.contains_key(&faction_id) {
            return Err(CoreError::ambiguous(format!(
                "duplicate reputation history for {faction_id}"
            )));
        }
        histories.insert(
            faction_id,
            HistoryAnchors {
                last_value: xml.optional_unique_direct_child(records[0], "lastValue")?,
                positive_timestamp: xml
                    .optional_unique_direct_child(records[0], "lastPositiveChange")?,
                negative_timestamp: xml
                    .optional_unique_direct_child(records[0], "lastNegativeChange")?,
            },
        );
    }
    Ok(histories)
}

fn has_ancestor_named(
    xml: &XmlDocument,
    mut id: ElementId,
    name: &str,
    stop_at: ElementId,
) -> bool {
    while let Some(parent) = xml.parent(id) {
        if xml.name(parent) == name {
            return true;
        }
        if parent == stop_at {
            break;
        }
        id = parent;
    }
    false
}

pub(crate) fn join_name(first: &str, last: &str) -> String {
    if last.is_empty() {
        first.to_owned()
    } else {
        format!("{first} {last}")
    }
}

fn parse_bool(value: &str, field: &str) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CoreError::validation(format!("{field} is not a boolean"))),
    }
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    let parsed: u64 = value
        .trim()
        .parse()
        .map_err(|_| CoreError::validation(format!("{field} is not a nonnegative Java long")))?;
    if parsed > i64::MAX as u64 {
        return Err(CoreError::validation(format!(
            "{field} exceeds the Java long range"
        )));
    }
    Ok(parsed)
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    let parsed: u32 = value
        .trim()
        .parse()
        .map_err(|_| CoreError::validation(format!("{field} is not a nonnegative Java int")))?;
    if parsed > i32::MAX as u32 {
        return Err(CoreError::validation(format!(
            "{field} exceeds the Java int range"
        )));
    }
    Ok(parsed)
}

fn parse_i64(value: &str, field: &str) -> Result<i64> {
    value
        .trim()
        .parse()
        .map_err(|_| CoreError::validation(format!("{field} is not an i64")))
}

fn parse_f32(value: &str, field: &str) -> Result<f32> {
    let value: f32 = value
        .trim()
        .parse()
        .map_err(|_| CoreError::validation(format!("{field} is not a float")))?;
    if !value.is_finite() {
        return Err(CoreError::validation(format!("{field} is not finite")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Developer-only smoke test for a permissioned local fixture. The path is
    /// never embedded in source and the test only opens the pair read-only.
    #[test]
    #[ignore = "requires SAVE_CORE_REAL_FIXTURE_DIR"]
    fn opens_real_fixture_from_environment_read_only() {
        let directory = std::env::var_os("SAVE_CORE_REAL_FIXTURE_DIR")
            .expect("set SAVE_CORE_REAL_FIXTURE_DIR to a local save directory");
        let opened = OpenedSave::open(
            SaveLocation::from_save_dir(directory),
            OpenOptions::default(),
        )
        .unwrap();
        assert!(!opened.snapshot().character.first_name.is_empty());
        assert!(!opened.snapshot().metadata.game_version.is_empty());
        if std::env::var_os("SAVE_CORE_EXPECT_CARGO_COLONIES").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            assert!(opened.snapshot().capabilities.inventory);
            assert!(!opened.snapshot().inventory.stacks.is_empty());
            assert!(!opened.snapshot().colonies.is_empty());
            assert!(opened
                .snapshot()
                .colonies
                .iter()
                .any(|colony| colony.storage.is_some()));
            assert!(opened.snapshot().capabilities.colony_resources);
            assert!(opened.snapshot().colonies.iter().any(|colony| colony
                .local_resources
                .as_ref()
                .is_some_and(|resources| {
                    resources
                        .stacks
                        .iter()
                        .any(|stack| stack.kind == InventoryKind::Resources)
                })));
        }
    }
}
