use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static EXPLICIT: OnceLock<bool> = OnceLock::new();
pub fn init(explicit: Option<PathBuf>) -> Result<()> {
    let explicit =
        explicit.or_else(|| std::env::var_os("FRC_CLIPPER_CONFIG_DIR").map(PathBuf::from));
    let is_explicit = explicit.is_some();
    let dir = match explicit {
        Some(p) => p,
        None => default_directory()?,
    };
    let _ = EXPLICIT.set(is_explicit);
    let _ = DIRECTORY.set(dir.clone());
    let _ = dotenvy::from_path(dir.join(".env"));
    let _ = dotenvy::dotenv();
    Ok(())
}
fn default_directory() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(root).join("frc-match-clipper"));
    }
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("Set --config-dir or FRC_CLIPPER_CONFIG_DIR")?,
    )
    .join(".config/frc-match-clipper"))
}
pub fn directory() -> &'static Path {
    DIRECTORY
        .get()
        .expect("Configuration initialized")
        .as_path()
}
pub fn credential_directory() -> PathBuf {
    select_credential_directory(
        directory(),
        *EXPLICIT.get().unwrap_or(&false),
        Path::new("."),
    )
}
fn select_credential_directory(dir: &Path, explicit: bool, cwd: &Path) -> PathBuf {
    if explicit || dir.join("client_secret.json").exists() {
        return dir.into();
    }
    // Keep client/token pairs together rather than selecting a token from another account.
    if cwd.join("client_secret.json").exists() {
        return cwd.into();
    }
    dir.into()
}
pub fn credential_file(name: &str) -> PathBuf {
    credential_directory().join(name)
}
pub fn ensure_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_keep_client_and_token_together() {
        let root = std::env::temp_dir().join(format!(
            "clipper-config-{}-{}",
            std::process::id(),
            crate::random_string(8)
        ));
        let global = root.join("global");
        let legacy = root.join("legacy");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(select_credential_directory(&global, false, &legacy), global);
        std::fs::write(legacy.join("client_secret.json"), b"{}").unwrap();
        assert_eq!(select_credential_directory(&global, false, &legacy), legacy);
        assert_eq!(select_credential_directory(&global, true, &legacy), global);
        std::fs::write(global.join("client_secret.json"), b"{}").unwrap();
        assert_eq!(select_credential_directory(&global, false, &legacy), global);
        std::fs::remove_dir_all(root).unwrap();
    }
}
