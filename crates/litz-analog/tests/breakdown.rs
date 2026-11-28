//! Reverse breakdown: a zener holds a node at its breakdown voltage, and an
//! otherwise identical diode without one blocks the same circuit.

use litz_analog::{DiodeModel, Element, Netlist};

/// 10 V behind 1 kΩ into a diode with its cathode at the node. The resistor
/// current is what the junction carries, so the node voltage says which region
/// the junction is in.
fn clamped_by(model: DiodeModel) -> (f64, f64) {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 node
    net.add(Element::VoltageSource {
        p: 1,
        n: 0,
        volts: 10.0,
    })
    .unwrap();
    net.add(Element::Resistor {
        a: 1,
        b: 2,
        ohms: 1_000.0,
    })
    .unwrap();
    let diode = net
        .add(Element::Diode {
            anode: 0,
            cathode: 2,
            model,
        })
        .unwrap();
    let dc = net.solve_dc().unwrap();
    let reverse_amps = dc.probe_current(diode).unwrap();
    (dc.node_volts[2], reverse_amps)
}

/// A 5.1 V zener across the node clamps it at breakdown and carries the
/// resistor's current backwards. `Vz` is quoted at 5 mA, so 4.2 mA sits a few
/// millivolts under it.
#[test]
fn a_zener_clamps_the_node_at_its_breakdown_voltage() {
    let (node, current) = clamped_by(DiodeModel::zener(5.1));
    assert!(
        (5.05..5.2).contains(&node),
        "clamped node {node:.4} V outside the 5.1 V zener's knee"
    );
    // Current flows cathode to anode inside the junction, so it reads
    // negative on the element's anode-to-cathode sense.
    assert!(
        (-5.0e-3..-4.0e-3).contains(&current),
        "zener current {current:e} A outside 4..5 mA"
    );
}

/// The same circuit with an ordinary diode stays at the supply: all of the
/// voltage is reverse bias, and only the saturation current flows.
#[test]
fn a_diode_without_a_breakdown_voltage_blocks_instead() {
    let (node, current) = clamped_by(DiodeModel::silicon());
    assert!(
        (9.9..=10.0).contains(&node),
        "blocking diode left the node at {node:.6} V"
    );
    assert!(current.abs() < 1e-9, "blocking diode carried {current:e} A");
}

/// Breakdown does not change the forward direction: the same zener from a 5 V
/// supply through 1 kΩ still drops a silicon junction.
#[test]
fn a_zener_conducts_forward_like_any_diode() {
    let mut net = Netlist::new(3); // 0 gnd, 1 supply, 2 anode
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
        model: DiodeModel::zener(5.1),
    })
    .unwrap();

    let dc = net.solve_dc().unwrap();
    let anode = dc.node_volts[2];
    assert!(
        (0.55..0.75).contains(&anode),
        "forward zener drop {anode:.4} V outside the silicon knee"
    );
}

/// The model is part of the netlist's validation, so a zero or negative
/// breakdown voltage is refused when the element is added.
#[test]
fn a_breakdown_voltage_must_be_positive() {
    let mut net = Netlist::new(2);
    let added = net.add(Element::Diode {
        anode: 1,
        cathode: 0,
        model: DiodeModel::zener(0.0),
    });
    assert!(added.is_err());
}
