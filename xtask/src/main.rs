use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Development tasks for paperjam")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate the synthetic fixture corpus into fixtures/gen/
    Fixtures,
    /// Check that locale catalogs have matching keys
    I18nCheck {
        /// Directory with the catalogs (default: web/src/locales)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Fixtures => {
            let out = workspace_root().join("fixtures").join("gen");
            xtask::fixtures::generate(&out)?;
            println!("fixtures written to {}", out.display());
            Ok(())
        }
        Command::I18nCheck { dir } => {
            let dir = dir.unwrap_or_else(|| workspace_root().join("web/src/locales"));
            let report = xtask::i18n::check(&dir)?;
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
            for error in &report.errors {
                eprintln!("error: {error}");
            }
            if report.errors.is_empty() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
    }
}
