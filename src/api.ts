import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { demoApply, demoBackups, demoDiagnostics, demoReview, demoSaves, demoSnapshot } from "./demo";
import type {
  ApplyMode,
  ApplyResult,
  BackupSummary,
  Diagnostics,
  DiscoveryResult,
  Edit,
  GameSettingsApplyResult,
  GameSettingsProfile,
  GameSettingsSnapshot,
  GameSettingsValues,
  PortraitPayload,
  Review,
  RecoveryState,
  SaveSnapshot,
  SaveSummary,
} from "./types";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

export const isDesktopRuntime = () => Boolean(window.__TAURI_INTERNALS__);

const call = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
  return invoke<T>(command, args);
};

const demoGameSettingsValues: GameSettingsValues = {
  playerMaxLevel: 15,
  skillPointsPerLevel: 1,
  storyPointsPerLevel: 4,
  officerMaxLevel: 5,
  officerMaxEliteSkills: 1,
};

const demoInstallation = {
  installationId: "installation-demo",
  displayName: "Starsector demo installation",
  displayPath: "Local Starsector installation",
  detectedVersion: "0.98a-RC8",
  savesRootAvailable: true,
};

export const api = {
  async discoverInstallations(): Promise<DiscoveryResult> {
    if (!isDesktopRuntime()) return { installations: [demoInstallation], registeredRoots: [] };
    return call<DiscoveryResult>("discover_installations");
  },

  async startupRecoveryState(): Promise<RecoveryState> {
    if (!isDesktopRuntime()) return { status: "clear", items: [] };
    return call<RecoveryState>("startup_recovery_state");
  },

  async onPathsDropped(handler: (paths: string[]) => void | Promise<void>): Promise<() => void> {
    if (!isDesktopRuntime()) return () => undefined;
    return getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === "drop") void handler(event.payload.paths);
    });
  },

  async scanSaves(rootId?: string): Promise<SaveSummary[]> {
    if (!isDesktopRuntime()) return demoSaves();
    return call<SaveSummary[]>("scan_saves", { rootId });
  },

  async chooseAndRegisterRoot(): Promise<void> {
    if (!isDesktopRuntime()) return;
    const selected = await open({ directory: true, multiple: false, title: "Choose Starsector installation or saves folder" });
    if (typeof selected === "string") {
      await call("register_root", { path: selected });
    }
  },

  async registerRoot(path: string): Promise<void> {
    if (!isDesktopRuntime()) return;
    await call("register_root", { path });
  },

  async forgetRoot(rootId: string): Promise<void> {
    if (!isDesktopRuntime()) return;
    await call<void>("forget_root", { rootId });
  },

  async listGameSettingsProfiles(): Promise<GameSettingsProfile[]> {
    if (!isDesktopRuntime()) {
      return [{
        profileId: "builtin-vanilla-rc8",
        name: "Vanilla RC8",
        values: { ...demoGameSettingsValues },
        builtIn: true,
      }];
    }
    return call<GameSettingsProfile[]>("list_game_settings_profiles");
  },

  async loadGameSettings(installationId: string): Promise<GameSettingsSnapshot> {
    if (!isDesktopRuntime()) {
      return {
        installationId,
        displayName: demoInstallation.displayName,
        displayPath: "Local Starsector installation/starsector-core/data/config/settings.json",
        values: { ...demoGameSettingsValues },
        revision: "0".repeat(64),
        writable: true,
      };
    }
    return call<GameSettingsSnapshot>("load_game_settings", { installationId });
  },

  async saveGameSettingsProfile(
    profileId: string | null,
    name: string,
    values: GameSettingsValues,
  ): Promise<GameSettingsProfile> {
    if (!isDesktopRuntime()) {
      return {
        profileId: profileId ?? "profile-demo",
        name,
        values: { ...values },
        builtIn: false,
      };
    }
    return call<GameSettingsProfile>("save_game_settings_profile", { profileId, name, values });
  },

  async deleteGameSettingsProfile(profileId: string): Promise<void> {
    if (!isDesktopRuntime()) return;
    await call<void>("delete_game_settings_profile", { profileId });
  },

  async applyGameSettings(
    installationId: string,
    expectedRevision: string,
    values: GameSettingsValues,
  ): Promise<GameSettingsApplyResult> {
    if (!isDesktopRuntime()) {
      return {
        snapshot: {
          installationId,
          displayName: demoInstallation.displayName,
          displayPath: "Local Starsector installation/starsector-core/data/config/settings.json",
          values: { ...values },
          revision: "1".repeat(64),
          writable: true,
        },
        backupId: "settings-backup-demo",
        message: "Game settings were backed up and updated. Restart Starsector to use them.",
      };
    }
    return call<GameSettingsApplyResult>("apply_game_settings", {
      installationId,
      expectedRevision,
      values,
    });
  },

  async openSave(saveId: string): Promise<SaveSnapshot> {
    if (!isDesktopRuntime()) return demoSnapshot(saveId);
    return call<SaveSnapshot>("open_save", { saveId });
  },

  async closeSession(sessionId: string): Promise<void> {
    if (!isDesktopRuntime()) return;
    await call<void>("close_session", { sessionId });
  },

  async loadPortrait(sessionId: string, portraitId: string): Promise<PortraitPayload> {
    if (!isDesktopRuntime()) return { portraitId, mimeType: "image/png", dataBase64: "" };
    return call<PortraitPayload>("load_portrait", { sessionId, portraitId });
  },

  async unlockProtectedSave(sessionId: string): Promise<SaveSnapshot> {
    if (!isDesktopRuntime()) {
      const snapshot = demoSnapshot(sessionId.replace("demo-session-", ""));
      snapshot.protectedLocked = false;
      return snapshot;
    }
    return call<SaveSnapshot>("unlock_protected_save", {
      sessionId,
      acknowledgement: true,
    });
  },

  async prepareReview(sessionId: string, revision: string, edits: Edit[]): Promise<Review> {
    if (!isDesktopRuntime()) return demoReview(revision, edits);
    return call<Review>("prepare_review", { sessionId, edits });
  },

  async discardReview(reviewId: string): Promise<void> {
    if (!isDesktopRuntime()) return;
    await call<void>("discard_review", { reviewId });
  },

  async applyReview(reviewId: string, mode: ApplyMode, acknowledgement: boolean): Promise<ApplyResult> {
    if (!isDesktopRuntime()) return demoApply();
    return call<ApplyResult>("apply_review", { reviewId, mode, acknowledgement });
  },

  async chooseCopyRoot(): Promise<string | null> {
    if (!isDesktopRuntime()) return "Demo save root";
    const selected = await open({ directory: true, multiple: false, title: "Choose destination saves folder" });
    return typeof selected === "string" ? selected : null;
  },

  async listBackups(saveId: string): Promise<BackupSummary[]> {
    if (!isDesktopRuntime()) return demoBackups();
    return call<BackupSummary[]>("list_backups", { saveId });
  },

  async prepareRestore(sessionId: string, backupId: string): Promise<Review> {
    if (!isDesktopRuntime()) {
      return {
        ...demoReview("demo-revision", []),
        reviewId: `restore-${backupId}`,
        canApply: true,
        changes: [{ key: "restore", section: "Save", label: "Restore backup", before: "Current pair", after: backupId, derived: null }],
      };
    }
    return call<Review>("prepare_restore", { sessionId, backupId });
  },

  async applyRestore(reviewId: string, acknowledgement: boolean): Promise<ApplyResult> {
    if (!isDesktopRuntime()) return demoApply();
    return call<ApplyResult>("apply_restore", { reviewId, acknowledgement });
  },

  async exportDiagnostics(): Promise<Diagnostics> {
    if (!isDesktopRuntime()) return demoDiagnostics();
    return call<Diagnostics>("export_diagnostics");
  },
};
