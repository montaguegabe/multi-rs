# Configuration

Multi uses a `multi.json` file in your workspace root to store configuration.

## File Location

```
my-workspace/
├── multi.json          # Configuration file
├── .vscode/            # Merged VS Code config
├── repo-a/             # Sub-repository
├── repo-b/             # Sub-repository
└── ...
```

## Configuration Format

```json
{
  "repos": [
    {
      "url": "https://github.com/org/repo-a",
      "name": "custom-name",
      "description": "Backend API service",
      "installSets": ["default", "dev"],
      "skipVSCode": false
    },
    {
      "url": "https://github.com/org/repo-b"
    }
  ],
  "vscode": {
    "skipSettings": ["workbench.colorCustomizations"]
  }
}
```

## Options

## Mode Selection

Use this table to choose the right mode:

| Scenario | `monoRepo` |
|----------|------------|
| Independent git repositories inside one workspace | `false` |
| One root git repository with project folders (no nested `.git`) | `true` |

If directories listed in `repos` contain their own `.git` folders, that is **not** a monorepo layout for Multi. Use standard mode (`monoRepo: false`).

### allowSymlinks

| Type | Default | Description |
|------|---------|-------------|
| boolean | `false` | Enable symlinking to existing repository clones instead of re-cloning |

When enabled, Multi maintains a global registry at `~/.multi/repos.json` that tracks where repositories have been cloned. If a repository already exists in another workspace, Multi will create a symlink to it instead of cloning fresh.

This is useful when you work with the same repositories across multiple workspaces, as it:
- Saves disk space by avoiding duplicate clones
- Keeps repository state synchronized across workspaces
- Speeds up `multi sync` by avoiding network operations

#### Example: Enable symlinks globally

```json
{
  "allowSymlinks": true,
  "repos": [...]
}
```

---

### monoRepo

| Type | Default | Description |
|------|---------|-------------|
| boolean | `false` | Enable monorepo mode for workspaces where sub-directories are not separate git repos |

When enabled, Multi treats the workspace as a monorepo where:
- The root workspace is the only git repository
- Sub-directories listed in `repos` are regular directories (not separate git repos)
- Git operations (cloning, branch switching) are skipped
- VS Code config syncing and optional agent instruction generation still work normally
- GitHub Actions workflows can be synced into root `.github/workflows` for commit-and-run behavior

This is useful for:
- Traditional monorepos with packages in subdirectories
- Workspaces where you want Multi's VS Code merging without git management

#### Example: Monorepo configuration

```json
{
  "monoRepo": true,
  "repos": [
    { "name": "packages/api", "description": "Backend API" },
    { "name": "packages/web", "description": "Frontend app" },
    { "name": "packages/shared", "description": "Shared utilities" }
  ]
}
```

**Note:** In monorepo mode, `url` is not required - only `name` is needed. The `multi git` and `multi set-branch` commands are disabled; use git directly in the root workspace.
Directories listed in `repos` should not contain nested `.git` folders.

---

### repos

An array of repository configurations. Each repository object supports:

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | string | Yes* | - | Git repository URL (HTTPS or SSH). *Not required in monorepo mode. |
| `name` | string | No** | Last segment of URL | Repository identity/name. **Required in monorepo mode if no URL. |
| `path` | string | No | `name` | Custom on-disk location, relative to the workspace root. Use to place a repo under a nested directory (e.g. `services/api`) instead of a flat top-level folder. Must be a relative path (no leading `/`, no `..`). |
| `description` | string | No | - | Repository description available to generated root agent instructions |
| `skipVSCode` | boolean | No | `false` | Skip this repo when merging VS Code configurations |
| `allowSymlink` | boolean | No | `false` | Allow symlinking to an existing clone of this repo |
| `manageGitignore` | boolean | No | `true` | Manage Multi-generated entries in this repo's `.gitignore` |
| `fixedBranch` | string | No | - | Keep this repo on a fixed branch during branch synchronization and worktree creation |
| `installSets` | string[] | No | included in all sets | Named sync/install sets that include this repo when `multi sync --install-set` is used |

#### Example: Basic repository list

```json
{
  "repos": [
    { "url": "https://github.com/org/backend" },
    { "url": "https://github.com/org/frontend" },
    { "url": "git@github.com:org/private-repo.git" }
  ]
}
```

#### Example: Custom directory names

```json
{
  "repos": [
    {
      "url": "https://github.com/org/t-ide-cli",
      "name": "cli"
    },
    {
      "url": "https://github.com/org/t-ide-extension",
      "name": "extension"
    }
  ]
}
```

Recommended pattern: if the GitHub repository is product-prefixed, use a short local `name` that reflects the role of the repo rather than repeating the full slug.

#### Example: Custom nested paths

Use `path` to lay repos out in nested directories instead of a flat top level.
`name` stays the repo's identity while `path` controls where it is cloned. Multi
anchors the generated `.gitignore`/`.ignore` entries to the exact path (e.g.
`/services/api/`), so a nested repo never accidentally matches a same-named
directory elsewhere in the workspace.

```json
{
  "repos": [
    {
      "url": "https://github.com/org/api",
      "path": "services/api"
    },
    {
      "url": "https://github.com/org/web",
      "path": "services/web"
    }
  ]
}
```

#### Example: Public installer and private dev sets

Use `installSets` when an installer should sync only a public/runtime subset of the workspace while development syncs can still include every repo. Running `multi sync` with no set still uses all repos.

```json
{
  "repos": [
    {
      "url": "https://github.com/org/product-cli",
      "name": "cli",
      "installSets": ["default", "dev"]
    },
    {
      "url": "https://github.com/org/product-console",
      "name": "console",
      "installSets": ["default", "dev"]
    },
    {
      "url": "git@github.com:org/product-ios.git",
      "name": "ios",
      "installSets": ["dev"]
    },
    {
      "url": "https://github.com/org/product-shared",
      "name": "shared"
    }
  ]
}
```

```bash
# Public/runtime installer sync
multi sync --install-set default

# Equivalent shorter form
multi sync --set default

# Full workspace sync, preserving existing behavior
multi sync
```

Repos without `installSets` are included in every named set for backward compatibility. In the example above, `shared` is included by both `--install-set default` and `--install-set dev`.

#### Example: Repository descriptions for AI context

Use `description` with `agentInstructions.enabled` when you want generated root agent instructions to include each repo's role in the workspace.

```json
{
  "repos": [
    {
      "url": "https://github.com/org/backend",
      "description": "REST API and background jobs"
    },
    {
      "url": "https://github.com/org/frontend",
      "description": "Customer-facing web app"
    }
  ]
}
```

### Agent Instructions (`agentInstructions`)

Agent instruction generation is disabled by default. Enable it when you want Multi to generate `AGENTS.md` and `CLAUDE.md` from tracked Markdown source files.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `false` | Generate agent instruction outputs during sync |
| `partsDir` | string | `AGENTS.parts` | Directory containing ordered `*.md` source parts |
| `includeRepoDescriptions` | boolean | `true` | Include repo descriptions in the root generated outputs |

```json
{
  "agentInstructions": {
    "enabled": true,
    "partsDir": "AGENTS.parts",
    "includeRepoDescriptions": true
  }
}
```

Use the configured parts directory for tracked Claude Code and Codex hook instructions that should appear in generated agent files. Root and subrepo parts are generated independently: root `AGENTS.parts/*.md` becomes root `AGENTS.md`/`CLAUDE.md`, while each subrepo's own parts become that subrepo's outputs. Multi does not manage executable tool runtime hook config; keep that config in the owning repo and document it in the relevant parts file.

#### Example: Skip VS Code config for a repo

```json
{
  "repos": [
    { "url": "https://github.com/org/main-app" },
    {
      "url": "https://github.com/org/legacy-tool",
      "skipVSCode": true
    }
  ]
}
```

#### Example: Enable symlink for a specific repo

Use this when a repository should be symlinked to an existing clone (saves disk space and keeps state synchronized):

```json
{
  "repos": [
    { "url": "https://github.com/org/shared-lib" },
    {
      "url": "https://github.com/org/reuse-existing-clone",
      "allowSymlink": true
    }
  ]
}
```

#### Example: Disable `.gitignore` management for a specific repo

Use this when a repository should not have Multi add or retain generated-file entries in its own `.gitignore`:

```json
{
  "repos": [
    {
      "url": "https://github.com/org/managed-repo"
    },
    {
      "url": "https://github.com/org/manual-gitignore-repo",
      "manageGitignore": false
    }
  ]
}
```

---

#### Example: Keep a repo on a fixed branch

Use `fixedBranch` for repositories that should remain on a shared branch while the rest of the workspace moves to a feature branch:

```json
{
  "repos": [
    { "url": "https://github.com/org/app" },
    {
      "url": "https://github.com/org/shared-fixtures",
      "fixedBranch": "main"
    }
  ]
}
```

Multi's branch invariant is centralized in one helper (`expected_branch_for_repo` in multi-core): a repo with `fixedBranch` is expected to use that branch, and every other repo follows the root workspace branch. Branch-aware commands should consume that helper instead of implementing their own fixed-branch checks.

---

### hooks

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `hooks.path` | string | - | Workspace-relative directory of git hook scripts that every repo in the workspace runs |

When set, `multi sync`, `multi init` and `multi worktree add` point `core.hooksPath` of the root repo and every sub-repo at this directory. Check it with `multi hooks status`. See [`hooks`](commands/hooks.md).

```json
{
  "hooks": { "path": ".githooks" }
}
```

### worktree

Configuration for `multi worktree add`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `symlink` | string[] | `[]` | Gitignored workspace-relative paths to symlink from the original workspace into the new worktree |
| `copy` | string[] | `[]` | Gitignored workspace-relative paths to copy from the original workspace into the new worktree |

All paths must be relative to the workspace root, must stay inside the workspace, and must be ignored by the owning Git repository. Existing destinations in the new worktree are left unchanged.

#### Example: Transfer local worktree files

```json
{
  "worktree": {
    "symlink": [".env", ".venv"],
    "copy": [".cursor/local.json", "api/.env.local"]
  },
  "repos": [
    { "url": "https://github.com/org/api", "name": "api" }
  ]
}
```

---

### vscode

VS Code-specific configuration options.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `skipSettings` | string[] | `["workbench.colorCustomizations"]` | Settings keys to exclude when merging settings.json |

#### Example: Skip additional settings

```json
{
  "repos": [...],
  "vscode": {
    "skipSettings": [
      "workbench.colorCustomizations",
      "editor.fontSize",
      "terminal.integrated.fontSize"
    ]
  }
}
```

This is useful when sub-repos have user-specific settings that shouldn't be merged into the root configuration.

## Shared VS Code Files

Multi supports "shared" VS Code configuration files that are merged into the final output. These files are useful for workspace-level settings that apply across all repos.

### .vscode/settings.shared.json

Settings that should be applied to all repos. Contents are merged into the final `settings.json`.

### .vscode/launch.shared.json

Launch configurations and compounds that span multiple repos. This is the recommended way to create debug compounds that launch services from different sub-repos together:

```json
{
  "compounds": [
    {
      "name": "Full Stack",
      "configurations": [
        "api: Django Server",
        "web: Next.js Dev"
      ]
    }
  ]
}
```

The shared launch file is merged after all sub-repo configurations are collected, so you can reference any configuration by its prefixed name (e.g., `"repo-name: Config Name"`).

### .vscode/tasks.shared.json

Tasks that span multiple repos or reference tasks from different sub-repos:

```json
{
  "tasks": [
    {
      "label": "Build All",
      "dependsOn": ["api: Build", "web: Build"],
      "dependsOrder": "parallel",
      "problemMatcher": []
    }
  ]
}
```

The shared tasks file is merged after all sub-repo tasks are collected, so you can reference any task by its prefixed label.

## Full Example

```json
{
  "allowSymlinks": false,
  "repos": [
    {
      "url": "https://github.com/myorg/api-server",
      "name": "api"
    },
    {
      "url": "https://github.com/myorg/web-client",
      "name": "web"
    },
    {
      "url": "https://github.com/myorg/mobile-app",
      "name": "mobile",
      "skipVSCode": true
    },
    {
      "url": "git@github.com:myorg/shared-utils.git",
      "name": "shared",
      "allowSymlink": true
    }
  ],
  "vscode": {
    "skipSettings": [
      "workbench.colorCustomizations",
      "editor.fontFamily"
    ]
  }
}
```

## Notes

- The `multi.json` file is created automatically by `multi init`
- You can manually edit this file to add or remove repositories
- After editing, run `multi sync` to apply changes
- Repository URLs can be HTTPS or SSH format
- The global registry is stored at `~/.multi/repos.json` and tracks all known repository locations
