#!/usr/bin/env bash
# Builds the decode-only ffmpeg that ships with flowproof (#818): it reads the
# screen recordings `author-from-video` is given and nothing else. No
# encoders but the two flowproof writes pictures with, no network, no HEVC
# (patent pools; recordings in it are told to export as H.264), no GPL parts,
# so the binary is LGPL and a few MB.
#
# Usage: scripts/ffmpeg/build.sh <linux-x64|darwin-arm64|darwin-x64|win32-x64> <out-dir>
#
# Writes <out-dir>/ffmpeg (ffmpeg.exe for Windows) and ffmpeg-licenses.txt,
# which ships beside it. Needs curl, a C compiler, make, nasm, meson, ninja
# and pkg-config; win32-x64 is cross-compiled with mingw-w64 and darwin-x64
# with the macOS clang, so every platform builds on a Linux or Apple Silicon
# runner.
#
# Keep both versions on their latest release: ffmpeg parses files users give
# it, so its security fixes are flowproof's. Bump the version and its hash
# together; the CI cache is keyed on this file, so a bump rebuilds.
set -euo pipefail

FFMPEG_VERSION=9.0.2
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
DAV1D_VERSION=1.5.4
DAV1D_SHA256=686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5

platform="${1:?usage: build.sh <platform> <out-dir>}"
out="${2:?usage: build.sh <platform> <out-dir>}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
prefix="$work/prefix"
jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

# Platform choices. The arrays are expanded with the ${a[@]+...} form
# because macOS ships bash 3.2, where an empty array trips `set -u`.
exe=ffmpeg
meson_cross=()
ffmpeg_platform=()
case "$platform" in
  linux-x64)
    # Static, so one binary runs on any glibc the wheel and npm package do.
    ffmpeg_platform=(--extra-ldflags=-static)
    ;;
  darwin-arm64)
    export MACOSX_DEPLOYMENT_TARGET=11.0
    ;;
  darwin-x64)
    export MACOSX_DEPLOYMENT_TARGET=10.15
    cat >"$work/cross.meson" <<'EOF'
[binaries]
c = ['clang', '-arch', 'x86_64']
ar = 'ar'
strip = 'strip'

[host_machine]
system = 'darwin'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
    meson_cross=(--cross-file "$work/cross.meson")
    ffmpeg_platform=(--enable-cross-compile --target-os=darwin --arch=x86_64
      --cc=clang "--extra-cflags=-arch x86_64" "--extra-ldflags=-arch x86_64")
    ;;
  win32-x64)
    exe=ffmpeg.exe
    cat >"$work/cross.meson" <<'EOF'
[binaries]
c = 'x86_64-w64-mingw32-gcc'
ar = 'x86_64-w64-mingw32-ar'
strip = 'x86_64-w64-mingw32-strip'
windres = 'x86_64-w64-mingw32-windres'

[host_machine]
system = 'windows'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
    meson_cross=(--cross-file "$work/cross.meson")
    ffmpeg_platform=(--enable-cross-compile --target-os=mingw32 --arch=x86_64
      --cross-prefix=x86_64-w64-mingw32- --extra-ldflags=-static)
    ;;
  *)
    echo "unknown platform: $platform" >&2
    exit 2
    ;;
esac

fetch() { # <url> <sha256> <dir>: download, verify, unpack into $work/<dir>
  curl -fsSL --retry 3 -o "$work/src.tar.xz" "$1"
  echo "$2  $work/src.tar.xz" | shasum -a 256 -c - >/dev/null
  mkdir -p "$work/$3"
  tar -xJf "$work/src.tar.xz" -C "$work/$3" --strip-components=1
  rm "$work/src.tar.xz"
}

fetch "https://downloads.videolan.org/pub/videolan/dav1d/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.xz" \
  "$DAV1D_SHA256" dav1d
fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_SHA256" ffmpeg

# dav1d: the AV1 decoder, static, 8-bit only (screen recordings are 8-bit).
meson setup "$work/dav1d-build" "$work/dav1d" ${meson_cross[@]+"${meson_cross[@]}"} \
  --prefix="$prefix" --libdir=lib --default-library=static --buildtype=release \
  -Dbitdepths=8 -Denable_tools=false -Denable_tests=false >/dev/null
ninja -C "$work/dav1d-build" install >/dev/null

# ffmpeg: only what video_frames.rs asks of it. Every option it passes
# (-f rawvideo/image2pipe/null, -c:v mjpeg, -vf fps/format/scale/crop,
# -progress pipe:1) needs a component enabled here.
mkdir -p "$work/ffmpeg-build"
cd "$work/ffmpeg-build"
PKG_CONFIG_PATH="$prefix/lib/pkgconfig" "$work/ffmpeg/configure" \
  ${ffmpeg_platform[@]+"${ffmpeg_platform[@]}"} \
  --pkg-config=pkg-config --pkg-config-flags=--static \
  --disable-everything --disable-autodetect --disable-network \
  --disable-doc --disable-debug --disable-ffplay --disable-ffprobe --enable-small \
  --enable-libdav1d \
  --enable-protocol=file,pipe \
  --enable-demuxer=mov,matroska,avi \
  --enable-decoder=h264,vp8,vp9,prores,mjpeg,libdav1d \
  --enable-parser=h264,vp8,vp9,mjpeg,av1 \
  --enable-bsf=vp9_superframe_split \
  --enable-filter=fps,format,scale,crop,null \
  --enable-encoder=mjpeg,rawvideo \
  --enable-muxer=image2pipe,rawvideo,null \
  >"$work/configure.log" || { tail -30 "$work/configure.log" "$work/ffmpeg-build/ffbuild/config.log"; exit 1; }
# The licence is what lets flowproof ship it: refuse anything but LGPL.
grep -q "^License: LGPL" "$work/configure.log" || {
  grep "^License" "$work/configure.log" >&2
  exit 1
}
make -j"$jobs" >/dev/null

# The Makefile's `ffmpeg` is already stripped (ffmpeg_g keeps the symbols).
cp "$exe" "$out/$exe"
{
  echo "flowproof ships ffmpeg $FFMPEG_VERSION, built by scripts/ffmpeg/build.sh in"
  echo "https://github.com/automators-com/flowproof from the unmodified source at"
  echo "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz, with dav1d $DAV1D_VERSION from"
  echo "https://downloads.videolan.org/pub/videolan/dav1d/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.xz."
  echo "ffmpeg is licensed under the GNU LGPL v2.1 or later; dav1d under the"
  echo "BSD 2-Clause licence. Both texts follow."
  echo
  echo "==== ffmpeg: GNU Lesser General Public License v2.1 ===="
  cat "$work/ffmpeg/COPYING.LGPLv2.1"
  echo
  echo "==== dav1d: BSD 2-Clause ===="
  cat "$work/dav1d/COPYING"
} >"$out/ffmpeg-licenses.txt"

ls -l "$out/$exe"
# Run it where this machine can: it must at least read AV1 and H.264.
case "$platform" in
  linux-x64 | darwin-arm64)
    decoders="$("$out/$exe" -hide_banner -decoders)"
    for want in h264 vp9 libdav1d; do
      grep -qE " $want( |$)" <<<"$decoders" || { echo "missing decoder: $want" >&2; exit 1; }
    done
    ;;
  *)
    file "$out/$exe"
    ;;
esac
