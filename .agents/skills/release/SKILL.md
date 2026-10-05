---
name: release
description: Cut a new fishpr release (bump the version, tag, push) and confirm it reaches the pacman and apt repos and the Fedora RPM release. Use when the maintainer says "make a release", "release", "ship it", or "cut vX.Y.Z".
---

# Releasing fishpr

Pushing a `v*` tag runs [`release.yml`](../../../.github/workflows/release.yml), which does the rest:

1. It builds in an Arch container.
2. It attaches `fishpr-<ver>-x86_64.tar.gz` to the `v<ver>` GitHub release.
3. It builds `fishpr-bin` from [`packaging/fishpr-bin/PKGBUILD`](../../../packaging/fishpr-bin/PKGBUILD) and signs it with the `GPG_PRIVATE_KEY` secret.
4. It publishes the package to the GitHub release tagged `repo`. That release is the `[fishpr]` pacman repo, so users get the new version on their next `pacman -Syu`.
5. It builds `fishpr_<ver>_amd64.deb` in an Ubuntu 24.04 container with [`packaging/deb/build.sh`](../../../packaging/deb/build.sh), attaches it to the `v<ver>` release, and publishes it to the GitHub release tagged `apt`, the apt repo. Users get it on their next `apt upgrade`.
6. It builds `fishpr-<ver>-1.x86_64.rpm` in a Fedora 42 container from [`packaging/rpm/fishpr.spec`](../../../packaging/rpm/fishpr.spec), signs it, attaches it to the `v<ver>` release, and publishes it as `fishpr.x86_64.rpm` to the GitHub release tagged `rpm`. It isn't a dnf repo: Fedora users get the new version by running `install.sh` again.

Run the whole release without checking in at each step. Stop and ask only in these cases:

- the tree has uncommitted changes. "From the last commit" means they're left out unless the maintainer says to include them.
- `main` is behind `origin/main`.
- there are no commits since the last tag.

## Steps

1. **Version.**
   - Use the version the maintainer gave, if any.
   - Otherwise, bump from the latest `v*` tag: patch for fixes, minor if `git log <last-tag>..HEAD` adds a user-visible feature.
2. **Bump.**
   - Change only `version` in `Cargo.toml`.
   - Run `cargo build --release` so `Cargo.lock` picks up the new version. CI builds with `--locked`.
   - Run `cargo test`.
3. **Ship.**
   - Commit `Cargo.toml` and `Cargo.lock` as `Release vX.Y.Z`.
   - Tag `vX.Y.Z`. The workflow rejects a tag that doesn't match `Cargo.toml`.
   - Run `git push --atomic origin main vX.Y.Z`.
4. **Verify.**
   - Watch the run with `gh run watch <id> --exit-status`. It takes about 15 minutes: the apt and rpm jobs start after the pacman one.
   - Then check that the assets of the `repo` release include `fishpr-bin-X.Y.Z-1-x86_64.pkg.tar.zst` and its `.sig`, and the assets of the `apt` release include `fishpr_X.Y.Z_amd64.deb` and a fresh `InRelease`, and the `rpm` release's `fishpr.x86_64.rpm` has been updated.
5. **Report** the version, `https://github.com/srafis/fishpr/releases/tag/vX.Y.Z`, and a line on what changed.

## If CI fails

Read the log with `gh run view <id> --log-failed`.

- **A transient failure** (network, container): run `gh run rerun <id>`. Uploads use `--clobber`, so a rerun is safe.
- **Only the `apt` or `rpm` job failed:** the pacman repo already has the release. Fix the cause, then run `gh run rerun <id> --failed` if the fix is outside the repo, or release the fix as the next patch.
- **A code fix is needed and "Sign and publish to the pacman repo" never ran:**
  1. Run `gh release delete vX.Y.Z --yes --cleanup-tag` and `git tag -d vX.Y.Z`.
  2. Fix the code.
  3. Tag the same version again.
- **The package was already published:** don't reuse the version. Release the fix as the next patch.

## Don't touch

- the `repo`, `apt` and `rpm` releases' names and URLs, the asset name `fishpr.x86_64.rpm`, or the package names `fishpr-bin` and `fishpr`. Installed systems point at them.
- the app ID `io.github.srafis.fishpr` and the desktop file named after it. Desktops key the user's shortcut and portal permissions to it.
- the signing key. Changing it means every user has to re-run `install.sh`.
- `repo`, `apt` and `rpm` release assets. Only CI writes them.
- `pkgver` and `sha256sums` in the PKGBUILD, which are placeholders CI fills in.
