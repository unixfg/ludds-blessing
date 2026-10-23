//! Deterministic TypeScript IPC binding export target.
//!
//! Run with `cargo test -p ludds-blessing --test bindings export_ipc_bindings`.
//! The generated, checked files are written to `src-tauri/bindings/` regardless
//! of the caller's working directory.

#![allow(dead_code)]

#[path = "../src/error.rs"]
mod error;
#[path = "../src/game_settings.rs"]
mod game_settings;
#[path = "../src/models.rs"]
mod models;

use ts_rs::{Config, TS};

#[test]
fn export_ipc_bindings() {
    let config = Config::new();
    models::DiscoveryResult::export_all(&config).unwrap();
    models::SaveSnapshot::export_all(&config).unwrap();
    models::PortraitPayload::export_all(&config).unwrap();
    models::Edit::export_all(&config).unwrap();
    models::Review::export_all(&config).unwrap();
    models::ApplyMode::export_all(&config).unwrap();
    models::ApplyResult::export_all(&config).unwrap();
    models::BackupSummary::export_all(&config).unwrap();
    models::RecoveryState::export_all(&config).unwrap();
    models::Diagnostics::export_all(&config).unwrap();
    game_settings::GameSettingsValues::export_all(&config).unwrap();
    game_settings::GameSettingsSnapshot::export_all(&config).unwrap();
    game_settings::GameSettingsProfile::export_all(&config).unwrap();
    game_settings::GameSettingsApplyResult::export_all(&config).unwrap();
    error::CommandError::export_all(&config).unwrap();

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
    for required in [
        "SaveSnapshot.ts",
        "Edit.ts",
        "Review.ts",
        "ApplyMode.ts",
        "RecoveryState.ts",
        "GameSettingsValues.ts",
        "GameSettingsSnapshot.ts",
        "GameSettingsProfile.ts",
        "GameSettingsApplyResult.ts",
        "CommandError.ts",
    ] {
        assert!(directory.join(required).is_file(), "missing {required}");
    }
}
