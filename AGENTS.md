# AGENTS.md

fishpr is push-to-talk dictation for KDE Plasma 6 on Wayland, written in Rust. [README.md](README.md) explains what it does and lists what each file in `src/` does.

## Build and test

```sh
cargo build --release   # needs rust, cmake, clang (whisper.cpp builds from source), and Qt 6 (qt6-base, qt6-declarative)
cargo test
```

To try a build, replace the running copy: `pkill -x fishpr; ./target/release/fishpr`.

Match the surrounding code. There's no `rustfmt.toml`, and the lines are wider than rustfmt's default, so don't run `cargo fmt` over whole files.

## Releases

fishpr is distributed through two signed repos that CI publishes from `v*` tags: `fishpr-bin` in a pacman repo (the `repo` release) and `fishpr` in an apt repo (the `apt` release). To make a release, follow [.agents/skills/release/SKILL.md](.agents/skills/release/SKILL.md). Don't change either release, the package names, the signing key, or the app ID `io.github.srafis.fishpr`: installed systems depend on all of them.
