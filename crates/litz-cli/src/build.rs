//! `litz build`: a project's own build recipe, in TOML.
//!
//! Litz does not ship a compiler. A board's toolchain changes faster than a
//! simulator should, and pinning Arduino cores or ESP-IDF inside this binary
//! would make every project's build depend on our release schedule. Instead a
//! project writes `litz.build.toml`, the same way it writes `litz.toml`: a
//! short, ordered list of shell steps, each with its own directory, environment
//! and timeout. `litz build` runs them; `litz test --build` runs them first.
//!
//! The recipe is deliberately close to a CI job, because that is where most
//! firmware is built. It is not a sandbox: a step runs with the user's
//! permissions, exactly like `make` or a GitHub Actions `run:` line, and the
//! only path rule is that a step's working directory must stay inside the
//! project.

use crate::config::parse_duration;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The file a project writes.
pub const FILE: &str = "litz.build.toml";

#[derive(Debug, Default, Deserialize)]
pub struct BuildConfig {
    #[serde(default)]
    pub build: BuildSection,
    /// One entry per `[[step]]`.
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

#[derive(Debug, Default, Deserialize)]
pub struct BuildSection {
    /// Directory every step runs in, relative to the project.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Environment for every step. A step's own `env` wins on a collision.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// A file that must exist when the last step succeeds.
    #[serde(default)]
    pub firmware: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    /// A label for the transcript; defaults to the command.
    #[serde(default)]
    pub name: Option<String>,
    /// The command, run by the platform shell.
    pub run: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Wall-clock limit for this step, for example `5m`. A step without one
    /// is limited by the caller's own patience, as `make` is.
    #[serde(default)]
    pub timeout: Option<String>,
}

impl BuildConfig {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join(FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: BuildConfig =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.steps.is_empty() {
            return Err(format!("{FILE}: at least one [[step]] is required"));
        }
        for (index, step) in self.steps.iter().enumerate() {
            if step.run.trim().is_empty() {
                return Err(format!("{FILE}: step {} has an empty `run`", index + 1));
            }
            if let Some(timeout) = &step.timeout {
                parse_duration(timeout)
                    .map_err(|e| format!("{FILE}: step {} timeout: {e}", index + 1))?;
            }
        }
        Ok(())
    }
}

/// Run a project's recipe. Returns a transcript; an error means a step failed
/// or the recipe could not be loaded.
pub fn run(dir: &Path, dry_run: bool) -> Result<String, String> {
    let mut output = String::new();
    run_with(dir, dry_run, &mut |chunk| output.push_str(chunk))?;
    Ok(output)
}

/// Run a project's recipe, handing each piece of transcript to `emit` as it
/// arrives. `run` is this function with a collector, and the editor link uses
/// it to stream a build's output instead of waiting for the last step.
pub fn run_with(dir: &Path, dry_run: bool, emit: &mut dyn FnMut(&str)) -> Result<(), String> {
    let config = BuildConfig::load(dir)?;
    let root = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    for (index, step) in config.steps.iter().enumerate() {
        let label = step.name.clone().unwrap_or_else(|| step.run.clone());
        emit(&format!(
            "[build {}/{}] {label}\n",
            index + 1,
            config.steps.len()
        ));
        if dry_run {
            emit(&format!("  $ {}\n", step.run));
            continue;
        }
        let workdir = resolve_workdir(
            &root,
            config.build.workdir.as_deref(),
            step.workdir.as_deref(),
        )?;
        let mut env: BTreeMap<&str, &str> = BTreeMap::new();
        for (key, value) in &config.build.env {
            env.insert(key, value);
        }
        for (key, value) in &step.env {
            env.insert(key, value);
        }
        let timeout = step
            .timeout
            .as_deref()
            .map(parse_duration)
            .transpose()?
            .map(Duration::from_nanos);
        execute(&step.run, &workdir, &env, timeout, emit)
            .map_err(|e| format!("step {} ({label}): {e}", index + 1))?;
    }
    if let Some(firmware) = &config.build.firmware {
        let path = root.join(firmware);
        if !dry_run && !path.exists() {
            return Err(format!(
                "[build] firmware = {firmware:?} does not exist after the last step"
            ));
        }
    }
    Ok(())
}

/// Where a project's built image lives, and which board it belongs to.
///
/// `litz build` does not need a diagram, but a pushed image does need a name
/// the editor can show. `[build] firmware` is the recipe's own output and wins
/// for a project with one board; a `[[board]]` section names each board's
/// image when there are several.
pub fn image_for(dir: &Path, board: Option<&str>) -> Result<(Option<String>, PathBuf), String> {
    let recipe = BuildConfig::load(dir).ok();
    let project = litz_engine::config::Config::load_project(dir).ok();
    let board_image = |id: &str| {
        project.as_ref().and_then(|config| {
            config
                .boards
                .iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| {
                    entry
                        .firmware
                        .clone()
                        .map(|path| (Some(entry.id.clone()), path))
                })
        })
    };
    let single_board = || {
        project.as_ref().and_then(|config| {
            let [entry] = &config.boards[..] else {
                return None;
            };
            entry
                .firmware
                .clone()
                .map(|path| (Some(entry.id.clone()), path))
        })
    };
    let built = || {
        recipe
            .as_ref()
            .and_then(|recipe| recipe.build.firmware.clone())
    };
    let configured = || {
        project
            .as_ref()
            .and_then(|config| config.sim.firmware.clone())
    };

    let mut candidates: Vec<(Option<String>, String)> = Vec::new();
    match board {
        Some(id) => {
            if let Some(hit) = board_image(id) {
                candidates.push(hit);
            }
            if let Some(path) = built() {
                candidates.push((Some(id.to_string()), path));
            }
            if let Some(path) = configured() {
                candidates.push((Some(id.to_string()), path));
            }
        }
        None => {
            if let Some(path) = built() {
                candidates.push((None, path));
            }
            if let Some(path) = configured() {
                candidates.push((None, path));
            }
            if let Some(hit) = single_board() {
                candidates.push(hit);
            }
        }
    }
    for (name, path) in candidates {
        let path = dir.join(path);
        if path.is_file() {
            return Ok((name, path));
        }
    }
    let available: Vec<String> = project
        .map(|config| config.boards.into_iter().map(|entry| entry.id).collect())
        .unwrap_or_default();
    Err(match board {
        Some(id) if !available.is_empty() => format!(
            "no firmware for board {id:?}; this project names {}",
            available.join(", ")
        ),
        Some(id) => format!("no firmware for board {id:?}; no image was built"),
        None => "no built image to push: set [build] firmware or [sim] firmware".to_string(),
    })
}

/// A step's working directory, refused when it leaves the project.
fn resolve_workdir(
    root: &Path,
    default: Option<&str>,
    step: Option<&str>,
) -> Result<PathBuf, String> {
    let relative = step.or(default).unwrap_or(".");
    if relative.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("build workdir {relative:?} escapes the project"));
    }
    let path = root.join(relative);
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.starts_with(root) {
        return Err(format!("build workdir {relative:?} escapes the project"));
    }
    Ok(path)
}

/// Run one command through the platform shell, handing its output to `emit`
/// line by line as it arrives.
///
/// Both pipes are drained on their own threads: a step that writes more than a
/// pipe buffer would otherwise block with no reader, and a build that only
/// reports at exit cannot be watched in the editor.
fn execute(
    command: &str,
    workdir: &Path,
    env: &BTreeMap<&str, &str>,
    timeout: Option<Duration>,
    emit: &mut dyn FnMut(&str),
) -> Result<(), String> {
    #[cfg(windows)]
    let mut child = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    };
    #[cfg(not(windows))]
    let mut child = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    child
        .current_dir(workdir)
        .envs(env.iter().map(|(key, value)| (*key, *value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child
        .spawn()
        .map_err(|e| format!("cannot start {command:?}: {e}"))?;

    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut pipes: Vec<Box<dyn Read + Send>> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pipes.push(Box::new(stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        pipes.push(Box::new(stderr));
    }
    let mut readers = Vec::new();
    for mut pipe in pipes {
        let sender = sender.clone();
        readers.push(std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        }));
    }
    drop(sender);

    let started = Instant::now();
    let mut pending: Vec<u8> = Vec::new();
    let status = loop {
        while let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(10)) {
            pending.extend_from_slice(&chunk);
            while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=index).collect();
                emit(&String::from_utf8_lossy(&line));
            }
        }
        match child.try_wait() {
            Ok(Some(finished)) => break finished,
            Ok(None) => {}
            Err(e) => return Err(format!("waiting for {command:?}: {e}")),
        }
        if let Some(limit) = timeout
            && started.elapsed() >= limit
        {
            let _ = child.kill();
            let _ = child.wait();
            for reader in readers {
                let _ = reader.join();
            }
            return Err(format!(
                "{command:?} did not finish within {} ms",
                limit.as_millis()
            ));
        }
    };
    for reader in readers {
        let _ = reader.join();
    }
    while let Ok(chunk) = receiver.try_recv() {
        pending.extend_from_slice(&chunk);
    }
    if !pending.is_empty() {
        let mut line = String::from_utf8_lossy(&pending).into_owned();
        line.push('\n');
        emit(&line);
    }

    if !status.success() {
        let code = status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "a signal".to_string());
        return Err(format!("{command:?} exited with {code}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("litz-build-{}-{n}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_recipe_runs_every_step_in_order() {
        let dir = scratch("ok");
        std::fs::write(
            dir.join(FILE),
            r#"
[build]
firmware = "out/app.bin"

[[step]]
name = "first"
run = "mkdir -p out && printf one > out/app.bin"

[[step]]
name = "second"
run = "printf two >> out/app.bin"
"#,
        )
        .unwrap();
        let text = run(&dir, false).unwrap();
        assert!(text.contains("[build 1/2] first"), "{text}");
        assert!(text.contains("[build 2/2] second"), "{text}");
        assert_eq!(std::fs::read(dir.join("out/app.bin")).unwrap(), b"onetwo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_step_stops_the_recipe() {
        let dir = scratch("fail");
        std::fs::write(
            dir.join(FILE),
            r#"
[[step]]
run = "printf before"

[[step]]
name = "explode"
run = "exit 7"

[[step]]
run = "printf after"
"#,
        )
        .unwrap();
        let error = run(&dir, false).unwrap_err();
        assert!(error.contains("explode"), "{error}");
        assert!(error.contains("exited with 7"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_step_workdir_may_not_leave_the_project() {
        let dir = scratch("escape");
        std::fs::write(
            dir.join(FILE),
            "[[step]]\nrun = \"true\"\nworkdir = \"../\"\n",
        )
        .unwrap();
        let error = run(&dir, false).unwrap_err();
        assert!(error.contains("escapes the project"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_firmware_output_is_an_error() {
        let dir = scratch("missing");
        std::fs::write(
            dir.join(FILE),
            "[build]\nfirmware = \"out/app.bin\"\n\n[[step]]\nrun = \"true\"\n",
        )
        .unwrap();
        let error = run(&dir, false).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dry_run_runs_nothing() {
        let dir = scratch("dry");
        std::fs::write(
            dir.join(FILE),
            "[[step]]\nrun = \"touch should-not-exist\"\n",
        )
        .unwrap();
        let text = run(&dir, true).unwrap();
        assert!(text.contains("$ touch should-not-exist"), "{text}");
        assert!(!dir.join("should-not-exist").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn step_environment_reaches_the_command() {
        let dir = scratch("env");
        std::fs::write(
            dir.join(FILE),
            r#"
[build.env]
GLOBAL = "one"

[[step]]
run = "printf \"$GLOBAL-$LOCAL\" > out.txt"
env = { LOCAL = "two" }
"#,
        )
        .unwrap();
        run(&dir, false).unwrap();
        assert_eq!(std::fs::read(dir.join("out.txt")).unwrap(), b"one-two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_recipe_is_refused() {
        let dir = scratch("empty");
        std::fs::write(dir.join(FILE), "[build]\n").unwrap();
        let error = BuildConfig::load(&dir).unwrap_err();
        assert!(error.contains("[[step]]"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
