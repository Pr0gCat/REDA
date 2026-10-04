# Reuse address-only Boolean oracles in default selection

The default selector previously reevaluated both combinational Boolean graphs in every retained-history phase. Each graph and its input bindings are immutable during qualification, so an address has one fixed Boolean result. Lazy per-address storage now reuses that result; physical simulation, observer history, gate final-state checks, public events, quality bounds, phase order and settling caps remain unchanged. No world or physical history is cached.

For six inputs, each graph now evaluates at most 64 addresses instead of 8192 phases. This is an operation-count reduction, not a measured overall compilation speedup. Failed graph evaluation still refuses qualification at the first affected phase. Storage is scoped to one qualification call; independent candidates retain independent oracles.

All six existing selector tests pass in release with no failures or ignores. They cover complete bounded-wave success, real arrival/settlement counterexamples, repeated public channels, sealed-view tampering, exact backup manifests and original driver/cycle errors. The preserved pre-change executable also passes the same six tests. Runtime tests use the mixed current worktree, whose Rust sources are hashed here; no clean whole-project result is claimed. Final indentation changes preserve whitespace-separated tokens, and the final native library check passes.

The existing six-input actual-default process completed its first 8192-phase run independently of this patch; its original binary is unchanged and its full-event audit remains separately tracked. This patch does not adopt six-input default admission or close any of the eight broad requirements.
