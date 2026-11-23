#!/bin/sh
# Rebuild parts/ym2610.wasm: verilate JT10, compile it with the WIT glue for
# wasm32-wasip1, then wrap the core module in the litz:part component type.
#
# The example is GPL-3.0 because third_party/jt12 is GPL-3.0; the rest of the
# repository remains Apache-2.0.
#
# Needs verilator and a wasi-sdk. Point WASI_SDK at the unpacked SDK:
#   WASI_SDK=/path/to/wasi-sdk-34.0 ./build.sh
set -eu
cd "$(dirname "$0")"

root=../../..
wasi=${WASI_SDK:-$root/build/toolchains/wasi-sdk-34.0-arm64-macos}
if [ ! -x "$wasi/bin/clang++" ]; then
    echo "set WASI_SDK to an unpacked wasi-sdk (bin/clang++ missing)" >&2
    exit 1
fi
verilator_root=$(verilator --getenv VERILATOR_ROOT)

rm -rf build
mkdir -p build/bindings

wit-bindgen c "$root/crates/litz-part-wit/wit" --out-dir build/bindings --world component \
    >/dev/null

# Verilator resolves a `-f` entry against its own idea of the working
# directory, not the list file's; absolute paths keep the build independent of
# where it is started.
sed "s|^|$PWD/|" files.f > build/files.f
verilator --cc --top-module jt10 -f build/files.f \
    -I../third_party/jt12/hdl -I../third_party/jt12/hdl/adpcm \
    -I../third_party/jt12/jt49/hdl \
    --Mdir build/model --prefix Vjt10 -O2 -Wno-fatal >/dev/null

cxx="$wasi/bin/clang++"
cc="$wasi/bin/clang"
# Section-per-function so the linker can drop the parts of Verilator's runtime
# that print, read VCD files or parse command lines; those pull in WASI file
# and clock imports that a part is not allowed to have.
flags="--target=wasm32-wasip1 -O2 -fno-exceptions -fno-threadsafe-statics -ffunction-sections -fdata-sections"

model_objects=
for src in build/model/Vjt10*.cpp; do
    obj="build/$(basename "$src" .cpp).o"
    $cxx $flags -Ibuild/model -I"$verilator_root/include" \
        -I"$verilator_root/include/vltstd" -c "$src" -o "$obj"
    model_objects="$model_objects $obj"
done
# VL_USER_FATAL and VL_PRINTF replace the runtime's exit() and stdio paths,
# which runtime_stubs.cpp supplies and a wasm link must not pull in. NDEBUG
# drops the runtime's internal assert(), whose report path drags in printf and
# with it every stdio syscall.
$cxx $flags -I"$verilator_root/include" -I"$verilator_root/include/vltstd" \
    -DVL_USER_FATAL '-DVL_PRINTF(...)=((void)0)' -DNDEBUG \
    -c "$verilator_root/include/verilated.cpp" -o build/verilated.o
$cxx $flags -I"$verilator_root/include" -I"$verilator_root/include/vltstd" \
    -DVL_USER_FATAL -c runtime_stubs.cpp -o build/runtime_stubs.o
$cxx $flags -Ibuild/model -I"$verilator_root/include" \
    -c model.cpp -o build/shim.o
$cc $flags -Ibuild/bindings -c glue.c -o build/glue.o
$cc $flags -Ibuild/bindings -c build/bindings/component.c -o build/component.o

# A reactor, not a command: the host calls the exports many times and none of
# them is the end of the process, so the module must not run its destructors
# (among them stdio's flush) after every call.
$cxx $flags -mexec-model=reactor -Wl,--export-memory -Wl,--gc-sections \
    -Wl,--strip-debug -Wl,--initial-memory=4194304 \
    $model_objects build/verilated.o build/runtime_stubs.o build/shim.o build/glue.o build/component.o \
    build/bindings/component_component_type.o \
    -o build/ym2610.core.wasm

cargo run -q --manifest-path "$root/Cargo.toml" -p litz-mcu-wasm \
    --features tooling --bin litz-componentize -- \
    build/ym2610.core.wasm ../parts/ym2610.wasm
