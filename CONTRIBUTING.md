# Development Guide

## Requirements

[mise](https://mise.jdx.dev/) is the only direct requirement. The project tools are declared in [mise.toml](mise.toml) and locked in `mise.lock` files.

Set up the repository with:

```bash
mise trust
mise install
mise run hooks:install
```

## Principles

- Keep reusable commands and toolchain definitions in mise so local development and CI use the same entry points.
- Run `mise run check` before finishing a change. If it fails, try `mise run fix`, then correct any remaining issues manually.
- Treat Clippy warnings as errors and keep code formatted with rustfmt.
- Use Cargo commands to regenerate `Cargo.lock`; do not edit it manually.

## Common Commands

Run `mise tasks` to see the full task list.

```bash
# Run all read-only checks
mise run check

# Apply safe automatic fixes
mise run fix

# Build the project
mise run build

# Run project tests
mise run test

# Run project-specific lint checks
mise run lint

# Audit project dependencies
mise -E ci run audit
```

## Documentation Site

```bash
# Start the local VitePress development server
mise run docs:dev

# Build the documentation site
mise run docs:build
```

## GitHub Actions Maintenance

```bash
# Update action versions and pin them to commit hashes
mise run actions:update
```

Read [mise.toml](mise.toml) and [mise.ci.toml](mise.ci.toml) for the task definitions.
