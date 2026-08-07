//! Ephervex command-line interface.
//!
//! The CLI reuses the exact same core services as the GUI. Commands are filled
//! in as their phases land; the surface here is the skeleton established in
//! the repository foundation.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ephervex",
    version,
    about = "Disposable Minecraft instances for mod development and bug reproduction"
)]
struct Cli {
    /// Emit machine-readable JSON where the command supports it.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show resolved application paths and build information.
    Info,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    ephervex_core::init_tracing("info");

    match cli.command {
        Command::Info => {
            if cli.json {
                let info = serde_json::json!({
                    "name": ephervex_model::APP_NAME,
                    "app_id": ephervex_model::APP_ID,
                    "version": ephervex_model::APP_VERSION,
                    "user_agent": ephervex_model::user_agent(),
                });
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!(
                    "{} {}",
                    ephervex_model::APP_NAME,
                    ephervex_model::APP_VERSION
                );
                println!("app id:     {}", ephervex_model::APP_ID);
                println!("user agent: {}", ephervex_model::user_agent());
            }
        }
    }

    Ok(())
}
