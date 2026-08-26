use crate::error::{CoreError, ErrorCode, Result};
use crate::model::{DecimalU64, FileFingerprint};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

pub(crate) fn read_regular_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            format!("{} is not a regular, non-symlink file", path.display()),
        ));
    }
    if metadata.len() > max_bytes {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            format!("{} exceeds the configured size limit", path.display()),
        ));
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > max_bytes {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            format!("{} grew beyond the configured size limit", path.display()),
        ));
    }
    let after = fs::symlink_metadata(path)?;
    if after.file_type().is_symlink() || !after.file_type().is_file() {
        return Err(CoreError::new(
            ErrorCode::StaleSave,
            "save file type changed while reading",
        ));
    }
    Ok(bytes)
}

pub(crate) fn ensure_regular_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(CoreError::new(
            ErrorCode::InvalidPath,
            format!("{} is not a regular, non-symlink directory", path.display()),
        ));
    }
    Ok(())
}

pub(crate) fn fingerprint(bytes: &[u8]) -> FileFingerprint {
    FileFingerprint {
        sha256: hex::encode(Sha256::digest(bytes)),
        byte_len: DecimalU64::new(bytes.len() as u64),
    }
}

pub(crate) fn opaque_id(prefix: &str, bytes: &[u8]) -> String {
    let digest = hex::encode(Sha256::digest(bytes));
    format!("{prefix}-{}", &digest[..24])
}
