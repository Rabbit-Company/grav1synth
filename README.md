# Grav1synth

[![LICENSE](https://img.shields.io/crates/l/grav1synth?style=for-the-badge)](https://github.com/Rabbit-Company/grav1synth/blob/main/LICENSE)

Grain Synth analyzer and editor for AV1 files

## Build

- Prerequisites:
  - ffmpeg headers
  - Rust compiler
- Pull the repo
- Run `cargo build --release`
- Copy the binary from `target/release/grav1synth` to a machine with compatible FFmpeg shared libraries

## Prebuilt Linux release

Published GitHub releases include the executable `grav1synth-<tag>-linux-x86_64-ffmpeg9.0.2` and its SHA-256 checksum. It is built on Debian testing against the exact RabbitEncoder FFmpeg 9.0.2 x86-64-v2 archive. That archive supplies the headers and shared libraries for the build; the release asset contains the grav1synth executable only.

At runtime, provide the matching RabbitEncoder FFmpeg shared libraries and their system dependencies. RabbitEncoder already downloads the v2, v3, and v4 FFmpeg archives and selects one through `LD_LIBRARY_PATH`. In another Debian testing environment, extract the [v2 FFmpeg archive](https://cdn.rabbit-company.com/rabbit-encoder/binaries/x86_64_v2/ffmpeg.tar.zst) under `/opt`, install its runtime dependencies, and set `LD_LIBRARY_PATH=/opt/ffmpeg-x86-64-v2/lib`. A v2-built executable can also use RabbitEncoder's v3 or v4 libraries when its selected variant is first in `LD_LIBRARY_PATH`. The x86-64 release is dynamically linked and is not a portable standalone binary.

Every push to `main` builds and tests the executable and uploads it as a workflow artifact. To produce a release, publish a GitHub release for a tag on the desired commit. The same workflow then attaches the executable and checksum to that release. It can also be run manually from the Actions tab. The workflow verifies the pinned FFmpeg CDN archive SHA-256 in `.github/scripts/build-linux-release.sh`.

## Usage

### `grav1synth inspect my_encode.mkv -o grain_file.txt`

Reads `my_encode.mkv` and outputs a film grain table file at `grain_file.txt`

### `grav1synth apply my_encode.mkv -o grainy_encode.mkv -g grain_file.txt`

Reads `my_encode.mkv`, applies film grain from `grain_file.txt`, and outputs the video to `grainy_encode.mkv`.

### `grav1synth apply my_encode.mkv -o grainy_encode.mkv --iso 400 --chroma`

Reads `my_encode.mkv`, generates photon-noise-based film grain at the strength given by `--iso` (up to `4294967295`), and outputs the video to `grainy_encode.mkv`. By default grain is applied to the luma plane only; `--chroma` enables grain on chroma planes as well.

In both forms, if the input already has film grain headers the command will print a notice and skip processing — add `--replace` to overwrite existing grain instead. This makes it safe to use in automated workflows where you want to protect videos that already have grain while still applying grain to those that do not.

### `grav1synth remove my_encode.mkv -o clean_encode.mkv`

Reads `my_encode.mkv`, removes all synthesized film grain, and outputs the video at `clean_encode.mkv`

### `grav1synth diff my_source.mkv denoised_source.mkv -o grain_file.txt`

Compares `my_source.mkv` and `denoised_source.mkv` and generates a film grain table at `grain_file.txt` based on the difference between them. This will provide the most accurate estimation of source film grain.

The `diff` command uses all available CPU threads for grain analysis by default. Use `--threads 8` (or another positive number) to limit its CPU use when other work, such as encoding, runs at the same time. Each of the two video decoders also uses up to half that many threads. Frames are still analyzed in order.

Before it starts, `diff` reads the whole source once to count frames for the progress bar. Pass `--frames N` when you already know the count to skip that step. The value only affects the progress bar and ETA, never the table.

Use `--noise-sampling all` (the default) to analyze every noise observation, with the same output as v0.3.2. `--noise-sampling half` analyzes about half the horizontal noise observations, while `--noise-sampling quarter` analyzes about one quarter. Both faster modes still decode every frame, check the full image for flat blocks, and measure grain strength in every flat block. They can change the resulting table, particularly on short or unusual scenes (compare encodes before using them for a whole video). The v0.3.3 `--accuracy full|balanced|fast` spelling remains accepted for existing scripts.

<!-- ### `grav1synth estimate my_source.mkv -o grain_file.txt`

Analyzes `my_source.mkv` and estimates the amount of noise in the source, then generates an appropriate film grain table at `grain_file.txt`. This is less accurate than the diff method, but is significantly faster. -->

## Debug Tracing

To enable AV1 bitstream trace header output (similar to FFmpeg's `trace_headers` bitstream filter), set the `RUST_LOG` environment variable:

```sh
RUST_LOG=trace_headers=debug grav1synth inspect my_encode.mkv -o grain_file.txt
```

This outputs each parsed OBU field with its bit position, binary representation, and decimal value.

## Known Issues

- There have been reports that certain videos will fail to apply film grain properly. This is likely related to aomenc's `--keyframe-filtering=2`.
