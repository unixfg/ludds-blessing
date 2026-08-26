import {
  Boxes,
  Building2,
  CircleAlert,
  Gauge,
  MapPin,
  PackageOpen,
  PackagePlus,
  RotateCcw,
  Search,
  ShieldAlert,
  Warehouse,
} from "lucide-react";
import { useEffect, useId, useMemo, useState } from "react";
import type {
  AddableItemView,
  ColonyResourcesView,
  ColonyResourceStackView,
  ColonyView,
  Edit,
  InventoryKind,
  InventoryStackView,
  InventoryView,
  SaveSnapshot,
  StorageStackView,
  StorageView,
} from "./types";
import { CatalogAddDialog } from "./CatalogAddDialog";

type StackContainer = InventoryView | StorageView | ColonyResourcesView;
type StackRow = InventoryStackView | StorageStackView | ColonyResourceStackView;

type DraftProps = {
  snapshot: SaveSnapshot;
  draft: Edit[];
  upsert: (edit: Edit) => void;
  reset: (key: string) => void;
  resetPrefix: (prefix: string) => void;
};

type ColonyPageProps = DraftProps;

const KIND_LABELS: Record<InventoryKind, string> = {
  resources: "Resources",
  weapons: "Weapons",
  fighter_wing: "Fighter wings",
  special: "Special items",
  unknown: "Unknown",
};

const formatDecimal = (raw: string): string => {
  const value = Number(raw);
  if (!Number.isFinite(value)) return raw;
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 }).format(value);
};

// Number inputs cannot accept locale grouping or decimal separators. Compact
// only the at-rest presentation; focus restores the exact saved float text so
// display formatting can never create a draft or silently normalize a value.
const formatSavedQuantity = (raw: string): string => {
  const value = Number(raw);
  if (!Number.isFinite(value) || Number.isInteger(value)) return raw;
  const compact = Number(value.toFixed(3));
  return compact > 0 ? String(compact) : raw;
};

const isDiscreteStack = (kind: InventoryKind): boolean => kind === "weapons" || kind === "fighter_wing" || kind === "special";

const quantityError = (stack: StackRow, raw: string): string | null => {
  if (raw.trim() === "") return "Enter a quantity.";
  const value = Number(raw);
  const rounded = Math.fround(value);
  if (!Number.isFinite(value) || !Number.isFinite(rounded)) return "Enter a finite single-precision quantity.";
  if (isDiscreteStack(stack.kind) && (!Number.isInteger(value) || value < 1)) return "Enter a whole quantity of 1 or more.";
  if (stack.kind === "resources" && rounded <= 0) return "Enter a resource quantity greater than zero after single-precision storage.";
  const maximum = Number(stack.maxQuantity);
  const roundedMaximum = Math.fround(maximum);
  if (Number.isFinite(maximum) && Number.isFinite(roundedMaximum) && rounded > roundedMaximum) return `Maximum supported quantity is ${formatDecimal(stack.maxQuantity)}.`;
  return null;
};

const f32RoundingPreview = (raw: string): string | null => {
  const value = Number(raw);
  const rounded = Math.fround(value);
  return Number.isFinite(value) && Number.isFinite(rounded) && rounded !== value ? String(rounded) : null;
};

function LedgerHeader({ eyebrow, title, description, action }: { eyebrow: string; title: string; description: string; action?: React.ReactNode }) {
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

function LedgerNotice({ id, tone = "info", title, children }: { id?: string; tone?: "info" | "warning" | "danger"; title: string; children: React.ReactNode }) {
  const Icon = tone === "danger" ? CircleAlert : tone === "warning" ? ShieldAlert : PackageOpen;
  return (
    <div className={`notice notice--${tone}`} id={id} role={tone === "danger" ? "alert" : "status"}>
      <Icon size={19} aria-hidden="true" />
      <div><strong>{title}</strong><div>{children}</div></div>
    </div>
  );
}

function LedgerEmpty({ icon: Icon, title, children }: { icon: typeof PackageOpen; title: string; children: React.ReactNode }) {
  return (
    <div className="empty-state">
      <span className="empty-state__orbit" aria-hidden="true"><Icon size={30} /></span>
      <h3>{title}</h3>
      <p>{children}</p>
    </div>
  );
}

type ExistingStackEditorProps = {
  inventory: StackContainer;
  heading: string;
  description: string;
  quantityLabel: string;
  edits: ReadonlyMap<string, string>;
  globallyDisabled: boolean;
  globalReason: string | null;
  showCapacity?: boolean;
  onChange: (stack: StackRow, quantity: string) => void;
  onReset: (stack: StackRow) => void;
  onResetAll: () => void;
};

export function ExistingStackEditor({
  inventory,
  heading,
  description,
  quantityLabel,
  edits,
  globallyDisabled,
  globalReason,
  showCapacity = true,
  onChange,
  onReset,
  onResetAll,
}: ExistingStackEditorProps) {
  const sectionId = useId();
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<InventoryKind | "all">("all");
  const [modifiedOnly, setModifiedOnly] = useState(false);
  const [invalidEntries, setInvalidEntries] = useState<Record<string, string>>({});
  const [focusedStackId, setFocusedStackId] = useState<string | null>(null);

  const clearInvalidEntry = (stackId: string) => {
    setInvalidEntries((current) => {
      if (!(stackId in current)) return current;
      const next = { ...current };
      delete next[stackId];
      return next;
    });
  };

  const kinds = useMemo(
    () => [...new Set(inventory.stacks.map((stack) => stack.kind))].sort(),
    [inventory.stacks],
  );
  const visible = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    return inventory.stacks.filter((stack) => {
      const searchable = `${stack.name} ${stack.itemId} ${stack.specialData ?? ""}`.toLocaleLowerCase();
      return (kind === "all" || stack.kind === kind)
        && (!normalizedQuery || searchable.includes(normalizedQuery))
        && (!modifiedOnly || edits.has(stack.id) || Object.prototype.hasOwnProperty.call(invalidEntries, stack.id));
    });
  }, [edits, invalidEntries, inventory.stacks, kind, modifiedOnly, query]);

  const hasLocalErrors = Object.keys(invalidEntries).length > 0;
  const readOnly = !inventory.editable || globallyDisabled;
  const readOnlyReason = inventory.reason
    ?? globalReason
    ?? "This cargo container is read-only because it could not be safely authorized for this session.";
  const readOnlyReasonId = `${sectionId}-readonly-reason`;

  const projectedSpace = useMemo(() => {
    if (edits.size === 0) return null;
    let total = Math.fround(0);
    for (const stack of inventory.stacks) {
      const quantity = Math.fround(Number(edits.get(stack.id) ?? stack.quantity));
      const perUnit = Math.fround(Number(stack.cargoSpacePerUnit));
      const contribution = Math.fround(quantity * perUnit);
      total = Math.fround(total + contribution);
      if (![quantity, perUnit, contribution, total].every(Number.isFinite)) return null;
    }
    return total;
  }, [edits, inventory.stacks]);

  const maxSpace = inventory.maxSpace === null ? null : Number(inventory.maxSpace);
  const projectedOverload = projectedSpace !== null && maxSpace !== null && Number.isFinite(maxSpace) && Number.isFinite(Math.fround(maxSpace))
    ? projectedSpace > Math.fround(maxSpace)
    : inventory.overloaded;
  const capacityMessage = edits.size === 0
    ? inventory.overloaded ? "Current cargo exceeds capacity." : "Current saved capacity."
    : projectedSpace === null
      ? "Staged capacity will be verified during Review."
      : projectedOverload
        ? "Projected cargo exceeds capacity."
        : "Projected from staged quantities.";

  return (
    <section className="panel stack-editor" aria-labelledby={`${sectionId}-heading`}>
      <div className="panel__heading">
        <div>
          <p className="eyebrow">Saved stacks</p>
          <h3 id={`${sectionId}-heading`}>{heading}</h3>
          <p className="muted">{description}</p>
        </div>
        {edits.size > 0 || hasLocalErrors ? (
          <button className="button button--ghost" type="button" onClick={() => { setInvalidEntries({}); setFocusedStackId(null); onResetAll(); }}>
            <RotateCcw size={15} aria-hidden="true" /> Reset section
          </button>
        ) : <span className="panel-count">{inventory.stacks.length}</span>}
      </div>

      {readOnly ? (
        <LedgerNotice id={readOnlyReasonId} tone="warning" title="Quantity editing unavailable">
          {readOnlyReason}
        </LedgerNotice>
      ) : null}

      {showCapacity ? (
        <div className={`capacity-band ${projectedOverload ? "capacity-band--over" : ""}`} role="status" aria-live="polite">
          <Gauge size={19} aria-hidden="true" />
          <div>
            <span>Cargo space</span>
            <strong>
              {projectedSpace === null ? formatDecimal(inventory.usedSpace) : formatDecimal(String(Math.max(0, projectedSpace)))}
              {inventory.maxSpace === null ? " used" : ` / ${formatDecimal(inventory.maxSpace)}`}
            </strong>
          </div>
          <small>{capacityMessage}</small>
        </div>
      ) : null}

      <div className="stack-toolbar">
        <label className="search-field">
          <span className="sr-only">Search {quantityLabel}</span>
          <Search size={16} aria-hidden="true" />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search name, item ID, or special data" />
        </label>
        <label>
          <span className="sr-only">Filter cargo category</span>
          <select aria-label="Filter cargo category" value={kind} onChange={(event) => setKind(event.target.value as InventoryKind | "all")}>
            <option value="all">All categories</option>
            {kinds.map((entry) => <option value={entry} key={entry}>{KIND_LABELS[entry]}</option>)}
          </select>
        </label>
        <label className="toggle">
          <input type="checkbox" checked={modifiedOnly} onChange={(event) => setModifiedOnly(event.target.checked)} />
          <span>Modified only</span>
        </label>
      </div>

      <div className="stack-list" aria-live="polite">
        {visible.map((stack, index) => {
          const changed = edits.has(stack.id);
          const quantity = edits.get(stack.id) ?? stack.quantity;
          const hasInvalidEntry = Object.prototype.hasOwnProperty.call(invalidEntries, stack.id);
          const compactSavedQuantity = formatSavedQuantity(stack.quantity);
          const displayedQuantity = hasInvalidEntry
            ? invalidEntries[stack.id]
            : changed || focusedStackId === stack.id
              ? quantity
              : compactSavedQuantity;
          const error = hasInvalidEntry ? quantityError(stack, displayedQuantity) : null;
          const savedAs = changed && !error ? f32RoundingPreview(quantity) : null;
          const disabled = globallyDisabled || !inventory.editable || !stack.editable;
          const stackReason = !stack.editable
            ? stack.reason ?? "This stack is read-only because it could not be safely authorized."
            : null;
          const reasonId = `${sectionId}-${index}-reason`;
          const errorId = `${sectionId}-${index}-error`;
          const roundingId = `${sectionId}-${index}-rounding`;
          const describedBy = [readOnly ? readOnlyReasonId : null, stackReason ? reasonId : null, error ? errorId : null, savedAs ? roundingId : null]
            .filter(Boolean)
            .join(" ") || undefined;
          return (
            <div className={`stack-row ${disabled ? "stack-row--readonly" : ""}`} key={stack.id}>
              <span className={`stack-kind stack-kind--${stack.kind}`} aria-hidden="true">{KIND_LABELS[stack.kind].slice(0, 1)}</span>
              <div className="stack-identity">
                <strong>{stack.name}</strong>
                <span>{KIND_LABELS[stack.kind]} · {stack.itemId}</span>
                {stack.specialData ? <small>Data: {stack.specialData}</small> : null}
                {stackReason ? <small className="stack-readonly" id={reasonId}>{stackReason}</small> : null}
              </div>
              <label className="stack-quantity">
                <span>{quantityLabel}</span>
                <input
                  aria-label={`${stack.name} quantity`}
                  aria-describedby={describedBy}
                  aria-invalid={Boolean(error)}
                  inputMode="decimal"
                  type="number"
                  min={isDiscreteStack(stack.kind) ? "1" : "1e-45"}
                  max={stack.maxQuantity}
                  step={isDiscreteStack(stack.kind) ? "1" : "any"}
                  value={displayedQuantity}
                  disabled={disabled}
                  title={!changed && compactSavedQuantity !== stack.quantity ? `Exact saved value: ${stack.quantity}` : undefined}
                  onFocus={() => setFocusedStackId(stack.id)}
                  onBlur={() => setFocusedStackId((current) => current === stack.id ? null : current)}
                  onChange={(event) => {
                    const next = event.target.value;
                    if (quantityError(stack, next)) {
                      setInvalidEntries((current) => ({ ...current, [stack.id]: next }));
                      return;
                    }
                    clearInvalidEntry(stack.id);
                    if (Math.fround(Number(next)) === Math.fround(Number(stack.quantity))) onReset(stack);
                    else onChange(stack, next);
                  }}
                />
                {error ? <small className="stack-error" id={errorId}>{error}</small> : null}
                {savedAs ? <small className="stack-rounding" id={roundingId}>Saved as {savedAs}</small> : null}
              </label>
              {changed || hasInvalidEntry ? (
                <button className="icon-button" type="button" onClick={() => { clearInvalidEntry(stack.id); setFocusedStackId((current) => current === stack.id ? null : current); onReset(stack); }} aria-label={`Reset ${stack.name} quantity`}>
                  <RotateCcw size={15} aria-hidden="true" />
                </button>
              ) : <span className="row-spacer" aria-hidden="true" />}
            </div>
          );
        })}
        {visible.length === 0 ? (
          <LedgerEmpty icon={PackageOpen} title="No matching stacks">Adjust the search or category filter, or use the Add action for a supported installed item.</LedgerEmpty>
        ) : null}
      </div>
    </section>
  );
}

export function InventoryPage({ snapshot, draft, upsert, reset, resetPrefix }: DraftProps) {
  const inventory = snapshot.inventory;
  const edits = useMemo(() => new Map(
    draft
      .filter((edit): edit is Extract<Edit, { type: "set_inventory_quantity" }> => edit.type === "set_inventory_quantity")
      .map((edit) => [edit.stackId, edit.quantity]),
  ), [draft]);
  const globallyDisabled = !snapshot.writeCapability.editable || snapshot.protectedLocked;
  const globalReason = snapshot.protectedLocked
    ? "Unlock this protected save before staging cargo changes."
    : snapshot.writeCapability.reason;

  return (
    <div className="page-stack">
      <LedgerHeader
        eyebrow="Fleet manifest"
        title="Inventory"
        description="Adjust quantities for existing, uniquely identified player-cargo stacks. Unsupported and unknown entries remain visible without becoming writable."
      />
      <LedgerNotice title="Object graph stays intact">
        Quantity changes never add, remove, merge, or reorder cargo-stack objects. Capacity and exact stored values are checked again during semantic review.
      </LedgerNotice>
      {inventory ? (
        <ExistingStackEditor
          inventory={inventory}
          heading="Player fleet cargo"
          description="Resources, weapons, fighter wings, and recognized special items in the authoritative player cargo container."
          quantityLabel="Fleet quantity"
          edits={edits}
          globallyDisabled={globallyDisabled}
          globalReason={globalReason}
          onChange={(stack, quantity) => upsert({ type: "set_inventory_quantity", stackId: stack.id, quantity })}
          onReset={(stack) => reset(`inventory.${stack.id}`)}
          onResetAll={() => resetPrefix("inventory.")}
        />
      ) : (
        <LedgerEmpty icon={PackageOpen} title="Inventory unavailable">
          This save does not expose one unique, safely anchored player-cargo container. No inventory data is guessed from names or volatile IDs.
        </LedgerEmpty>
      )}
    </div>
  );
}

function ColonyIdentity({ colony }: { colony: ColonyView }) {
  return (
    <div className="colony-hero">
      <span className="faction-mark" aria-hidden="true">{colony.name.slice(0, 1).toUpperCase()}</span>
      <div>
        <p className="eyebrow">Editable colony holdings</p>
        <h3>{colony.name}</h3>
        <p><MapPin size={14} aria-hidden="true" /> {colony.locationContext || "Location unavailable"}</p>
      </div>
    </div>
  );
}

type ColonyStackTab = "storage" | "localResources";

const defaultColonyTab = (colony: ColonyView): ColonyStackTab => {
  if (colony.storage !== null) return "storage";
  return "localResources";
};

const holdingStatus = (label: string, count: number | null) => `${label}: ${count === null ? "unavailable" : count}`;

function PendingAdditions({
  title,
  additions,
  catalog,
  onReset,
}: {
  title: string;
  additions: Array<Extract<Edit, { type: "add_storage_item" | "add_colony_resource" }>>;
  catalog: AddableItemView[];
  onReset: (edit: Extract<Edit, { type: "add_storage_item" | "add_colony_resource" }>) => void;
}) {
  if (additions.length === 0) return null;
  return (
    <section className="pending-additions" aria-label={title}>
      <div><p className="eyebrow">Pending additions</p><strong>{title}</strong><span>{additions.length}</span></div>
      <ul>
        {additions.map((edit) => {
          const item = catalog.find((candidate) => candidate.id === edit.catalogItemId);
          return <li key={edit.catalogItemId}><PackagePlus size={16} aria-hidden="true" /><span><strong>{item?.name ?? "Catalog item"}</strong><small>{item?.itemId ?? edit.catalogItemId} · +{edit.quantity}</small></span><button className="icon-button" type="button" onClick={() => onReset(edit)} aria-label={`Reset ${item?.name ?? "item"} addition`}><RotateCcw size={15} /></button></li>;
        })}
      </ul>
    </section>
  );
}

export function ColoniesPage({ snapshot, draft, upsert, reset, resetPrefix }: ColonyPageProps) {
  const holdingsId = useId();
  const [query, setQuery] = useState("");
  const [stacksOnly, setStacksOnly] = useState(false);
  const [selectedId, setSelectedId] = useState(snapshot.colonies[0]?.id ?? "");
  const [activeTabs, setActiveTabs] = useState<Record<string, ColonyStackTab>>({});
  const [catalogTarget, setCatalogTarget] = useState<"storage" | "localResources" | null>(null);
  const visible = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    return snapshot.colonies.filter((colony) => {
      const matches = `${colony.name} ${colony.locationContext ?? ""}`.toLocaleLowerCase().includes(normalizedQuery);
      return matches && (!stacksOnly || colony.storage !== null || colony.localResources !== null);
    });
  }, [query, snapshot.colonies, stacksOnly]);

  useEffect(() => {
    if (!visible.some((colony) => colony.id === selectedId)) setSelectedId(visible[0]?.id ?? "");
  }, [selectedId, visible]);

  useEffect(() => {
    setCatalogTarget(null);
  }, [selectedId]);

  const colony = visible.find((candidate) => candidate.id === selectedId) ?? visible[0];
  const activeStackTab = colony ? activeTabs[colony.id] ?? defaultColonyTab(colony) : "storage";
  const setActiveStackTab = (tab: ColonyStackTab) => {
    if (!colony) return;
    setActiveTabs((current) => ({ ...current, [colony.id]: tab }));
  };

  const storageEdits = useMemo(() => new Map(
    draft
      .filter((edit): edit is Extract<Edit, { type: "set_storage_stack_quantity" }> => edit.type === "set_storage_stack_quantity" && edit.colonyId === colony?.id)
      .map((edit) => [edit.stackId, edit.quantity]),
  ), [colony?.id, draft]);
  const resourceEdits = useMemo(() => new Map(
    draft
      .filter((edit): edit is Extract<Edit, { type: "set_colony_resource_quantity" }> => edit.type === "set_colony_resource_quantity" && edit.colonyId === colony?.id)
      .map((edit) => [edit.stackId, edit.quantity]),
  ), [colony?.id, draft]);
  const storageAdditions = useMemo(() => draft.filter(
    (edit): edit is Extract<Edit, { type: "add_storage_item" }> => edit.type === "add_storage_item" && edit.colonyId === colony?.id,
  ), [colony?.id, draft]);
  const resourceAdditions = useMemo(() => draft.filter(
    (edit): edit is Extract<Edit, { type: "add_colony_resource" }> => edit.type === "add_colony_resource" && edit.colonyId === colony?.id,
  ), [colony?.id, draft]);
  const globallyDisabled = !snapshot.writeCapability.editable || snapshot.protectedLocked;
  const globalReason = snapshot.protectedLocked
    ? "Unlock this protected save before staging colony stack changes."
    : snapshot.writeCapability.reason;
  const handleStackTabKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!colony || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    const available: ColonyStackTab[] = [
      "storage",
      "localResources",
    ];
    event.preventDefault();
    const current = Math.max(0, available.indexOf(activeStackTab));
    const next = event.key === "Home"
      ? available[0]
      : event.key === "End"
        ? available[available.length - 1]
        : available[(current + (event.key === "ArrowRight" ? 1 : -1) + available.length) % available.length];
    setActiveStackTab(next);
    (event.currentTarget.querySelector(`[data-stack-tab="${next}"]`) as HTMLButtonElement | null)?.focus();
  };

  return (
    <div className="page-stack">
      <LedgerHeader
        eyebrow="Holding register"
        title="Colonies"
        description="Edit each colony's Storage and Local Resources from one focused holdings workspace."
      />
      {snapshot.colonies.length === 0 ? (
        <LedgerEmpty icon={Building2} title="No player colonies found">
          The save exposes no uniquely anchored player-owned colonies. Markets belonging to other factions are not presented as editable holdings.
        </LedgerEmpty>
      ) : (
        <>
          <section className="panel colony-browser">
            <div className="stack-toolbar">
              <label className="search-field">
                <span className="sr-only">Search colonies</span>
                <Search size={16} aria-hidden="true" />
                <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search colony or location" />
              </label>
              <label className="toggle"><input type="checkbox" checked={stacksOnly} onChange={(event) => setStacksOnly(event.target.checked)} /><span>Holdings available</span></label>
            </div>
            <div className="colony-layout">
              <aside className="colony-roster" aria-label="Player colonies">
                {visible.map((candidate) => (
                  <button type="button" key={candidate.id} className={candidate.id === colony?.id ? "is-selected" : ""} aria-pressed={candidate.id === colony?.id} onClick={() => setSelectedId(candidate.id)}>
                    <span className="colony-roster__mark" aria-hidden="true"><Building2 size={17} /></span>
                    <span><strong>{candidate.name}</strong><small>{candidate.locationContext || "Location unavailable"}</small></span>
                    <span className="colony-roster__holdings">
                      <span className={candidate.storage === null ? "is-unavailable" : ""} aria-label={holdingStatus("Storage stacks", candidate.storage?.stacks.length ?? null)} title={holdingStatus("Storage stacks", candidate.storage?.stacks.length ?? null)}>
                        <Warehouse size={13} aria-hidden="true" /><b>{candidate.storage?.stacks.length ?? "—"}</b>
                      </span>
                      <span className={candidate.localResources === null ? "is-unavailable" : ""} aria-label={holdingStatus("Local Resources stacks", candidate.localResources?.stacks.length ?? null)} title={holdingStatus("Local Resources stacks", candidate.localResources?.stacks.length ?? null)}>
                        <Boxes size={13} aria-hidden="true" /><b>{candidate.localResources?.stacks.length ?? "—"}</b>
                      </span>
                    </span>
                  </button>
                ))}
                {visible.length === 0 ? <p className="muted colony-roster__empty">No colonies match these filters.</p> : null}
              </aside>
              {colony ? (
                <article className="colony-detail">
                  <ColonyIdentity colony={colony} />

                  <div className="colony-stack-workspace">
                    <div className="colony-stack-tabs" role="tablist" aria-label={`${colony.name} holdings`} onKeyDown={handleStackTabKeyDown}>
                      <button
                        type="button"
                        id={`${holdingsId}-storage-tab`}
                        role="tab"
                        aria-label={colony.storage === null ? "Storage, unavailable" : `Storage, ${colony.storage.stacks.length} stacks`}
                        aria-selected={activeStackTab === "storage"}
                        aria-controls={`${holdingsId}-storage-panel`}
                        data-stack-tab="storage"
                        tabIndex={activeStackTab === "storage" ? 0 : -1}
                        className={activeStackTab === "storage" ? "is-selected" : ""}
                        onClick={() => setActiveStackTab("storage")}
                      >
                        <Warehouse size={16} aria-hidden="true" />
                        <span className="colony-stack-tab__label">Storage</span>
                        <span className={`colony-stack-tab__count ${colony.storage === null ? "is-unavailable" : ""}`}>{colony.storage?.stacks.length ?? "Unavailable"}</span>
                      </button>
                      <button
                        type="button"
                        id={`${holdingsId}-resources-tab`}
                        role="tab"
                        aria-label={colony.localResources === null ? "Local Resources, unavailable" : `Local Resources, ${colony.localResources.stacks.length} stacks`}
                        aria-selected={activeStackTab === "localResources"}
                        aria-controls={`${holdingsId}-resources-panel`}
                        data-stack-tab="localResources"
                        tabIndex={activeStackTab === "localResources" ? 0 : -1}
                        className={activeStackTab === "localResources" ? "is-selected" : ""}
                        onClick={() => setActiveStackTab("localResources")}
                      >
                        <Boxes size={16} aria-hidden="true" />
                        <span className="colony-stack-tab__label">Local Resources</span>
                        <span className={`colony-stack-tab__count ${colony.localResources === null ? "is-unavailable" : ""}`}>{colony.localResources?.stacks.length ?? "Unavailable"}</span>
                      </button>
                    </div>

                    <div
                      role="tabpanel"
                      id={`${holdingsId}-${activeStackTab === "localResources" ? "resources" : "storage"}-panel`}
                      aria-labelledby={`${holdingsId}-${activeStackTab === "localResources" ? "resources" : "storage"}-tab`}
                      className="colony-stack-panel"
                    >
                      {activeStackTab === "storage" && colony.storage ? (
                        <>
                          <div className="holdings-action"><div><strong>Add installed cargo</strong><span>Commodities, weapons, fighter LPCs, and individual blueprints.</span></div><button className="button" type="button" disabled={globallyDisabled || !colony.storage.editable} onClick={() => setCatalogTarget("storage")}><PackagePlus size={16} /> Add item</button></div>
                          <PendingAdditions title={`${colony.name} storage`} additions={storageAdditions} catalog={snapshot.catalog.addableItems} onReset={(edit) => reset(`colony.${edit.colonyId}.storage.add.${edit.catalogItemId}`)} />
                          <ExistingStackEditor
                            key={`${colony.id}:storage`}
                            inventory={colony.storage}
                            heading={`${colony.name} storage`}
                            description="Edit recognized saved stacks or stage a catalog-backed new stack above."
                            quantityLabel="Storage quantity"
                            edits={storageEdits}
                            globallyDisabled={globallyDisabled}
                            globalReason={globalReason}
                            onChange={(stack, quantity) => upsert({ type: "set_storage_stack_quantity", colonyId: colony.id, stackId: stack.id, quantity })}
                            onReset={(stack) => reset(`colony.${colony.id}.storage.${stack.id}`)}
                            onResetAll={() => resetPrefix(`colony.${colony.id}.storage.`)}
                          />
                        </>
                      ) : activeStackTab === "storage" ? (
                        <LedgerEmpty icon={Warehouse} title="Storage unavailable">
                          This colony does not expose one unique, safely indexed Storage container. Local Resources remains available in its own tab.
                        </LedgerEmpty>
                      ) : activeStackTab === "localResources" && colony.localResources ? (
                        <>
                          <LedgerNotice title="Local Resources · Economy-managed stockpile">
                            Edit the colony's current stockpile directly. This creates no month-end charge or refund. Starsector may replenish or consume these amounts normally after loading.
                          </LedgerNotice>
                          <div className="holdings-action"><div><strong>Add a commodity</strong><span>Only economic commodities from the validated local installation are offered.</span></div><button className="button" type="button" disabled={globallyDisabled || !colony.localResources.editable} onClick={() => setCatalogTarget("localResources")}><PackagePlus size={16} /> Add commodity</button></div>
                          <PendingAdditions title={`${colony.name} Local Resources`} additions={resourceAdditions} catalog={snapshot.catalog.addableItems} onReset={(edit) => reset(`colony.${edit.colonyId}.localResources.add.${edit.catalogItemId}`)} />
                          <ExistingStackEditor
                            key={`${colony.id}:local-resources`}
                            inventory={colony.localResources}
                            heading={`${colony.name} Local Resources`}
                            description="Edit existing recognized commodities or stage a validated new commodity above. Accounting trackers, production, supply, and demand remain untouched."
                            quantityLabel="Stockpile quantity"
                            edits={resourceEdits}
                            globallyDisabled={globallyDisabled}
                            globalReason={globalReason}
                            showCapacity={false}
                            onChange={(stack, quantity) => upsert({ type: "set_colony_resource_quantity", colonyId: colony.id, stackId: stack.id, quantity })}
                            onReset={(stack) => reset(`colony.${colony.id}.localResources.${stack.id}`)}
                            onResetAll={() => resetPrefix(`colony.${colony.id}.localResources.`)}
                          />
                        </>
                      ) : (
                        <LedgerEmpty icon={Boxes} title="Local Resources unavailable">
                          This colony does not expose one unique, supported Local Resources stockpile. Storage remains usable, and no economy data is guessed.
                        </LedgerEmpty>
                      )}
                    </div>
                  </div>

                  {colony.warnings.map((warning) => <LedgerNotice tone="warning" title="Colony warning" key={warning}>{warning}</LedgerNotice>)}

                  {catalogTarget ? (
                    <CatalogAddDialog
                      title={catalogTarget === "storage" ? `Add to ${colony.name} storage` : `Add to ${colony.name} Local Resources`}
                      description={catalogTarget === "storage" ? "Choose a supported installed item. Existing matches are increased; absent matches become a checked RC8 stack." : "Choose a recognized economic commodity. Starsector's economy may change the amount after loading."}
                      items={catalogTarget === "storage" ? snapshot.catalog.addableItems : snapshot.catalog.addableItems.filter((item) => item.localResourcesEligible)}
                      existingItemIds={new Set((catalogTarget === "storage" ? colony.storage?.stacks : colony.localResources?.stacks)?.map((stack) => stack.itemId) ?? [])}
                      onClose={() => setCatalogTarget(null)}
                      onAdd={(item, quantity) => upsert(catalogTarget === "storage"
                        ? { type: "add_storage_item", colonyId: colony.id, catalogItemId: item.id, quantity }
                        : { type: "add_colony_resource", colonyId: colony.id, catalogItemId: item.id, quantity })}
                    />
                  ) : null}
                </article>
              ) : null}
            </div>
          </section>
        </>
      )}
    </div>
  );
}
