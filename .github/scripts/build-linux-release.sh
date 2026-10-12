#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
release_tag="${RELEASE_TAG:-local}"
ffmpeg_sha256=e7b092f856767e6de7339b6b68e0acafcecd9f555b099bd3a0dfb008c8ef4a22
ffmpeg_prefix=/opt/ffmpeg-x86-64-v2
ffmpeg_url=https://cdn.rabbit-company.com/rabbit-encoder/binaries/x86_64_v2/ffmpeg.tar.zst

if [[ "$(uname -m)" != x86_64 ]]; then
  echo "This release script builds the Linux x86_64 artifact." >&2
  exit 1
fi
if [[ ! "$release_tag" =~ ^[A-Za-z0-9._+-]+$ ]]; then
  echo "Invalid release tag: $release_tag" >&2
  exit 1
fi
if [[ -e "$ffmpeg_prefix" ]]; then
  echo "Refusing to replace $ffmpeg_prefix" >&2
  exit 1
fi

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT
ffmpeg_archive="${FFMPEG_ARCHIVE:-$build_dir/ffmpeg.tar.zst}"
if [[ ! -f "$ffmpeg_archive" ]]; then
  curl -fsSL --retry 3 --connect-timeout 20 "$ffmpeg_url" -o "$ffmpeg_archive"
fi
printf '%s  %s\n' "$ffmpeg_sha256" "$ffmpeg_archive" | sha256sum -c -
tar --zstd -xf "$ffmpeg_archive" -C /opt

export PKG_CONFIG_LIBDIR="$ffmpeg_prefix/lib/pkgconfig"
export LD_LIBRARY_PATH="$ffmpeg_prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
test "$(pkg-config --modversion libavcodec)" = 63.1.102
test "$(pkg-config --modversion libavformat)" = 63.1.102
test "$(pkg-config --modversion libavutil)" = 61.1.102

pushd "$repo_root" >/dev/null
cargo test --locked
cargo build --release --locked
popd >/dev/null

mkdir -p "$repo_root/dist"
asset="grav1synth-$release_tag-linux-x86_64-ffmpeg9.0.2"
cp "$repo_root/target/release/grav1synth" "$repo_root/dist/$asset"
chmod +x "$repo_root/dist/$asset"

ldd "$repo_root/dist/$asset" > "$build_dir/ldd.txt"
cat "$build_dir/ldd.txt"
if grep -q 'not found' "$build_dir/ldd.txt"; then
  echo 'The release binary has missing shared libraries.' >&2
  exit 1
fi
if ! grep -q "libavcodec.so.*=> $ffmpeg_prefix/lib/" "$build_dir/ldd.txt"; then
  echo 'The release binary is not using RabbitEncoder FFmpeg.' >&2
  exit 1
fi
"$repo_root/dist/$asset" --help > /dev/null

sample_dir="$build_dir/smoke"
mkdir -p "$sample_dir"
"$ffmpeg_prefix/bin/ffmpeg" -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=128x128:rate=8 -frames:v 16 \
  -c:v ffv1 -y "$sample_dir/source.mkv"
"$ffmpeg_prefix/bin/ffmpeg" -hide_banner -loglevel error \
  -i "$sample_dir/source.mkv" -vf boxblur=2:1 \
  -c:v ffv1 -y "$sample_dir/denoised.mkv"
"$repo_root/dist/$asset" diff \
  "$sample_dir/source.mkv" "$sample_dir/denoised.mkv" \
  -o "$sample_dir/grain.tbl" -y --threads 1
"$repo_root/dist/$asset" diff \
  "$sample_dir/source.mkv" "$sample_dir/denoised.mkv" \
  -o "$sample_dir/grain-parallel.tbl" -y --threads 4
test -s "$sample_dir/grain.tbl"
head -n 1 "$sample_dir/grain.tbl" | grep -qx 'filmgrn1'
cmp "$sample_dir/grain.tbl" "$sample_dir/grain-parallel.tbl"

pushd "$repo_root/dist" >/dev/null
sha256sum "$asset" > "$asset.sha256"
popd >/dev/null
echo "Created dist/$asset and its SHA-256 checksum."
