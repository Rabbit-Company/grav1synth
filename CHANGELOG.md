## Version 0.3.7

- Analyze several frame pairs at once in `diff` and add them to the noise model in order.
  All three planes of a frame share the worker pool, and the 8-bit conversion runs on the
  decoder threads. The table is unchanged for a fixed `--threads` setting.

## Version 0.3.6

- Decode the source and denoised video with multiple threads in `diff`. Each decoder uses
  up to half of `--threads`. Decoded frames and the resulting table are unchanged.
- Fix `diff` dropping packets when a decoder still held finished frames. Sources with
  reordered frames, such as HEVC with B-frames, lost frames and produced a wrong table.

## Version 0.3.5

- Add `diff --frames N` to supply the source frame count for the progress bar.
  This skips the packet count that otherwise reads the whole source before the diff starts.

## Version 0.3.4

- Reuse grain analysis worker threads and scratch buffers across frames, keeping
  the same row partitions and result order for a fixed `--threads` setting.
- Score independent flat blocks in parallel using the same worker pool.
- Rename the `diff` analysis control to `--noise-sampling all|half|quarter`.
  The former `--accuracy full|balanced|fast` spelling remains accepted for scripts.
  All three modes use the same analysis as their corresponding v0.3.3 modes.

## Version 0.3.3

- Add `diff --accuracy full|balanced|fast`. Full retains the v0.3.2 analysis.
  balanced and fast sample half and one quarter of the noise equation observations,
  respectively, while still decoding every frame and measuring grain strength in
  every flat block. Sampling rotates across rows, blocks, and frames to reduce bias.

## Version 0.3.2

- Reduce work in `diff`'s noise equation loop by reusing the normalization factor
  and filling the symmetric half of each equation matrix after accumulation.

## Version 0.3.1

- Decode the source and denoised video concurrently with grain analysis. Frame pairs still
  reach the noise model in their original order, and decode buffering stays bounded.
- Calculate each symmetric noise equation once, reducing CPU work in the `diff` hot path.

## Version 0.3.0

- Merge `apply` and `generate` into a single `apply` command. `--grain <FILE>` applies
  table-based grain; `--iso <NUM>` (with optional `--chroma`) applies photon-noise-based grain.
- `apply` now checks for existing grain headers before writing. If grain is already present it
  prints a notice and skips, unless `--replace` is also provided. This makes it safe to use in
  automated workflows where existing grain should be preserved.
- Parallelize the noise analysis in `diff` across CPU threads while processing frames in order.
  The new `--threads <N>` option limits workers. By default it uses the available CPU threads.
- Build the Linux release against RabbitEncoder's FFmpeg 9.0.2 libraries.

## Version 0.2.0

- Upgrade all of the internals to the latest rust-av crates
- Remove ISO range limit
- Support ffmpeg 8.0
- Extend packet to include adjustment obu size
- Fix writing `apply_grain` to false
- Add parsing for `global_param`

## Version 0.1.0-beta.6

- Add a progress bar for the `diff` function.
- Fix bug where some codecs may not return the final frame.
- Add ability to crop and resize `diff` sources.
- Fix issue where `diff` may produce bad grain tables due to decoder frame desyncs.

## Version 0.1.0-beta.5

- Fix issue where `diff` may fail in certain circumstances.
- Considerably speed up the `diff` command.

## Version 0.1.0-beta.4

- Fix issue where `apply` and `remove` commands did not modify the file correctly.

## Version 0.1.0-beta.3

- Fix compatibility with a number of videos.
  - There are still a few known files with issues, but a significantly larger number should work now.

## Version 0.1.0-beta.2

- Implement the "diff" command. See README for usage.

## Version 0.1.0-beta.1

- Implement the "apply" and "generate" commands. See README for usage.

## Version 0.1.0-alpha.4

- Implement the "remove" command, which removes all grain synthesis from a video

## Version 0.1.0-alpha.3

- Fix parsing bugs affecting some videos

## Version 0.1.0-alpha.2

- Fix the timestamp output to be in 1/10,000,000 of a second units

## Version 0.1.0-alpha.1

- Initial release
- Enables the `inspect` command
