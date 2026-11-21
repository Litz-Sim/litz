//! Transient analysis with backward-Euler companion models.
//!
//! At each step a capacitor `C` is replaced by a conductance `C/dt` in parallel
//! with a current source that carries the previous voltage, and an inductor
//! `L` by a conductance `dt/L` in parallel with a current source that carries
//! the previous current. The resulting circuit is solved with the same damped
//! Newton loop as DC, so nonlinear devices keep their junction limiting.
//!
//! Backward Euler is first order and unconditionally stable; it damps high
//! frequencies aggressively, which is the right trade for a deterministic
//! teaching simulator with a fixed step.

use crate::dc::newton;
use crate::mna::{self, Mode, System};
use crate::{Element, ElementId, Error, Netlist, Solution};

/// Cap on the number of steps one [`Transient::run`] may take, so a tiny `dt`
/// cannot exhaust memory.
const MAX_RUN_STEPS: f64 = 1_000_000.0;

/// One solved transient point.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Simulation time at the end of the step, in seconds.
    pub time: f64,
    pub solution: Solution,
}

/// A transient run: a netlist plus the state carried between steps.
///
/// Create one with [`Netlist::transient`] (start from the DC operating point)
/// or [`Transient::with_zero_state`] (start every node at 0 V, for a charging
/// curve). Then call [`Transient::step`] for one interval or
/// [`Transient::run`] for a sequence.
#[derive(Clone, Debug)]
pub struct Transient {
    netlist: Netlist,
    time: f64,
    /// Node voltages at the end of the last step, indexed by node.
    prev_v: Vec<f64>,
    /// Inductor current at the end of the last step, indexed by element.
    prev_i: Vec<Option<f64>>,
    last: Option<Solution>,
}

impl Transient {
    /// Starts from the DC operating point.
    ///
    /// Capacitors and inductors begin with the voltages and currents they hold
    /// at DC (capacitors open, inductors short). This is SPICE's default and is
    /// the right choice for a supply that has been on for a long time.
    pub fn new(netlist: &Netlist) -> Result<Self, Error> {
        let dc = netlist.solve_dc()?;
        let prev_i = netlist
            .elements()
            .iter()
            .enumerate()
            .map(|(index, element)| match element {
                Element::Inductor { .. } => Some(dc.element_currents[index]),
                _ => None,
            })
            .collect();
        Ok(Self {
            netlist: netlist.clone(),
            time: 0.0,
            prev_v: dc.node_volts.clone(),
            prev_i,
            last: Some(dc),
        })
    }

    /// Starts with every node at 0 V and every inductor at 0 A.
    ///
    /// Use this for a circuit that is switched on at `t = 0`: an RC branch
    /// begins discharged even if its DC steady state would be charged.
    pub fn with_zero_state(netlist: &Netlist) -> Result<Self, Error> {
        netlist.validate()?;
        let prev_i = netlist
            .elements()
            .iter()
            .map(|element| match element {
                Element::Inductor { .. } => Some(0.0),
                _ => None,
            })
            .collect();
        Ok(Self {
            netlist: netlist.clone(),
            time: 0.0,
            prev_v: vec![0.0; netlist.node_count()],
            prev_i,
            last: None,
        })
    }

    /// The netlist being simulated.
    pub fn netlist(&self) -> &Netlist {
        &self.netlist
    }

    /// Simulation time at the end of the last step, in seconds.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// The most recent solution, if a step (or the initial DC point) exists.
    pub fn solution(&self) -> Option<&Solution> {
        self.last.as_ref()
    }

    /// Current through an element at the most recent point, in amperes.
    ///
    /// Returns [`Error::NoSolutionYet`] if the run started from zero state and
    /// has not been stepped.
    pub fn probe_current(&self, element: ElementId) -> Result<f64, Error> {
        match &self.last {
            Some(solution) => solution.probe_current(element),
            None => Err(Error::NoSolutionYet),
        }
    }

    /// Advances the simulation by `dt` seconds and returns the new point.
    pub fn step(&mut self, dt: f64) -> Result<Solution, Error> {
        if !dt.is_finite() || dt <= 0.0 {
            return Err(Error::InvalidTimeStep(dt));
        }
        let system = System::new(&self.netlist, true);

        let solution = {
            let mode = Mode::Transient {
                dt,
                prev_v: &self.prev_v,
                prev_i: &self.prev_i,
            };
            let mut x = vec![0.0; system.unknowns];
            for node in 1..self.netlist.node_count() {
                x[node - 1] = self.prev_v.get(node).copied().unwrap_or(0.0);
            }
            if let Some(last) = &self.last {
                for (index, element) in self.netlist.elements().iter().enumerate() {
                    if !matches!(
                        element,
                        Element::VoltageSource { .. } | Element::OpAmp { .. }
                    ) {
                        continue;
                    }
                    if let Some(branch) = system.branch_of[index] {
                        let row = system.branch_row(branch);
                        x[row] = last.element_currents.get(index).copied().unwrap_or(0.0);
                    }
                }
            }

            let mut junction = vec![[0.0; 2]; self.netlist.len()];
            newton(&self.netlist, &system, &mode, &mut x, &mut junction)?;
            mna::solution(&self.netlist, &system, &x, &mode)
        };

        self.prev_v.clone_from(&solution.node_volts);
        for (index, element) in self.netlist.elements().iter().enumerate() {
            if matches!(element, Element::Inductor { .. }) {
                self.prev_i[index] =
                    Some(solution.element_currents.get(index).copied().unwrap_or(0.0));
            }
        }
        self.time += dt;
        self.last = Some(solution.clone());
        Ok(solution)
    }

    /// Steps `duration / dt` times and returns every point.
    ///
    /// `duration` must be at least one `dt` (otherwise
    /// [`Error::InvalidDuration`] is returned); the number of steps is
    /// `floor(duration / dt + 1e-9)`, so an exact multiple produces exactly
    /// that many samples and time never overshoots `duration`.
    pub fn run(&mut self, duration: f64, dt: f64) -> Result<Vec<Sample>, Error> {
        if !duration.is_finite() || duration <= 0.0 {
            return Err(Error::InvalidDuration(duration));
        }
        if !dt.is_finite() || dt <= 0.0 {
            return Err(Error::InvalidTimeStep(dt));
        }
        let steps = (duration / dt + 1e-9).floor();
        if steps < 1.0 {
            return Err(Error::InvalidDuration(duration));
        }
        if !steps.is_finite() || steps > MAX_RUN_STEPS {
            return Err(Error::TooManySteps { steps });
        }
        let mut samples = Vec::with_capacity(steps as usize);
        for _ in 0..steps as usize {
            let solution = self.step(dt)?;
            samples.push(Sample {
                time: self.time,
                solution,
            });
        }
        Ok(samples)
    }
}
