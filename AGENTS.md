# AGENTS.md

fishpr is push-to-talk dictation for KDE Plasma 6 on Wayland, written in Rust. It lives in the system tray. Hold Ctrl+Space to record through `pw-record`. When the key is released, a local Silero VAD check runs, the audio goes to ChatGPT's anonymous transcribe endpoint, and the text is pasted with a virtual Ctrl+V through `/dev/uinput`. [README.md](README.md) has the user-facing details and a table of what each file in `src/` does.

## Build and test

```sh
cargo build --release   # needs rust, cmake, clang (whisper.cpp is compiled from source)
cargo test
```

The binary is `target/release/fishpr`. To try a change, stop the running copy and start the new one: `pkill -x fishpr; ./target/release/fishpr`. Users normally run the packaged `/usr/bin/fishpr`, and on first run it downloads the VAD model into `~/.local/share/fishpr/`.

Match the surrounding code: short doc comments that explain *why*, and `anyhow` errors with `.context(...)`. No `rustfmt.toml` exists, and lines run past rustfmt's default width, so don't run `cargo fmt` over whole files.

## Distribution

Users install with `install.sh`. It adds a signed pacman repo called `[fishpr]` and installs the `fishpr-bin` package, and from then on updates come through `pacman -Syu`. The pieces:

- [`.github/workflows/release.yml`](.github/workflows/release.yml) runs when a `v*` tag is pushed. It builds in an Arch container and attaches `fishpr-<ver>-x86_64.tar.gz` to the `v<ver>` GitHub release. Then it builds the package from [`packaging/fishpr-bin/PKGBUILD`](packaging/fishpr-bin/PKGBUILD), signs it with the `GPG_PRIVATE_KEY` secret, and publishes the package and the repo database to the GitHub release tagged `repo`.
- The `repo` release **is** the pacman repo. `install.sh` and every user's `pacman.conf` point at `https://github.com/srafis/fishpr/releases/download/repo`.

Don't change these without the maintainer asking:

- the `repo` release's name or URL, or the package name `fishpr-bin`, because installed systems depend on them
- the signing key. Users trusted it once, and a new key means every user has to re-run `install.sh`.
- assets on the `repo` release. Only CI writes them.

`pkgver` and `sha256sums` in the PKGBUILD are placeholders that CI fills in. Don't edit them by hand.

## Making a release

When the maintainer says "make a release" (or "release", "ship it", "cut vX.Y.Z"), follow these steps without asking for confirmation at each one. Stop and ask only where a step says so.

1. **Check the starting point.**
   - `git status`: you must be on `main`.
   - Run `git fetch origin`. `main` must not be behind `origin/main`. If it is, stop and ask.
   - "From the last commit" means the release contains what's committed. If there are uncommitted changes, ask whether to commit them into this release or leave them out. Never include them silently.
2. **Choose the version.**
   - Find the last release with `git tag --list 'v*' --sort=-v:refname | head -n1`.
   - Use the version the maintainer named, if any.
   - Otherwise, bump the **patch** version (0.1.1 → 0.1.2) for fixes and small changes. Bump the **minor** version (0.1.1 → 0.2.0) if the commits since the last tag (`git log <last-tag>..HEAD --oneline`) add a user-visible feature.
   - If `git log <last-tag>..HEAD` is empty, there is nothing to release. Say so and stop.
3. **Update the version.**
   - `Cargo.toml` `version` is the only version you edit by hand.
   - Then run `cargo build --release` to refresh `Cargo.lock`, because CI builds with `--locked`.
   - Run `cargo test`. If tests fail, stop and report.
4. **Commit, tag, push.**

   ```sh
   git add Cargo.toml Cargo.lock
   git commit -m "Release vX.Y.Z"
   git tag vX.Y.Z
   git push --atomic origin main vX.Y.Z
   ```

   The workflow fails if the tag doesn't match the version in `Cargo.toml`.
5. **Watch CI.**
   - Find the run with `gh run list --workflow release.yml --limit 1`, then run `gh run watch <id> --exit-status`. It takes about 6 minutes.
   - When it succeeds, check that `gh release view repo --json assets --jq '.assets[].name'` lists `fishpr-bin-X.Y.Z-1-x86_64.pkg.tar.zst` and its `.sig`.
6. **Report** the version, the release URL (`https://github.com/srafis/fishpr/releases/tag/vX.Y.Z`), and a one-line summary of what changed. Users get the new version on their next `pacman -Syu`.

**If CI fails:** read the log with `gh run view <id> --log-failed`.

- **If the failure isn't in the code** (a network error, a flaky container): re-run it with `gh run rerun <id>`. Uploads use `--clobber`, so re-running is safe.
- **If the code needs a fix, and the "Sign and publish to the pacman repo" step never ran:**
  1. Delete the broken release and tag with `gh release delete vX.Y.Z --yes --cleanup-tag` and `git tag -d vX.Y.Z`.
  2. Fix and commit.
  3. Tag the same version again and push.
- **If the package was already published:** never re-tag the same version. Release the fix as the next patch version.
