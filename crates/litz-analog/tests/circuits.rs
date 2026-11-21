//! Named circuit tests: each test builds one circuit and checks a property a
//! hand calculation predicts. Tolerances say what is being verified: exact for
//! linear algebra, a few percent for the exponential device models and for the
//! first-order backward-Euler transient.

use litz_analog::{
    BjtModel, DiodeModel, Element, ElementId, Error, MAX_NODES, MosfetModel, Netlist, OpAmpModel,
};

/// One 10 V source across two equal resistors: the midpoint is exactly 5 V.
#[test]
fn resistive_divider_exact() {
    let mut net = Netlist::new(3); // 0 = ground, 1 = midpoint, 2 = top
    let source = net
        .add(Element::VoltageSource {
            p: 2,
            n: 0,
            volts: 10.0,
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

    let dc = net.solve_dc().unwrap();
    assert!((dc.node_volts[1] - 5.0).abs() < 1e-9);
    assert_eq!(dc.node_volts[0], 0.0);

    // The ideal source supplies 5 mA into the divider. The MNA branch current
    // is defined from p to n through the source, so a source driving a load is
    // negative.
    let i = dc.probe_current(source).unwrap();
    assert!((i + 5e-3).abs() < 1e-12, "source current was {i:e} A");
}

/// An LED with `Is = 1e-17 A`, `n = 2` behind 220 Ω from 5 V settles near
/// 1.8 V and 14.5 mA.
#[test]
fn led_series_resistor_operating_point() {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 anode
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    let resistor = net
        .add(Element::Resistor {
            a: 1,
            b: 2,
            ohms: 220.0,
        })
        .unwrap();
    let led = net
        .add(Element::Diode {
            anode: 2,
            cathode: 0,
            model: DiodeModel::led(1e-17, 2.0),
        })
        .unwrap();

    let dc = net.solve_dc().unwrap();
    let va = dc.node_volts[2];
    assert!(
        (1.7..1.9).contains(&va),
        "LED anode voltage {va:.4} V outside 1.7..1.9"
    );

    let i_led = dc.probe_current(led).unwrap();
    let i_resistor = dc.probe_current(resistor).unwrap();
    assert!(i_led > 0.0, "LED current must flow anode to cathode");
    assert!(
        (i_led - i_resistor).abs() < 1e-6,
        "series current mismatch: LED {i_led:e} vs resistor {i_resistor:e}"
    );
    // KVL: the resistor carries exactly (5 - Va) / 220.
    let expected = (5.0 - va) / 220.0;
    assert!((i_led - expected).abs() < 1e-4);
}

/// An RC branch charged from 0 V reaches 63.2 % of 5 V at one time constant;
/// backward Euler with 100 steps per tau is within 2 %.
#[test]
fn rc_charge_at_tau_within_2_percent() {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 capacitor node
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
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

    let tau = 1_000.0 * 1e-6; // 1 ms
    let dt = tau / 100.0;
    let mut run = litz_analog::Transient::with_zero_state(&net).unwrap();
    let samples = run.run(tau, dt).unwrap();
    assert_eq!(samples.len(), 100);

    let at_tau = &samples[99];
    assert!((at_tau.time - tau).abs() < 1e-12);
    let expected = 5.0 * (1.0 - (-1.0f64).exp());
    let got = at_tau.solution.node_volts[2];
    let error = (got - expected).abs() / expected;
    assert!(
        error < 0.02,
        "RC voltage at tau was {got:.6} V, expected {expected:.6} V ({:.2}% error)",
        error * 100.0
    );
    // It is a charge curve: monotone, and still below the supply.
    assert!(got > 3.0 && got < 5.0, "unexpected RC voltage {got}");
}

/// 5 V through 1 kΩ into a silicon diode clamps the node just under 0.7 V.
#[test]
fn diode_clamp() {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 clamped node
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0,
    })
    .unwrap();
    net.add(Element::Diode {
        anode: 2,
        cathode: 0,
        model: DiodeModel::silicon(),
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    let clamped = dc.node_volts[2];
    assert!(
        (0.60..0.75).contains(&clamped),
        "clamped node {clamped:.4} V outside the silicon knee"
    );
    // A clamp is only a clamp because it is far below the unloaded 5 V.
    assert!(clamped < 1.0);
}

/// A common-emitter stage with base bias and a collector resistor: the
/// transistor sits in the active region with `Ic ≈ beta · Ib`, softened by the
/// Early effect.
#[test]
fn common_emitter_bias() {
    let mut net = Netlist::new(4); // 0 gnd/emitter, 1 Vcc, 2 collector, 3 base
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
        ohms: 470_000.0, // Rb
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

    let dc = net.solve_dc().unwrap();
    let vbe = dc.node_volts[3];
    let vce = dc.node_volts[2];
    assert!(
        (0.60..0.75).contains(&vbe),
        "base-emitter voltage {vbe:.4} V outside the active region"
    );
    assert!(
        (3.5..4.5).contains(&vce),
        "collector voltage {vce:.4} V outside the expected bias point"
    );

    let ic = dc.probe_current(q).unwrap();
    let ib = (5.0 - vbe) / 470_000.0;
    let expected = 100.0 * ib * (1.0 + vce / 100.0);
    let error = (ic - expected).abs() / expected;
    assert!(
        error < 0.05,
        "Ic {ic:e} A differs from beta*Ib*(1+Vce/Va) = {expected:e} A by {:.1}%",
        error * 100.0
    );
    assert!(
        ic > 0.5e-3 && ic < 1.5e-3,
        "Ic {ic:e} outside expected range"
    );
}

/// A unity-gain follower with a 10 kΩ load drives its output to the input.
#[test]
fn opamp_follower() {
    let mut net = Netlist::new(3); // 0 gnd, 1 input, 2 output (= inverting input)
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 1.0,
    })
    .unwrap();
    let opamp = net
        .add(Element::OpAmp {
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

    let dc = net.solve_dc().unwrap();
    let out = dc.node_volts[2];
    assert!(
        (out - 1.0).abs() < 1e-3,
        "follower output {out:.6} V did not track 1 V"
    );

    // The op-amp sources the 0.1 mA the load draws, so its branch current
    // (defined leaving the output node into the amplifier) is negative.
    let i = dc.probe_current(opamp).unwrap();
    assert!((i + 1e-4).abs() < 1e-6, "op-amp output current {i:e} A");
}

/// A node with no DC path is a singular matrix: an error, never NaN voltages.
#[test]
fn floating_node_returns_error_not_nan() {
    let mut net = Netlist::new(3);
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    // Node 2 hangs off a capacitor, which is an open circuit at DC.
    net.add(Element::Capacitor {
        a: 2,
        b: 0,
        farads: 1e-6,
    })
    .unwrap();

    match net.solve_dc() {
        Err(Error::Singular(_)) => {}
        Err(other) => panic!("expected a singular-matrix error, got {other}"),
        Ok(solution) => panic!(
            "expected a singular-matrix error, got node voltages {:?}",
            solution.node_volts
        ),
    }
}

/// Two ideal sources in parallel are a source loop, also singular.
#[test]
fn voltage_source_loop_returns_error() {
    let mut net = Netlist::new(2);
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 3.0,
    })
    .unwrap();

    assert!(matches!(net.solve_dc(), Err(Error::Singular(_))));
}

/// Two solves of one netlist produce bit-identical numbers.
#[test]
fn deterministic_same_netlist_twice() {
    let mut net = Netlist::new(4); // 0 gnd, 1 supply, 2 anode, 3 base
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 220.0,
    })
    .unwrap();
    net.add(Element::Diode {
        anode: 2,
        cathode: 0,
        model: DiodeModel::led(1e-17, 2.0),
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 3,
        ohms: 10_000.0,
    })
    .unwrap();

    let first = net.solve_dc().unwrap();
    let second = net.solve_dc().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.node_volts, second.node_volts);
    assert!(
        first
            .node_volts
            .iter()
            .map(|v| v.to_bits())
            .eq(second.node_volts.iter().map(|v| v.to_bits())),
        "node voltages are not bit-identical across solves"
    );
    assert!(
        first
            .element_currents
            .iter()
            .map(|v| v.to_bits())
            .eq(second.element_currents.iter().map(|v| v.to_bits())),
        "element currents are not bit-identical across solves"
    );
}

/// Malformed input is rejected at construction, without panicking, and leaves
/// the netlist unchanged.
#[test]
fn malformed_netlist_is_rejected() {
    let mut net = Netlist::new(2);
    assert!(matches!(
        net.add(Element::Resistor {
            a: 1,
            b: 5,
            ohms: 1.0,
        }),
        Err(Error::InvalidNode { node: 5, .. })
    ));
    assert!(matches!(
        net.add(Element::Resistor {
            a: 1,
            b: 0,
            ohms: 0.0,
        }),
        Err(Error::InvalidParameter { .. })
    ));
    assert_eq!(net.len(), 0);

    // A zero-node netlist is only rejected when solving.
    let empty = Netlist::new(0);
    assert!(matches!(empty.solve_dc(), Err(Error::NoGround)));

    // Oversized netlists are refused before any matrix is allocated.
    let huge = Netlist::new(MAX_NODES + 1);
    assert!(matches!(huge.solve_dc(), Err(Error::TooLarge { .. })));
}

/// probe_current on an element that does not exist is an error, not a panic.
#[test]
fn probe_unknown_element_is_an_error() {
    let mut net = Netlist::new(2);
    net.add(Element::Resistor {
        a: 1,
        b: 0,
        ohms: 1_000.0,
    })
    .unwrap();
    let dc = net.solve_dc().unwrap();
    assert!(matches!(
        dc.probe_current(ElementId(99)),
        Err(Error::UnknownElement(ElementId(99)))
    ));
}

/// An op-amp asked for more than its supply delivers clamps at the rail.
#[test]
fn opamp_follower_clamps_to_rail() {
    let mut net = Netlist::new(3);
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 10.0,
    })
    .unwrap();
    net.add(Element::OpAmp {
        plus: 1,
        minus: 2,
        out: 2,
        model: OpAmpModel::rail_to_rail_5v(),
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    let out = dc.node_volts[2];
    assert!(
        (out - 5.0).abs() < 1e-9,
        "op-amp output {out:.6} V did not clamp to the 5 V rail (nodes {:?})",
        dc.node_volts
    );
}

/// A current source pushes 1 mA through a 1 kΩ resistor: 1 V, and the source
/// current is reported with the sign it was given.
#[test]
fn current_source_into_resistor() {
    let mut net = Netlist::new(2);
    let source = net
        .add(Element::CurrentSource {
            p: 0,
            n: 1,
            amps: 1e-3,
        })
        .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 0,
        ohms: 1_000.0,
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    assert!((dc.node_volts[1] - 1.0).abs() < 1e-12);
    assert!((dc.probe_current(source).unwrap() - 1e-3).abs() < 1e-15);
}

/// An inductor is an ideal short at DC: the current is the resistor's.
#[test]
fn inductor_is_a_short_at_dc() {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 inductor node
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 5.0,
    })
    .unwrap();
    let resistor = net
        .add(Element::Resistor {
            a: 1,
            b: 2,
            ohms: 1_000.0,
        })
        .unwrap();
    let inductor = net
        .add(Element::Inductor {
            a: 2,
            b: 0,
            henries: 1e-3,
        })
        .unwrap();

    let dc = net.solve_dc().unwrap();
    assert!(dc.node_volts[2].abs() < 1e-12, "inductor node is not 0 V");
    let i_r = dc.probe_current(resistor).unwrap();
    let i_l = dc.probe_current(inductor).unwrap();
    assert!((i_r - 5e-3).abs() < 1e-12);
    assert!((i_l - 5e-3).abs() < 1e-12);
}

/// The MOSFET model is exercised at a simple bias point: a 10 kΩ drain load
/// from 5 V with `Vgs = 2 V`, `Vth = 1 V`, `K = 1 mA/V²`.
#[test]
fn mosfet_bias_point() {
    let mut net = Netlist::new(4); // 0 gnd, 1 supply, 2 gate, 3 drain
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
    net.add(Element::Resistor {
        a: 1,
        b: 3,
        ohms: 10_000.0,
    })
    .unwrap();
    net.add(Element::Mosfet {
        drain: 3,
        gate: 2,
        source: 0,
        model: MosfetModel::nmos(1.0, 1e-3),
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    // Saturation would ask for I = 0.5e-3·(2-1)² = 0.5 mA, dropping 5 V, which
    // would leave Vds = 0; the device therefore lands in the triode region,
    // where 0.5e-3·(1·Vds - Vds²/2) = (5 - Vds)/10 k.
    let vds = dc.node_volts[3];
    let id = 1e-3 * (vds - 0.5 * vds * vds);
    assert!((vds - (5.0 - id * 10_000.0)).abs() < 1e-6);
    assert!((0.0..1.0).contains(&vds), "unexpected Vds {vds}");
}
