# Nimbo SQLx SQLite driver patch

This directory vendors the unmodified `sqlx-sqlite` 0.9.0 source with one
dependency-range change in `Cargo.toml`:

```toml
[dependencies.libsqlite3-sys]
version = ">=0.39.0, <0.40.0"
```

Upstream SQLx 0.9.0 accepts `libsqlite3-sys >=0.30.1, <0.38.0`, which resolves
to a bundled SQLite older than 3.54.0. RustSec RUSTSEC-2026-0459
(CVE-2026-119375) is a local information-disclosure issue in those bundled
versions. `libsqlite3-sys` 0.39.1 bundles SQLite 3.54.1 and provides the same
FFI surface used by this driver.

The local patch is declared in `backend/Cargo.toml` under
`[patch.crates-io]`. Keep it only until a SQLx release directly accepts
`libsqlite3-sys >=0.39`. When upgrading SQLx, first try removing this patch and
run `cargo audit`, `cargo test --locked`, Clippy, and the security regression
script. If the patch is still required, refresh these files from the exact
matching upstream `sqlx-sqlite` release and reapply only the dependency-range
change.

Files that are generated package metadata (`Cargo.toml.orig`, package lock,
`.cargo-ok`, `.cargo_vcs_info.json`) are intentionally omitted. License files
and source are retained from the upstream crate.
