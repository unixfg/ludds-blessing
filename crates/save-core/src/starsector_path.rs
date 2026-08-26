use std::fs;
#[cfg(windows)]
use std::fs::File;
use std::io;
#[cfg(windows)]
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(windows)]
const MAX_LAUNCH_CONFIG_BYTES: u64 = 128 * 1024;
#[cfg(windows)]
const MAX_LAUNCH_ARGUMENTS: usize = 2_048;
#[cfg(windows)]
const MAX_LAUNCH_ARGUMENT_BYTES: usize = 8 * 1024;
#[cfg(windows)]
const SAVE_PATH_PROPERTY: &str = "-Dcom.fs.starfarer.settings.paths.saves=";

/// Resolves the save root selected by a verified Starsector installation.
///
/// Windows' native launcher reads `vmparams` and starts Java with
/// `starsector-core` as its working directory. Relative save paths therefore
/// resolve from that directory, not from the user's profile or the editor.
/// When an older installation has no `vmparams`, the historical sibling
/// `saves` directory remains the bounded fallback. The native Linux archive
/// root and macOS app's `Contents/Resources/Java` directory are already the
/// Java working directory, so their bounded native save root is the direct
/// `saves` child.
///
/// # Errors
///
/// Returns an I/O or invalid-data error when the installation, launch
/// configuration, or configured save root cannot be verified safely.
pub fn resolve_starsector_save_root(installation_root: &Path) -> io::Result<PathBuf> {
    reject_symlink(installation_root, "Starsector installation")?;
    let installation_root = fs::canonicalize(installation_root)?;
    if !installation_root.is_dir() {
        return Err(invalid_data("Starsector installation is not a directory"));
    }

    #[cfg(windows)]
    let candidate = match configured_windows_save_path(&installation_root)? {
        Some(configured) => {
            let configured =
                PathBuf::from(configured.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR));
            if configured.is_absolute() {
                configured
            } else {
                installation_root.join("starsector-core").join(configured)
            }
        }
        None => installation_root.join("saves"),
    };

    #[cfg(not(windows))]
    let candidate = installation_root.join("saves");

    reject_symlink(&candidate, "configured Starsector save root")?;
    let candidate = fs::canonicalize(candidate)?;
    if !candidate.is_dir() {
        return Err(invalid_data(
            "configured Starsector save root is not a directory",
        ));
    }
    Ok(candidate)
}

#[cfg(windows)]
fn configured_windows_save_path(installation_root: &Path) -> io::Result<Option<String>> {
    let path = installation_root.join("vmparams");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_data(
            "Starsector vmparams must be a regular non-symbolic-link file",
        ));
    }
    if metadata.len() > MAX_LAUNCH_CONFIG_BYTES {
        return Err(invalid_data("Starsector vmparams exceeds the safety limit"));
    }

    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_LAUNCH_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LAUNCH_CONFIG_BYTES {
        return Err(invalid_data("Starsector vmparams exceeds the safety limit"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| invalid_data("Starsector vmparams contains malformed UTF-8"))?;
    parse_save_path_property(text).map(Some)
}

#[cfg(windows)]
fn parse_save_path_property(text: &str) -> io::Result<String> {
    let arguments = tokenize_launch_arguments(text)?;
    let mut configured = None;
    for argument in arguments {
        if let Some(value) = argument.strip_prefix(SAVE_PATH_PROPERTY) {
            if value.is_empty() {
                return Err(invalid_data("Starsector save path is empty"));
            }
            if configured.replace(value.to_owned()).is_some() {
                return Err(invalid_data(
                    "Starsector vmparams defines more than one save path",
                ));
            }
        } else if argument.starts_with("-Dcom.fs.starfarer.settings.paths.saves") {
            return Err(invalid_data(
                "Starsector vmparams contains a malformed save-path setting",
            ));
        }
    }
    configured.ok_or_else(|| invalid_data("Starsector vmparams has no save-path setting"))
}

#[cfg(windows)]
fn tokenize_launch_arguments(text: &str) -> io::Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for character in text.chars() {
        if character == '\0' || (character.is_control() && !character.is_whitespace()) {
            return Err(invalid_data(
                "Starsector vmparams contains unsupported control characters",
            ));
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            push_argument(&mut arguments, &mut current)?;
        } else {
            current.push(character);
        }
        if current.len() > MAX_LAUNCH_ARGUMENT_BYTES {
            return Err(invalid_data(
                "A Starsector vmparams argument exceeds the safety limit",
            ));
        }
    }
    if quote.is_some() {
        return Err(invalid_data("Starsector vmparams contains an open quote"));
    }
    push_argument(&mut arguments, &mut current)?;
    Ok(arguments)
}

#[cfg(windows)]
fn push_argument(arguments: &mut Vec<String>, current: &mut String) -> io::Result<()> {
    if current.is_empty() {
        return Ok(());
    }
    if arguments.len() >= MAX_LAUNCH_ARGUMENTS {
        return Err(invalid_data(
            "Starsector vmparams exceeds the argument-count limit",
        ));
    }
    arguments.push(std::mem::take(current));
    Ok(())
}

fn reject_symlink(path: &Path, label: &str) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid_data(format!("{label} may not be a symbolic link")));
    }
    Ok(())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn create_install(root: &Path, save_root: &Path, vmparams: Option<&str>) {
        fs::create_dir_all(root.join("starsector-core")).unwrap();
        fs::create_dir_all(save_root).unwrap();
        if let Some(vmparams) = vmparams {
            fs::write(root.join("vmparams"), vmparams).unwrap();
        }
    }

    #[test]
    fn resolves_relative_and_quoted_absolute_vmparams_paths() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Starsector");
        let sibling = install.join("saves");
        create_install(
            &install,
            &sibling,
            Some("java -Dcom.fs.starfarer.settings.paths.saves=..\\saves Game"),
        );
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap(),
            sibling.canonicalize().unwrap()
        );

        let external = temp.path().join("My Saved Campaigns");
        fs::create_dir_all(&external).unwrap();
        fs::write(
            install.join("vmparams"),
            format!(
                "java \"-Dcom.fs.starfarer.settings.paths.saves={}\" Game",
                external.display()
            ),
        )
        .unwrap();
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap(),
            external.canonicalize().unwrap()
        );
    }

    #[test]
    fn missing_vmparams_uses_the_historical_install_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Starsector");
        let saves = install.join("saves");
        create_install(&install, &saves, None);
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap(),
            saves.canonicalize().unwrap()
        );
    }

    #[test]
    fn malformed_duplicate_and_oversized_vmparams_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("Starsector");
        let saves = install.join("saves");
        create_install(&install, &saves, Some("java -Xmx2g Game"));
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        fs::write(
            install.join("vmparams"),
            "java -Dcom.fs.starfarer.settings.paths.saves=..\\saves -Dcom.fs.starfarer.settings.paths.saves=..\\other Game",
        )
        .unwrap();
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let oversized = File::create(install.join("vmparams")).unwrap();
        oversized.set_len(MAX_LAUNCH_CONFIG_BYTES + 1).unwrap();
        assert_eq!(
            resolve_starsector_save_root(&install).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
