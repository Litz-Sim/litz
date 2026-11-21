//! Named AC-sweep tests: each builds one linear network and checks the
//! response a hand calculation predicts. Tolerances are tight because the AC
//! solve is a single complex linear system per frequency; the only error is
//! floating-point round-off.

use litz_analog::{
    AcRequest, BjtModel, Complex, DiodeModel, Element, MosfetModel, Netlist, OpAmpModel,
    THERMAL_VOLTAGE,
};

fn magnitude(point: &litz_analog::AcPoint, node: usize) -> f64 {
    point.node_voltage(node).unwrap().magnitude()
}

/// At 0 Hz the reactive elements vanish and an AC sweep of a resistive divider
/// reports the same voltage and current as the DC solve.
#[test]
fn resistive_divider_matches_dc_at_zero_hz() {
    let mut net = Netlist::new(3); // 0 gnd, 1 mid, 2 top
    net.add(Element::AcVoltageSource {
        p: 2,
        n: 0,
        amplitude: 10.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 2,
        b: 1,
        ohms: 1_000.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 0,
        ohms: 1_000.0,
    })
    .unwrap();

    let sweep = net.ac_sweep(AcRequest::frequencies([0.0])).unwrap();
    let point = &sweep.points[0];
    let v = point.node_voltage(1).unwrap();
    assert!((v.re - 5.0).abs() < 1e-12, "midpoint was {v:?}");
    assert_eq!(v.im, 0.0);
    assert_eq!(point.node_voltage(0).unwrap(), Complex::ZERO);
    // The source drives 5 mA, so its branch current is negative.
    assert!((point.source_currents[0].re + 5e-3).abs() < 1e-15);
    assert_eq!(point.source_currents[0].im, 0.0);
    assert_eq!(sweep.frequencies, vec![0.0]);
}

/// An RC low-pass with `R = 1 kΩ` and `C = 1 µF` is −3.01 dB and −45° at
/// `f = 1/(2πRC) = 159.15 Hz`.
#[test]
fn rc_low_pass_is_minus_3_db_at_cutoff() {
    let mut net = Netlist::new(3); // 0 gnd, 1 input, 2 output
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 2,
        b: 0,
        farads: 1e-6,
    })
    .unwrap();

    let cutoff = 1.0 / (std::f64::consts::TAU * 1_000.0 * 1e-6);
    let sweep = net.ac_sweep(AcRequest::frequencies([cutoff])).unwrap();
    let point = &sweep.points[0];

    let db = point.magnitude_db(2).unwrap();
    assert!(
        (db + 3.010_299_956_639_812).abs() < 1e-6,
        "RC gain at cutoff was {db:.9} dB, not -3.0103 dB"
    );
    let phase = point.phase_deg(2).unwrap();
    assert!(
        (phase + 45.0).abs() < 1e-9,
        "RC phase at cutoff was {phase:.6}°, not -45°"
    );
}

/// Above its cutoff an RC low-pass loses 20 dB per decade.
#[test]
fn rc_low_pass_rolls_off_20_db_per_decade() {
    let mut net = Netlist::new(3);
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 2,
        b: 0,
        farads: 1e-6,
    })
    .unwrap();

    let cutoff = 1.0 / (std::f64::consts::TAU * 1_000.0 * 1e-6);
    let sweep = net
        .ac_sweep(AcRequest::frequencies([10.0 * cutoff, 100.0 * cutoff]))
        .unwrap();
    let db_at_10 = sweep.points[0].magnitude_db(2).unwrap();
    let db_at_100 = sweep.points[1].magnitude_db(2).unwrap();
    // One decade above cutoff: -20.04 dB; two decades: -40.0004 dB.
    // The exact one-decade slope from 10·fc to 100·fc is 19.96 dB; it
    // approaches the 20 dB asymptote, so allow a few hundredths.
    let slope = db_at_10 - db_at_100;
    assert!(
        (slope - 20.0).abs() < 0.05,
        "RC slope was {slope:.4} dB per decade, not about 20"
    );
    assert!(
        db_at_100 < -39.9,
        "RC gain two decades up was {db_at_100} dB"
    );
}

/// An RL high-pass (`R = 1 kΩ`, `L = 100 mH`) is −3 dB at `R/(2πL)`, passes
/// high frequencies and rolls off below cutoff.
#[test]
fn rl_high_pass_rolls_off_below_cutoff() {
    let mut net = Netlist::new(3); // 0 gnd, 1 input, 2 output across L
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0,
    })
    .unwrap();
    net.add(Element::Inductor {
        a: 2,
        b: 0,
        henries: 0.1,
    })
    .unwrap();

    let cutoff = 1_000.0 / (std::f64::consts::TAU * 0.1);
    let sweep = net
        .ac_sweep(AcRequest::frequencies([
            cutoff / 10.0,
            cutoff,
            cutoff * 10.0,
        ]))
        .unwrap();

    let db_low = sweep.points[0].magnitude_db(2).unwrap();
    let db_cut = sweep.points[1].magnitude_db(2).unwrap();
    let db_high = sweep.points[2].magnitude_db(2).unwrap();
    assert!(
        (db_cut + 3.010_299_956_639_812).abs() < 1e-6,
        "RL gain at cutoff was {db_cut:.9} dB"
    );
    assert!(
        (sweep.points[1].phase_deg(2).unwrap() - 45.0).abs() < 1e-9,
        "RL phase at cutoff was not +45°"
    );
    assert!(
        (db_low + 20.0).abs() < 0.5,
        "RL gain a decade below cutoff was {db_low:.4} dB, not about -20"
    );
    assert!(
        db_high.abs() < 0.1,
        "RL gain a decade above cutoff was {db_high:.4} dB, not about 0"
    );
}

/// A series RLC (`R = 10 Ω`, `L = 1 mH`, `C = 1 µF`) peaks at
/// `1/(2π√(LC))`, where the capacitor voltage is `Q` times the drive.
#[test]
fn series_rlc_peaks_at_resonance() {
    let mut net = Netlist::new(4); // 0 gnd, 1 input, 2 after R, 3 across C
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 10.0,
    })
    .unwrap();
    net.add(Element::Inductor {
        a: 2,
        b: 3,
        henries: 1e-3,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 3,
        b: 0,
        farads: 1e-6,
    })
    .unwrap();

    let resonance = 1.0 / (std::f64::consts::TAU * (1e-3_f64 * 1e-6).sqrt());
    let sweep = net
        .ac_sweep(AcRequest::frequencies([
            resonance / 2.0,
            resonance,
            resonance * 2.0,
        ]))
        .unwrap();

    let below = magnitude(&sweep.points[0], 3);
    let at = magnitude(&sweep.points[1], 3);
    let above = magnitude(&sweep.points[2], 3);
    // Q = (1/R)√(L/C) = 3.162, so the peak is 10 dB.
    let q = (1e-3_f64 / 1e-6).sqrt() / 10.0;
    assert!(
        (at / q - 1.0).abs() < 1e-6,
        "peak was {at:.9} V, expected Q = {q:.9} V"
    );
    assert!(at > below * 1.2, "peak {at} did not stand above {below}");
    assert!(at > above * 1.2, "peak {at} did not stand above {above}");
    assert!(
        (sweep.points[1].phase_deg(3).unwrap() + 90.0).abs() < 1e-9,
        "capacitor voltage at resonance should lag by 90°"
    );
}

/// A series capacitor blocks the DC limit and passes high frequencies.
#[test]
fn capacitor_blocks_zero_hz() {
    let mut net = Netlist::new(3); // 0 gnd, 1 input, 2 output across R
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 1,
        b: 2,
        farads: 1e-6,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 2,
        b: 0,
        ohms: 1_000.0,
    })
    .unwrap();

    let sweep = net.ac_sweep(AcRequest::frequencies([0.0, 1e6])).unwrap();
    assert_eq!(sweep.points[0].node_voltage(2).unwrap(), Complex::ZERO);
    let high = sweep.points[1].magnitude_db(2).unwrap();
    assert!(high.abs() < 0.01, "series C at 1 MHz gave {high} dB");
}

/// A diode biased at `Id` presents the incremental conductance Newton used at
/// the DC point, `g = (Id + Is)/(n·Vt)`; the AC divider must follow it.
#[test]
fn diode_small_signal_conductance_matches_dc_current() {
    let mut net = Netlist::new(4); // 0 gnd, 1 supply, 2 drive, 3 anode
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 2,
        amplitude: 10e-3,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 2,
        b: 3,
        ohms: 1_000.0,
    })
    .unwrap();
    let diode = net
        .add(Element::Diode {
            anode: 3,
            cathode: 0,
            model: DiodeModel::silicon(),
        })
        .unwrap();

    let dc = net.solve_dc().unwrap();
    let id = dc.probe_current(diode).unwrap();
    assert!(id > 1e-3, "diode was not forward biased: {id:e} A");

    let sweep = net.ac_sweep(AcRequest::frequencies([1_000.0])).unwrap();
    // g = (Id + Is)/(n·Vt); Is is negligible beside a milliamp.
    let g = (id + 1e-14) / THERMAL_VOLTAGE;
    let expected = 10e-3 / (1.0 + 1_000.0 * g);
    let got = magnitude(&sweep.points[0], 3);
    let error = (got - expected).abs() / expected;
    assert!(
        error < 1e-6,
        "diode node was {got:.9} V, expected {expected:.9} V ({error:.2e} relative)"
    );
}

/// An op-amp follower passes an AC input at unity gain; the finite open-loop
/// gain leaves a `1/(1+A)` error, and the phase stays at 0°.
#[test]
fn opamp_follower_tracks_ac_input() {
    let mut net = Netlist::new(3); // 0 gnd, 1 input, 2 output (= minus)
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 0.5,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::OpAmp {
        plus: 1,
        minus: 2,
        out: 2,
        model: OpAmpModel::rail_to_rail_5v(),
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 2,
        b: 0,
        ohms: 10_000.0,
    })
    .unwrap();

    let sweep = net.ac_sweep(AcRequest::frequencies([100.0])).unwrap();
    let gain = 1e5;
    let expected = 0.5 * gain / (1.0 + gain);
    let got = magnitude(&sweep.points[0], 2);
    assert!(
        (got - expected).abs() < 1e-9,
        "follower output was {got:.12} V, expected {expected:.12} V"
    );
    assert!(sweep.points[0].phase_deg(2).unwrap().abs() < 1e-9);
}

/// A MOSFET biased in the triode region has `gm = K·Vds` and
/// `gds = K·(Vov − Vds)`; a small gate drive must produce the corresponding
/// drain swing into the DC load resistor.
#[test]
fn mosfet_common_source_small_signal_gain() {
    let mut net = Netlist::new(5); // 0 gnd, 1 supply, 2 DC gate, 3 AC gate, 4 drain
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::VoltageSource {
        p: 2,
        n: 0,
        volts: 2.0,
    })
    .unwrap();
    net.add(Element::AcVoltageSource {
        p: 2,
        n: 3,
        amplitude: 1e-3,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 4,
        ohms: 10_000.0,
    })
    .unwrap();
    net.add(Element::Mosfet {
        drain: 4,
        gate: 3,
        source: 0,
        model: MosfetModel::nmos(1.0, 1e-3),
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    let vds = dc.node_volts[4];
    assert!((0.0..1.0).contains(&vds), "MOSFET left the triode region");
    let gm = 1e-3 * vds;
    let gds = 1e-3 * (1.0 - vds);
    let expected = 1e-3 * gm / (1.0 / 10_000.0 + gds);

    let sweep = net.ac_sweep(AcRequest::frequencies([1_000.0])).unwrap();
    let got = magnitude(&sweep.points[0], 4);
    let error = (got - expected).abs() / expected;
    assert!(
        error < 1e-6,
        "drain swing was {got:.9} V, expected {expected:.9} V ({error:.2e} relative)"
    );
}

/// A common-emitter stage driven through a coupling capacitor gives the gain
/// the DC bias implies, `gm·Rc/(1 + gce·Rc)`, with `gm = Ic/Vt` and an Early
/// conductance `gce = Ic/Va`.
#[test]
fn bjt_common_emitter_small_signal_gain() {
    let mut net = Netlist::new(5); // 0 gnd/emitter, 1 Vcc, 2 collector, 3 base, 4 AC drive
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0, // Rc
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 3,
        ohms: 390_000.0, // Rb: about 1.1 mA of collector current
    })
    .unwrap();
    let q = net
        .add(Element::Bjt {
            collector: 2,
            base: 3,
            emitter: 0,
            model: BjtModel::npn(),
        })
        .unwrap();
    // At 1 MHz a 1 µF coupling capacitor is a fraction of an ohm, so the base
    // sees essentially the full 1 mV drive.
    net.add(Element::AcVoltageSource {
        p: 4,
        n: 0,
        amplitude: 1e-3,
        phase_deg: 0.0,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 4,
        b: 3,
        farads: 1e-6,
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    let ic = dc.probe_current(q).unwrap();
    assert!(
        ic > 1e-3,
        "transistor was not in the active region: {ic:e} A"
    );

    let gm = ic / THERMAL_VOLTAGE;
    let gce = ic / BjtModel::npn().early_voltage;
    let expected = gm * 1e-3 / (1.0 / 1_000.0 + gce);

    let sweep = net.ac_sweep(AcRequest::frequencies([1e6])).unwrap();
    let got = magnitude(&sweep.points[0], 2);
    let error = (got - expected).abs() / expected;
    assert!(
        error < 0.02,
        "collector swing was {got:.9} V, expected {expected:.9} V ({error:.2e} relative)"
    );
}

/// Two sweeps of one netlist are bit-identical: frequencies, every phasor
/// component and every branch current.
#[test]
fn ac_sweep_is_deterministic() {
    let mut net = Netlist::new(4); // 0 gnd, 1 input, 2 after R, 3 across C
    net.add(Element::AcVoltageSource {
        p: 1,
        n: 0,
        amplitude: 1.0,
        phase_deg: 30.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 470.0,
    })
    .unwrap();
    net.add(Element::Inductor {
        a: 2,
        b: 3,
        henries: 4.7e-3,
    })
    .unwrap();
    net.add(Element::Capacitor {
        a: 3,
        b: 0,
        farads: 100e-9,
    })
    .unwrap();
    net.add(Element::Diode {
        anode: 3,
        cathode: 0,
        model: DiodeModel::silicon(),
    })
    .unwrap();

    let request = AcRequest::decades(10.0, 1e6, 4);
    let first = net.ac_sweep(request.clone()).unwrap();
    let second = net.ac_sweep(request).unwrap();

    assert_eq!(first.frequencies, second.frequencies);
    assert_eq!(first.points.len(), second.points.len());
    for (a, b) in first.points.iter().zip(second.points.iter()) {
        assert_eq!(a.hz.to_bits(), b.hz.to_bits());
        for (x, y) in a.node_volts.iter().zip(b.node_volts.iter()) {
            assert_eq!(x.re.to_bits(), y.re.to_bits());
            assert_eq!(x.im.to_bits(), y.im.to_bits());
        }
        for (x, y) in a.source_currents.iter().zip(b.source_currents.iter()) {
            assert_eq!(x.re.to_bits(), y.re.to_bits());
            assert_eq!(x.im.to_bits(), y.im.to_bits());
        }
    }
}
