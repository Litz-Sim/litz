//! The 24C02's behaviour, free of the component boundary so it is testable on
//! the host.
//!
//! The part is a 256-byte register file with an address pointer. A write phase
//! starts by sending the pointer byte; following bytes are stored and move the
//! pointer, wrapping inside an 8-byte page exactly as the 24C02's page-write
//! buffer does. A read phase returns bytes from the pointer and moves it
//! through the whole array. The first byte of a read phase is the one at the
//! pointer left by the preceding write phase, which is what makes a register
//! read a write pointer followed by a repeated start.

/// Bytes in a 24C02.
pub const SIZE: usize = 256;
/// Bytes per write page.
pub const PAGE: usize = 8;

pub struct Eeprom {
    memory: [u8; SIZE],
    pointer: u8,
    reading: bool,
    /// True until the pointer byte of a write phase has arrived.
    first_write: bool,
    /// Bytes a UART peer sent, drained by `uart_poll`. The reference device is
    /// an I2C part; the callback exists so the same module can exercise the
    /// other bus attachments without a second artifact.
    uart_in: Vec<u8>,
}

impl Eeprom {
    pub fn new() -> Self {
        Self {
            memory: [0; SIZE],
            pointer: 0,
            reading: false,
            first_write: true,
            uart_in: Vec::new(),
        }
    }

    /// Copy the host-supplied ROM image into memory. Factory contents longer
    /// than the array are truncated; a short image leaves the rest zero.
    pub fn load_rom(&mut self, bytes: &[u8]) {
        let len = bytes.len().min(SIZE);
        self.memory[..len].copy_from_slice(&bytes[..len]);
    }

    pub fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    pub fn read(&mut self) -> u8 {
        let value = self.memory[self.pointer as usize];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    pub fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            self.memory[self.pointer as usize] = byte;
            // A page write wraps inside the current page; the rest of the
            // array keeps the bytes that were not addressed.
            let page = self.pointer & !(PAGE as u8 - 1);
            self.pointer = page | (self.pointer.wrapping_add(1) & (PAGE as u8 - 1));
        }
        true
    }

    pub fn stop(&mut self) {
        self.reading = false;
    }

    /// SPI is not the 24C02's bus. The reference implementation echoes the
    /// transfer and pads with 0xff so a board test can see the call arrive.
    pub fn spi_transfer(&mut self, tx: &[u8], rx_len: u32) -> Vec<u8> {
        let mut out = tx.to_vec();
        out.resize(rx_len as usize, 0xff);
        out
    }

    pub fn uart_receive(&mut self, bytes: &[u8]) {
        self.uart_in.extend_from_slice(bytes);
    }

    pub fn uart_poll(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart_in)
    }

    pub fn bytes(&self) -> &[u8; SIZE] {
        &self.memory
    }
}

impl Default for Eeprom {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A register read: pointer write, repeated start, then sequential bytes.
    fn read_at(part: &mut Eeprom, at: u8, len: usize) -> Vec<u8> {
        part.connect(false);
        assert!(part.write(at));
        part.stop();
        part.connect(true);
        let out = (0..len).map(|_| part.read()).collect();
        part.stop();
        out
    }

    #[test]
    fn a_write_then_read_round_trips() {
        let mut part = Eeprom::new();
        part.connect(false);
        assert!(part.write(0x10));
        assert!(part.write(0xde));
        assert!(part.write(0xad));
        part.stop();
        assert_eq!(read_at(&mut part, 0x10, 2), vec![0xde, 0xad]);
    }

    #[test]
    fn a_page_write_wraps_inside_its_page() {
        let mut part = Eeprom::new();
        part.connect(false);
        assert!(part.write(0x06));
        for byte in 0..4 {
            assert!(part.write(byte));
        }
        part.stop();
        assert_eq!(part.bytes()[0x06], 0);
        assert_eq!(part.bytes()[0x07], 1);
        assert_eq!(part.bytes()[0x00], 2);
        assert_eq!(part.bytes()[0x01], 3);
    }

    #[test]
    fn reading_runs_through_the_whole_array() {
        let mut part = Eeprom::new();
        part.connect(false);
        assert!(part.write(0xfe));
        assert!(part.write(0xaa));
        assert!(part.write(0xbb));
        part.stop();
        assert_eq!(read_at(&mut part, 0xfe, 3), vec![0xaa, 0xbb, 0x00]);
    }

    #[test]
    fn rom_seeds_the_memory() {
        let mut part = Eeprom::new();
        part.load_rom(&[1, 2, 3, 4]);
        assert_eq!(read_at(&mut part, 0, 4), vec![1, 2, 3, 4]);
    }
}
