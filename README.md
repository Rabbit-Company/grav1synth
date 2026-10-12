# Grav1synth

[![docs.rs](https://img.shields.io/docsrs/grav1synth?style=for-the-badge)](https://docs.rs/grav1synth)
[![Crates.io](https://img.shields.io/crates/v/grav1synth?style=for-the-badge)](https://crates.io/crates/grav1synth)
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

To produce a release, merge the release workflow into the default branch, then publish a GitHub release for a tag on the desired commit. The workflow downloads the pinned FFmpeg CDN archive, builds and tests grav1synth against it, uploads the executable and checksum, and attaches them to that release. It can also be run manually from the Actions tab to inspect artifacts before publishing. The CDN archive SHA-256 is pinned in `.github/scripts/build-linux-release.sh`.

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
