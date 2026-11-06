//! Manual test tool for a running RealityScan Remote Command server.
//!
//! cargo run -p rs-client --example rsctl -- status
//! cargo run -p rs-client --example rsctl -- send '-newScene -add "Z:\photos\a.jpg"'
//! cargo run -p rs-client --example rsctl -- template state.tpl
//! cargo run -p rs-client --example rsctl -- events

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use rs_client::{Client, Command, EventReceiver};

#[derive(Parser)]
struct Args {
    /// Base URL given to -RsRemoteStartREST.
    #[arg(long, default_value = "http://127.0.0.1:30010", env = "RS_URL")]
    url: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Health,
    Status,
    /// Send a raw command string, e.g. '-newScene -align'.
    Send {
        #[arg(allow_hyphen_values = true)]
        commands: String,
    },
    /// Evaluate a report template from a file ("-" for stdin).
    Template {
        file: PathBuf,
    },
    /// Enable notifications and print events until Ctrl+C.
    /// RealityScan keeps one notification target, so this takes over from any other listener.
    Events,
    Abort,
    Pause,
    Resume,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let client = Client::new(&args.url);
    match args.cmd {
        Cmd::Health => println!("{}", if client.health().await { "up" } else { "down" }),
        Cmd::Status => println!("{:#?}", client.status().await?),
        Cmd::Send { commands } => println!("sequence {}", client.send_raw(&commands).await?),
        Cmd::Template { file } => {
            let mut text = String::new();
            if file.as_os_str() == "-" {
                std::io::stdin().read_to_string(&mut text)?;
            } else {
                text =
                    std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            }
            print!("{}", client.template(&text).await?);
        }
        Cmd::Events => {
            let mut rx = EventReceiver::bind().await?;
            let seq = Command::new("RsRemoteStartNotifyREST").arg(rx.url()).into();
            client.send(&seq).await?;
            eprintln!("listening on {} (Ctrl+C to stop)", rx.url());
            loop {
                tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => {
                            // Flush per event so output survives being killed when piped.
                            let mut out = std::io::stdout().lock();
                            writeln!(out, "{event:?}")?;
                            out.flush()?;
                        }
                        None => break,
                    },
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
        }
        Cmd::Abort => client.abort().await?,
        Cmd::Pause => client.pause().await?,
        Cmd::Resume => client.resume().await?,
    }
    Ok(())
}
