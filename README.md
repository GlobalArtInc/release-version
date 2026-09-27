# release-version

Take the version from a release tag and put it where the project keeps it — `package.json`,
`Cargo.toml` (with `Cargo.lock`) or any JSON manifest with a top-level `"version"`. Ships as a small
Rust CLI, a composite GitHub Action and a reusable workflow.

The idea: **publishing a GitHub release is the only manual step.** The tag decides the version;
CI builds with it and moves the default branch up to it.

## Reusable workflow — bump the default branch after a release

```yaml
# .github/workflows/release.yml in your project
on:
  release:
    types: [published]

jobs:
  # … build and attach artifacts first …

  bump:
    needs: package
    if: ${{ !github.event.release.prerelease }}
    uses: GlobalArtInc/release-version/.github/workflows/bump-version.yml@v1
    permissions:
      contents: write
    with:
      tag: ${{ github.event.release.tag_name }}
      # strict: true               # browser extensions: plain X.Y.Z only
      # files: |                   # default: package.json, then Cargo.toml
      #   package.json
      #   src-tauri/Cargo.toml
```

It checks out the default branch, sets the version with `--if-newer` (a hotfix tag on an old line
never moves it backwards), commits only the rewritten files as `Bump version to X.Y.Z` and pushes,
rebasing and retrying if the branch moved meanwhile.

| Input | Default | |
| --- | --- | --- |
| `tag` | — | Release tag, `v1.2.3` / `1.2.3` / `refs/tags/v1.2.3` |
| `files` | `''` | Manifests, one per line; empty means `package.json`, then `Cargo.toml` |
| `working-directory` | `.` | Directory the files are relative to |
| `strict` | `false` | Only `X.Y.Z` with parts ≤ 65535, no `-beta` / `+build` |
| `branch` | default branch | Branch to bump |
| `commit-message` | `Bump version to {version}` | `{version}` is substituted |
| secret `token` | `GITHUB_TOKEN` | Pass a token that may push if the branch is protected |

Outputs: `version`, `changed`.

## Action — set the version inside a job

Use it before building so artifacts carry the tag's version even though the tagged commit does not:

```yaml
- uses: actions/checkout@v7
  with:
    ref: ${{ github.event.release.tag_name }}

- id: version
  uses: GlobalArtInc/release-version@v1
  with:
    strict: true

- run: pnpm build   # package.json now says ${{ steps.version.outputs.version }}
```

Inputs are those of the workflow above plus `if-newer` (`false`), `commit` (`false`) and
`commit-message`; `tag` defaults to the published release's tag, then the pushed ref. Outputs:

| Output | |
| --- | --- |
| `version` | Version from the tag, e.g. `1.2.3` |
| `previous` | What the first file held before |
| `changed` | `true` if any file was rewritten |
| `prerelease` | `true` if the tag has a suffix such as `-rc.1` |
| `files` | Every rewritten file (manifests and `Cargo.lock`), one per line |

The action compiles the CLI from its own source at the pinned ref (about half a minute), so the
tool always matches the action version. GitHub-hosted runners ship with Rust; on self-hosted
runners add a toolchain step first.

## CLI

```
cargo install --git https://github.com/GlobalArtInc/release-version --locked

release-version v1.2.3                          # package.json or Cargo.toml in the current dir
release-version v1.2.3 --file a/package.json --file b/Cargo.toml
release-version v1.2.3 --if-newer --strict
```

- Only the version value is rewritten; key order, indentation, comments and line endings stay as they
  were. Nested `"version"` keys (dependencies, engines) are never touched.
- A `Cargo.toml` bump also updates the crate's entry in the nearest `Cargo.lock`, so
  `cargo build --locked` keeps working. `[workspace.package].version` is supported; a member with
  `version.workspace = true` is rejected with a pointer to the workspace root.
- All files are checked before any is written, so a bad file never leaves the others half-updated.
- Exit codes: `0` success, `1` invalid tag or file, `2` usage error.

## Releasing this repository

Publish a release tagged `vX.Y.Z`. `release.yml` then moves the major tag (`v1`) to it — that is
the ref callers pin — and bumps `Cargo.toml` / `Cargo.lock` on `main` using this action.
