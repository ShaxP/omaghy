//! omaghy — a full GitHub client for the terminal.
//!
//! See `spec/00-overview.md`.

use anyhow::{Context, Result};
use clap::Parser;
use omaghy_store::FakeStore;
use omaghy_tui::{App, Route, terminal};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "omaghy",
    version,
    about = "A full GitHub client for the terminal"
)]
struct Cli {
    /// Route to open, e.g. `notifications` or `pr:ShaxP/shax#61`.
    #[arg(default_value = "notifications")]
    route: String,

    /// Write a log here instead of the default state directory.
    #[arg(long, env = "OMAGHY_LOG")]
    log: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let route: Route = cli
        .route
        .parse()
        .with_context(|| format!("`{}` is not a route; try `omaghy notifications`", cli.route))?;

    init_logging(cli.log.as_deref())?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(route))
}

async fn run(route: Route) -> Result<()> {
    // P0.4 has no real store yet: SqliteStore arrives in W1.2. Until then the
    // shell runs against the fixture corpus, which is enough to prove the
    // Store reaches the screen.
    let store = Arc::new(FakeStore::with_corpus());
    let now = omaghy_store::fake::FIXTURE_NOW;

    let mut tui = terminal::init().context("could not set up the terminal")?;
    let _guard = terminal::Guard;

    let mut app = App::new(store, now);
    let result = async {
        app.start(route).await?;
        app.run(&mut tui).await
    }
    .await;

    // The guard also restores on unwind; doing it here keeps the ordering
    // obvious for the normal path.
    terminal::restore().ok();
    result.map_err(Into::into)
}

/// A TUI owns the screen, so there is no `println` to debug with. Logs go to a
/// file, quietly, and never to stdout.
fn init_logging(explicit: Option<&std::path::Path>) -> Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};

    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let dirs =
                directories::ProjectDirs::from("", "", "omaghy").context("no home directory")?;
            let dir = dirs.state_dir().unwrap_or_else(|| dirs.data_dir());
            std::fs::create_dir_all(dir)?;
            dir.join("omaghy.log")
        }
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_env("OMAGHY_LOG_LEVEL").unwrap_or_else(|_| "info".into()),
        )
        .init();
    tracing::info!(?path, "omaghy starting");
    Ok(())
}
