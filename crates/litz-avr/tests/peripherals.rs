//! Behaviour tests for the extended ATmega328P peripherals. Each test runs a
//! hand-assembled program through the core so the register protocol, the
//! instruction stream and the peripheral model are exercised together.

use std::cell::RefCell;
use std::rc::Rc;

use litz_avr::peripherals::addr;
use litz_avr::{Machine, PORT_D, Stop, vector};

// -- a tiny assembler ------------------------------------------------------

const SEI: u16 = 0x9478;
const RETI: u16 = 0x9518;
const RJMP_SELF: u16 = 0xcfff;

/// `ldi Rd, K` for `d` in 16..=31.
fn ldi(d: u8, k: u8) -> u16 {
    debug_assert!((16..=31).contains(&d));
    0xe000 | ((u16::from(k) & 0x00f0) << 4) | ((u16::from(d) - 16) << 4) | (u16::from(k) & 0x000f)
}

/// `lds Rd, k`, the two-word absolute load.
fn lds(d: u8, k: u16) -> [u16; 2] {
    [0x9000 | (u16::from(d) << 4), k]
}

/// `sts k, Rr`, the two-word absolute store.
fn sts(r: u8, k: u16) -> [u16; 2] {
    [0x9200 | (u16::from(r) << 4), k]
}

/// `sbi A, b`, set a bit in the low I/O space.
fn sbi(address: u16, bit: u8) -> u16 {
    0x9a00 | (((address - 0x20) & 0x1f) << 3) | u16::from(bit)
}

/// `sbrc Rr, b`, skip the next word when the bit is clear.
fn sbrc(r: u8, bit: u8) -> u16 {
    0xfc00 | (u16::from(r) << 4) | u16::from(bit)
}

/// `sbrs Rr, b`, skip the next word when the bit is set.
fn sbrs(r: u8, bit: u8) -> u16 {
    0xfe00 | (u16::from(r) << 4) | u16::from(bit)
}

/// `rjmp` with a word offset from the following instruction.
fn rjmp(offset: i16) -> u16 {
    0xc000 | ((offset as u16) & 0x0fff)
}

fn load_words(machine: &mut Machine, word: usize, program: &[u16]) {
    let mut bytes = Vec::with_capacity(program.len() * 2);
    for instruction in program {
        bytes.extend_from_slice(&instruction.to_le_bytes());
    }
    machine.load_bin(&bytes, (word * 2) as u64).unwrap();
}

fn load_vector(machine: &mut Machine, vector_number: u8, program: &[u16]) {
    load_words(
        machine,
        usize::from(vector::word_address(vector_number)),
        program,
    );
}

fn word_at(machine: &Machine, address: usize) -> u16 {
    u16::from_le_bytes([machine.sram()[address], machine.sram()[address + 1]])
}

// -- timer interrupts ------------------------------------------------------

#[test]
fn a_timer0_overflow_interrupt_calls_the_vector() {
    let mut machine = Machine::new();
    // sei ; rjmp .
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    // Handler: ldi r16, 1 ; sts 0x0100, r16 ; reti
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, vector::TIMER0_OVF, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer0.tccr_b = 0x01; // no prescale
        p.timer0.count = 0xfe; // overflow two ticks in
        p.timer0.timsk = 0x01; // TOIE0
    }

    assert_eq!(machine.run(100), Stop::Budget);

    assert_eq!(machine.cpu().reg(16), 1, "the ISR ran");
    assert_eq!(machine.sram()[0], 1, "the ISR stored its mark");
    assert_eq!(
        machine.peripherals().timer0.tifr & 0x01,
        0,
        "vector entry cleared TOV0"
    );
}

#[test]
fn millis_advances_through_timer0_overflow_isr() {
    let mut machine = Machine::new();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    // A 16-bit millisecond-style counter at 0x0100: load, increment, store.
    let mut handler = Vec::new();
    handler.extend_from_slice(&lds(24, 0x0100));
    handler.extend_from_slice(&lds(25, 0x0101));
    handler.push(0x9601); // adiw r24, 1
    handler.extend_from_slice(&sts(24, 0x0100));
    handler.extend_from_slice(&sts(25, 0x0101));
    handler.push(RETI);
    load_vector(&mut machine, vector::TIMER0_OVF, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        // Arduino's Timer0 setup: fast PWM, TOP = 0xFF, prescaler 64.
        p.timer0.tccr_a = 0x03;
        p.timer0.tccr_b = 0x03;
        p.timer0.timsk = 0x01;
    }

    machine.run(60_000);

    let ticks = word_at(&machine, 0);
    assert!(
        ticks >= 2,
        "two overflow periods of 16 384 cycles fit in the budget, got {ticks}"
    );
    assert!(
        machine.peripherals().timer0.overflows >= 2,
        "the peripheral counted the same overflows"
    );
}

#[test]
fn analog_write_sets_a_duty_cycle_on_oc0a() {
    let mut machine = Machine::new();
    let mut program = vec![sbi(addr::DDRD, 6)]; // pinMode(6, OUTPUT)
    program.push(ldi(16, 128));
    program.extend_from_slice(&sts(16, addr::OCR0A));
    program.push(ldi(16, 0x83)); // COM0A = clear on match, WGM = fast PWM
    program.extend_from_slice(&sts(16, addr::TCCR0A));
    program.push(ldi(16, 0x03)); // prescaler 64
    program.extend_from_slice(&sts(16, addr::TCCR0B));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.run(40); // finish the setup and enter the idle loop

    let start = machine.cycles();
    let mut high = 0u64;
    let mut total = 0u64;
    while machine.cycles() - start < 60_000 {
        let before = machine.cycles();
        machine.step().expect("the setup loop is valid");
        let elapsed = machine.cycles() - before;
        if elapsed == 0 {
            break;
        }
        if machine.peripherals().gpio.driven_level(PORT_D, 6) == Some(true) {
            high += elapsed;
        }
        total += elapsed;
    }

    let duty = high as f64 / total as f64;
    assert!(
        (0.45..=0.55).contains(&duty),
        "OCR0A = 128 gives about a 50% duty on D6, measured {duty}"
    );

    let edges = machine.take_outputs();
    assert!(
        edges.contains(&(6, true)) && edges.contains(&(6, false)),
        "the compare output drives D6 in both directions: {edges:?}"
    );
}

#[test]
fn a_pin_change_interrupt_reaches_pcint0() {
    let mut machine = Machine::new();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, vector::PCINT0, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.pcmsk[0] = 0x01; // watch PB0, Arduino D8
        p.pcicr = 0x01; // enable PCINT0
    }
    machine.set_input(8, true);

    assert!(
        machine.peripherals().pcifr & 0x01 != 0,
        "the masked host edge set PCIF0"
    );
    assert_eq!(machine.run(30), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "the PCINT0 ISR ran");
    assert_eq!(
        machine.peripherals().pcifr & 0x01,
        0,
        "vector entry cleared PCIF0"
    );
}

#[test]
fn timer1_captures_icp1_on_a_rising_edge() {
    let mut machine = Machine::new();
    load_words(&mut machine, 0, &[RJMP_SELF]);
    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer1.tccr_b = 0x41; // ICES1: capture on rising edge, no prescale
        p.timer1.count = 0x1234;
    }
    machine.run(10);
    let before = machine.peripherals().timer1.count;

    machine.set_input(8, true); // PB0 = ICP1
    let captured = machine.peripherals().timer1.icr;
    assert!(
        machine.peripherals().timer1.tifr & 0x20 != 0,
        "ICF1 rose on the edge"
    );
    assert!(
        captured >= before,
        "ICR1 latched the running counter ({captured} >= {before})"
    );

    machine.set_input(8, false);
    assert_eq!(
        machine.peripherals().timer1.icr,
        captured,
        "a falling edge is ignored while ICES1 selects rising"
    );
}

#[test]
fn a_timer_wakes_the_core_from_sleep() {
    let mut machine = Machine::new();
    // sei ; sleep ; rjmp .
    load_words(&mut machine, 0, &[SEI, 0x9588, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, vector::TIMER0_OVF, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer0.tccr_b = 0x01;
        p.timer0.count = 0xfe;
        p.timer0.timsk = 0x01;
    }

    assert_eq!(machine.run(500), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "the overflow interrupted the sleep");
}

// -- ADC -------------------------------------------------------------------

#[test]
fn an_adc_conversion_returns_the_injected_voltage() {
    let mut machine = Machine::new();
    let mut program = vec![ldi(16, 0x40)]; // AVcc reference, ADC0
    program.extend_from_slice(&sts(16, addr::ADMUX));
    program.push(ldi(16, 0xc7)); // ADEN | ADSC | /128
    program.extend_from_slice(&sts(16, addr::ADCSRA));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::ADCSRA)); // wait for ADSC to clear
    program.push(sbrc(17, 6));
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.extend_from_slice(&lds(18, addr::ADCL));
    program.extend_from_slice(&lds(19, addr::ADCH));
    program.extend_from_slice(&sts(18, 0x0100));
    program.extend_from_slice(&sts(19, 0x0101));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.set_analog_input(0, 2_500_000); // 2.5 V

    assert_eq!(machine.run(5_000), Stop::Budget);

    // 2.5 V against a 5 V reference is 512 counts: ADCL 0x00, ADCH 0x02.
    assert_eq!(machine.sram()[0], 0x00);
    assert_eq!(machine.sram()[1], 0x02);
    assert_eq!(word_at(&machine, 0), 512);
}

// -- SPI -------------------------------------------------------------------

#[test]
fn an_spi_transfer_sends_and_receives_a_byte() {
    let mut machine = Machine::new();
    let mut program = vec![ldi(16, 0x50)]; // SPE | MSTR | /4
    program.extend_from_slice(&sts(16, addr::SPCR));
    program.push(ldi(16, 0xa5));
    program.extend_from_slice(&sts(16, addr::SPDR));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::SPSR));
    program.push(sbrs(17, 7)); // skip the loop while SPIF is set
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.extend_from_slice(&lds(18, addr::SPDR));
    program.extend_from_slice(&sts(18, 0x0100));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    machine.set_spi_callback(Box::new(move |cs, tx| {
        sink.borrow_mut().push((cs, tx));
        0x5a
    }));

    machine.boot();
    assert_eq!(machine.run(500), Stop::Budget);

    assert_eq!(
        seen.borrow().as_slice(),
        [(10, 0xa5)],
        "the callback saw the default SS pin and the transmitted byte"
    );
    assert_eq!(machine.sram()[0], 0x5a, "the received byte reached the CPU");
    assert_eq!(
        machine.peripherals_mut().read(addr::SPSR) & 0x80,
        0,
        "reading SPDR cleared SPIF"
    );
}

// -- TWI -------------------------------------------------------------------

#[test]
fn a_wire_write_reaches_the_i2c_callback() {
    let mut machine = Machine::new();
    let mut program = Vec::new();
    program.push(ldi(16, 0xa4)); // TWINT | TWSTA | TWEN: START
    program.extend_from_slice(&sts(16, addr::TWCR));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::TWCR));
    program.push(sbrs(17, 7));
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));

    program.push(ldi(16, 0x84)); // SLA+W, address 0x42
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84)); // TWINT | TWEN: transmit
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(ldi(16, b'h'));
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84));
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(ldi(16, b'i'));
    program.extend_from_slice(&sts(16, addr::TWDR));
    program.push(ldi(16, 0x84));
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(ldi(16, 0x94)); // TWINT | TWSTO | TWEN: STOP
    program.extend_from_slice(&sts(16, addr::TWCR));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    let seen = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&seen);
    machine.set_i2c_write_callback(Box::new(move |address, bytes| {
        *sink.borrow_mut() = Some((address, bytes.to_vec()));
    }));

    machine.boot();
    assert_eq!(machine.run(300), Stop::Budget);

    let seen = seen.borrow();
    let (address, bytes) = seen.as_ref().expect("the STOP completed the write");
    assert_eq!(*address, 0x42);
    assert_eq!(bytes.as_slice(), b"hi");
}

// -- EEPROM ----------------------------------------------------------------

#[test]
fn an_eeprom_byte_survives_a_read_after_the_write_time() {
    let mut machine = Machine::new();
    let mut program = vec![ldi(16, 0x00)];
    program.extend_from_slice(&sts(16, addr::EEARH));
    program.push(ldi(16, 0x10));
    program.extend_from_slice(&sts(16, addr::EEARL));
    program.push(ldi(16, 0x5a));
    program.extend_from_slice(&sts(16, addr::EEDR));
    program.push(ldi(16, 0x04)); // EEMPE
    program.extend_from_slice(&sts(16, addr::EECR));
    program.push(ldi(16, 0x02)); // EEPE
    program.extend_from_slice(&sts(16, addr::EECR));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::EECR));
    program.push(sbrc(17, 1)); // loop while EEPE is set
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.push(ldi(16, 0x01)); // EERE
    program.extend_from_slice(&sts(16, addr::EECR));
    program.extend_from_slice(&lds(18, addr::EEDR));
    program.extend_from_slice(&sts(18, 0x0100));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.run(500);
    assert!(
        machine.peripherals().eeprom.busy(),
        "EEPE stays set for the 3.4 ms write"
    );
    assert_eq!(
        machine.peripherals().eeprom.memory()[0x10],
        0xff,
        "the cell still holds its erased value"
    );

    machine.run(200_000);

    assert_eq!(
        machine.peripherals().eeprom.memory()[0x10],
        0x5a,
        "the write completed"
    );
    assert_eq!(machine.sram()[0], 0x5a, "the CPU read the byte back");
}

// -- USART receive ---------------------------------------------------------

#[test]
fn a_usart_rx_byte_sets_rxc_and_interrupts() {
    let mut machine = Machine::new();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = Vec::new();
    handler.extend_from_slice(&lds(16, addr::UDR0));
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, vector::USART_RX, &handler);

    machine.boot();
    machine.peripherals_mut().usart0.ucsr_b = 0x80; // RXCIE0
    machine.write_console(b"Z");

    assert!(
        machine.peripherals().usart0.rxc(),
        "RXC0 follows the receive queue"
    );
    assert_eq!(machine.run(50), Stop::Budget);

    assert_eq!(machine.sram()[0], b'Z', "the RX ISR read UDR0");
    assert!(
        !machine.peripherals().usart0.rxc(),
        "reading UDR0 drained the queue and cleared RXC0"
    );
    assert_eq!(
        machine.peripherals_mut().read(addr::UCSR0A) & 0x80,
        0,
        "UCSR0A no longer reports a waiting byte"
    );
}
