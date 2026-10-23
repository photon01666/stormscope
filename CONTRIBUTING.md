# Contributing to Stormscope

Thanks for taking a look! This is a small, fast-moving project; the notes below
are what keeps it that way.

## Getting set up

```sh
git clone https://github.com/photon01666/stormscope
cd stormscope
cargo check --workspace          # keep this green at every step
cargo test  --workspace          # some tests are integration tests (network)
cargo install --path crates/stormscope-app   # run it
```

Runtime requirements: Linux, Vulkan drivers, Wayland or X11. Rust stable
(edition 2024 — needs ≥ 1.85).

## Before you open a PR

- `cargo fmt` — CI enforces formatting.
- `cargo test --workspace` — CI runs the full suite. A few tests download a
  real NEXRAD volume fixture (`crates/radar-decode/tests/fixtures/download.sh`)
  and hit S3/`api.weather.gov`; they skip themselves when offline.
- `cargo clippy --workspace --all-targets` — **not enforced yet** (about 100
  pre-existing lints, mostly `collapsible_if` / `unnecessary_map_or`); the CI
  step is informational. Feel free to fix some in a dedicated PR.

## Code conventions

- No blocking I/O or heavy decode on the UI thread: background threads + a
  channel, the app polls each frame.
- Library crates use `thiserror`; the app and binaries use `anyhow`.
- Log at `warn!` for recoverable fetch/decode failures, never `panic!` on
  data errors.
- Unit-test anything with a formula behind it — projection math, calibration,
  color tables, eviction budgets, hover logic. Several past bugs (a
  georeferencing regression, a tile-provider stall, a memory creep) are
  pinned by tests; please keep that habit.
- Comments explain *why* (the surprising bit), not *what*.
- Performance-sensitive paths: measure before optimizing, and note the
  measured numbers in the commit message. `crates/radar-render/examples/`
  has the harnesses (`raster_bench`, `render_fixture`).

## Commits & PRs

- Commit messages describe the reasoning and any measurement (see the existing
  log for the expected shape).
- Keep the workspace warning-free: `cargo check --workspace --all-targets`.
- PRs: one logical change per PR when possible, and say how you tested it.
  Screenshots for UI changes are very welcome.

## Releases & repository hygiene

- Release artifacts (binaries, archives, screenshots, `dist/` output) are
  **never committed**. Build them from the tagged commit and attach them to the
  GitHub Release only; the git repository stays source-only.
- **Always purge old release artifacts from the git repository.** Remove stale
  binaries/archives from the working tree, and if one was ever committed,
  remove it from history before tagging rather than leaving it in the pack.
- When a release is superseded, delete its stale assets and draft releases on
  GitHub; keep the source tags for history.
- Before a release: `cargo fmt --all --check`, `cargo test --workspace`,
  `cargo build --release`, then bump the workspace version and CHANGELOG and
  tag the release commit.

## Reporting bugs

Please include:

- What you did, what you expected, what happened.
- The app log: run with `RUST_LOG=debug` and paste the interesting part
  (per-raster timings, memory/texture accounting, tile/provider warnings).
- Your setup: GPU + driver, Wayland/X11, scale factor, and whether it
  reproduces with a single site.

## Data etiquette

The app deliberately uses only free, public sources and behaves politely:
descriptive `User-Agent` for `api.weather.gov`, low-volume anonymous tile
fetching with a disk cache and per-provider backoff, and no API keys. Please
don't add a provider that requires an account or whose terms forbid this kind
of use.
