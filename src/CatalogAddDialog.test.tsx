import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { CatalogAddDialog } from "./CatalogAddDialog";
import type { AddableItemView } from "./types";

const ITEMS: AddableItemView[] = [{
  id: "catalog-item-vulcan",
  itemId: "vulcan",
  name: "Vulcan Cannon",
  kind: "weapon",
  cargoSpacePerUnit: "5",
  maxQuantity: "1000000",
  localResourcesEligible: false,
}];

function DialogHarness({ onAdd = vi.fn() }: { onAdd?: (item: AddableItemView, quantity: string) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Add item</button>
      {open ? (
        <CatalogAddDialog
          title="Add installed item"
          description="Choose one supported item."
          items={ITEMS}
          existingItemIds={new Set()}
          onClose={() => setOpen(false)}
          onAdd={onAdd}
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
    const stage = screen.getByRole("button", { name: "Stage addition" });
    stage.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(close).toHaveFocus();

    close.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(stage).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invoker).toHaveFocus();
  });
});
