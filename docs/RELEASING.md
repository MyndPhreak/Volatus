# Releasing Volatus

Releases are made by [release-please](https://github.com/googleapis/release-please) and the
workflow `.github/workflows/release-please.yml`. Nobody builds an installer by hand.

## Settings (one time)

In the repository settings, let GitHub Actions create pull requests. Set squash merge with the
pull request title as the default commit message.

## Normal work

1. Work on a branch. Commit messages on the branch are free.
2. Open a pull request. CI runs the tests and builds an MSI.
3. Give the pull request a conventional title: `feat: ...` for a new function, `fix: ...` for a
   correction. `feat!: ...` marks a breaking change. Other types (`docs:`, `ci:`, `chore:`) do not
   start a release on their own.
4. Squash-merge it. release-please then opens or updates the release pull request
   `chore(main): release X.Y.Z`. It holds the new version and the `CHANGELOG.md` text.

## Test the MSI of a pull request

Open the CI run of the pull request (the **Checks** tab). On the run's **Summary** page, under
**Artifacts**, download `volatus-msi` (kept for 7 days). Extract the zip and run the MSI. Windows
asks for administrator rights one time.

## Make a release

1. Open the release pull request. CI does not start on it by itself, because release-please
   uses the default `GITHUB_TOKEN`. Close the pull request and open it again to start CI.
2. When CI is green, merge it. The workflow then:
   - makes a draft release and the tag `vX.Y.Z`;
   - builds the MSI from the tag on Windows, after it checks that the tag is the version in
     `app/Cargo.toml` and that the WiX download has the correct SHA-256;
   - attaches `volatus-X.Y.Z-x86_64.msi` to the release and publishes it.
3. The release is published only with its MSI. If a step fails, the release stays a draft.

The MSI is not signed. When it is downloaded, Windows SmartScreen can show "Windows protected your
PC". Select **More info**, then **Run anyway**.

`Cargo.lock` still has the old version after a release. The next local build updates it: commit
that change in the next pull request.

## If the release build fails

- A network fault: open the failed run and select **Re-run failed jobs**. This keeps the release
  that release-please made. Do not re-run all jobs: release-please does not make the release again,
  so the build does not start.
- A fault in the code or the scripts: fix it in a pull request with a `fix:` title. Its release pull request has
  the next version. When that release is published, delete the old draft and its tag.

## What happens now (after 0.1.0)

Version 0.1.0 is published with the tag `v0.1.0`. `.release-please-manifest.json` says `0.1.0`.
`release-please-config.json` no longer has `release-as` or `bootstrap-sha`, so release-please
counts the commits after the tag `v0.1.0`.

1. Squash-merge pull requests with conventional titles (see Normal work).
2. After each merge to `main`, release-please opens or updates the release pull request. A `fix:`
   gives 0.1.1. A `feat:` gives 0.2.0, because the version is below 1.0 (`bump-minor-pre-major`).
3. Merge the release pull request when you want a release (see Make a release).
4. The workflow makes the tag, builds the MSI and publishes it.

Do not change the version in `app/Cargo.toml` or in the manifest by hand: the release pull request
does it. Do not add `release-as` again, unless you must force one version. Remove it after that
release, or release-please proposes the same version again.
