use std::path::PathBuf;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "kg",
    author,
    version,
    about = "High-performance multi-language code knowledge graph engine",
    long_about = "Deterministic, embedded, incremental code intelligence engine for multi-language repositories."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new knowledge graph repository store (.kg)
    Init {
        /// Target repository path
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Display status and statistics of the knowledge graph
    Status {
        /// Target repository path
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    // Initialize tracing logger
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            let kg_dir = path.join(".kg");
            if kg_dir.exists() {
                println!("Knowledge graph already initialized at: {}", kg_dir.display());
            } else {
                std::fs::create_dir_all(&kg_dir)?;
                println!("Initialized empty knowledge graph repository at: {}", kg_dir.display());
            }
        }
        Commands::Status { path } => {
            let kg_dir = path.join(".kg");
            if !kg_dir.exists() {
                println!("No knowledge graph found at: {}. Run 'kg init' first.", path.display());
            } else {
                println!("Knowledge graph directory active at: {}", kg_dir.display());
            }
        }
    }

    Ok(())
}
