//! A dense Cholesky factorisation, and back-substitution against it.
//!
//! Deliberately small, and deliberately not a dependency. The crate has none
//! for linear algebra and compiles to wasm, where a foreign kernel's choice of
//! instruction is exactly the thing that would make native and browser layouts
//! disagree.
//!
//! One factorisation, then one solve per axis against it. The matrix is the
//! spring graph's weighted Laplacian with the pinned bodies struck out and the
//! step's anchor added to the diagonal; the graph and the stiffnesses hold
//! still for a whole relaxation, but the anchor doubles every step, so the
//! matrix is rebuilt and refactorised once per step and then serves all three
//! axes -- one `O(n^3/3)` against three `O(n^2)`. A sparse solver would buy
//! nothing until circuits are much larger than seven_segment's couple of
//! hundred bodies, and would cost the property this one has for free: the loop
//! order is fixed, nothing is parallel, and `f64` addition, multiplication and
//! `sqrt` are exact IEEE-754 operations, so two toolchains agree bit for bit.

/// A symmetric positive-definite matrix, factorised as `L * Lᵀ`.
///
/// `Debug` because `a_system_with_no_unique_answer_is_refused` calls
/// `expect_err`, which is bound `where T: Debug` on the `Ok` type.
#[derive(Debug)]
pub struct Factorisation {
    lower: Vec<f64>,
    order: usize,
}

/// Where the factorisation ran out of positive pivot.
///
/// For a bare Laplacian this means a connected component that may slide freely,
/// so the system has no unique answer. `relax` never hands one over: it adds an
/// anchor to every diagonal entry, which makes the matrix strictly diagonally
/// dominant whatever the graph looks like. So what reaches this type from there
/// is a stiffness that is not positive or a pull whose two ends are the same
/// body -- a graph built wrong, which is what `RelaxError::Unsolvable` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPositiveDefinite {
    pub row: usize,
}

impl Factorisation {
    /// Factorise `matrix`, given row-major as `order * order`. Only the lower
    /// triangle is read.
    pub fn of(matrix: &[f64], order: usize) -> Result<Factorisation, NotPositiveDefinite> {
        assert_eq!(matrix.len(), order * order, "matrix is not {order} by {order}");
        let mut lower = vec![0.0; order * order];
        for j in 0..order {
            let mut diagonal = matrix[j * order + j];
            for k in 0..j {
                diagonal -= lower[j * order + k] * lower[j * order + k];
            }
            if diagonal <= 0.0 {
                return Err(NotPositiveDefinite { row: j });
            }
            let pivot = diagonal.sqrt();
            lower[j * order + j] = pivot;
            for i in (j + 1)..order {
                let mut sum = matrix[i * order + j];
                for k in 0..j {
                    sum -= lower[i * order + k] * lower[j * order + k];
                }
                lower[i * order + j] = sum / pivot;
            }
        }
        Ok(Factorisation { lower, order })
    }

    /// Solve `matrix * x = rhs`, overwriting `rhs` with `x`.
    ///
    /// Indexed rather than iterated on purpose: back-substitution reads
    /// `rhs[k]` for `k < i` while writing `rhs[i]`, and the fixed loop order is
    /// what makes two toolchains agree bit for bit. Clippy's
    /// `needless_range_loop` fires on both inner loops and its suggestion --
    /// iterate the slice -- is the one thing this must not do.
    #[allow(clippy::needless_range_loop)]
    pub fn solve(&self, rhs: &mut [f64]) {
        assert_eq!(rhs.len(), self.order, "right-hand side is not {} long", self.order);
        for i in 0..self.order {
            let mut sum = rhs[i];
            for k in 0..i {
                sum -= self.lower[i * self.order + k] * rhs[k];
            }
            rhs[i] = sum / self.lower[i * self.order + i];
        }
        for i in (0..self.order).rev() {
            let mut sum = rhs[i];
            for k in (i + 1)..self.order {
                sum -= self.lower[k * self.order + i] * rhs[k];
            }
            rhs[i] = sum / self.lower[i * self.order + i];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A system small enough to solve by hand:
    /// `4a + b = 1`, `a + 3b = 2` gives `a = 1/11`, `b = 7/11`.
    #[test]
    fn it_solves_a_system_whose_answer_is_known() {
        let factorisation =
            Factorisation::of(&[4.0, 1.0, 1.0, 3.0], 2).expect("this one is positive definite");
        let mut rhs = [1.0, 2.0];
        factorisation.solve(&mut rhs);
        assert!((rhs[0] - 1.0 / 11.0).abs() < 1e-12, "a came out {}", rhs[0]);
        assert!((rhs[1] - 7.0 / 11.0).abs() < 1e-12, "b came out {}", rhs[1]);
    }

    /// The bare Laplacian of one edge: translation is free, so there is no
    /// unique answer -- and a solver that returns one anyway returns a
    /// placement nobody can reproduce.
    ///
    /// A matrix `relax` never builds, deliberately. Task 8 adds an anchor to
    /// every diagonal entry, which is what makes the system solvable with
    /// nothing pinned. This is the statement that the refusal is real, so that
    /// `RelaxError::Unsolvable` means what Task 8 says it means: not a
    /// component free to translate, but a graph built wrong.
    #[test]
    fn a_system_with_no_unique_answer_is_refused() {
        let error = Factorisation::of(&[1.0, -1.0, -1.0, 1.0], 2)
            .expect_err("a free translation has no unique answer");
        assert_eq!(error.row, 1);
    }

    /// Add an anchor to that same edge's diagonal and it becomes solvable, at
    /// every anchor strength down to the weakest one Task 8 uses.
    ///
    /// This is the property the whole step loop rests on: `A + λI` with
    /// `λ >= 1` is strictly diagonally dominant, so it is positive definite
    /// whether or not anything is pinned -- and `compile()` pins nothing.
    #[test]
    fn an_anchor_on_the_diagonal_makes_the_same_system_solvable() {
        for anchor in [1.0, 2.0, 1024.0] {
            let factorisation = Factorisation::of(&[1.0 + anchor, -1.0, -1.0, 1.0 + anchor], 2)
                .unwrap_or_else(|error| panic!("anchor {anchor} left row {} flat", error.row));
            // Both bodies anchored to the same place: the spring is already at
            // rest there, so that is where they stay.
            let mut rhs = [7.0 * anchor, 7.0 * anchor];
            factorisation.solve(&mut rhs);
            assert!((rhs[0] - 7.0).abs() < 1e-12, "anchor {anchor} landed at {}", rhs[0]);
            assert!((rhs[1] - 7.0).abs() < 1e-12, "anchor {anchor} landed at {}", rhs[1]);
        }
    }

    /// Striking a pinned body out works too, and is what the solve does with
    /// one: the free body lands exactly on the pinned one, because a spring at
    /// rest has zero length.
    #[test]
    fn striking_out_a_pinned_body_leaves_a_system_with_one_unknown() {
        let factorisation = Factorisation::of(&[1.0], 1).expect("one pinned neighbour is enough");
        let mut rhs = [7.0];
        factorisation.solve(&mut rhs);
        assert!((rhs[0] - 7.0).abs() < 1e-12, "landed at {}", rhs[0]);
    }

    /// Same input, same bits. Everything downstream is reproducible only if
    /// this is.
    #[test]
    fn the_same_system_solves_to_the_same_bits_twice() {
        let matrix = [4.0, 1.0, 0.5, 1.0, 3.0, 0.25, 0.5, 0.25, 2.0];
        let first = {
            let mut rhs = [1.0, 2.0, 3.0];
            Factorisation::of(&matrix, 3).expect("positive definite").solve(&mut rhs);
            rhs
        };
        let second = {
            let mut rhs = [1.0, 2.0, 3.0];
            Factorisation::of(&matrix, 3).expect("positive definite").solve(&mut rhs);
            rhs
        };
        assert_eq!(first.map(f64::to_bits), second.map(f64::to_bits));
    }
}
