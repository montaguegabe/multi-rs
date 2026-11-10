# multi-rs

Rust implementation of `multi` (CLI + `multi-core` library). Remake; the Python `multi` repo is reference only and must never be pushed to from here.

- `crates/multi-core`: all behavior. Keep the CLI (`crates/multi-cli`) a thin clap layer over it; the desktop app links `multi-core` directly.
- Behavior and file formats (`multi.json`, managed `.gitignore` blocks, generated `.vscode` files, `AGENTS.parts` output) must stay compatible with existing multi workspaces.
- All git access shells out to `git`; do not add libgit2.
- Run `cargo fmt --all`, `cargo clippy --all-targets`, and `cargo test` before committing. Integration tests create temporary git repos and redirect `~/.multi` via `multi_core::fsx::set_multi_home_dir`.
- Trunk-based: commit and push to `main`.
