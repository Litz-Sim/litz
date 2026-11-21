//! Complex arithmetic and a dense complex linear solver.
//!
//! AC analysis turns the real nodal matrix into a complex one (conductances
//! become `jωC`, inductors contribute `jωL`, and the right-hand side becomes a
//! phasor). This module supplies the minimum needed for that: a plain
//! [`Complex`] value type and Gaussian elimination with partial pivoting.
//!
//! Like the real solver in [`crate::linalg`], this is deliberately plain:
//! row-major storage, no blocking, no SIMD, no external dependency. Pivoting
//! compares squared magnitudes and keeps the earlier row on a tie, so the same
//! matrix always takes the same pivots and produces the same bits on a given
//! build. Magnitudes are computed with a square root of a sum of squares rather
//! than `hypot`, because `sqrt` is correctly rounded by IEEE 754 while platform
//! `hypot` implementations are not required to agree bit for bit.

use crate::linalg::{LinalgError, PIVOT_EPSILON};

/// A complex number, `re + j·im`.
///
/// The electrical sign convention matches the rest of the crate: the imaginary
/// unit is the one that makes a capacitor's admittance `jωC`, so a capacitor
/// current leads its voltage by 90°.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    /// Real part.
    pub re: f64,
    /// Imaginary part.
    pub im: f64,
}

impl Complex {
    /// `0 + j0`.
    pub const ZERO: Self = Self { re: 0.0, im: 0.0 };

    /// `1 + j0`.
    pub const ONE: Self = Self { re: 1.0, im: 0.0 };

    /// The number `re + j·im`.
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    /// A phasor of the given magnitude and phase in degrees.
    ///
    /// This is how an AC source's `(amplitude, phase_deg)` pair becomes the
    /// right-hand side of the nodal system.
    pub fn from_polar(magnitude: f64, phase_deg: f64) -> Self {
        let radians = phase_deg.to_radians();
        Self {
            re: magnitude * radians.cos(),
            im: magnitude * radians.sin(),
        }
    }

    /// `√(re² + im²)`.
    pub fn magnitude(self) -> f64 {
        (self.re * self.re + self.im * self.im).sqrt()
    }

    /// The argument in degrees, in `(-180, 180]`.
    pub fn phase_deg(self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }

    /// Squared magnitude, used for pivot comparisons without a square root.
    fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

impl std::ops::Neg for Complex {
    type Output = Self;

    fn neg(self) -> Self {
        Self {
            re: -self.re,
            im: -self.im,
        }
    }
}

impl std::ops::Add for Complex {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            re: self.re + rhs.re,
            im: self.im + rhs.im,
        }
    }
}

impl std::ops::Sub for Complex {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self {
            re: self.re - rhs.re,
            im: self.im - rhs.im,
        }
    }
}

impl std::ops::Mul for Complex {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self {
        Self {
            re: self.re * rhs.re - self.im * rhs.im,
            im: self.re * rhs.im + self.im * rhs.re,
        }
    }
}

impl std::ops::Div for Complex {
    type Output = Self;

    fn div(self, rhs: Self) -> Self {
        // Multiply numerator and denominator by the conjugate of the
        // denominator. Written out rather than calling `conj()` so the four
        // products stay visible and the operation order is fixed.
        let denom = rhs.norm_sqr();
        Self {
            re: (self.re * rhs.re + self.im * rhs.im) / denom,
            im: (self.im * rhs.re - self.re * rhs.im) / denom,
        }
    }
}

/// Solves `a * x = b` for a complex dense matrix, returning `x`.
///
/// `a` is row-major with `n * n` entries, where `n` is `b.len()`. The input is
/// left untouched. A pivot at or below [`PIVOT_EPSILON`] is reported as
/// [`LinalgError::Singular`] instead of being divided through, so a circuit
/// with a floating node fails loudly rather than producing infinities.
pub fn solve(a: &[Complex], b: &[Complex]) -> Result<Vec<Complex>, LinalgError> {
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
        // Partial pivoting on the largest squared magnitude; `>` keeps the
        // earlier row when two candidates tie. Comparing squares avoids a
        // square root and is monotonic in magnitude for non-negative inputs.
        let mut pivot = col;
        let mut best = m[col * n + col].norm_sqr();
        for row in (col + 1)..n {
            let v = m[row * n + col].norm_sqr();
            if v > best {
                best = v;
                pivot = row;
            }
        }
        if best <= PIVOT_EPSILON * PIVOT_EPSILON || best.is_nan() {
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
            if factor == Complex::ZERO {
                continue;
            }
            m[row * n + col] = Complex::ZERO;
            for k in (col + 1)..n {
                m[row * n + k] = m[row * n + k] - factor * m[col * n + k];
            }
            x[row] = x[row] - factor * x[col];
        }
    }

    // Back substitution.
    for col in (0..n).rev() {
        let mut sum = x[col];
        for k in (col + 1)..n {
            sum = sum - m[col * n + k] * x[k];
        }
        x[col] = sum / m[col * n + col];
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn complex_arithmetic_matches_the_definitions() {
        let a = Complex::new(3.0, 4.0);
        let b = Complex::new(1.0, -2.0);
        assert_eq!(a + b, Complex::new(4.0, 2.0));
        assert_eq!(a - b, Complex::new(2.0, 6.0));
        // (3 + 4j)(1 - 2j) = 3 - 6j + 4j - 8j² = 11 - 2j
        assert_eq!(a * b, Complex::new(11.0, -2.0));
        // Division undoes multiplication.
        let q = a * b / b;
        assert!(close(q.re, a.re) && close(q.im, a.im));
    }

    #[test]
    fn polar_round_trip() {
        let z = Complex::from_polar(2.0, 30.0);
        assert!(close(z.re, 3.0f64.sqrt()));
        assert!(close(z.im, 1.0));
        assert!(close(z.magnitude(), 2.0));
        assert!(close(z.phase_deg(), 30.0));
    }

    #[test]
    fn solves_two_by_two_complex_system() {
        // A = [[1+j, 2], [3, 4-j]] applied to x = [1, -2] gives
        // b = [-3+j, -5+2j]; solving must recover x.
        let a = [
            Complex::new(1.0, 1.0),
            Complex::new(2.0, 0.0),
            Complex::new(3.0, 0.0),
            Complex::new(4.0, -1.0),
        ];
        let b = [Complex::new(-3.0, 1.0), Complex::new(-5.0, 2.0)];
        let x = solve(&a, &b).unwrap();
        assert!(close(x[0].re, 1.0) && close(x[0].im, 0.0));
        assert!(close(x[1].re, -2.0) && close(x[1].im, 0.0));
    }

    #[test]
    fn solves_three_by_three_complex_system() {
        // A = [[2, j, 0], [0, 1+j, 1], [1, 0, 2]] with x = [1, 2, 3] gives
        // b = [2+2j, 5+2j, 7].
        let a = [
            Complex::new(2.0, 0.0),
            Complex::new(0.0, 1.0),
            Complex::ZERO,
            Complex::ZERO,
            Complex::new(1.0, 1.0),
            Complex::ONE,
            Complex::ONE,
            Complex::ZERO,
            Complex::new(2.0, 0.0),
        ];
        let b = [
            Complex::new(2.0, 2.0),
            Complex::new(5.0, 2.0),
            Complex::new(7.0, 0.0),
        ];
        let x = solve(&a, &b).unwrap();
        assert!(close(x[0].re, 1.0) && close(x[0].im, 0.0));
        assert!(close(x[1].re, 2.0) && close(x[1].im, 0.0));
        assert!(close(x[2].re, 3.0) && close(x[2].im, 0.0));
    }

    #[test]
    fn pivots_when_first_entry_is_zero() {
        // [[0, 1], [1, 0]] * [x, y] = [2, 3] -> x = 3, y = 2.
        let a = [Complex::ZERO, Complex::ONE, Complex::ONE, Complex::ZERO];
        let b = [Complex::new(2.0, 0.0), Complex::new(3.0, 0.0)];
        let x = solve(&a, &b).unwrap();
        assert!(close(x[0].re, 3.0) && close(x[0].im, 0.0));
        assert!(close(x[1].re, 2.0) && close(x[1].im, 0.0));
    }

    #[test]
    fn singular_matrix_is_an_error() {
        // The second row is twice the first, so elimination leaves a zero
        // pivot. The report names the row that became singular.
        let a = [
            Complex::ONE,
            Complex::new(2.0, 0.0),
            Complex::new(2.0, 0.0),
            Complex::new(4.0, 0.0),
        ];
        let b = [Complex::ONE, Complex::new(3.0, 0.0)];
        assert_eq!(solve(&a, &b), Err(LinalgError::Singular { row: 1 }));
    }

    #[test]
    fn empty_system_is_empty() {
        assert!(solve(&[], &[]).unwrap().is_empty());
    }

    #[test]
    fn mismatched_lengths_are_singular() {
        assert_eq!(
            solve(&[Complex::ONE], &[Complex::ONE, Complex::ONE]),
            Err(LinalgError::Singular { row: 0 })
        );
    }
}
