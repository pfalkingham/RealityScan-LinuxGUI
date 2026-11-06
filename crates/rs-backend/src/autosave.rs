//! RealityScan's crash-recovery autosaves.
//!
//! When an instance doesn't exit cleanly it leaves an autosave entry in the
//! prefix registry (`HKCU\Software\EpicGames.RealityScan\RealityScan\Auto Save`)
//! and the next start asks whether to recover it. Headless, that prompt is
//! suppressed by `-silent`, but the entry should still be surfaced to the user
//! and cleared, which is what this module is for.

use std::path::{Path, PathBuf};

use crate::install::Install;

const REG_SECTION: &str = r"[Software\\EpicGames.RealityScan\\RealityScan\\Auto Save]";
const REG_KEY: &str = r"HKCU\Software\EpicGames.RealityScan\RealityScan\Auto Save";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autosave {
    /// Registry value name, e.g. `instance0`.
    pub key: String,
    /// Autosave base path as stored (Windows form), e.g. `C:\users\me\...\RSTemp\{guid}\New`.
    pub wine_path: String,
    /// The same on the Linux side, if it maps into the prefix.
    pub path: Option<PathBuf>,
}

impl Autosave {
    /// The `.rsproj.autosave` file, if it exists.
    pub fn project_file(&self) -> Option<PathBuf> {
        let p = self.path.as_ref()?.with_extension("rsproj.autosave");
        p.is_file().then_some(p)
    }
}

/// Autosave entries recorded in the prefix's `user.reg` (none when the prefix doesn't exist
/// yet, as with a CrossOver bottle before its first launch).
pub fn pending(install: &Install) -> std::io::Result<Vec<Autosave>> {
    let reg = install.prefix().join("user.reg");
    if !reg.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read(reg)?;
    Ok(parse_user_reg(&String::from_utf8_lossy(&text), &install.drive_c()))
}

fn parse_user_reg(reg: &str, drive_c: &Path) -> Vec<Autosave> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in reg.lines() {
        if line.starts_with('[') {
            in_section = line.starts_with(REG_SECTION);
            continue;
        }
        if !in_section || line.is_empty() {
            in_section &= !line.is_empty();
            continue;
        }
        // "instance0"="C:\\users\\..."
        let Some((key, value)) = line.split_once("\"=\"") else { continue };
        let key = key.trim_start_matches('"').to_owned();
        let wine_path = value.trim_end_matches('"').replace("\\\\", "\\");
        let path = rs_client::path::from_wine(&wine_path, Some(drive_c));
        out.push(Autosave { key, wine_path, path });
    }
    out
}

/// Remove autosave entries from the registry, running `reg.exe` the same way RealityScan is
/// launched (umu/Proton or CrossOver) so it talks to the prefix's wineserver whether or not an
/// instance is running. Boots the prefix when nothing runs, so this can take a few seconds.
pub async fn clear(launcher: &crate::Launcher, entries: &[Autosave]) -> std::io::Result<()> {
    for entry in entries {
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            launcher.run_in_prefix(&["reg.exe", "delete", REG_KEY, "/v", &entry.key, "/f"]),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "reg.exe did not finish"))??;
        if !status.success() {
            tracing::warn!(key = %entry.key, "reg delete failed: {status}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_autosave_entries() {
        let reg = "[Software\\\\EpicGames.RealityScan\\\\RealityScan\\\\Auto Save] 1791822587\n\
                   #time=1dd5a66e53e9916\n\
                   \"instance0\"=\"C:\\\\users\\\\steamuser\\\\AppData\\\\Local\\\\Temp\\\\RSTemp\\\\{0F22C681}\\\\New\"\n\
                   \n\
                   [Software\\\\EpicGames.RealityScan\\\\RealityScan\\\\Operation Log] 1789152900\n\
                   \"instance0\"=\"ignored\"\n";
        let entries = parse_user_reg(reg, Path::new("/pfx/drive_c"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "instance0");
        assert_eq!(entries[0].wine_path, r"C:\users\steamuser\AppData\Local\Temp\RSTemp\{0F22C681}\New");
        assert_eq!(
            entries[0].path.as_deref(),
            Some(Path::new("/pfx/drive_c/users/steamuser/AppData/Local/Temp/RSTemp/{0F22C681}/New"))
        );
    }

    #[test]
    fn no_section_means_no_entries() {
        assert!(
            parse_user_reg("[Software\\\\Wine\\\\Fonts]\n\"LogPixels\"=dword:60\n", Path::new("/x"))
                .is_empty()
        );
    }
}
