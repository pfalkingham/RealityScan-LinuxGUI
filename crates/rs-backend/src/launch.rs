use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rs_client::{Client, Command, CommandSeq, EventReceiver, path::to_wine};
use tokio::process::Child;

use crate::install::{CROSSOVER_PLUGIN, Install, JXL_CODEC_DLLS, JXL_CODEC_REG, Runtime};
use crate::proc_tree::{kill_tree, kill_with_args};

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("preparing instance files in {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("starting {program}: {source}")]
    Spawn { program: PathBuf, source: std::io::Error },
    #[error("RealityScan exited before its REST server came up (status {status}); see {log}")]
    ExitedEarly { status: String, log: PathBuf },
    #[error("RealityScan's REST server did not answer within {0:?}; see {1}")]
    Timeout(Duration, PathBuf),
    #[error("binding the event receiver: {0}")]
    Events(std::io::Error),
    #[error(transparent)]
    Path(#[from] rs_client::path::PathError),
    #[error(transparent)]
    Command(#[from] rs_client::CommandError),
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// RealityScan instance name (`-setInstanceName`); also names the work directory.
    pub instance_name: String,
    /// REST port on 127.0.0.1; `None` picks a free one.
    pub port: Option<u16>,
    /// Also start push notifications into an [`EventReceiver`].
    pub events: bool,
    /// Extra CLI arguments appended after the standard ones (e.g. `-load <project>`).
    pub extra_args: Vec<String>,
    /// How long to wait for the REST server.
    pub startup_timeout: Duration,
    /// Where to keep per-instance files (rscmd, crash dumps, logs).
    /// Default: `$XDG_CACHE_HOME/realityscan-gtk/instances`.
    pub work_root: Option<PathBuf>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            instance_name: "realityscan-gtk".to_owned(),
            port: None,
            events: true,
            extra_args: Vec::new(),
            startup_timeout: Duration::from_secs(180),
            work_root: None,
        }
    }
}

/// Starts RealityScan instances for an [`Install`].
#[derive(Debug, Clone)]
pub struct Launcher {
    install: Install,
}

impl Launcher {
    pub fn new(install: Install) -> Self {
        Self { install }
    }

    pub fn install(&self) -> &Install {
        &self.install
    }

    /// Start a headless instance and wait until its REST server answers.
    pub async fn launch(&self, opts: LaunchOptions) -> Result<Instance, LaunchError> {
        let work_dir = work_root(opts.work_root.clone()).join(&opts.instance_name);
        let crash_dir = work_dir.join("crash");
        std::fs::create_dir_all(&crash_dir)
            .map_err(|source| LaunchError::Io { path: crash_dir.clone(), source })?;

        let port = match opts.port {
            Some(p) => p,
            None => free_port()
                .map_err(|source| LaunchError::Io { path: PathBuf::from("127.0.0.1:0"), source })?,
        };
        let base_url = format!("http://127.0.0.1:{port}");

        let events =
            if opts.events { Some(EventReceiver::bind().await.map_err(LaunchError::Events)?) } else { None };

        // The plugin's commands only exist once it is registered, which happens after the
        // command line is validated, so they have to come from an .rscmd file.
        let mut startup = CommandSeq::from(Command::new("RsRemoteStartREST").arg(&base_url));
        if let Some(rx) = &events {
            startup.push(Command::new("RsRemoteStartNotifyREST").arg(rx.url()));
        }
        let rscmd = work_dir.join("start.rscmd");
        std::fs::write(&rscmd, format!("{}\n", startup.render()?))
            .map_err(|source| LaunchError::Io { path: rscmd.clone(), source })?;

        let log_path = work_dir.join("realityscan.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|source| LaunchError::Io { path: log_path.clone(), source })?;
        let log_err = log.try_clone().map_err(|source| LaunchError::Io { path: log_path.clone(), source })?;

        let mut args = vec![
            "-silent".to_owned(),
            to_wine(&crash_dir)?,
            "-headless".to_owned(),
            "-setInstanceName".to_owned(),
            opts.instance_name.clone(),
            "-registerPlugin".to_owned(),
            self.plugin_arg()?,
            "-execRSCMD".to_owned(),
            to_wine(&rscmd)?,
        ];
        args.extend(opts.extra_args.iter().cloned());
        let mut cmd = self
            .command(&work_dir, "run.sh", &self.install.exe_display(), &args)
            .map_err(|source| LaunchError::Io { path: work_dir.clone(), source })?;
        cmd.stdin(Stdio::null()).stdout(Stdio::from(log)).stderr(Stdio::from(log_err)).kill_on_drop(true);
        match self.install.repair_nvidia_libs_nvapi() {
            Ok(true) => tracing::info!("linked nvidia-libs/nvapi in Proton so nvcuda can be enabled"),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("couldn't repair nvidia-libs/nvapi in Proton (CUDA may be unavailable): {e}")
            }
        }
        tracing::info!(instance = %opts.instance_name, %base_url, runtime = self.install.short_label(), "launching RealityScan");
        let mut child =
            cmd.spawn().map_err(|source| LaunchError::Spawn { program: self.program_path(), source })?;

        let client = Client::new(&base_url);
        let deadline = tokio::time::Instant::now() + opts.startup_timeout;
        loop {
            if client.health().await {
                break;
            }
            if let Some(status) = child.try_wait().ok().flatten() {
                return Err(LaunchError::ExitedEarly { status: status.to_string(), log: log_path });
            }
            if tokio::time::Instant::now() >= deadline {
                kill_child_tree(&mut child).await;
                kill_with_args(&["-setInstanceName", &opts.instance_name]);
                return Err(LaunchError::Timeout(opts.startup_timeout, log_path));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        tracing::info!(instance = %opts.instance_name, "RealityScan REST server is up");

        Ok(Instance { name: opts.instance_name, client, events, child, log_path })
    }

    /// Run a Windows program inside the prefix (e.g. `reg.exe`) the same way RealityScan is
    /// launched, with the same environment, and wait for it.
    pub async fn run_in_prefix(&self, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
        let (program, rest) = args
            .split_first()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no program given"))?;
        let work_dir = work_root(None);
        std::fs::create_dir_all(&work_dir)?;
        let rest: Vec<String> = rest.iter().map(|s| (*s).to_owned()).collect();
        self.command(&work_dir, "cmd.sh", program, &rest)?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
    }

    /// Give a CrossOver bottle the JPEG XL decoder another prefix carries (see
    /// [`JXL_CODEC_DLLS`]): copy the DLLs from `source_system32` into the bottle's `system32`
    /// and register the decoder with `regedit`. Call it after a launch, when the bottle exists
    /// and its wineserver is up. Returns `Ok(true)` when the codec was installed, `Ok(false)`
    /// when there is nothing to do (not CrossOver, no bottle yet, codec already present, or
    /// the source lacks the DLLs).
    pub async fn install_jxl_codec(&self, source_system32: &Path) -> std::io::Result<bool> {
        let Runtime::CrossOver { bottle, .. } = &self.install.runtime else { return Ok(false) };
        let system32 = bottle.join("drive_c/windows/system32");
        if !system32.is_dir() || system32.join(JXL_CODEC_DLLS[1]).exists() {
            return Ok(false);
        }
        let sources: Vec<PathBuf> = JXL_CODEC_DLLS.iter().map(|d| source_system32.join(d)).collect();
        if !sources.iter().all(|s| s.is_file()) {
            return Ok(false);
        }
        for (src, name) in sources.iter().zip(JXL_CODEC_DLLS) {
            std::fs::copy(src, system32.join(name))?;
        }
        let reg = work_root(None).join("jxl-wic.reg");
        std::fs::write(&reg, JXL_CODEC_REG)?;
        let reg_arg = to_wine(&reg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
        let status = self.run_in_prefix(&["regedit", "/S", &reg_arg]).await?;
        if !status.success() {
            return Err(std::io::Error::other(format!("regedit failed: {status}")));
        }
        Ok(true)
    }

    /// Put the project's `dxcore.dll` (see `tools/dxcore/`) into the prefix: Wine's own one
    /// makes ONNX Runtime reject every GPU, so RealityScan's AI masking runs on the CPU.
    /// Replaces `system32/dxcore.dll` (a symlink to the builtin in Proton prefixes; the
    /// original's contents are kept as `dxcore.dll.wine` the first time) and sets the
    /// `dxcore=native` override through `reg.exe`. Does nothing when the prefix doesn't exist
    /// yet or already has this exact file. Returns `Ok(true)` when it changed something.
    pub async fn install_dxcore(&self, dll: &Path) -> std::io::Result<bool> {
        let system32 = self.install.drive_c().join("windows/system32");
        if !system32.is_dir() {
            return Ok(false);
        }
        let target = system32.join("dxcore.dll");
        let wanted = std::fs::read(dll)?;
        if std::fs::read(&target).ok().as_deref() == Some(wanted.as_slice()) {
            return Ok(false);
        }
        let backup = system32.join("dxcore.dll.wine");
        if !backup.exists()
            && let Ok(original) = std::fs::read(&target)
        {
            std::fs::write(&backup, original)?;
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            std::fs::remove_file(&target)?;
        }
        std::fs::write(&target, &wanted)?;
        let status = self
            .run_in_prefix(&[
                "reg.exe",
                "add",
                r"HKCU\Software\Wine\DllOverrides",
                "/v",
                "dxcore",
                "/t",
                "REG_SZ",
                "/d",
                "native",
                "/f",
            ])
            .await?;
        if !status.success() {
            return Err(std::io::Error::other(format!("reg.exe failed: {status}")));
        }
        Ok(true)
    }

    /// `-registerPlugin`'s argument: the plugin as Wine sees it.
    fn plugin_arg(&self) -> Result<String, rs_client::path::PathError> {
        match &self.install.runtime {
            Runtime::Proton { plugin, .. } => to_wine(plugin),
            Runtime::CrossOver { .. } => Ok(CROSSOVER_PLUGIN.to_owned()),
        }
    }

    /// The executable this runtime starts (for error messages).
    fn program_path(&self) -> PathBuf {
        match &self.install.runtime {
            Runtime::Proton { umu_run, .. } => umu_run.clone(),
            Runtime::CrossOver { root, distrobox: None, .. } => root.join("bin/wine"),
            Runtime::CrossOver { distrobox: Some(_), .. } => PathBuf::from("distrobox"),
        }
    }

    /// A process that runs `program args` inside this runtime's prefix. A distrobox launch
    /// goes through a small wrapper script named `script_name` in `work_dir`, because
    /// `distrobox enter` joins its arguments back into one command line and loses quoting.
    fn command(
        &self,
        work_dir: &Path,
        script_name: &str,
        program: &str,
        args: &[String],
    ) -> std::io::Result<tokio::process::Command> {
        match &self.install.runtime {
            Runtime::Proton { umu_run, .. } => {
                let mut cmd = tokio::process::Command::new(umu_run);
                cmd.arg(program).args(args).envs(self.proton_env());
                Ok(cmd)
            }
            Runtime::CrossOver { root, distrobox: None, .. } => {
                let mut cmd = tokio::process::Command::new(root.join("bin/wine"));
                cmd.args(crossover_args(program, args));
                Ok(cmd)
            }
            Runtime::CrossOver { root, distrobox: Some(name), .. } => {
                let script = work_dir.join(script_name);
                let line = std::iter::once(root.join("bin/wine").to_string_lossy().into_owned())
                    .chain(crossover_args(program, args))
                    .map(|a| sh_quote(&a))
                    .collect::<Vec<_>>()
                    .join(" ");
                std::fs::write(&script, format!("#!/bin/sh\nexec {line}\n"))?;
                let mut cmd = tokio::process::Command::new("distrobox");
                cmd.args(["enter", name, "--", "sh"]).arg(&script);
                Ok(cmd)
            }
        }
    }

    /// Environment umu/Proton need, mirroring what Heroic sets. Empty for CrossOver, whose
    /// `wine` script sets up its own.
    fn proton_env(&self) -> Vec<(&'static str, String)> {
        let Runtime::Proton { exe, prefix, proton_dir, .. } = &self.install.runtime else {
            return Vec::new();
        };
        let prefix = prefix.to_string_lossy().into_owned();
        let steam = dirs::home_dir().unwrap_or_default().join(".steam/steam").to_string_lossy().into_owned();
        let install_dir = exe.parent().unwrap_or(Path::new("/")).to_string_lossy().into_owned();
        vec![
            ("WINEPREFIX", prefix.clone()),
            ("STEAM_COMPAT_DATA_PATH", prefix),
            ("STEAM_COMPAT_INSTALL_PATH", install_dir),
            ("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam),
            ("PROTONPATH", proton_dir.to_string_lossy().into_owned()),
            ("GAMEID", "umu-0".to_owned()),
            ("STEAM_COMPAT_APP_ID", "0".to_owned()),
            ("SteamAppId", "0".to_owned()),
            // Proton's default verb (waitforexitandrun) blocks while anything else runs in
            // the prefix, which would serialise our instances; `run` starts straight away.
            ("PROTON_VERB", "run".to_owned()),
            // Wine-Wayland can't show windows at non-96 DPI and we don't need any; use X11.
            ("PROTON_ENABLE_WAYLAND", "0".to_owned()),
            ("PROTON_ENABLE_NVAPI", "1".to_owned()),
            ("DXVK_NVAPI_ALLOW_OTHER_DRIVERS", "1".to_owned()),
            // GE-Proton ships a real nvcuda (wine-nvcuda) but installs a disabled stub
            // unless asked; RealityScan needs CUDA for depth maps, meshing and texturing.
            // (Not PROTON_NVIDIA_LIBS: GE-Proton 11 crashes looking for nvidia-libs/nvapi.)
            ("PROTON_NVIDIA_NVCUDA", "1".to_owned()),
            // Quiet by default; REALITYSCAN_GTK_WINEDEBUG=+dxcore,… turns Wine channels on
            // for diagnosis (output lands in the instance's realityscan.log).
            ("WINEDEBUG", std::env::var("REALITYSCAN_GTK_WINEDEBUG").unwrap_or_else(|_| "-all".to_owned())),
        ]
    }
}

/// Where per-instance files go.
fn work_root(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| dirs::cache_dir().map(|c| c.join("realityscan-gtk/instances")))
        .unwrap_or_else(std::env::temp_dir)
}

/// CrossOver's `wine` script arguments for running `program args` in the package's bottle.
/// `RealityScan.exe` must be named by its full Windows path: the `--workdir` + bare name form
/// the package's own `realityscan-cli` uses fails with "cannot execute". `--wait-children`
/// keeps the script alive until RealityScan exits, so the child's status tracks the instance.
fn crossover_args(program: &str, args: &[String]) -> Vec<String> {
    let mut out = vec!["--bottle=default".to_owned(), "--wait-children".to_owned(), program.to_owned()];
    out.extend(args.iter().cloned());
    out
}

/// Single-quote `s` for `sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A running RealityScan instance.
pub struct Instance {
    name: String,
    client: Client,
    events: Option<EventReceiver>,
    child: Child,
    log_path: PathBuf,
}

impl Instance {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Push notifications, if the instance was launched with `events: true`.
    pub fn events(&mut self) -> Option<&mut EventReceiver> {
        self.events.as_mut()
    }

    /// Take ownership of the notification receiver (it keeps working after this).
    pub fn take_events(&mut self) -> Option<EventReceiver> {
        self.events.take()
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// True while the launcher process is alive. RealityScan runs inside umu's sandbox (or
    /// under CrossOver's `wine --wait-children` script): if it crashes, the launcher exits
    /// too. The reverse doesn't hold — after a clean `-quit`, umu can linger while other
    /// instances keep the prefix's wineserver busy — so also check [`Client::health`].
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask RealityScan to quit and wait until its REST server has gone away
    /// (connection refused, not merely a failed request). Falls back to killing
    /// the whole process tree if that doesn't happen within `grace`.
    ///
    /// A killed instance leaves an autosave behind; see [`crate::autosave`].
    pub async fn shutdown(mut self, grace: Duration) -> Result<(), LaunchError> {
        if self.client.send(&Command::quit().into()).await.is_err() {
            tracing::warn!(instance = %self.name, "-quit not accepted; killing");
            return self.kill().await;
        }
        let deadline = tokio::time::Instant::now() + grace;
        while !self.client.is_gone().await {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(instance = %self.name, "still running after -quit; killing");
                return self.kill().await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // RealityScan is gone; give the launcher a moment to notice, then stop waiting for it.
        if tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await.is_err() {
            tracing::debug!(instance = %self.name, "launcher lingering after quit; killing its tree");
            kill_child_tree(&mut self.child).await;
        }
        Ok(())
    }

    /// Kill this instance's processes only. `wineserver -k` would also take down every other
    /// instance sharing the prefix, so it's deliberately not used here. Inside a distrobox the
    /// Wine processes aren't descendants of our `distrobox enter` client, so they are also
    /// found by the instance name on their command line.
    async fn kill(&mut self) -> Result<(), LaunchError> {
        kill_child_tree(&mut self.child).await;
        kill_with_args(&["-setInstanceName", &self.name]);
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Dropped without `shutdown` (e.g. the app panicked): don't leave a sandbox behind.
        if let Some(pid) = self.child.id() {
            tracing::warn!(instance = %self.name, "instance dropped while running; killing its tree");
            kill_tree(pid);
            kill_with_args(&["-setInstanceName", &self.name]);
        }
    }
}

/// Kill the launcher and its whole sandbox tree, then reap the launcher.
async fn kill_child_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        kill_tree(pid);
    }
    let _ = child.kill().await;
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance").field("name", &self.name).field("url", &self.client.base_url()).finish()
    }
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_survives_apostrophes_and_backslashes() {
        assert_eq!(sh_quote(r"C:\Program Files\it's"), r"'C:\Program Files\it'\''s'");
        let args = crossover_args(r"C:\x\RealityScan.exe", &["-headless".to_owned()]);
        assert_eq!(args, ["--bottle=default", "--wait-children", r"C:\x\RealityScan.exe", "-headless"]);
    }
}
