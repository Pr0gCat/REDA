//! A tagged CNF builder and a CDCL SAT solver, written here because the crate
//! takes no dependencies.
//!
//! **Why this file exists at all.** Every routing method on this branch reports
//! "no safe local route from A to B", which says only *I did not find one*. A
//! complete decision procedure can say the other thing -- *these constraints
//! cannot hold simultaneously, and here they are*. That is the whole reason the
//! windowed model in `planner`'s test module is worth building, and it needs a
//! solver that is **complete**: one that distinguishes `Unsat` from `Unknown`
//! and never returns the first when it means the second.
//!
//! **The dependency decision, stated rather than assumed.** The plan's Global
//! Constraints (`docs/superpowers/plans/2026-08-13-spring-placement.md`, line
//! 42) say "**No new dependencies.** The crate has none for linear algebra and
//! compiles to wasm; the solver is sixty lines of dense Cholesky written here."
//! Task 12 carved out exactly one exception and carved it narrowly:
//! `wasm-bindgen-test` is a **dev**-dependency of `viewer/` only, and the report
//! measured that it never reaches `viewer/pkg/`. This file is a
//! `#[cfg(test)]`-only module of the *root* crate, which is a different shape
//! from that carve-out, so rather than stretch someone else's exception the
//! constraint is simply honoured: a small CDCL is written here, the windows are
//! small, and the tree gains no `Cargo.toml` line at all.
//!
//! **Everything below is generic SAT machinery and knows nothing about
//! redstone.** That separation is deliberate: the game's rules are stated once,
//! in `planner`'s window builder, against the shipping predicates. A solver that
//! knew about dust would be a second statement of the rules, which is the top
//! risk this whole phase exists to manage.
//!
//! This module is `#[cfg(test)]`, so it is compiled by `cargo test` and by
//! `cargo clippy --all-targets` and ships in nothing.

use std::collections::{BTreeMap, BTreeSet};

/// A literal: `v` for the positive form of variable `v`, `-v` for its negation.
/// Variables are numbered from 1, so `0` is never a literal.
pub type Lit = i32;

/// Which group of the model a clause came from.
///
/// The point of the tag is the unsatisfiable core: "UNSAT" alone is exactly the
/// uninformative answer this work exists to replace. A core named in terms of
/// the model's own vocabulary -- *the placement must be somewhere*, *these two
/// nets need clearance*, *this net has to reach its socket* -- is a statement
/// about redstone rather than about a CNF.
pub type Group = usize;

/// A CNF, plus the name of the constraint group each clause belongs to.
#[derive(Debug, Clone, Default)]
pub struct Cnf {
    vars: usize,
    groups: Vec<String>,
    clauses: Vec<(Vec<Lit>, Group)>,
}

impl Cnf {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh variable. Numbering starts at 1.
    pub fn var(&mut self) -> i32 {
        self.vars += 1;
        self.vars as i32
    }

    pub fn vars(&self) -> usize {
        self.vars
    }

    pub fn clause_count(&self) -> usize {
        self.clauses.len()
    }

    /// Declare a constraint group, returning its handle.
    pub fn group(&mut self, name: impl Into<String>) -> Group {
        self.groups.push(name.into());
        self.groups.len() - 1
    }

    pub fn group_name(&self, group: Group) -> &str {
        &self.groups[group]
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Add a clause. Tautologies (`x` and `-x` together) are dropped, because
    /// they constrain nothing and would only make a core look larger than it is.
    pub fn add(&mut self, literals: impl IntoIterator<Item = Lit>, group: Group) {
        let mut seen: BTreeSet<Lit> = BTreeSet::new();
        for literal in literals {
            debug_assert!(literal != 0, "0 is not a literal");
            debug_assert!(
                literal.unsigned_abs() as usize <= self.vars,
                "literal {literal} names a variable that was never allocated"
            );
            if seen.contains(&-literal) {
                return;
            }
            seen.insert(literal);
        }
        self.clauses.push((seen.into_iter().collect(), group));
    }

    /// `left -> right`.
    pub fn implies(&mut self, left: Lit, right: Lit, group: Group) {
        self.add([-left, right], group);
    }

    /// At most one of `literals` is true, by the sequential (ladder) encoding:
    /// `n - 1` auxiliary variables and `3(n - 1)` clauses, rather than the
    /// `n(n-1)/2` a pairwise encoding would need. A window offers hundreds of
    /// candidate landings, where pairwise would be hundreds of thousands of
    /// clauses.
    pub fn at_most_one(&mut self, literals: &[Lit], group: Group) {
        if literals.len() < 2 {
            return;
        }
        let mut carry: Vec<i32> = Vec::with_capacity(literals.len() - 1);
        for _ in 0..literals.len() - 1 {
            carry.push(self.var());
        }
        // s[0] >= x[0]
        self.add([-literals[0], carry[0]], group);
        for index in 1..literals.len() {
            if index < literals.len() - 1 {
                // s[i] >= x[i], s[i] >= s[i-1]
                self.add([-literals[index], carry[index]], group);
                self.add([-carry[index - 1], carry[index]], group);
            }
            // not (x[i] and s[i-1])
            self.add([-literals[index], -carry[index - 1]], group);
        }
    }

    pub fn exactly_one(&mut self, literals: &[Lit], group: Group) {
        self.add(literals.iter().copied(), group);
        self.at_most_one(literals, group);
    }

    /// Solve with every group enabled.
    pub fn solve(&self, budget: u64) -> Outcome {
        let all: BTreeSet<Group> = (0..self.groups.len()).collect();
        self.solve_groups(&all, budget)
    }

    /// Solve using only the clauses whose group is in `enabled`.
    pub fn solve_groups(&self, enabled: &BTreeSet<Group>, budget: u64) -> Outcome {
        let mut solver = Solver::new(self.vars);
        for (clause, group) in &self.clauses {
            if !enabled.contains(group) {
                continue;
            }
            if !solver.add_clause(clause) {
                return Outcome::Unsat;
            }
        }
        let outcome = solver.solve(budget);
        // **A wrong SAT is the one failure mode this whole exercise exists to
        // avoid**, and it is the cheap one to rule out: reading the answer back
        // against the clauses costs one pass and turns any defect in the watched
        // literals, the backjump or the database reduction into a panic here
        // rather than a plan the verifier rejects three stages later. UNSAT has
        // no such check, which is why it is the answer the known-answer tests in
        // this module concentrate on.
        if let Outcome::Sat(model) = &outcome {
            for (clause, group) in &self.clauses {
                if !enabled.contains(group) {
                    continue;
                }
                assert!(
                    clause
                        .iter()
                        .any(|&literal| value(model, literal.abs()) == (literal > 0)),
                    "the solver returned a model that does not satisfy {clause:?}                      from group `{}`",
                    self.groups[*group]
                );
            }
        }
        outcome
    }

    /// A minimal unsatisfiable subset of the *groups*, by deletion.
    ///
    /// Each group is dropped in turn and the rest re-solved; a group whose
    /// absence leaves the formula unsatisfiable was not needed and stays out.
    /// What survives is a set of groups that is unsatisfiable together and
    /// satisfiable if any one of them is removed -- which is the statement
    /// "these constraints cannot hold simultaneously" with the constraints
    /// named.
    ///
    /// Returns `None` if the full formula is not unsatisfiable, or if any
    /// intermediate solve exceeds its budget -- an `Unknown` in the middle of a
    /// deletion loop would silently produce a core that is not one.
    pub fn core(&self, budget: u64) -> Option<Vec<Group>> {
        let mut keep: BTreeSet<Group> = (0..self.groups.len()).collect();
        if !matches!(self.solve_groups(&keep, budget), Outcome::Unsat) {
            return None;
        }
        for group in 0..self.groups.len() {
            if !keep.contains(&group) {
                continue;
            }
            let mut without = keep.clone();
            without.remove(&group);
            match self.solve_groups(&without, budget) {
                Outcome::Unsat => keep = without,
                Outcome::Sat(_) => {}
                Outcome::Unknown => return None,
            }
        }
        Some(keep.into_iter().collect())
    }

    /// DIMACS, for feeding an instance to an outside checker by hand.
    pub fn dimacs(&self) -> String {
        let mut text = format!("p cnf {} {}\n", self.vars, self.clauses.len());
        for (clause, _) in &self.clauses {
            for literal in clause {
                text.push_str(&literal.to_string());
                text.push(' ');
            }
            text.push_str("0\n");
        }
        text
    }
}

/// What a solve concluded. **`Unknown` is not `Unsat`**, and keeping them apart
/// is the one thing this type exists for: a budget-exceeded search reported as
/// "infeasible" is exactly the wrong answer this work was commissioned to stop
/// producing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sat(Vec<bool>),
    Unsat,
    Unknown,
}

impl Outcome {
    pub fn model(&self) -> Option<&[bool]> {
        match self {
            Outcome::Sat(model) => Some(model),
            _ => None,
        }
    }
}

/// Read a variable's value out of a model. Variables are 1-based; the model is
/// 0-based.
pub fn value(model: &[bool], variable: i32) -> bool {
    model[variable as usize - 1]
}

const UNDEF: i8 = -1;

fn index_of(literal: Lit) -> usize {
    let variable = literal.unsigned_abs() as usize - 1;
    2 * variable + usize::from(literal < 0)
}

/// A CDCL solver: two watched literals, 1UIP learning with recursive
/// minimisation, EVSIDS-style activity, phase saving, Luby restarts and a
/// clause-database reduction.
struct Solver {
    vars: usize,
    clauses: Vec<Vec<Lit>>,
    learnt: Vec<bool>,
    activity_clause: Vec<f64>,
    watches: Vec<Vec<usize>>,
    assign: Vec<i8>,
    level: Vec<u32>,
    reason: Vec<i32>,
    trail: Vec<Lit>,
    trail_lim: Vec<usize>,
    head: usize,
    activity: Vec<f64>,
    bump: f64,
    phase: Vec<bool>,
    /// A max-heap of unassigned variables by activity, with each variable's
    /// position kept alongside so a bump is a sift rather than a search.
    ///
    /// This was a `BTreeSet<(key, var)>` and the difference is not cosmetic: a
    /// conflict bumps tens of variables, each bump was a remove and an insert
    /// through a tree of tens of thousands of nodes, and the windowed model has
    /// enough variables for that to dominate the search. Measured on `and4`'s
    /// `g3` window, which did not finish 200,000 conflicts in seven minutes
    /// before this and the two allocations below were removed.
    heap: Vec<usize>,
    position: Vec<usize>,
    /// Conflict-analysis scratch, kept across conflicts and cleared by the
    /// entries it touched. Allocating it per conflict is `O(vars)` work on every
    /// one of them, which on a windowed model with tens of thousands of
    /// variables dominates everything else the solver does.
    seen: Vec<bool>,
    seen_touched: Vec<usize>,
    conflicts: u64,
    unsat: bool,
}

impl Solver {
    fn new(vars: usize) -> Self {
        Solver {
            vars,
            clauses: Vec::new(),
            learnt: Vec::new(),
            activity_clause: Vec::new(),
            watches: vec![Vec::new(); 2 * vars],
            assign: vec![UNDEF; vars],
            level: vec![0; vars],
            reason: vec![-1; vars],
            trail: Vec::new(),
            trail_lim: Vec::new(),
            head: 0,
            activity: vec![0.0; vars],
            bump: 1.0,
            phase: vec![false; vars],
            heap: Vec::with_capacity(vars),
            position: vec![usize::MAX; vars],
            seen: vec![false; vars],
            seen_touched: Vec::new(),
            conflicts: 0,
            unsat: false,
        }
    }

    fn value(&self, literal: Lit) -> i8 {
        let variable = literal.unsigned_abs() as usize - 1;
        match self.assign[variable] {
            UNDEF => UNDEF,
            assigned => {
                if literal > 0 {
                    assigned
                } else {
                    1 - assigned
                }
            }
        }
    }

    fn decision_level(&self) -> u32 {
        self.trail_lim.len() as u32
    }

    /// Sift `at` up until its parent outranks it. Ties break on the variable
    /// index, so two runs of the same formula make the same decisions.
    fn sift_up(&mut self, mut at: usize) {
        while at > 0 {
            let parent = (at - 1) / 2;
            if self.outranks(self.heap[at], self.heap[parent]) {
                self.swap_heap(at, parent);
                at = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut at: usize) {
        loop {
            let left = 2 * at + 1;
            if left >= self.heap.len() {
                break;
            }
            let right = left + 1;
            let mut best = left;
            if right < self.heap.len() && self.outranks(self.heap[right], self.heap[left]) {
                best = right;
            }
            if self.outranks(self.heap[best], self.heap[at]) {
                self.swap_heap(at, best);
                at = best;
            } else {
                break;
            }
        }
    }

    fn outranks(&self, left: usize, right: usize) -> bool {
        let (first, second) = (self.activity[left], self.activity[right]);
        first > second || (first == second && left < right)
    }

    fn swap_heap(&mut self, left: usize, right: usize) {
        self.heap.swap(left, right);
        self.position[self.heap[left]] = left;
        self.position[self.heap[right]] = right;
    }

    fn enqueue_order(&mut self, variable: usize) {
        if self.position[variable] != usize::MAX || self.assign[variable] != UNDEF {
            return;
        }
        self.heap.push(variable);
        self.position[variable] = self.heap.len() - 1;
        self.sift_up(self.heap.len() - 1);
    }

    fn bump_var(&mut self, variable: usize) {
        self.activity[variable] += self.bump;
        if self.activity[variable] > 1e100 {
            for slot in &mut self.activity {
                *slot *= 1e-100;
            }
            self.bump *= 1e-100;
        }
        if self.position[variable] != usize::MAX {
            let at = self.position[variable];
            self.sift_up(at);
        }
    }

    /// Returns false when the clause makes the formula unsatisfiable outright.
    fn add_clause(&mut self, clause: &[Lit]) -> bool {
        if self.unsat {
            return false;
        }
        let mut literals: Vec<Lit> = Vec::with_capacity(clause.len());
        for &literal in clause {
            match self.value(literal) {
                1 if self.level[literal.unsigned_abs() as usize - 1] == 0 => return true,
                0 if self.level[literal.unsigned_abs() as usize - 1] == 0 => continue,
                _ => {
                    if !literals.contains(&literal) {
                        literals.push(literal);
                    }
                }
            }
        }
        if literals.is_empty() {
            self.unsat = true;
            return false;
        }
        if literals.len() == 1 {
            if !self.enqueue(literals[0], -1) {
                self.unsat = true;
                return false;
            }
            return self.propagate().is_none();
        }
        self.attach(literals, false);
        true
    }

    fn attach(&mut self, literals: Vec<Lit>, learnt: bool) -> usize {
        let index = self.clauses.len();
        self.watches[index_of(literals[0])].push(index);
        self.watches[index_of(literals[1])].push(index);
        self.clauses.push(literals);
        self.learnt.push(learnt);
        self.activity_clause.push(0.0);
        index
    }

    fn enqueue(&mut self, literal: Lit, reason: i32) -> bool {
        match self.value(literal) {
            UNDEF => {
                let variable = literal.unsigned_abs() as usize - 1;
                self.assign[variable] = i8::from(literal > 0);
                self.level[variable] = self.decision_level();
                self.reason[variable] = reason;
                self.trail.push(literal);
                true
            }
            1 => true,
            _ => false,
        }
    }

    /// Unit propagation. Returns the index of a conflicting clause, if any.
    fn propagate(&mut self) -> Option<usize> {
        while self.head < self.trail.len() {
            let literal = self.trail[self.head];
            self.head += 1;
            // Clauses watching `-literal`, which has just become false.
            let slot = index_of(-literal);
            // Compacted in place rather than into a fresh `keep` vector: this
            // runs once per assigned literal, and a heap allocation on that path
            // is the single most expensive thing a propagation-heavy formula
            // does. `take` swaps in an empty vector without allocating, and the
            // list is handed straight back below.
            let mut watching = std::mem::take(&mut self.watches[slot]);
            let mut conflict = None;
            let mut read = 0usize;
            let mut write = 0usize;
            while read < watching.len() {
                let index = watching[read];
                read += 1;
                let false_literal = -literal;
                if self.clauses[index][0] == false_literal {
                    self.clauses[index].swap(0, 1);
                }
                let first = self.clauses[index][0];
                if self.value(first) == 1 {
                    watching[write] = index;
                    write += 1;
                    continue;
                }
                let mut moved = false;
                for position in 2..self.clauses[index].len() {
                    let candidate = self.clauses[index][position];
                    if self.value(candidate) != 0 {
                        self.clauses[index].swap(1, position);
                        self.watches[index_of(self.clauses[index][1])].push(index);
                        moved = true;
                        break;
                    }
                }
                if moved {
                    continue;
                }
                watching[write] = index;
                write += 1;
                if !self.enqueue(first, index as i32) {
                    conflict = Some(index);
                    break;
                }
            }
            while read < watching.len() {
                watching[write] = watching[read];
                read += 1;
                write += 1;
            }
            watching.truncate(write);
            // A watch moved during the loop can only land on a literal that is
            // not false, and the literal whose list this is is false, so nothing
            // was pushed back into this slot -- but taking whatever is there
            // costs nothing and makes that reasoning unnecessary.
            let pushed = std::mem::take(&mut self.watches[slot]);
            watching.extend(pushed);
            self.watches[slot] = watching;
            if conflict.is_some() {
                self.head = self.trail.len();
                return conflict;
            }
        }
        None
    }

    fn cancel_until(&mut self, level: u32) {
        while self.decision_level() > level {
            let limit = *self.trail_lim.last().expect("a level to undo");
            while self.trail.len() > limit {
                let literal = self.trail.pop().expect("a literal to undo");
                let variable = literal.unsigned_abs() as usize - 1;
                self.phase[variable] = self.assign[variable] == 1;
                self.assign[variable] = UNDEF;
                self.reason[variable] = -1;
                self.enqueue_order(variable);
            }
            self.trail_lim.pop();
        }
        self.head = self.trail.len();
    }

    /// 1UIP conflict analysis, with recursive minimisation of the learnt clause.
    fn analyze(&mut self, conflict: usize) -> (Vec<Lit>, u32) {
        for &variable in &self.seen_touched {
            self.seen[variable] = false;
        }
        self.seen_touched.clear();
        let mut learnt: Vec<Lit> = vec![0];
        let mut counter = 0usize;
        let mut literal: Option<Lit> = None;
        let mut index = self.trail.len();
        let mut clause = conflict;

        loop {
            self.activity_clause[clause] += 1.0;
            let start = usize::from(literal.is_some());
            for position in start..self.clauses[clause].len() {
                let other = self.clauses[clause][position];
                let variable = other.unsigned_abs() as usize - 1;
                if self.seen[variable] || self.level[variable] == 0 {
                    continue;
                }
                self.seen[variable] = true;
                self.seen_touched.push(variable);
                self.bump_var(variable);
                if self.level[variable] >= self.decision_level() {
                    counter += 1;
                } else {
                    learnt.push(other);
                }
            }

            loop {
                index -= 1;
                let variable = self.trail[index].unsigned_abs() as usize - 1;
                if self.seen[variable] {
                    break;
                }
            }
            let chosen = self.trail[index];
            let variable = chosen.unsigned_abs() as usize - 1;
            self.seen[variable] = false;
            counter -= 1;
            if counter == 0 {
                learnt[0] = -chosen;
                break;
            }
            literal = Some(chosen);
            clause = self.reason[variable] as usize;
        }

        // Recursive minimisation: drop a literal whose reason is itself already
        // implied by the rest of the clause.
        let mut minimised: Vec<Lit> = Vec::with_capacity(learnt.len());
        minimised.push(learnt[0]);
        for &other in &learnt[1..] {
            let variable = other.unsigned_abs() as usize - 1;
            if self.reason[variable] < 0 || !self.redundant(variable) {
                minimised.push(other);
            }
        }

        let backtrack = if minimised.len() == 1 {
            0
        } else {
            let mut best = 1usize;
            for position in 2..minimised.len() {
                let here = self.level[minimised[position].unsigned_abs() as usize - 1];
                let there = self.level[minimised[best].unsigned_abs() as usize - 1];
                if here > there {
                    best = position;
                }
            }
            minimised.swap(1, best);
            self.level[minimised[1].unsigned_abs() as usize - 1]
        };
        (minimised, backtrack)
    }

    /// Whether `variable`'s antecedents are all already named by the learnt
    /// clause (or settled at level 0), so keeping it adds nothing.
    ///
    /// Local rather than recursive, and allocation-free. The recursive form was
    /// written first and needed a fresh stack and visited set per literal per
    /// conflict, which on a model with tens of thousands of variables costs more
    /// than the shorter clause saves. Dropping a literal this calls redundant is
    /// sound either way; keeping one it cannot prove redundant only makes the
    /// learnt clause weaker.
    fn redundant(&self, variable: usize) -> bool {
        let reason = self.reason[variable];
        if reason < 0 {
            return false;
        }
        self.clauses[reason as usize][1..].iter().all(|&other| {
            let next = other.unsigned_abs() as usize - 1;
            self.seen[next] || self.level[next] == 0
        })
    }

    fn pick(&mut self) -> Option<Lit> {
        while !self.heap.is_empty() {
            let variable = self.heap[0];
            let last = self.heap.len() - 1;
            self.swap_heap(0, last);
            self.heap.pop();
            self.position[variable] = usize::MAX;
            if !self.heap.is_empty() {
                self.sift_down(0);
            }
            if self.assign[variable] == UNDEF {
                let literal = variable as i32 + 1;
                return Some(if self.phase[variable] { literal } else { -literal });
            }
        }
        None
    }

    fn reduce(&mut self) {
        let mut removable: Vec<(u64, usize)> = Vec::new();
        for index in 0..self.clauses.len() {
            if !self.learnt[index] || self.clauses[index].len() <= 2 {
                continue;
            }
            // A clause that is somebody's reason right now cannot be dropped.
            let first = self.clauses[index][0];
            let variable = first.unsigned_abs() as usize - 1;
            if self.reason[variable] == index as i32 {
                continue;
            }
            let score = (self.activity_clause[index] * 1000.0) as u64;
            removable.push((score, index));
        }
        removable.sort_unstable();
        let drop_count = removable.len() / 2;
        let doomed: BTreeSet<usize> =
            removable.into_iter().take(drop_count).map(|(_, index)| index).collect();
        if doomed.is_empty() {
            return;
        }
        // Rebuild rather than patch: indices shift, and a stale watch index is
        // the kind of defect that shows up as a wrong answer rather than a
        // crash.
        let mut kept: Vec<Vec<Lit>> = Vec::with_capacity(self.clauses.len() - doomed.len());
        let mut kept_learnt: Vec<bool> = Vec::with_capacity(kept.capacity());
        let mut kept_activity: Vec<f64> = Vec::with_capacity(kept.capacity());
        let mut remap: BTreeMap<usize, usize> = BTreeMap::new();
        for index in 0..self.clauses.len() {
            if doomed.contains(&index) {
                continue;
            }
            remap.insert(index, kept.len());
            kept.push(std::mem::take(&mut self.clauses[index]));
            kept_learnt.push(self.learnt[index]);
            kept_activity.push(self.activity_clause[index]);
        }
        self.clauses = kept;
        self.learnt = kept_learnt;
        self.activity_clause = kept_activity;
        for slot in &mut self.watches {
            slot.clear();
        }
        // Positions 0 and 1 are re-watched as they stand, and that is correct
        // rather than convenient -- **a claim that was doubted, tested and then
        // established rather than assumed**. The worry was that a rebuild at
        // level 0 could leave a watch on a literal level 0 has already
        // falsified, which never fires again and makes the clause invisible.
        // Two things rule it out. Positions 0 and 1 *are* the watched literals
        // by construction: `attach` watches them and `propagate` maintains it
        // through its two swaps. And two-watched-literals' own invariant -- a
        // watched literal is false only when the other watched literal is true
        // -- survives backtracking, because undoing assignments only ever makes
        // a literal less false. Re-choosing the watches defensively was written,
        // and then reverted: injecting the "defect" left 4,000 random 3-SAT
        // instances at 16 variables agreeing with exhaustive search on every
        // one, and there is a reason it cannot bite -- a learnt clause is a
        // consequence of the original ones, so losing one costs search time and
        // can never change an answer.
        for index in 0..self.clauses.len() {
            let zero = index_of(self.clauses[index][0]);
            let one = index_of(self.clauses[index][1]);
            self.watches[zero].push(index);
            self.watches[one].push(index);
        }
        for variable in 0..self.vars {
            let reason = self.reason[variable];
            if reason >= 0 {
                self.reason[variable] = match remap.get(&(reason as usize)) {
                    Some(&fresh) => fresh as i32,
                    None => -1,
                };
            }
        }
    }

    fn solve(&mut self, budget: u64) -> Outcome {
        if self.unsat {
            return Outcome::Unsat;
        }
        for variable in 0..self.vars {
            self.enqueue_order(variable);
        }
        if self.propagate().is_some() {
            return Outcome::Unsat;
        }

        let mut restart = 0u32;
        let mut until = luby(restart) * 100;
        let mut since_restart = 0u64;
        // MiniSat's own starting point, a third of the problem, and low enough
        // that the small formulas in this module's tests reduce many times over
        // -- which is the point: the reduction rebuilds the clause database and
        // re-chooses every watch, and a path that only ever runs on the largest
        // instance is a path nothing checks. The 200 random 3-SAT instances
        // below are each compared against exhaustive search with this at 64.
        let mut max_learnt = (self.clauses.len() / 3).max(64);

        loop {
            match self.propagate() {
                Some(conflict) => {
                    self.conflicts += 1;
                    since_restart += 1;
                    if self.decision_level() == 0 {
                        return Outcome::Unsat;
                    }
                    let (learnt, backtrack) = self.analyze(conflict);
                    self.cancel_until(backtrack);
                    if learnt.len() == 1 {
                        if !self.enqueue(learnt[0], -1) {
                            return Outcome::Unsat;
                        }
                    } else {
                        let index = self.attach(learnt, true);
                        let first = self.clauses[index][0];
                        if !self.enqueue(first, index as i32) {
                            return Outcome::Unsat;
                        }
                    }
                    self.bump *= 1.0 / 0.95;
                    if self.conflicts >= budget {
                        return Outcome::Unknown;
                    }
                }
                None => {
                    if since_restart >= until {
                        since_restart = 0;
                        restart += 1;
                        until = luby(restart) * 100;
                        self.cancel_until(0);
                        let learnt_now = self.learnt.iter().filter(|is| **is).count();
                        if learnt_now > max_learnt {
                            self.reduce();
                            max_learnt += max_learnt / 2;
                        }
                        continue;
                    }
                    match self.pick() {
                        None => {
                            let model =
                                (0..self.vars).map(|variable| self.assign[variable] == 1).collect();
                            return Outcome::Sat(model);
                        }
                        Some(literal) => {
                            self.trail_lim.push(self.trail.len());
                            self.enqueue(literal, -1);
                        }
                    }
                }
            }
        }
    }
}

/// The Luby sequence, which is what makes restarts provably not cost
/// completeness: every finite run length recurs infinitely often.
fn luby(mut index: u32) -> u64 {
    let mut size = 1u32;
    let mut sequence = 0u32;
    while size < index + 1 {
        sequence += 1;
        size = 2 * size + 1;
    }
    while size != index + 1 {
        size = (size - 1) / 2;
        sequence -= 1;
        index %= size;
    }
    1u64 << sequence
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pigeons` pigeons into `holes` holes: every pigeon takes a hole, no hole
    /// takes two. Unsatisfiable exactly when `pigeons > holes`, which is what
    /// makes it the standard test of a *complete* solver -- every assignment has
    /// to be refuted, so a search that stops early gets it wrong and a
    /// propagation defect that loses a clause reports SAT.
    fn pigeonhole(pigeons: usize, holes: usize) -> (Cnf, Vec<Vec<i32>>) {
        let mut cnf = Cnf::new();
        let group = cnf.group("pigeonhole");
        let mut place = vec![vec![0i32; holes]; pigeons];
        for pigeon in place.iter_mut() {
            for hole in pigeon.iter_mut() {
                *hole = cnf.var();
            }
        }
        for pigeon in &place {
            cnf.add(pigeon.iter().copied(), group);
        }
        for left in 0..pigeons {
            for right in left + 1..pigeons {
                for (&here, &there) in place[left].iter().zip(&place[right]) {
                    cnf.add([-here, -there], group);
                }
            }
        }
        (cnf, place)
    }

    /// The solver's own known answers. A model that cannot reproduce answers we
    /// already know is not trustworthy on answers we do not -- and that applies
    /// to the solver underneath the model just as much as to the model.
    #[test]
    fn a_satisfiable_formula_is_solved_and_the_model_satisfies_every_clause() {
        let mut cnf = Cnf::new();
        let group = cnf.group("test");
        let a = cnf.var();
        let b = cnf.var();
        let c = cnf.var();
        cnf.add([a, b], group);
        cnf.add([-a, c], group);
        cnf.add([-b, -c], group);
        let outcome = cnf.solve(100_000);
        let model = outcome.model().expect("satisfiable").to_vec();
        assert!(value(&model, a) || value(&model, b));
        assert!(!value(&model, a) || value(&model, c));
        assert!(!value(&model, b) || !value(&model, c));
    }

    #[test]
    fn a_contradiction_is_unsat_rather_than_unknown() {
        let mut cnf = Cnf::new();
        let group = cnf.group("test");
        let a = cnf.var();
        cnf.add([a], group);
        cnf.add([-a], group);
        assert_eq!(cnf.solve(100_000), Outcome::Unsat);
    }

    /// Six pigeons, five holes. The classic complete-solver test: every
    /// assignment has to be refuted, so a search that stops early gets it wrong,
    /// and a propagation bug that loses a clause reports SAT.
    #[test]
    fn the_pigeonhole_principle_comes_out_unsat() {
        let holes = 5;
        let pigeons = holes + 1;
        let (cnf, _) = pigeonhole(pigeons, holes);
        assert_eq!(cnf.solve(1_000_000), Outcome::Unsat);
    }

    /// Five pigeons, five holes: the same generator one pigeon smaller must be
    /// satisfiable, or the test above proves nothing about the encoding.
    #[test]
    fn the_same_generator_one_pigeon_smaller_is_satisfiable() {
        let holes = 5;
        let pigeons = 5;
        let (cnf, place) = pigeonhole(pigeons, holes);
        let outcome = cnf.solve(1_000_000);
        let model = outcome.model().expect("five into five fits").to_vec();
        for (pigeon, holes_for_it) in place.iter().enumerate() {
            assert_eq!(
                holes_for_it.iter().filter(|&&slot| value(&model, slot)).count(),
                1,
                "pigeon {pigeon} takes exactly one hole"
            );
        }
    }

    /// The sequential at-most-one encoding has to be exactly at-most-one: a
    /// version that is too weak lets two gates stand in one place, and one that
    /// is too strong makes a satisfiable window look infeasible.
    #[test]
    fn at_most_one_forbids_two_and_permits_one_and_permits_none() {
        for chosen in 0..4usize {
            let mut cnf = Cnf::new();
            let group = cnf.group("amo");
            let literals: Vec<i32> = (0..4).map(|_| cnf.var()).collect();
            cnf.at_most_one(&literals, group);
            cnf.add([literals[chosen]], group);
            let outcome = cnf.solve(100_000);
            let model = outcome.model().expect("one is allowed").to_vec();
            assert_eq!(literals.iter().filter(|&&slot| value(&model, slot)).count(), 1);
        }
        let mut cnf = Cnf::new();
        let group = cnf.group("amo");
        let literals: Vec<i32> = (0..4).map(|_| cnf.var()).collect();
        cnf.at_most_one(&literals, group);
        cnf.add([literals[1]], group);
        cnf.add([literals[3]], group);
        assert_eq!(cnf.solve(100_000), Outcome::Unsat);

        let mut cnf = Cnf::new();
        let group = cnf.group("amo");
        let literals: Vec<i32> = (0..4).map(|_| cnf.var()).collect();
        cnf.at_most_one(&literals, group);
        for &literal in &literals {
            cnf.add([-literal], group);
        }
        assert!(cnf.solve(100_000).model().is_some(), "none is allowed");
    }

    /// The core has to name the groups that actually conflict and drop the ones
    /// that do not.
    #[test]
    fn the_core_names_the_groups_that_conflict_and_omits_a_bystander() {
        let mut cnf = Cnf::new();
        let says_yes = cnf.group("says-yes");
        let says_no = cnf.group("says-no");
        let bystander = cnf.group("bystander");
        let a = cnf.var();
        let b = cnf.var();
        cnf.add([a], says_yes);
        cnf.add([-a], says_no);
        cnf.add([b], bystander);
        let core = cnf.core(100_000).expect("unsatisfiable");
        assert_eq!(core, vec![says_yes, says_no]);
    }

    /// A satisfiable formula has no core, and saying so is not the same as
    /// returning an empty one.
    #[test]
    fn a_satisfiable_formula_has_no_core() {
        let mut cnf = Cnf::new();
        let group = cnf.group("fine");
        let a = cnf.var();
        cnf.add([a], group);
        assert_eq!(cnf.core(100_000), None);
    }

    /// A budget of zero must report `Unknown`, never `Unsat`. This is the
    /// distinction the whole exercise turns on: "I ran out of time" and "no such
    /// thing exists" are different answers and only one of them is a result.
    #[test]
    fn an_exhausted_budget_is_unknown_and_never_unsat() {
        let holes = 8;
        let pigeons = holes + 1;
        let (cnf, _) = pigeonhole(pigeons, holes);
        assert_eq!(cnf.solve(1), Outcome::Unknown);
        assert_eq!(cnf.solve(10_000_000), Outcome::Unsat);
    }

    /// Random 3-SAT against a brute-force oracle, every instance, both answers.
    /// This is the test that catches a watched-literal, backjump or
    /// database-reduction defect: those do not fail on hand-written formulas,
    /// they fail on the hundredth instance with a wrong SAT or a wrong UNSAT.
    ///
    /// **Measured to reach the reduction, and honest about what that does and
    /// does not prove.** With `max_learnt` starting at `clauses / 3` (64 here),
    /// `reduce` fires eight times across this module's tests -- counted, by an
    /// `eprintln` since removed -- so the database rebuild is executed rather
    /// than theoretically reachable. It is **not** established that this test
    /// would catch a defect in that rebuild: the one candidate defect
    /// (re-watching blindly) was injected and left all 4,000 instances of a
    /// scaled-up run agreeing with exhaustive search, for the reason recorded at
    /// `reduce` itself. What does guard the rebuild is the model check in
    /// [`Cnf::solve_groups`], which reads every SAT answer back against every
    /// clause.
    #[test]
    fn random_three_sat_agrees_with_exhaustive_search_on_every_instance() {
        let variables = 12usize;
        let mut state = 0x2026_0816_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut sat = 0;
        let mut unsat = 0;
        for _ in 0..200 {
            let count = 40 + (next() % 25) as usize;
            let mut cnf = Cnf::new();
            let group = cnf.group("random");
            let literals: Vec<i32> = (0..variables).map(|_| cnf.var()).collect();
            let mut raw: Vec<[i32; 3]> = Vec::with_capacity(count);
            for _ in 0..count {
                let mut clause = [0i32; 3];
                for slot in &mut clause {
                    let variable = literals[(next() % variables as u64) as usize];
                    *slot = if next() % 2 == 0 { variable } else { -variable };
                }
                cnf.add(clause, group);
                raw.push(clause);
            }
            let expected = (0..(1u32 << variables)).any(|mask| {
                raw.iter().all(|clause| {
                    clause.iter().any(|&literal| {
                        let bit = (mask >> (literal.unsigned_abs() - 1)) & 1 == 1;
                        bit == (literal > 0)
                    })
                })
            });
            match cnf.solve(10_000_000) {
                Outcome::Sat(model) => {
                    assert!(expected, "solver said SAT where exhaustive search said UNSAT");
                    for clause in &raw {
                        assert!(
                            clause.iter().any(|&literal| {
                                value(&model, literal.abs()) == (literal > 0)
                            }),
                            "the model does not satisfy {clause:?}"
                        );
                    }
                    sat += 1;
                }
                Outcome::Unsat => {
                    assert!(!expected, "solver said UNSAT where exhaustive search said SAT");
                    unsat += 1;
                }
                Outcome::Unknown => panic!("ten million conflicts is not a budget problem here"),
            }
        }
        assert!(sat > 20, "the generator produced too few satisfiable instances: {sat}");
        assert!(unsat > 20, "the generator produced too few unsatisfiable instances: {unsat}");
    }
}
