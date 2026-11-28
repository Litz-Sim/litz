//! Behaviour of the three parts added beyond the ATmega328P: per-device
//! instruction sets, a timer and a serial path on each, a port only that part
//! has, vector addressing from the part's own table, and determinism.

use litz_avr::peripherals::addr;
use litz_avr::{ATMEGA32U4, ATMEGA2560, ATTINY85, Device, Machine, Stop};

const SEI: u16 = 0x9478;
const RET: u16 = 0x9508;
const RETI: u16 = 0x9518;
const RJMP_SELF: u16 = 0xcfff;

fn ldi(d: u8, k: u8) -> u16 {
    0xe000 | ((u16::from(k) & 0x00f0) << 4) | ((u16::from(d) - 16) << 4) | (u16::from(k) & 0x000f)
}

fn lds(d: u8, k: u16) -> [u16; 2] {
    [0x9000 | (u16::from(d) << 4), k]
}

fn sts(r: u8, k: u16) -> [u16; 2] {
    [0x9200 | (u16::from(r) << 4), k]
}

fn rjmp(offset: i16) -> u16 {
    0xc000 | ((offset as u16) & 0x0fff)
}

fn rcall(offset: i16) -> u16 {
    0xd000 | ((offset as u16) & 0x0fff)
}

/// `jmp`/`call` with a 22-bit word target.
fn absolute(base: u16, target: u32) -> [u16; 2] {
    let high = target >> 16;
    let first = base | (((high as u16) & 0x1f) << 4) | ((high >> 5) as u16 & 0x0001);
    [first, (target & 0xffff) as u16]
}

fn load_words(machine: &mut Machine, byte: usize, program: &[u16]) {
    let mut bytes = Vec::with_capacity(program.len() * 2);
    for instruction in program {
        bytes.extend_from_slice(&instruction.to_le_bytes());
    }
    machine.load_bin(&bytes, byte as u64).unwrap();
}

fn load_vector(machine: &mut Machine, device: &Device, vector: u8, program: &[u16]) {
    let byte = usize::from(vector) * usize::from(device.vector_words) * 2;
    load_words(machine, byte, program);
}

// -- ATmega2560 ------------------------------------------------------------

#[test]
fn atmega2560_has_its_own_memory_map() {
    let machine = Machine::for_device(&ATMEGA2560);
    assert_eq!(machine.flash().len(), 256 * 1024);
    assert_eq!(machine.sram().len(), 8 * 1024);
    assert_eq!(machine.peripherals().eeprom.size(), 4 * 1024);
    assert_eq!(machine.device().ramend, 0x21ff);
}

#[test]
fn atmega2560_jmp_reaches_the_upper_flash() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let mut program = Vec::new();
    program.extend_from_slice(&absolute(0x940c, 0x8000));
    load_words(&mut machine, 0, &program);
    load_words(&mut machine, 0x10000, &[ldi(16, 0x42), RJMP_SELF]);

    machine.boot();
    assert_eq!(machine.run(20), Stop::Budget);
    assert_eq!(machine.cpu().reg(16), 0x42, "JMP reached word 0x8000");
}

#[test]
fn atmega2560_eind_supplies_bit_sixteen_for_eijmp() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let mut program = vec![ldi(30, 0x00), ldi(31, 0x80), ldi(16, 1)];
    program.extend_from_slice(&sts(16, 0x5c)); // EIND = 1
    program.push(0x9419); // eijmp
    load_words(&mut machine, 0, &program);
    load_words(&mut machine, 3 * 0x10000, &[ldi(16, 0x5a), RJMP_SELF]);

    machine.boot();
    assert_eq!(machine.run(30), Stop::Budget);
    assert_eq!(machine.cpu().reg(16), 0x5a, "EIND:Z reached word 0x18000");
}

#[test]
fn atmega2560_elpm_reads_through_rampz() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let mut program = vec![ldi(30, 0x00), ldi(31, 0x00), ldi(16, 1)];
    program.extend_from_slice(&sts(16, 0x5b)); // RAMPZ = 1
    program.push(0x9116); // elpm r17, Z
    program.extend_from_slice(&sts(17, 0x0200));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);
    machine.load_bin(&[0x37], 0x10000).unwrap();

    machine.boot();
    assert_eq!(machine.run(40), Stop::Budget);
    assert_eq!(machine.sram()[0], 0x37, "ELPM read byte 0x10000");
}

#[test]
fn atmega2560_call_pushes_three_address_bytes_and_returns() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let mut program = Vec::new();
    program.extend_from_slice(&absolute(0x940e, 0x8000));
    program.push(ldi(17, 2));
    program.extend_from_slice(&sts(17, 0x0200));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);
    load_words(&mut machine, 0x10000, &[ldi(16, 7), RET]);

    machine.boot();
    let before = machine.cpu().sp();
    assert_eq!(before, 0x21ff);
    assert_eq!(machine.run(60), Stop::Budget);
    assert_eq!(machine.cpu().reg(16), 7, "the subroutine ran");
    assert_eq!(machine.sram()[0], 2, "RET came back to the caller");
    assert_eq!(machine.cpu().sp(), before, "all three return bytes popped");
}

#[test]
fn atmega2560_timer3_uses_its_own_vector() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let device = machine.device();
    let vector = device.timer(3).unwrap().vectors.overflow.unwrap();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0200));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer3.tccr_b = 0x01;
        p.timer3.count = 0xfffe;
        p.timer3.timsk = 0x01; // TOIE3
    }
    assert_eq!(machine.run(200), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "TIMER3_OVF was taken");
    assert_eq!(machine.peripherals().timer3.tifr & 0x01, 0);
    assert!(machine.peripherals().timer3.overflows >= 1);
}

#[test]
fn atmega2560_drives_port_l() {
    let mut machine = Machine::for_device(&ATMEGA2560);
    let mut program = vec![ldi(16, 0x80)];
    program.extend_from_slice(&sts(16, 0x10a)); // DDRL
    program.extend_from_slice(&sts(16, 0x10b)); // PORTL
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.run(20);
    assert!(
        machine.take_outputs().contains(&(87, true)),
        "PL7 is host pin 87"
    );
}

// -- ATtiny85 --------------------------------------------------------------

#[test]
fn attiny85_has_its_own_memory_map() {
    let machine = Machine::for_device(&ATTINY85);
    assert_eq!(machine.flash().len(), 8 * 1024);
    assert_eq!(machine.sram().len(), 512);
    assert_eq!(machine.peripherals().eeprom.size(), 512);
    assert_eq!(machine.device().ramend, 0x025f);
    assert_eq!(machine.device().port_of_pin(5), Some((0, 5)));
    assert_eq!(machine.device().port_of_pin(6), None, "PB6 is not bonded");
}

#[test]
fn attiny85_traps_on_mul() {
    let mut machine = Machine::for_device(&ATTINY85);
    let program = [ldi(16, 3), ldi(17, 4), 0x9d11, RJMP_SELF]; // mul r16, r17
    load_words(&mut machine, 0, &program);
    machine.boot();
    assert_eq!(machine.run(20), Stop::Trap);
    assert!(machine.fault().is_some(), "the reduced core has no MUL");
}

#[test]
fn attiny85_traps_on_absolute_jmp() {
    let mut machine = Machine::for_device(&ATTINY85);
    let program = [0x940c, 0x0004, RJMP_SELF];
    load_words(&mut machine, 0, &program);
    machine.boot();
    assert_eq!(machine.run(20), Stop::Trap);
}

#[test]
fn attiny85_rjmp_and_rcall_still_work() {
    let mut machine = Machine::for_device(&ATTINY85);
    let program = [
        rcall(2),   // word 0: call word 3
        RJMP_SELF,  // word 1
        RJMP_SELF,  // word 2
        ldi(16, 9), // word 3
        RET,        // word 4
    ];
    load_words(&mut machine, 0, &program);
    machine.boot();
    machine.run(40);
    assert_eq!(machine.cpu().reg(16), 9);
    assert_eq!(machine.cpu().sp(), 0x025f, "the return address popped");
}

#[test]
fn attiny85_timer1_counts_to_ocr1c() {
    let mut machine = Machine::for_device(&ATTINY85);
    let device = machine.device();
    let spec = device.timer(1).unwrap();
    let vector = spec.vectors.overflow.unwrap();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0060));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer1.tccr_a = 0x81; // CTC1, prescaler /1
        p.timer1.ocr_c = 0x10; // OCR1C is TOP
        p.timer1.count = 0x0f;
        // TIMSK is shared: one write at 0x59 enables both timers' bits.
        p.write(0x59, spec.masks.overflow);
    }
    assert_eq!(machine.run(100), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "TIMER1_OVF was taken at TOP");
    assert!(machine.peripherals().timer1.overflows >= 1);
}

#[test]
fn attiny85_usi_overflow_shifts_and_interrupts() {
    let mut machine = Machine::for_device(&ATTINY85);
    let device = machine.device();
    let vector = device.vectors.usi_overflow.unwrap();
    let mut program = vec![SEI, ldi(16, 0x41)]; // USIOIE | USITC
    program.extend_from_slice(&sts(16, addr::USICR));
    for _ in 0..16 {
        program.extend_from_slice(&sts(16, addr::USICR));
    }
    program.push(RJMP_SELF);
    // The tiny85's one-word vector table reaches word 14, so the reset vector
    // jumps past it before the strobe loop starts.
    let main = 32;
    load_words(&mut machine, 0, &[rjmp((main - 1) as i16)]);
    load_words(&mut machine, main * 2, &program);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0060));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    assert_eq!(machine.run(300), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "USI_OVF was taken after 16 edges");
    assert_eq!(
        machine.peripherals_mut().read(addr::USISR) & 0x40,
        0,
        "vector entry cleared USIOIF"
    );
}

#[test]
fn attiny85_drives_port_b() {
    let mut machine = Machine::for_device(&ATTINY85);
    let mut program = vec![ldi(16, 0x20)];
    program.extend_from_slice(&sts(16, 0x37)); // DDRB
    program.extend_from_slice(&sts(16, 0x38)); // PORTB
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.run(20);
    assert!(
        machine.take_outputs().contains(&(5, true)),
        "PB5 is host pin 5"
    );
}

// -- ATmega32U4 ------------------------------------------------------------

#[test]
fn atmega32u4_has_its_own_memory_map() {
    let machine = Machine::for_device(&ATMEGA32U4);
    assert_eq!(machine.flash().len(), 32 * 1024);
    assert_eq!(machine.sram().len(), 2560);
    assert_eq!(machine.peripherals().eeprom.size(), 1024);
    assert_eq!(machine.device().ramend, 0x0aff);
}

#[test]
fn atmega32u4_console_usart_is_usart1() {
    let mut machine = Machine::for_device(&ATMEGA32U4);
    let device = machine.device();
    let vector = device.usarts[1].vector_rx.unwrap();
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = Vec::new();
    handler.extend_from_slice(&lds(16, 0x00ce)); // UDR1
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    machine.peripherals_mut().usart1.ucsr_b = 0x80; // RXCIE1
    machine.write_console(b"Q");
    assert_eq!(machine.run(80), Stop::Budget);
    assert_eq!(machine.sram()[0], b'Q');
    assert!(!machine.peripherals().usart1.rxc());
}

#[test]
fn atmega32u4_timer0_compare_a_vector_is_index_21() {
    let mut machine = Machine::for_device(&ATMEGA32U4);
    let device = machine.device();
    let vector = device.timer(0).unwrap().vectors.compare_a.unwrap();
    assert_eq!(vector, 21);
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0100));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer0.tccr_a = 0x02; // CTC, TOP = OCR0A
        p.timer0.ocr_a = 5;
        p.timer0.tccr_b = 0x01;
        p.timer0.timsk = 0x02; // OCIE0A
    }
    assert_eq!(machine.run(100), Stop::Budget);
    assert_eq!(machine.sram()[0], 1, "TIMER0_COMPA was taken");
}

#[test]
fn atmega32u4_adc_reads_the_high_channel_bank() {
    let mut machine = Machine::for_device(&ATMEGA32U4);
    let mut program = vec![ldi(16, 0x40)]; // AVcc reference, channel 0
    program.extend_from_slice(&sts(16, addr::ADMUX));
    program.push(ldi(16, 0x20)); // MUX5: ADC8
    program.extend_from_slice(&sts(16, addr::ADCSRB));
    program.push(ldi(16, 0xc7)); // ADEN | ADSC | /128
    program.extend_from_slice(&sts(16, addr::ADCSRA));
    let poll = program.len();
    program.extend_from_slice(&lds(17, addr::ADCSRA));
    program.push(0xfd16); // sbrc r17, 6
    program.push(rjmp((poll as i16) - (program.len() as i16 + 1)));
    program.extend_from_slice(&lds(18, addr::ADCL));
    program.extend_from_slice(&lds(19, addr::ADCH));
    program.extend_from_slice(&sts(18, 0x0100));
    program.extend_from_slice(&sts(19, 0x0101));
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.set_analog_input(8, 2_500_000);
    assert_eq!(machine.run(5_000), Stop::Budget);
    assert_eq!(machine.sram()[0], 0x00);
    assert_eq!(machine.sram()[1], 0x02);
}

#[test]
fn atmega32u4_drives_port_f() {
    let mut machine = Machine::for_device(&ATMEGA32U4);
    let mut program = vec![ldi(16, 0x01)];
    program.extend_from_slice(&sts(16, 0x30)); // DDRF
    program.extend_from_slice(&sts(16, 0x31)); // PORTF
    program.push(RJMP_SELF);
    load_words(&mut machine, 0, &program);

    machine.boot();
    machine.run(20);
    assert!(
        machine.take_outputs().contains(&(32, true)),
        "PF0 is host pin 32"
    );
}

// -- determinism ------------------------------------------------------------

fn signature(device: &'static Device) -> (u64, Vec<u8>, Vec<(u8, bool)>) {
    let mut machine = Machine::for_device(device);
    let spec = device.timer(0).unwrap();
    let vector = spec.vectors.overflow.unwrap();
    let mask = spec.masks.overflow;
    load_words(&mut machine, 0, &[SEI, RJMP_SELF]);
    let mut handler = vec![ldi(16, 1)];
    handler.extend_from_slice(&sts(16, 0x0200));
    handler.push(RETI);
    load_vector(&mut machine, device, vector, &handler);

    machine.boot();
    {
        let p = machine.peripherals_mut();
        p.timer0.tccr_b = 0x01;
        p.timer0.count = 0xfd;
        p.timer0.timsk = mask;
    }
    let stop = machine.run(400);
    assert_eq!(stop, Stop::Budget);
    (
        machine.cycles(),
        machine.sram().to_vec(),
        machine.take_outputs(),
    )
}

#[test]
fn every_part_runs_the_same_program_to_the_same_state() {
    for device in [&ATMEGA2560, &ATTINY85, &ATMEGA32U4] {
        let first = signature(device);
        let second = signature(device);
        assert_eq!(first.0, second.0, "{} cycles", device.name);
        assert_eq!(first.1, second.1, "{} SRAM", device.name);
        assert_eq!(first.2, second.2, "{} outputs", device.name);
    }
}
