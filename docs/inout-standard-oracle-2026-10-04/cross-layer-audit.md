# Top-level inout: read-only implementation audit

Audited current shared sources on 2026-10-04. No production or tests edited. This is a design proposal, not implementation/support evidence. Prior internal single-driver inout controls remain valid and must not be replaced.

## Current complete path and first missing contracts

* AST: `src/frontend/ast.rs` PortDirection already has Inout. PortDecl preserves source id, span, range, net_type and direction. Parser accepts declarations but sized literals in parser.rs reject x/z/?; ConstKind::Sized and expression literals contain Boolean bits only.
* Elaboration: elaborate.rs:211 rejects every top-level inout before design creation. Internal Inout branch around 840 uses width-checked lvalue connections and SignalAlias, preserving concat/slice order. Design/SignalInfo/ElabAssign and canonical bit aliases provide reusable source identity, bit range and assignment-origin records; they currently carry no absent drive.
* RTL: rtl.rs RtlKind::Const(Vec<bool>), Mux, Concat, Slice etc have value only. Z must be accepted as an absent-drive expression exclusively in resolved continuous-assignment context. General arithmetic with Z/X remains unsupported with explicit diagnostics. A global conversion of Z to false is incorrect.
* Logic: logic.rs:520 currently stores one assignment per canonical alias bit and reports duplicate or input-plus-assignment writers. AliasClasses is the right place to join all hierarchical driver sets. Existing LogicGraph input/constant/not/and/or/xor/mux and LogicOrigin/SyntheticOrigin can synthesize the resolver without a new general gate vocabulary. BitState::InProgress already rejects combinational self-dependency; preserve that check for read-dependent bus drive.
* Netlist and metadata: compile/mod.rs Netlist is inputs/outputs/gates only. frontend/mod.rs PortBinding already preserves Inout direction/elab/bit but has just one signal. CompileArtifact.ports can preserve the original port occurrence, yet needs a separate resolved-bit binding tying read/value/presence/status to it. SelectedFrontend currently drops everything except output bindings. Transport a dedicated resolved-boundary table through selection, physical compilation, export and consumers; never expose its synthesized auxiliary signals as invented HDL ports.
* Placement/materialization: planner.rs:3034 PortRole and PortPin describe one directional neighbor, explicitly excluding other signal-carrying neighbors. Input and output diode handovers use opposing cells. Reinterpreting the same ordinary PortPin twice violates its contract. Introduce a dedicated composite resolved-bus terminal with one caller-owned shared channel, independent outward-injection and read-tap faces, plus explicit protocol sidebands. Keep ordinary pin guards unchanged.
* CLI/dump/viewer/game: build_circuit.rs:938 emits input/output maps and labels only. Viewer BakedPinout has those two maps and validates against ordinary Netlist identity. Add versioned `inouts` entries with original bit identity, shared-channel/tap/injection coordinates, auxiliary protocol names/roles and status coordinates. Validate every coordinate against shipped materialized hardware before writing/loading. Reuse ordinary pins only for sidebands, never call their aggregate an inout. mc_dump must retain actual shared-channel and sideband blocks and probe targets; existing conformance runner can operate their levers/diodes and preserve actual blockstate histories. Public observer results must report validity, never merely sample a lamp on an invalid bus.
* Optional Yosys boundary now rejects inout instead of silently losing it. Preserve this until JSON tri-state cells/drivers and original public-boundary metadata have an equivalent qualified transport. Native support alone does not make forced Yosys support true.

## Smallest coherent binary resolved-bus contract

Support continuous tri-state drive assignments on wire/inout nets, Boolean conditions/data, internal canonical aliases and acyclic read-dependent logic. Each driver bit is `(value,present)`. An ordinary non-Z wire assignment is always present. `enable ? data : 1'bz` becomes `(data,enable)`; nested drive muxes select presence and value. A top-level bit contributes an explicit external pair `(external_value,external_present)` owned by the caller. Presence is per bit; a vector-wide convenience can broadcast explicitly. Do not infer presence from dust power: active zero and absent both have zero power.

For each resolved bit compute:

```
high = OR_i(present_i AND value_i)
low  = OR_i(present_i AND NOT value_i)
present = high OR low
conflict = high AND low
valid = present AND NOT conflict
value = high                // meaningful only when valid
floating = NOT present
```

This uses linear-size reductions instead of pairwise comparisons, preserves legally overlapping equal values, and diagnoses both X (conflict) and Z (floating). Origin records must retain every contributing assignment and the source bus bit. No implicit pull is permitted. Explicit source pull declarations would require separately supported syntax/driver strength semantics; do not invent a pull silently.

A two-valued compiler must not call invalid `value` a source result. Expose per-bit present/conflict/valid diagnostics through a checked observation API, sidecar and viewer/game probes; invalid read/output/clock observation returns an explicit floating/contention error. In a game, status lamp high/low is auxiliary protocol hardware, not HDL X/Z encoded as dust. Stored scalar bus power under invalid status is not semantically observable. Arbitrary combinational downstream signals may physically exist then, but consumers cannot claim HDL equivalence for them. Clocked capture must be permitted only under a proved or checked valid sampling protocol; this is a subsequent qualification, not an automatic extension from settled combinational success.

## Real shared physical boundary, not split-port renaming

Reserve one actual connected redstone shared channel per source bit, with independent diode-isolated injection branches for internal drivers and caller contribution, and a dedicated read tap delivering that same channel into the generated readers. All internal contributions inject `present AND value`; absent and active-zero contributions inject no high power. The caller still owns and drives the shared channel. Public outward drive and public readback occur at that common channel, not separate replacement HDL pins.

The protocol must also convey external_present **and external_value** independently of the resolved channel (ordinary directional sideband pins are sufficient). External presence alone cannot distinguish a competing external active-zero from an internal active-one: the shared dust reads high in both the legal/internal-only and conflicting cases. Sampling resolved dust as external_value would hide that conflict. This is an information limitation, not a routing optimization. An exclusive-ownership protocol could avoid the extra value sideband, but must reject overlapping ownership and would no longer accept all legal equal-value overlap cases; do not silently choose that narrower semantics.

Use external_value sideband only for validity/conflict computation and independent reference prediction, not as the physical substitute for readback. Physical reader routes must originate at the shared tap. The abstract logic resolver predicts the tap's legal value, and materialization must carry the resolved boundary identity instead of generating a separate disconnected resolver output as the public bus. Accordingly the ordinary Netlist alone is insufficient for physical routing; a small boundary descriptor linking resolver read node, driver contributions, tap and protocol sidebands is unavoidable. Reusing ordinary output then ordinary input at disconnected cells is an adapter test, not genuine inout acceptance.

A waveform is valid only after source/sideband changes and channel settle according to the fixed protocol. During changes, presence/value skew may transiently raise status; preserve full histories and qualify the observer/capture rule. Do not suppress transients by changing simulator physics or silently lengthening a cap. External fixture must prove that the actual shared-channel drive agrees with advertised external_value/present. Channel divergence raises a physical protocol violation rather than overwriting source semantics.

## Positive source and independent oracle

Concrete legal multi-driver, top-level source (synthesizable tri-state wiring; currently correctly rejected by REDA):

```systemverilog
module writer(input wire en, data, inout wire bus);
  assign bus = en ? data : 1'bz;
endmodule
module top(input wire e0,d0,e1,d1,inout wire bus,output wire y);
  writer a(.en(e0),.data(d0),.bus(bus));
  writer b(.en(e1),.data(d1),.bus(bus));
  assign y = bus;
endmodule
```

A separate testbench adds the third external driver `assign bus = ext_en ? ext_data : 1'bz;`. Freeze it before compiler development. Use an independent standard four-state event simulator (e.g. Icarus Verilog, with exact executable/version/argv recorded) and use `===` to classify bus/y as 0,1,x,z. It is an oracle for HDL resolution, not a synthesis-tool wired-OR assumption. All 64 states of the three Boolean `(enable,data)` pairs cover inactive, active zero, active one, equal overlaps, opposite overlaps and floating. Enumerate all 4096 ordered state transitions in an unchanged world, retain raw per-phase standard-HDL classifications, simulator states and game probes. Numeric 4096 here means 64x64 transition pairs, not circuit blocks. Settle protocol and physical observation timing must be frozen, and failures retained.

Initially qualify settled acyclic combinational readback only. For each valid standard-HDL phase assert public bus tap and y equal the oracle, outward behavior survives external removal, disabled internal driver does not power the channel, and status valid is true. For X/Z oracle phases assert conflict/floating respectively and checked observations fail, rather than assert a binary y. Probe the internal contribution branches, shared tap, sideband agreement and status, not just public lamps. Repeat renamed ports, reordered vectors, sliced/concatenated hierarchy, two readers and explicit source-ordered metadata. Existing internal-inout 512-phase test remains an independent regression.

A follow-up read-dependent drive `assign bus = en ? ~bus : 1'bz` must remain an explicit combinational-loop diagnostic; declaring it supported because a game reached some stable state is incorrect. Legal DFF-controlled enables need independent power-on/clock wave qualification. Do not add them to the acceptance claim until demonstrated.

## Recommended implementation sequence

1. Freeze supported drive grammar, validity/observation protocol, dedicated source boundary metadata and above oracle.
2. Add contextual Z drive representation and canonical driver-set resolution using existing Boolean helpers and provenance. Qualify all 64 four-state truth classifications without claiming physical completion.
3. Carry original Inout occurrence plus resolved-boundary table through native/Auto selection and exporters. Keep Yosys failure explicit.
4. Implement dedicated shared physical terminal and use its actual tap for readers; sidebands remain accurately labeled protocol pins. Prove materialized ownership/isolation and export roundtrip.
5. Run exhaustive transitions in existing simulator and actual Minecraft with frozen caller adapter, raw status/channel histories and checked invalid observations. This is the first stage eligible to claim genuine top-level inout support for the specified binary valid domain.

Static rejection, a read-only alias, always-driven-only special cases, split value/enable/read adapters, or positive resolver truth tables alone do not close the gap.
