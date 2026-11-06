//! Where RealityScan is and how it runs.
//!
//! Two runtimes are supported. [`Runtime::Proton`] is Heroic's Epic Games Store install, run
//! through umu/Proton. [`Runtime::CrossOver`] is Epic's own Linux package (`RealityScan-2.2.deb`),
//! which is a CrossOver bottle wrapping the very same `RealityScan.exe` and Remote Command
//! plugin; it can be installed on this machine (`/opt/realityscan`, or unpacked somewhere
//! else) or inside a distrobox container, in which case it is driven through `distrobox enter`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("no RealityScan install found in Heroic's library")]
    NotInHeroic,
    #[error("Heroic has no Proton/Wine configuration for {0}")]
    NoHeroicWineConfig(String),
    #[error("{what} not found at {path}")]
    Missing { what: &'static str, path: PathBuf },
    #[error("reading {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("parsing {path}: {source}")]
    Json { path: PathBuf, source: serde_json::Error },
    #[error(
        "no RealityScan found: not in Heroic's library, no Linux package in {CROSSOVER_ROOT} or ~/.local/opt/realityscan, none in a running distrobox"
    )]
    NotFound,
    #[error("unknown runtime {0:?} (use heroic, crossover[:ROOT] or distrobox:NAME[:ROOT])")]
    BadSpec(String),
    #[error("distrobox {0:?} is not running or has no RealityScan package at {1}")]
    NotInDistrobox(String, PathBuf),
}

/// `RealityScan.exe` inside the CrossOver bottle of Epic's Linux package.
pub const CROSSOVER_EXE: &str = r"C:\Program Files\Epic Games\RealityScan\RealityScan.exe";
/// The Remote Command plugin inside that bottle.
pub const CROSSOVER_PLUGIN: &str = r"C:\Program Files\Epic Games\RealityScan\Plugins\RealityScan.RemoteCommandPlugin\RealityScan.RemoteCommandPlugin.rsplugin";
/// Where the `.deb` installs the package.
pub const CROSSOVER_ROOT: &str = "/opt/realityscan";
/// The bottle's name (`support/default` points at it).
const CROSSOVER_BOTTLE: &str = "realityscan";
/// Environment override for the runtime, in [`Install::from_spec`] syntax.
pub const RUNTIME_ENV: &str = "REALITYSCAN_GTK_RUNTIME";

/// The JPEG XL WIC decoder the Heroic prefix carries: jxl-winthumb plus the `jxl_wicfix.dll`
/// wrapper that works around Wine's stream handling (source in the prefix's `jxl_wicfix_src/`).
/// Epic's Linux package has no JPEG XL support of its own.
pub const JXL_CODEC_DLLS: [&str; 2] = ["jxl_winthumb.dll", "jxl_wicfix.dll"];
/// Registry entries that make WIC (and so RealityScan) use [`JXL_CODEC_DLLS`].
pub const JXL_CODEC_REG: &str = r#"Windows Registry Editor Version 5.00

[HKEY_LOCAL_MACHINE\Software\Classes\.jxl]
@="jxlwinthumbfile"
"Content Type"="image/jxl"
"PerceivedType"="image"

[HKEY_LOCAL_MACHINE\Software\Classes\.jxl\OpenWithProgids\jxlwinthumbfile]

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}]
@="jxl-winthumb"
"FileExtensions"=".jxl"
"FriendlyName"="jxl-winthumb WIC Decoder"
"MimeTypes"="image/jxl"
"VendorGUID"="{448D5EB7-6555-476B-A840-034CCA9AFE6E}"

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}\Formats\{F5C7AD2D-6A8D-43DD-A7A8-A29935261AE9}]

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}\InProcServer32]
@="C:\\windows\\system32\\jxl_wicfix.dll"
"ThreadingModel"="Both"

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}\Patterns\0]
"Length"=dword:00000002
"Mask"=hex:ff,ff
"Pattern"=hex:ff,0a
"Position"=dword:00000000

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}\Patterns\1]
"Length"=dword:0000000c
"Mask"=hex:ff,ff,ff,ff,ff,ff,ff,ff,ff,ff,ff,ff
"Pattern"=hex:00,00,00,0c,4a,58,4c,20,0d,0a,87,0a
"Position"=dword:00000000

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{7ED96837-96F0-4812-B211-F13C24117ED3}\Instance\{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}]
"CLSID"="{655896C6-B7D0-4D74-8AFB-A02ECE3F5E5A}"
"FriendlyName"="jxl-winthumb WIC Decoder"

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{95FFE0F8-AB15-4751-A2F3-CFAFDBF13664}]
@="jxl-winthumb"

[HKEY_LOCAL_MACHINE\Software\Classes\CLSID\{95FFE0F8-AB15-4751-A2F3-CFAFDBF13664}\InProcServer32]
@="C:\\windows\\system32\\jxl_winthumb.dll"
"ThreadingModel"="Both"

[HKEY_LOCAL_MACHINE\Software\Classes\jxlwinthumbfile]
@="JXL File"
"#;

/// How `RealityScan.exe` is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runtime {
    /// Heroic's Epic install through umu/Proton.
    Proton {
        /// `RealityScan.exe` (Linux path).
        exe: PathBuf,
        /// The Remote Command plugin's `.rsplugin` file (Linux path).
        plugin: PathBuf,
        /// Wine prefix (`WINEPREFIX`).
        prefix: PathBuf,
        /// Proton distribution directory (contains `proton` and `files/bin/wine`).
        proton_dir: PathBuf,
        /// The `umu-run` launcher.
        umu_run: PathBuf,
    },
    /// Epic's Linux package: CrossOver under `root`, running the bottle at `bottle`
    /// (CrossOver's private copy of the package's managed bottle, usually
    /// `~/.realityscan/realityscan`), optionally inside the named distrobox container.
    CrossOver { root: PathBuf, bottle: PathBuf, distrobox: Option<String> },
}

/// Where RealityScan and its runtime are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub runtime: Runtime,
}

impl Install {
    /// A Heroic/Proton install from explicit paths, checking that the main files exist.
    pub fn new(install_dir: &Path, prefix: &Path, proton_dir: &Path) -> Result<Self, InstallError> {
        let exe = install_dir.join("RealityScan.exe");
        let plugin = install_dir
            .join("Plugins")
            .join("RealityScan.RemoteCommandPlugin")
            .join("RealityScan.RemoteCommandPlugin.rsplugin");
        let umu_run = which("umu-run").unwrap_or_else(|| PathBuf::from("/usr/bin/umu-run"));
        let checks = [
            ("RealityScan.exe", &exe),
            ("Remote Command plugin", &plugin),
            ("Wine prefix", &prefix.to_owned()),
            ("Proton directory", &proton_dir.to_owned()),
            ("umu-run", &umu_run),
        ];
        for (what, path) in checks {
            if !path.exists() {
                return Err(InstallError::Missing { what, path: path.clone() });
            }
        }
        Ok(Self {
            runtime: Runtime::Proton {
                exe,
                plugin,
                prefix: prefix.to_owned(),
                proton_dir: proton_dir.to_owned(),
                umu_run,
            },
        })
    }

    /// Epic's Linux package under `root`, on this machine or inside the named distrobox.
    pub fn crossover(root: &Path, distrobox: Option<&str>) -> Result<Self, InstallError> {
        let wine = root.join("bin/wine");
        match distrobox {
            None => {
                if !wine.is_file() {
                    return Err(InstallError::Missing { what: "CrossOver's wine", path: wine });
                }
                let exe =
                    root.join("support/default/drive_c/Program Files/Epic Games/RealityScan/RealityScan.exe");
                if !exe.is_file() {
                    return Err(InstallError::Missing { what: "RealityScan.exe", path: exe });
                }
            }
            Some(name) => {
                if !distrobox_has(name, &wine) {
                    return Err(InstallError::NotInDistrobox(name.to_owned(), wine));
                }
            }
        }
        Ok(Self {
            runtime: Runtime::CrossOver {
                root: root.to_owned(),
                bottle: bottle_dir(),
                distrobox: distrobox.map(str::to_owned),
            },
        })
    }

    /// The runtime named by `REALITYSCAN_GTK_RUNTIME`, else the first one found by
    /// [`Install::detect_all`].
    pub fn detect() -> Result<Self, InstallError> {
        if let Ok(spec) = std::env::var(RUNTIME_ENV) {
            return Self::from_spec(&spec);
        }
        Self::detect_all().into_iter().next().ok_or(InstallError::NotFound)
    }

    /// Every usable runtime on this machine: Heroic's install first, then the Linux package
    /// under `/opt/realityscan` or `~/.local/opt/realityscan`, then the Linux package inside
    /// any *running* distrobox (stopped containers aren't started just to look).
    pub fn detect_all() -> Vec<Self> {
        let mut out = Vec::new();
        if let Ok(i) = Self::detect_heroic() {
            out.push(i);
        }
        for root in crossover_roots() {
            if let Ok(i) = Self::crossover(&root, None) {
                out.push(i);
            }
        }
        for name in running_distroboxes() {
            if let Ok(i) = Self::crossover(Path::new(CROSSOVER_ROOT), Some(&name)) {
                out.push(i);
            }
        }
        out
    }

    /// Parse a runtime spec: `heroic`, `crossover`, `crossover:ROOT`, `distrobox:NAME` or
    /// `distrobox:NAME:ROOT`.
    pub fn from_spec(spec: &str) -> Result<Self, InstallError> {
        let spec = spec.trim();
        match spec.split_once(':') {
            None if spec == "heroic" || spec == "proton" => Self::detect_heroic(),
            None if spec == "crossover" => crossover_roots()
                .into_iter()
                .find_map(|root| Self::crossover(&root, None).ok())
                .ok_or(InstallError::NotFound),
            Some(("crossover", root)) if !root.is_empty() => Self::crossover(Path::new(root), None),
            Some(("distrobox", rest)) if !rest.is_empty() => {
                let (name, root) = rest.split_once(':').unwrap_or((rest, CROSSOVER_ROOT));
                Self::crossover(Path::new(root), Some(name))
            }
            _ => Err(InstallError::BadSpec(spec.to_owned())),
        }
    }

    /// The spec [`Install::from_spec`] would turn back into this runtime.
    pub fn spec(&self) -> String {
        match &self.runtime {
            Runtime::Proton { .. } => "heroic".to_owned(),
            Runtime::CrossOver { root, distrobox: None, .. } => format!("crossover:{}", root.display()),
            Runtime::CrossOver { root, distrobox: Some(name), .. } => {
                format!("distrobox:{name}:{}", root.display())
            }
        }
    }

    /// A one-line description for menus.
    pub fn label(&self) -> String {
        match &self.runtime {
            Runtime::Proton { proton_dir, .. } => format!(
                "Heroic install · {}",
                proton_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Proton".into())
            ),
            Runtime::CrossOver { root, distrobox: None, .. } => {
                format!("Epic's Linux package (CrossOver) · {}", root.display())
            }
            Runtime::CrossOver { distrobox: Some(name), .. } => {
                format!("Epic's Linux package (CrossOver) in distrobox “{name}”")
            }
        }
    }

    /// "Proton" or "CrossOver", for the window subtitle.
    pub fn short_label(&self) -> &'static str {
        match self.runtime {
            Runtime::Proton { .. } => "Proton",
            Runtime::CrossOver { .. } => "CrossOver",
        }
    }

    pub fn detect_heroic() -> Result<Self, InstallError> {
        let heroic = dirs::config_dir().ok_or(InstallError::NotInHeroic)?.join("heroic");
        Self::detect_heroic_in(&heroic)
    }

    /// Find RealityScan in Heroic's Epic library and use the Proton version Heroic has
    /// configured for it.
    pub fn detect_heroic_in(heroic_config: &Path) -> Result<Self, InstallError> {
        let installed_path = heroic_config.join("legendaryConfig/legendary/installed.json");
        let installed: std::collections::BTreeMap<String, LegendaryGame> = read_json(&installed_path)?;
        let (app_id, game) = installed
            .into_iter()
            .find(|(_, g)| {
                g.title.starts_with("RealityScan") || g.executable.eq_ignore_ascii_case("RealityScan.exe")
            })
            .ok_or(InstallError::NotInHeroic)?;

        let config_path = heroic_config.join("GamesConfig").join(format!("{app_id}.json"));
        let config: serde_json::Map<String, serde_json::Value> = read_json(&config_path)?;
        let game_config: HeroicGameConfig = config
            .get(&app_id)
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .ok_or_else(|| InstallError::NoHeroicWineConfig(game.title.clone()))?;
        if game_config.wine_version.kind != "proton" {
            return Err(InstallError::NoHeroicWineConfig(game.title));
        }
        let proton_dir = game_config
            .wine_version
            .bin
            .parent()
            .ok_or_else(|| InstallError::NoHeroicWineConfig(game.title.clone()))?;
        Self::new(&game.install_path, &game_config.wine_prefix, proton_dir)
    }

    /// The Wine prefix: Proton's, or the CrossOver bottle (which may not exist before the
    /// first launch).
    pub fn prefix(&self) -> &Path {
        match &self.runtime {
            Runtime::Proton { prefix, .. } => prefix,
            Runtime::CrossOver { bottle, .. } => bottle,
        }
    }

    pub fn drive_c(&self) -> PathBuf {
        self.prefix().join("drive_c")
    }

    /// The distrobox container the runtime lives in, if any.
    pub fn distrobox(&self) -> Option<&str> {
        match &self.runtime {
            Runtime::CrossOver { distrobox, .. } => distrobox.as_deref(),
            Runtime::Proton { .. } => None,
        }
    }

    /// `RealityScan.exe` as the launcher names it: a Linux path for umu, a Windows path inside
    /// the bottle for CrossOver.
    pub fn exe_display(&self) -> String {
        match &self.runtime {
            Runtime::Proton { exe, .. } => exe.display().to_string(),
            Runtime::CrossOver { .. } => CROSSOVER_EXE.to_owned(),
        }
    }

    /// Work around a GE-Proton 11 packaging gap: with `PROTON_NVIDIA_NVCUDA=1` its script
    /// copies NVAPI from `nvidia-libs/nvapi/`, which GE doesn't ship, and dies before
    /// launching. GE does ship the same DLL layout under `nvapi/` (dxvk-nvapi), so point
    /// the missing directory at it. Returns `Ok(true)` if a link was created. CrossOver
    /// needs nothing of the kind.
    pub fn repair_nvidia_libs_nvapi(&self) -> std::io::Result<bool> {
        let Runtime::Proton { proton_dir, .. } = &self.runtime else { return Ok(false) };
        let libs = proton_dir.join("files/lib/wine/nvidia-libs");
        let wanted = libs.join("nvapi");
        let have = proton_dir.join("files/lib/wine/nvapi");
        if !libs.is_dir() || wanted.exists() || !have.is_dir() {
            return Ok(false);
        }
        std::os::unix::fs::symlink("../nvapi", &wanted)?;
        Ok(true)
    }

    /// RealityScan's own console log (`%TEMP%\RealityScan.log` of the Wine user).
    /// Shared by every instance in the prefix; useful for live "what is it doing" lines.
    pub fn realityscan_log(&self) -> Option<PathBuf> {
        let users = std::fs::read_dir(self.drive_c().join("users")).ok()?;
        users
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "Public"))
            .map(|p| p.join("AppData/Local/Temp/RealityScan.log"))
            .find(|p| p.exists())
    }
}

/// CrossOver's private copy of the package's bottle: `$CX_BOTTLE_PATH/realityscan`, by
/// default `~/.realityscan/realityscan` (the product id is the directory name).
fn bottle_dir() -> PathBuf {
    match std::env::var_os("CX_BOTTLE_PATH") {
        Some(p) => PathBuf::from(p).join(CROSSOVER_BOTTLE),
        None => dirs::home_dir().unwrap_or_default().join(".realityscan").join(CROSSOVER_BOTTLE),
    }
}

/// Where the Linux package may be on this machine: the `.deb`'s location, and an unpacked
/// copy under the user's home.
fn crossover_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(CROSSOVER_ROOT)];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/opt/realityscan"));
    }
    roots
}

/// Names of the distrobox containers that are currently running.
fn running_distroboxes() -> Vec<String> {
    let mut cmd = std::process::Command::new("distrobox");
    cmd.args(["list", "--no-color"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let Some(out) = output_with_timeout(cmd, Duration::from_secs(15)) else { return Vec::new() };
    parse_distrobox_list(&String::from_utf8_lossy(&out))
}

/// Container names with an "Up" status from `distrobox list` output.
fn parse_distrobox_list(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split('|').map(str::trim);
            let _id = cols.next()?;
            let name = cols.next()?;
            let status = cols.next()?;
            (status.starts_with("Up") && !name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

/// Does `path` exist inside the named (running) distrobox?
fn distrobox_has(name: &str, path: &Path) -> bool {
    let mut cmd = std::process::Command::new("distrobox");
    cmd.args(["enter", name, "--", "test", "-e"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    output_with_timeout(cmd, Duration::from_secs(20)).is_some()
}

/// Run `cmd`, returning its stdout if it exits successfully within `timeout`.
fn output_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[derive(Deserialize)]
struct LegendaryGame {
    title: String,
    install_path: PathBuf,
    #[serde(default)]
    executable: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeroicGameConfig {
    wine_version: HeroicWineVersion,
    wine_prefix: PathBuf,
}

#[derive(Deserialize)]
struct HeroicWineVersion {
    bin: PathBuf,
    #[serde(rename = "type")]
    kind: String,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, InstallError> {
    let text =
        std::fs::read_to_string(path).map_err(|source| InstallError::Io { path: path.to_owned(), source })?;
    serde_json::from_str(&text).map_err(|source| InstallError::Json { path: path.to_owned(), source })
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|dir| Path::new(dir).join(name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_install_is_reported() {
        let err = Install::new(
            Path::new("/nonexistent/rs"),
            Path::new("/nonexistent/pfx"),
            Path::new("/nonexistent/proton"),
        )
        .unwrap_err();
        assert!(matches!(err, InstallError::Missing { what: "RealityScan.exe", .. }), "{err}");
    }

    #[test]
    fn heroic_detection_fails_cleanly_without_heroic() {
        let err = Install::detect_heroic_in(Path::new("/nonexistent/heroic")).unwrap_err();
        assert!(matches!(err, InstallError::Io { .. }), "{err}");
    }

    #[test]
    fn missing_crossover_root_is_reported() {
        let err = Install::crossover(Path::new("/nonexistent/realityscan"), None).unwrap_err();
        assert!(matches!(err, InstallError::Missing { what: "CrossOver's wine", .. }), "{err}");
    }

    #[test]
    fn specs_round_trip() {
        let host = Install {
            runtime: Runtime::CrossOver {
                root: PathBuf::from("/opt/realityscan"),
                bottle: PathBuf::from("/home/me/.realityscan/realityscan"),
                distrobox: None,
            },
        };
        assert_eq!(host.spec(), "crossover:/opt/realityscan");
        let boxed = Install {
            runtime: Runtime::CrossOver {
                root: PathBuf::from("/opt/realityscan"),
                bottle: PathBuf::from("/home/me/.realityscan/realityscan"),
                distrobox: Some("distrodeb".into()),
            },
        };
        assert_eq!(boxed.spec(), "distrobox:distrodeb:/opt/realityscan");
        assert!(boxed.label().contains("distrodeb"));
        assert_eq!(boxed.short_label(), "CrossOver");
        assert!(matches!(Install::from_spec("nonsense").unwrap_err(), InstallError::BadSpec(_)));
        assert!(matches!(Install::from_spec("crossover:").unwrap_err(), InstallError::BadSpec(_)));
    }

    #[test]
    fn parses_running_distroboxes() {
        let text = "ID           | NAME                 | STATUS             | IMAGE\n\
                    bb7dc8edb149 | xmalabDev            | Exited (143) 11 days ago | registry.fedoraproject.org/fedora:44\n\
                    dcb240b675a9 | distrodeb            | Up 19 minutes      | docker.io/library/ubuntu:24.04\n";
        assert_eq!(parse_distrobox_list(text), vec!["distrodeb".to_owned()]);
        assert!(parse_distrobox_list("").is_empty());
    }
}
