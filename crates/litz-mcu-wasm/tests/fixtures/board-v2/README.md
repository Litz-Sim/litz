# `litz:mcu@0.2.0` test fixture

The guest here is the board template built once and checked in, so the host's
0.2 host calls are tested against a real component without a wasm toolchain in
the test run. Rebuild it when the template or the contract changes:

```sh
cd build
litz catalog new board v2-fixture  # from the repository's litz binary
cd boards/board-v2-fixture/simulator
./build.sh                         # guest + litz-componentize
cp ../board-v2-fixture.wasm ../../../crates/litz-mcu-wasm/tests/fixtures/board-v2/board-v2.wasm
```

`board-v2.onboard.json` is the generated schematic, renamed with the board so
the whole directory can be loaded as a catalog.
