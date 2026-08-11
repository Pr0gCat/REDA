use std::cmp::Ordering;

use crate::compile::{self, CompiledCircuit, LegacyEmission, Netlist};

/// A fixed coordinate selected by the planner without referring to a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// One routed connection in a candidate plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    id: String,
    anchors: Vec<Anchor>,
    owner: Option<String>,
    terminal_kinds: Vec<RouteTerminalKind>,
}

/// The physical component selected at a route's final socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTerminalKind {
    RepeaterIntoSupport,
    DirectedDustIntoSupport,
}

impl Route {
    /// Construct immutable route metadata for a candidate or unit test.
    pub fn new(id: impl Into<String>, anchors: Vec<Anchor>) -> Self {
        let mut distinct_anchors = Vec::with_capacity(anchors.len());
        for anchor in anchors {
            if distinct_anchors.last() != Some(&anchor) {
                distinct_anchors.push(anchor);
            }
        }

        Self {
            id: id.into(),
            anchors: distinct_anchors,
            owner: None,
            terminal_kinds: Vec::new(),
        }
    }

    pub(crate) fn from_legacy(
        id: String,
        anchors: Vec<Anchor>,
        terminal_kinds: Vec<RouteTerminalKind>,
    ) -> Self {
        let mut route = Self::new(id.clone(), anchors);
        route.owner = Some(id);
        route.terminal_kinds = terminal_kinds;
        route
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// The source net this route belongs to, if it came from legacy emission.
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// The terminal decisions emitted for this route's sinks.
    pub fn terminal_kinds(&self) -> &[RouteTerminalKind] {
        &self.terminal_kinds
    }
}

/// Immutable planner input.  It deliberately contains no [`World`]-backed
/// state: legacy placement remains outside this candidate model for now.
#[derive(Debug, Clone)]
pub struct PlanCandidate {
    anchors: Vec<Anchor>,
    routes: Vec<Route>,
    legacy_emission: Option<LegacyEmission>,
}

impl PartialEq for PlanCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.anchors == other.anchors && self.routes == other.routes
    }
}

impl Eq for PlanCandidate {}

impl PlanCandidate {
    /// Construct a pure candidate from its selected anchors and route IDs.
    pub fn new(anchors: Vec<Anchor>, routes: Vec<Route>) -> Self {
        Self {
            anchors,
            routes,
            legacy_emission: None,
        }
    }

    pub(crate) fn from_legacy(
        anchors: Vec<Anchor>,
        routes: Vec<Route>,
        legacy_emission: LegacyEmission,
    ) -> Self {
        Self {
            anchors,
            routes,
            legacy_emission: Some(legacy_emission),
        }
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    pub fn cost(&self) -> CostBreakdown {
        CostBreakdown::from_candidate(self)
    }

    /// Score this candidate against itself, which is the normalised seed score.
    pub fn score(&self, weights: &PlannerWeights) -> Result<NormalisedScore, ScoreError> {
        let cost = self.cost();
        cost.normalised_against(&cost, weights)
    }

    /// Score this candidate against immutable seed metadata.
    pub fn score_against(
        &self,
        seed: &PlanCandidate,
        weights: &PlannerWeights,
        effort: PlannerEffort,
    ) -> Result<CandidateScore, ScoreError> {
        self.score_against_at(seed, weights, effort, 0)
    }

    fn score_against_at(
        &self,
        seed: &PlanCandidate,
        weights: &PlannerWeights,
        effort: PlannerEffort,
        original_index: usize,
    ) -> Result<CandidateScore, ScoreError> {
        let cost = self.cost();
        let normalised = cost.normalised_against(&seed.cost(), weights)?;

        Ok(CandidateScore {
            cost,
            normalised,
            effort,
            order: CandidateOrder {
                normalised,
                original_index,
            },
        })
    }
}

/// A legacy compiler output cannot be converted into a legal planner seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerError {
    LegacyMetadataUnavailable,
    NetlistDoesNotMatchCompiledOutput,
    PhysicalInvariant(compile::CompileError),
}

impl std::fmt::Display for PlannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyMetadataUnavailable => write!(f, "compiled circuit has no legacy emission metadata"),
            Self::NetlistDoesNotMatchCompiledOutput => {
                write!(f, "netlist does not match the legacy compiler output")
            }
            Self::PhysicalInvariant(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PlannerError {}

/// Extract a planner seed from the legacy emitter's explicit metadata.
///
/// This intentionally never inspects world blocks to guess route ownership:
/// `emit` recorded each primitive anchor, route owner and terminal decision at
/// the time it made the corresponding placement decision.
pub fn seed_from_legacy(
    netlist: &Netlist,
    compiled: &CompiledCircuit,
) -> Result<PlanCandidate, PlannerError> {
    let emission = compiled
        .legacy_emission()
        .ok_or(PlannerError::LegacyMetadataUnavailable)?;
    if emission.netlist() != netlist {
        return Err(PlannerError::NetlistDoesNotMatchCompiledOutput);
    }

    let routes = emission
        .routes()
        .iter()
        .map(|route| {
            Route::from_legacy(
                route.owner().to_string(),
                route.anchors().to_vec(),
                route.terminal_kinds().to_vec(),
            )
        })
        .collect();

    Ok(PlanCandidate::from_legacy(
        emission.primitive_anchors().to_vec(),
        routes,
        emission.clone(),
    ))
}

/// Realise a legacy seed and run the compiler's physical invariant suite.
pub fn verify_candidate(candidate: &PlanCandidate) -> Result<(), PlannerError> {
    let emission = candidate
        .legacy_emission
        .as_ref()
        .ok_or(PlannerError::LegacyMetadataUnavailable)?;
    compile::verify_legacy_emission(emission).map_err(PlannerError::PhysicalInvariant)
}

/// Integer weights for the candidate cost terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerWeights {
    pub delay: u32,
    pub wire: u32,
    pub space: u32,
    pub turns: u32,
}

impl Default for PlannerWeights {
    fn default() -> Self {
        Self {
            delay: 1,
            wire: 1,
            space: 1,
            turns: 1,
        }
    }
}

/// Reproducibility metadata for a future candidate search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerEffort {
    pub evaluations: usize,
    pub seed: u64,
}

/// The independently-derived cost of one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostBreakdown {
    pub delay: u64,
    pub wire: u64,
    pub space: u64,
    pub turns: u64,
}

/// Exact normalised scoring could not be represented in the fixed-width score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreError {
    NormalisedNumeratorOverflow,
    NormalisedDenominatorOverflow,
    NormalisedWeightOverflow,
}

impl CostBreakdown {
    fn from_candidate(candidate: &PlanCandidate) -> Self {
        let mut cost = Self {
            delay: 0,
            wire: 0,
            space: bounding_volume(candidate),
            turns: 0,
        };

        for route in &candidate.routes {
            cost.delay = cost
                .delay
                .saturating_add(route.anchors.len().saturating_sub(1) as u64);
            cost.wire = cost.wire.saturating_add(route_wire_length(route));
            cost.turns = cost.turns.saturating_add(route_turns(route));
        }

        cost
    }

    /// Return the weighted average of this cost's nonzero seed-normalised
    /// terms.  The pair is reduced and ordered entirely with integers.
    pub fn normalised_against(
        &self,
        seed: &Self,
        weights: &PlannerWeights,
    ) -> Result<NormalisedScore, ScoreError> {
        let terms = [
            (self.delay, seed.delay, weights.delay),
            (self.wire, seed.wire, weights.wire),
            (self.space, seed.space, weights.space),
            (self.turns, seed.turns, weights.turns),
        ];
        let mut numerator = 0_u128;
        let mut denominator = 1_u128;
        let mut total_weight = 0_u128;

        for (value, baseline, weight) in terms {
            if baseline == 0 || weight == 0 {
                continue;
            }

            let baseline = u128::from(baseline);
            let weighted_value = u128::from(weight)
                .checked_mul(u128::from(value))
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            let scaled_numerator = numerator
                .checked_mul(baseline)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            let scaled_value = weighted_value
                .checked_mul(denominator)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            numerator = scaled_numerator
                .checked_add(scaled_value)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            denominator = denominator
                .checked_mul(baseline)
                .ok_or(ScoreError::NormalisedDenominatorOverflow)?;
            let (reduced_numerator, reduced_denominator) = reduce(numerator, denominator);
            numerator = reduced_numerator;
            denominator = reduced_denominator;
            total_weight = total_weight
                .checked_add(u128::from(weight))
                .ok_or(ScoreError::NormalisedWeightOverflow)?;
        }

        if total_weight == 0 {
            return Ok(NormalisedScore::ZERO);
        }

        let denominator = denominator
            .checked_mul(total_weight)
            .ok_or(ScoreError::NormalisedDenominatorOverflow)?;
        Ok(NormalisedScore::new(numerator, denominator))
    }
}

/// A reduced rational total normalised cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalisedScore {
    pub numerator: u128,
    pub denominator: u128,
}

impl NormalisedScore {
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    fn new(numerator: u128, denominator: u128) -> Self {
        debug_assert_ne!(denominator, 0);
        let (numerator, denominator) = reduce(numerator, denominator);
        Self {
            numerator,
            denominator,
        }
    }
}

impl Ord for NormalisedScore {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_fractions(
            self.numerator,
            self.denominator,
            other.numerator,
            other.denominator,
        )
    }
}

impl PartialOrd for NormalisedScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An ordered score plus the original input position, which keeps ties stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandidateOrder {
    pub normalised: NormalisedScore,
    pub original_index: usize,
}

/// One scored candidate and the immutable metadata that made it reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateScore {
    pub cost: CostBreakdown,
    pub normalised: NormalisedScore,
    pub effort: PlannerEffort,
    pub order: CandidateOrder,
}

/// Score candidates against one immutable seed and return them in stable order.
pub fn rank_candidates(
    candidates: &[PlanCandidate],
    seed: &PlanCandidate,
    weights: &PlannerWeights,
    effort: PlannerEffort,
) -> Result<Vec<CandidateScore>, ScoreError> {
    let mut scores = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| candidate.score_against_at(seed, weights, effort, index))
        .collect::<Result<Vec<_>, _>>()?;
    scores.sort_by_key(|score| score.order);
    Ok(scores)
}

fn bounding_volume(candidate: &PlanCandidate) -> u64 {
    let anchors = candidate.anchors.iter().copied().chain(
        candidate
            .routes
            .iter()
            .flat_map(|route| route.anchors.iter().copied()),
    );
    let Some(first) = anchors.clone().next() else {
        return 0;
    };
    let (mut min_x, mut max_x) = (first.x, first.x);
    let (mut min_y, mut max_y) = (first.y, first.y);
    let (mut min_z, mut max_z) = (first.z, first.z);

    for anchor in anchors {
        min_x = min_x.min(anchor.x);
        max_x = max_x.max(anchor.x);
        min_y = min_y.min(anchor.y);
        max_y = max_y.max(anchor.y);
        min_z = min_z.min(anchor.z);
        max_z = max_z.max(anchor.z);
    }

    axis_span(min_x, max_x)
        .saturating_mul(axis_span(min_y, max_y))
        .saturating_mul(axis_span(min_z, max_z))
}

fn axis_span(minimum: i32, maximum: i32) -> u64 {
    (i64::from(maximum) - i64::from(minimum) + 1) as u64
}

fn route_wire_length(route: &Route) -> u64 {
    route
        .anchors
        .windows(2)
        .map(|pair| manhattan_distance(pair[0], pair[1]))
        .fold(0, u64::saturating_add)
}

fn route_turns(route: &Route) -> u64 {
    route
        .anchors
        .windows(3)
        .filter(|window| direction(window[0], window[1]) != direction(window[1], window[2]))
        .count() as u64
}

fn manhattan_distance(left: Anchor, right: Anchor) -> u64 {
    (i64::from(left.x) - i64::from(right.x)).unsigned_abs()
        + (i64::from(left.y) - i64::from(right.y)).unsigned_abs()
        + (i64::from(left.z) - i64::from(right.z)).unsigned_abs()
}

fn direction(from: Anchor, to: Anchor) -> (i8, i8, i8) {
    (
        (to.x > from.x) as i8 - (to.x < from.x) as i8,
        (to.y > from.y) as i8 - (to.y < from.y) as i8,
        (to.z > from.z) as i8 - (to.z < from.z) as i8,
    )
}

fn reduce(numerator: u128, denominator: u128) -> (u128, u128) {
    let divisor = gcd(numerator, denominator);
    (numerator / divisor, denominator / divisor)
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

/// Compare two positive-denominator fractions without multiplying them.
///
/// Equal whole-number parts can be removed before reciprocating both
/// remainders; reciprocation reverses their order.  That keeps the comparison
/// exact even when a cross multiplication would exceed `u128`.
fn compare_fractions(
    mut left_numerator: u128,
    mut left_denominator: u128,
    mut right_numerator: u128,
    mut right_denominator: u128,
) -> Ordering {
    let mut reversed = false;

    loop {
        let whole_order =
            (left_numerator / left_denominator).cmp(&(right_numerator / right_denominator));
        if whole_order != Ordering::Equal {
            return if reversed {
                whole_order.reverse()
            } else {
                whole_order
            };
        }

        let left_remainder = left_numerator % left_denominator;
        let right_remainder = right_numerator % right_denominator;
        match (left_remainder == 0, right_remainder == 0) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if reversed {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if reversed {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {
                left_numerator = left_denominator;
                left_denominator = left_remainder;
                right_numerator = right_denominator;
                right_denominator = right_remainder;
                reversed = !reversed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_seed() -> PlanCandidate {
        PlanCandidate::new(
            vec![
                Anchor { x: 0, y: 0, z: 0 },
                Anchor { x: 2, y: 0, z: 0 },
                Anchor { x: 2, y: 0, z: 3 },
            ],
            vec![Route::new(
                "seed-route",
                vec![
                    Anchor { x: 0, y: 0, z: 0 },
                    Anchor { x: 2, y: 0, z: 0 },
                    Anchor { x: 2, y: 0, z: 3 },
                ],
            )],
        )
    }

    fn fixture_candidate() -> PlanCandidate {
        PlanCandidate::new(
            vec![
                Anchor { x: 0, y: 0, z: 0 },
                Anchor { x: 3, y: 0, z: 0 },
                Anchor { x: 3, y: 0, z: 3 },
            ],
            vec![Route::new(
                "candidate-route",
                vec![
                    Anchor { x: 0, y: 0, z: 0 },
                    Anchor { x: 3, y: 0, z: 0 },
                    Anchor { x: 3, y: 0, z: 3 },
                ],
            )],
        )
    }

    fn run_fixture(seed: u64) -> CandidateScore {
        fixture_candidate()
            .score_against(
                &fixture_seed(),
                &PlannerWeights::default(),
                PlannerEffort {
                    evaluations: 4,
                    seed,
                },
            )
            .expect("fixture costs fit the exact score representation")
    }

    #[test]
    fn a_seed_scores_one_for_every_nonzero_normalised_term() {
        let seed = fixture_seed();
        assert_eq!(
            seed.score(&PlannerWeights::default())
                .expect("fixture costs fit the exact score representation"),
            NormalisedScore::ONE
        );
    }

    #[test]
    fn same_candidate_weights_effort_and_seed_score_identically() {
        assert_eq!(run_fixture(17), run_fixture(17));
    }

    #[test]
    fn cost_comes_from_route_geometry_and_occupied_bounding_volume() {
        assert_eq!(
            fixture_seed().cost(),
            CostBreakdown {
                delay: 2,
                wire: 5,
                space: 12,
                turns: 1,
            }
        );
    }

    #[test]
    fn ranking_keeps_input_order_for_equal_scores() {
        let seed = fixture_seed();
        let candidates = vec![fixture_candidate(), fixture_candidate()];
        let ranked = rank_candidates(
            &candidates,
            &seed,
            &PlannerWeights::default(),
            PlannerEffort {
                evaluations: 4,
                seed: 17,
            },
        )
        .expect("fixture costs fit the exact score representation");

        assert_eq!(
            ranked
                .iter()
                .map(|score| score.order.original_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn normalised_score_ordering_does_not_collapse_large_distinct_fractions() {
        let just_above_one = NormalisedScore {
            numerator: u128::MAX,
            denominator: u128::MAX - 1,
        };
        let just_below_one = NormalisedScore {
            numerator: u128::MAX - 1,
            denominator: u128::MAX,
        };

        assert!(just_above_one > just_below_one);
    }

    #[test]
    fn normalisation_rejects_two_large_terms_that_cannot_be_averaged_exactly() {
        let cost = CostBreakdown {
            delay: 1,
            wire: 1,
            space: 0,
            turns: 0,
        };
        let seed = CostBreakdown {
            delay: u64::MAX,
            wire: u64::MAX - 1,
            space: 0,
            turns: 0,
        };

        assert_eq!(
            cost.normalised_against(&seed, &PlannerWeights::default()),
            Err(ScoreError::NormalisedDenominatorOverflow)
        );
    }

    #[test]
    fn normalisation_rejects_three_large_terms_that_cannot_be_accumulated_exactly() {
        let cost = CostBreakdown {
            delay: 1,
            wire: 1,
            space: 1,
            turns: 0,
        };
        let seed = CostBreakdown {
            delay: u64::MAX,
            wire: u64::MAX - 1,
            space: u64::MAX - 2,
            turns: 0,
        };

        assert_eq!(
            cost.normalised_against(&seed, &PlannerWeights::default()),
            Err(ScoreError::NormalisedNumeratorOverflow)
        );
    }

    #[test]
    fn repeated_adjacent_anchors_do_not_add_delay_or_turns() {
        let a = Anchor { x: 0, y: 0, z: 0 };
        let b = Anchor { x: 1, y: 0, z: 0 };
        let candidate = PlanCandidate::new(vec![], vec![Route::new("degenerate", vec![a, a, b])]);

        assert_eq!(candidate.routes()[0].anchors(), &[a, b]);
        assert_eq!(
            candidate.cost(),
            CostBreakdown {
                delay: 1,
                wire: 1,
                space: 2,
                turns: 0,
            }
        );
    }
}
