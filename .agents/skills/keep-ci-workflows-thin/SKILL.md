---
name: keep-ci-workflows-thin
description: Keep CI workflows thin and avoid duplicating CI behavior between mise and workflow YAML. Use whenever writing, modifying, maintaining, or reviewing files under .github/workflows.
---

# Keep CI Workflows Thin

When writing or changing CI, place each piece of behavior in two steps.

## Step 1: Decide whether the behavior is portable

Ask whether the behavior:

- is independent of GitHub Actions and its expression syntax, contexts, outputs, permissions, and APIs;
- can be run and tested locally with explicit inputs;
- is useful enough to reuse or diagnose outside one workflow step.

If all apply, implement the behavior as a task in `mise.ci.toml`.

Examples include calculating a release version, validating a tag, generating
release notes, preparing publication metadata, or uploading coverage through a
CI-platform-independent client when local upload is intentionally supported.

If they do not apply, keep the behavior inline in `.github/workflows`.

Examples include selecting jobs from GitHub event data, writing
`GITHUB_OUTPUT` or `GITHUB_STEP_SUMMARY`, creating a pull request through the
GitHub API, using GitHub environments, and wiring permissions, secrets,
artifacts, or deployments to GitHub Actions.

## Step 2: Implement the behavior in exactly one place

For portable behavior:

1. Check whether an existing mise task already provides it.
2. Reuse an existing shared task; otherwise define one task in `mise.ci.toml`.
3. Pass workflow-specific values as explicit environment variables or task
   arguments.
4. Make the workflow step call only that task:

```yaml
- name: Generate release notes
  run: mise -E ci run release:notes
```

For platform-specific behavior, implement it directly in the workflow and do
not mirror it in `mise.ci.toml`.

Never split one portable behavior between a mise task and surrounding workflow
shell. Move the complete testable unit into the task so changing it requires
editing only one place. If its implementation is too large for readable TOML,
put it under `scripts/` and expose it only through the mise task.

Before finishing, run the task locally with representative inputs, then run
`mise run check`.
