//! A pure logical evaluator for a [`Netlist`], including positive-edge
//! flip-flop stepping.
//!
//! This is a *verification* utility, not a compiler pass: nothing in the
//! compile pipeline calls it, and it changes nothing in `src/compile`. What
//! it gives the frontend is an oracle that needs no placement, no routing,
//! no world, and no Yosys -- so an exhaustive truth table or a bounded
//! sequential trace can be compared on native and on `wasm32` alike, in
//! milliseconds rather than in simulated redstone ticks.
//!
//! It is deliberately the *only* logical evaluator here. The bit-level graph
//! does not get a second one: a machine that agreed with the compiler's own
//! internal representation but not with the netlist it emitted would be
//! measuring the wrong thing.
//!
//! Semantics:
//!
//! - every DFF's `Q` starts at `false`, as does its remembered clock level,
//!   so a trace begins from all-zero state;
//! - combinational logic settles against the *current* `Q` values --
//!   `Netlist::combinational_order` already treats a sequential gate as a
//!   source, which is what makes a feedback path through one legal;
//! - [`Evaluator::step`] settles, samples every DFF whose clock made a
//!   0-to-1 transition, commits them simultaneously, and settles again, so
//!   `a <= b; b <= a;` swaps rather than copying.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::compile::topology::GateKind;
use crate::compile::Netlist;

/// Why a netlist cannot be evaluated. Every variant is a structural fault
/// that would also break placement -- it is better to name it here, in
/// milliseconds, than to discover it as a missing torch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A gate reads a signal that is neither a primary input nor any gate's
    /// output.
    UndrivenNet { gate: String, input: String },
    /// Two gates drive the same signal.
    DuplicateOutput(String),
    /// A declared output is not produced by any gate.
    UndrivenOutput(String),
    /// A gate's input count disagrees with its kind.
    MalformedGate { gate: String, message: String },
    /// A loop that never crosses a sequential element.
    CombinationalCycle,
    /// An input of the netlist was not given a value.
    MissingInput(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UndrivenNet { gate, input } => {
                write!(f, "gate `{gate}` reads undriven net `{input}`")
            }
            EvalError::DuplicateOutput(name) => write!(f, "net `{name}` has more than one driver"),
            EvalError::UndrivenOutput(name) => {
                write!(f, "declared output `{name}` is not driven by any gate")
            }
            EvalError::MalformedGate { gate, message } => write!(f, "gate `{gate}`: {message}"),
            EvalError::CombinationalCycle => {
                write!(f, "the netlist has a combinational cycle")
            }
            EvalError::MissingInput(name) => write!(f, "no value was given for input `{name}`"),
        }
    }
}

impl std::error::Error for EvalError {}

/// A netlist plus the state of its flip-flops.
#[derive(Debug, Clone)]
pub struct Evaluator {
    netlist: Netlist,
    order: Vec<usize>,
    /// Gate indices of the sequential elements, in netlist order.
    registers: Vec<usize>,
    /// Current `Q` of each register, parallel to `registers`.
    q: Vec<bool>,
    /// The clock level each register saw at the previous step.
    previous_clock: Vec<bool>,
}

impl Evaluator {
    /// Validate `netlist` and start it from all-zero state.
    pub fn new(netlist: &Netlist) -> Result<Evaluator, EvalError> {
        validate(netlist)?;
        let order = netlist
            .combinational_order()
            .ok_or(EvalError::CombinationalCycle)?;
        let registers: Vec<usize> = netlist
            .gates
            .iter()
            .enumerate()
            .filter(|(_, gate)| gate.kind.is_sequential())
            .map(|(index, _)| index)
            .collect();
        let count = registers.len();
        Ok(Evaluator {
            netlist: netlist.clone(),
            order,
            registers,
            q: vec![false; count],
            previous_clock: vec![false; count],
        })
    }

    /// Reset every register to zero and forget every clock level.
    pub fn reset(&mut self) {
        self.q.iter_mut().for_each(|value| *value = false);
        self.previous_clock
            .iter_mut()
            .for_each(|value| *value = false);
    }

    /// The current value of each register's `Q`, in netlist gate order.
    pub fn state(&self) -> &[bool] {
        &self.q
    }

    /// Settle the combinational logic against the current register state and
    /// return every net's value.
    pub fn settle(
        &self,
        inputs: &BTreeMap<String, bool>,
    ) -> Result<BTreeMap<String, bool>, EvalError> {
        let mut values: BTreeMap<String, bool> = BTreeMap::new();
        for name in &self.netlist.inputs {
            let value = *inputs
                .get(name)
                .ok_or_else(|| EvalError::MissingInput(name.clone()))?;
            values.insert(name.clone(), value);
        }

        let register_of: HashMap<usize, usize> = self
            .registers
            .iter()
            .enumerate()
            .map(|(slot, &gate)| (gate, slot))
            .collect();

        for &index in &self.order {
            let gate = &self.netlist.gates[index];
            let value = if let Some(&slot) = register_of.get(&index) {
                // A register's output is a source for this settle: its D
                // input is sampled only at a clock edge.
                self.q[slot]
            } else {
                let mut operands = Vec::with_capacity(gate.inputs.len());
                for input in &gate.inputs {
                    operands.push(*values.get(input).ok_or_else(|| EvalError::UndrivenNet {
                        gate: gate.name.clone(),
                        input: input.clone(),
                    })?);
                }
                gate.kind.evaluate(&operands)
            };
            values.insert(gate.output.clone(), value);
        }
        Ok(values)
    }

    /// Settle and return only the declared outputs, by signal name.
    pub fn evaluate(
        &self,
        inputs: &BTreeMap<String, bool>,
    ) -> Result<BTreeMap<String, bool>, EvalError> {
        let values = self.settle(inputs)?;
        let mut outputs = BTreeMap::new();
        for name in &self.netlist.outputs {
            let value = *values
                .get(name)
                .ok_or_else(|| EvalError::UndrivenOutput(name.clone()))?;
            outputs.insert(name.clone(), value);
        }
        Ok(outputs)
    }

    /// One clock step: settle against the old state, commit every register
    /// whose clock rose from 0 to 1, then settle again and return the
    /// declared outputs as an observer would see them after the edge.
    pub fn step(
        &mut self,
        inputs: &BTreeMap<String, bool>,
    ) -> Result<BTreeMap<String, bool>, EvalError> {
        let before = self.settle(inputs)?;

        let mut next = self.q.clone();
        let mut clocks = self.previous_clock.clone();
        for (slot, &index) in self.registers.iter().enumerate() {
            let gate = &self.netlist.gates[index];
            // `GateKind::DffPosedge`'s pin order is `[D, C]`.
            let read = |pin: usize| -> Result<bool, EvalError> {
                let name = &gate.inputs[pin];
                before
                    .get(name)
                    .copied()
                    .ok_or_else(|| EvalError::UndrivenNet {
                        gate: gate.name.clone(),
                        input: name.clone(),
                    })
            };
            let data = read(0)?;
            let clock = read(1)?;
            if clock && !self.previous_clock[slot] {
                next[slot] = data;
            }
            clocks[slot] = clock;
        }
        self.q = next;
        self.previous_clock = clocks;

        self.evaluate(inputs)
    }
}

/// Structural checks a netlist must pass before it can be evaluated -- and,
/// as it happens, exactly the ones the plan asks gate mapping to make before
/// returning a `Netlist` at all.
pub fn validate(netlist: &Netlist) -> Result<(), EvalError> {
    let mut driven: HashSet<&str> = HashSet::new();
    for name in &netlist.inputs {
        driven.insert(name.as_str());
    }
    for gate in &netlist.gates {
        if !gate.kind.accepts_arity(gate.inputs.len()) {
            return Err(EvalError::MalformedGate {
                gate: gate.name.clone(),
                message: format!(
                    "{:?} takes {} input(s), not {}",
                    gate.kind,
                    gate.kind.arity(),
                    gate.inputs.len()
                ),
            });
        }
        if gate.kind.is_sequential() && gate.kind != GateKind::DffPosedge {
            return Err(EvalError::MalformedGate {
                gate: gate.name.clone(),
                message: "the positive-edge DFF is the only sequential kind".to_string(),
            });
        }
        if !driven.insert(gate.output.as_str()) {
            return Err(EvalError::DuplicateOutput(gate.output.clone()));
        }
    }
    for gate in &netlist.gates {
        for input in &gate.inputs {
            if !driven.contains(input.as_str()) {
                return Err(EvalError::UndrivenNet {
                    gate: gate.name.clone(),
                    input: input.clone(),
                });
            }
        }
    }
    for name in &netlist.outputs {
        if !netlist.gates.iter().any(|gate| &gate.output == name) {
            return Err(EvalError::UndrivenOutput(name.clone()));
        }
    }
    if netlist.combinational_order().is_none() {
        return Err(EvalError::CombinationalCycle);
    }
    Ok(())
}

/// Build the input map for one row of a truth table: bit `i` of `row` is the
/// value of `inputs[i]`.
pub fn inputs_from_bits(inputs: &[String], row: u32) -> BTreeMap<String, bool> {
    inputs
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), (row >> index) & 1 == 1))
        .collect()
}
