import { PackagePlus, Search, X } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { AddableItemKind, AddableItemView } from "./types";

const KIND_LABELS: Record<AddableItemKind, string> = {
  commodity: "Commodities",
  weapon: "Weapons",
  fighter_wing: "Fighter LPCs",
  ship_blueprint: "Ship blueprints",
  weapon_blueprint: "Weapon blueprints",
  fighter_blueprint: "Fighter blueprints",
};

const isWholeQuantity = (kind: AddableItemKind): boolean => kind !== "commodity";

export type CatalogAddDialogProps = {
  title: string;
  description: string;
  items: AddableItemView[];
  existingCatalogItemIds: ReadonlySet<AddableItemView["id"]>;
  blockedItemReasons?: ReadonlyMap<AddableItemView["id"], string>;
  stagedAdditions?: ReadonlyMap<AddableItemView["id"], string>;
  onClose: () => void;
  onAdd: (item: AddableItemView, quantity: string) => void;
};

const EMPTY_STAGED_ADDITIONS: ReadonlyMap<AddableItemView["id"], string> = new Map();
const EMPTY_BLOCKED_ITEM_REASONS: ReadonlyMap<AddableItemView["id"], string> = new Map();

export function CatalogAddDialog({
  title,
  description,
  items,
  existingCatalogItemIds,
  blockedItemReasons = EMPTY_BLOCKED_ITEM_REASONS,
  stagedAdditions = EMPTY_STAGED_ADDITIONS,
  onClose,
  onAdd,
}: CatalogAddDialogProps) {
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<AddableItemKind | "all">("all");
  const [selected, setSelected] = useState<AddableItemView | null>(null);
  const [quantity, setQuantity] = useState("1");
  const [stagedConfirmation, setStagedConfirmation] = useState("");
  const resultId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    searchRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const dialog = dialogRef.current;
      if (!dialog) return;
      const focusable = Array.from(dialog.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      )).filter((element) => element.tabIndex >= 0 && element.getAttribute("aria-hidden") !== "true");
      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (!dialog.contains(active)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (event.shiftKey && active === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (previouslyFocused?.isConnected) previouslyFocused.focus();
    };
  }, []);

  const kinds = useMemo(() => [...new Set(items.map((item) => item.kind))], [items]);
  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return items.filter((item) => (kind === "all" || item.kind === kind)
      && (!normalized || `${item.name} ${item.itemId} ${KIND_LABELS[item.kind]}`.toLocaleLowerCase().includes(normalized)));
  }, [items, kind, query]);

  useEffect(() => {
    if (selected && !filtered.some((item) => item.id === selected.id)) {
      setSelected(null);
      setQuantity("1");
    }
  }, [filtered, selected]);

  const rounded = Math.fround(Number(quantity));
  const maximum = selected ? Math.fround(Number(selected.maxQuantity)) : 0;
  const selectedWasStaged = selected ? stagedAdditions.has(selected.id) : false;
  const quantityError = !selected
    ? "Choose an item."
    : quantity.trim() === "" || !Number.isFinite(Number(quantity)) || !Number.isFinite(rounded)
      ? "Enter a finite quantity."
      : rounded < 1
        ? "New stacks require a quantity of at least 1."
        : isWholeQuantity(selected.kind) && !Number.isInteger(Number(quantity))
          ? "This item requires a whole quantity."
          : Number.isFinite(maximum) && rounded > maximum
            ? `Maximum supported quantity is ${selected.maxQuantity}.`
            : null;

  return (
    <div className="catalog-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section ref={dialogRef} className="catalog-dialog" role="dialog" aria-modal="true" aria-labelledby="catalog-dialog-title" aria-describedby="catalog-dialog-description" tabIndex={-1}>
        <header>
          <div><p className="eyebrow">Validated local catalog</p><h3 id="catalog-dialog-title">{title}</h3><p id="catalog-dialog-description">{description}</p></div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="Close item catalog"><X size={18} /></button>
        </header>
        <div className="catalog-dialog__toolbar">
          <label className="search-field">
            <Search size={16} aria-hidden="true" />
            <span className="sr-only">Search item catalog</span>
            <input ref={searchRef} value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search name or installed ID" />
          </label>
          <label><span className="sr-only">Catalog category</span><select aria-label="Catalog category" value={kind} onChange={(event) => setKind(event.target.value as AddableItemKind | "all")}><option value="all">All supported types</option>{kinds.map((entry) => <option key={entry} value={entry}>{KIND_LABELS[entry]}</option>)}</select></label>
        </div>
        <div className="catalog-dialog__status-line">
          <p className="catalog-dialog__count" role="status">{filtered.length} supported {filtered.length === 1 ? "entry" : "entries"}</p>
          <p className="catalog-dialog__staged-status" role="status" aria-live="polite" aria-atomic="true">{stagedConfirmation}</p>
        </div>
        <div className="catalog-dialog__body">
          <div className="catalog-results" role="group" aria-label="Supported items">
            {filtered.map((item, index) => {
              const blockedReason = blockedItemReasons.get(item.id);
              const blockedReasonId = blockedReason ? `${resultId}-${index}-reason` : undefined;
              return (
                <button
                  key={item.id}
                  type="button"
                  aria-disabled={blockedReason ? "true" : undefined}
                  aria-describedby={blockedReasonId}
                  aria-pressed={selected?.id === item.id}
                  className={selected?.id === item.id ? "is-selected" : ""}
                  onClick={() => {
                    if (blockedReason) return;
                    setSelected(item);
                    setQuantity(stagedAdditions.get(item.id) ?? "1");
                  }}
                >
                  <span>
                    <strong>{item.name}</strong>
                    <small>{KIND_LABELS[item.kind]} · {item.itemId}</small>
                    {blockedReason ? <small className="catalog-results__reason" id={blockedReasonId}>{blockedReason}</small> : null}
                  </span>
                  {blockedReason ? <b>Unavailable</b> : stagedAdditions.has(item.id) ? <b>Staged +{stagedAdditions.get(item.id)}</b> : existingCatalogItemIds.has(item.id) ? <b>Increase saved stack</b> : <b>New stack</b>}
                </button>
              );
            })}
            {filtered.length === 0 ? <p className="catalog-results__empty">No supported catalog entries match this search.</p> : null}
          </div>
          <aside className="catalog-selection">
            {selected ? <>
              <PackagePlus size={28} aria-hidden="true" />
              <div><strong>{selected.name}</strong><span>{selected.itemId}</span><small>{selected.cargoSpacePerUnit} cargo space per unit</small></div>
              <label><span>Amount to add</span><input aria-label={`Amount of ${selected.name} to add`} type="number" min="1" max={selected.maxQuantity} step={isWholeQuantity(selected.kind) ? "1" : "any"} value={quantity} onChange={(event) => setQuantity(event.target.value)} aria-invalid={Boolean(quantityError)} /></label>
              {quantityError ? <p className="stack-error" role="alert">{quantityError}</p> : null}
              <button
                className="button"
                type="button"
                disabled={Boolean(quantityError)}
                onClick={() => {
                  const stagedQuantity = String(rounded);
                  const pendingCount = stagedAdditions.size + (selectedWasStaged ? 0 : 1);
                  onAdd(selected, stagedQuantity);
                  setStagedConfirmation(`${selectedWasStaged ? "Updated" : "Staged"} ${selected.name} (+${stagedQuantity}). ${pendingCount} pending ${pendingCount === 1 ? "addition" : "additions"}.`);
                  setSelected(null);
                  setQuantity("1");
                  searchRef.current?.focus();
                  searchRef.current?.select();
                }}
              ><PackagePlus size={16} /> {selectedWasStaged ? "Update staged addition" : "Stage addition"}</button>
            </> : <p className="muted">Select an installed catalog entry to stage an addition.</p>}
          </aside>
        </div>
        <footer className="catalog-dialog__footer">
          <span>{stagedAdditions.size} staged {stagedAdditions.size === 1 ? "addition" : "additions"}</span>
          <button className="button button--secondary" type="button" onClick={onClose}>Done adding</button>
        </footer>
      </section>
    </div>
  );
}
