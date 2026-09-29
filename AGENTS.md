# AGENTS.md

fishpr is push-to-talk dictation for KDE Plasma 6 on Wayland, written in Rust. [README.md](README.md) explains what it does and lists what each file in `src/` does.

## Build and test

```sh
cargo build --release   # needs rust, cmake, clang (whisper.cpp builds from source)
cargo test
```

To try a build, replace the running copy: `pkill -x fishpr; ./target/release/fishpr`.

Match the surrounding code. There's no `rustfmt.toml`, and the lines are wider than rustfmt's default, so don't run `cargo fmt` over whole files.

## Releases

fishpr is distributed as `fishpr-bin` through a signed pacman repo that CI publishes from `v*` tags. To make a release, follow [.agents/skills/release/SKILL.md](.agents/skills/release/SKILL.md). Don't change the `repo` release, the package name, or the signing key: installed systems depend on all three.
