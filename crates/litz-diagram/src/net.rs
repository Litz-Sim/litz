//! Wires to nets.
//!
//! A connection is a pair of pins; a net is the transitive closure of those
//! pairs. Junction points do not exist in the format — a three-way join is
//! written as three connections — so union-find over pins is the whole model.
//!
//! Ordering is deterministic: pins are interned in sorted order, nets are
//! numbered by their smallest pin. Two identical circuits therefore produce
//! identical net ids on every host.

use crate::{Circuit, DiagramError, PinRef};
use std::collections::BTreeMap;

/// A net, numbered in the order its smallest pin sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetId(pub u32);

/// One electrically common node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Net {
    pub id: NetId,
    pub pins: Vec<PinRef>,
}

#[derive(Debug)]
pub struct Netlist {
    pins: Vec<PinRef>,
    pin_net: BTreeMap<PinRef, NetId>,
    nets: Vec<Net>,
}

impl Netlist {
    /// Resolve a circuit's connections into nets.
    ///
    /// A pin connected to itself is an error; the same pair written twice is
    /// not, because tools that emit diagrams write it by accident and the
    /// electrical meaning is unchanged.
    pub fn build(circuit: &Circuit) -> Result<Self, DiagramError> {
        let mut pins: Vec<PinRef> = Vec::new();
        for c in &circuit.connections {
            if c.from == c.to {
                return Err(DiagramError::Invalid(format!(
                    "connection {c} is a pin wired to itself"
                )));
            }
            pins.push(c.from.clone());
            pins.push(c.to.clone());
        }
        pins.sort();
        pins.dedup();

        let lookup: BTreeMap<PinRef, usize> = pins
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, p)| (p, i))
            .collect();
        let mut parent: Vec<usize> = (0..pins.len()).collect();
        for c in &circuit.connections {
            let a = lookup[&c.from];
            let b = lookup[&c.to];
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                // Always attach the larger index to the smaller, so a group's
                // root is its smallest pin and BTreeMap order is pin order.
                parent[ra.max(rb)] = ra.min(rb);
            }
        }

        let mut groups: BTreeMap<usize, Vec<PinRef>> = BTreeMap::new();
        for (i, pin) in pins.iter().enumerate() {
            let root = find(&mut parent, i);
            groups.entry(root).or_default().push(pin.clone());
        }

        let mut pin_net = BTreeMap::new();
        let mut nets = Vec::with_capacity(groups.len());
        for (i, (_, pins)) in groups.into_iter().enumerate() {
            let id = NetId(i as u32);
            for pin in &pins {
                pin_net.insert(pin.clone(), id);
            }
            nets.push(Net { id, pins });
        }

        Ok(Self {
            pins,
            pin_net,
            nets,
        })
    }

    pub fn net_of(&self, pin: &PinRef) -> Option<NetId> {
        self.pin_net.get(pin).copied()
    }

    pub fn nets(&self) -> &[Net] {
        &self.nets
    }

    /// Pins that appear in at least one connection, sorted.
    pub fn pins(&self) -> &[PinRef] {
        &self.pins
    }
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Connection;

    fn wire(a: &str, b: &str) -> Connection {
        Connection {
            from: a.parse().unwrap(),
            to: b.parse().unwrap(),
            color: None,
        }
    }

    #[test]
    fn three_way_join_is_one_net() {
        let circuit = Circuit::new(Vec::new(), vec![wire("a:1", "b:1"), wire("b:1", "c:1")]);
        let nets = Netlist::build(&circuit).unwrap();
        assert_eq!(nets.nets().len(), 1);
        assert_eq!(nets.nets()[0].pins.len(), 3);
    }

    #[test]
    fn separate_wires_are_separate_nets() {
        let circuit = Circuit::new(Vec::new(), vec![wire("a:1", "b:1"), wire("c:1", "d:1")]);
        assert_eq!(Netlist::build(&circuit).unwrap().nets().len(), 2);
    }

    #[test]
    fn net_ids_follow_sorted_pins() {
        let circuit = Circuit::new(Vec::new(), vec![wire("z:1", "z:2"), wire("a:1", "a:2")]);
        let netlist = Netlist::build(&circuit).unwrap();
        assert_eq!(netlist.net_of(&"a:1".parse().unwrap()), Some(NetId(0)));
        assert_eq!(netlist.net_of(&"z:1".parse().unwrap()), Some(NetId(1)));
    }

    #[test]
    fn self_connection_is_rejected() {
        let circuit = Circuit::new(Vec::new(), vec![wire("a:1", "a:1")]);
        assert!(Netlist::build(&circuit).is_err());
    }
}
