# Preserve every public port at the optional Yosys boundary

The original source `module m(input a,inout wire bus,output y); assign bus=a; assign y=bus; endmodule` is independently synthesized with all three public ports in its Yosys JSON. Both forced Yosys and Auto then returned success with only input a and output y; bus disappeared because the bridge skipped every direction other than input/output.

The Boolean bridge now validates every public direction before constructing bindings. Its current input/output transport cannot resolve bidirectional external drive, so inout and unknown directions produce an explicit error naming the port and direction. Existing supported input/output handling is preserved. This fail-closed boundary prevents a partial artifact from being reported as successful compilation; it is not inout support.

The isolated candidate passes all9 bridge unit tests and repeats both actual Yosys calls with the original source, each giving the expected explicit bus/inout diagnostic. Source/binary drift is zero. Root independently integrates the previously clean bridge file and reruns all9 unit controls successfully in the shared worktree. Original source/JSON/partial artifact, both before/after transcripts, source/producer manifests and the negative inout/unknown-direction test remain retained. Logs and patch are stored losslessly as gzip; decompress to their original names for original manifest checks.

The attached read-only audit identifies the remaining standard HDL drive-presence/value resolution, X/Z boundary, public metadata and real shared-wire physical adapter requirements. No split-pin interface is counted as bidirectional physical support. Top-level inout, full optional-Yosys absence coverage and broad eight-gap acceptance remain open.
