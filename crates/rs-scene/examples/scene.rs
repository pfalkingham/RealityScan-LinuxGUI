//! Print the state of a running RealityScan instance.
//!
//! cargo run -p rs-scene --example scene [-- http://127.0.0.1:30010]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).unwrap_or_else(|| "http://127.0.0.1:30010".to_owned());
    let client = rs_client::Client::new(url);
    let scene = rs_scene::fetch_state(&client).await?;
    println!("{scene:#?}");
    Ok(())
}
