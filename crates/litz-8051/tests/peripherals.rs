//! Timers, the serial port and the interrupt controller at datasheet cycle
//! counts.

mod common;

use common::Asm;
use litz_8051::{INT0_GPIO, Machine, T0_GPIO, T2_GPIO, T2EX_GPIO, ie, scon, sfr, t2con, tcon};

/// A machine whose code is NOPs, booted, so a test can program an SFR and
/// spend known machine cycles.
fn nop_machine() -> Machine {
    let mut machine = Machine::new();
    machine.load_bin(&vec![0x00; 1024], 0).unwrap();
    machine.boot();
    machine
}

fn tf0(machine: &Machine) -> bool {
    machine.read_sfr(sfr::TCON) & tcon::TF0 != 0
}

fn tf1(machine: &Machine) -> bool {
    machine.read_sfr(sfr::TCON) & tcon::TF1 != 0
}

fn tf2(machine: &Machine) -> bool {
    machine.read_sfr(sfr::T2CON) & t2con::TF2 != 0
}

#[test]
fn timer0_overflows_two_machine_cycles_after_ffff() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TMOD, 0x01); // Timer 0, mode 1 (16-bit)
    machine.write_sfr(sfr::TH0, 0xFF);
    machine.write_sfr(sfr::TL0, 0xFE);
    machine.write_sfr(sfr::TCON, tcon::TR0);

    machine.step().unwrap(); // 0xfffe -> 0xffff
    assert!(!tf0(&machine), "one cycle only reaches 0xffff");
    machine.step().unwrap(); // 0xffff -> overflow
    assert!(tf0(&machine), "the second machine cycle overflows");
}

#[test]
fn timer0_mode2_reloads_tl0_from_th0() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TMOD, 0x02); // 8-bit auto-reload
    machine.write_sfr(sfr::TH0, 0xFE);
    machine.write_sfr(sfr::TL0, 0xFF);
    machine.write_sfr(sfr::TCON, tcon::TR0);

    machine.step().unwrap();
    assert!(tf0(&machine));
    assert_eq!(machine.read_sfr(sfr::TL0), 0xFE);
}

#[test]
fn timer0_counts_falling_edges_in_counter_mode() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TMOD, 0x05); // Timer 0, mode 1, C/T = 1
    machine.write_sfr(sfr::TH0, 0);
    machine.write_sfr(sfr::TL0, 0);
    machine.write_sfr(sfr::TCON, tcon::TR0);

    machine.set_input(T0_GPIO, true);
    machine.set_input(T0_GPIO, false);
    assert_eq!(machine.read_sfr(sfr::TL0), 1);
    machine.set_input(T0_GPIO, true);
    machine.set_input(T0_GPIO, false);
    assert_eq!(machine.read_sfr(sfr::TL0), 2);

    machine.set_input(T0_GPIO, true);
    machine.set_input(T0_GPIO, false);
    machine.set_input(T0_GPIO, true);
    machine.set_input(T0_GPIO, false);
    assert_eq!(machine.read_sfr(sfr::TL0), 4);
}

#[test]
fn a_timer0_interrupt_vectors_and_clears_tf0() {
    let mut asm = Asm::new();
    asm.org(0x0000);
    asm.ljmp("main");
    asm.org(0x000B);
    asm.ljmp("t0isr");
    asm.org(0x0030);
    asm.label("main");
    asm.mov_direct(sfr::TMOD, 0x01);
    asm.mov_direct(sfr::TH0, 0xFF);
    asm.mov_direct(sfr::TL0, 0xFE);
    asm.mov_direct(sfr::IE, ie::EA | ie::ET0);
    asm.setb(0x8C); // TCON.4 = TR0
    asm.label("spin");
    asm.sjmp("spin");
    asm.org(0x0100);
    asm.label("t0isr");
    asm.setb(0x90); // P1.0
    asm.clr_bit(0x8C); // stop the timer so TF0 stays clear
    asm.reti();

    let mut machine = Machine::new();
    machine.load_bin(&asm.finish(), 0).unwrap();
    machine.boot();
    machine.run(2_000);

    assert_eq!(
        machine.read_sfr(sfr::P1) & 0x01,
        0x01,
        "the ISR ran and set P1.0"
    );
    assert!(!tf0(&machine), "hardware cleared TF0 on entry");
}

#[test]
fn the_high_priority_source_is_serviced_first() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::IE, ie::EA | ie::ET0 | ie::ET1);
    machine.write_sfr(sfr::TCON, tcon::TF0 | tcon::TF1);
    machine.step().unwrap();
    assert_eq!(
        machine.pc(),
        0x000B,
        "same priority polls Timer 0 before Timer 1"
    );

    let mut machine = nop_machine();
    machine.write_sfr(sfr::IE, ie::EA | ie::ET0 | ie::ET1);
    machine.write_sfr(sfr::IP, 0x08); // PT1: Timer 1 is high priority
    machine.write_sfr(sfr::TCON, tcon::TF0 | tcon::TF1);
    machine.step().unwrap();
    assert_eq!(
        machine.pc(),
        0x001B,
        "the high-priority Timer 1 wins even though Timer 0 polls first"
    );
}

#[test]
fn int0_level_mode_follows_the_pin_and_edge_mode_latches() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TCON, 0); // IT0 = 0: low level
    machine.set_input(INT0_GPIO, false);
    assert_ne!(machine.read_sfr(sfr::TCON) & tcon::IE0, 0);
    machine.set_input(INT0_GPIO, true);
    assert_eq!(machine.read_sfr(sfr::TCON) & tcon::IE0, 0);

    machine.write_sfr(sfr::TCON, tcon::IT0); // falling edge
    machine.set_input(INT0_GPIO, false);
    assert_ne!(machine.read_sfr(sfr::TCON) & tcon::IE0, 0);
    machine.set_input(INT0_GPIO, true);
    assert_ne!(
        machine.read_sfr(sfr::TCON) & tcon::IE0,
        0,
        "the edge flag is cleared only by entry or software"
    );
}

#[test]
fn int0_services_its_vector_on_a_falling_edge() {
    let mut asm = Asm::new();
    asm.org(0x0000);
    asm.ljmp("main");
    asm.org(0x0003);
    asm.ljmp("int0");
    asm.org(0x0030);
    asm.label("main");
    asm.mov_direct(sfr::TCON, tcon::IT0);
    asm.mov_direct(sfr::IE, ie::EA | ie::EX0);
    asm.label("spin");
    asm.sjmp("spin");
    asm.org(0x0100);
    asm.label("int0");
    asm.setb(0x90); // P1.0
    asm.reti();

    let mut machine = Machine::new();
    machine.load_bin(&asm.finish(), 0).unwrap();
    machine.boot();
    // Run the setup, then make INT0 fall.
    for _ in 0..4 {
        machine.step().unwrap();
    }
    machine.set_input(INT0_GPIO, false);
    machine.run(500);

    assert_eq!(machine.read_sfr(sfr::P1) & 0x01, 0x01);
    assert_eq!(
        machine.read_sfr(sfr::TCON) & tcon::IE0,
        0,
        "hardware cleared the edge flag on entry"
    );
}

#[test]
fn uart_transmit_waits_for_the_frame_time() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TMOD, 0x20); // Timer 1, mode 2
    machine.write_sfr(sfr::TH1, 0xFD); // 9600 baud at 11.0592 MHz
    machine.write_sfr(sfr::SCON, 0x40); // mode 1
    machine.write_sfr(sfr::TCON, tcon::TR1);
    machine.write_sfr(sfr::SBUF, b'A');

    // 9600 baud at 11.0592 MHz is 1152 oscillator clocks per bit; a mode 1
    // frame is ten bits.
    machine.run(6_000);
    assert!(
        machine.take_console().is_empty(),
        "five bits in, the stop bit has not finished"
    );
    machine.run(10_000);
    assert_eq!(machine.take_console(), b"A");
    assert_ne!(machine.read_sfr(sfr::SCON) & scon::TI, 0);
}

#[test]
fn uart_mode0_shifts_eight_bits_at_fosc_over_12() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::SCON, 0x00); // mode 0
    machine.write_sfr(sfr::SBUF, b'C');

    machine.run(80); // six and a half bits
    assert!(machine.take_console().is_empty());
    machine.run(40);
    assert_eq!(machine.take_console(), b"C");
}

#[test]
fn uart_mode2_uses_the_fixed_fosc_over_64_rate() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::SCON, scon::SM0); // mode 2, SMOD = 0
    machine.write_sfr(sfr::SBUF, b'B');

    machine.run(600); // eight and a bit of eleven bits
    assert!(machine.take_console().is_empty());
    machine.run(200);
    assert_eq!(machine.take_console(), b"B");
}

#[test]
fn uart_receives_a_host_byte_into_sbuf() {
    let mut asm = Asm::new();
    asm.label("main");
    asm.mov_direct(sfr::TMOD, 0x20);
    asm.mov_direct(sfr::TH1, 0xFD);
    asm.mov_direct(sfr::SCON, 0x50); // mode 1, REN
    asm.setb(0x8E); // TR1
    asm.label("poll");
    asm.jnb(0x98, "poll"); // SCON.0 = RI
    asm.mov_a_direct(sfr::SBUF);
    asm.mov_direct_a(sfr::P1);
    asm.clr_bit(0x98); // CLR RI
    asm.label("done");
    asm.sjmp("done");

    let mut machine = Machine::new();
    machine.load_bin(&asm.finish(), 0).unwrap();
    machine.boot();
    machine.write_console(b"Z");
    machine.run(50_000);

    assert_eq!(
        machine.read_sfr(sfr::P1),
        0x5A,
        "SBUF reached P1 through the firmware"
    );
    assert_eq!(machine.read_sfr(sfr::SCON) & scon::RI, 0);
}

#[test]
fn timer2_auto_reloads_and_sets_tf2() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::T2CON, t2con::TR2);
    machine.write_sfr(sfr::RCAP2H, 0xFF);
    machine.write_sfr(sfr::RCAP2L, 0xFE);
    machine.write_sfr(sfr::TH2, 0xFF);
    machine.write_sfr(sfr::TL2, 0xFE);

    machine.step().unwrap(); // 0xfffe -> 0xffff
    assert!(!tf2(&machine));
    machine.step().unwrap(); // overflow, reload
    assert!(tf2(&machine));
    assert_eq!(machine.read_sfr(sfr::TH2), 0xFF);
    assert_eq!(machine.read_sfr(sfr::TL2), 0xFE);
}

#[test]
fn timer2_baud_mode_overflows_without_raising_tf2() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::T2CON, t2con::TR2 | t2con::TCLK);
    machine.write_sfr(sfr::RCAP2H, 0xFF);
    machine.write_sfr(sfr::RCAP2L, 0xFF);
    machine.write_sfr(sfr::TH2, 0xFF);
    machine.write_sfr(sfr::TL2, 0xFF);

    machine.step().unwrap();
    assert!(
        !tf2(&machine),
        "the baud generator clocks the UART, not TF2"
    );
    assert_eq!(machine.read_sfr(sfr::TL2), 0xFF);
}

#[test]
fn timer2_captures_on_t2ex() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::T2CON, t2con::EXEN2 | t2con::CPRL2);
    machine.write_sfr(sfr::TH2, 0x12);
    machine.write_sfr(sfr::TL2, 0x34);

    machine.set_input(T2EX_GPIO, true);
    machine.set_input(T2EX_GPIO, false);

    assert_eq!(machine.read_sfr(sfr::RCAP2H), 0x12);
    assert_eq!(machine.read_sfr(sfr::RCAP2L), 0x34);
    assert_ne!(machine.read_sfr(sfr::T2CON) & t2con::EXF2, 0);
}

#[test]
fn timer2_counts_falling_edges_on_t2() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::T2CON, t2con::CT2 | t2con::TR2);
    machine.set_input(T2_GPIO, true);
    machine.set_input(T2_GPIO, false);
    assert_eq!(machine.read_sfr(sfr::TL2), 1);
    machine.set_input(T2_GPIO, true);
    machine.set_input(T2_GPIO, false);
    assert_eq!(machine.read_sfr(sfr::TL2), 2);
}

#[test]
fn timer1_mode2_is_the_uart_clock_source() {
    let mut machine = nop_machine();
    machine.write_sfr(sfr::TMOD, 0x20);
    machine.write_sfr(sfr::TH1, 0xFD);
    machine.write_sfr(sfr::TL1, 0xFD);
    machine.write_sfr(sfr::TCON, tcon::TR1);

    // 0xfd -> 0xfe -> 0xff -> reload 0xfd, with TF1 set on the overflow.
    machine.step().unwrap();
    assert!(!tf1(&machine));
    machine.step().unwrap();
    assert!(!tf1(&machine));
    machine.step().unwrap();
    assert!(tf1(&machine));
    assert_eq!(machine.read_sfr(sfr::TL1), 0xFD);
}
