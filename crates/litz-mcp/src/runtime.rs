//! The one part of the server that boots firmware.
//!
//! The MCP crate must not depend on `litz-cli` (which depends on this crate to
//! provide the `litz mcp` subcommand), and the runner must not be copied here.
//! So the server declares the two operations it needs and the CLI installs an
//! implementation that calls the existing runner. Everything else — checking a
//! diagram, editing a circuit, generating code, importing a diagram — is done here
//! against the lower-level crates.

use litz_core::PartState;
use std::path::Path;
use std::sync::OnceLock;

/// One `[[test]]` entry, or a scenario, after the runner decided pass or fail.
#[derive(Debug, Clone)]
pub struct TestEntry {
    pub name: String,
    pub passed: bool,
    pub exit_code: i32,
    pub output: String,
    pub sim_ms: u32,
}

/// The result of running a project's tests or a scenario.
#[derive(Debug, Clone)]
pub struct TestReport {
    pub exit_code: i32,
    pub output: String,
    pub sim_ms: u32,
    pub tests: Vec<TestEntry>,
}

/// The result of a bounded run whose serial output was captured, not streamed.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub exit_code: i32,
    pub output: String,
    pub sim_ms: u32,
    /// Part states read back after the run, in diagram order.
    pub part_states: Vec<(String, PartState)>,
    /// Electrical faults the board found, if any.
    pub faults: Vec<String>,
}

/// What the server needs from a simulator.
pub trait Runtime: Send + Sync {
    /// Run the `[[test]]` entries, or a scenario file, through the runner.
    fn test(
        &self,
        root: &Path,
        scenario: Option<&Path>,
        strict: bool,
    ) -> Result<TestReport, String>;

    /// Boot the firmware for a bounded amount of simulated time and capture
    /// the console instead of writing it to the process's stdout.
    fn run(&self, root: &Path, timeout: Option<&str>, strict: bool) -> Result<RunReport, String>;
}

static RUNTIME: OnceLock<Box<dyn Runtime>> = OnceLock::new();

/// Give the server the simulator it should use. `litz mcp` calls this once
/// before serving; a later call is ignored.
pub fn install_runtime(runtime: Box<dyn Runtime>) {
    let _ = RUNTIME.set(runtime);
}

/// The installed runtime, or an error naming how to get one.
pub(crate) fn runtime() -> Result<&'static dyn Runtime, String> {
    RUNTIME
        .get()
        .map(|runtime| runtime.as_ref())
        .ok_or_else(|| {
            "no simulation runtime is installed; start the server with `litz mcp`".to_string()
        })
}
