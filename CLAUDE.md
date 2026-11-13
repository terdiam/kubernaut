# Rust engineering rules for Kubernaut

Act as an expert Rust systems engineer. Priorities, in order: low RAM, low CPU,
modular reusable code. These rules apply to every Rust line you write or change
here. Follow them on the first pass so nothing needs a later optimisation
refactor.

## Hot paths

Be strict on hot paths: per-frame render, applying a watch batch, row
projection, log and terminal streaming. Cold paths (startup, dialogs, one-off
commands) may favour clarity, but still follow the safety rules.

Owned data is fine at these boundaries, so do not contort code to avoid it:
`tokio::spawn` (`'static`), GPUI `cx.listener(move ..)` closures, serde
input/output, and values that outlive the call.

## 1. Memory

- No `.clone()`, `.to_owned()`, `.to_string()` or `String::from()` unless
  ownership is required. Borrow instead.
- Take `&str` not `String`, `&[T]` not `Vec<T>`, `&Path` not `PathBuf`.
  Take `impl AsRef<str>` / `impl Into<String>` only when callers differ.
- Share large read-only data with `Arc<T>` / `Arc<str>` instead of cloning it.
  Cloning an `Arc` is fine; cloning what it points at is not.
- Know the size up front? `Vec::with_capacity`, `String::with_capacity`,
  `HashMap::with_capacity`. Reuse buffers across loop iterations (`clear()`).
- Small fixed collections: arrays, or `SmallVec` / `ArrayVec` if that crate is
  already a dependency. Do not add a dependency for one call site.
- Do not `collect()` into a `Vec` just to iterate again; keep the iterator.
- Return iterators (`impl Iterator<Item = &T> + '_`) rather than building a
  `Vec` for the caller to loop over once.

## 2. CPU

- Static dispatch: generics and `impl Trait`. No `Box<dyn Trait>` / `&dyn Trait`
  unless the set of types is open at runtime and the call is not hot.
- Prefer iterator chains over index loops; they elide bounds checks.
- `#[inline]` on small, frequently called functions. Not on large ones.
- Concurrency: channels or atomics before `Mutex`. If a lock is needed, keep
  the critical section tiny, never hold it across `.await`, and prefer
  `parking_lot` (already a dependency) or `arc_swap` for read-mostly state.
- Never block the GPUI thread or a tokio worker. Kubernetes I/O stays on the
  tokio runtime; the UI only receives results.
- Render and apply deltas, not whole snapshots. Do not rebuild a list that a
  keyed update can patch.

## 3. Modularity

- Behaviour goes behind small traits, not inheritance-style structs.
- Write generic functions and structs so a module is reusable across types.
- One responsibility per `mod`. Shared, UI-free logic belongs in a crate with no
  window-system or IPC dependency (`k8s-app`, `k8s-core`, `k8s-ops`), not in the
  UI crate.
- Pure logic goes in plain functions that take data and return data, so it can
  be unit-tested without a window (see `visible_groups` in `kubernaut-ui`).

## 4. Safety and lints

- No `.unwrap()` or `.expect()` in non-test code. Return `Result<T, E>` and use
  `?`. Define errors with `thiserror`. At a true top level (`main`), log with
  `tracing::error!` and exit non-zero instead of panicking.
- No `panic!`, `unreachable!` or indexing that can fail on external input; use
  `get`, `first`, `split_first`, `checked_*`.
- No `unsafe` unless there is no alternative; keep it minimal and justify it in
  a comment.
- Code must pass `cargo clippy --workspace --all-targets -- -D warnings`.
  `kubernaut-ui` additionally denies `unwrap_used` and `expect_used`.
- Comments only for a non-obvious WHY. No narration of what the code does.

## 5. Build profiles

- `release`: shipping build (thin LTO, `opt-level = 3`, symbols kept so crash
  logs have function names).
- `release-small`: smallest binary (`opt-level = "z"`, fat LTO, symbols
  stripped). Measured on `kubernaut-ui` at the end of phase 3: 10.3 MB against
  24.3 MB for `release`, with a similar build time (about 3 minutes). Crash backtraces lose function names. Build with
  `cargo build --profile release-small -p <crate>`.
- Do not change profiles to hide a size or speed problem in code. Measure first.

## 6. Before saying done

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Check the `cargo` exit code itself: a pipeline like `cargo build | tail` reports
`tail`'s exit code, not cargo's.

## Project notes

- UI is migrating from Tauri + React to GPUI on branch `gpui-rewrite`.
  New UI code goes in `crates/kubernaut-ui`; shared state in `crates/k8s-app`.
- Depend on `gpui-kit` only, never `gpui` directly: it pins a gpui fork family
  and a second copy would give incompatible types.
- `use gpui_kit::*` also imports GPUI's `#[test]` macro and shadows Rust's. In
  `#[cfg(test)]` modules import names explicitly, never `use super::*`.
- Needs rustc 1.93 or newer.
