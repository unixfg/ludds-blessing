use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "reviewed-warnings-v1.json";
const MAX_BYTES: u64 = 1024 * 1024;

/// App-owned preferences, scoped to a save and the exact warning conditions.
#[derive(Default, Deserialize, Serialize)]
pub(crate) struct WarningAcknowledgements {
    saves: BTreeMap<String, BTreeSet<String>>,
}

impl WarningAcknowledgements {
    pub fn load(app_data_dir: &Path) -> io::Result<Self> {
        let path = app_data_dir.join(FILE_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(io::Error::other("Invalid warning acknowledgement file"));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other(
                "Warning acknowledgements exceed the size limit",
            ));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn contains(&self, save_id: &str, fingerprint: &str) -> bool {
        self.saves
            .get(save_id)
            .is_some_and(|warnings| warnings.contains(fingerprint))
    }

    pub fn remember(&mut self, save_id: &str, fingerprints: &[String]) {
        if !fingerprints.is_empty() {
            self.saves
                .entry(save_id.to_owned())
                .or_default()
                .extend(fingerprints.iter().cloned());
        }
    }

    pub fn persist(&self, app_data_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::other(
                "Warning acknowledgements exceed the size limit",
            ));
        }
        crate::service::atomic_write_private_file(&app_data_dir.join(FILE_NAME), &bytes)
    }
}
