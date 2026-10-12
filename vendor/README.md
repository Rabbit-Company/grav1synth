# Patched av1-grain dependency

`av1-grain/` is a local copy of the `av1-grain` 0.4.2 crate from crates.io
(original archive SHA-256: `620afb8bfa7c1eaef9f5b6284ff7c1e239b85ac9c56b9f2df1f101ea9827d379`).
Its upstream source is [rust-av/av1-grain](https://github.com/rust-av/av1-grain),
and its BSD-2-Clause license is included in `av1-grain/LICENSE`.

Grav1synth uses this local copy because `diff` spends most of its time inside
the dependency's noise-model solver. The changes in `src/diff.rs` and
`src/diff/solver.rs` add a per-generator thread count and process independent
block rows concurrently, then combine their results in row order. Frame order
and the noise model's state remain serial. The solver also calculates only the
unique half of each symmetric noise equation matrix. Other crate behavior is
unchanged.
