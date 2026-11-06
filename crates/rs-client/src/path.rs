//! Translating between Linux paths and the Windows paths RealityScan sees under Wine.
//!
//! Wine maps the Linux root to `Z:\`, and `C:\` to the prefix's `drive_c`.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path {0:?} is not absolute")]
    NotAbsolute(PathBuf),
    #[error("path {0:?} is not valid UTF-8")]
    NotUtf8(PathBuf),
    #[error("path {0:?} contains a backslash, which Wine can't represent")]
    Backslash(PathBuf),
}

/// `/home/me/photos/a.jpg` → `Z:\home\me\photos\a.jpg`
pub fn to_wine(path: &Path) -> Result<String, PathError> {
    if !path.is_absolute() {
        return Err(PathError::NotAbsolute(path.to_owned()));
    }
    let mut out = String::from("Z:");
    for comp in path.components() {
        match comp {
            Component::RootDir => {}
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| PathError::NotUtf8(path.to_owned()))?;
                if part.contains('\\') {
                    return Err(PathError::Backslash(path.to_owned()));
                }
                out.push('\\');
                out.push_str(part);
            }
            // Absolute paths only contain these after normalisation by the caller;
            // keep them literal so Wine resolves them the same way Linux would.
            Component::CurDir => out.push_str("\\."),
            Component::ParentDir => out.push_str("\\.."),
            Component::Prefix(_) => unreachable!("no path prefixes on Unix"),
        }
    }
    if out == "Z:" {
        out.push('\\');
    }
    Ok(out)
}

/// `Z:\home\me\a.jpg` → `/home/me/a.jpg`; `C:\users\...` → `<drive_c>/users/...`.
/// Returns `None` for other drives, or for `C:` when `drive_c` isn't known.
pub fn from_wine(path: &str, drive_c: Option<&Path>) -> Option<PathBuf> {
    let (drive, rest) = path.split_once(':')?;
    let base = match drive {
        "Z" | "z" => PathBuf::from("/"),
        "C" | "c" => drive_c?.to_owned(),
        _ => return None,
    };
    let mut out = base;
    for part in rest.split(['\\', '/']).filter(|p| !p.is_empty()) {
        out.push(part);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_to_wine() {
        assert_eq!(
            to_wine(Path::new("/home/me/RealityScan 2.2/a.jxl")).unwrap(),
            r"Z:\home\me\RealityScan 2.2\a.jxl"
        );
        assert_eq!(to_wine(Path::new("/")).unwrap(), r"Z:\");
        assert!(matches!(to_wine(Path::new("rel/a.jpg")), Err(PathError::NotAbsolute(_))));
        assert!(matches!(to_wine(Path::new(r"/odd\name")), Err(PathError::Backslash(_))));
    }

    #[test]
    fn wine_to_linux_realityscan_image_path() {
        let p = from_wine(r"Z:\home\me\Photog\3 - 2268\2268a\image_001.jxl", None).unwrap();
        assert_eq!(p, PathBuf::from("/home/me/Photog/3 - 2268/2268a/image_001.jxl"));
    }

    #[test]
    fn wine_to_linux() {
        assert_eq!(from_wine(r"Z:\home\me\a.jpg", None), Some(PathBuf::from("/home/me/a.jpg")));
        let c = Path::new("/pfx/drive_c");
        assert_eq!(
            from_wine(r"C:\users\steamuser\x.txt", Some(c)),
            Some(PathBuf::from("/pfx/drive_c/users/steamuser/x.txt"))
        );
        assert_eq!(from_wine(r"C:\x", None), None);
        assert_eq!(from_wine(r"X:\Games\a", None), None);
        assert_eq!(from_wine("no drive", None), None);
    }
}
