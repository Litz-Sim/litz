//! `litz run` / `litz test` / `litz check`.
//!
//! Exit codes: 0 = pass (all boards halted), 42 = the run reached its time limit, 1 = assertion
//! failed, electrical fault or invalid diagram.

use litz_cli::catalog;
use litz_cli::report;
use litz_cli::runner::{
    GdbRequest, Interactive, Mode, Options, Screenshot, execute_capture, execute_scenario_with,
    execute_with,
};
use litz_cli::serve;
use litz_cli::toolchain;
use litz_mcp::{RunReport, Runtime, TestEntry, TestReport};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp") {
        return run_mcp(&args);
    }
    // The catalog is a command group of its own: its subcommands and their
    // positional arguments are not the runner's flags, so they are parsed here
    // rather than in the option loop below.
    if args.first().map(String::as_str) == Some("catalog") {
        return run_catalog(&args[1..]);
    }
    // Static memory and heap bounds come from the ELF itself; no project and
    // no run are involved, so the subcommand is parsed before the option loop.
    if args.first().map(String::as_str) == Some("heap") {
        return run_heap(&args[1..]);
    }
    // A board bundle is checked on its own, without a project: an author runs
    // this in the directory `litz catalog new board` made, before installing.
    if args.first().map(String::as_str) == Some("board") {
        return run_board(&args[1..]);
    }
    // `toolchain` and `doctor` introspect the host machine, not a project, so
    // their flags are parsed here; `editor` writes editor files for a project.
    if matches!(
        args.first().map(String::as_str),
        Some("toolchain" | "doctor")
    ) {
        return run_toolchain(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("editor") {
        return run_editor(&args[1..]);
    }

    let mut command = None;
    let mut dir: Option<PathBuf> = None;
    let mut strict = false;
    let mut port = serve::DEFAULT_PORT;
    let mut as_json = false;
    let mut junit: Option<PathBuf> = None;
    let mut scenario: Option<PathBuf> = None;
    let mut update_lock = false;
    let mut dry_run = false;
    let mut build_first = false;
    let mut options = Options::default();

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        // `--interactive=BOARD` names the board without a second token, so the
        // flag keeps its usual no-argument shape.
        if let Some(board) = arg.strip_prefix("--interactive=") {
            if board.is_empty() {
                eprintln!("litz: --interactive= needs a board name");
                return ExitCode::from(2);
            }
            options.interactive = Some(Interactive {
                board: Some(board.to_string()),
            });
            continue;
        }
        match arg.as_str() {
            "run" | "test" | "check" | "lint" | "build" | "serve" => command = Some(arg),
            "--dry-run" => dry_run = true,
            "--build" => build_first = true,
            "--strict" | "--warnings-as-errors" => strict = true,
            "--json" => as_json = true,
            "--update-lock" => update_lock = true,
            "--analog" => options.analog = true,
            "--coverage" => options.coverage = true,
            "--interactive" => options.interactive = Some(Interactive::default()),
            "--wifi" => match args.next() {
                Some(spec) => options.wifi = Some(spec),
                None => {
                    eprintln!("litz: --wifi needs an access point (SSID or SSID:password)");
                    return ExitCode::from(2);
                }
            },
            "--probe" => match args.next() {
                Some(point) => options.probes.push(point),
                None => {
                    eprintln!("litz: --probe needs a point, e.g. esp:GPIO8 or led1:A");
                    return ExitCode::from(2);
                }
            },
            "--scope" => match args.next() {
                Some(path) => options.scope = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --scope needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--decode" => match args.next().and_then(|spec| spec.parse().ok()) {
                Some(spec) => options.decodes.push(spec),
                None => {
                    eprintln!(
                        "litz: --decode needs a decoder, e.g. uart:rx=tx,baud=115200 or \
                         i2c:sda=sda,scl=scl"
                    );
                    return ExitCode::from(2);
                }
            },
            "--vcd" => match args.next() {
                Some(path) => options.vcd = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --vcd needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--pcap" => match args.next() {
                Some(path) => options.pcap = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --pcap needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--pcap-air" => match args.next() {
                Some(path) => options.pcap_air = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --pcap-air needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--pcap-eth" => match args.next() {
                Some(path) => options.pcap_eth = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --pcap-eth needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--net" => options.host_network = true,
            "--hostfwd" => match args.next() {
                Some(spec) => {
                    options.host_network = true;
                    options.host_forwards.push(spec);
                }
                None => {
                    eprintln!("litz: --hostfwd needs tcp:[host:]port-[guest:]port");
                    return ExitCode::from(2);
                }
            },
            "--sd-export" => match args.next() {
                Some(path) => options.sd_export = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --sd-export needs a file or directory");
                    return ExitCode::from(2);
                }
            },
            "--heap" => options.heap_sample = true,
            "--heap-interval" => match args.next().and_then(|ms| ms.parse::<u64>().ok()) {
                Some(ms) if ms > 0 => {
                    options.heap_sample = true;
                    options.heap_interval = Some(ms.saturating_mul(1_000_000));
                }
                _ => {
                    eprintln!("litz: --heap-interval needs milliseconds (a positive whole number)");
                    return ExitCode::from(2);
                }
            },
            "--heap-min-free" => match args.next().and_then(|bytes| bytes.parse::<u32>().ok()) {
                Some(bytes) => {
                    options.heap_sample = true;
                    options.heap_min_free = Some(bytes);
                }
                None => {
                    eprintln!("litz: --heap-min-free needs a byte count");
                    return ExitCode::from(2);
                }
            },
            "--heap-csv" => match args.next() {
                Some(path) => {
                    options.heap_sample = true;
                    options.heap_csv = Some(PathBuf::from(path));
                }
                None => {
                    eprintln!("litz: --heap-csv needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--trigger" => match args
                .next()
                .map(|spec| litz_engine::runner::parse_trigger(&spec))
            {
                Some(Ok(trigger)) => options.trigger = Some(trigger),
                Some(Err(error)) => {
                    eprintln!("litz: {error}");
                    return ExitCode::from(2);
                }
                None => {
                    eprintln!("litz: --trigger needs CHANNEL=high|low[,pre=...,post=...]");
                    return ExitCode::from(2);
                }
            },
            "--break" => match args.next().and_then(|a| parse_address(&a)) {
                Some(pc) => options.break_at = Some(pc),
                None => {
                    eprintln!("litz: --break needs an address (0x40380000 or 1077346304)");
                    return ExitCode::from(2);
                }
            },
            "--screenshot" => match args.next().and_then(|spec| parse_screenshot(&spec)) {
                Some(capture) => options.screenshot = Some(capture),
                None => {
                    eprintln!("litz: --screenshot needs a file to write, or <board>@<file>");
                    return ExitCode::from(2);
                }
            },
            "--gdb" => match args.next().and_then(|spec| parse_gdb(&spec)) {
                Some(request) => options.gdb = Some(request),
                None => {
                    eprintln!("litz: --gdb needs a port, or <board>@<port> to pick the board");
                    return ExitCode::from(2);
                }
            },
            "--scenario" => match args.next() {
                Some(path) => scenario = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --scenario needs a YAML file");
                    return ExitCode::from(2);
                }
            },
            "--junit" => match args.next() {
                Some(path) => junit = Some(PathBuf::from(path)),
                None => {
                    eprintln!("litz: --junit needs a file to write");
                    return ExitCode::from(2);
                }
            },
            "--port" => match args.next().and_then(|p| p.parse().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("litz: --port needs a number");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                usage();
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("litz {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("litz: unknown option {other}");
                return ExitCode::from(2);
            }
            other => dir = Some(PathBuf::from(other)),
        }
    }

    let dir = dir.unwrap_or_else(|| PathBuf::from("."));

    if command.as_deref() == Some("build") {
        return match litz_cli::build::run(&dir, dry_run) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("litz: {e}");
                ExitCode::from(1)
            }
        };
    }

    // `--build` makes `test` and `run` one command for a project whose
    // firmware is produced by its own recipe, which is how most CI jobs are
    // already shaped.
    if build_first && matches!(command.as_deref(), None | Some("test" | "run")) {
        match litz_cli::build::run(&dir, false) {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("litz: {e}");
                return ExitCode::from(1);
            }
        }
    }

    if command.as_deref() == Some("serve") {
        return match serve::serve(&dir, port) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("litz: {e}");
                ExitCode::from(1)
            }
        };
    }

    let mode = match command.as_deref().unwrap_or("run") {
        "run" => Mode::Run,
        "test" => Mode::Test,
        // `lint` is the name a build system may already use; both validate the
        // diagram without running firmware, so the exit codes stay identical.
        "check" | "lint" => Mode::Check,
        _ => unreachable!(),
    };

    let result = match &scenario {
        Some(path) => execute_scenario_with(&dir, path, strict, update_lock, &options),
        None => execute_with(&dir, mode, strict, update_lock, &options),
    };
    match result {
        Ok(outcome) => {
            if let Some(path) = &junit
                && let Err(e) = std::fs::write(path, report::junit(&outcome, "litz"))
            {
                eprintln!("litz: {}: {e}", path.display());
            }
            if as_json {
                println!("{}", report::json(&outcome));
            } else {
                print!("{}", outcome.output);
            }
            ExitCode::from(outcome.exit_code as u8)
        }
        Err(e) => {
            eprintln!("litz: {e}");
            ExitCode::from(1)
        }
    }
}

/// `litz toolchain` (alias `litz doctor`): report the firmware toolchains on
/// this machine. Missing tools are reported, never installed, and the exit
/// code stays 0 so a diagnostic script can read the table without branching.
fn run_toolchain(args: &[String]) -> ExitCode {
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => as_json = true,
            "-h" | "--help" => {
                toolchain_usage();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("litz: unknown toolchain option {other}");
                return ExitCode::from(2);
            }
            other => {
                eprintln!("litz: toolchain takes no directory (got {other:?})");
                return ExitCode::from(2);
            }
        }
    }

    let environment = toolchain::Environment::from_process();
    let tools = toolchain::detect(&environment);
    if as_json {
        print!("{}", toolchain::to_json(&tools));
    } else {
        print!("{}", toolchain::table(&tools));
    }
    ExitCode::SUCCESS
}

fn toolchain_usage() {
    eprintln!("usage: litz toolchain [--json]   (alias: litz doctor)");
    eprintln!("  Reports, one line per tool, whether these are installed and their version:");
    eprintln!("    arduino-cli and the ESP32 core, idf.py / $IDF_PATH, riscv32-esp-elf-gcc,");
    eprintln!("    xtensa-esp32/s3/c3-elf-gcc, arm-none-eabi-gcc, avr-gcc, avrdude,");
    eprintln!("    micropython, mpremote, rustc and the wasm32-unknown-unknown target.");
    eprintln!("  --json prints a machine-readable object: {{\"tools\": [...]}}.");
    eprintln!();
    eprintln!("  This command does not change your system: it only reads PATH and $IDF_PATH");
    eprintln!("  and runs version probes with a short timeout. It makes no network calls and");
    eprintln!("  installs nothing, and exits 0 even when tools are missing.");
}

/// `litz editor --vscode [directory] [--force] [--gdb-port N]`: write the
/// project's `.vscode/launch.json` and `.vscode/tasks.json` so F5 boots the
/// firmware under Litz's GDB stub and the serial output stays in a terminal.
fn run_editor(args: &[String]) -> ExitCode {
    let mut vscode = false;
    let mut force = false;
    let mut port = toolchain::DEFAULT_GDB_PORT;
    let mut directory: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--vscode" => vscode = true,
            "--force" => force = true,
            "-h" | "--help" => {
                editor_usage();
                return ExitCode::SUCCESS;
            }
            "--gdb-port" => match args.get(i + 1).and_then(|value| value.parse().ok()) {
                Some(value) => {
                    port = value;
                    i += 1;
                }
                None => {
                    eprintln!("litz: --gdb-port needs a number");
                    return ExitCode::from(2);
                }
            },
            _ if arg.starts_with("--gdb-port=") => match arg["--gdb-port=".len()..].parse().ok() {
                Some(value) => port = value,
                None => {
                    eprintln!("litz: --gdb-port needs a number");
                    return ExitCode::from(2);
                }
            },
            other if other.starts_with('-') => {
                eprintln!("litz: unknown editor option {other}");
                return ExitCode::from(2);
            }
            other => {
                if directory.is_some() {
                    eprintln!("litz: editor takes at most one directory");
                    return ExitCode::from(2);
                }
                directory = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    if !vscode {
        eprintln!("litz: editor needs --vscode (the only editor target today)");
        editor_usage();
        return ExitCode::from(2);
    }

    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    match toolchain::write_vscode(&directory, force, port) {
        Ok(paths) => {
            for path in paths {
                println!("wrote {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("litz: {error}");
            ExitCode::from(1)
        }
    }
}

fn editor_usage() {
    eprintln!("usage: litz editor --vscode [directory] [--force] [--gdb-port N]");
    eprintln!("  Writes .vscode/launch.json (cppdbg -> 127.0.0.1:PORT) and .vscode/tasks.json,");
    eprintln!("  whose background task runs `litz run . --gdb PORT` so F5 debugs the");
    eprintln!("  firmware and the task's terminal keeps streaming its serial output.");
    eprintln!("  directory defaults to the current directory; PORT defaults to 3333.");
    eprintln!("  --force merges into existing files and keeps unrelated configurations;");
    eprintln!("  without it an existing launch.json or tasks.json is refused.");
}

/// `litz catalog [--json]` and its `list`, `check`, `add`, `new`, `new board`
/// and `remove` subcommands.
///
/// `add`, `new` and `remove` act on the project in the current directory and
/// write to `--dir` (default `parts`, or `boards` for `new board`). `list` and
/// `check` take the project directory as their positional argument.
/// `litz board check`: does this bundle parse, instantiate and describe itself?
fn run_board(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("check") => {
            let path = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            match litz_engine::board::check(&path) {
                Ok(report) => {
                    print!("{}", report.lines());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("litz: {error}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("usage: litz board check [directory]");
            ExitCode::from(2)
        }
    }
}

fn run_catalog(args: &[String]) -> ExitCode {
    let mut json = false;
    let mut dir = String::from("parts");
    let mut dir_given = false;
    let mut name: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => {
                catalog_usage();
                return ExitCode::SUCCESS;
            }
            "--json" => json = true,
            "--dir" => match args.get(i + 1) {
                Some(value) => {
                    dir = value.clone();
                    dir_given = true;
                    i += 1;
                }
                None => {
                    eprintln!("litz: catalog --dir needs a directory");
                    return ExitCode::from(2);
                }
            },
            "--name" => match args.get(i + 1) {
                Some(value) => {
                    name = Some(value.clone());
                    i += 1;
                }
                None => {
                    eprintln!("litz: catalog --name needs a name");
                    return ExitCode::from(2);
                }
            },
            _ if arg.starts_with("--dir=") => {
                dir = arg["--dir=".len()..].to_string();
                dir_given = true;
            }
            _ if arg.starts_with("--name=") => name = Some(arg["--name=".len()..].to_string()),
            _ if arg.starts_with('-') => {
                eprintln!("litz: unknown catalog option {arg}");
                return ExitCode::from(2);
            }
            _ => positionals.push(arg.to_string()),
        }
        i += 1;
    }

    match positionals.first().map(String::as_str) {
        None => {
            if json {
                println!("{}", catalog::builtin_json());
            } else {
                print!("{}", catalog::builtin_table());
            }
            ExitCode::SUCCESS
        }
        Some("list") => {
            let Some(project) = project_argument(&positionals) else {
                return catalog_usage_error("catalog list takes one directory");
            };
            match catalog::listing(&project) {
                Ok(rows) => {
                    print!("{}", catalog::table(&rows, true));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("litz: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some("check") => {
            let Some(project) = project_argument(&positionals) else {
                return catalog_usage_error("catalog check takes one directory");
            };
            let report = catalog::check_dir(&project);
            if report.errors.is_empty() {
                let noun = if report.checked == 1 {
                    "manifest"
                } else {
                    "manifests"
                };
                println!("{} {noun} ok", report.checked);
                ExitCode::SUCCESS
            } else {
                for error in report.errors {
                    println!("{error}");
                }
                ExitCode::from(1)
            }
        }
        Some("add") => {
            if positionals.len() != 2 {
                return catalog_usage_error("catalog add takes one bundle directory");
            }
            let project = match current_project() {
                Ok(project) => project,
                Err(error) => return error,
            };
            match catalog::add(Path::new(&positionals[1]), &project, &dir) {
                Ok(installed) => {
                    println!("installed {} at {}", installed.id, installed.path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("litz: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some("new") => {
            // `new <id>` is a part; `new board <id>` is a board bundle. A part
            // may still be called "board", because that form has two
            // positionals and the board form has three.
            let (board, id) = match positionals.as_slice() {
                [_, id] => (false, id.as_str()),
                [_, kind, id] if kind.as_str() == "board" => (true, id.as_str()),
                _ => {
                    return catalog_usage_error("catalog new takes an id, or `board <id>`");
                }
            };
            let project = match current_project() {
                Ok(project) => project,
                Err(error) => return error,
            };
            // A board belongs in `boards/` unless the author chose elsewhere.
            let dir = if board && !dir_given {
                "boards".to_string()
            } else {
                dir.clone()
            };
            let created = if board {
                catalog::new_board(&project, &dir, id, name.as_deref())
            } else {
                catalog::new_part(&project, &dir, id, name.as_deref())
            };
            match created {
                Ok(path) => {
                    println!("created {}", path.display());
                    if board {
                        // The generated directory is a complete bundle in
                        // this project's catalog; the two commands that turn
                        // it into a running board.
                        println!("  next: cd {} && ./simulator/build.sh", path.display());
                        println!("        litz board check .");
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("litz: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some("remove") => {
            if positionals.len() != 2 {
                return catalog_usage_error("catalog remove takes one id");
            }
            let project = match current_project() {
                Ok(project) => project,
                Err(error) => return error,
            };
            match catalog::remove(&project, &dir, &positionals[1]) {
                Ok(path) => {
                    println!("removed {} ({})", positionals[1], path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("litz: {error}");
                    ExitCode::from(1)
                }
            }
        }
        Some(other) => {
            eprintln!("litz: unknown catalog subcommand {other:?}");
            ExitCode::from(2)
        }
    }
}

/// The positional project directory, or the current one. At most one is
/// accepted.
fn project_argument(positionals: &[String]) -> Option<PathBuf> {
    match positionals {
        [_] => Some(PathBuf::from(".")),
        [_, project] => Some(PathBuf::from(project)),
        _ => None,
    }
}

fn current_project() -> Result<PathBuf, ExitCode> {
    std::env::current_dir().map_err(|error| {
        eprintln!("litz: cannot read the current directory: {error}");
        ExitCode::from(1)
    })
}

fn catalog_usage_error(message: &str) -> ExitCode {
    eprintln!("litz: {message}");
    catalog_usage();
    ExitCode::from(2)
}

fn catalog_usage() {
    eprintln!("usage: litz catalog [--json]");
    eprintln!("       litz catalog list [directory]");
    eprintln!("       litz catalog check [directory]");
    eprintln!(
        "       litz catalog add <bundle> [--dir DIR]   (default parts; boards install under DIR/<id>)"
    );
    eprintln!("       litz catalog new <id> [--dir parts] [--name NAME]");
    eprintln!("       litz catalog new board <id> [--dir boards] [--name NAME]");
    eprintln!("       litz catalog remove <id> [--dir parts]");
    eprintln!("  --json prints the builtin catalog as JSON");
}

/// `litz heap FIRMWARE.elf`: static RAM and heap bounds from the image.
fn run_heap(args: &[String]) -> ExitCode {
    match args {
        [flag] if flag == "-h" || flag == "--help" => {
            println!("usage: litz heap FIRMWARE.elf");
            println!("  Report the image's static RAM sections and its heap bounds.");
            println!("  Runtime free heap is sampled by `litz run --heap` on an RV32 chip.");
            ExitCode::SUCCESS
        }
        [path] => match litz_cli::heap::report(Path::new(path)) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("litz: {error}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprintln!("litz: heap takes one firmware ELF (try `litz heap firmware.elf`)");
            ExitCode::from(2)
        }
    }
}

/// `litz mcp`: speak the Model Context Protocol on stdin/stdout.
///
/// The server owns stdout, so any diagnostic goes to stderr. A clean EOF is a
/// normal shutdown; only bad arguments or an I/O failure are errors.
fn run_mcp(args: &[String]) -> ExitCode {
    match args.get(1).map(String::as_str) {
        None => {}
        Some("-h" | "--help") if args.len() == 2 => {
            usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("litz: mcp takes no arguments (got {other:?})");
            return ExitCode::from(2);
        }
    }

    // The server crate must not depend on the CLI (the CLI calls it), so the
    // runner is installed here, from the same `runner` module the CLI uses.
    litz_mcp::install_runtime(Box::new(LitzRuntime));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match litz_mcp::serve_stdio(stdin.lock(), stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("litz: mcp: {error}");
            ExitCode::from(1)
        }
    }
}

/// The installed runtime: the existing runner, shaped for the protocol crate.
struct LitzRuntime;

impl Runtime for LitzRuntime {
    fn test(
        &self,
        root: &Path,
        scenario: Option<&Path>,
        strict: bool,
    ) -> Result<TestReport, String> {
        let outcome = match scenario {
            Some(path) => execute_scenario_with(root, path, strict, false, &Options::default()),
            None => execute_with(root, Mode::Test, strict, false, &Options::default()),
        }?;
        Ok(TestReport {
            exit_code: outcome.exit_code,
            output: outcome.output,
            sim_ms: outcome.sim_ms,
            tests: outcome
                .tests
                .into_iter()
                .map(|test| TestEntry {
                    name: test.name,
                    passed: test.passed,
                    exit_code: test.exit_code,
                    output: test.output,
                    sim_ms: test.sim_ms,
                })
                .collect(),
        })
    }

    fn run(&self, root: &Path, timeout: Option<&str>, strict: bool) -> Result<RunReport, String> {
        let outcome = execute_capture(root, timeout, strict, false, &Options::default())?;
        Ok(RunReport {
            exit_code: outcome.exit_code,
            output: outcome.output,
            sim_ms: outcome.sim_ms,
            part_states: outcome.part_states.into_iter().collect(),
            faults: outcome.faults,
        })
    }
}

/// A program counter as hex (`0x40380000`) or plain decimal.
/// `[<board>@]FILE`: which board's display to capture, and where. A prefix is
/// read as a board only when it has no path separator, so a file named
/// `shots/a@2x.png` stays a path.
fn parse_screenshot(text: &str) -> Option<Screenshot> {
    let (board, path) = match text.split_once('@') {
        // `@file` is neither a board nor a sensible name; `./@file` is a path.
        Some(("", _)) => return None,
        Some((prefix, rest)) if !prefix.contains('/') && !prefix.contains('\\') => {
            (Some(prefix.to_string()), rest)
        }
        _ => (None, text),
    };
    if path.is_empty() {
        return None;
    }
    Some(Screenshot {
        board,
        path: PathBuf::from(path),
    })
}

/// `3333` or `b@3333`: which board's chip to hand to the debugger, and the
/// port to listen on.
fn parse_gdb(text: &str) -> Option<GdbRequest> {
    match text.split_once('@') {
        Some((board, port)) if !board.is_empty() => Some(GdbRequest {
            board: Some(board.to_string()),
            port: port.parse().ok()?,
        }),
        Some(_) => None,
        None => Some(GdbRequest {
            board: None,
            port: text.parse().ok()?,
        }),
    }
}

fn parse_address(text: &str) -> Option<u32> {
    let text = text.trim();
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn usage() {
    eprintln!("usage: litz <run|test|check|lint> [directory] [--strict] [--json] [--junit FILE]");
    eprintln!("       litz test [directory] --scenario scenario.yaml");
    eprintln!("       litz build [directory] [--dry-run]");
    eprintln!("       litz serve [directory] [--port N]");
    eprintln!(
        "       litz catalog [--json] | list | check | add | new | remove
       litz board check [directory]"
    );
    eprintln!("       litz heap FIRMWARE.elf");
    eprintln!("       litz toolchain [--json]   (alias: litz doctor)");
    eprintln!("       litz editor --vscode [directory] [--force] [--gdb-port N]");
    eprintln!("       litz mcp");
    eprintln!("       litz --version");
    eprintln!(
        "  run    boot the firmware and stream its serial output (0 if every board halts, 42 at the time limit)"
    );
    eprintln!(
        "  test   run the [[test]] entries in litz.toml and assert on serial output and part state"
    );
    eprintln!(
        "  check  validate diagram.json against the catalog without running (`lint` is an alias)"
    );
    eprintln!("  build  run the project's litz.build.toml recipe and check its firmware output");
    eprintln!("  serve  serve a directory over HTTP (default directory: ., default port: 8000)");
    eprintln!("  catalog  print the builtin catalog, or install and inspect a project's own parts");
    eprintln!(
        "  toolchain  report installed firmware toolchains (read-only; no network, no installs)"
    );
    eprintln!("  editor     write .vscode launch/tasks so F5 debugs under Litz's GDB stub");
    eprintln!("  mcp    serve the Model Context Protocol over stdin/stdout");
    eprintln!("  --strict       treat validation warnings as errors (--warnings-as-errors)");
    eprintln!("  --json         print one JSON record instead of the transcript");
    eprintln!("  --junit FILE   also write a JUnit XML report");
    eprintln!("  --scenario F   run an automation scenario instead of the [[test]] entries");
    eprintln!("  --build        run litz.build.toml before `test` or `run`");
    eprintln!("  --dry-run      print the build steps without running them");
    eprintln!("  --update-lock  rewrite litz.lock instead of checking it");
    eprintln!(
        "  --vcd FILE     record every net transition as a VCD (PulseView, GTKWave)
  --pcap FILE    write the simulated network's traffic as a pcap (Wireshark)"
    );
    eprintln!(
        "  --pcap-air FILE  record every 802.11 frame as a link-type-105 pcap (Wireshark)
  --pcap-eth FILE  record the station's Ethernet frames as a link-type-1 pcap"
    );
    eprintln!(
        "  --net           put host sockets behind the Wi-Fi gateway (not deterministic)
  --hostfwd SPEC  forward a host port into the guest (tcp:[host:]port-[guest:]port)"
    );
    eprintln!("  --sd-export PATH  save the image of every microSD card the guest wrote");
    eprintln!(
        "  --heap         follow the firmware's free heap (default: every 1000 ms)
  --heap-interval MS  sample the heap every MS of simulated time
  --heap-min-free N   fail the run when the free heap drops below N bytes
  --heap-csv FILE     append part,time_s,free,min_free samples to FILE"
    );
    eprintln!("  --probe [NAME=]POINT  watch a point (esp:GPIO8, sck=esp:GPIO2); repeat for more");
    eprintln!("  --scope FILE   write every probe's record as a CSV when the run ends");
    eprintln!(
        "  --trigger SPEC start the scope at a logic-analyzer trigger (CHANNEL=high|low[,pre=..,post=..])"
    );
    eprintln!(
        "  --decode SPEC  read a protocol off probe channels; repeat for more \
         (uart:rx=tx[,baud=N], i2c:sda=a,scl=b, spi:sck=c,mosi=d,cs=e[,miso=f][,mode=N])"
    );
    eprintln!("  --analog       solve the analog network while the firmware runs");
    eprintln!("  --coverage     print the backend's instruction coverage report after the run");
    eprintln!(
        "  --interactive[=BOARD]  forward lines typed on stdin to that board's console (default: the first board)"
    );
    eprintln!("  --wifi SPEC    give the ESP32 radio an access point (ssid=NAME[,psk=PASS])");
    eprintln!("  --break ADDR   stop at this program counter (hex or decimal); inspect with --vcd");
    eprintln!(
        "  --screenshot [BOARD@]FILE  write that board's display as a PNG when the run ends or a test fails"
    );
    eprintln!(
        "  --gdb [BOARD@]PORT  serve the GDB remote protocol on 127.0.0.1:PORT; \
         BOARD picks the chip (default: the first with a debug target)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screenshot_names_a_file_and_optionally_a_board() {
        let plain = parse_screenshot("shot.png").expect("a file alone is enough");
        assert_eq!(plain.path, PathBuf::from("shot.png"));
        assert!(plain.board.is_none());

        let named = parse_screenshot("b@shot.png").expect("a board may come first");
        assert_eq!(named.board.as_deref(), Some("b"));
        assert_eq!(named.path, PathBuf::from("shot.png"));

        // A path with a separator is a path, `@` and all.
        let nested = parse_screenshot("build/shots/a@2x.png").expect("a path stays a path");
        assert!(nested.board.is_none());
        assert_eq!(nested.path, PathBuf::from("build/shots/a@2x.png"));

        assert!(parse_screenshot("b@").is_none(), "a board with no file");
        assert!(parse_screenshot("@shot.png").is_none(), "an empty board");
    }

    #[test]
    fn the_gdb_argument_names_a_port_and_optionally_a_board() {
        let plain = parse_gdb("3333").expect("a port alone is enough");
        assert_eq!(plain.port, 3333);
        assert!(plain.board.is_none());

        let named = parse_gdb("b@3334").expect("a board may come first");
        assert_eq!(named.port, 3334);
        assert_eq!(named.board.as_deref(), Some("b"));

        assert!(parse_gdb("b@").is_none(), "a board with no port");
        assert!(parse_gdb("@3333").is_none(), "a port with an empty board");
        assert!(parse_gdb("nope").is_none());
    }
}
