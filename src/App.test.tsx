import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { api } from "./api";
import { demoSnapshot } from "./demo";

afterEach(() => vi.restoreAllMocks());

describe("Ludd’s Blessing app shell", () => {
  it("explains bounded automatic discovery and keeps manual folder selection visible", async () => {
    vi.spyOn(api, "scanSaves").mockResolvedValue([]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "No save folders found" })).toBeInTheDocument();
    expect(screen.getByText(/Refresh checks the common locations without crawling your computer/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Choose folder" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByText(/configured save paths and bounded platform-standard locations/i)).toBeInTheDocument();
    expect(screen.getByText(/never crawls an entire drive or home directory/i)).toBeInTheDocument();
    expect(screen.getByLabelText("Starsector installation or saves folder")).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Game Settings Profiles" })).toBeInTheDocument();
  });

  it("loads, stages, and revision-binds supported installation game settings", async () => {
    vi.spyOn(api, "discoverInstallations").mockResolvedValue({
      installations: [{
        installationId: "installation-verified",
        displayName: "Verified Starsector",
        displayPath: "Local installation",
        detectedVersion: "0.98a-RC8",
        savesRootAvailable: true,
      }],
      registeredRoots: [],
    });
    vi.spyOn(api, "listGameSettingsProfiles").mockResolvedValue([{
      profileId: "builtin-vanilla-rc8",
      name: "Vanilla RC8",
      values: { playerMaxLevel: 15, skillPointsPerLevel: 1, storyPointsPerLevel: 4, officerMaxLevel: 5, officerMaxEliteSkills: 1 },
      builtIn: true,
    }]);
    vi.spyOn(api, "loadGameSettings").mockResolvedValue({
      installationId: "installation-verified",
      displayName: "Verified Starsector",
      displayPath: "Local installation/starsector-core/data/config/settings.json",
      values: { playerMaxLevel: 15, skillPointsPerLevel: 1, storyPointsPerLevel: 4, officerMaxLevel: 5, officerMaxEliteSkills: 1 },
      revision: "a".repeat(64),
      writable: true,
    });
    const applySettings = vi.spyOn(api, "applyGameSettings").mockResolvedValue({
      snapshot: {
        installationId: "installation-verified",
        displayName: "Verified Starsector",
        displayPath: "Local installation/starsector-core/data/config/settings.json",
        values: { playerMaxLevel: 30, skillPointsPerLevel: 1, storyPointsPerLevel: 4, officerMaxLevel: 5, officerMaxEliteSkills: 1 },
        revision: "b".repeat(64),
        writable: true,
      },
      backupId: "settings-backup-test",
      message: "Settings updated.",
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const maxLevel = await screen.findByLabelText("Player maximum level");
    fireEvent.change(maxLevel, { target: { value: "30" } });
    expect(screen.getByText("Unsaved installation changes")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create backup & apply settings" }));

    await waitFor(() => expect(applySettings).toHaveBeenCalledWith(
      "installation-verified",
      "a".repeat(64),
      expect.objectContaining({ playerMaxLevel: 30 }),
    ));
    expect(await screen.findByText("Matches the loaded settings file")).toBeInTheDocument();
  });

  it("lists and forgets only remembered save roots", async () => {
    vi.spyOn(api, "discoverInstallations").mockResolvedValue({
      installations: [],
      registeredRoots: [{
        rootId: "root-remembered",
        displayName: "Old campaigns",
        displayPath: "Documents/Starsector/saves",
        available: true,
        writable: true,
      }],
    });
    const forgetRoot = vi.spyOn(api, "forgetRoot").mockResolvedValue();
    vi.spyOn(api, "scanSaves").mockResolvedValue([]);

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByText("Old campaigns")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Forget" }));
    await waitFor(() => expect(forgetRoot).toHaveBeenCalledWith("root-remembered"));
  });

  it("protects unapplied game settings when navigating away", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const maxLevel = await screen.findByLabelText("Player maximum level");
    fireEvent.change(maxLevel, { target: { value: "25" } });
    fireEvent.click(screen.getByRole("button", { name: "Saves" }));

    expect(confirm).toHaveBeenCalledWith(expect.stringMatching(/Discard the unapplied game settings/i));
    expect(screen.getByRole("heading", { name: "Game Settings Profiles" })).toBeInTheDocument();

    confirm.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "Saves" }));
    expect(await screen.findByRole("heading", { name: "Campaign records" })).toBeInTheDocument();
  });

  it("moves focus to the refreshed page region after navigation", async () => {
    render(<App />);
    await screen.findByText("Mira Venn");
    document.documentElement.scrollTop = 400;
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    await waitFor(() => expect(document.getElementById("main-content")).toHaveFocus());
    expect(document.documentElement.scrollTop).toBe(0);
    expect(screen.getByRole("link", { name: "Skip to main content" })).toHaveAttribute("href", "#main-content");
  });

  it("discovers demo saves and distinguishes editable from preview-only records", async () => {
    render(<App />);

    expect(await screen.findByText("Mira Venn")).toBeInTheDocument();
    expect(screen.getByText("Archive Walker")).toBeInTheDocument();
    expect(screen.getByText("Editable")).toBeInTheDocument();
    expect(screen.getByText("Read-only preview")).toBeInTheDocument();
  });

  it("stages a supported field and prepares a semantic review", async () => {
    render(<App />);
    const openButtons = await screen.findAllByRole("button", { name: "Open editor" });
    fireEvent.click(openButtons[0]);

    expect(await screen.findByRole("heading", { name: "Name and portrait" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Luddic portrait 01" })).toHaveAttribute("aria-pressed", "true");
    const balance = screen.getByLabelText("Current balance");
    fireEvent.change(balance, { target: { value: "90000000" } });

    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));
    expect(await screen.findByRole("heading", { name: "Review staged changes" })).toBeInTheDocument();
    expect(screen.getByText("Game activity check")).toBeInTheDocument();
    expect(screen.getByText(/Starsector may remain open when this target save is not currently loaded/i)).toBeInTheDocument();
    expect(screen.getByText(/Apply rechecks game activity and blocks every possibly active save/i)).toBeInTheDocument();
    expect(screen.getByText(/Keep only one Starsector instance open/i)).toBeInTheDocument();
    expect(screen.getByText(/do not load or switch to this target while Apply or Restore is running/i)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("Credits")).toBeInTheDocument());
    expect(screen.getByText("82418096")).toBeInTheDocument();
    expect(screen.getByText("90000000")).toBeInTheDocument();
  });

  it("releases superseded native sessions after a replacement opens", async () => {
    const first = demoSnapshot("demo-mira");
    first.sessionId = "session-first";
    const second = demoSnapshot("demo-mira");
    second.sessionId = "session-second";
    vi.spyOn(api, "openSave")
      .mockResolvedValueOnce(first)
      .mockResolvedValueOnce(second);
    const closeSession = vi.spyOn(api, "closeSession").mockResolvedValue();

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Saves" }));
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);

    await waitFor(() => expect(closeSession).toHaveBeenCalledWith("session-first"));
  });

  it("discards abandoned native reviews when the draft is cleared", async () => {
    const discardReview = vi.spyOn(api, "discardReview").mockResolvedValue();
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.change(screen.getByLabelText("Current balance"), { target: { value: "90000000" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));
    await screen.findByRole("heading", { name: "Review staged changes" });
    fireEvent.click(screen.getByRole("button", { name: "Discard draft" }));

    await waitFor(() => expect(discardReview).toHaveBeenCalledTimes(1));
  });

  it("routes backup restores through the dedicated restore review", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });

    fireEvent.click(screen.getByRole("button", { name: "Backups" }));
    fireEvent.click(await screen.findByRole("button", { name: "Review restore" }));

    expect(await screen.findByRole("heading", { name: "Review backup restore" })).toBeInTheDocument();
    expect(screen.getByText("Game activity check")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create backup & restore" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save a copy" })).not.toBeInTheDocument();
  });

  it("keeps unknown mod skills read-only", async () => {
    render(<App />);
    const openButtons = await screen.findAllByRole("button", { name: "Open editor" });
    fireEvent.click(openButtons[0]);
    expect(await screen.findByText("mod_unknown")).toBeInTheDocument();
    expect(screen.getByText("No trusted local skill definition.")).toBeInTheDocument();
  });

  it("shows an empty officer roster for a new campaign", async () => {
    const snapshot = demoSnapshot("demo-mira");
    snapshot.officers = [];
    vi.spyOn(api, "openSave").mockResolvedValue(snapshot);

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Officers" }));

    expect(await screen.findByRole("heading", { name: "No officers found" })).toBeInTheDocument();
    expect(screen.getByText(/authoritative officer roster/i)).toBeInTheDocument();
  });

  it("keeps the officer roster available and bulk-sets every editable skill rank", async () => {
    const snapshot = demoSnapshot("demo-mira");
    snapshot.officers[0].skills.push({
      id: "mod_readonly_officer_skill",
      name: "Restricted Doctrine",
      group: "Unknown mod",
      rank: 1,
      maxRank: 2,
      editable: false,
      reason: "No trusted local skill definition.",
      iconId: null,
    });
    vi.spyOn(api, "openSave").mockResolvedValue(snapshot);

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Officers" }));

    const roster = await screen.findByLabelText("Officer roster");
    expect(roster).toHaveClass("officer-roster");
    expect(within(roster).getAllByRole("button")[0]).toHaveAttribute("aria-pressed", "true");
    const readonlySkill = screen.getByRole("radiogroup", { name: "Restricted Doctrine rank" });

    fireEvent.click(screen.getByRole("button", { name: "Make all Unlearned" }));
    const targetUnlearned = within(screen.getByRole("radiogroup", { name: "Target Analysis rank" })).getByRole("radio", { name: "Unlearned" });
    expect(targetUnlearned).toHaveAttribute("type", "radio");
    expect(targetUnlearned).toBeChecked();
    expect(within(readonlySkill).getByRole("radio", { name: "Learned" })).toBeChecked();
    expect(screen.getByRole("button", { name: /Review 3 changes/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Make all Learned" }));
    expect(within(screen.getByRole("radiogroup", { name: "Energy Weapon Mastery rank" })).getByRole("radio", { name: "Learned" })).toBeChecked();
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Make all Elite" }));
    expect(within(screen.getByRole("radiogroup", { name: "Target Analysis rank" })).getByRole("radio", { name: "Elite" })).toBeChecked();
    expect(within(screen.getByRole("radiogroup", { name: "Helmsmanship rank" })).getByRole("radio", { name: "Elite" })).toBeChecked();
    expect(within(readonlySkill).getByRole("radio", { name: "Learned" })).toBeChecked();
    expect(screen.getByRole("button", { name: /Review 2 changes/i })).toBeInTheDocument();
  });

  it("filters inventory stacks and reviews an existing-stack quantity change", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });

    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    expect(await screen.findByRole("heading", { name: "Inventory" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Filter cargo category"), { target: { value: "weapons" } });
    expect(screen.getByLabelText("Railgun quantity")).toBeInTheDocument();
    expect(screen.queryByLabelText("Fuel quantity")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Railgun quantity"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));

    expect(await screen.findByRole("heading", { name: "Review staged changes" })).toBeInTheDocument();
    const inventoryReview = screen.getByRole("heading", { name: "Inventory" }).closest("section");
    expect(inventoryReview).not.toBeNull();
    expect(within(inventoryReview!).getByText("Railgun quantity")).toBeInTheDocument();
    expect(within(inventoryReview!).getByText("2")).toBeInTheDocument();
    expect(within(inventoryReview!).getByText("3")).toBeInTheDocument();
  });

  it("guards stack quantity rules and previews single-precision rounding", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    await screen.findByRole("heading", { name: "Inventory" });

    const fuel = screen.getByLabelText("Fuel quantity");
    fireEvent.change(fuel, { target: { value: "0" } });
    expect(fuel).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a resource quantity greater than zero after single-precision storage.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Review 1 changes/i })).not.toBeInTheDocument();

    fireEvent.change(fuel, { target: { value: "1e-50" } });
    expect(fuel).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a resource quantity greater than zero after single-precision storage.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Review 1 changes/i })).not.toBeInTheDocument();

    fireEvent.change(fuel, { target: { value: "0.1" } });
    expect(fuel).toHaveAttribute("aria-invalid", "false");
    expect(await screen.findByText("Saved as 0.10000000149011612")).toBeInTheDocument();
    expect(screen.getByText("775.025 / 1,800")).toBeInTheDocument();
    expect(screen.getByText("Projected from staged quantities.")).toBeInTheDocument();

    fireEvent.change(fuel, { target: { value: "1000000.01" } });
    expect(fuel).toHaveAttribute("aria-invalid", "false");
    expect(screen.getByText("Saved as 1000000")).toBeInTheDocument();
    expect(screen.queryByText("Maximum supported quantity is 1,000,000.")).not.toBeInTheDocument();

    const railgun = screen.getByLabelText("Railgun quantity");
    fireEvent.change(railgun, { target: { value: "1.5" } });
    expect(railgun).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a whole quantity of 1 or more.")).toBeInTheDocument();
  });

  it("keeps unknown inventory data visible and read-only", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    await screen.findByRole("heading", { name: "Inventory" });

    expect(screen.getByLabelText("mod_opaque_stack quantity")).toBeDisabled();
    expect(screen.getByText("Data: opaque mod payload")).toBeInTheDocument();
    expect(screen.getByText("No trusted local item definition; preserved as read-only.")).toBeInTheDocument();
  });

  it("provides a safe fallback explanation for a read-only cargo container", async () => {
    const openSave = api.openSave.bind(api);
    vi.spyOn(api, "openSave").mockImplementation(async (saveId) => {
      const snapshot = await openSave(saveId);
      return {
        ...snapshot,
        inventory: snapshot.inventory ? { ...snapshot.inventory, editable: false, reason: null } : null,
      };
    });

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    await screen.findByRole("heading", { name: "Inventory" });

    expect(screen.getByText("This cargo container is read-only because it could not be safely authorized for this session.")).toBeInTheDocument();
    const fuel = screen.getByLabelText("Fuel quantity");
    expect(fuel).toBeDisabled();
    expect(fuel.getAttribute("aria-describedby")).toContain("readonly-reason");
  });

  it("focuses colonies on editable holdings and reviews a storage-stack change", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));

    expect(await screen.findByRole("heading", { name: "Colonies" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Ships" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Storage, 4 stacks" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Local Resources, 3 stacks" })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    const colonyDetail = screen.getByRole("heading", { name: "Asteria Outpost" }).closest(".colony-detail");
    const tablist = screen.getByRole("tablist", { name: "Asteria Outpost holdings" });
    expect(colonyDetail?.children[1]).toBe(tablist.parentElement);
    expect(screen.queryByText("Colony details")).not.toBeInTheDocument();
    expect(screen.queryByText("Last recorded stability")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Supplies quantity"), { target: { value: "1201" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));

    expect(await screen.findByRole("heading", { name: "Review staged changes" })).toBeInTheDocument();
    const colonyReview = screen.getByRole("heading", { name: "Colonies" }).closest("section");
    expect(colonyReview).not.toBeNull();
    expect(within(colonyReview!).getByText("Asteria Outpost · Supplies")).toBeInTheDocument();
    expect(within(colonyReview!).getByText("1200")).toBeInTheDocument();
    expect(within(colonyReview!).getByText("1201")).toBeInTheDocument();
  });

  it("edits Local Resources without presenting a cargo-capacity limit", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    fireEvent.click(screen.getByRole("tab", { name: /Local resources/i }));
    expect(await screen.findByRole("heading", { name: "Asteria Outpost Local Resources" })).toBeInTheDocument();
    expect(screen.getByText("Local Resources · Economy-managed stockpile")).toBeInTheDocument();
    expect(screen.getByText(/no month-end charge or refund/i)).toBeInTheDocument();
    expect(screen.queryByText("Cargo space")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Metals quantity"), { target: { value: "1048" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));

    await screen.findByRole("heading", { name: "Review staged changes" });
    const colonyReview = screen.getByRole("heading", { name: "Colonies" }).closest("section");
    expect(colonyReview).not.toBeNull();
    expect(within(colonyReview!).getByText("Asteria Outpost · Metals local resource quantity")).toBeInTheDocument();
    expect(within(colonyReview!).getByText("1047")).toBeInTheDocument();
    expect(within(colonyReview!).getByText("1048")).toBeInTheDocument();
  });

  it("compacts saved resource float noise without staging or losing the exact edit value", async () => {
    const snapshot = demoSnapshot("demo-mira");
    const resources = snapshot.colonies[0].localResources!;
    resources.stacks[0] = {
      ...resources.stacks[0],
      itemId: "crew",
      name: "Crew",
      quantity: "721.32007",
    };
    resources.stacks[1] = {
      ...resources.stacks[1],
      quantity: "1e-45",
    };
    vi.spyOn(api, "openSave").mockResolvedValue(snapshot);

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });
    fireEvent.click(screen.getByRole("tab", { name: /Local resources/i }));

    const crew = screen.getByLabelText("Crew quantity");
    expect(crew).toHaveValue(721.32);
    expect(crew).toHaveAttribute("title", "Exact saved value: 721.32007");
    expect(screen.getByLabelText("Fuel quantity")).toHaveValue(1e-45);
    expect(screen.queryByRole("button", { name: /Review 1 changes/i })).not.toBeInTheDocument();

    fireEvent.focus(crew);
    expect(crew).toHaveValue(721.32007);
    expect(screen.queryByRole("button", { name: /Review 1 changes/i })).not.toBeInTheDocument();
    fireEvent.blur(crew);
    expect(crew).toHaveValue(721.32);

    fireEvent.focus(crew);
    fireEvent.change(crew, { target: { value: "722.5" } });
    fireEvent.blur(crew);
    expect(crew).toHaveValue(722.5);
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Reset Crew quantity" }));
    expect(crew).toHaveValue(721.32);
    expect(screen.queryByRole("button", { name: /Review 1 changes/i })).not.toBeInTheDocument();
  });

  it("keeps storage and Local Resources drafts isolated and supports keyboard tab switching", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    fireEvent.change(screen.getByLabelText("Supplies quantity"), { target: { value: "1201" } });
    const resourcesTab = screen.getByRole("tab", { name: /Local resources/i });
    fireEvent.click(resourcesTab);
    fireEvent.change(screen.getByLabelText("Metals quantity"), { target: { value: "1048" } });
    expect(screen.getByRole("button", { name: /Review 2 changes/i })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reset section" }));
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();

    fireEvent.keyDown(resourcesTab, { key: "ArrowLeft" });
    const storageTab = screen.getByRole("tab", { name: /Storage/i });
    expect(storageTab).toHaveAttribute("aria-selected", "true");
    expect(storageTab).toHaveFocus();
    expect(screen.getByLabelText("Supplies quantity")).toHaveValue(1201);
  });

  it("defaults to Local Resources when colony storage is unavailable", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    fireEvent.click(screen.getByRole("button", { name: /Lacaille Anchorage/ }));
    expect(await screen.findByRole("heading", { name: "Lacaille Anchorage" })).toBeInTheDocument();
    expect(screen.getAllByText("Lacaille Habitat")).toHaveLength(2);
    expect(screen.getByRole("tab", { name: /Storage/i })).not.toBeDisabled();
    expect(screen.getByRole("tab", { name: /Local resources/i })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("heading", { name: "Lacaille Anchorage Local Resources" })).toBeInTheDocument();
    expect(screen.getByLabelText("Food quantity")).toBeInTheDocument();
    expect(screen.getByText("No unique storage container was found; Local Resources remains available.")).toBeInTheDocument();

    const storageTab = screen.getByRole("tab", { name: /Storage/i });
    fireEvent.click(storageTab);
    expect(screen.getByRole("heading", { name: "Storage unavailable" })).toBeInTheDocument();
    expect(screen.getByRole("tabpanel")).toHaveAttribute("aria-labelledby", storageTab.id);
  });

  it("remembers each colony's editable holdings tab", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    fireEvent.click(screen.getByRole("tab", { name: /Local resources/i }));
    expect(screen.getByRole("tab", { name: /Local resources/i })).toHaveAttribute("aria-selected", "true");
    fireEvent.click(screen.getByRole("button", { name: /Lacaille Anchorage/ }));
    fireEvent.click(screen.getByRole("tab", { name: /Storage/i }));
    expect(screen.getByRole("heading", { name: "Storage unavailable" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Asteria Outpost/ }));
    expect(screen.getByRole("tab", { name: /Local resources/i })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("heading", { name: "Asteria Outpost Local Resources" })).toBeInTheDocument();
  });

  it("keeps an unavailable Local Resources tab selectable and explanatory", async () => {
    const snapshot = demoSnapshot("demo-mira");
    snapshot.colonies[0].localResources = null;
    vi.spyOn(api, "openSave").mockResolvedValue(snapshot);
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    const resourcesTab = screen.getByRole("tab", { name: "Local Resources, unavailable" });
    expect(resourcesTab).not.toBeDisabled();
    fireEvent.click(resourcesTab);
    expect(screen.getByRole("heading", { name: "Local Resources unavailable" })).toBeInTheDocument();
    expect(screen.getByText(/no economy data is guessed/i)).toBeInTheDocument();
  });

  it("stages a catalog-backed weapon addition for colony storage", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });

    fireEvent.click(screen.getByRole("button", { name: "Add item" }));
    expect(screen.getByRole("dialog", { name: "Add to Asteria Outpost storage" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Search item catalog"), { target: { value: "vulcan cannon" } });
    fireEvent.click(screen.getByRole("button", { name: /Vulcan Cannon.*Weapons/i }));
    fireEvent.change(screen.getByLabelText("Amount of Vulcan Cannon to add"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Stage addition" }));

    expect(screen.getByText("Pending additions")).toBeInTheDocument();
    expect(screen.getByText("vulcan · +3")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));
    await screen.findByRole("heading", { name: "Review staged changes" });
    expect(screen.getByText("Asteria Outpost · Add Vulcan Cannon")).toBeInTheDocument();
  });

  it("limits Local Resources additions to economic commodities", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });
    fireEvent.click(screen.getByRole("tab", { name: /Local resources/i }));
    fireEvent.click(screen.getByRole("button", { name: "Add commodity" }));

    const dialog = screen.getByRole("dialog", { name: "Add to Asteria Outpost Local Resources" });
    expect(within(dialog).getByRole("button", { name: /Food.*Commodities/i })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /Vulcan/i })).not.toBeInTheDocument();
  });

  it("offers individual installed blueprints as reviewed storage additions", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Colonies" }));
    await screen.findByRole("heading", { name: "Colonies" });
    fireEvent.click(screen.getByRole("button", { name: "Add item" }));

    fireEvent.change(screen.getByLabelText("Catalog category"), { target: { value: "ship_blueprint" } });
    const blueprint = screen.getByRole("button", { name: /Hammerhead blueprint.*Ship blueprints/i });
    fireEvent.click(blueprint);
    fireEvent.click(screen.getByRole("button", { name: "Stage addition" }));
    expect(screen.getByText("hammerhead · +1")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();
  });

  it("provides a visible app-level refresh that reloads the open save", async () => {
    const scanSaves = vi.spyOn(api, "scanSaves");
    const openSave = vi.spyOn(api, "openSave");
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });

    fireEvent.click(screen.getByRole("button", { name: "Refresh save data" }));
    await waitFor(() => expect(scanSaves).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(openSave).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("Library and open save refreshed.")).toBeInTheDocument();
  });

  it("does not refresh away staged work without confirmation", async () => {
    const scanSaves = vi.spyOn(api, "scanSaves");
    vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    fireEvent.change(await screen.findByLabelText("Current balance"), { target: { value: "90000000" } });

    fireEvent.click(screen.getByRole("button", { name: "Refresh save data" }));
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();
    expect(scanSaves).toHaveBeenCalledTimes(1);
  });

  it("resets an inventory section without leaving staged changes", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    await screen.findByRole("heading", { name: "Inventory" });

    fireEvent.change(screen.getByLabelText("Fuel quantity"), { target: { value: "421" } });
    fireEvent.change(screen.getByLabelText("Railgun quantity"), { target: { value: "3" } });
    expect(screen.getByRole("button", { name: /Review 2 changes/i })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reset section" }));
    expect(screen.queryByRole("button", { name: /Review \d+ changes/i })).not.toBeInTheDocument();
    expect(screen.getByLabelText("Fuel quantity")).toHaveValue(420);
    expect(screen.getByLabelText("Railgun quantity")).toHaveValue(2);
  });

  it("requires warning acknowledgement again after a row reset rebuilds review", async () => {
    const prepareReview = api.prepareReview.bind(api);
    vi.spyOn(api, "prepareReview").mockImplementation(async (sessionId, revision, edits) => ({
      ...await prepareReview(sessionId, revision, edits),
      warnings: ["Review the staged cargo warning."],
    }));

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    await screen.findByRole("heading", { name: "Name and portrait" });
    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    await screen.findByRole("heading", { name: "Inventory" });
    fireEvent.change(screen.getByLabelText("Fuel quantity"), { target: { value: "421" } });
    fireEvent.change(screen.getByLabelText("Railgun quantity"), { target: { value: "3" } });

    fireEvent.click(screen.getByRole("button", { name: /Review 2 changes/i }));
    const firstAcknowledgement = await screen.findByLabelText(/I reviewed the warnings/i);
    fireEvent.click(firstAcknowledgement);
    expect(firstAcknowledgement).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Inventory" }));
    fireEvent.click(await screen.findByRole("button", { name: "Reset Railgun quantity" }));
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));

    const rebuiltAcknowledgement = await screen.findByLabelText(/I reviewed the warnings/i);
    expect(rebuiltAcknowledgement).not.toBeChecked();
  });

  it("offers an actionable review for interrupted transaction recovery", async () => {
    vi.spyOn(api, "startupRecoveryState").mockResolvedValueOnce({
      status: "recovery_required",
      items: [{
        transactionId: "opaque-recovery-token",
        saveId: "opaque-save-id",
        summary: "Interrupted replacement for a local save.",
        lastCompletedPhase: "campaign replaced",
      }],
    });
    const prepareRestore = vi.spyOn(api, "prepareRestore");

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Review recovery" }));

    expect(prepareRestore).toHaveBeenCalledWith("opaque-recovery-token", "opaque-recovery-token");
    expect(await screen.findByRole("heading", { name: "Review backup restore" })).toBeInTheDocument();
  });

  it("discards a consumed review after a failed apply while preserving the draft", async () => {
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    fireEvent.change(await screen.findByLabelText("Current balance"), { target: { value: "90000000" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));
    await screen.findByRole("heading", { name: "Review staged changes" });

    const applyReview = vi.spyOn(api, "applyReview").mockRejectedValueOnce({
      code: "GAME_RUNNING",
      message: "This save is currently loaded in Starsector. Switch to another save before applying this review.",
      retryable: true,
      detail: null,
      diskChanged: null,
    });
    fireEvent.click(screen.getByRole("button", { name: "Create backup & apply" }));

    expect(await screen.findByText("GAME_RUNNING")).toBeInTheDocument();
    expect(screen.getByText(/This save is currently loaded in Starsector/i)).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Prepare secure review" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Review 1 changes/i })).toBeInTheDocument();
    expect(applyReview).toHaveBeenCalledTimes(1);
  });

  it("refreshes actionable recovery immediately when an apply requires recovery", async () => {
    const startupRecoveryState = vi.spyOn(api, "startupRecoveryState")
      .mockResolvedValueOnce({ status: "clear", items: [] })
      .mockResolvedValueOnce({
        status: "recovery_required",
        items: [{
          transactionId: "new-recovery-token",
          saveId: "affected-save-id",
          summary: "A replacement was interrupted and must be recovered.",
          lastCompletedPhase: "campaign replaced",
        }],
      });

    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Open editor" }))[0]);
    fireEvent.change(await screen.findByLabelText("Current balance"), { target: { value: "90000000" } });
    fireEvent.click(screen.getByRole("button", { name: /Review 1 changes/i }));
    await screen.findByRole("heading", { name: "Review staged changes" });

    vi.spyOn(api, "applyReview").mockRejectedValueOnce({
      code: "RECOVERY_REQUIRED",
      message: "Recover the interrupted transaction before another write.",
      retryable: false,
      detail: null,
      diskChanged: null,
    });
    fireEvent.click(screen.getByRole("button", { name: "Create backup & apply" }));

    await waitFor(() => expect(startupRecoveryState).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("RECOVERY_REQUIRED")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review recovery" })).toBeInTheDocument();
  });
});
