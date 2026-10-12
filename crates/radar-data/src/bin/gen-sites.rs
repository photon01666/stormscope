//! Regenerates the full WSR-88D site catalog as Rust source on stdout:
//!
//! ```sh
//! cargo run -p radar-data --bin gen-sites > crates/radar-core/src/sites.rs
//! ```

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let user_agent = std::env::var("STORMSCOPE_USER_AGENT")
        .unwrap_or_else(|_| radar_data::sites_gen::DEFAULT_USER_AGENT.to_string());
    let sites = radar_data::sites_gen::fetch_catalog(&user_agent)?;
    if sites.len() < 100 {
        anyhow::bail!(
            "implausibly small WSR-88D catalog ({} sites) — refusing to emit a broken sites.rs",
            sites.len()
        );
    }
    eprintln!("gen-sites: fetched {} WSR-88D sites", sites.len());
    print!("{}", radar_data::sites_gen::render_sites_rs(&sites));
    Ok(())
}
