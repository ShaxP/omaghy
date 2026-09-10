//! omaghy — a full GitHub client for the terminal.
//!
//! See `spec/00-overview.md`.

use anyhow::{Context, Result, bail};
use clap::Parser;
use omaghy_api::viewer_login;
use omaghy_cache::SqliteStore;
use omaghy_store::{FakeStore, Store, Viewer};
use omaghy_sync::{PollConfig, Syncer};
use omaghy_tui::{App, Route, terminal};
use std::sync::Arc;
use time::OffsetDateTime;

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
    // Without this the failure is `No such device or address (os error 6)`,
    // which is what you get piping omaghy, running it from a script, or in a
    // container. Name the actual problem instead.
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!(
            "omaghy needs an interactive terminal — stdout is not a TTY.\n\
             If you are piping or scripting, there is no non-interactive mode yet."
        );
    }

    // Checked before the store is built: `real_store` authenticates and asks
    // GitHub who we are, and spending a round trip to then refuse to start is
    // rude to both ends.
    // `OMAGHY_FAKE` keeps the fixture corpus reachable, because it is the only
    // way a human can see the unhappy half of `30-ui.md` §8 — a real inbox
    // will not produce "rate limited" on demand. Absent, omaghy talks to
    // GitHub.
    let fake = std::env::var("OMAGHY_FAKE").ok();
    let (store, now): (Arc<dyn Store>, OffsetDateTime) = match fake.as_deref() {
        Some(mode) => (
            Arc::new(fake_store(Some(mode))?),
            omaghy_store::fake::FIXTURE_NOW,
        ),
        None => (real_store().await?, OffsetDateTime::now_utc()),
    };

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

/// The real thing: a cache on disk, fed by GitHub.
///
/// Construction is circular by nature — the store holds the remote, and the
/// remote writes back into the store — so the syncer is built first, handed to
/// the store, and only then given its way back (`omaghy_sync::Syncer::attach`).
async fn real_store() -> Result<Arc<dyn Store>> {
    use omaghy_api::{GitHubClient, ReqwestTransport, resolve_token};

    let resolved = resolve_token().context("could not find a GitHub token")?;
    tracing::info!(source = ?resolved.source, "token resolved");

    // `ReqwestTransport::new` installs the `ring` crypto provider — not
    // `aws-lc-rs`, which would need cmake (PREREQUISITES.md §5.1).
    let transport = Arc::new(ReqwestTransport::new().context("could not build an HTTP client")?);
    let client = Arc::new(GitHubClient::new(resolved.token, transport));

    // The cache is keyed by viewer, so this has to happen before it opens.
    // One GraphQL point, once per start — and it doubles as the check that the
    // token actually works, which is worth failing on here rather than three
    // screens later.
    let login = viewer_login(&client)
        .await
        .context("could not ask GitHub who this token belongs to")?;
    tracing::info!(%login, "authenticated");

    let syncer = Syncer::new(client);
    let store =
        Arc::new(open_cache(&cache_path()?, Viewer::new(login))?.with_remote(syncer.clone()));
    syncer.attach(&store);

    // Nothing refreshed on its own before this: omaghy fetched on entering a
    // surface and on `r`, so an inbox left open showed the morning's rows all
    // afternoon. The handles are dropped deliberately — the tasks hold a
    // `Weak` to the store and end when the TUI drops it.
    syncer.start_polling(PollConfig::default());

    Ok(store)
}

/// Open the cache, tolerating one that has to be rebuilt on the way in.
///
/// `Cache::open` rebuilds a corrupt file *and still returns the error*, so a
/// caller holding rows from the file just deleted learns they are stale
/// (`spec/20-store.md` §3.2). At startup nobody holds anything, and refusing
/// to launch over a cache we have already replaced is the wrong answer — a
/// half-deleted `cache.db` made omaghy exit before drawing a frame.
fn open_cache(path: &std::path::Path, viewer: Viewer) -> Result<SqliteStore> {
    use omaghy_model::{CacheError, StoreError};

    match SqliteStore::open(path, viewer.clone()) {
        Ok(store) => Ok(store),
        Err(StoreError::Cache(CacheError::Corrupt(why))) => {
            tracing::warn!(%why, "cache was unusable and has been rebuilt");
            // The rebuild has already happened; this opens what it left.
            SqliteStore::open(path, viewer).context("could not open the rebuilt cache")
        }
        Err(e) => Err(e).context("could not open the cache"),
    }
}

/// `$XDG_CACHE_HOME/omaghy/cache.db`.
fn cache_path() -> Result<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "omaghy").context("no home directory")?;
    let dir = dirs.cache_dir();
    std::fs::create_dir_all(dir)?;
    Ok(dir.join("cache.db"))
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
