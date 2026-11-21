//! The two fixture programs.
//!
//! Both are position-dependent: they are assembled for [`CODE_ADDR`] and
//! loaded there, so a `lui` is enough to form an absolute address and no
//! relocation, data segment or runtime setup is needed. Register use is
//! deliberately simple and written down in one place per program so it can be
//! reviewed against the C3 register map.

use crate::encoder::{
    A0, A1, SP, T0, T1, T2, T3, T4, ZERO, addi, andi, beq, bne, jal, li, lw, srli, sw,
};
use crate::{CODE_ADDR, CYCLES_PER_MS, DRAM_TOP, GPIO_BASE, IO_MUX_BASE, UART0_BASE, image};

/// Append-only instruction buffer that knows where it will be loaded.
struct Asm {
    base: u32,
    code: Vec<u8>,
}

impl Asm {
    fn new(base: u32) -> Self {
        Self {
            base,
            code: Vec::new(),
        }
    }

    /// Address of the next instruction. Labels are just `here()` values, and
    /// every branch computes its offset from one of them in one place.
    fn here(&self) -> u32 {
        self.base + self.code.len() as u32
    }

    fn emit(&mut self, instruction: u32) {
        self.code.extend_from_slice(&instruction.to_le_bytes());
    }

    fn emit_li(&mut self, rd: u32, value: u32) {
        for instruction in li(rd, value) {
            self.emit(instruction);
        }
    }

    /// Emit `bne` to an already-known label address.
    fn bne_to(&mut self, rs1: u32, rs2: u32, target: u32) {
        let offset = i64::from(target) - i64::from(self.here());
        let offset = i32::try_from(offset).expect("branch target out of range");
        self.emit(bne(rs1, rs2, offset));
    }

    /// Emit `beq` to an already-known label address.
    fn beq_to(&mut self, rs1: u32, rs2: u32, target: u32) {
        let offset = i64::from(target) - i64::from(self.here());
        let offset = i32::try_from(offset).expect("branch target out of range");
        self.emit(beq(rs1, rs2, offset));
    }

    /// Emit `jal` to an already-known label address.
    fn jal_to(&mut self, rd: u32, target: u32) {
        let offset = i64::from(target) - i64::from(self.here());
        let offset = i32::try_from(offset).expect("jump target out of range");
        self.emit(jal(rd, offset));
    }

    fn finish(self) -> Vec<u8> {
        self.code
    }

    /// Emit a branch whose target is not known yet, and return the slot so
    /// [`Asm::patch_forward`] can fill the offset in once it is.
    fn branch_forward(&mut self, op: Op, rs1: u32, rs2: u32) -> usize {
        let slot = self.code.len();
        self.emit(match op {
            Op::Bne => bne(rs1, rs2, 0),
            Op::Beq => beq(rs1, rs2, 0),
        });
        slot
    }

    fn patch_forward(&mut self, slot: usize, op: Op, rs1: u32, rs2: u32, target: u32) {
        let from = i64::from(self.base) + slot as i64;
        let offset = i32::try_from(i64::from(target) - from).expect("branch target out of range");
        let word = match op {
            Op::Bne => bne(rs1, rs2, offset),
            Op::Beq => beq(rs1, rs2, offset),
        };
        self.code[slot..slot + 4].copy_from_slice(&word.to_le_bytes());
    }
}

#[derive(Clone, Copy)]
enum Op {
    Beq,
    Bne,
}

/// Print an ASCII string through UART0's TX FIFO.
///
/// The FIFO is register 0 and accepts one byte per 32-bit store (the low eight
/// bits are the data). The loop is unrolled: the strings are short and fixed,
/// so a string table plus a copy loop would cost more code than it saves.
fn print(asm: &mut Asm, uart: u32, text: &[u8]) {
    for &byte in text {
        asm.emit(addi(T2, ZERO, i32::from(byte)));
        asm.emit(sw(T2, uart, 0));
    }
}

/// Busy-wait for roughly `cycles` CPU cycles.
///
/// `addi` plus a taken `bne` is two cycles per iteration, so the iteration
/// count is `cycles / 2`; the division floors, losing at most one cycle. A
/// 32-bit counter holds every count a `u32` period can produce (at most
/// 2^31 - 1), so the single loop is the live path. The nested form handles a
/// wider count without silently truncating it: `outer = ceil(count / inner)`
/// with `inner` the largest 32-bit counter, so `outer * inner >= count` and
/// the delay overshoots by less than one inner loop — irrelevant for a blink
/// half-period.
fn delay(asm: &mut Asm, cycles: u64) {
    let iterations = cycles / 2;
    if iterations == 0 {
        return;
    }
    if iterations <= u64::from(u32::MAX) {
        asm.emit_li(T2, iterations as u32);
        let loop_top = asm.here();
        asm.emit(addi(T2, T2, -1));
        asm.bne_to(T2, ZERO, loop_top);
    } else {
        let inner = u64::from(u32::MAX);
        let outer = iterations.div_ceil(inner);
        asm.emit_li(T3, outer as u32);
        let outer_top = asm.here();
        asm.emit_li(T2, inner as u32);
        let inner_top = asm.here();
        asm.emit(addi(T2, T2, -1));
        asm.bne_to(T2, ZERO, inner_top);
        asm.emit(addi(T3, T3, -1));
        asm.bne_to(T3, ZERO, outer_top);
    }
}

/// Build the blinky image: configure GPIO8 as an output, drive it high and
/// low forever, and mark each edge on UART0.
///
/// `period_cycles` is the time the LED spends in each state, in CPU cycles at
/// 160 MHz; `16_000_000` is 100 ms.
///
/// Program edge cases are handled explicitly: `sp` is pointed at the top of
/// DRAM so a surprise trap has a plausible stack, the final `jal x0, 0` is a
/// never-returning spin, and a `period_cycles` of 0 or 1 simply omits the
/// delay loop.
///
/// Serial output is one banner line, then one character per edge: `+` for on
/// and `-` for off. A line per edge is the obvious choice but a free-running
/// blink would grow the capture without bound; a single character keeps an
/// unambiguous timeline and still proves the program is alive.
pub fn blinky(period_cycles: u32) -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);

    // The program never uses the stack, but `sp = 0` turns any unexpected
    // trap or call into a confusing fault loop. Match the bootloader.
    asm.emit_li(SP, DRAM_TOP);

    // IO_MUX has one 32-bit register per pad starting at +4, so GPIO8 is at
    // +0x24. Bit 12 (MCU_SEL = 1) routes the pad to the GPIO matrix; bit 9
    // (FUN_IE) enables the input buffer. 0x1200 is the value ESP-IDF writes
    // for an ordinary GPIO output.
    asm.emit_li(T0, IO_MUX_BASE);
    asm.emit_li(T1, 0x1200);
    asm.emit(sw(T1, T0, 0x24));

    // GPIO_ENABLE_W1TS at +0x24: set bit 8 to make GPIO8 a push-pull output.
    asm.emit_li(A0, GPIO_BASE);
    asm.emit(addi(T1, ZERO, 1 << 8));
    asm.emit(sw(T1, A0, 0x24));

    // UART0's TX FIFO is its first register; no configuration is needed
    // because the emulator maps the FIFO straight to the console.
    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"\r\nlitz: blinky\r\n");

    let on = asm.here();
    // GPIO_OUT_W1TS at +0x08 sets the pin; GPIO_OUT_W1TC at +0x0c clears it.
    // Both take the 1 << 8 mask already held in T1.
    asm.emit(sw(T1, A0, 0x08));
    asm.emit(addi(T2, ZERO, i32::from(b'+')));
    asm.emit(sw(T2, A1, 0));
    delay(&mut asm, u64::from(period_cycles));

    asm.emit(sw(T1, A0, 0x0c));
    asm.emit(addi(T2, ZERO, i32::from(b'-')));
    asm.emit(sw(T2, A1, 0));
    delay(&mut asm, u64::from(period_cycles));

    // A branch would do; `jal` reuses the same back-edge arithmetic as the
    // delay loop and reads as "call the top forever". Entering via `jal`
    // leaves `ra` pointing at this instruction, which is harmless.
    asm.jal_to(ZERO, on);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

/// Build the "hold low" image: configure GPIO8 as an output, drive it low and
/// leave it there, printing one line.
///
/// This is the sender half of a two-board diagram. A second board wired to
/// that pin runs the button image and sees a contact that closes at boot and
/// never opens, so one MCU's pin level is what makes the other's LED light —
/// the smallest honest proof that two chips on one diagram talk to each
/// other.
pub fn hold_low() -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);
    asm.emit_li(SP, DRAM_TOP);

    asm.emit_li(T0, IO_MUX_BASE);
    asm.emit_li(T1, 0x1200);
    asm.emit(sw(T1, T0, 0x24)); // GPIO8, routed to the GPIO matrix

    asm.emit_li(A0, GPIO_BASE);
    asm.emit(addi(T1, ZERO, 1 << 8));
    asm.emit(sw(T1, A0, 0x24)); // GPIO8 is an output
    asm.emit(sw(T1, A0, 0x0c)); // OUT_W1TC: drive it low

    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"\r\nlitz: holding GPIO8 low\r\n");

    let spin = asm.here();
    asm.jal_to(ZERO, spin);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

/// Build the button image: light the LED while GPIO0 is held low, and mark
/// each edge on UART0.
///
/// This is the interactive demo. The wiring is a pushbutton from GPIO0 to
/// ground with a pull-up resistor, and an LED on GPIO8 through a series
/// resistor, so pressing the button changes what the firmware reads and the
/// board resolves the LED's drive from that.
///
/// The program is two polling loops rather than one loop with a state flag:
/// each loop is waiting for one edge, both branch targets are known when the
/// instruction is emitted (no forward patch), and the LED write happens once
/// per edge instead of once per spin — which is what keeps the serial log to
/// one character per change.
pub fn button_led() -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);
    asm.emit_li(SP, DRAM_TOP);

    // GPIO0 is an input, GPIO8 an output; both pads need MCU_SEL = 1 (bit 12)
    // to reach the GPIO matrix, and GPIO0 alone needs the input buffer (bit 9).
    asm.emit_li(T0, IO_MUX_BASE);
    asm.emit_li(T1, 0x1200);
    asm.emit(sw(T1, T0, 0x04)); // GPIO0
    asm.emit(sw(T1, T0, 0x24)); // GPIO8

    asm.emit_li(A0, GPIO_BASE);
    asm.emit(addi(T1, ZERO, 1 << 8));
    asm.emit(sw(T1, A0, 0x24)); // GPIO8 is an output

    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"\r\nlitz: press the button\r\n");

    // Idle: the button is open, so the pull-up keeps bit 0 set. Anything that
    // is not zero keeps this loop spinning.
    let idle = asm.here();
    asm.emit(lw(T3, A0, 0x3c)); // GPIO_IN
    asm.emit(andi(T3, T3, 1)); // bit 0 is GPIO0
    asm.bne_to(T3, ZERO, idle);

    asm.emit(sw(T1, A0, 0x08)); // OUT_W1TS: LED on
    asm.emit(addi(T2, ZERO, i32::from(b'+')));
    asm.emit(sw(T2, A1, 0));

    // Held: the contact pulls the pin low, so only zero waits here.
    let held = asm.here();
    asm.emit(lw(T3, A0, 0x3c));
    asm.emit(andi(T3, T3, 1));
    asm.beq_to(T3, ZERO, held);

    asm.emit(sw(T1, A0, 0x0c)); // OUT_W1TC: LED off
    asm.emit(addi(T2, ZERO, i32::from(b'-')));
    asm.emit(sw(T2, A1, 0));
    asm.jal_to(ZERO, idle);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

/// Build the potentiometer image: read ADC1 channel 0 and light the LED while
/// the wiper is past half scale.
///
/// The ADC block is memory-mapped at `0x6004_0000` on the C3. `SARADC_CTRL2`
/// (+0x20) takes the channel in bits 25..28, the ADC unit's start bit in bit 31
/// (unit 1) or 30 (unit 2), and the trigger in bit 17; the result lands in
/// +0x2c, low twelve bits. GPIO0 is ADC1 channel 0, so the write is a constant
/// assembled once and stored on every poll.
///
/// Half scale is bit 11 of the result, which is what makes the threshold a
/// shift rather than a comparison: the wiper reaching 1.65 V on a 3.3 V track
/// lights the LED, and the two polling loops keep the serial log to one
/// character per crossing, as the button image does.
pub fn pot_led() -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);
    asm.emit_li(SP, DRAM_TOP);

    // GPIO0 becomes an analog pad: MCU_SEL = 1 routes it to the GPIO matrix,
    // and the input buffer stays off because the ADC reads the pad directly.
    asm.emit_li(T0, IO_MUX_BASE);
    asm.emit_li(T1, 0x1000);
    asm.emit(sw(T1, T0, 0x04)); // GPIO0
    asm.emit_li(T1, 0x1200);
    asm.emit(sw(T1, T0, 0x24)); // GPIO8, an ordinary output

    asm.emit_li(A0, GPIO_BASE);
    asm.emit(addi(T1, ZERO, 1 << 8));
    asm.emit(sw(T1, A0, 0x24)); // GPIO8 is an output

    // ADC block. CTRL2 holds the unit select in bit 31 (unit 1, which is
    // where channel 0 lives) and the one-shot trigger in bit 29; the trigger
    // fires on a rising edge, so every poll stores the select-only value
    // first.
    asm.emit_li(T0, 0x6004_0000);
    asm.emit_li(T3, 0x8000_0000_u32); // unit 1 selected, not triggered
    asm.emit_li(T4, 0xA000_0000_u32); // unit 1, trigger high

    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"\r\nlitz: turn the knob\r\n");

    // Below half scale: keep converting until a result sets bit 11.
    let low = asm.here();
    asm.emit(sw(T3, T0, 0x20)); // CTRL2: select, no trigger
    asm.emit(sw(T4, T0, 0x20)); // CTRL2: trigger rises
    asm.emit(lw(T2, T0, 0x2c)); // result, low twelve bits
    asm.emit(srli(T2, T2, 11)); // bit 11 becomes bit 0
    asm.emit(andi(T2, T2, 1));
    let to_on = asm.branch_forward(Op::Bne, T2, ZERO);
    // One conversion per millisecond. A conversion is a full sweep of the
    // calibration table, so polling flat out would spend the whole budget on
    // the ADC and make a run take minutes of host time.
    delay(&mut asm, u64::from(CYCLES_PER_MS));
    asm.jal_to(ZERO, low);

    let on = asm.here();
    asm.emit(sw(T1, A0, 0x08)); // OUT_W1TS: LED on
    asm.emit(addi(T2, ZERO, i32::from(b'+')));
    asm.emit(sw(T2, A1, 0));

    // Above half scale: the mirror loop, holding the LED lit.
    let high = asm.here();
    asm.emit(sw(T3, T0, 0x20));
    asm.emit(sw(T4, T0, 0x20));
    asm.emit(lw(T2, T0, 0x2c));
    asm.emit(srli(T2, T2, 11));
    asm.emit(andi(T2, T2, 1));
    let to_off = asm.branch_forward(Op::Beq, T2, ZERO);
    delay(&mut asm, u64::from(CYCLES_PER_MS));
    asm.jal_to(ZERO, high);

    let off = asm.here();
    asm.emit(sw(T1, A0, 0x0c)); // OUT_W1TC: LED off
    asm.emit(addi(T2, ZERO, i32::from(b'-')));
    asm.emit(sw(T2, A1, 0));
    asm.jal_to(ZERO, low);

    asm.patch_forward(to_on, Op::Bne, T2, ZERO, on);
    asm.patch_forward(to_off, Op::Beq, T2, ZERO, off);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

/// Build the echo image: print a banner, then send every byte that arrives on
/// UART0's receive FIFO back out of its transmitter.
///
/// This is what a serial monitor with an input box is for. UART0's status
/// register (+0x1c) carries the receive FIFO count, and reading +0 is the FIFO
/// itself — the same address that transmits when written — so the loop is
/// "is a byte waiting, take it, put it back".
pub fn echo() -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);
    asm.emit_li(SP, DRAM_TOP);
    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"\r\nlitz: echo, type a line\r\n");

    let wait = asm.here();
    asm.emit(lw(T0, A1, 0x1c)); // UART_STATUS
    asm.emit(andi(T0, T0, 0x3ff)); // receive FIFO count
    asm.beq_to(T0, ZERO, wait);
    asm.emit(lw(T1, A1, 0)); // one received byte
    asm.emit(andi(T1, T1, 0xff));
    asm.emit(sw(T1, A1, 0)); // back out
    asm.jal_to(ZERO, wait);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

/// Build the hello image: print one line over UART0 and spin forever.
///
/// Nothing is configured — not even `sp` — because this image exists to be the
/// smallest possible end-to-end smoke test: the header parses, the segment
/// copies, the core executes, and the console receives bytes.
pub fn hello() -> Vec<u8> {
    let mut asm = Asm::new(CODE_ADDR);
    asm.emit_li(A1, UART0_BASE);
    print(&mut asm, A1, b"litz: hello\r\n");
    // `jal x0, 0` is an unconditional branch to itself: a clean never-return.
    let spin = asm.here();
    asm.jal_to(ZERO, spin);

    let code = asm.finish();
    image(CODE_ADDR, &[(CODE_ADDR, &code)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{addi, bne, lui};

    /// Labels are plain addresses and branch offsets are the difference of two
    /// `here()` values. For the delay loop the `bne` sits at `loop_top + 4`,
    /// so its offset is always -4 regardless of the period:
    ///
    /// ```text
    /// loop_top: addi x7, x7, -1     ; here() == loop_top
    ///           bne  x7, x0, loop_top ; here() == loop_top + 4
    /// offset = loop_top - (loop_top + 4) = -4 bytes = one instruction back
    /// ```
    ///
    /// The decoded word is `bne(T2, ZERO, -4) == 0xFE03_9EE3`, checked in the
    /// encoder tests; this test confirms the assembler emits it.
    #[test]
    fn the_blink_delay_branches_one_instruction_back() {
        let code = blinky(16_000_000);
        let len = u32::from_le_bytes(code[28..32].try_into().unwrap()) as usize;
        let segment = &code[32..32 + len];
        let bne_word = bne(T2, ZERO, -4).to_le_bytes();
        assert!(segment.windows(4).any(|window| window == &bne_word[..]));
    }

    /// A 100 ms half-period at 160 MHz is 16,000,000 cycles, or 8,000,000
    /// loop iterations = 0x7A1200. `li` must split that into
    /// `lui x7, 0x7A1` (0x007A_13B7) followed by `addi x7, x7, 0x200`
    /// (0x2003_8393). Finding that pair proves the period reached the counter
    /// unchanged.
    #[test]
    fn the_default_period_reaches_the_counter() {
        let code = blinky(16_000_000);
        let len = u32::from_le_bytes(code[28..32].try_into().unwrap()) as usize;
        let segment = &code[32..32 + len];
        let mut needle = Vec::new();
        needle.extend(lui(T2, 0x7A1).to_le_bytes());
        needle.extend(addi(T2, T2, 0x200).to_le_bytes());
        assert!(
            segment
                .windows(needle.len())
                .any(|window| window == needle.as_slice())
        );
    }

    #[test]
    fn a_zero_period_still_assembles_a_valid_image() {
        let code = blinky(0);
        assert_eq!(code[0], 0xE9);
        assert_eq!(code[1], 1);
        assert_eq!(code.len() % 16, 0);
    }
}
