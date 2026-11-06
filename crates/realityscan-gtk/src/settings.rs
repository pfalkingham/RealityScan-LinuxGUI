//! Persistent settings: a flat key/value JSON file in the user's config directory, shared by
//! the alignment panel (RealityScan's `sfm*` keys) and app preferences such as the runtime.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Which RealityScan runtime to launch, in [`rs_backend::Install::from_spec`] syntax.
pub const RUNTIME_KEY: &str = "runtime";

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("realityscan-gtk/settings.json"))
}

pub fn load() -> BTreeMap<String, String> {
    let Some(path) = path() else { return BTreeMap::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Merge `pairs` into the stored settings.
pub fn save(pairs: &[(String, String)]) {
    let mut all = load();
    for (k, v) in pairs {
        all.insert(k.clone(), v.clone());
    }
    if let Some(path) = path() {
        let _ = path.parent().map(std::fs::create_dir_all);
        if let Ok(text) = serde_json::to_string_pretty(&all) {
            let _ = std::fs::write(path, text);
        }
    }
}

pub fn get(key: &str) -> Option<String> {
    load().remove(key)
}

pub fn set(key: &str, value: &str) {
    save(&[(key.to_owned(), value.to_owned())]);
}
