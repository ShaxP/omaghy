# omaghy — the Store

`Store` is the only seam `omaghy-tui` sees. It lives in its own crate,
`omaghy-store` — see `00-overview.md` §4. Everything about the network, the
cache, freshness, and retries lives behind it.

It exists for two reasons. First, **latency is the design problem**: ~700ms per
GitHub round trip means no surface can afford to wait for the network before it
paints. Second, **it is the parallel-work boundary** — UI agents build against
a fake implementation while API agents build against recorded fixtures, and
they meet at this trait.

---

## 1. The contract

**Reads never touch the network.** They answer from SQLite, immediately,
possibly stale, possibly empty. A read that would block on a socket is a bug.

**Refresh is fire-and-forget.** `refresh()` schedules work and returns. When it
lands, subscribers get an event and re-query. There is no "await the fresh
data" call, because offering one guarantees somebody awaits it in a draw path.

**Every read carries its provenance.** The UI must be able to say "showing
cached results from 3 minutes ago, refreshing" rather than silently lying.

```rust
pub struct Fresh<T> {
    pub value: T,
    pub fetched_at: Option<OffsetDateTime>,   // None = never fetched
    pub source: Source,                        // Cache | Network
    pub stale: bool,                           // past its TTL (§4)
    pub refreshing: bool,                      // a refresh is in flight now
}
```

```rust
#[async_trait]
pub trait Store: Send + Sync + 'static {
    fn subscribe(&self) -> broadcast::Receiver<StoreEvent>;
    fn viewer(&self) -> &Viewer;

    // reads — cache only, never block on network
    async fn dashboard(&self, cfg: &DashboardConfig) -> Result<Fresh<Dashboard>>;
    async fn notifications(&self, q: &NotificationQuery) -> Result<Fresh<Page<Notification>>>;
    async fn pull_requests(&self, q: &PrQuery) -> Result<Fresh<Page<PullRequest>>>;
    async fn pull_request(&self, r: &SubjectRef) -> Result<Fresh<Option<PrDetail>>>;
    async fn issues(&self, q: &IssueQuery) -> Result<Fresh<Page<Issue>>>;
    // … one per surface

    // refresh — schedules, returns immediately
    fn refresh(&self, target: RefreshTarget);
    fn cancel(&self, target: &RefreshTarget);

    // mutations — optimistic, see §5
    async fn mark_read(&self, ids: &[NotificationId]) -> Result<()>;
    async fn submit_review(&self, r: &SubjectRef, review: ReviewInput) -> Result<()>;
    // …
}
```

```rust
pub enum StoreEvent {
    Updated(RefreshTarget),                 // new data landed; re-query
    RefreshStarted(RefreshTarget),
    RefreshFailed { target: RefreshTarget, error: StoreError },
    RateLimited { until: OffsetDateTime },
    AuthLost(AuthError),
}
```

Events say *what changed*, never carry the data. Carrying payloads means two
paths into the UI's state and they diverge; re-querying the cache is cheap and
has one.

---

## 2. What the Store does not do

It does not format, sort for display, paginate for the viewport, or hold cursor
position. Those are `omaghy-tui`'s. It also does not decide *when* a surface
wants data — surfaces call `refresh()` on open and on demand; only the
background poller (§6) has a timetable of its own.

---

## 3. Cache

SQLite via `rusqlite`, at `$XDG_CACHE_HOME/omaghy/cache.db`. WAL mode, so
`omaghy watch` and the TUI can share it without a protocol.

```sql
CREATE TABLE entities (
  node_id    TEXT NOT NULL,
  viewer     TEXT NOT NULL,          -- §3.1
  kind       TEXT NOT NULL,          -- 'pr' | 'issue' | 'repo' | …
  body       BLOB NOT NULL,          -- serialized omaghy-model type
  etag       TEXT,
  fetched_at INTEGER NOT NULL,
  PRIMARY KEY (node_id, viewer)
);

-- List membership is stored apart from entities, so one PR changing does not
-- invalidate every list it appears in.
CREATE TABLE list_items (
  list_key TEXT NOT NULL, viewer TEXT NOT NULL,
  position INTEGER NOT NULL, node_id TEXT NOT NULL,
  PRIMARY KEY (list_key, viewer, position)
);
CREATE TABLE list_meta (
  list_key TEXT NOT NULL, viewer TEXT NOT NULL,
  etag TEXT, cursor TEXT, complete INTEGER NOT NULL,
  fetched_at INTEGER NOT NULL,
  PRIMARY KEY (list_key, viewer)
);

CREATE TABLE notifications (        -- REST id space, own table
  id TEXT NOT NULL, viewer TEXT NOT NULL,
  body BLOB NOT NULL, unread INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, enrichment TEXT NOT NULL,
  PRIMARY KEY (id, viewer)
);

CREATE TABLE kv (k TEXT PRIMARY KEY, v BLOB NOT NULL);  -- rate limit, poll interval, last-modified
```

Bodies are serialized `omaghy-model` types, not raw API JSON — translation
happens once, at the `omaghy-api` boundary, not on every cache read.

### 3.1 Viewer keying is not optional

`i_am_requested`, `my_review`, and `unread` all answer *"does this need me"*.
Every table carries `viewer`, and every query filters on it. A cache shared
across accounts silently answers the wrong question, which is worse than
failing.

### 3.2 Schema versioning

A `user_version` pragma. **On mismatch, delete the database and rebuild.** It
is a cache; migrations would be effort spent protecting data we can re-fetch.

---

## 4. Freshness

Per-kind TTL decides `stale`, which drives whether a surface auto-refreshes on
open and whether the UI says so:

| Kind | TTL |
|---|---|
| Notifications | poll interval from GitHub (§6), floor 60s |
| Dashboard | 5 min |
| PR / issue lists | 5 min |
| PR / issue detail | 2 min |
| Check runs | 30s |
| Repo metadata | 24 h |

Stale never means hidden. Stale data renders normally with an indicator; only
*absent* data shows an empty state.

**Conditional requests everywhere.** ETags on REST, `Last-Modified` on
notifications. A 304 costs no REST rate limit, so aggressive polling stays
cheap. Store the validator beside the data and always send it.

---

## 5. Mutations

Optimistic, because a 700ms wait on a keypress feels broken.

1. Write the new state to the cache and emit `Updated`.
2. Fire the request.
3. On success, reconcile with the server's response.
4. On failure, roll back, emit `Updated` again, and surface a toast.

Rollback needs the prior value, so mutations capture it before writing. In-
flight mutations are tracked so a refresh landing mid-flight does not resurrect
the old state — the pending change wins until it resolves.

Every mutation is idempotent where GitHub allows it; `mark_read` on an
already-read thread must not error.

---

## 6. Refresh policy and rate limits

**GraphQL is points-based** (5000/hr; a combined dashboard query measured at
**1 point**), **REST is requests-based** (5000/hr, 304s free). Track both
independently from response headers in `kv`.

**Respect `X-Poll-Interval`.** GitHub tells you how often to poll notifications
and it is not a suggestion — ignoring it earns secondary rate limits.

Back off on: primary limit (wait for reset), secondary limit (exponential, and
never retry a mutation automatically), and 5xx (exponential, capped).

Coalesce refreshes: the same target requested twice while in flight is one
request with two waiters. Cancel refreshes for surfaces the user has left.

---

## 7. Errors

```rust
pub enum StoreError {
    Auth(AuthError),                                    // missing, expired, scope
    RateLimited { until: OffsetDateTime, secondary: bool },
    Offline,                                            // DNS/TLS/timeout
    NotFound,
    Forbidden,                                          // lost access — distinct from NotFound
    Upstream { status: u16, message: String },
    Cache(CacheError),
}
```

The distinctions earn their place by producing different UI: `Offline` with
cache shows stale data plus a banner; `Offline` without shows an empty state
naming the cause. `Forbidden` on an enriched notification is recorded as
`Enrichment::Failed` and never retried in a loop.

---

## 8. Testing

Two implementations ship, and the TUI cannot tell them apart:

- **`SqliteStore`** — the real one. Tested against recorded HTTP fixtures; no
  test opens a socket.
- **`FakeStore`** — backed by `fixtures/`, with knobs for staleness, latency,
  failure injection, and rate limiting. This is what surface agents build
  against, and what snapshot tests run on so screens are deterministic.

`FakeStore` must be able to produce every arm of §7 on demand. Error states are
the ones that get skipped otherwise, and they are most of what a user sees on a
bad day.
