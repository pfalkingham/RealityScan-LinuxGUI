//! Launch a headless RealityScan (runtime per REALITYSCAN_GTK_RUNTIME or auto-detected),
//! print its state, quit.
//!
//! cargo run -p rs-backend --example launch [-- <extra RealityScan args...>]

use std::time::Duration;

use rs_backend::{Install, LaunchOptions, Launcher, autosave};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let install = Install::detect()?;
    println!("runtime: {} ({})", install.label(), install.spec());
    println!("install: {install:#?}");
    for a in autosave::pending(&install)? {
        println!("pending autosave: {} -> {:?}", a.key, a.project_file());
    }

    let launcher = Launcher::new(install);
    // RS_STARTUP_TIMEOUT_SECS=5 exercises the timeout/kill path.
    let startup_timeout = std::env::var("RS_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(LaunchOptions::default().startup_timeout, Duration::from_secs);
    let opts = LaunchOptions {
        instance_name: std::env::var("RS_INSTANCE_NAME").unwrap_or_else(|_| "rsgtk-example".to_owned()),
        extra_args: std::env::args().skip(1).collect(),
        startup_timeout,
        ..LaunchOptions::default()
    };
    let started = std::time::Instant::now();
    let mut instance = launcher.launch(opts).await?;
    println!("up in {:.1?}: {instance:?}", started.elapsed());
    println!("status: {:#?}", instance.client().status().await?);
    println!("scene: {:#?}", rs_scene::fetch_state(instance.client()).await?);
    if let Some(rx) = instance.events() {
        while let Some(e) = rx.try_recv() {
            println!("event: {e:?}");
        }
    }
    // RS_HOLD=1 keeps the instance up for manual poking (rsctl) until Ctrl+C.
    if std::env::var_os("RS_HOLD").is_some() {
        println!("holding; RS_URL={} (Ctrl+C to quit)", instance.client().base_url());
        tokio::signal::ctrl_c().await?;
    }
    instance.shutdown(Duration::from_secs(60)).await?;
    println!("shut down cleanly");
    Ok(())
}
