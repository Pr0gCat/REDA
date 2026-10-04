# Top-level inout: public decision still required

The current CompileArtifact/Netlist interface exposes Boolean input and output bindings. An inout is simultaneously a net that can be read and driven, with a potentially absent drive and an external driver. Mapping it to an ordinary input/output loses that distinction. The internal single-driver alias implementation solves hierarchy connectivity; it does not define an external boundary.

Two concrete alternatives need a public-contract decision before implementation:

1. An explicitly selected split-interface compilation mode exposes read value, drive value and drive enable for each source inout bit. Consumers must supply the external read value and respect drive enable. The artifact records the original inout identity and those three roles, rather than silently renaming an ordinary port. This can support a synthesizable restricted source subset without pretending the emitted three Boolean pins are one real-game bidirectional wire. Contention/Z still require an explicit environment policy and separate physical boundary design/conformance. Source semantic tests must cover enabled/disabled readback, external drive changes and forbidden overlapping drive.
2. A genuine four-state/net-resolution boundary represents external and internal drive states and drives a qualified physical bidirectional interface. This expands the netlist, consumers and state-observation model and needs actual Minecraft physical evidence. It cannot be achieved by changing the existing Boolean simulator's rules or choosing wired OR as a replacement for Verilog contention.

The first alternative is a smaller useful interface, but it is a material public API/consumer choice and must be named explicitly. Neither alternative is implemented or counted as completed support. Existing top-level inout rejection remains until an accepted contract has source, physical and consumer evidence.
