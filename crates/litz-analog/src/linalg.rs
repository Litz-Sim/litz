//! A small dense linear solver: LU with partial pivoting.
//!
//! The matrices here are the nodal-admittance matrices of a circuit, which are
//! small (tens of nodes) and, when the netlist is well formed, nonsingular.
//! The solver is deliberately plain: Gaussian elimination with row pivoting,
//! `O(n^3)`, no blocking, no SIMD. That keeps it deterministic — the same
//! matrix always takes the same pivots on every host and produces the same
//! bits — which is what the determinism tests promise. It is not a sparse
//! solver; see the crate limits.
//!
//! Pivoting picks the row with the largest absolute entry in the column,
//! comparing with `>` so ties keep the earlier row. A column whose best pivot
//! is below [`PIVOT_EPSILON`] is treated as singular rather than divided
//! through, so a floating node becomes an error instead of an infinity.

/// Below this, a pivot is treated as numerically zero.
///
/// A real circuit matrix has entries on the order of `1e-12` siemens (a
/// gigaohm) up to `1e3` (a milliohm); a pivot this small means the node it
/// belongs to is floating.
pub const PIVOT_EPSILON: f64 = 1.0e-14;

/// Why a linear system could not be solved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinalgError {
    /// The matrix is singular to working precision. Usually a floating node,
    /// a loop of ideal voltage sources, or a voltage source with no path to
    /// ground.
    Singular { row: usize },
}

impl std::error::Error for LinalgError {}

impl std::fmt::Display for LinalgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Singular { row } => {
                write!(
                    f,
                    "matrix is singular at row {row} (floating node or source loop)"
                )
            }
        }
    }
}

/// Solves `a * x = b` in place, returning `x`.
///
/// `a` is consumed; `b` is left untouched. The two slices must be the same
/// length `n`, and `a` must hold `n * n` values row-major. A length mismatch
/// is reported as singular rather than panicking, because this crate is
/// reachable from parsed netlists.
pub fn solve(a: &[f64], b: &[f64]) -> Result<Vec<f64>, LinalgError> {
    let n = b.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    if a.len() != n * n {
        return Err(LinalgError::Singular { row: 0 });
    }
    let mut m = a.to_vec();
    let mut x = b.to_vec();

    for col in 0..n {
        // Partial pivoting: the largest magnitude in this column, earliest win.
        let mut pivot = col;
        let mut best = m[col * n + col].abs();
        for row in (col + 1)..n {
            let v = m[row * n + col].abs();
            if v > best {
                best = v;
                pivot = row;
            }
        }
        if best <= PIVOT_EPSILON || best.is_nan() {
            return Err(LinalgError::Singular { row: col });
        }
        if pivot != col {
            for k in 0..n {
                m.swap(col * n + k, pivot * n + k);
            }
            x.swap(col, pivot);
        }

        let diag = m[col * n + col];
        for row in (col + 1)..n {
            let factor = m[row * n + col] / diag;
            if factor == 0.0 {
                continue;
            }
            m[row * n + col] = 0.0;
            for k in (col + 1)..n {
                m[row * n + k] -= factor * m[col * n + k];
            }
            x[row] -= factor * x[col];
        }
    }

    // Back substitution.
    for col in (0..n).rev() {
        let mut sum = x[col];
        for k in (col + 1)..n {
            sum -= m[col * n + k] * x[k];
        }
        x[col] = sum / m[col * n + col];
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_two_by_two() {
        // 2x + y = 5, x + 3y = 10 -> x = 1, y = 3
        let a = [2.0, 1.0, 1.0, 3.0];
        let x = solve(&a, &[5.0, 10.0]).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-12);
        assert!((x[1] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn pivots_when_first_entry_is_zero() {
        // [[0, 1], [1, 0]] * [x, y] = [2, 3] -> x = 3, y = 2
        let x = solve(&[0.0, 1.0, 1.0, 0.0], &[2.0, 3.0]).unwrap();
        assert!((x[0] - 3.0).abs() < 1e-12);
        assert!((x[1] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn singular_is_an_error() {
        assert_eq!(
            solve(&[1.0, 2.0, 2.0, 4.0], &[1.0, 2.0]),
            Err(LinalgError::Singular { row: 1 })
        );
    }

    #[test]
    fn empty_is_empty() {
        assert!(solve(&[], &[]).unwrap().is_empty());
    }
}
