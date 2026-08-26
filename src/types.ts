// IPC models are generated from the Rust command boundary by ts-rs.
export type { ApplyMode } from "../src-tauri/bindings/ApplyMode";
export type { ApplyResult } from "../src-tauri/bindings/ApplyResult";
export type { AddableItemKind } from "../src-tauri/bindings/AddableItemKind";
export type { AddableItemView } from "../src-tauri/bindings/AddableItemView";
export type { BackupSummary } from "../src-tauri/bindings/BackupSummary";
export type { CatalogView } from "../src-tauri/bindings/CatalogView";
export type { CharacterView } from "../src-tauri/bindings/CharacterView";
export type { ColonyResourceStackView } from "../src-tauri/bindings/ColonyResourceStackView";
export type { ColonyResourcesView } from "../src-tauri/bindings/ColonyResourcesView";
export type { ColonyView } from "../src-tauri/bindings/ColonyView";
export type { CompatibilityState } from "../src-tauri/bindings/CompatibilityState";
export type { Diagnostics } from "../src-tauri/bindings/Diagnostics";
export type { DiscoveryResult } from "../src-tauri/bindings/DiscoveryResult";
export type { Edit } from "../src-tauri/bindings/Edit";
export type { ErrorCode } from "../src-tauri/bindings/ErrorCode";
export type { FieldCapability } from "../src-tauri/bindings/FieldCapability";
export type { GameSettingsApplyResult } from "../src-tauri/bindings/GameSettingsApplyResult";
export type { GameSettingsProfile } from "../src-tauri/bindings/GameSettingsProfile";
export type { GameSettingsSnapshot } from "../src-tauri/bindings/GameSettingsSnapshot";
export type { GameSettingsValues } from "../src-tauri/bindings/GameSettingsValues";
export type { InstallationInfo } from "../src-tauri/bindings/InstallationInfo";
export type { InventoryKind } from "../src-tauri/bindings/InventoryKind";
export type { InventoryStackView } from "../src-tauri/bindings/InventoryStackView";
export type { InventoryView } from "../src-tauri/bindings/InventoryView";
export type { OfficerView } from "../src-tauri/bindings/OfficerView";
export type { PortraitPayload } from "../src-tauri/bindings/PortraitPayload";
export type { PortraitView } from "../src-tauri/bindings/PortraitView";
export type { RecoveryItem } from "../src-tauri/bindings/RecoveryItem";
export type { RecoveryState } from "../src-tauri/bindings/RecoveryState";
export type { RelationView } from "../src-tauri/bindings/RelationView";
export type { Review } from "../src-tauri/bindings/Review";
export type { ReviewChange } from "../src-tauri/bindings/ReviewChange";
export type { SaveSnapshot } from "../src-tauri/bindings/SaveSnapshot";
export type { SaveRoot } from "../src-tauri/bindings/SaveRoot";
export type { SaveSummary } from "../src-tauri/bindings/SaveSummary";
export type { SkillView } from "../src-tauri/bindings/SkillView";
export type { StorageStackView } from "../src-tauri/bindings/StorageStackView";
export type { StorageView } from "../src-tauri/bindings/StorageView";

import type { CommandError } from "../src-tauri/bindings/CommandError";

export type ApiFailure = Omit<CommandError, "code"> & {
  code: CommandError["code"] | "UNEXPECTED";
};

export const isApiFailure = (value: unknown): value is ApiFailure => {
  return Boolean(
    value &&
      typeof value === "object" &&
      "code" in value &&
      "message" in value &&
      "retryable" in value &&
      "detail" in value &&
      "diskChanged" in value,
  );
};
