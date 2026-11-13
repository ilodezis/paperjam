use std::process::ExitCode;

use clap::Parser;
use mimalloc::MiMalloc;

// glibc malloc keeps per-thread arenas full of freed OCR and image buffers
// forever; mimalloc hands them back to the OS (see `paperjam::alloc`).
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> ExitCode {
    // Before the runtime exists: the huge-page flag only applies to memory
    // mapped afterwards, and the runtime's worker threads are the first
    // allocations of the process.
    paperjam::alloc::tune();
    let cli = paperjam::cli::Cli::parse();
    // The parsing worker needs no runtime, database or logging.
    if matches!(cli.cmd, paperjam::cli::Command::ExtractWorker) {
        return paperjam::extract::worker::child::run();
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(cli))
}

async fn run(cli: paperjam::cli::Cli) -> ExitCode {
    if let Err(e) = paperjam::logging::init() {
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    match paperjam::cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
