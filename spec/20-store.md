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
    RateLimited { kind: LimitKind, until: OffsetDateTime },
    AuthLost(AuthError),
}
```

`RateLimited` carries the kind because the UI says different things for the
two: a primary limit is "back at 14:05", a secondary one is "slow down", and
only the latter means an in-flight mutation must not be retried.

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
  node_id       TEXT NOT NULL,
  viewer        TEXT NOT NULL,       -- §3.1
  kind          TEXT NOT NULL,       -- 'pr' | 'issue' | 'repo' | …
  body          BLOB NOT NULL,       -- serialized omaghy-model type
  etag          TEXT,
  last_modified TEXT,
  fetched_at    INTEGER NOT NULL,
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
  etag TEXT, last_modified TEXT, cursor TEXT, total INTEGER,
  complete INTEGER NOT NULL, fetched_at INTEGER NOT NULL,
  PRIMARY KEY (list_key, viewer)
);

CREATE TABLE notifications (        -- REST id space, own table
  id TEXT NOT NULL, viewer TEXT NOT NULL,
  body BLOB NOT NULL, unread INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, enrichment TEXT NOT NULL,
  PRIMARY KEY (id, viewer)
);
CREATE INDEX notifications_by_viewer_updated
  ON notifications (viewer, updated_at DESC);

CREATE TABLE kv (                   -- rate limit, poll interval
  k TEXT NOT NULL, viewer TEXT NOT NULL, v BLOB NOT NULL,
  PRIMARY KEY (k, viewer)
);
```

Bodies are serialized `omaghy-model` types, not raw API JSON — translation
happens once, at the `omaghy-api` boundary, not on every cache read. That makes
the model part of the schema: a model change bumps `user_version` (§3.2) even
when the SQL is untouched.

Four columns above were not in the original DDL. Three were added in W1.2,
where the schema was first compiled, and the fourth when the dashboard was
first given real numbers:

- **`last_modified` on `entities` and `list_meta`.** §4 requires both
  validators stored beside the data; the original DDL had only `etag`, leaving
  nowhere to put the one notifications actually use.
- **`viewer` on `kv`.** §3.1 says *every* table carries it, and the values `kv`
  holds — rate-limit budget, poll interval — belong to a token, not a machine.
  A single-keyed `kv` lets one account's exhausted budget throttle another's.
- **An index on `(viewer, updated_at)`**, because every read of that table is
  "this viewer's inbox, newest first".
- **`total` on `list_meta`** — how many rows *match*, as against how many we
  hold. A dashboard section fetches at most `limit` ids and renders a count, so
  counting `list_items` reported the limit: a review queue of forty read as
  "10". The two numbers are different questions and the schema now has room for
  both. `NULL` means the fetch reported no total, and the stored ids are the
  whole answer — which is right for any list fetched complete.

`unread`, `updated_at` and `enrichment` are denormalized out of the
notification body so the inbox can be filtered and sorted, and so "which rows
still want enriching" is one query rather than a deserialize of every row. The
**column is authoritative for read state**: a body serialized before a
`mark_read` would otherwise resurrect the old value on the next read.

Notifications have their own table but still need the *list* metadata every
other collection has — freshness and the `Last-Modified` validator. Those live
in `list_meta` under the key `notifications`, rather than in a second home.

### 3.1 Viewer keying is not optional

`i_am_requested`, `my_review`, and `unread` all answer *"does this need me"*.
Every table carries `viewer`, and every query filters on it. A cache shared
across accounts silently answers the wrong question, which is worse than
failing.

### 3.2 Schema versioning

A `user_version` pragma. **On mismatch, delete the database and rebuild.** It
is a cache; migrations would be effort spent protecting data we can re-fetch.
A mismatch is not an error — the caller gets a working, empty database.

A file that is not a database is different, and is **reported** as
`CacheError::Corrupt` as well as rebuilt. `StoreError::keeps_cached_content()`
names that as the one error for which the UI must drop what it is holding, so
swallowing it would leave rows on screen that no longer have a source. The
rebuild still happens on the way out, so corruption is one bad read rather
than a permanently broken install.

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

**Conditional requests everywhere.** ETags on REST, `Last-Modified` where
GitHub offers it. A 304 costs no REST rate limit, so aggressive polling stays
cheap. Store the validator beside the data and always send it.

`Validators` is `omaghy-model`'s type (`10-domain-model.md` §4) — the fetcher
and the cache both handle it and neither may depend on the other. `omaghy-api`
extends it with the header conversions; everything below is behaviour, not
storage shape.

Measured in W1.1, against the live API: `GET /notifications` returns an `ETag`
and no `Last-Modified`. **Corrected in W2.1: it does send `Last-Modified`.**
W1.1 measured an *empty* inbox, which has no most-recently-modified thread to
report one from; against an inbox with contents the header is there. Both
recordings are committed — `notifications_conditional.json` is the empty case
and `notifications_page.json` the populated one — because the difference is
exactly the kind that would otherwise be rediscovered as a bug.

`omaghy-api` stores and sends both validators regardless. The 304 claim is
confirmed twice over: `X-RateLimit-Used` is identical across both recorded
200/304 pairs.

One more thing the populated recording shows, and it is a trap worth naming:
the 200 carries a **weak** ETag (`W/"…"`) and the matching 304 echoes the
**strong** form of the same value. Since a 304's validators are kept in
preference to the held ones (§4, `Validators::merged_with`), the stored ETag
silently changes form after the first poll. Verified against the live API that
GitHub answers 304 to either form, so this is harmless — but a client that
assumed the validator it stored is the validator it sent would have found out
the expensive way.

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
already-read thread must not error. Confirmed in W2.1 and recorded as
`notifications_mark_read.json`: `PATCH /notifications/threads/{id}` answers
`205 Reset Content` whether or not the thread was already read.

**`mark_unread` has no remote counterpart, and this section assumed it did.**
GitHub's REST API offers exactly two thread verbs — `PATCH` (read) and `DELETE`
(done) — and no way back. Verified rather than inferred: a `PATCH` carrying
`{"unread": true}` answers `205` and leaves the thread read. So `mark_unread`
is a **local** state change. The optimistic write of step 1 is the whole of the
effect, there is nothing to reconcile in step 3, and the next poll will
overwrite it with GitHub's answer. `omaghy_api::Notifications::mark_unread`
therefore sends nothing and returns `Ok`, which is the honest shape rather than
a fabricated request.

This is worth knowing before a surface offers the action: `30-ui.md` §9 says
`u` *filters* to unread rather than setting it, which happens to be the only
behaviour GitHub can support.

---

## 6. Refresh policy and rate limits

**GraphQL is points-based** (5000/hr; a combined dashboard query measured at
**1 point**), **REST is requests-based** (5000/hr, 304s free). Track both
independently from response headers in `kv`.

W2.1 measured the second batched query this design depends on: enriching a
whole page of notifications — one aliased `repository` selection per subject,
each with two single-node connections — also costs **1 point**. Points are
charged for requested *nodes*, so the aliases are nearly free and the
connections are what would not be: a `contexts(first: 100)` per subject to get
per-check counts would cost fifty times as much, which is why `CheckRollup` in
an inbox row carries GitHub's aggregate verdict and no counts.

Both arrive through the *same* `X-RateLimit-*` headers, distinguished by
**`X-RateLimit-Resource`** (`core` or `graphql`) — W1.1. A response naming any
other resource (`search` is 30/min) belongs to neither budget and is ignored
rather than filed under `core`, where it would read as an outage.

**Respect `X-Poll-Interval`.** GitHub tells you how often to poll notifications
and it is not a suggestion — ignoring it earns secondary rate limits.

Back off on: primary limit (wait for reset), secondary limit (exponential, and
never retry a mutation automatically), and 5xx (exponential, capped).

The first two are not waited out *inline*: `omaghy-api` records when sending
may resume, refuses everything until then, and fails the call, because blocking
a refresh for the minutes GitHub asked for is indistinguishable from a hang.
Only 5xx and an unreachable host are retried within one call.

Coalesce refreshes: the same target requested twice while in flight is one
request with two waiters. Cancel refreshes for surfaces the user has left.

---

## 7. Errors

```rust
pub enum StoreError {
    Auth(AuthError),                                    // missing, expired, scope
    RateLimited { kind: LimitKind, at: OffsetDateTime }, // Primary | Secondary
    Offline(String),                                    // DNS/TLS/timeout, and which
    NotFound,
    Forbidden,                                          // lost access — distinct from NotFound
    Upstream { status: u16, message: String },
    Cache(CacheError),
}
```

Two shapes differ from this section's first draft, corrected here to the ones
P0.1 landed and W1.1 is the first code to consume. `secondary: bool` became
`LimitKind`, because the two limits differ in more than a flag — a secondary
limit forbids automatic retry of a mutation and a primary one does not — and an
enum makes the distinction nameable at the call site. `Offline` carries the
cause, because "GitHub is unreachable" alone does not tell a user whether to
check their VPN or their DNS.

The distinctions earn their place by producing different UI: `Offline` with
cache shows stale data plus a banner; `Offline` without shows an empty state
naming the cause. `Forbidden` on an enriched notification is recorded as
`Enrichment::Failed` and never retried in a loop.

---

## 8. Testing

Two implementations ship, and the TUI cannot tell them apart:

- **`SqliteStore`** — the real one, in `omaghy-cache`. It owns the cache, the
  freshness policy, the events, and the optimistic half of every mutation. It
  does **not** make HTTP requests: `omaghy-api` depends on `omaghy-cache`, not
  the reverse (`CONTRIBUTING.md`), so the dependency is inverted — the store is
  handed a `Remote` that schedules fetches and delivers mutations, and
  `omaghy-sync` implements it with `omaghy-api` behind it at M1 integration.
  Nothing in `omaghy-cache` knows how to open a socket, which is how "no test
  opens a socket" is enforced rather than merely intended. The fixtures that
  drive the fetch half are `omaghy-api`'s.
- **`FakeStore`** — backed by `fake::corpus()`, with a `Behaviour` struct
  toggling staleness, emptiness, cold cache, in-flight refresh, and read/write
  failure injection. This is what surface agents build against, and what
  snapshot tests run on so screens are deterministic. Its clock is fixed
  (`FIXTURE_NOW`) so relative ages never make a snapshot fail on a Tuesday.

`FakeStore` must be able to produce every arm of §7 on demand. Error states are
the ones that get skipped otherwise, and they are most of what a user sees on a
bad day.
