import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { CatalogAddDialog } from "./CatalogAddDialog";
import type { AddableItemView } from "./types";

const ITEMS: AddableItemView[] = [
  {
    id: "catalog-item-vulcan",
    itemId: "vulcan",
    name: "Vulcan Cannon",
    kind: "weapon",
    cargoSpacePerUnit: "5",
    maxQuantity: "1000000",
    localResourcesEligible: false,
  },
  {
    id: "catalog-item-railgun",
    itemId: "railgun",
    name: "Railgun",
    kind: "weapon",
    cargoSpacePerUnit: "5",
    maxQuantity: "1000000",
    localResourcesEligible: false,
  },
];

function DialogHarness({ onAdd }: { onAdd?: (item: AddableItemView, quantity: string) => void }) {
  const [open, setOpen] = useState(false);
  const [stagedAdditions, setStagedAdditions] = useState<ReadonlyMap<AddableItemView["id"], string>>(() => new Map());
  const stageAddition = (item: AddableItemView, quantity: string) => {
    setStagedAdditions((current) => {
      const next = new Map(current);
      next.set(item.id, quantity);
      return next;
    });
    onAdd?.(item, quantity);
  };
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Add item</button>
      {open ? (
        <CatalogAddDialog
          title="Add installed item"
          description="Choose one supported item."
          items={ITEMS}
          existingCatalogItemIds={new Set()}
          stagedAdditions={stagedAdditions}
          onClose={() => setOpen(false)}
          onAdd={stageAddition}
        />
      ) : null}
    </>
  );
}

const openDialog = () => {
  const invoker = screen.getByRole("button", { name: "Add item" });
  invoker.focus();
  fireEvent.click(invoker);
  expect(screen.getByRole("dialog", { name: "Add installed item" })).toBeInTheDocument();
  expect(screen.getByLabelText("Search item catalog")).toHaveFocus();
  return invoker;
};

describe("CatalogAddDialog keyboard accessibility", () => {
  it("uses ordinary pressed-state buttons instead of an incomplete listbox pattern", () => {
    render(<DialogHarness />);
    openDialog();

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    const items = screen.getByRole("group", { name: "Supported items" });
    expect(within(items).queryByRole("option")).not.toBeInTheDocument();

    const item = screen.getByRole("button", { name: /Vulcan Cannon/i });
    expect(item).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(item);
    expect(item).toHaveAttribute("aria-pressed", "true");
  });

  it("wraps Tab within the modal and restores focus to its invoking control", () => {
    render(<DialogHarness />);
    const invoker = openDialog();
    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon/i }));

    const close = screen.getByRole("button", { name: "Close item catalog" });
    const done = screen.getByRole("button", { name: "Done adding" });
    done.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(close).toHaveFocus();

    close.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(done).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invoker).toHaveFocus();
  });

  it("stages multiple items without closing and exits explicitly when finished", () => {
    const onAdd = vi.fn();
    render(<DialogHarness onAdd={onAdd} />);
    const invoker = openDialog();
    const search = screen.getByLabelText("Search item catalog");
    const category = screen.getByLabelText("Catalog category");

    fireEvent.change(category, { target: { value: "weapon" } });
    fireEvent.change(search, { target: { value: "vulcan" } });
    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon/i }));
    fireEvent.change(screen.getByLabelText("Amount of Vulcan Cannon to add"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Stage addition" }));

    expect(onAdd).toHaveBeenNthCalledWith(1, ITEMS[0], "3");
    expect(screen.getByRole("dialog", { name: "Add installed item" })).toBeInTheDocument();
    expect(search).toHaveValue("vulcan");
    expect(category).toHaveValue("weapon");
    expect(search).toHaveFocus();
    expect(search).toHaveProperty("selectionStart", 0);
    expect(search).toHaveProperty("selectionEnd", "vulcan".length);
    expect(screen.getByText("Staged Vulcan Cannon (+3). 1 pending addition.")).toHaveAttribute("aria-live", "polite");
    expect(screen.getByRole("button", { name: /Vulcan Cannon.*Staged \+3/i })).toBeInTheDocument();

    fireEvent.change(search, { target: { value: "railgun" } });
    fireEvent.click(screen.getByRole("button", { name: /Railgun/i }));
    expect(screen.getByLabelText("Amount of Railgun to add")).toHaveValue(1);
    fireEvent.change(screen.getByLabelText("Amount of Railgun to add"), { target: { value: "7" } });
    fireEvent.click(screen.getByRole("button", { name: "Stage addition" }));

    expect(onAdd).toHaveBeenNthCalledWith(2, ITEMS[1], "7");
    expect(screen.getByText("Staged Railgun (+7). 2 pending additions.")).toBeInTheDocument();
    expect(screen.getByText("2 staged additions")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Done adding" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invoker).toHaveFocus();
  });

  it("preloads and updates a repeated staged item instead of duplicating it", () => {
    const onAdd = vi.fn();
    render(<DialogHarness onAdd={onAdd} />);
    openDialog();

    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon/i }));
    fireEvent.change(screen.getByLabelText("Amount of Vulcan Cannon to add"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Stage addition" }));

    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon.*Staged \+3/i }));
    expect(screen.getByLabelText("Amount of Vulcan Cannon to add")).toHaveValue(3);
    fireEvent.change(screen.getByLabelText("Amount of Vulcan Cannon to add"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Update staged addition" }));

    expect(onAdd).toHaveBeenCalledTimes(2);
    expect(onAdd).toHaveBeenLastCalledWith(ITEMS[0], "5");
    expect(screen.getByText("Updated Vulcan Cannon (+5). 1 pending addition.")).toBeInTheDocument();
    expect(screen.getByText("1 staged addition")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Vulcan Cannon.*Staged \+5/i })).toBeInTheDocument();
  });

  it("keeps invalid quantities in the open dialog without staging them", () => {
    const onAdd = vi.fn();
    render(<DialogHarness onAdd={onAdd} />);
    openDialog();

    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon/i }));
    fireEvent.change(screen.getByLabelText("Amount of Vulcan Cannon to add"), { target: { value: "1.5" } });

    expect(screen.getByRole("alert")).toHaveTextContent("This item requires a whole quantity.");
    expect(screen.getByRole("button", { name: "Stage addition" })).toBeDisabled();
    expect(onAdd).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Add installed item" })).toBeInTheDocument();
  });

  it("retains the existing close button and backdrop dismissal paths", () => {
    render(<DialogHarness />);
    const invoker = openDialog();

    fireEvent.click(screen.getByRole("button", { name: "Close item catalog" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invoker).toHaveFocus();

    openDialog();
    const dialog = screen.getByRole("dialog", { name: "Add installed item" });
    const backdrop = dialog.parentElement;
    if (!backdrop) throw new Error("Catalog dialog backdrop is missing");
    fireEvent.mouseDown(dialog);
    expect(dialog).toBeInTheDocument();
    fireEvent.mouseDown(backdrop);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invoker).toHaveFocus();
  });
});
