# multi-rs

`multi` is a better way to work with VS Code/Cursor on multiple Git repos at once. It is an alternative to [multi-root workspaces](https://code.visualstudio.com/docs/editing/workspaces/multi-root-workspaces): sub-repos are simply cloned into a root workspace folder (no submodules), and `multi` keeps their branches aligned and merges their IDE configuration into the root.

This repository is the Rust implementation of multi. It is a successor to the Python `multi-workspace` package and keeps the same `multi.json` format, command surface and generated-file layout, so existing workspaces keep working. The Multi desktop app lives in [multi-rs-desktop](https://github.com/montaguegabe/multi-rs-desktop) and links `multi-core` directly.

Features:

- Generates files in your root `.vscode` folder from sub-repo `launch.json`, `tasks.json`, `settings.json` and `extensions.json` files.
- Supports optional `installSets` so installer scripts can sync only a public/runtime subset of repos.
- Optionally generates `CLAUDE.md` and `AGENTS.md` files from tracked `AGENTS.parts/*.md` files.
- In monorepo mode, syncs sub-repo GitHub workflows into root `.github/workflows`.
- Workspace-level git hooks, isolated workspace worktrees, cross-repo git commands, branch alignment checks and diagnostics.
- A loopback-only JSON service and a library API (`multi-core`) used by Multi Desktop for workspace diffs, branch visibility and status.

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/multi-core` | Library: workspace model, sync, git helpers, project inspection API |
| `crates/multi-cli` | The `multi` binary (clap) |

## Installation

```bash
cargo install --git https://github.com/montaguegabe/multi-rs multi-cli
```

Or build from a checkout:

```bash
cargo build --release
# binary at target/release/multi
```

`git` must be on your `PATH`. `gh` is needed only for `multi init --github-repo` and `multi collaborator`.

## Getting started

Create a new workspace directory and run:

```bash
multi init
```

When prompted, paste the URLs of the repositories you want in your workspace, optionally with descriptions. Non-interactively:

```bash
multi init \
  --repo https://github.com/org/backend \
  --repo-description "Backend API" \
  --repo https://github.com/org/frontend \
  --repo-description "Frontend app"
```

To create the GitHub repositories first (uses `gh repo create`, private by default):

```bash
multi init --github-repo org/backend --github-description "Backend API"
```

Product-prefixed slugs are shortened to local folder names when they match the workspace directory name: in a `t-ide/` workspace, `t-ide-cli` becomes local folder `cli`. The same rule applies to `multi add`.

## Commands

| Command | Description |
| --- | --- |
| `multi init` | Initialize a new workspace (interactive or with `--repo` / `--github-repo`) |
| `multi add URL [--name NAME]` | Add a repository and sync |
| `multi remove NAME [--delete] [--force]` | Remove a repository from `multi.json` |
| `multi sync [--install-set NAME]` | Clone missing repos, install hooks, merge `.vscode`, generate agent files, sync monorepo workflows |
| `multi sync vscode [settings\|launch\|tasks\|extensions\|devcontainer]` | Merge only VS Code configuration |
| `multi sync agents` | Generate `AGENTS.md` / `CLAUDE.md` from `AGENTS.parts/*.md` (opt-in) |
| `multi sync github` | Sync root GitHub Actions workflows (monorepo mode) |
| `multi branch [check]` | Report branch alignment, read-only, works with dirty trees; exits 1 on mismatch |
| `multi set-branch BRANCH` | Create/switch every repo to `BRANCH` (fixed-branch repos stay put); requires clean trees |
| `multi git ARGS...` | Run a git command in the root and every cloned sub-repo |
| `multi worktree add NAME [--branch B] [--base-ref REF] [--install-set NAME]` | Create an isolated worktree of the whole workspace under the sibling `<workspace>-worktrees/` directory |
| `multi hooks install\|status [--json]\|uninstall` | Workspace-level git hooks (`hooks.path` in `multi.json`) |
| `multi skills install [--dev] [--source PATH] [--skill NAME]...` | Install the Multi agent skills: copy the published skills with `npx skills add`, or with `--dev` symlink a local `multi-skills` checkout by running its `scripts/install-dev.sh` |
| `multi doctor [--strict] [--json] [--fix] [--no-remote-check]` | Diagnose workspace configuration issues |
| `multi collaborator add\|accept\|remove\|recent-users` | Manage GitHub collaborators across every workspace repo via `gh` |
| `multi convert-monorepo --confirm` | Convert a standard workspace to monorepo mode (destructive) |
| `multi open PATH` | Open a project in Multi Desktop (`multi://open?path=...`) |
| `multi service [--host 127.0.0.1] [--port 0]` | Run the loopback JSON service used by local tools |

Every command accepts `--verbose`. After most commands `multi` checks that all cloned repos are on their expected branches and prints a warning when they are not.

Configuration (`multi.json`) is documented in [docs/configuration.md](docs/configuration.md).

## Install sets

```json
{
  "repos": [
    { "name": "cli", "url": "https://github.com/org/product-cli", "installSets": ["default", "dev"] },
    { "name": "ios", "url": "git@github.com:org/product-ios.git", "installSets": ["dev"] }
  ]
}
```

```bash
multi sync --install-set default   # or: multi sync --set default
```

Running `multi sync` without `--install-set` still includes every repo. Repos without `installSets` are included in every named set.

## Per-user state

`multi` keeps a small amount of state under `~/.multi` (override with `MULTI_HOME`): the clone registry used by `allowSymlinks`, recent GitHub collaborator usernames, and the desktop app's project list and settings.

## Development

```bash
cargo test          # unit + integration tests (uses temporary git repos; no network)
cargo clippy --all-targets
cargo fmt --all
```

Integration tests redirect `~/.multi` to a scratch directory, so they never touch your real registry.

## Differences from the Python implementation

- Single static binary; no Python runtime.
- `multi doctor` gains `--no-remote-check` to skip `git ls-remote` reachability checks.
- `multi --version` reports the crate version.
- Generated YAML for monorepo workflows uses YAML 1.2 scalars (`on:` stays a string).
- The desktop app links `multi-core` directly instead of spawning `multi service`; the service remains available for other local tooling.
