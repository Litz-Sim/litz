# Contributing

Litz is Apache-2.0 licensed. By opening a pull request you agree to license your contribution under the same terms.

## Layout

| Path | Contents |
|---|---|
| `crates/` | The Rust workspace: engine, CLI, MCP server, chip adapters, board runtime, catalog, analog solver, decoders and the wasm ABI. |
| `vendor/` | Renamed esp32sim forks (`litz-esp32`, `litz-esp32c3`, …) with their local patches. They are path dependencies, not workspace members. |
| `js/` | The `@litz-sim/litz` package: ES module wrapper, type declarations, built wasm and tests. |
| `docs/` | The docsify site served at [docs.litzsim.top](https://docs.litzsim.top/). |
| `examples/` | Complete projects with `diagram.json`, `litz.toml` and firmware. |
| `tools/` | Release and firmware-fetch scripts. |

The dependency direction is one way: chip adapters depend on `litz-board`, while the board depends only on `litz-core`, `litz-diagram` and `litz-catalog`.

## Prerequisites

- Rust stable. The workspace uses edition 2024, so a recent toolchain is required; `rustup toolchain install stable` is enough.
- `wasm32-unknown-unknown` for the npm package: `rustup target add wasm32-unknown-unknown`.
- Node 22 and npm for `js/`. Publishing the package needs npm 11.5.1 or newer, because that is the first npm with OIDC publishing.

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace suite needs no downloaded assets: the fixture firmware and the wasm chip module are in the tree. ROM-backed ESP-IDF tests are `#[ignore]`d and need files the CI job does not fetch.

The examples are the product, so run at least one through the same binary a user installs:

```sh
cargo run -q -p litz-cli -- test examples/blinky
cargo run -q -p litz-cli -- test examples/pico-blink
```

The npm package builds the module from this tree and drives it with real firmware:

```sh
cd js
node scripts/build.mjs
node --test test/*.test.js
npm pack --dry-run
```

## Code

Match the surrounding code rather than a separate style guide. Comments explain why a choice exists; a measured behaviour belongs in a named test instead of a note. Public APIs need docs that say what the thing does, not why it was designed that way.

Add a dependency with `cargo add` and keep it current; do not hand-write a version. If a version has to stay behind because another dependency blocks it, say which dependency and what unlocks it next to the entry.

A crate added under `crates/` joins the workspace automatically. Give it an entry in `[workspace.dependencies]` in the root `Cargo.toml` and have other members ask for it with `{ workspace = true }`; that keeps one release version across the graph.

## Commits

Write the subject in the imperative, in English, and say why in the body when the change is not obvious. Keep a commit to one idea. Do not commit generated files that a build can reproduce.

## Releases

Every crate in the workspace and the npm package share one version. A release is a tag whose name matches that version, and the `publish` workflow refuses the tag when `Cargo.toml`, `js/package.json` and the tag disagree.

1. Move the `[Unreleased]` entries in [CHANGELOG.md](CHANGELOG.md) under the new version and leave an empty `[Unreleased]`.
2. Set the version in one place:

   ```sh
   tools/set-version.sh 0.5.0
   ```

   The script updates `[workspace.package]` and the npm package, and relaxes `[workspace.dependencies]` to the compatible `0.5` range. A patch release (`0.4.1`) keeps the range.
3. Commit and tag:

   ```sh
   git commit -am "Release 0.5.0"
   git tag v0.5.0
   git push origin main v0.5.0
   ```

4. The tag runs `.github/workflows/publish.yml`: it verifies the versions, publishes the crates in dependency order, publishes the npm package, and creates the GitHub release. Both registries use trusted publishing, so no token is stored in the repository.

Trusted publishing is configured once per crate on crates.io, under Settings → Trusted Publishing, with owner `Litz-Sim`, repository `litz` and workflow `publish.yml`. `tools/publish-crates.sh --list` prints the current set of crates to configure. The npm package is configured the same way on npmjs.com.

If a run stops after some crates were published, re-running the workflow is safe: `tools/publish-crates.sh` checks the registry and skips versions that already exist.

## Documentation

The docs site is docsify over the Markdown in `docs/`. Preview it with any static server:

```sh
cd docs
python3 -m http.server 8000
# http://127.0.0.1:8000/#/getting-started
```

Links between pages are relative `.md` files; docsify turns them into hash routes. A link that leaves the docs directory must be absolute, because the site runs on its own origin.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md) for the private channel.
