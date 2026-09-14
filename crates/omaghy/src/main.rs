//! omaghy — a full GitHub client for the terminal.
//!
//! See `spec/00-overview.md`.

use anyhow::{Context, Result, bail};
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
    // Still the fixture corpus: wiring SqliteStore to the API is M1
    // integration. `OMAGHY_FAKE` makes the unhappy half of the state matrix
    // reachable by running the program — without it only "populated" could be
    // seen by a human, so most of `30-ui.md` §8 was unsmokeable. Found by W2.2.
    let store = Arc::new(fake_store(std::env::var("OMAGHY_FAKE").ok().as_deref())?);
    let now = omaghy_store::fake::FIXTURE_NOW;

    // Without this the failure is `No such device or address (os error 6)`,
    // which is what you get piping omaghy, running it from a script, or in a
    // container. Name the actual problem instead.
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!(
            "omaghy needs an interactive terminal — stdout is not a TTY.\n\
             If you are piping or scripting, there is no non-interactive mode yet."
        );
    }

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

/// Build the fixture store, optionally misbehaving.
///
/// `OMAGHY_FAKE=offline` and friends exist so every screen in `30-ui.md` §8
/// can be reached by running the program, not only by a snapshot test.
fn fake_store(mode: Option<&str>) -> Result<FakeStore> {
    use omaghy_model::{AuthError, LimitKind, StoreError};
    use omaghy_store::fake::Behaviour;

    let store = match mode {
        Some("empty") => return Ok(FakeStore::empty()),
        _ => FakeStore::with_corpus(),
    };
    let behaviour = match mode {
        None | Some("") => return Ok(store),
        Some("stale") | Some("offline") => Behaviour::offline_with_cache(),
        Some("cold") => Behaviour::offline_without_cache(),
        Some("refreshing") => Behaviour {
            refreshing: true,
            ..Default::default()
        },
        Some("forbidden") => Behaviour::failing(StoreError::Forbidden),
        Some("unauthorized") => Behaviour::failing(StoreError::Auth(AuthError::Rejected)),
        Some("ratelimited") => Behaviour::failing(StoreError::RateLimited {
            kind: LimitKind::Primary,
            at: omaghy_store::fake::FIXTURE_NOW,
        }),
        Some("error") => Behaviour::failing(StoreError::Upstream {
            status: 502,
            message: "bad gateway".into(),
        }),
        Some(other) => bail!(
            "OMAGHY_FAKE={other} is not a mode; try one of: \
             empty, stale, offline, cold, refreshing, forbidden, unauthorized, \
             ratelimited, error"
        ),
    };
    store.set_behaviour(behaviour);
    Ok(store)
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
