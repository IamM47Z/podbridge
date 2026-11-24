#!/usr/bin/env bash
# Builds the release binary that runs on any distribution with glibc >= MAX_GLIBC.
# Distributions ship different FFmpeg sonames (Ubuntu 26.04 libavutil.so.60,
# Arch libavutil.so.61), so FFmpeg is linked statically, and only the AAC
# decoder that src/audio/eld.rs uses is built. The glibc floor is the build
# host's, so the release runs on the oldest supported runner image.
set -euo pipefail

# The minor version must match ffmpeg-sys-next's, whose bindings target it.
FFMPEG_VERSION=8.1.3
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
MAX_GLIBC=2.39

root="$PWD/target/ffmpeg"
prefix="$root/dist"
if [[ ! -f $prefix/lib/libavcodec.a ]]; then
  rm -rf "$root"
  mkdir -p "$root"
  curl -fsSLo "$root/ffmpeg.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
  echo "$FFMPEG_SHA256  $root/ffmpeg.tar.xz" | sha256sum -c -
  tar -xf "$root/ffmpeg.tar.xz" -C "$root"
  (
    cd "$root/ffmpeg-$FFMPEG_VERSION"
    ./configure --prefix="$prefix" --enable-static --disable-shared --enable-pic \
      --disable-autodetect --disable-programs --disable-doc --disable-network \
      --disable-everything --enable-decoder=aac \
      --disable-avformat --disable-avfilter --disable-avdevice \
      --disable-swscale --disable-swresample
    make -j"$(nproc)" install
  )
fi

# Only static archives sit in $prefix/lib, so the linker cannot pick a shared FFmpeg.
FFMPEG_DIR="$prefix" cargo build --release --locked

bin=target/release/podbridge
needed=$(readelf -d "$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
echo "NEEDED: $(tr '\n' ' ' <<<"$needed")"
if grep -E '^lib(av|sw)' <<<"$needed"; then
  echo "error: $bin links FFmpeg dynamically" >&2
  exit 1
fi

glibc=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n1)
echo "Highest glibc symbol version: $glibc"
if [[ $(printf '%s\n' "$glibc" "$MAX_GLIBC" | sort -V | tail -n1) != "$MAX_GLIBC" ]]; then
  echo "error: $bin needs glibc $glibc, above $MAX_GLIBC" >&2
  exit 1
fi
