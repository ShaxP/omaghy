# Prerequisites

What you need to build and run omaghy. Kept current as dependencies land —
see [Keeping this current](#keeping-this-current).

> **Status.** Requirements marked ✅ are verified on a real machine by code
> that exercises them; ⚪ are optional. Every ⏳ (a policy decision taken before
> any code exercised it) has since been promoted — the last two, TLS and
> SQLite in §5, by M1 running against the live API. Nothing here is guesswork
> about what *might* be needed.

---

## 1. Quick start — Arch / Omarchy

```bash
sudo pacman -S --needed rustup base-devel git github-cli
rustup default stable
git clone https://github.com/ShaxP/omaghy.git && cd omaghy
cargo build --release
```

Everything else on this page is either already present on a stock Omarchy
install or optional.

---

## 2. Build requirements

| | Why | Arch package |
|---|---|---|
| ✅ **Rust, stable** | Edition 2024, resolver 3. Version is pinned by `rust-toolchain.toml`, and `rustup` honours it automatically. | `rustup` |
| ✅ **`clippy`, `rustfmt`** | Required by CI; `rust-toolchain.toml` installs them with the toolchain. | — |
| ✅ **C compiler + linker** | SQLite is compiled from source (§5.2). | `base-devel` |
| ✅ **`pkg-config`** | Probing for system libraries. | `base-devel` |
| ✅ **No `cmake`** | Deliberate — see §5.1. Verified in P0.2: the lockfile contains `ring`, and no `aws-lc-rs` or `aws-lc-sys`. | — |
| ✅ **No OpenSSL** | Deliberate — see §5.1. Verified in P0.2: no `openssl`, `openssl-sys` or `native-tls` in the lockfile. | — |

`Cargo.lock` is committed, so a plain `cargo build` resolves exactly the
dependency versions CI tested. Use `cargo build --locked` to make a mismatch an
error rather than a silent update — this is what a packager should use.

Disk: a full debug build of a workspace this size, with `reqwest` and
`rusqlite` bundled, is realistically **1.5–3 GB** in `target/`. Budget for it.
The dependency tree is 358 crates as of M1.

---

## 3. Runtime requirements

| | Why |
|---|---|
| ✅ **A truecolor terminal** | omaghy assumes 24-bit colour. It does **not** require any particular terminal — images degrade kitty-protocol → sixel → half-block → coloured initials. Omarchy's default is `foot` (sixel, no kitty protocol). |
| ✅ **A GitHub token** | Resolved as `OMAGHY_TOKEN` → `GH_TOKEN` → `gh auth token`. The last needs [`gh`](https://cli.github.com) and `gh auth login`; there is no OAuth flow and no stored credential of our own. |
| ✅ **Network access to `api.github.com`** | Reads are cache-first, so omaghy starts and renders offline — but it cannot fetch anything new. |
| ⚪ **`git`** | Clone and checkout handoff — the Repositories surface, M4. Nothing shells out to it yet. |
| ⚪ **`xdg-open` or `$BROWSER`** | The `o` key opens the current thing on github.com. `$BROWSER` wins if set, `xdg-open` (from `xdg-utils`) otherwise. Without either, `o` names the URL it could not open and why, so it can still be copied. |
| ⚪ **A Nerd Font** | Octicons — PR, merge, issue, check glyphs — come from the Nerd Font glyph range. Without one you get replacement boxes where icons should be. Omarchy ships JetBrainsMono Nerd Font. omaghy must stay legible without it, but it will look worse. |

### Required token scopes

`repo` · `read:org` · `workflow`. Notifications work under `repo`; a dedicated
`notifications` scope is not needed. `gh auth login` grants a superset of these
by default.

Deliberately **not** asked for: `read:project`. Four pull-request timeline
event types (the ProjectV2 ones) can only be read with it; omaghy leaves them
unselected and shows them as a bare event with no actor rather than demand a
scope for a line nobody reads.

---

## 4. Development extras

| | Why | Install |
|---|---|---|
| ✅ **`gh`** | The PR workflow in `CONTRIBUTING.md`. | `pacman -S github-cli` |
| ⚪ **`cargo-nextest`** | Faster, clearer test runs. CI uses plain `cargo test`. | `cargo install cargo-nextest` |
| ⚪ **`cargo-insta`** | Reviewing snapshot diffs for TUI screens (`spec/00-overview.md` §7). | `cargo install cargo-insta` |

---

## 5. Dependency policy

Two decisions taken deliberately, because both are far cheaper to hold to from
the start than to retrofit.

### 5.1 No OpenSSL — `rustls` with the `ring` provider

TLS is `rustls`, not `native-tls`. No OpenSSL headers, no version skew against
whatever the distro ships, no linkage surprises.

Two sub-decisions that follow:

- **Crypto provider is `ring`, not `aws-lc-rs`.** `aws-lc-rs` is `rustls`'
  default in current versions and it requires `cmake` — plus `nasm` on some
  targets. `ring` needs only the C compiler already required by §5.2. The
  provider must be pinned explicitly; taking the default silently adds a build
  dependency.

  **In practice this is a feature-flag trap.** `reqwest`'s plain `rustls`
  feature selects `aws-lc-rs`. The combination that does not is:

  ```toml
  reqwest = { default-features = false,
              features = ["rustls-no-provider", "rustls-native-certs", …] }
  rustls  = { default-features = false, features = ["ring", "std", "tls12"] }
  ```

  `omaghy-api` then installs the provider once at startup. Verify with
  `grep -E '^name = "(ring|aws-lc-sys|openssl-sys)"' Cargo.lock` — `ring`
  should be the only hit.
- **Root certificates come from the system store** via `rustls-native-certs`,
  not from a compiled-in `webpki-roots` bundle. Compiled-in roots break anyone
  behind a corporate TLS-inspecting proxy, and fail in a way that looks like a
  network bug.

### 5.2 SQLite is bundled, not system

`rusqlite` with the `bundled` feature compiles SQLite from source rather than
linking the system library.

- Reproducible: the SQLite version is the one we tested against, not whatever
  the distro ships.
- Distributable: no runtime `.so` dependency, so a built binary can simply be
  copied.
- Cost: a C compiler at build time (§2) and roughly 30 extra seconds on a clean
  build.

The system `sqlite` package is **not** required at build or run time. It's
present on most systems anyway — omaghy just doesn't use it.

---

## 6. Other platforms

omaghy targets Omarchy first but is an ordinary terminal program. Nothing in
§2–§3 is Linux-specific beyond the package names.

| | Build packages |
|---|---|
| **Debian / Ubuntu** | `build-essential pkg-config git gh` + [rustup](https://rustup.rs) |
| **Fedora** | `@development-tools pkgconf-pkg-config git gh` + rustup |
| **macOS** | Xcode CLT (`xcode-select --install`), `brew install gh` + rustup |
| **NixOS** | A flake is not provided yet. `gcc`, `pkg-config`, `rustup`, `gh`. |

Untested on macOS and non-Arch Linux. Reports welcome.

---

## Keeping this current

**Any PR that adds a dependency with a system requirement must update this
file in the same PR.** A prerequisites page discovered to be wrong at build
time is worse than none, because it was trusted.

Specifically, say so here when a change:

- adds a crate needing a system library, compiler, or build tool
- changes the TLS or SQLite decisions in §5
- adds a runtime requirement (a binary omaghy shells out to, a font, a protocol)
- changes the minimum Rust version or edition

When a ⏳ item is first exercised by real code, confirm it on a clean machine
and promote it to ✅.
