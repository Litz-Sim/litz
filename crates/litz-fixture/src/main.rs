//! Write a fixture firmware image to a path.
//!
//! ```text
//! litz-fixture blinky <out.bin> [--period-ms N]
//! litz-fixture button <out.bin>
//! litz-fixture pot    <out.bin>
//! litz-fixture echo   <out.bin>
//! litz-fixture hello  <out.bin>
//! ```
//!
//! The output is a complete ESP-IDF application image, ready for
//! `C3::write_flash(0x1_0000, ...)` followed by `C3::boot_app(0x1_0000)`.
//! Success is one line on stderr with the byte count and entry address; a bad
//! argument or a write failure exits 1.

use litz_fixture::{
    CODE_ADDR, CYCLES_PER_MS, blinky, button_led, echo, hello, hold_low, pico_blink, pot_led,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("litz-fixture: {message}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        usage();
        return Err("missing command (try --help)".into());
    };
    match command {
        "-h" | "--help" => {
            usage();
            Ok(())
        }
        "blinky" => write_blinky(&args[1..]),
        "button" => write_plain(&args[1..], "button", button_led),
        "pot" => write_plain(&args[1..], "pot", pot_led),
        "hello" => write_plain(&args[1..], "hello", hello),
        "echo" => write_plain(&args[1..], "echo", echo),
        "pico" => write_pico(&args[1..]),
        "stage2" => write_stage2(&args[1..]),
        "low" => write_plain(&args[1..], "low", hold_low),
        other => Err(format!("unknown command {other:?} (try --help)")),
    }
}

/// `blinky <out.bin> [--period-ms N]`.
fn write_blinky(args: &[String]) -> Result<(), String> {
    let mut output: Option<PathBuf> = None;
    let mut period_ms: u32 = 100;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            "--period-ms" => {
                index += 1;
                let value = args.get(index).ok_or("--period-ms needs a value")?;
                period_ms = parse_period_ms(value)?;
            }
            _ if arg.starts_with("--period-ms=") => {
                period_ms = parse_period_ms(&arg["--period-ms=".len()..])?;
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option {arg:?} (try --help)"));
            }
            _ => {
                if output.replace(PathBuf::from(arg)).is_some() {
                    return Err("more than one output path".into());
                }
            }
        }
        index += 1;
    }

    let path = output
        .ok_or("missing output path (usage: litz-fixture blinky <out.bin> [--period-ms N])")?;
    // The tick is `period_ms * 160_000` cycles; a u32 period that does not fit
    // is rejected rather than wrapped into a much shorter blink.
    let cycles = period_ms
        .checked_mul(CYCLES_PER_MS)
        .ok_or_else(|| format!("--period-ms {period_ms}: too large for a 32-bit cycle count"))?;
    write(&path, &blinky(cycles))
}

/// `pico <out.bin> [--gpio N] [--period-ms N]`: an RP2040 flash image.
fn write_pico(args: &[String]) -> Result<(), String> {
    let mut output: Option<PathBuf> = None;
    let mut gpio: u8 = 15;
    let mut period_ms: u32 = 100;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            "--gpio" => {
                index += 1;
                let value = args.get(index).ok_or("--gpio needs a value")?;
                gpio = value
                    .parse()
                    .map_err(|_| format!("--gpio {value:?}: expected 0-29"))?;
            }
            "--period-ms" => {
                index += 1;
                let value = args.get(index).ok_or("--period-ms needs a value")?;
                period_ms = parse_period_ms(value)?;
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option {arg:?} (try --help)"));
            }
            _ => {
                if output.replace(PathBuf::from(arg)).is_some() {
                    return Err("more than one output path".into());
                }
            }
        }
        index += 1;
    }

    if gpio > 29 {
        return Err(format!(
            "--gpio {gpio}: the RP2040 has GPIO0 through GPIO29"
        ));
    }
    let path = output.ok_or(
        "missing output path (usage: litz-fixture pico <out.bin> [--gpio N] [--period-ms N])",
    )?;
    write_at(
        &path,
        &pico_blink(gpio, period_ms),
        litz_fixture::pico::FLASH_BASE,
    )
}

/// `stage2 <out.bin>`: the 256 bytes the RP2040 ROM copies into SRAM.
fn write_stage2(args: &[String]) -> Result<(), String> {
    let mut output: Option<PathBuf> = None;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag:?} (try --help)"));
            }
            path => {
                if output.replace(PathBuf::from(path)).is_some() {
                    return Err("more than one output path".into());
                }
            }
        }
    }
    let path = output.ok_or("missing output path (usage: litz-fixture stage2 <out.bin>)")?;
    report(&path, &litz_fixture::pico::stage2(), None)
}

/// `<command> <out.bin>` for the programs that take no options.
fn write_plain(args: &[String], command: &str, build: fn() -> Vec<u8>) -> Result<(), String> {
    let mut output: Option<PathBuf> = None;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag:?} (try --help)"));
            }
            path => {
                if output.replace(PathBuf::from(path)).is_some() {
                    return Err("more than one output path".into());
                }
            }
        }
    }
    let path = output
        .ok_or_else(|| format!("missing output path (usage: litz-fixture {command} <out.bin>)"))?;
    write(&path, &build())
}

fn parse_period_ms(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .map_err(|_| format!("--period-ms {text:?}: expected a whole number of milliseconds"))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_at(path, bytes, CODE_ADDR)
}

fn write_at(path: &Path, bytes: &[u8], entry: u32) -> Result<(), String> {
    report(path, bytes, Some(entry))
}

fn report(path: &Path, bytes: &[u8], entry: Option<u32>) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    match entry {
        Some(entry) => eprintln!(
            "litz-fixture: wrote {} bytes to {} (entry {:#010x})",
            bytes.len(),
            path.display(),
            entry
        ),
        None => eprintln!(
            "litz-fixture: wrote {} bytes to {}",
            bytes.len(),
            path.display()
        ),
    }
    Ok(())
}

fn usage() {
    eprintln!("usage: litz-fixture <command> <out.bin> [options]");
    eprintln!();
    eprintln!("commands:");
    eprintln!("  blinky   ESP32-C3 image: configure GPIO8, blink it, mark edges on UART0");
    eprintln!("  button   ESP32-C3 image: light GPIO8 while GPIO0 is held low");
    eprintln!("  pot      ESP32-C3 image: read ADC1 channel 0, light GPIO8 past half");
    eprintln!("  hello    minimal ESP32-C3 image: print one UART0 line and spin");
    eprintln!("  echo     ESP32-C3 image: send every UART0 receive byte back out");
    eprintln!("  pico     RP2040 flash image: print on UART0 (GP0) and blink a GPIO");
    eprintln!("  stage2   the 256 bytes the RP2040 ROM copies to SRAM before the app");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --period-ms N   blinky half-period in milliseconds (default 100)");
    eprintln!("  --gpio N        pico blink pin, 0-29 (default 15)");
    eprintln!("  -h, --help      show this message");
}
