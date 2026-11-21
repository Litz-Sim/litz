//! Wrap a core WebAssembly module in a component, using the WIT contract.
//!
//! `cargo build --target wasm32-unknown-unknown` produces a core module: it
//! has the right exports, but no component type information, so a host that
//! speaks the component model cannot link it. `wit-component` reads the
//! `component-type:<world>` custom section that `wit-bindgen` embeds and
//! produces the component.
//!
//! This exists so the repository does not need `cargo-component` installed:
//! the componentizer is a crate in the workspace, and the build is two `cargo`
//! commands with no third-party binary in between.
//!
//! ```text
//! litz-componentize <core.wasm> <out.wasm>
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [input, output] => match run(PathBuf::from(input), PathBuf::from(output)) {
            Ok(size) => {
                eprintln!("litz-componentize: wrote {size} bytes to {}", args[1]);
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("litz-componentize: {message}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("usage: litz-componentize <core.wasm> <component.wasm>");
            ExitCode::from(2)
        }
    }
}

fn run(input: PathBuf, output: PathBuf) -> Result<usize, String> {
    let bytes = std::fs::read(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    // The core module carries the world it was built against, so the encoder
    // needs no arguments beyond the module itself.
    let component = wit_component::ComponentEncoder::default()
        .module(&bytes)
        .map_err(|e| format!("{}: {e}", input.display()))?
        .validate(true)
        .encode()
        .map_err(|e| format!("{}: {e}", input.display()))?;
    std::fs::write(&output, &component).map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(component.len())
}
