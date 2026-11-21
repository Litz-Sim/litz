//! Device equations and their small-signal linearisations.
//!
//! Every function here is a pure evaluation of a nonlinear element at a trial
//! voltage (or voltage pair) and returns both the terminal current and its
//! derivatives. The assembly step turns those into MNA stamps. Junction
//! voltages are passed through [`limit_junction`] first, which is the same
//! limiting SPICE uses: a Newton step may not jump an exponential by more than
//! a few thermal voltages, so a cold start with a diode reverse-biased at 5 V
//! walks down instead of overflowing.

use crate::{BjtModel, DiodeModel, MosfetModel, THERMAL_VOLTAGE};

/// Exponent clamp for the Shockley equation. `exp(60) ≈ 1.1e26`, large enough
/// that a real junction never reaches it once limiting is applied, small enough
/// that the product with a tiny `Is` stays finite.
const MAX_EXP_ARG: f64 = 60.0;

const SQRT_2: f64 = std::f64::consts::SQRT_2;

/// Applies SPICE's junction voltage limiting.
///
/// `v_new` is the voltage the linear solve proposed, `v_old` the value used at
/// the previous iteration. Above the critical voltage the step is compressed
/// logarithmically, so the exponent changes by at most a few `Vt` per iteration.
pub(crate) fn limit_junction(v_new: f64, v_old: f64, vt: f64, is: f64) -> f64 {
    if is <= 0.0 || !is.is_finite() || vt <= 0.0 {
        return v_new;
    }
    let vcrit = vt * (vt / (SQRT_2 * is)).ln();
    if !vcrit.is_finite() {
        return v_new;
    }
    if v_new > vcrit && (v_new - v_old).abs() > 2.0 * vt {
        if v_old > 0.0 {
            let arg = 1.0 + (v_new - v_old) / vt;
            if arg > 0.0 {
                return v_old + vt * arg.ln();
            }
            return vcrit;
        }
        if v_new > 0.0 {
            return vt * (v_new / vt).ln();
        }
    }
    v_new
}

/// The evaluated state of one Shockley junction.
pub(crate) struct Junction {
    /// Limited junction voltage used for the evaluation.
    pub voltage: f64,
    pub current: f64,
    pub conductance: f64,
}

/// Evaluates a diode junction at `v_new`, limited from `v_old`.
pub(crate) fn diode(v_new: f64, v_old: f64, model: &DiodeModel) -> Junction {
    let vt = model.emission_coefficient * THERMAL_VOLTAGE;
    let is = model.saturation_current;
    let voltage = limit_junction(v_new, v_old, vt, is);
    let e = (voltage / vt).clamp(-MAX_EXP_ARG, MAX_EXP_ARG).exp();
    Junction {
        voltage,
        current: is * (e - 1.0),
        conductance: is / vt * e,
    }
}

/// The evaluated state of an NPN transistor.
pub(crate) struct Bjt {
    pub vbe: f64,
    pub vbc: f64,
    pub ic: f64,
    pub ib: f64,
    pub ie: f64,
    pub d_ic_dvbe: f64,
    pub d_ic_dvbc: f64,
    pub d_ib_dvbe: f64,
    pub d_ib_dvbc: f64,
}

/// Evaluates the Ebers-Moll transport model at `(vbe, vbc)`.
pub(crate) fn bjt(vbe_new: f64, vbc_new: f64, vbe_old: f64, vbc_old: f64, model: &BjtModel) -> Bjt {
    let vt = THERMAL_VOLTAGE;
    let is = model.saturation_current();
    let beta_f = model.beta;
    // The reverse gain is not exposed; a transport model needs a finite
    // reverse beta, and 1.0 keeps the reverse junction physical without
    // letting a reverse-biased collector inject spurious base current.
    let beta_r = 1.0;

    let vbe = limit_junction(vbe_new, vbe_old, vt, is);
    let vbc = limit_junction(vbc_new, vbc_old, vt, is);
    let ebe = (vbe / vt).clamp(-MAX_EXP_ARG, MAX_EXP_ARG).exp();
    let ebc = (vbc / vt).clamp(-MAX_EXP_ARG, MAX_EXP_ARG).exp();

    let ibe = is / beta_f * (ebe - 1.0);
    let ibc = is / beta_r * (ebc - 1.0);
    let ict = is * (ebe - ebc);
    // Early effect: Vce = vbe - vbc scales the transport current.
    let early = 1.0 + (vbe - vbc) / model.early_voltage;

    let ic = ict * early - ibc;
    let ib = ibe + ibc;
    let ie = -(ic + ib);

    let d_ic_dvbe = is / vt * ebe * early + ict / model.early_voltage;
    let d_ic_dvbc = -is / vt * ebc * early - ict / model.early_voltage - is / beta_r / vt * ebc;
    let d_ib_dvbe = is / beta_f / vt * ebe;
    let d_ib_dvbc = is / beta_r / vt * ebc;

    Bjt {
        vbe,
        vbc,
        ic,
        ib,
        ie,
        d_ic_dvbe,
        d_ic_dvbc,
        d_ib_dvbe,
        d_ib_dvbc,
    }
}

/// The evaluated state of an NMOS transistor.
pub(crate) struct Mosfet {
    pub id: f64,
    pub gm: f64,
    pub gds: f64,
    /// The `Vgs` and `Vds` at which the current and derivatives were evaluated.
    pub vgs: f64,
    pub vds: f64,
}

/// Evaluates the level-1 square-law model at `(vgs, vds)`.
///
/// `Vds` is clamped to `0` for the *evaluation* (the derivatives still see the
/// real sign), which keeps a reverse-biased `Vds` from manufacturing current
/// while still giving Newton a finite conductance to work with.
pub(crate) fn mosfet(vgs: f64, vds: f64, model: &MosfetModel) -> Mosfet {
    let vov = vgs - model.threshold;
    if vov <= 0.0 {
        return Mosfet {
            id: 0.0,
            gm: 0.0,
            gds: 0.0,
            vgs,
            vds: vds.max(0.0),
        };
    }
    let vds_eval = vds.max(0.0);
    if vds_eval < vov {
        // Triode.
        Mosfet {
            id: model.k * (vov * vds_eval - 0.5 * vds_eval * vds_eval),
            gm: model.k * vds_eval,
            gds: model.k * (vov - vds_eval),
            vgs,
            vds: vds_eval,
        }
    } else {
        // Saturation.
        let lambda_term = 1.0 + model.lambda * vds_eval;
        Mosfet {
            id: 0.5 * model.k * vov * vov * lambda_term,
            gm: model.k * vov * lambda_term,
            gds: 0.5 * model.k * vov * vov * model.lambda,
            vgs,
            vds: vds_eval,
        }
    }
}
