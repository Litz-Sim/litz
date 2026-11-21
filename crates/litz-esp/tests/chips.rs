//! Every chip the adapter carries answers the same trait, built the same way.
//!
//! These are shape tests, not boot tests: the point is that adding a chip is an
//! alias and a bus factory in `backend`, not a second adapter. Booting each chip
//! with its own firmware lives with the boards in `litz-cli`.

#![cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]

use litz_board::chip::Chip;

const MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x00, 0x01, 0x10];
const FLASH: usize = 4 << 20;

fn reports(chip: &impl Chip, name: &str) {
    let info = chip.info();
    assert_eq!(info.name, name);
    assert!(info.cpu_hz > 0, "{name} has no clock");
    assert_eq!(info.flash.app, 0x1_0000);
    assert_eq!(info.flash.size, FLASH as u64);
}

#[cfg(feature = "esp32")]
#[test]
fn the_classic_esp32_reports_itself() {
    reports(&litz_esp::backend::Esp32::new(MAC, FLASH), "esp32");
}

#[cfg(feature = "esp32c3")]
#[test]
fn the_c3_reports_itself() {
    reports(&litz_esp::backend::C3::new(MAC, FLASH), "esp32c3");
}

#[cfg(feature = "esp32c6")]
#[test]
fn the_c6_reports_itself() {
    reports(&litz_esp::backend::C6::new(MAC, FLASH), "esp32c6");
}

#[cfg(feature = "esp32s3")]
#[test]
fn the_s3_reports_itself() {
    reports(&litz_esp::backend::S3::new(MAC, FLASH), "esp32s3");
}

#[cfg(feature = "esp32p4")]
#[test]
fn the_p4_reports_itself() {
    reports(&litz_esp::backend::P4::new(MAC, FLASH), "esp32p4");
}

#[cfg(feature = "esp32s31")]
#[test]
fn the_s31_reports_itself() {
    reports(&litz_esp::backend::S31::new(MAC, FLASH), "esp32s31");
}

#[cfg(feature = "esp32c3")]
#[test]
fn the_c3_full_ble_controller_takes_a_central_command() {
    let mut chip = litz_esp::backend::C3::new(MAC, FLASH);
    chip.enable_full_ble(false)
        .expect("the C3 starts its link controller");
    chip.ble_command("connect")
        .expect("the virtual central queues a connection");
    assert_eq!(
        chip.ble_pending_commands(),
        1,
        "a connection waits for the guest to advertise"
    );
}

#[cfg(feature = "esp32c3")]
#[test]
fn a_heap_sample_without_symbols_says_why() {
    let mut chip = litz_esp::backend::C3::new(MAC, FLASH);
    let error = chip.sample_heap().unwrap_err();
    assert!(error.contains("esp_get_free_heap_size"), "{error}");
}

#[cfg(feature = "esp32c3")]
#[test]
fn a_call_into_unmapped_memory_reports_it_instead_of_hanging() {
    let mut chip = litz_esp::backend::C3::new(MAC, FLASH);
    let error = chip.call_guest(0xdead_beef).unwrap_err();
    assert!(error.contains("did not return"), "{error}");
}

/// The Xtensa constructors install the windowed trampoline; before that this
/// answered "not supported" without touching a core. An unmapped target should
/// now fail as a call that did not return.
#[cfg(feature = "esp32")]
#[test]
fn the_classic_esp32_installs_a_windowed_guest_call() {
    let mut chip = litz_esp::backend::Esp32::new(MAC, FLASH);
    let error = chip.call_guest(0xdead_beef).unwrap_err();
    assert!(error.contains("did not return"), "{error}");
    assert!(!error.contains("not supported"), "{error}");
}

#[cfg(feature = "esp32s3")]
#[test]
fn the_s3_installs_a_windowed_guest_call() {
    let mut chip = litz_esp::backend::S3::new(MAC, FLASH);
    let error = chip.call_guest(0xdead_beef).unwrap_err();
    assert!(error.contains("did not return"), "{error}");
    assert!(!error.contains("not supported"), "{error}");
}

/// A windowed function written into IRAM: `entry a1, 16; movi a2, 42; retw`.
/// Running it proves the synthesized `call8` returns through the sentinel and
/// reads the value from `a2`.
#[cfg(feature = "esp32")]
#[test]
fn the_esp32_guest_call_returns_the_callees_a2() {
    let mut chip = litz_esp::backend::Esp32::new(MAC, FLASH);
    chip.load_script("0 poke 0x40080000 0x22002136\n0 poke 0x40080004 0x00902aa0")
        .expect("the IRAM words are writable");
    chip.run(1);
    assert_eq!(chip.call_guest(0x4008_0000).unwrap(), 42);
}

#[cfg(feature = "esp32s3")]
#[test]
fn the_s3_guest_call_returns_the_callees_a2() {
    let mut chip = litz_esp::backend::S3::new(MAC, FLASH);
    chip.load_script("0 poke 0x40370000 0x22002136\n0 poke 0x40370004 0x00902aa0")
        .expect("the IRAM words are writable");
    chip.run(1);
    assert_eq!(chip.call_guest(0x4037_0000).unwrap(), 42);
}

#[cfg(feature = "esp32")]
#[test]
fn a_heap_sample_without_symbols_says_why_on_the_esp32() {
    let mut chip = litz_esp::backend::Esp32::new(MAC, FLASH);
    let error = chip.sample_heap().unwrap_err();
    assert!(error.contains("esp_get_free_heap_size"), "{error}");
}

#[cfg(feature = "esp32s3")]
#[test]
fn a_heap_sample_without_symbols_says_why_on_the_s3() {
    let mut chip = litz_esp::backend::S3::new(MAC, FLASH);
    let error = chip.sample_heap().unwrap_err();
    assert!(error.contains("esp_get_free_heap_size"), "{error}");
}
