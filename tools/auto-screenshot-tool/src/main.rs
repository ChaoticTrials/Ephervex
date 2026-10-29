//! Documentation screenshots from the compiled Ephervex interface.

mod artwork;
mod capture;
mod catalog;
mod fixtures;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

use crate::capture::{CaptureOptions, Dimensions, ThemeName};
use crate::catalog::STATES;

#[derive(Parser)]
#[command(about = "Capture Ephervex documentation screenshots from synthetic UI data")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate every documented UI state and output variant.
    Generate(GenerateArgs),
}

#[derive(Args)]
struct GenerateArgs {
    /// Generate only these states. Repeat to select more than one.
    #[arg(long)]
    state: Vec<String>,
    /// Generate only these themes: light, dark, halloween, christmas, pride, or
    /// april-fools. Repeat to select more than one.
    #[arg(long)]
    theme: Vec<ThemeName>,
    /// Capture these window dimensions as WIDTHxHEIGHT. Repeat to select more.
    #[arg(long)]
    size: Vec<Dimensions>,
    /// Use this bundled interface locale.
    #[arg(long, default_value = "en-US")]
    locale: String,
    /// Directory for generated PNG files.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Derive 1080p full-window files from 1440p only for states marked layout-identical.
    #[arg(long)]
    derive_1080: bool,
    /// Directory holding the project logos fetched for the recorded mod rows.
    #[arg(long)]
    artwork_cache: Option<PathBuf>,
    /// Fetch no logos. Logos missing from the cache are left empty.
    #[arg(long)]
    offline: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Generate(args) => run(args),
    }
}

fn run(args: GenerateArgs) -> Result<()> {
    ensure!(
        capture::DRAWS_WITH_SKIA,
        "this build draws with the software renderer, which leaves out item transforms and rounded clips; run tools/auto-screenshot-tool/generate.sh, which builds with --features skia"
    );
    ephervex_i18n::select_language(&args.locale)
        .with_context(|| format!("select screenshot locale {:?}", args.locale))?;

    let replace_default_output =
        args.output.is_none() && args.state.is_empty() && args.theme.is_empty();
    let states = if args.state.is_empty() {
        STATES.to_vec()
    } else {
        args.state
            .iter()
            .map(|name| {
                STATES
                    .iter()
                    .find(|state| state.name == name)
                    .copied()
                    .with_context(|| format!("unknown state {name:?}"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let themes = if args.theme.is_empty() {
        ThemeName::ALL.to_vec()
    } else {
        args.theme
    };
    let sizes = if args.size.is_empty() {
        Dimensions::ALL.to_vec()
    } else {
        args.size
    };
    if args.derive_1080 {
        ensure!(
            sizes.contains(&Dimensions::HD) && sizes.contains(&Dimensions::QHD),
            "--derive-1080 needs both 1920x1080 and 2560x1440 outputs"
        );
    }
    let output = args
        .output
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("screenshots"));

    let artwork_cache = args
        .artwork_cache
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("artwork"));
    artwork::prepare(&artwork_cache, args.offline)?;
    capture::initialize()?;
    if replace_default_output && output.exists() {
        std::fs::remove_dir_all(&output)
            .with_context(|| format!("replace generated screenshots in {}", output.display()))?;
    }
    let files = capture::generate(&CaptureOptions {
        states: &states,
        themes: &themes,
        sizes: &sizes,
        output: &output,
        derive_1080: args.derive_1080,
    })?;
    println!(
        "Generated {} PNG files in {}",
        files.len(),
        output.display()
    );
    Ok(())
}
