//! A reference Litz chip: RV32IMC as a WebAssembly component.
//!
//! This is a complete MCU under the `litz:mcu@0.2.0` contract: a core, a
//! memory map, a GPIO block and UART0, plus the debug interface. It is not an emulator of the whole
//! ESP32-C3 — it has no ADC, no timers, no flash controller — but it runs the
//! firmware this repository generates (`litz-fixture`) unchanged, which is
//! what makes it a working reference rather than a sketch.
//!
//! It is also the template for a real chip crate: implement [`cpu`]'s memory
//! map for your part, replace [`machine`]'s peripherals with yours, keep the
//! glue at the bottom of this file.
#![allow(clippy::needless_range_loop)]

#[cfg(target_arch = "wasm32")]
mod component;
pub mod cpu;
pub mod machine;

#[cfg(test)]
mod tests {
    use crate::machine::Machine;

    /// The machine's `load` returns the entry; keep it so `boot()` can repeat
    /// it the way esp32sim does.
    #[test]
    fn a_fixture_image_loads_and_runs_into_a_gpio_write() {
        let image = litz_fixture();
        let mut machine = Machine::new();
        let entry = machine.load(&image).expect("the image parses");
        assert_eq!(entry, 0x4038_0000);
        assert_eq!(machine.boot(), Some(entry));
        let _ = machine.run(50_000);
        let outputs = machine.take_outputs();
        assert!(
            !outputs.is_empty(),
            "the firmware drives an LED pin within 50k instructions"
        );
    }

    /// A minimal image: enable GPIO8, drive it high forever. Built here rather
    /// than depending on `litz-fixture`, which is a host-side crate.
    fn litz_fixture() -> Vec<u8> {
        // auipc not needed: `lui` with an absolute address works because the
        // image is position-dependent.
        let program: [u32; 5] = [
            0x6000_42B7, // lui x5, 0x60004  (GPIO base)
            0x1000_0313, // addi x6, x0, 0x100
            0x0262_A223, // sw x6, 0x24(x5)  ENABLE_W1TS
            0x0062_A423, // sw x6, 0x08(x5)  OUT_W1TS
            0x0000_006F, // jal x0, 0
        ];
        let mut bytes = Vec::new();
        for word in program {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        let mut image = vec![0xE9, 1, 0, 0];
        image.extend(0x4038_0000u32.to_le_bytes());
        image.extend([0u8; 16]);
        image.extend(0x4038_0000u32.to_le_bytes());
        image.extend((bytes.len() as u32).to_le_bytes());
        image.extend(bytes);
        image
    }
}
