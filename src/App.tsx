import {
  ArchiveRestore,
  ArrowLeft,
  BadgeCheck,
  BookOpenCheck,
  Building2,
  Boxes,
  ChevronRight,
  CircleAlert,
  CircleDollarSign,
  ClipboardCopy,
  DatabaseBackup,
  FileClock,
  FileSearch,
  FolderOpen,
  Gauge,
  HardDrive,
  Info,
  LockKeyhole,
  Orbit,
  PackageOpen,
  PanelLeftClose,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Settings,
  Shield,
  ShieldAlert,
  Sparkles,
  Star,
  Trash2,
  UserRound,
  UsersRound,
  X,
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { api, isDesktopRuntime } from "./api";
import { ColoniesPage, InventoryPage } from "./InventorySections";
import type {
  ApiFailure,
  ApplyMode,
  BackupSummary,
  Diagnostics,
  DiscoveryResult,
  Edit,
  GameSettingsProfile,
  GameSettingsSnapshot,
  GameSettingsValues,
  OfficerView,
  PortraitView,
  RecoveryItem,
  RecoveryState,
  RelationView,
  Review,
  SaveSnapshot,
  SaveSummary,
  SkillView,
} from "./types";
import { isApiFailure } from "./types";

type Page = "saves" | "character" | "inventory" | "reputation" | "officers" | "colonies" | "review" | "backups" | "settings";

type NavItem = { id: Page; label: string; icon: typeof Save };

const LIBRARY_NAV_ITEMS: NavItem[] = [
  { id: "saves", label: "Saves", icon: HardDrive },
  { id: "settings", label: "Settings", icon: Settings },
];

const EDITOR_NAV_ITEMS: NavItem[] = [
  { id: "character", label: "Character", icon: UserRound },
  { id: "inventory", label: "Inventory", icon: PackageOpen },
  { id: "reputation", label: "Reputation", icon: Shield },
  { id: "officers", label: "Officers", icon: UsersRound },
  { id: "colonies", label: "Colonies", icon: Building2 },
  { id: "review", label: "Review", icon: BookOpenCheck },
  { id: "backups", label: "Backups", icon: DatabaseBackup },
];

const RECOVERY_NAV_ITEMS = EDITOR_NAV_ITEMS.filter(({ id }) => id === "review");

const editKey = (edit: Edit): string => {
  switch (edit.type) {
    case "set_player_name":
      return "player.name";
    case "set_player_portrait":
      return "player.portrait";
    case "set_credits":
      return "player.credits";
    case "grant_player_xp":
    case "raise_player_to_level":
      return "player.progression";
    case "set_player_points":
      return "player.points";
    case "set_player_skill":
      return `player.skill.${edit.skillId}`;
    case "set_relation":
      return `relation.${edit.factionId}`;
    case "grant_officer_xp":
    case "raise_officer_to_level":
      return `officer.${edit.officerId}.progression`;
    case "set_officer_points":
      return `officer.${edit.officerId}.points`;
    case "set_officer_skill":
      return `officer.${edit.officerId}.skill.${edit.skillId}`;
    case "set_inventory_quantity":
      return `inventory.${edit.stackId}`;
    case "set_storage_stack_quantity":
      return `colony.${edit.colonyId}.storage.${edit.stackId}`;
    case "set_colony_resource_quantity":
      return `colony.${edit.colonyId}.localResources.${edit.stackId}`;
    case "add_storage_item":
      return `colony.${edit.colonyId}.storage.add.${edit.catalogItemId}`;
    case "add_colony_resource":
      return `colony.${edit.colonyId}.localResources.add.${edit.catalogItemId}`;
  }
};

const humanBytes = (raw: string): string => {
  const value = Number(raw);
  if (!Number.isFinite(value)) return raw;
  const units = ["B", "KB", "MB", "GB"];
  let index = 0;
  let scaled = value;
  while (scaled >= 1024 && index < units.length - 1) {
    scaled /= 1024;
    index += 1;
  }
  return `${scaled.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
};

const formatIntegerString = (raw: string): string => {
  const match = /^([+-]?)(\d+)$/.exec(raw.trim());
  if (!match) return raw;
  return `${match[1]}${match[2].replace(/\B(?=(\d{3})+(?!\d))/g, ",")}`;
};

const factionTone = (factionId: string): number => Math.abs(
  [...factionId].reduce((total, char) => total + char.charCodeAt(0), 0),
) % 12;

const displayError = (error: unknown): ApiFailure => {
  if (isApiFailure(error)) return error;
  const message = error instanceof Error
    ? error.message
    : typeof error === "string"
      ? error
      : "An unexpected error occurred.";
  return { code: "UNEXPECTED", message, retryable: false, detail: null, diskChanged: null };
};

function CompatibilityBadge({ save }: { save: SaveSummary }) {
  const labels = {
    editable: "Editable",
    preview: "Read-only preview",
    locked: "Temporarily locked",
    unreadable: "Unreadable",
  };
  return <span className={`badge badge--${save.compatibility}`}>{labels[save.compatibility]}</span>;
}

function ChangedMarker({ onReset }: { onReset: () => void }) {
  return (
    <span className="changed-marker">
      Changed
      <button className="icon-button icon-button--small" type="button" onClick={onReset} aria-label="Reset field">
        <RotateCcw size={13} aria-hidden="true" />
      </button>
    </span>
  );
}

function PortraitThumbnail({ sessionId, portrait }: { sessionId: string; portrait: PortraitView }) {
  const [source, setSource] = useState<string>();
  const placeholder = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (source || !isDesktopRuntime()) return;
    let cancelled = false;
    let observer: IntersectionObserver | undefined;
    const load = () => {
      observer?.disconnect();
      void api.loadPortrait(sessionId, portrait.id).then((payload) => {
        if (!cancelled && payload.dataBase64) setSource(`data:${payload.mimeType};base64,${payload.dataBase64}`);
      }).catch(() => undefined);
    };
    if (placeholder.current && "IntersectionObserver" in window) {
      observer = new IntersectionObserver((entries) => {
        if (entries.some((entry) => entry.isIntersecting)) load();
      }, { rootMargin: "80px" });
      observer.observe(placeholder.current);
    } else {
      load();
    }
    return () => { cancelled = true; observer?.disconnect(); };
  }, [portrait.id, sessionId, source]);

  return source
    ? <img src={source} alt="" />
    : <span ref={placeholder} aria-hidden="true">{portrait.label.slice(0, 2).toUpperCase()}</span>;
}

function SectionHeader({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: React.ReactNode }) {
  return (
    <header className="section-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h2>{title}</h2>
        <p>{description}</p>
      </div>
      {action}
    </header>
  );
}

function Notice({ tone = "info", title, children }: { tone?: "info" | "warning" | "danger" | "success"; title: string; children: React.ReactNode }) {
  const Icon = tone === "danger" ? CircleAlert : tone === "warning" ? ShieldAlert : tone === "success" ? BadgeCheck : Info;
  return (
    <div className={`notice notice--${tone}`} role={tone === "danger" ? "alert" : "status"}>
      <Icon size={19} aria-hidden="true" />
      <div>
        <strong>{title}</strong>
        <div>{children}</div>
      </div>
    </div>
  );
}

function EmptyState({ icon: Icon, title, children }: { icon: typeof Save; title: string; children: React.ReactNode }) {
  return (
    <div className="empty-state">
      <span className="empty-state__orbit" aria-hidden="true"><Icon size={30} /></span>
      <h3>{title}</h3>
      <p>{children}</p>
    </div>
  );
}

function SkillControl({ skill, rank, disabled, onChange }: { skill: SkillView; rank: number; disabled: boolean; onChange: (rank: number) => void }) {
  const options = ["Unlearned", "Learned", "Elite"].slice(0, skill.maxRank + 1);
  const groupName = useId();
  return (
    <div className={`skill-row ${disabled ? "skill-row--disabled" : ""}`}>
      <div className="skill-row__identity">
        <span className="skill-glyph" aria-hidden="true">{skill.name.slice(0, 1).toUpperCase()}</span>
        <div>
          <strong>{skill.name}</strong>
          <span>{skill.group} · {skill.id}</span>
          {!skill.editable && skill.reason ? <small>{skill.reason}</small> : null}
        </div>
      </div>
      <div className="segmented" role="radiogroup" aria-label={`${skill.name} rank`}>
        {options.map((label, optionRank) => (
          <label key={label}>
            <input
              className="segmented__input"
              type="radio"
              name={groupName}
              checked={rank === optionRank}
              onChange={() => onChange(optionRank)}
              disabled={disabled || !skill.editable}
            />
            <span className="segmented__option">
              {optionRank === 2 ? <Star size={13} aria-hidden="true" /> : null}
              {label}
            </span>
          </label>
        ))}
      </div>
    </div>
  );
}

function SavesPage({ saves, busy, onRefresh, onChooseRoot, onOpen }: { saves: SaveSummary[]; busy: boolean; onRefresh: () => void; onChooseRoot: () => void; onOpen: (id: string) => void }) {
  return (
    <div className="page-stack">
      <SectionHeader
        eyebrow="Save library"
        title="Campaign records"
        description="Opening a save refreshes discovery first. Refresh also checks each verified installation’s configured save folder plus bounded platform-standard locations. Unsupported saves remain available as read-only records."
        action={
          <div className="button-row">
            <button className="button button--secondary" type="button" onClick={onRefresh} disabled={busy}>
              <RefreshCw size={16} className={busy ? "spin" : ""} aria-hidden="true" /> Refresh
            </button>
            <button className="button" type="button" onClick={onChooseRoot}>
              <FolderOpen size={16} aria-hidden="true" /> Choose folder
            </button>
          </div>
        }
      />

      {!isDesktopRuntime() ? (
        <Notice title="Browser preview">
          This preview uses synthetic data. Filesystem and write commands become available only inside the signed desktop boundary.
        </Notice>
      ) : null}

      {saves.length === 0 ? (
        <EmptyState icon={FileSearch} title="No save folders found">
          Refresh checks the common locations without crawling your computer. If your saves live elsewhere, choose a Starsector installation, saves folder, or individual save directory.
        </EmptyState>
      ) : (
        <div className="save-grid" aria-busy={busy}>
          {saves.map((save) => (
            <article className="save-card" key={save.id}>
              <div className="save-card__topline">
                <CompatibilityBadge save={save} />
                <span>{save.autosave ? "Autosave" : save.ironMode ? "Iron Mode" : "Manual save"}</span>
              </div>
              <div className="save-card__identity">
                <span className="monogram" aria-hidden="true">{save.characterName.split(/\s+/).map((part) => part[0]).join("").slice(0, 2)}</span>
                <div>
                  <h3>{save.characterName}</h3>
                  <p>Level {save.characterLevel} · {save.location || "Location unavailable"}</p>
                </div>
              </div>
              <dl className="metadata-grid">
                <div><dt>Game</dt><dd>{save.gameVersion}</dd></div>
                <div><dt>Format</dt><dd>{save.saveFileVersion}</dd></div>
                <div><dt>Saved</dt><dd>{save.saveDate}</dd></div>
                <div><dt>Mods</dt><dd>{save.enabledMods.length}</dd></div>
              </dl>
              {save.compatibilityReason ? <p className="card-warning">{save.compatibilityReason}</p> : null}
              <button
                className="button button--card"
                type="button"
                disabled={save.compressed || save.compatibility === "unreadable"}
                title={save.compressed ? "Compressed campaign data cannot be opened; descriptor metadata remains visible on this card." : undefined}
                onClick={() => onOpen(save.id)}
              >
                {save.compressed ? "Descriptor metadata only" : save.compatibility === "unreadable" ? "Preview unavailable" : save.compatibility === "editable" ? "Open editor" : "Open preview"}
                <ChevronRight size={17} aria-hidden="true" />
              </button>
              <span className="save-card__path" title={save.path}>{save.path}</span>
            </article>
          ))}
        </div>
      )}
    </div>
  );
}

function CharacterPage({ snapshot, draft, upsert, reset }: { snapshot: SaveSnapshot; draft: Edit[]; upsert: (edit: Edit) => void; reset: (key: string) => void }) {
  const disabled = !snapshot.writeCapability.editable || snapshot.protectedLocked;
  const nameEdit = draft.find((edit): edit is Extract<Edit, { type: "set_player_name" }> => edit.type === "set_player_name");
  const portraitEdit = draft.find((edit): edit is Extract<Edit, { type: "set_player_portrait" }> => edit.type === "set_player_portrait");
  const creditsEdit = draft.find((edit): edit is Extract<Edit, { type: "set_credits" }> => edit.type === "set_credits");
  const pointsEdit = draft.find((edit): edit is Extract<Edit, { type: "set_player_points" }> => edit.type === "set_player_points");
  const progressionEdit = draft.find((edit) => edit.type === "grant_player_xp" || edit.type === "raise_player_to_level");
  const firstName = nameEdit?.firstName ?? snapshot.character.firstName;
  const lastName = nameEdit?.lastName ?? snapshot.character.lastName;

  const skillRank = (skill: SkillView) => {
    const edit = draft.find((candidate): candidate is Extract<Edit, { type: "set_player_skill" }> => candidate.type === "set_player_skill" && candidate.skillId === skill.id);
    return edit?.rank ?? skill.rank;
  };

  const setName = (nextFirst: string, nextLast: string) => {
    if (nextFirst === snapshot.character.firstName && nextLast === snapshot.character.lastName) reset("player.name");
    else upsert({ type: "set_player_name", firstName: nextFirst, lastName: nextLast });
  };

  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Character record" title={`${snapshot.character.firstName} ${snapshot.character.lastName}`} description="Identity, progression, points, and learned skills. No changes reach disk until review." />
      {!snapshot.progressionCapability.editable ? <Notice tone="warning" title="Progression simulation unavailable">{snapshot.progressionCapability.reason}</Notice> : null}

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Identity</p><h3>Name and portrait</h3></div>{nameEdit ? <ChangedMarker onReset={() => reset("player.name")} /> : null}</div>
        <div className="form-grid form-grid--identity">
          <label>First name<input value={firstName} maxLength={64} disabled={disabled} onChange={(event) => setName(event.target.value, lastName)} /></label>
          <label>Last name<input value={lastName} maxLength={64} disabled={disabled} onChange={(event) => setName(firstName, event.target.value)} /></label>
          <div className="combined-name"><span>Combined name</span><strong>{`${firstName} ${lastName}`.trim() || "Name required"}</strong></div>
        </div>
        <div className="portrait-strip" aria-label="Portrait choices">
          {snapshot.catalog.portraits.length === 0 ? <p className="muted">Choose a valid game installation to load portrait choices.</p> : snapshot.catalog.portraits.map((portrait) => {
            const selected = (portraitEdit?.portraitId ?? snapshot.character.portraitId) === portrait.id;
            return (
              <button type="button" className={`portrait-option ${selected ? "is-selected" : ""}`} key={portrait.id} aria-pressed={selected} disabled={disabled} onClick={() => selected && portrait.id === snapshot.character.portraitId ? reset("player.portrait") : upsert({ type: "set_player_portrait", portraitId: portrait.id })}>
                <PortraitThumbnail sessionId={snapshot.sessionId} portrait={portrait} />
                <small>{portrait.label}</small>
              </button>
            );
          })}
        </div>
      </section>

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Economy</p><h3>Credits</h3></div>{creditsEdit ? <ChangedMarker onReset={() => reset("player.credits")} /> : null}</div>
        <div className="metric-edit">
          <CircleDollarSign size={22} aria-hidden="true" />
          <label><span>Current balance</span><input inputMode="numeric" value={creditsEdit?.value ?? snapshot.character.credits} disabled={disabled} onChange={(event) => {
            const value = event.target.value.replace(/[^0-9.]/g, "");
            if (value === snapshot.character.credits) reset("player.credits"); else upsert({ type: "set_credits", value });
          }} /></label>
          <p>Stored by Starsector as a single-precision value. Any rounding is shown during review.</p>
        </div>
      </section>

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Progression</p><h3>Level and experience</h3></div>{progressionEdit ? <ChangedMarker onReset={() => reset("player.progression")} /> : null}</div>
        <div className="stat-band">
          <div><span>Level</span><strong>{snapshot.character.level}</strong></div>
          <div><span>Total XP</span><strong>{formatIntegerString(snapshot.character.xp)}</strong></div>
          <div><span>Skill points</span><strong>{pointsEdit?.skillPoints ?? snapshot.character.skillPoints}</strong></div>
          <div><span>Story points</span><strong>{pointsEdit?.storyPoints ?? snapshot.character.storyPoints}</strong></div>
        </div>
        <div className="form-grid">
          <label>Grant XP<input type="number" min="1" step="1" placeholder="Amount to add" disabled={disabled || !snapshot.progressionCapability.editable} value={progressionEdit?.type === "grant_player_xp" ? progressionEdit.amount : ""} onChange={(event) => event.target.value ? upsert({ type: "grant_player_xp", amount: event.target.value }) : reset("player.progression")} /></label>
          <label>Raise to level<input type="number" min={snapshot.character.level + 1} max="15" placeholder={`${snapshot.character.level + 1}–15`} disabled={disabled || !snapshot.progressionCapability.editable} value={progressionEdit?.type === "raise_player_to_level" ? progressionEdit.level : ""} onChange={(event) => event.target.value ? upsert({ type: "raise_player_to_level", level: Number(event.target.value) }) : reset("player.progression")} /></label>
          <label>Unspent skill points<input type="number" min="0" max="2147483647" disabled={disabled} value={pointsEdit?.skillPoints ?? snapshot.character.skillPoints} onChange={(event) => upsert({ type: "set_player_points", skillPoints: Number(event.target.value), storyPoints: pointsEdit?.storyPoints ?? snapshot.character.storyPoints })} /></label>
          <label>Unspent story points<input type="number" min="0" max="2147483647" disabled={disabled} value={pointsEdit?.storyPoints ?? snapshot.character.storyPoints} onChange={(event) => upsert({ type: "set_player_points", skillPoints: pointsEdit?.skillPoints ?? snapshot.character.skillPoints, storyPoints: Number(event.target.value) })} /></label>
        </div>
        <p className="helper-text"><Gauge size={15} aria-hidden="true" /> XP grants simulate RC8 story checkpoints and point awards. Reductions are intentionally unavailable.</p>
      </section>

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Skills</p><h3>Learned abilities</h3></div><span className="panel-count">{snapshot.character.skills.length}</span></div>
        <Notice title="Point pools remain explicit">Changing a skill does not spend or refund skill or story points. Unusual totals are surfaced during review.</Notice>
        <div className="skill-list">
          {snapshot.character.skills.map((skill) => <SkillControl key={skill.id} skill={skill} rank={skillRank(skill)} disabled={disabled} onChange={(rank) => rank === skill.rank ? reset(`player.skill.${skill.id}`) : upsert({ type: "set_player_skill", skillId: skill.id, rank })} />)}
        </div>
      </section>
    </div>
  );
}

function ReputationPage({ snapshot, draft, upsert, reset }: { snapshot: SaveSnapshot; draft: Edit[]; upsert: (edit: Edit) => void; reset: (key: string) => void }) {
  const [query, setQuery] = useState("");
  const [modifiedOnly, setModifiedOnly] = useState(false);
  const disabled = !snapshot.writeCapability.editable || snapshot.protectedLocked;
  const relationEdit = (relation: RelationView) => draft.find((edit): edit is Extract<Edit, { type: "set_relation" }> => edit.type === "set_relation" && edit.factionId === relation.factionId);
  const visible = snapshot.relations.filter((relation) => {
    const matches = `${relation.displayName} ${relation.factionId}`.toLowerCase().includes(query.toLowerCase());
    return matches && (!modifiedOnly || Boolean(relationEdit(relation)));
  });

  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Diplomatic ledger" title="Faction reputation" description="Only existing player relationships are editable; shared graph references remain untouched." />
      <section className="panel">
        <div className="toolbar">
          <label className="search-field"><Search size={16} aria-hidden="true" /><input placeholder="Search faction or ID" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
          <label className="toggle"><input type="checkbox" checked={modifiedOnly} onChange={(event) => setModifiedOnly(event.target.checked)} /><span>Modified only</span></label>
        </div>
        <div className="relation-list">
          {visible.map((relation) => {
            const edit = relationEdit(relation);
            const value = edit?.value ?? relation.value;
            return (
              <div className="relation-row" key={relation.factionId}>
                <span className={`faction-mark faction-mark--tone-${factionTone(relation.factionId)}`} aria-hidden="true">{relation.displayName.slice(0, 1)}</span>
                <div className="relation-identity"><strong>{relation.displayName}</strong><span>{relation.factionId}</span></div>
                <input className="relation-slider" aria-label={`${relation.displayName} reputation`} type="range" min="-100" max="100" step="1" value={value} disabled={disabled || !relation.editable} onChange={(event) => upsert({ type: "set_relation", factionId: relation.factionId, value: Number(event.target.value) })} />
                <input className="relation-number" aria-label={`${relation.displayName} exact reputation`} type="number" min="-100" max="100" step="0.1" value={value} disabled={disabled || !relation.editable} onChange={(event) => upsert({ type: "set_relation", factionId: relation.factionId, value: Number(event.target.value) })} />
                <span className={`standing standing--${value <= -50 ? "hostile" : value >= 50 ? "favorable" : "neutral"}`}>{value <= -50 ? "Hostile" : value >= 50 ? "Favorable" : "Neutral"}</span>
                {edit ? <button className="icon-button" type="button" onClick={() => reset(`relation.${relation.factionId}`)} aria-label={`Reset ${relation.displayName}`}><RotateCcw size={15} /></button> : <span className="row-spacer" />}
              </div>
            );
          })}
          {visible.length === 0 ? <EmptyState icon={Shield} title="No matching factions">Adjust the search or show all relationships.</EmptyState> : null}
        </div>
      </section>
    </div>
  );
}

function OfficersPage({ snapshot, draft, upsert, reset }: { snapshot: SaveSnapshot; draft: Edit[]; upsert: (edit: Edit) => void; reset: (key: string) => void }) {
  const [selectedId, setSelectedId] = useState(snapshot.officers[0]?.id ?? "");
  const officer = snapshot.officers.find((candidate) => candidate.id === selectedId) ?? snapshot.officers[0];
  if (!officer) return <EmptyState icon={UsersRound} title="No officers found">This save has no entries in the player fleet’s authoritative officer roster.</EmptyState>;
  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Personnel roster" title="Officers" description="Existing officer progression and skills are editable. Identity, personality, and assignment remain contextual." />
      <div className="split-layout">
        <aside className="roster officer-roster" aria-label="Officer roster">
          {snapshot.officers.map((candidate) => (
            <button type="button" key={candidate.id} className={candidate.id === officer.id ? "is-selected" : ""} aria-pressed={candidate.id === officer.id} onClick={() => setSelectedId(candidate.id)}>
              <span className="monogram monogram--small" aria-hidden="true">{candidate.name.split(/\s+/).map((part) => part[0]).join("").slice(0, 2)}</span>
              <span><strong>{candidate.name}</strong><small>Level {candidate.level} · {candidate.assignment || "Unassigned"}</small></span>
              <ChevronRight size={16} aria-hidden="true" />
            </button>
          ))}
        </aside>
        <OfficerDetail snapshot={snapshot} officer={officer} draft={draft} upsert={upsert} reset={reset} />
      </div>
    </div>
  );
}

function OfficerDetail({ snapshot, officer, draft, upsert, reset }: { snapshot: SaveSnapshot; officer: OfficerView; draft: Edit[]; upsert: (edit: Edit) => void; reset: (key: string) => void }) {
  const disabled = !snapshot.writeCapability.editable || snapshot.protectedLocked;
  const progression = draft.find((edit) => (edit.type === "grant_officer_xp" || edit.type === "raise_officer_to_level") && edit.officerId === officer.id);
  const points = draft.find((edit): edit is Extract<Edit, { type: "set_officer_points" }> => edit.type === "set_officer_points" && edit.officerId === officer.id);
  const rank = (skill: SkillView) => draft.find((edit): edit is Extract<Edit, { type: "set_officer_skill" }> => edit.type === "set_officer_skill" && edit.officerId === officer.id && edit.skillId === skill.id)?.rank ?? skill.rank;
  const editableSkills = officer.skills.filter((skill) => skill.editable);
  const setAllSkillRanks = (requestedRank: number) => {
    editableSkills.forEach((skill) => {
      const nextRank = Math.min(requestedRank, skill.maxRank);
      const key = `officer.${officer.id}.skill.${skill.id}`;
      if (nextRank === skill.rank) reset(key);
      else upsert({ type: "set_officer_skill", officerId: officer.id, skillId: skill.id, rank: nextRank });
    });
  };
  return (
    <section className="panel officer-detail">
      <div className="officer-hero">
        <span className="monogram" aria-hidden="true">{officer.name.split(/\s+/).map((part) => part[0]).join("").slice(0, 2)}</span>
        <div><p className="eyebrow">{officer.personality}</p><h3>{officer.name}</h3><p>{officer.assignment || "Unassigned reserve officer"}</p></div>
      </div>
      {!officer.progressionEditable ? <Notice tone="warning" title="Progression unavailable">{officer.progressionReason || "This officer’s progression rules are not verified."}</Notice> : null}
      <div className="stat-band stat-band--compact">
        <div><span>Level</span><strong>{officer.level}</strong></div>
        <div><span>XP</span><strong>{formatIntegerString(officer.xp)}</strong></div>
        <div><span>Unspent</span><strong>{points?.skillPoints ?? officer.skillPoints}</strong></div>
      </div>
      <div className="form-grid">
        <label>Grant XP<input type="number" min="1" placeholder="Amount to add" value={progression?.type === "grant_officer_xp" ? progression.amount : ""} disabled={disabled || !officer.progressionEditable} onChange={(event) => event.target.value ? upsert({ type: "grant_officer_xp", officerId: officer.id, amount: event.target.value }) : reset(`officer.${officer.id}.progression`)} /></label>
        <label>Raise to level<input type="number" min={officer.level + 1} placeholder={`>${officer.level}`} value={progression?.type === "raise_officer_to_level" ? progression.level : ""} disabled={disabled || !officer.progressionEditable} onChange={(event) => event.target.value ? upsert({ type: "raise_officer_to_level", officerId: officer.id, level: Number(event.target.value) }) : reset(`officer.${officer.id}.progression`)} /></label>
        <label>Unspent points<input type="number" min="0" value={points?.skillPoints ?? officer.skillPoints} disabled={disabled} onChange={(event) => upsert({ type: "set_officer_points", officerId: officer.id, skillPoints: Number(event.target.value) })} /></label>
      </div>
      <div className="subsection-title"><div><p className="eyebrow">Known skills</p><h4>Officer abilities</h4></div><span>{officer.skills.length}</span></div>
      <div className="officer-skill-toolbar">
        <div className="officer-skill-toolbar__copy">
          <strong>Set every editable ability</strong>
          <span>Read-only skills stay unchanged. Elite uses each skill’s highest supported rank.</span>
        </div>
        <div className="officer-skill-toolbar__actions" role="group" aria-label={`${officer.name} bulk skill ranks`}>
          <button className="button button--secondary" type="button" disabled={disabled || editableSkills.length === 0} onClick={() => setAllSkillRanks(0)}>Make all Unlearned</button>
          <button className="button button--secondary" type="button" disabled={disabled || editableSkills.length === 0} onClick={() => setAllSkillRanks(1)}>Make all Learned</button>
          <button className="button button--secondary" type="button" disabled={disabled || editableSkills.length === 0} onClick={() => setAllSkillRanks(2)}>Make all Elite</button>
        </div>
      </div>
      <div className="skill-list">
        {officer.skills.map((skill) => <SkillControl key={skill.id} skill={skill} rank={rank(skill)} disabled={disabled} onChange={(nextRank) => nextRank === skill.rank ? reset(`officer.${officer.id}.skill.${skill.id}`) : upsert({ type: "set_officer_skill", officerId: officer.id, skillId: skill.id, rank: nextRank })} />)}
      </div>
    </section>
  );
}

function ReviewPage({ review, draftCount, busy, warningAccepted, setWarningAccepted, onPrepare, onApply, onSaveCopy, onDiscard, isRestore }: { review: Review | null; draftCount: number; busy: boolean; warningAccepted: boolean; setWarningAccepted: (value: boolean) => void; onPrepare: () => void; onApply: () => void; onSaveCopy: () => void; onDiscard: () => void; isRestore: boolean }) {
  const grouped = useMemo(() => {
    const result = new Map<string, Review["changes"]>();
    review?.changes.forEach((change) => result.set(change.section, [...(result.get(change.section) ?? []), change]));
    return result;
  }, [review]);
  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Write boundary" title={isRestore ? "Review backup restore" : "Review staged changes"} description="The native core verifies source hashes, patch spans, XML structure, and semantic results before this review becomes applicable." action={!isRestore ? <button className="button button--secondary" type="button" disabled={draftCount === 0 || busy} onClick={onPrepare}><RefreshCw size={16} className={busy ? "spin" : ""} /> Rebuild review</button> : undefined} />
      <Notice title="Game activity check">
        Starsector may remain open when this target save is not currently loaded. Apply rechecks game activity and blocks every possibly active save. Keep only one Starsector instance open, and do not load or switch to this target while Apply or Restore is running.
      </Notice>
      {!review && draftCount === 0 ? <EmptyState icon={BookOpenCheck} title="No staged changes">Edit a supported character, inventory, reputation, officer, storage, or Local Resources field to build a review.</EmptyState> : !review ? <div className="center-action"><button className="button" type="button" onClick={onPrepare} disabled={busy}><BookOpenCheck size={17} /> Prepare secure review</button></div> : (
        <>
          {review.errors.map((error) => <Notice tone="danger" title="Validation error" key={error}>{error}</Notice>)}
          {review.warnings.map((warning) => <Notice tone="warning" title="Review warning" key={warning}>{warning}</Notice>)}
          {[...grouped.entries()].map(([section, changes]) => (
            <section className="panel diff-panel" key={section}>
              <div className="panel__heading"><h3>{section}</h3><span className="panel-count">{changes.length}</span></div>
              <div className="diff-list">
                {changes.map((change) => <div className="diff-row" key={change.key}><div><strong>{change.label}</strong>{change.derived ? <span className="derived-tag">Derived</span> : null}</div><span>{change.before}</span><ChevronRight size={16} aria-hidden="true" /><span>{change.after}</span></div>)}
              </div>
            </section>
          ))}
          {review.warnings.length > 0 ? <label className="warning-ack"><input type="checkbox" checked={warningAccepted} onChange={(event) => setWarningAccepted(event.target.checked)} /><span>I reviewed the warnings and still want to apply these changes.</span></label> : null}
          <div className="commit-bar">
            <div><strong>{review.changes.length} verified changes</strong><span>A separate app-owned backup is created first.</span></div>
            <button className="button button--ghost" type="button" onClick={onDiscard}><Trash2 size={16} /> {isRestore ? "Cancel restore" : "Discard draft"}</button>
            {!isRestore ? <button className="button button--secondary" type="button" onClick={onSaveCopy} disabled={!review.canApply || review.errors.length > 0 || (review.warnings.length > 0 && !warningAccepted) || busy}><ClipboardCopy size={16} /> Save a copy</button> : null}
            <button className="button" type="button" onClick={onApply} disabled={!review.canApply || review.errors.length > 0 || (review.warnings.length > 0 && !warningAccepted) || busy}><DatabaseBackup size={16} /> {isRestore ? "Create backup & restore" : "Create backup & apply"}</button>
          </div>
        </>
      )}
    </div>
  );
}

function BackupsPage({ snapshot, backups, busy, onRefresh, onRestore }: { snapshot: SaveSnapshot; backups: BackupSummary[]; busy: boolean; onRefresh: () => void; onRestore: (backup: BackupSummary) => void }) {
  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Recovery archive" title="App-owned backups" description="These byte-identical save pairs live outside Starsector’s save tree and never replace the game’s own .bak files." action={<button className="button button--secondary" type="button" onClick={onRefresh}><RefreshCw size={16} className={busy ? "spin" : ""} /> Refresh</button>} />
      {backups.length === 0 ? <EmptyState icon={DatabaseBackup} title="No backups yet">The first protected unlock or successful apply creates a backup for {snapshot.summary.characterName}.</EmptyState> : <div className="backup-list">
        {backups.map((backup) => <article className="backup-card" key={backup.id}><span className="backup-card__icon"><ArchiveRestore size={20} /></span><div><strong>{backup.createdAt}</strong><span>{backup.reason} · {backup.gameVersion}</span><small>{humanBytes(backup.sizeBytes)} {backup.pinned ? "· Pinned" : ""}</small></div><button className="button button--secondary" type="button" onClick={() => onRestore(backup)} disabled={busy}><FileClock size={15} /> Review restore</button></article>)}
      </div>}
    </div>
  );
}

const GAME_SETTING_FIELDS: Array<{
  key: keyof GameSettingsValues;
  label: string;
  min: number;
  max: number;
  help: string;
}> = [
  { key: "playerMaxLevel", label: "Player maximum level", min: 1, max: 100, help: "The game’s player level ceiling." },
  { key: "skillPointsPerLevel", label: "Skill points per level", min: 0, max: 10, help: "Along with maximum level, this controls the normal skill-point budget." },
  { key: "storyPointsPerLevel", label: "Story points per level", min: 0, max: 100, help: "Story-point award rate for future progression." },
  { key: "officerMaxLevel", label: "Officer maximum level", min: 1, max: 100, help: "The game’s officer level ceiling." },
  { key: "officerMaxEliteSkills", label: "Officer elite-skill limit", min: 0, max: 100, help: "Cannot exceed the officer maximum level." },
];

type SettingsPageProps = {
  diagnostics: Diagnostics | null;
  rootPath: string;
  setRootPath: (value: string) => void;
  refreshToken: number;
  onRegisterRoot: () => Promise<void>;
  onForgetRoot: (rootId: string) => Promise<void>;
  onDiagnostics: () => Promise<void>;
  onToast: (message: string) => void;
  onDirtyChange: (dirty: boolean) => void;
};

function SettingsPage({ diagnostics, rootPath, setRootPath, refreshToken, onRegisterRoot, onForgetRoot, onDiagnostics, onToast, onDirtyChange }: SettingsPageProps) {
  const [discovery, setDiscovery] = useState<DiscoveryResult | null>(null);
  const [profiles, setProfiles] = useState<GameSettingsProfile[]>([]);
  const [selectedInstallationId, setSelectedInstallationId] = useState("");
  const [selectedProfileId, setSelectedProfileId] = useState("builtin-vanilla-rc8");
  const [profileName, setProfileName] = useState("My campaign rules");
  const [settingsSnapshot, setSettingsSnapshot] = useState<GameSettingsSnapshot | null>(null);
  const [settingsValues, setSettingsValues] = useState<GameSettingsValues | null>(null);
  const [settingsBusy, setSettingsBusy] = useState(false);
  const [settingsError, setSettingsError] = useState<ApiFailure | null>(null);

  const refreshConfiguration = useCallback(async () => {
    setSettingsBusy(true);
    setSettingsError(null);
    try {
      const [nextDiscovery, nextProfiles] = await Promise.all([
        api.discoverInstallations(),
        api.listGameSettingsProfiles(),
      ]);
      setDiscovery(nextDiscovery);
      setProfiles(nextProfiles);
      setSelectedInstallationId((current) => nextDiscovery.installations.some((item) => item.installationId === current)
        ? current
        : nextDiscovery.installations[0]?.installationId ?? "");
      setSelectedProfileId((current) => nextProfiles.some((profile) => profile.profileId === current)
        ? current
        : nextProfiles[0]?.profileId ?? "");
    } catch (caught) {
      setSettingsError(displayError(caught));
    } finally {
      setSettingsBusy(false);
    }
  }, []);

  const loadSettings = useCallback(async (installationId: string) => {
    if (!installationId) {
      setSettingsSnapshot(null);
      setSettingsValues(null);
      return;
    }
    setSettingsBusy(true);
    setSettingsError(null);
    try {
      const loaded = await api.loadGameSettings(installationId);
      setSettingsSnapshot(loaded);
      setSettingsValues({ ...loaded.values });
    } catch (caught) {
      setSettingsSnapshot(null);
      setSettingsValues(null);
      setSettingsError(displayError(caught));
    } finally {
      setSettingsBusy(false);
    }
  }, []);

  useEffect(() => { void refreshConfiguration(); }, [refreshConfiguration, refreshToken]);
  useEffect(() => { void loadSettings(selectedInstallationId); }, [loadSettings, selectedInstallationId, refreshToken]);

  const selectedProfile = profiles.find((profile) => profile.profileId === selectedProfileId) ?? null;
  const settingsValuesValid = Boolean(settingsValues
    && GAME_SETTING_FIELDS.every(({ key, min, max }) => Number.isSafeInteger(settingsValues[key]) && settingsValues[key] >= min && settingsValues[key] <= max)
    && settingsValues.officerMaxEliteSkills <= settingsValues.officerMaxLevel);
  const settingsChanged = Boolean(
    settingsSnapshot
      && settingsValues
      && GAME_SETTING_FIELDS.some(({ key }) => settingsSnapshot.values[key] !== settingsValues[key]),
  );

  useEffect(() => {
    onDirtyChange(settingsChanged);
  }, [onDirtyChange, settingsChanged]);
  useEffect(() => () => onDirtyChange(false), [onDirtyChange]);

  const useSelectedProfile = () => {
    if (!selectedProfile) return;
    setSettingsValues({ ...selectedProfile.values });
    if (!selectedProfile.builtIn) setProfileName(selectedProfile.name);
  };

  const saveProfile = async (updateExisting: boolean) => {
    if (!settingsValues) return;
    setSettingsBusy(true);
    setSettingsError(null);
    try {
      const profile = await api.saveGameSettingsProfile(
        updateExisting && selectedProfile && !selectedProfile.builtIn ? selectedProfile.profileId : null,
        profileName,
        settingsValues,
      );
      setProfiles((current) => [...current.filter((item) => item.profileId !== profile.profileId), profile]
        .sort((left, right) => Number(right.builtIn) - Number(left.builtIn) || left.name.localeCompare(right.name)));
      setSelectedProfileId(profile.profileId);
      setProfileName(profile.name);
      onToast(updateExisting ? "Game settings profile updated." : "Game settings profile saved.");
    } catch (caught) {
      setSettingsError(displayError(caught));
    } finally {
      setSettingsBusy(false);
    }
  };

  const deleteProfile = async () => {
    if (!selectedProfile || selectedProfile.builtIn) return;
    if (!window.confirm(`Delete the local profile “${selectedProfile.name}”? This does not change Starsector.`)) return;
    setSettingsBusy(true);
    setSettingsError(null);
    try {
      await api.deleteGameSettingsProfile(selectedProfile.profileId);
      const remaining = profiles.filter((profile) => profile.profileId !== selectedProfile.profileId);
      setProfiles(remaining);
      setSelectedProfileId(remaining[0]?.profileId ?? "");
      setProfileName("My campaign rules");
      onToast("Game settings profile deleted. Starsector was not changed.");
    } catch (caught) {
      setSettingsError(displayError(caught));
    } finally {
      setSettingsBusy(false);
    }
  };

  const applySettings = async () => {
    if (!settingsSnapshot || !settingsValues || !settingsChanged) return;
    if (!window.confirm("Close Starsector before continuing. Create an external backup and apply these installation-wide game settings?")) return;
    setSettingsBusy(true);
    setSettingsError(null);
    try {
      const result = await api.applyGameSettings(
        settingsSnapshot.installationId,
        settingsSnapshot.revision,
        settingsValues,
      );
      setSettingsSnapshot(result.snapshot);
      setSettingsValues({ ...result.snapshot.values });
      onToast(result.message);
    } catch (caught) {
      setSettingsError(displayError(caught));
    } finally {
      setSettingsBusy(false);
    }
  };

  const copyDiagnostics = async () => {
    if (!diagnostics) return;
    await navigator.clipboard.writeText([`Ludd’s Blessing ${diagnostics.appVersion}`, diagnostics.os, ...diagnostics.entries].join("\n"));
  };
  return (
    <div className="page-stack">
      <SectionHeader eyebrow="Local configuration" title="Settings and diagnostics" description="No telemetry, network requests, game-class loading, or automatic update service is enabled." action={<button className="button button--secondary" type="button" onClick={() => void refreshConfiguration()} disabled={settingsBusy}><RefreshCw size={16} className={settingsBusy ? "spin" : ""} /> Refresh settings</button>} />
      {settingsError ? <Notice tone="danger" title={settingsError.code}>{settingsError.message}{settingsError.detail ? ` ${settingsError.detail}` : ""}</Notice> : null}

      <section className="panel game-settings-panel">
        <div className="panel__heading"><div><p className="eyebrow">Installation-wide rules</p><h3>Game Settings Profiles</h3></div><Settings size={21} aria-hidden="true" /></div>
        <Notice tone="warning" title="Close Starsector before applying">These values change the selected local installation, not one save. Ludd’s Blessing makes a separate app-owned backup and verifies the exact settings revision before replacement. Restart the game afterward.</Notice>
        <Notice title="Save-editor progression safeguard">XP and target-level simulation is disabled only for the affected progression track when its RC8 rules differ: player level, point awards, story awards, and max-level bonus XP for the character; officer level and XP requirements for officers. The officer elite-skill limit alone does not disable XP editing. Saves without a verified installation association fail closed. Reopen an existing save after changing these settings.</Notice>
        {discovery && discovery.installations.length === 0 ? <EmptyState icon={Settings} title="No verified installation found">Register or install Starsector in a supported local location, then refresh. Raw paths are never accepted by the settings writer.</EmptyState> : (
          <>
            <div className="settings-selectors">
              <label htmlFor="settings-installation">Starsector installation<select id="settings-installation" value={selectedInstallationId} disabled={settingsBusy || !discovery?.installations.length} onChange={(event) => setSelectedInstallationId(event.target.value)}>{discovery?.installations.map((installation) => <option key={installation.installationId} value={installation.installationId}>{installation.displayName} — {installation.displayPath}</option>)}</select></label>
              <label htmlFor="settings-profile">Profile<select id="settings-profile" value={selectedProfileId} disabled={settingsBusy || profiles.length === 0} onChange={(event) => setSelectedProfileId(event.target.value)}>{profiles.map((profile) => <option key={profile.profileId} value={profile.profileId}>{profile.name}{profile.builtIn ? " (built in)" : ""}</option>)}</select></label>
              <button className="button button--secondary settings-use-profile" type="button" onClick={useSelectedProfile} disabled={settingsBusy || !selectedProfile || !settingsValues}><BookOpenCheck size={16} /> Use profile values</button>
            </div>
            {settingsSnapshot && settingsValues ? (
              <>
                <div className="settings-file-status"><span><strong>{settingsSnapshot.displayName}</strong><small title={settingsSnapshot.displayPath}>{settingsSnapshot.displayPath}</small></span><span className={`badge ${settingsSnapshot.writable ? "badge--editable" : "badge--locked"}`}>{settingsSnapshot.writable ? "Writable" : "Read-only"}</span><button className="button button--ghost" type="button" onClick={() => void loadSettings(settingsSnapshot.installationId)} disabled={settingsBusy}><RefreshCw size={15} /> Reload file</button></div>
                <div className="form-grid game-settings-grid">
                  {GAME_SETTING_FIELDS.map((field) => <div className="game-settings-field" key={field.key}><label htmlFor={`game-setting-${field.key}`}>{field.label}</label><input id={`game-setting-${field.key}`} aria-describedby={`game-setting-${field.key}-help`} type="number" min={field.min} max={field.max} step="1" value={settingsValues[field.key]} disabled={settingsBusy || !settingsSnapshot.writable} onChange={(event) => setSettingsValues((current) => current ? { ...current, [field.key]: Number(event.target.value) } : current)} /><small id={`game-setting-${field.key}-help`}>{field.help}</small></div>)}
                </div>
                {!settingsValuesValid ? <Notice tone="danger" title="Check game setting values">Use whole numbers within the shown ranges. Officer elite skills cannot exceed officer maximum level.</Notice> : null}
                <p className="helper-text"><Info size={15} aria-hidden="true" /> Player maximum level and skill points per level together control the normal skill-point budget. Existing characters are not retroactively rebalanced by this editor.</p>
                <div className="settings-commit"><span>{settingsChanged ? "Unsaved installation changes" : "Matches the loaded settings file"}</span><button className="button" type="button" onClick={() => void applySettings()} disabled={settingsBusy || !settingsSnapshot.writable || !settingsChanged || !settingsValuesValid}><DatabaseBackup size={16} /> Create backup & apply settings</button></div>
              </>
            ) : discovery?.installations.length ? <p className="muted">Select a verified installation to load its supported game settings.</p> : null}
          </>
        )}
      </section>

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Reusable local presets</p><h3>Profile library</h3></div><span className="panel-count">{profiles.length}</span></div>
        <p className="muted">Profiles live in Ludd’s Blessing app data. Saving or deleting one does not edit Starsector until you explicitly apply its values above.</p>
        <div className="profile-editor"><label htmlFor="profile-name">Profile name<input id="profile-name" value={profileName} maxLength={64} onChange={(event) => setProfileName(event.target.value)} /></label><div className="button-row"><button className="button button--secondary" type="button" disabled={settingsBusy || !settingsValues || !settingsValuesValid || !profileName.trim()} onClick={() => void saveProfile(false)}><Save size={16} /> Save as new</button><button className="button button--secondary" type="button" disabled={settingsBusy || !settingsValues || !settingsValuesValid || !selectedProfile || selectedProfile.builtIn || !profileName.trim()} onClick={() => void saveProfile(true)}>Update selected</button><button className="button button--ghost" type="button" disabled={settingsBusy || !selectedProfile || selectedProfile.builtIn} onClick={() => void deleteProfile()}><Trash2 size={16} /> Delete selected</button></div></div>
      </section>

      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Discovery</p><h3>Remembered save folders</h3></div><Boxes size={21} aria-hidden="true" /></div>
        <p className="muted">Automatic discovery reads verified installations’ configured save paths and bounded platform-standard locations. Register an installation, saves folder, or individual save folder for anything else; the app never crawls an entire drive or home directory.</p>
        <div className="path-entry"><label className="sr-only" htmlFor="manual-save-root">Starsector installation or saves folder</label><input id="manual-save-root" value={rootPath} onChange={(event) => setRootPath(event.target.value)} placeholder="Starsector installation or saves folder" /><button className="button" type="button" onClick={() => void onRegisterRoot().then(refreshConfiguration)} disabled={!rootPath.trim()}><FolderOpen size={16} /> Register</button></div>
        <div className="remembered-roots" aria-live="polite">
          {discovery?.registeredRoots.map((root) => <div className="remembered-root" key={root.rootId}><span><strong>{root.displayName}</strong><small title={root.displayPath}>{root.displayPath}</small></span><span className={`badge ${root.available ? "badge--editable" : "badge--unreadable"}`}>{root.available ? (root.writable ? "Available" : "Read-only") : "Unavailable"}</span><button className="button button--ghost" type="button" onClick={() => void onForgetRoot(root.rootId).then(refreshConfiguration)}><Trash2 size={15} /> Forget</button></div>)}
          {discovery && discovery.registeredRoots.length === 0 ? <p className="muted">No manual folders are remembered. Automatic roots remain internal and are refreshed from the platform.</p> : null}
        </div>
      </section>
      <section className="panel">
        <div className="panel__heading"><div><p className="eyebrow">Privacy-safe report</p><h3>Diagnostics</h3></div><button className="button button--secondary" type="button" onClick={() => void onDiagnostics()}><FileSearch size={16} /> Generate</button></div>
        {diagnostics ? <div className="diagnostics"><div><strong>Version {diagnostics.appVersion}</strong><span>{diagnostics.os}</span></div><ul>{diagnostics.entries.map((entry) => <li key={entry}>{entry}</li>)}</ul><button className="button button--ghost" type="button" onClick={copyDiagnostics}><ClipboardCopy size={15} /> Copy report</button></div> : <p className="muted">Reports exclude save contents and redact user-specific path components by default.</p>}
      </section>
      <section className="panel about-panel"><Orbit size={30} aria-hidden="true" /><div><h3>Ludd’s Blessing 0.2.2</h3><p>An independent, local-first community tool. Starsector is created by Fractal Softworks. No Starsector assets are bundled.</p></div></section>
    </div>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>("saves");
  const [navCollapsed, setNavCollapsed] = useState(false);
  const [saves, setSaves] = useState<SaveSummary[]>([]);
  const [snapshot, setSnapshot] = useState<SaveSnapshot | null>(null);
  const [draft, setDraft] = useState<Edit[]>([]);
  const [review, setReview] = useState<Review | null>(null);
  const [backups, setBackups] = useState<BackupSummary[]>([]);
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [rootPath, setRootPath] = useState("");
  const [settingsRefreshToken, setSettingsRefreshToken] = useState(0);
  const [settingsDirty, setSettingsDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiFailure | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [warningAccepted, setWarningAccepted] = useState(false);
  const [reviewMode, setReviewMode] = useState<"edit" | "restore" | "recovery">("edit");
  const [recovery, setRecovery] = useState<RecoveryState | null>(null);
  const activeSessionId = useRef<string | null>(null);
  const activeReviewId = useRef<string | null>(null);
  const mainContent = useRef<HTMLElement>(null);
  const didMountPage = useRef(false);

  useEffect(() => {
    if (!didMountPage.current) {
      didMountPage.current = true;
      return;
    }
    document.documentElement.scrollTop = 0;
    document.body.scrollTop = 0;
    mainContent.current?.focus({ preventScroll: true });
  }, [page]);

  useEffect(() => {
    const previous = activeSessionId.current;
    const current = snapshot?.sessionId ?? null;
    activeSessionId.current = current;
    if (previous && previous !== current) {
      void api.closeSession(previous).catch(() => undefined);
    }
  }, [snapshot?.sessionId]);

  useEffect(() => {
    const previous = activeReviewId.current;
    const current = review?.reviewId ?? null;
    activeReviewId.current = current;
    if (previous && previous !== current) {
      void api.discardReview(previous).catch(() => undefined);
    }
  }, [review?.reviewId]);

  useEffect(() => () => {
    if (activeReviewId.current) {
      void api.discardReview(activeReviewId.current).catch(() => undefined);
    }
    if (activeSessionId.current) {
      void api.closeSession(activeSessionId.current).catch(() => undefined);
    }
  }, []);

  const refreshSaves = useCallback(async () => {
    setBusy(true);
    setError(null);
    try { setSaves(await api.scanSaves()); } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  }, []);

  useEffect(() => { void refreshSaves(); }, [refreshSaves]);

  useEffect(() => {
    void api.startupRecoveryState().then((state) => {
      setRecovery(state);
    }).catch((caught) => setError(displayError(caught)));
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: () => void = () => undefined;
    void api.onPathsDropped(async (paths) => {
      if (paths.length === 0) return;
      setBusy(true); setError(null);
      try {
        for (const path of paths) await api.registerRoot(path);
        await refreshSaves();
        setToast(paths.length === 1 ? "Dropped save location registered." : `${paths.length} dropped save locations registered.`);
      } catch (caught) {
        setError(displayError(caught));
      } finally {
        setBusy(false);
      }
    }).then((cleanup) => {
      if (disposed) cleanup(); else unlisten = cleanup;
    }).catch((caught) => setError(displayError(caught)));
    return () => { disposed = true; unlisten(); };
  }, [refreshSaves]);

  useEffect(() => {
    if (draft.length === 0 && !settingsDirty) return;
    const protectDraft = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", protectDraft);
    return () => window.removeEventListener("beforeunload", protectDraft);
  }, [draft.length, settingsDirty]);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 5000);
    return () => window.clearTimeout(timer);
  }, [toast]);

  const chooseRoot = async () => {
    setError(null);
    try { await api.chooseAndRegisterRoot(); await refreshSaves(); } catch (caught) { setError(displayError(caught)); }
  };

  const openSave = async (saveId: string) => {
    if (draft.length > 0 && snapshot?.saveId !== saveId && !window.confirm("Discard the current staged changes and open another save?")) return;
    setBusy(true); setError(null);
    try {
      const refreshedSaves = await api.scanSaves();
      setSaves(refreshedSaves);
      const refreshedSave = refreshedSaves.find((save) => save.id === saveId);
      if (!refreshedSave) {
        setToast("That save is no longer available. The library was refreshed.");
        return;
      }
      if (refreshedSave.compressed || refreshedSave.compatibility === "unreadable") {
        setToast("That save changed and can no longer be opened. The library was refreshed.");
        return;
      }
      const opened = await api.openSave(saveId);
      setSnapshot(opened); setDraft([]); setReview(null); setReviewMode("edit"); setBackups([]); setPage("character");
      if (!opened.writeCapability.editable) setToast("Opened in read-only preview mode.");
    } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  };

  const closeEditor = () => {
    if ((draft.length > 0 || review !== null)
      && !window.confirm("Close this save and discard its staged changes or pending review? No pending changes have been written.")) return;
    setSnapshot(null);
    setDraft([]);
    setReview(null);
    setReviewMode("edit");
    setWarningAccepted(false);
    setBackups([]);
    setPage("saves");
    void refreshSaves();
  };

  const refreshApplication = async () => {
    if ((draft.length > 0 || review !== null || settingsDirty)
      && !window.confirm("Refresh local data and discard staged save edits, reviews, or unapplied game settings? No pending changes have been written yet.")) return;
    setBusy(true);
    setError(null);
    try {
      const [refreshedSaves, recoveryState] = await Promise.all([
        api.scanSaves(),
        api.startupRecoveryState(),
      ]);
      setSaves(refreshedSaves);
      setRecovery(recoveryState);
      if (snapshot) {
        if (refreshedSaves.some((save) => save.id === snapshot.saveId)) {
          const opened = await api.openSave(snapshot.saveId);
          setSnapshot(opened);
          setBackups([]);
          if (page === "review") setPage("character");
        } else {
          setSnapshot(null);
          setBackups([]);
          setPage("saves");
          setToast("The previously open save is no longer available. The library was refreshed.");
        }
      }
      setDraft([]);
      setReview(null);
      setReviewMode("edit");
      setWarningAccepted(false);
      setSettingsRefreshToken((value) => value + 1);
      if (!snapshot || refreshedSaves.some((save) => save.id === snapshot.saveId)) {
        setToast(snapshot ? "Library and open save refreshed." : "Save library refreshed.");
      }
    } catch (caught) {
      setError(displayError(caught));
    } finally {
      setBusy(false);
    }
  };

  const upsertEdit = (edit: Edit) => {
    const key = editKey(edit);
    setDraft((current) => [...current.filter((candidate) => editKey(candidate) !== key), edit]);
    setReview(null); setReviewMode("edit"); setWarningAccepted(false);
  };
  const resetEdit = (key: string) => {
    setDraft((current) => current.filter((edit) => editKey(edit) !== key));
    setReview(null);
    setWarningAccepted(false);
  };
  const resetEditPrefix = (prefix: string) => {
    setDraft((current) => current.filter((edit) => !editKey(edit).startsWith(prefix)));
    setReview(null);
    setWarningAccepted(false);
  };
  const discardDraft = () => { setDraft([]); setReview(null); setWarningAccepted(false); setToast("Draft discarded. No files were changed."); };

  const unlockProtected = async () => {
    if (!snapshot) return;
    const acknowledged = window.confirm("Unlock this protected save for this session? A pinned backup will be created before editing is enabled.");
    if (!acknowledged) return;
    setBusy(true);
    try { setSnapshot(await api.unlockProtectedSave(snapshot.sessionId)); setToast("Protected save unlocked; baseline backup created."); } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  };

  const prepareReview = async () => {
    if (!snapshot || draft.length === 0) return;
    setBusy(true); setError(null);
    try { setReview(await api.prepareReview(snapshot.sessionId, snapshot.revision, draft)); setReviewMode("edit"); setPage("review"); } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  };

  const apply = async (mode: ApplyMode) => {
    if (!review) return;
    const completedMode = reviewMode;
    setBusy(true); setError(null);
    try {
      const result = completedMode !== "edit"
        ? await api.applyRestore(review.reviewId, warningAccepted)
        : await api.applyReview(review.reviewId, mode, warningAccepted);
      setToast(result.message || `Save committed to ${result.targetPath}`);
      setDraft([]); setReview(null); setWarningAccepted(false);
      if (completedMode === "recovery") {
        setRecovery(await api.startupRecoveryState());
        setSnapshot(null);
        await refreshSaves();
        setPage("saves");
      } else if (completedMode === "edit") {
        setSnapshot(null);
        setBackups([]);
        setPage("saves");
        await refreshSaves();
      } else {
        if (snapshot) setSnapshot(await api.openSave(snapshot.saveId));
        setPage("backups");
      }
      setReviewMode("edit");
    } catch (caught) {
      const failure = displayError(caught);
      setError(failure);
      // Reviews are immutable and single-use at the native boundary, including
      // failed attempts. Keep semantic edit drafts, but never offer a consumed
      // review for retry.
      setReview(null);
      setWarningAccepted(false);

      if (failure.code === "RECOVERY_REQUIRED") {
        try { setRecovery(await api.startupRecoveryState()); } catch { /* Preserve the transaction error. */ }
        setReviewMode("edit");
        setPage("saves");
      } else {
        if ((failure.code === "STALE_SAVE" || failure.diskChanged) && snapshot) {
          try {
            setSnapshot(await api.openSave(snapshot.saveId));
          } catch (reopenFailure) {
            setSnapshot(null);
            setError(displayError(reopenFailure));
            setReviewMode("edit");
            setPage("saves");
            return;
          }
        }
        setReviewMode("edit");
        setPage(completedMode === "edit" ? "review" : completedMode === "restore" ? "backups" : "saves");
      }
    } finally { setBusy(false); }
  };

  const saveCopy = async () => {
    const targetRoot = await api.chooseCopyRoot();
    if (targetRoot) await apply({ type: "save_copy", targetRoot });
  };

  const loadBackups = async () => {
    if (!snapshot) return;
    setBusy(true);
    try { setBackups(await api.listBackups(snapshot.saveId)); } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  };

  useEffect(() => { if (page === "backups" && snapshot) void loadBackups(); }, [page, snapshot?.saveId]); // eslint-disable-line react-hooks/exhaustive-deps

  const restoreBackup = async (backup: BackupSummary) => {
    if (!snapshot) return;
    setBusy(true);
    try { setReview(await api.prepareRestore(snapshot.sessionId, backup.id)); setReviewMode("restore"); setWarningAccepted(false); setPage("review"); } catch (caught) { setError(displayError(caught)); } finally { setBusy(false); }
  };

  const recoverInterruptedWrite = async (item: RecoveryItem) => {
    setBusy(true); setError(null);
    try {
      setReview(await api.prepareRestore(item.transactionId, item.transactionId));
      setReviewMode("recovery");
      setWarningAccepted(false);
      setPage("review");
    } catch (caught) {
      setError(displayError(caught));
    } finally {
      setBusy(false);
    }
  };

  const registerManualRoot = async () => {
    try { await api.registerRoot(rootPath.trim()); setRootPath(""); await refreshSaves(); setToast("Save root registered."); } catch (caught) { setError(displayError(caught)); }
  };

  const forgetManualRoot = async (rootId: string) => {
    try { await api.forgetRoot(rootId); await refreshSaves(); setToast("Remembered save root removed."); } catch (caught) { setError(displayError(caught)); }
  };

  const generateDiagnostics = async () => {
    try { setDiagnostics(await api.exportDiagnostics()); } catch (caught) { setError(displayError(caught)); }
  };

  const navigate = (target: Page) => {
    if (!snapshot && !["saves", "settings"].includes(target)) { setToast("Open a save before using this section."); return; }
    if (page === "settings" && target !== "settings" && settingsDirty
      && !window.confirm("Discard the unapplied game settings changes? Starsector has not been modified.")) return;
    if (target === "review" && draft.length > 0 && !review) void prepareReview();
    else setPage(target);
  };

  const libraryContext = page === "saves" || page === "settings";
  const navItems = libraryContext
    ? LIBRARY_NAV_ITEMS
    : snapshot
      ? EDITOR_NAV_ITEMS
      : RECOVERY_NAV_ITEMS;

  return (
    <div className={`app-shell ${navCollapsed ? "app-shell--collapsed" : ""}`}>
      <a className="skip-link" href="#main-content">Skip to main content</a>
      <aside className="sidebar">
        <div className="brand"><span className="brand__mark" aria-hidden="true"><Orbit /></span><div><strong>Ludd’s Blessing</strong><span>Campaign ledger</span></div></div>
        <nav aria-label={libraryContext ? "Save library navigation" : "Save editor navigation"}>{navItems.map(({ id, label, icon: Icon }) => <button type="button" key={id} className={page === id ? "is-active" : ""} aria-label={label} aria-current={page === id ? "page" : undefined} onClick={() => navigate(id)}><Icon size={18} aria-hidden="true" /><span>{label}</span>{id === "review" && draft.length > 0 ? <b>{draft.length}</b> : null}</button>)}</nav>
        <button className="sidebar__collapse" type="button" onClick={() => setNavCollapsed((value) => !value)} aria-label={navCollapsed ? "Expand navigation" : "Collapse navigation"}><PanelLeftClose size={17} /><span>Collapse</span></button>
      </aside>

      <div className="workspace">
        <header className="topbar">
          <div>{snapshot ? <><strong>{snapshot.summary.characterName}</strong><span title={snapshot.summary.path}>{snapshot.summary.gameVersion} · {snapshot.summary.enabledMods.length} mods</span></> : <><strong>Local save editor</strong><span>No campaign open</span></>}</div>
          <div className="topbar__actions">
            {snapshot ? <button className="button button--secondary" type="button" onClick={closeEditor} disabled={busy}><ArrowLeft size={15} aria-hidden="true" /> Close editor</button> : null}
            <button className="button button--secondary" type="button" onClick={() => void refreshApplication()} disabled={busy} aria-label="Refresh save data">
              <RefreshCw size={15} className={busy ? "spin" : ""} aria-hidden="true" /> Refresh
            </button>
            {snapshot ? <CompatibilityBadge save={snapshot.summary} /> : null}
            {snapshot?.protectedLocked ? <button className="button button--warning" type="button" onClick={unlockProtected} disabled={busy}><LockKeyhole size={15} /> Unlock protected save</button> : null}
            {draft.length > 0 ? <button className="button" type="button" onClick={() => void prepareReview()} disabled={busy}><Sparkles size={15} /> Review {draft.length} changes</button> : null}
          </div>
        </header>

        <main id="main-content" ref={mainContent} tabIndex={-1}>
          {recovery?.status === "recovery_required" ? (
            <section className="recovery-banner" role="alert" aria-labelledby="recovery-title">
              <ShieldAlert size={22} aria-hidden="true" />
              <div>
                <strong id="recovery-title">Recovery required before another write</strong>
                <p>An interrupted replacement has a verified editor backup. Review the affected transaction and restore it before continuing.</p>
                <div className="recovery-list">
                  {recovery.items.map((item) => (
                    <div key={item.transactionId}>
                      <span>{item.summary}<small>{item.lastCompletedPhase}</small></span>
                      <button className="button button--warning" type="button" disabled={busy} onClick={() => void recoverInterruptedWrite(item)}>
                        <ArchiveRestore size={15} aria-hidden="true" /> Review recovery
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            </section>
          ) : null}
          {error ? <div className="global-error" role="alert"><CircleAlert size={20} /><div><strong>{error.code}</strong><p>{error.message}</p>{error.detail ? <small>{error.detail}</small> : null}</div><button className="icon-button" type="button" onClick={() => setError(null)} aria-label="Dismiss error"><X size={17} /></button></div> : null}
          {page === "saves" ? <SavesPage saves={saves} busy={busy} onRefresh={() => void refreshSaves()} onChooseRoot={() => void chooseRoot()} onOpen={(id) => void openSave(id)} /> : null}
          {page === "character" && snapshot ? <CharacterPage snapshot={snapshot} draft={draft} upsert={upsertEdit} reset={resetEdit} /> : null}
          {page === "inventory" && snapshot ? <InventoryPage snapshot={snapshot} draft={draft} upsert={upsertEdit} reset={resetEdit} resetPrefix={resetEditPrefix} /> : null}
          {page === "reputation" && snapshot ? <ReputationPage snapshot={snapshot} draft={draft} upsert={upsertEdit} reset={resetEdit} /> : null}
          {page === "officers" && snapshot ? <OfficersPage snapshot={snapshot} draft={draft} upsert={upsertEdit} reset={resetEdit} /> : null}
          {page === "colonies" && snapshot ? <ColoniesPage snapshot={snapshot} draft={draft} upsert={upsertEdit} reset={resetEdit} resetPrefix={resetEditPrefix} /> : null}
          {page === "review" ? <ReviewPage review={review} draftCount={draft.length} busy={busy} warningAccepted={warningAccepted} setWarningAccepted={setWarningAccepted} onPrepare={() => void prepareReview()} onApply={() => void apply({ type: "replace_original" })} onSaveCopy={() => void saveCopy()} onDiscard={() => { const previousMode = reviewMode; discardDraft(); setReviewMode("edit"); if (previousMode === "restore") setPage("backups"); else if (previousMode === "recovery") setPage("saves"); }} isRestore={reviewMode !== "edit"} /> : null}
          {page === "backups" && snapshot ? <BackupsPage snapshot={snapshot} backups={backups} busy={busy} onRefresh={() => void loadBackups()} onRestore={(backup) => void restoreBackup(backup)} /> : null}
          {page === "settings" ? <SettingsPage diagnostics={diagnostics} rootPath={rootPath} setRootPath={setRootPath} refreshToken={settingsRefreshToken} onRegisterRoot={registerManualRoot} onForgetRoot={forgetManualRoot} onDiagnostics={generateDiagnostics} onToast={setToast} onDirtyChange={setSettingsDirty} /> : null}
        </main>
      </div>

      {toast ? <div className="toast" role="status"><BadgeCheck size={18} /><span>{toast}</span><button className="icon-button" type="button" onClick={() => setToast(null)} aria-label="Dismiss notification"><X size={15} /></button></div> : null}
    </div>
  );
}
