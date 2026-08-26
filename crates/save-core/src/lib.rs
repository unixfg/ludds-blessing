//! Lossless, fail-closed primitives for reading and editing Starsector saves.
//!
//! This crate deliberately treats a campaign as an identity-bearing XML graph.
//! It never invokes game code, loads mod jars, or serializes a campaign back to
//! XML. Mutations are represented as checked byte-span patches.

mod descriptor;
mod discovery;
mod error;
mod file_util;
mod model;
mod patch;
mod progression;
mod review;
mod semantic;
mod skill_json;
mod starsector_path;
mod transaction;
mod xml;

pub use descriptor::{parse_descriptor, DescriptorDocument};
pub use discovery::{inspect_save_dir, scan_save_root, ScanOptions};
pub use error::{CoreError, ErrorCode, Result};
pub use model::*;
pub use patch::{apply_patches, SpanPatch};
pub use progression::{
    grant_officer_xp, grant_player_xp, officer_xp_for_level, player_source_xp_to_reach,
    player_xp_for_level, raise_officer_to_level, raise_player_to_level, OfficerProgress,
    PlayerProgress, Rc8Progression,
};
pub use review::PreparedReview;
pub use semantic::{OpenOptions, OpenedSave};
pub use starsector_path::resolve_starsector_save_root;
pub use transaction::{
    ensure_private_directory, ensure_starsector_closed, harden_private_file,
    harden_private_storage_tree, replace_file_atomically, ApplyOutcome, BackupStore, BackupSummary,
    RecoveryRecord,
};
pub use xml::{ElementId, XmlDocument, XmlLimits};

/// The only game/save-format pair for which mutation is enabled in v1.
pub const SUPPORTED_GAME_VERSION: &str = "0.98a-RC8";
pub const SUPPORTED_SAVE_FORMAT: &str = "0.6";
