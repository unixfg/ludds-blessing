import type {
  ApplyResult,
  BackupSummary,
  Diagnostics,
  Edit,
  Review,
  ReviewChange,
  SaveSnapshot,
  SaveSummary,
} from "./types";

const saves: SaveSummary[] = [
  {
    id: "demo-mira",
    rootId: null,
    path: "C:\\Games\\Starsector\\saves\\save_MiraVenn_demo",
    characterName: "Mira Venn",
    characterLevel: 6,
    gameVersion: "0.98a-RC8",
    saveFileVersion: "0.6",
    saveDate: "2026-08-16 23:04 UTC",
    location: "Westernesse Star System",
    ironMode: false,
    autosave: false,
    compressed: false,
    enabledMods: [],
    compatibility: "editable",
    compatibilityReason: null,
  },
  {
    id: "demo-legacy",
    rootId: null,
    path: "D:\\Starsector\\saves\\save_Legacy_preview",
    characterName: "Archive Walker",
    characterLevel: 12,
    gameVersion: "0.97a-RC11",
    saveFileVersion: "0.5",
    saveDate: "2025-02-12 14:31 UTC",
    location: "Unknown",
    ironMode: true,
    autosave: false,
    compressed: false,
    enabledMods: ["nexerelin"],
    compatibility: "preview",
    compatibilityReason: "Only 0.98a-RC8 / save format 0.6 is writable.",
  },
];

export const demoSaves = (): SaveSummary[] => structuredClone(saves);

export const demoSnapshot = (saveId: string): SaveSnapshot => {
  const summary = saves.find((candidate) => candidate.id === saveId) ?? saves[0];
  const editable = summary.compatibility === "editable";

  return {
    sessionId: `demo-session-${saveId}`,
    saveId,
    revision: "demo-revision-1",
    summary,
    protectedLocked: summary.ironMode || summary.autosave,
    writeCapability: {
      editable,
      reason: editable ? null : summary.compatibilityReason,
    },
    progressionCapability: {
      editable: editable && summary.enabledMods.length === 0,
      reason:
        summary.enabledMods.length > 0
          ? "Progression simulation is disabled when mods are enabled."
          : null,
    },
    character: {
      firstName: summary.characterName.split(" ")[0] || "Mira",
      lastName: summary.characterName.split(" ").slice(1).join(" ") || "Venn",
      portraitId: "graphics/portraits/portrait_luddic01.png",
      portraitPath: "graphics/portraits/portrait_luddic01.png",
      credits: "82418096",
      level: summary.characterLevel,
      xp: "630904",
      skillPoints: 0,
      storyPoints: 14,
      skills: [
        { id: "field_repairs", name: "Field Repairs", group: "Industry", rank: 1, maxRank: 1, editable, reason: null, iconId: null },
        { id: "crew_training", name: "Crew Training", group: "Leadership", rank: 1, maxRank: 1, editable, reason: null, iconId: null },
        { id: "field_modulation", name: "Field Modulation", group: "Combat", rank: 1, maxRank: 2, editable, reason: null, iconId: null },
        { id: "ballistic_mastery", name: "Ballistic Mastery", group: "Combat", rank: 0, maxRank: 2, editable, reason: null, iconId: null },
        { id: "navigation", name: "Navigation", group: "Technology", rank: 0, maxRank: 1, editable, reason: null, iconId: null },
        { id: "mod_unknown", name: "mod_unknown", group: "Unknown mod", rank: 1, maxRank: 1, editable: false, reason: "No trusted local skill definition.", iconId: null },
      ],
    },
    relations: [
      { factionId: "hegemony", displayName: "Hegemony", value: 20, editable, reason: null },
      { factionId: "independent", displayName: "Independent", value: 35, editable, reason: null },
      { factionId: "luddic_church", displayName: "Luddic Church", value: 0, editable, reason: null },
      { factionId: "luddic_path", displayName: "Luddic Path", value: -65, editable, reason: null },
      { factionId: "pirates", displayName: "Pirates", value: -100, editable, reason: null },
      { factionId: "tritachyon", displayName: "Tri-Tachyon", value: -15, editable, reason: null },
    ],
    officers: [
      {
        id: "p_41833",
        name: "Elektra Mintaka",
        portraitPath: null,
        personality: "Steady",
        assignment: "ISS Immaculate Vessel",
        level: 5,
        xp: "448000",
        skillPoints: 0,
        progressionEditable: editable,
        progressionReason: null,
        skills: [
          { id: "target_analysis", name: "Target Analysis", group: "Combat", rank: 1, maxRank: 2, editable, reason: null, iconId: null },
          { id: "energy_weapon_mastery", name: "Energy Weapon Mastery", group: "Combat", rank: 2, maxRank: 2, editable, reason: null, iconId: null },
          { id: "helmsmanship", name: "Helmsmanship", group: "Combat", rank: 1, maxRank: 2, editable, reason: null, iconId: null },
        ],
      },
      {
        id: "p_81c7a",
        name: "Chaiya Malone",
        portraitPath: null,
        personality: "Steady",
        assignment: "Wyrm",
        level: 1,
        xp: "33541",
        skillPoints: 0,
        progressionEditable: editable,
        progressionReason: null,
        skills: [
          { id: "ballistic_mastery", name: "Ballistic Mastery", group: "Combat", rank: 1, maxRank: 2, editable, reason: null, iconId: null },
          { id: "field_modulation", name: "Field Modulation", group: "Combat", rank: 0, maxRank: 2, editable, reason: null, iconId: null },
        ],
      },
    ],
    catalog: {
      portraits: [
        { id: "graphics/portraits/portrait_luddic01.png", relativePath: "graphics/portraits/portrait_luddic01.png", label: "Luddic portrait 01" },
        { id: "graphics/portraits/portrait_luddic02.png", relativePath: "graphics/portraits/portrait_luddic02.png", label: "Luddic portrait 02" },
        { id: "graphics/portraits/portrait_mercenary01.png", relativePath: "graphics/portraits/portrait_mercenary01.png", label: "Mercenary portrait 01" },
      ],
      addableItems: [
        { id: "catalog-supplies", itemId: "supplies", name: "Supplies", kind: "commodity", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: true },
        { id: "catalog-food", itemId: "food", name: "Food", kind: "commodity", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: true },
        { id: "catalog-vulcan", itemId: "vulcan", name: "Vulcan Cannon", kind: "weapon", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: false },
        { id: "catalog-broadsword", itemId: "broadsword_wing", name: "Broadsword LPC", kind: "fighter_wing", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: false },
        { id: "catalog-hammerhead-bp", itemId: "hammerhead", name: "Hammerhead blueprint", kind: "ship_blueprint", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: false },
        { id: "catalog-vulcan-bp", itemId: "vulcan", name: "Vulcan Cannon blueprint", kind: "weapon_blueprint", cargoSpacePerUnit: "1", maxQuantity: "1000000", localResourcesEligible: false },
      ],
    },
    inventory: {
      usedSpace: "880",
      maxSpace: "1800",
      overloaded: false,
      editable,
      reason: editable ? null : summary.compatibilityReason,
      stacks: [
        { id: "cargo-supplies", itemId: "supplies", name: "Supplies", kind: "resources", quantity: "613", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-fuel", itemId: "fuel", name: "Fuel", kind: "resources", quantity: "420", maxQuantity: "1000000", cargoSpacePerUnit: "0.25", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-machinery", itemId: "heavy_machinery", name: "Heavy Machinery", kind: "resources", quantity: "152", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-railgun", itemId: "railgun", name: "Railgun", kind: "weapons", quantity: "2", maxQuantity: "10000", cargoSpacePerUnit: "5", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-broadsword", itemId: "broadsword_wing", name: "Broadsword Fighter Wing", kind: "fighter_wing", quantity: "1", maxQuantity: "10000", cargoSpacePerUnit: "0", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-corrupted-forge", itemId: "corrupted_nanoforge", name: "Corrupted Nanoforge", kind: "special", quantity: "1", maxQuantity: "1", cargoSpacePerUnit: "0", specialData: "pristine=false", editable, reason: editable ? null : summary.compatibilityReason },
        { id: "cargo-unknown", itemId: "mod_opaque_stack", name: "mod_opaque_stack", kind: "unknown", quantity: "4", maxQuantity: "4", cargoSpacePerUnit: "0", specialData: "opaque mod payload", editable: false, reason: "No trusted local item definition; preserved as read-only." },
      ],
    },
    colonies: [
      {
        id: "colony-asteria",
        name: "Asteria Outpost",
        locationContext: "Westernesse · Asteria",
        storage: {
          usedSpace: "1420",
          maxSpace: null,
          overloaded: false,
          editable,
          reason: editable ? null : summary.compatibilityReason,
          stacks: [
            { id: "storage-asteria-supplies", itemId: "supplies", name: "Supplies", kind: "resources", quantity: "1200", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "storage-asteria-machinery", itemId: "heavy_machinery", name: "Heavy Machinery", kind: "resources", quantity: "220", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "storage-asteria-gamma", itemId: "gamma_core", name: "Gamma Core", kind: "special", quantity: "2", maxQuantity: "1000", cargoSpacePerUnit: "0", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "storage-asteria-unknown", itemId: "mod_unknown_relic", name: "mod_unknown_relic", kind: "unknown", quantity: "1", maxQuantity: "1", cargoSpacePerUnit: "0", specialData: "unrecognized relic data", editable: false, reason: "Unknown mod storage data remains read-only." },
          ],
        },
        localResources: {
          usedSpace: "10422",
          maxSpace: null,
          overloaded: false,
          editable,
          reason: editable ? null : summary.compatibilityReason,
          stacks: [
            { id: "resources-asteria-supplies", itemId: "supplies", name: "Supplies", kind: "resources", quantity: "9375", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "resources-asteria-fuel", itemId: "fuel", name: "Fuel", kind: "resources", quantity: "15000", maxQuantity: "1000000", cargoSpacePerUnit: "0", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "resources-asteria-metals", itemId: "metals", name: "Metals", kind: "resources", quantity: "1047", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
          ],
        },
        warnings: [],
      },
      {
        id: "colony-lacaille",
        name: "Lacaille Anchorage",
        locationContext: "Lacaille Habitat",
        storage: null,
        localResources: {
          usedSpace: "850",
          maxSpace: null,
          overloaded: false,
          editable,
          reason: editable ? null : summary.compatibilityReason,
          stacks: [
            { id: "resources-lacaille-food", itemId: "food", name: "Food", kind: "resources", quantity: "500", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
            { id: "resources-lacaille-organics", itemId: "organics", name: "Organics", kind: "resources", quantity: "350", maxQuantity: "1000000", cargoSpacePerUnit: "1", specialData: null, editable, reason: editable ? null : summary.compatibilityReason },
          ],
        },
        warnings: ["No unique storage container was found; Local Resources remains available."],
      },
    ],
    warnings: editable ? [] : [summary.compatibilityReason ?? "This save is read-only."],
  };
};

const rankLabel = (rank: number): string => ["Unlearned", "Learned", "Elite"][rank] ?? String(rank);

const editLabel = (edit: Edit, snapshot: SaveSnapshot): ReviewChange => {
  switch (edit.type) {
    case "set_player_name":
      return { key: "player.name", section: "Character", label: "Character name", before: `${snapshot.character.firstName} ${snapshot.character.lastName}`.trim(), after: `${edit.firstName} ${edit.lastName}`.trim(), derived: null };
    case "set_player_portrait":
      return { key: "player.portrait", section: "Character", label: "Portrait", before: snapshot.character.portraitId ?? "None", after: edit.portraitId, derived: null };
    case "set_credits":
      return { key: "player.credits", section: "Character", label: "Credits", before: snapshot.character.credits, after: edit.value, derived: null };
    case "grant_player_xp":
      return { key: "player.xp", section: "Character", label: "Grant XP", before: snapshot.character.xp, after: `${snapshot.character.xp} + ${edit.amount}`, derived: true };
    case "raise_player_to_level":
      return { key: "player.level", section: "Character", label: "Raise to level", before: String(snapshot.character.level), after: String(edit.level), derived: true };
    case "set_player_points":
      return { key: "player.points", section: "Character", label: "Unspent points", before: `${snapshot.character.skillPoints} skill / ${snapshot.character.storyPoints} story`, after: `${edit.skillPoints} skill / ${edit.storyPoints} story`, derived: null };
    case "set_player_skill": {
      const skill = snapshot.character.skills.find((candidate) => candidate.id === edit.skillId);
      return { key: `player.skill.${edit.skillId}`, section: "Character", label: skill?.name ?? edit.skillId, before: rankLabel(skill?.rank ?? 0), after: rankLabel(edit.rank), derived: null };
    }
    case "set_relation": {
      const relation = snapshot.relations.find((candidate) => candidate.factionId === edit.factionId);
      return { key: `relation.${edit.factionId}`, section: "Reputation", label: relation?.displayName ?? edit.factionId, before: String(relation?.value ?? 0), after: String(edit.value), derived: null };
    }
    case "grant_officer_xp":
      return { key: `officer.${edit.officerId}.xp`, section: "Officers", label: "Grant officer XP", before: snapshot.officers.find((candidate) => candidate.id === edit.officerId)?.xp ?? "Unknown", after: `+${edit.amount}`, derived: true };
    case "raise_officer_to_level":
      return { key: `officer.${edit.officerId}.level`, section: "Officers", label: "Raise officer level", before: String(snapshot.officers.find((candidate) => candidate.id === edit.officerId)?.level ?? "Unknown"), after: String(edit.level), derived: true };
    case "set_officer_points":
      return { key: `officer.${edit.officerId}.points`, section: "Officers", label: "Officer unspent points", before: String(snapshot.officers.find((candidate) => candidate.id === edit.officerId)?.skillPoints ?? "Unknown"), after: String(edit.skillPoints), derived: null };
    case "set_officer_skill": {
      const skill = snapshot.officers.find((candidate) => candidate.id === edit.officerId)?.skills.find((candidate) => candidate.id === edit.skillId);
      return { key: `officer.${edit.officerId}.skill.${edit.skillId}`, section: "Officers", label: skill?.name ?? edit.skillId, before: rankLabel(skill?.rank ?? 0), after: rankLabel(edit.rank), derived: null };
    }
    case "set_inventory_quantity": {
      const stack = snapshot.inventory?.stacks.find((candidate) => candidate.id === edit.stackId);
      return { key: `inventory.${edit.stackId}`, section: "Inventory", label: `${stack?.name ?? stack?.itemId ?? edit.stackId} quantity`, before: stack?.quantity ?? "Unknown", after: edit.quantity, derived: null };
    }
    case "set_storage_stack_quantity": {
      const colony = snapshot.colonies.find((candidate) => candidate.id === edit.colonyId);
      const stack = colony?.storage?.stacks.find((candidate) => candidate.id === edit.stackId);
      return { key: `colony.${edit.colonyId}.storage.${edit.stackId}`, section: "Colonies", label: `${colony?.name ?? edit.colonyId} · ${stack?.name ?? stack?.itemId ?? edit.stackId}`, before: stack?.quantity ?? "Unknown", after: edit.quantity, derived: null };
    }
    case "set_colony_resource_quantity": {
      const colony = snapshot.colonies.find((candidate) => candidate.id === edit.colonyId);
      const stack = colony?.localResources?.stacks.find((candidate) => candidate.id === edit.stackId);
      return { key: `colony.${edit.colonyId}.localResources.${edit.stackId}`, section: "Colonies", label: `${colony?.name ?? edit.colonyId} · ${stack?.name ?? stack?.itemId ?? edit.stackId} local resource quantity`, before: stack?.quantity ?? "Unknown", after: edit.quantity, derived: null };
    }
    case "add_storage_item": {
      const colony = snapshot.colonies.find((candidate) => candidate.id === edit.colonyId);
      const item = snapshot.catalog.addableItems.find((candidate) => candidate.id === edit.catalogItemId);
      return { key: `colony.${edit.colonyId}.storage.add.${edit.catalogItemId}`, section: "Colonies", label: `${colony?.name ?? edit.colonyId} · Add ${item?.name ?? "item"}`, before: "Not present", after: edit.quantity, derived: null };
    }
    case "add_colony_resource": {
      const colony = snapshot.colonies.find((candidate) => candidate.id === edit.colonyId);
      const item = snapshot.catalog.addableItems.find((candidate) => candidate.id === edit.catalogItemId);
      return { key: `colony.${edit.colonyId}.localResources.add.${edit.catalogItemId}`, section: "Colonies", label: `${colony?.name ?? edit.colonyId} · Add ${item?.name ?? "commodity"} to Local Resources`, before: "Not present", after: edit.quantity, derived: null };
    }
  }
};

export const demoReview = (revision: string, edits: Edit[]): Review => ({
  reviewId: `demo-review-${Date.now()}`,
  revision,
  changes: edits.map((edit) => editLabel(edit, demoSnapshot("demo-mira"))),
  warnings: edits.some((edit) => edit.type.includes("skill"))
    ? ["Skill changes do not automatically spend or refund point pools."]
    : [],
  errors: [],
  canApply: edits.length > 0,
});

export const demoApply = (): ApplyResult => ({
  saveId: "demo-mira",
  backupId: "demo-backup",
  targetPath: saves[0].path,
  campaignHash: "demo-campaign-hash",
  descriptorHash: "demo-descriptor-hash",
  message: "Demo review completed. No files were written outside the Tauri app.",
});

export const demoBackups = (): BackupSummary[] => [
  {
    id: "backup-20260816",
    saveId: "demo-mira",
    createdAt: "2026-08-16 23:18 UTC",
    reason: "Before character edit",
    sizeBytes: "10359250",
    gameVersion: "0.98a-RC8",
    pinned: false,
  },
];

export const demoDiagnostics = (): Diagnostics => ({
    appVersion: "0.2.2-demo",
  os: "Browser preview",
  entries: [
    "Tauri runtime not detected; no filesystem commands are available.",
    "Paths and save contents are excluded from diagnostics by default.",
  ],
});
