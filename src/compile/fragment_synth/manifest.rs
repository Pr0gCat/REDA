use serde::{Deserialize, Serialize};

use crate::compile::metrics::{canonical_fingerprint, Fingerprint};

const MANIFEST_POLICY_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub from: Vec<bool>,
    pub to: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionManifest {
    policy_version: u64,
    input_ports: Vec<String>,
    transitions: Vec<Transition>,
}

impl TransitionManifest {
    pub fn new(input_ports: Vec<String>) -> Self {
        let input_count = input_ports.len();
        let transitions = if input_count <= 4 {
            exhaustive_transitions(input_count)
        } else {
            sparse_transitions(input_count)
        };
        TransitionManifest {
            policy_version: MANIFEST_POLICY_VERSION,
            input_ports,
            transitions,
        }
    }

    pub fn input_ports(&self) -> &[String] {
        &self.input_ports
    }

    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    pub fn fingerprint(&self) -> Fingerprint {
        canonical_fingerprint(
            &serde_json::to_vec(self).expect("a transition manifest must serialize"),
        )
    }
}

fn bits_of(mask: usize, width: usize) -> Vec<bool> {
    (0..width)
        .map(|index| (mask >> (width - 1 - index)) & 1 == 1)
        .collect()
}

fn exhaustive_transitions(input_count: usize) -> Vec<Transition> {
    let states = 1usize << input_count;
    let vectors: Vec<Vec<bool>> = (0..states).map(|mask| bits_of(mask, input_count)).collect();
    let mut transitions = Vec::with_capacity(states.saturating_mul(states.saturating_sub(1)));
    for (from_index, from) in vectors.iter().enumerate() {
        for (to_index, to) in vectors.iter().enumerate() {
            if from_index != to_index {
                transitions.push(Transition {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    transitions
}

fn sparse_transitions(input_count: usize) -> Vec<Transition> {
    let mut vectors = vec![vec![false; input_count], vec![true; input_count]];
    for index in 0..input_count {
        let mut one_hot = vec![false; input_count];
        one_hot[index] = true;
        vectors.push(one_hot);
    }
    for index in 0..input_count {
        let mut one_cold = vec![true; input_count];
        one_cold[index] = false;
        vectors.push(one_cold);
    }
    vectors.dedup();

    let mut transitions = Vec::new();
    for from in &vectors {
        for to in &vectors {
            if from
                .iter()
                .zip(to)
                .filter(|(from_bit, to_bit)| from_bit != to_bit)
                .count()
                == 1
            {
                transitions.push(Transition {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    transitions
}

#[cfg(test)]
mod tests {
    use super::TransitionManifest;

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("i{index}")).collect()
    }

    #[test]
    fn four_or_fewer_inputs_emit_every_ordered_distinct_pair_in_numeric_order() {
        let manifest = TransitionManifest::new(vec!["a".into(), "b".into()]);

        assert_eq!(manifest.input_ports(), &["a", "b"]);
        assert_eq!(manifest.transitions().len(), 12);
        assert_eq!(manifest.transitions()[0].from, vec![false, false]);
        assert_eq!(manifest.transitions()[0].to, vec![false, true]);
        assert_eq!(manifest.transitions()[1].from, vec![false, false]);
        assert_eq!(manifest.transitions()[1].to, vec![true, false]);
        assert_eq!(manifest.transitions()[11].from, vec![true, true]);
        assert_eq!(manifest.transitions()[11].to, vec![true, false]);
    }

    #[test]
    fn more_than_four_inputs_emit_only_ordered_single_bit_toggles_of_policy_vectors() {
        let manifest = TransitionManifest::new(names(5));

        assert_eq!(manifest.transitions().len(), 20);
        assert!(manifest.transitions().iter().all(|transition| {
            transition
                .from
                .iter()
                .zip(&transition.to)
                .filter(|(from, to)| from != to)
                .count()
                == 1
        }));
        assert_eq!(manifest.transitions()[0].from, vec![false; 5]);
        assert_eq!(
            manifest.transitions()[0].to,
            vec![true, false, false, false, false]
        );
        assert_eq!(manifest.transitions()[1].from, vec![false; 5]);
        assert_eq!(
            manifest.transitions()[1].to,
            vec![false, true, false, false, false]
        );
    }

    #[test]
    fn manifest_hash_is_repeatable_and_binds_ordered_input_ports() {
        let first = TransitionManifest::new(vec!["a".into(), "b".into()]);
        let repeated = TransitionManifest::new(vec!["a".into(), "b".into()]);
        let reordered = TransitionManifest::new(vec!["b".into(), "a".into()]);

        assert_eq!(first.fingerprint(), repeated.fingerprint());
        assert_ne!(first.fingerprint(), reordered.fingerprint());
        assert!(!first.fingerprint().as_str().is_empty());
    }
}
