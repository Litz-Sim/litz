//! Boots the stock ESP-IDF `hello_world` for ESP32-C3 through the `esp32sim`
//! backend, from the real mask ROM up.
//!
//! ```sh
//! LITZ_ROM_DIR=build/roms \
//! LITZ_C3_ASSETS=build/esp32sim/web/wasm/fw/public \
//! cargo run -p litz-esp --features esp32c3 --example c3_boot
//! ```
//!
//! Nothing in this path is patched or stubbed: this is the same `.bin` that
//! `idf.py build` produces.

#[cfg(feature = "esp32c3")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    let rom_dir = PathBuf::from(std::env::var("LITZ_ROM_DIR")?);
    let assets = PathBuf::from(std::env::var("LITZ_C3_ASSETS")?);

    let rom = std::fs::read(rom_dir.join("esp32c3_rev3_rom.elf"))?;
    let bootloader = std::fs::read(assets.join("c3-hello-bootloader.bin"))?;
    let ptable = std::fs::read(assets.join("c3-hello-ptable.bin"))?;
    let app = std::fs::read(assets.join("c3-hello_world.bin"))?;

    let mut mcu = litz_esp::backend::C3::new([0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56], 4 * 1024 * 1024);
    mcu.load_rom(&rom)?;
    mcu.write_flash(0x0, &bootloader)?;
    mcu.write_flash(0x8000, &ptable)?;
    mcu.write_flash(0x1_0000, &app)?;

    for _ in 0..200 {
        let _ = mcu.run(1_000_000);
        if String::from_utf8_lossy(mcu.uart0()).contains("Hello") {
            break;
        }
    }

    print!("{}", String::from_utf8_lossy(mcu.uart0()));
    Ok(())
}

#[cfg(not(feature = "esp32c3"))]
fn main() {
    eprintln!("build with --features esp32c3");
}
