//! Small, fully specified truth tables: prime implicants plus a deterministic
//! greedy cover retaining a common term across unchanged single-input
//! transitions. Callers cap support at six bits, so neither enumeration nor
//! pairwise merging grows with arbitrary HDL input width.
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cube {
    pub bits: usize,
    pub mask: usize,
}

impl Cube {
    pub fn contains(self, value: usize) -> bool {
        value & self.mask == self.bits
    }
}

pub fn cover(table: &[bool], value: bool) -> Vec<Cube> {
    let mut layer: BTreeSet<_> = table
        .iter()
        .enumerate()
        .filter(|(_, bit)| **bit == value)
        .map(|(bits, _)| Cube {
            bits,
            mask: table.len() - 1,
        })
        .collect();
    let mut primes = BTreeSet::new();
    while !layer.is_empty() {
        let mut combined = BTreeSet::new();
        let mut next = BTreeSet::new();
        for a in &layer {
            for bit in 0..table.len().trailing_zeros() {
                let delta = 1 << bit;
                if a.mask & delta == 0 || a.bits & delta != 0 {
                    continue;
                }
                let b = Cube {
                    bits: a.bits | delta,
                    mask: a.mask,
                };
                if layer.contains(&b) {
                    combined.insert(*a);
                    combined.insert(b);
                    next.insert(Cube {
                        bits: a.bits,
                        mask: a.mask ^ delta,
                    });
                }
            }
        }
        primes.extend(layer.difference(&combined).copied());
        layer = next;
    }
    let all_primes = primes.clone();
    let edges: Vec<_> = table
        .iter()
        .enumerate()
        .filter(|(_, bit)| **bit == value)
        .flat_map(|(point, _)| {
            (0..table.len().trailing_zeros()).filter_map(move |bit| {
                let other = point ^ (1 << bit);
                (point < other && table[other] == value).then_some((point, other))
            })
        })
        .collect();
    let mut uncovered: BTreeSet<_> = table
        .iter()
        .enumerate()
        .filter_map(|(i, bit)| (*bit == value).then_some(i))
        .collect();
    let mut selected = Vec::new();
    while !uncovered.is_empty() {
        // Mandatory primes first, then maximum new coverage / fewest literals.
        let essential = uncovered.iter().find_map(|&point| {
            let candidates: Vec<_> = primes
                .iter()
                .filter(|cube| cube.contains(point))
                .copied()
                .collect();
            (candidates.len() == 1).then(|| candidates[0])
        });
        let best = essential.unwrap_or_else(|| {
            *primes
                .iter()
                .min_by_key(|cube| {
                    (
                        std::cmp::Reverse(uncovered.iter().filter(|&&i| cube.contains(i)).count()),
                        cube.mask.count_ones(),
                        **cube,
                    )
                })
                .unwrap()
        });
        uncovered.retain(|&i| !best.contains(i));
        primes.remove(&best);
        selected.push(best);
    }
    // Keep a term asserted across each single-input transition whose result
    // is unchanged. Point coverage alone can drop the consensus term and
    // leave a gap when the two paths arrive at different physical times.
    for &(a, b) in &edges {
        if !selected
            .iter()
            .any(|cube| cube.contains(a) && cube.contains(b))
        {
            selected.push(
                *all_primes
                    .iter()
                    .filter(|cube| cube.contains(a) && cube.contains(b))
                    .min_by_key(|cube| (cube.mask.count_ones(), **cube))
                    .unwrap(),
            );
        }
    }
    // A later selection can make an earlier choice redundant, but must
    // preserve both point coverage and the transition-spanning terms.
    for i in (0..selected.len()).rev() {
        if table.iter().enumerate().all(|(point, bit)| {
            *bit != value
                || selected
                    .iter()
                    .enumerate()
                    .any(|(j, cube)| j != i && cube.contains(point))
        }) && edges.iter().all(|&(a, b)| {
            selected
                .iter()
                .enumerate()
                .any(|(j, cube)| j != i && cube.contains(a) && cube.contains(b))
        }) {
            selected.remove(i);
        }
    }
    selected.sort();
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_three_input_functions_have_exact_covers_in_both_polarities() {
        for function in 0..256u32 {
            let table: Vec<_> = (0..8).map(|i| function & (1 << i) != 0).collect();
            for value in [false, true] {
                let cubes = cover(&table, value);
                for (point, expected) in table.iter().enumerate() {
                    assert_eq!(
                        cubes.iter().any(|cube| cube.contains(point)),
                        *expected == value
                    );
                    for bit in 0..3 {
                        let other = point ^ (1 << bit);
                        if *expected == value && table[other] == value {
                            assert!(
                                cubes
                                    .iter()
                                    .any(|cube| cube.contains(point) && cube.contains(other)),
                                "function {function}, polarity {value}, edge {point}->{other}"
                            );
                        }
                    }
                }
                assert_eq!(cubes, cover(&table, value));
            }
        }
    }
}
