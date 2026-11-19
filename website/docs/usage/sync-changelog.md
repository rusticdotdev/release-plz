# sync-changelog

Copy the changelog edits made in the body of the release Pull Request into the changelog files.

In the body of the release PR, [`release-plz release-pr`](release-pr.md) wraps the changelog of
each package in HTML comments, which are invisible in the rendered PR:

```md
<!-- release-plz:changelog-start my-crate 0.10.0 -->
### Added

- add a rustls config
<!-- release-plz:changelog-end -->
```

`release-plz sync-changelog` reads the body of the release PR. For each pair of comments, it
replaces the notes of that version in the changelog of the package with the text between the
comments, and commits the result to the release PR branch.
The title of the release in the changelog (e.g. `## [0.10.0] - 2026-01-01`) isn't edited.

Like [`release-plz update`](update.md), this command looks for the changelog of each package
at the [`changelog_path`](../config.md#the-changelog_path-field).

Run the command with the release PR branch checked out:

```sh
release-plz sync-changelog --git-token <token> --pr <pr_number>
```

If you omit `--pr`, release-plz uses the opened release PR.

:::warning
When [`release-plz release-pr`](release-pr.md) updates the release PR, e.g. because you pushed a
new commit to the main branch, it generates the changelog and the PR body again,
overwriting your edits.
:::

## GitHub Actions

Run `sync-changelog` when the body of the release PR is edited:

```yaml
name: Sync changelog

on:
  pull_request:
    types: [edited]

permissions:
  contents: write
  pull-requests: read

jobs:
  sync-changelog:
    if: >-
      github.event.changes.body != null &&
      startsWith(github.head_ref, 'release-plz-') &&
      github.event.pull_request.head.repo.full_name == github.repository
    runs-on: ubuntu-latest
    concurrency:
      group: sync-changelog-${{ github.event.pull_request.number }}
      cancel-in-progress: false
    steps:
      - uses: actions/checkout@v6
        with:
          ref: ${{ github.head_ref }}
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@stable
      # Install release-plz here.
      - run: release-plz sync-changelog --pr "${PR_NUMBER}"
        env:
          GIT_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          PR_NUMBER: ${{ github.event.pull_request.number }}
```
