use serde::Serialize;

use crate::compile::fragment_synth::manifest::TransitionManifestKind;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::routing::RouterLimits;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchConfig {
    pub router_limits: RouterLimits,
    pub max_seed_shell_radius: u32,
    pub max_fragment_shell_radius: u32,
    pub max_seed_backtracks: u64,
    pub max_fragment_backtracks_per_proposal: u64,
    pub max_equivalence_proof_steps: u64,
    pub max_certification_transitions: u64,
    pub max_simulator_events_per_transition: u64,
    pub max_game_ticks_per_transition: u64,
    pub fragment_instance_schedule: Vec<u16>,
    pub max_boundary_nets: u16,
    pub max_fragment_manhattan_radius: u32,
}

impl SearchConfig {
    pub fn checked_defaults() -> Self {
        Self {
            router_limits: RouterLimits {
                max_node_expansions: 262_144,
                max_queue_entries: 262_144,
            },
            max_seed_shell_radius: 64,
            max_fragment_shell_radius: 32,
            max_seed_backtracks: 1_000_000,
            max_fragment_backtracks_per_proposal: 100_000,
            max_equivalence_proof_steps: 1_000_000,
            max_certification_transitions: 65_536,
            max_simulator_events_per_transition: 1_000_000,
            max_game_ticks_per_transition: 2_048,
            fragment_instance_schedule: vec![1, 2, 4, 8],
            max_boundary_nets: 32,
            max_fragment_manhattan_radius: 48,
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        canonical_fingerprint(
            &serde_json::to_vec(self).expect("search configuration must serialize"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertificationConfig {
    pub exhaustive_input_threshold: u16,
    pub transition_manifest_kind: TransitionManifestKind,
    pub max_equivalence_proof_steps: u64,
    pub max_certification_transitions: u64,
    pub max_simulator_events_per_transition: u64,
    pub max_game_ticks_per_transition: u64,
}

impl CertificationConfig {
    pub fn from_search(search: &SearchConfig) -> Self {
        Self {
            exhaustive_input_threshold: 8,
            transition_manifest_kind: TransitionManifestKind::FixedV1,
            max_equivalence_proof_steps: search.max_equivalence_proof_steps,
            max_certification_transitions: search.max_certification_transitions,
            max_simulator_events_per_transition: search.max_simulator_events_per_transition,
            max_game_ticks_per_transition: search.max_game_ticks_per_transition,
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        canonical_fingerprint(
            &serde_json::to_vec(self).expect("certification configuration must serialize"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{CertificationConfig, SearchConfig};

    #[test]
    fn checked_defaults_are_the_fixed_search_contract() {
        let config = SearchConfig::checked_defaults();

        assert_eq!(config.router_limits.max_node_expansions, 262_144);
        assert_eq!(config.router_limits.max_queue_entries, 262_144);
        assert_eq!(config.max_seed_shell_radius, 64);
        assert_eq!(config.max_fragment_shell_radius, 32);
        assert_eq!(config.max_seed_backtracks, 1_000_000);
        assert_eq!(config.max_fragment_backtracks_per_proposal, 100_000);
        assert_eq!(config.max_equivalence_proof_steps, 1_000_000);
        assert_eq!(config.max_certification_transitions, 65_536);
        assert_eq!(config.max_simulator_events_per_transition, 1_000_000);
        assert_eq!(config.max_game_ticks_per_transition, 2_048);
        assert_eq!(config.fragment_instance_schedule, [1, 2, 4, 8]);
        assert_eq!(config.max_boundary_nets, 32);
        assert_eq!(config.max_fragment_manhattan_radius, 48);
    }

    #[test]
    fn every_search_field_changes_the_canonical_fingerprint() {
        let base = SearchConfig::checked_defaults();
        let fingerprint = base.fingerprint();
        let mut variants = Vec::new();

        let mut changed = base.clone();
        changed.router_limits.max_node_expansions += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.router_limits.max_queue_entries += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_seed_shell_radius += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_shell_radius += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_seed_backtracks += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_backtracks_per_proposal += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_equivalence_proof_steps += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_certification_transitions += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_simulator_events_per_transition += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_game_ticks_per_transition += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.fragment_instance_schedule.push(16);
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_boundary_nets += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_manhattan_radius += 1;
        variants.push(changed);

        assert_eq!(variants.len(), 13);
        for variant in variants {
            assert_ne!(variant.fingerprint(), fingerprint);
        }
    }

    #[test]
    fn certification_policy_copies_caps_and_has_its_own_fingerprint() {
        let search = SearchConfig::checked_defaults();
        let config = CertificationConfig::from_search(&search);

        assert_eq!(config.exhaustive_input_threshold, 8);
        assert_eq!(
            config.max_equivalence_proof_steps,
            search.max_equivalence_proof_steps
        );
        assert_eq!(
            config.max_certification_transitions,
            search.max_certification_transitions
        );
        assert_eq!(
            config.max_simulator_events_per_transition,
            search.max_simulator_events_per_transition
        );
        assert_eq!(
            config.max_game_ticks_per_transition,
            search.max_game_ticks_per_transition
        );

        let fingerprint = config.fingerprint();
        let mut variants = Vec::new();
        let mut changed = config.clone();
        changed.exhaustive_input_threshold += 1;
        variants.push(changed);
        let mut changed = config.clone();
        changed.max_equivalence_proof_steps += 1;
        variants.push(changed);
        let mut changed = config.clone();
        changed.max_certification_transitions += 1;
        variants.push(changed);
        let mut changed = config.clone();
        changed.max_simulator_events_per_transition += 1;
        variants.push(changed);
        let mut changed = config.clone();
        changed.max_game_ticks_per_transition += 1;
        variants.push(changed);
        for variant in variants {
            assert_ne!(variant.fingerprint(), fingerprint);
        }
    }
}
