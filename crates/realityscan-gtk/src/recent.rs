//! Recently opened projects, kept in `$XDG_CONFIG_HOME/realityscan-gtk/recent.json`.

use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;

fn file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("realityscan-gtk/recent.json"))
}

/// Most recent first; entries whose file no longer exists are dropped.
pub fn load() -> Vec<PathBuf> {
    let Some(path) = file() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let list: Vec<PathBuf> = serde_json::from_str(&text).unwrap_or_default();
    list.into_iter().filter(|p| p.is_file()).collect()
}

/// Move `project` to the front of the list and save it.
pub fn push(project: &Path) -> Vec<PathBuf> {
    let mut list = load();
    list.retain(|p| p != project);
    list.insert(0, project.to_path_buf());
    list.truncate(MAX_RECENT);
    if let Some(path) = file() {
        let _ = path.parent().map(std::fs::create_dir_all);
        if let Ok(text) = serde_json::to_string_pretty(&list)
            && let Err(e) = std::fs::write(&path, text)
        {
            tracing::warn!("couldn't save recent projects: {e}");
        }
    }
    list
}
