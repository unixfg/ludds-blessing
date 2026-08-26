use tauri::State;

use crate::error::CommandError;
use crate::game_settings::{
    GameSettingsApplyResult, GameSettingsProfile, GameSettingsProfileId, GameSettingsSnapshot,
    GameSettingsValues,
};
use crate::models::*;
use crate::service::AppState;

pub type CommandResult<T> = Result<T, CommandError>;

#[tauri::command]
pub fn discover_installations(state: State<'_, AppState>) -> CommandResult<DiscoveryResult> {
    state.service.discover_installations()
}

#[tauri::command(rename_all = "camelCase")]
pub fn register_root(path: String, state: State<'_, AppState>) -> CommandResult<SaveRoot> {
    state.service.register_root(&path)
}

#[tauri::command(rename_all = "camelCase")]
pub fn forget_root(root_id: RootId, state: State<'_, AppState>) -> CommandResult<()> {
    state.service.forget_root(&root_id)
}

#[tauri::command]
pub fn list_game_settings_profiles(
    state: State<'_, AppState>,
) -> CommandResult<Vec<GameSettingsProfile>> {
    state.service.list_game_settings_profiles()
}

#[tauri::command(rename_all = "camelCase")]
pub fn load_game_settings(
    installation_id: InstallationId,
    state: State<'_, AppState>,
) -> CommandResult<GameSettingsSnapshot> {
    state.service.load_game_settings(&installation_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn save_game_settings_profile(
    profile_id: Option<GameSettingsProfileId>,
    name: String,
    values: GameSettingsValues,
    state: State<'_, AppState>,
) -> CommandResult<GameSettingsProfile> {
    state
        .service
        .save_game_settings_profile(profile_id.as_ref(), &name, values)
}

#[tauri::command(rename_all = "camelCase")]
pub fn delete_game_settings_profile(
    profile_id: GameSettingsProfileId,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    state.service.delete_game_settings_profile(&profile_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn apply_game_settings(
    installation_id: InstallationId,
    expected_revision: String,
    values: GameSettingsValues,
    state: State<'_, AppState>,
) -> CommandResult<GameSettingsApplyResult> {
    state
        .service
        .apply_game_settings(&installation_id, &expected_revision, values)
}

#[tauri::command(rename_all = "camelCase")]
pub fn scan_saves(
    root_id: Option<RootId>,
    state: State<'_, AppState>,
) -> CommandResult<Vec<SaveSummary>> {
    state.service.scan_saves(root_id.as_ref())
}

#[tauri::command(rename_all = "camelCase")]
pub fn open_save(save_id: SaveId, state: State<'_, AppState>) -> CommandResult<SaveSnapshot> {
    state.service.open_save(&save_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn close_session(session_id: SessionId, state: State<'_, AppState>) -> CommandResult<()> {
    state.service.close_session(&session_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn load_portrait(
    session_id: SessionId,
    portrait_id: PortraitId,
    state: State<'_, AppState>,
) -> CommandResult<PortraitPayload> {
    state.service.load_portrait(&session_id, &portrait_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn unlock_protected_save(
    session_id: SessionId,
    acknowledgement: bool,
    state: State<'_, AppState>,
) -> CommandResult<SaveSnapshot> {
    state
        .service
        .unlock_protected_save(&session_id, acknowledgement)
}

#[tauri::command(rename_all = "camelCase")]
pub fn prepare_review(
    session_id: SessionId,
    edits: Vec<Edit>,
    state: State<'_, AppState>,
) -> CommandResult<Review> {
    state.service.prepare_review(&session_id, edits)
}

#[tauri::command(rename_all = "camelCase")]
pub fn discard_review(review_id: ReviewId, state: State<'_, AppState>) -> CommandResult<()> {
    state.service.discard_review(&review_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn apply_review(
    review_id: ReviewId,
    mode: ApplyMode,
    acknowledgement: bool,
    state: State<'_, AppState>,
) -> CommandResult<ApplyResult> {
    state
        .service
        .apply_review(&review_id, mode, acknowledgement)
}

#[tauri::command(rename_all = "camelCase")]
pub fn list_backups(
    save_id: SaveId,
    state: State<'_, AppState>,
) -> CommandResult<Vec<BackupSummary>> {
    state.service.list_backups(&save_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn prepare_restore(
    session_id: SessionId,
    backup_id: BackupId,
    state: State<'_, AppState>,
) -> CommandResult<Review> {
    state.service.prepare_restore(&session_id, &backup_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn apply_restore(
    review_id: ReviewId,
    acknowledgement: bool,
    state: State<'_, AppState>,
) -> CommandResult<ApplyResult> {
    state.service.apply_restore(&review_id, acknowledgement)
}

#[tauri::command]
pub fn startup_recovery_state(state: State<'_, AppState>) -> CommandResult<RecoveryState> {
    state.service.startup_recovery_state()
}

#[tauri::command]
pub fn export_diagnostics(state: State<'_, AppState>) -> CommandResult<Diagnostics> {
    state.service.export_diagnostics()
}
