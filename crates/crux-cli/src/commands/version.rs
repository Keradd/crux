//! `crux version` — version + build info.

use anyhow::Result;

use crate::Cli;

pub fn run(cli: &Cli) -> Result<()> {
    let v = env!("CARGO_PKG_VERSION");
    if cli.json {
        let payload = serde_json::json!({
            "version": v,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("crux {}", v);
    }
    Ok(())
}
