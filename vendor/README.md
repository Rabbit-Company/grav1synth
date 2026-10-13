# Patched av1-grain dependency

`av1-grain/` is a local copy of the `av1-grain` 0.4.2 crate from crates.io
(original archive SHA-256: `620afb8bfa7c1eaef9f5b6284ff7c1e239b85ac9c56b9f2df1f101ea9827d379`).
Its upstream source is [rust-av/av1-grain](https://github.com/rust-av/av1-grain),
and its BSD-2-Clause license is included in `av1-grain/LICENSE`.

Grav1synth uses this local copy because `diff` spends most of its time inside
the dependency's noise-model solver. The changes in `src/diff.rs` and
`src/diff/solver.rs` split the analysis of a frame into a part that reads no
model state and a part that updates the model. Worker threads run the first
part for several frames at once, one thread per plane, and the generator adds
the results to the model in frame order. Each plane is summed in the original
block order, so the table is the same as the unpatched crate's for every thread
count. The solver also calculates only the unique half of each symmetric noise
equation matrix, and replaces the division of each contribution with a multiply
and one FMA correction step that gives the same quotient (a unit test checks
every possible contribution). The `diff --accuracy` modes sample noise
equation observations in this solver. The default sample step of one preserves
the full v0.3.2 analysis. Other crate behavior is unchanged.
