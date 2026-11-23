#!/usr/bin/env bash
# Invoked by package.py after SHA-256 verification. No network calls. Usage:
# build-codecs.sh TARGET DEST FFMPEG OGG VORBIS LAME OPUS ZLIB (source archives)
set -euo pipefail
export LC_ALL=C
unset CPATH LIBRARY_PATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH PKG_CONFIG_PATH CFLAGS CXXFLAGS LDFLAGS
paths=()
for arg in "${@:2}"; do
    if command -v cygpath >/dev/null; then arg=$(cygpath -u "$arg"); fi
    paths+=("$arg")
done
target=$1; dest=${paths[0]}
work=$(mktemp -d /tmp/split2flac-codecs.XXXXXXXX)
trap 'rm -rf "$work"' EXIT
cd "$work"
packages=(ffmpeg ogg vorbis lame opus zlib)
for i in "${!packages[@]}"; do
    mkdir "${packages[i]}"
    tar xf "${paths[i+1]}" -C "${packages[i]}" --strip-components=1
done
prefix=$work/prefix
mkdir -p "$prefix/lib" "$prefix/include" "$dest"
jobs=${SPLIT2FLAC_BUILD_JOBS:-4}
export PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig
# pkg-config on native Windows is a MinGW program. Use a Windows absolute
# path for the .pc files, not an MSYS /tmp prefix it cannot open.
if command -v cygpath >/dev/null; then
    prefix=$(cygpath -m "$prefix")
    export PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig
fi
export CPPFLAGS="-I$prefix/include" LDFLAGS="-L$prefix/lib" CFLAGS='-O2 -g0'
fftarget=(); host=(); suffix=''
case "$target" in
    x86_64-pc-windows-*)
        cross=x86_64-w64-mingw32-
        # UCRT64 installs binutils as ar/ranlib/objdump, without the
        # cross prefix used on a Darwin/Linux build host.
        if command -v cygpath >/dev/null && [[ $(gcc -dumpmachine) == x86_64-w64-mingw32 ]]; then cross=; fi
        export CC=${cross}gcc AR=${cross}ar RANLIB=${cross}ranlib
        export CXX=${cross}g++
        host=(--host=x86_64-w64-mingw32)
        fftarget=(--target-os=mingw32 --arch=x86_64 --cross-prefix="$cross" --enable-cross-compile --disable-pthreads)
        LDFLAGS+=" -static -static-libgcc"
        suffix=.exe
        (cd zlib && make -j"$jobs" -fwin32/Makefile.gcc PREFIX="$cross" libz.a && cp libz.a "$prefix/lib/" && cp zlib.h zconf.h "$prefix/include/")
        ;;
    *-apple-darwin)
        arch=${target%%-*}; [[ "$arch" != aarch64 ]] || arch=arm64
        [[ $(uname -s) == Darwin ]] || { echo 'Darwin build requires an Apple SDK' >&2; exit 1; }
        export MACOSX_DEPLOYMENT_TARGET=11.0
        export CC="clang -arch $arch" CXX="clang++ -arch $arch" AR=ar RANLIB=ranlib
        if [[ "$arch" != "$(uname -m)" ]]; then
            host=(--host="${target%%-*}-apple-darwin")
            fftarget=(--target-os=darwin --arch="${target%%-*}" --enable-cross-compile)
        fi
        (cd zlib && ./configure --static --prefix="$prefix" && make -j"$jobs" install)
        ;;
    *) echo "unsupported target: $target" >&2; exit 1;;
esac
common=(--prefix="$prefix" --disable-shared --enable-static --disable-dependency-tracking)
for pkg in ogg vorbis lame opus; do
    (
        cd "$pkg"
        # LAME 3.100 predates Apple Silicon. Refresh only Autoconf's host
        # detection scripts from the pinned Opus archive (included in package).
        if [[ "$pkg" != opus ]]; then cp ../opus/config.guess ../opus/config.sub .; fi
        case "$pkg" in
            ogg) options=();;
            vorbis)
                options=(--disable-oggtest --with-ogg="$prefix")
                if [[ "$target" == *-apple-darwin ]]; then
                    # Vorbis unconditionally resets its Darwin CFLAGS to a
                    # PowerPC-era ld flag rejected by modern Xcode.
                    sed 's/-force_cpusubtype_ALL //g' configure > configure.current
                    mv configure.current configure; chmod +x configure
                fi;;
            lame) options=(--disable-frontend --disable-decoder); export CFLAGS="$CFLAGS -std=gnu17";;
            opus) options=(--disable-extra-programs --disable-doc);;
        esac
        ./configure "${common[@]}" ${host[@]+"${host[@]}"} ${options[@]+"${options[@]}"}
        make -j"$jobs" install
    )
done
cd ffmpeg
./configure --prefix="$prefix" --cc="$CC" ${fftarget[@]+"${fftarget[@]}"} \
    --disable-autodetect --disable-network --disable-shared --enable-static --disable-doc \
    --disable-debug --disable-x86asm --disable-ffplay --disable-everything \
    --enable-libmp3lame --enable-libopus --enable-libvorbis --enable-zlib \
    --pkg-config=pkg-config --pkg-config-flags=--static --extra-cflags="$CPPFLAGS" --extra-ldflags="$LDFLAGS" \
    --enable-protocol=file,pipe,fd --enable-indev=lavfi \
    '--enable-decoder=ape,flac,tta,wavpack,pcm_*,aac,mp3*,vorbis,opus,png,mjpeg,bmp,gif,tiff,webp,ppm,rawvideo' \
    '--enable-encoder=flac,tta,wavpack,pcm_*,aac,libmp3lame,libvorbis,libopus,mjpeg,png,rawvideo,wrapped_avframe' \
    '--enable-demuxer=ape,flac,tta,wv,wav,w64,pcm_*,mov,mp3,ogg,image2,image2pipe,*_pipe,aac' \
    '--enable-muxer=flac,tta,wv,wav,pcm_*,ipod,mp4,mp3,ogg,opus,image2,image2pipe,adts,null,md5,hash,framemd5' \
    --enable-parser=aac,flac,mpegaudio,opus,vorbis,mjpeg,png,pnm \
    --enable-filter=scale,pad,format,aformat,aresample,asplit,anull,atrim,anullsrc,sine,color,volume
make -j"$jobs" ffmpeg"$suffix" ffprobe"$suffix"
for tool in ffmpeg ffprobe; do cp "$tool$suffix" "$dest/$tool$suffix"; done
cp ffbuild/config.mak config.h "$dest/"
"${CC%% *}" --version > "$dest/compiler.txt"
