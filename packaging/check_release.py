#!/usr/bin/env python3
"""Exercise an unpacked release, including every codec pair, without PATH tools."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import wave
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    ('Darwin', 'arm64'): 'aarch64-apple-darwin',
    ('Darwin', 'x86_64'): 'x86_64-apple-darwin',
    ('Windows', 'amd64'): 'x86_64-pc-windows-msvc',
}
CODECS = {'flac': 'flac', 'm4a': 'aac', 'mp3': 'mp3', 'ogg': 'vorbis', 'opus': 'opus', 'wav': 'pcm_s16le'}
APE_HASH = '7c3e30ff33812c909baf9107c479a82d67b307fcb844899ec366d65626d6313b'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def matrix(program, work, env):
    suffix = program.suffix
    ffmpeg = program.parent / 'tools' / ('ffmpeg' + suffix)
    ffprobe = program.parent / 'tools' / ('ffprobe' + suffix)

    def output(*args):
        return subprocess.check_output(args, env=env, cwd=work)

    def ff(*args):
        return output(ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-xerror', *args)

    def pcm(path):
        return ff('-i', path, '-map', '0:a:0', '-f', 's16le', '-acodec', 'pcm_s16le', '-')

    raw = b''.join(struct.pack('<hh', (i * 13) % 17011 - 8505, 2300 if i % 139 < 69 else -2301) for i in range(16631))
    wav = work / 'disc.wav'
    with wave.open(str(wav), 'wb') as f:
        f.setparams((2, 2, 44100, 0, 'NONE', 'not compressed'))
        f.writeframes(raw)
    ape = work / 'disc.ape'
    shutil.copyfile(ROOT / 'tests/fixtures/ramp.ape', ape)
    require(hashlib.sha256(ape.read_bytes()).hexdigest() == APE_HASH, 'synthetic APE changed')
    for ext, codec in [('flac', 'flac'), ('tta', 'tta'), ('wv', 'wavpack')]:
        ff('-i', wav, '-c:a', codec, wav.with_suffix('.' + ext))
    # A genuine RGB PNG, independent of the encoders we are testing.
    def chunk(kind, value):
        return struct.pack('>I', len(value)) + kind + value + struct.pack('>I', zlib.crc32(kind + value))
    cover = work / '合成 cover.png'
    image = b''.join(b'\0' + bytes([row, 64, 192]) * 32 for row in range(32))
    cover.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 32, 32, 8, 2, 0, 0, 0))
                      + chunk(b'IDAT', zlib.compress(image)) + chunk(b'IEND', b''))
    fingerprints = {}
    for ext in ('wav', 'ape', 'flac', 'tta', 'wv'):
        source = wav.with_suffix('.' + ext)
        (work / 'disc.cue').write_text(f'PERFORMER "Fixture"\nTITLE "Pair test"\nFILE "{source.name}" WAVE\n'
            '  TRACK 01 AUDIO\n    TITLE "Before"\n    INDEX 01 00:00:00\n'
            '  TRACK 02 AUDIO\n    TITLE "After"\n    INDEX 01 00:00:12\n', encoding='utf-8')
        require(pcm(source) == raw, f'{ext}: input does not contain reference PCM')
        for fmt, codec in CODECS.items():
            case = f'{ext}-to-{fmt}'
            dest = work / case
            dest.mkdir()
            alias = program.with_name('split2' + fmt + suffix)
            output(alias, '-nd', '-nC', '-c', cover, '-cs', '16x16', '-nask', '-o', dest, '-of', '@track - @title.@ext', source)
            paths = sorted(dest.iterdir())
            require([p.name for p in paths] == [f'01 - Before.{fmt}', f'02 - After.{fmt}'], f'{case}: {paths}')
            for n, p in enumerate(paths):
                info = json.loads(output(ffprobe, '-v', 'error', '-show_streams', '-show_format', '-of', 'json', p))
                streams = info['streams']
                audio = [s for s in streams if s['codec_type'] == 'audio']
                require(len(audio) == 1 and audio[0]['codec_name'] == codec and audio[0]['channels'] == 2, f'{case}: {streams}')
                pictures = [(s['width'], s['height']) for s in streams if s['disposition']['attached_pic']]
                require(pictures == ([(16, 16)] if fmt in ('m4a', 'flac') else []), f'{case}: cover streams {pictures}')
                tags = {k.lower(): v for group in (info['format'], audio[0]) for k, v in group.get('tags', {}).items()}
                for key, value in [('title', ('Before', 'After')[n]), ('album', 'Pair test'), ('artist', 'Fixture')]:
                    require(tags.get(key) == (None if fmt == 'wav' else value), f'{case}: {p.name} {key}: {tags}')
                data = pcm(p)
                require(data, f'{case}: empty decoded track {n + 1}')
                if fmt in ('flac', 'wav'):
                    require(data == (raw[:7056 * 4] if n == 0 else raw[7056 * 4:]), f'{case}: wrong PCM at {p.name}')
                # Same input PCM fed to the same encoder on THIS host; this tests
                # track identity without comparing lossy audio to lossless PCM.
                digest = hashlib.sha256(data).hexdigest()
                key = (fmt, n)
                if ext == 'wav':
                    fingerprints[key] = digest
                else:
                    require(digest == fingerprints[key], f'{case}: different decoded track {n + 1}')
            print(f'PASS {case}', flush=True)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('archive', type=Path)
    p.add_argument('--target', required=True, choices=[*TARGETS.values(), 'x86_64-pc-windows-gnu', 'x86_64-pc-windows-gnullvm'])
    args = p.parse_args()
    local = TARGETS.get((platform.system(), platform.machine().lower()))
    host_target = 'x86_64-pc-windows-msvc' if '-windows-' in args.target else args.target
    require(local == host_target, f'{args.target} needs native hardware; this Python reports {local}')
    if platform.system() == 'Darwin':
        translated = subprocess.run(['/usr/sbin/sysctl', '-in', 'sysctl.proc_translated'], capture_output=True)
        require(translated.stdout.strip() != b'1', 'Rosetta is not native acceptance')
    else:
        # x64 Python may report AMD64 even when emulated on Windows ARM64.
        import ctypes
        process_machine = ctypes.c_ushort()
        native_machine = ctypes.c_ushort()
        ok = ctypes.windll.kernel32.IsWow64Process2(
            ctypes.c_void_p(-1), ctypes.byref(process_machine), ctypes.byref(native_machine))
        require(ok and process_machine.value == 0 and native_machine.value == 0x8664, 'native Windows AMD64 required')
    archive = args.archive.resolve()
    with archive.open('rb') as stream:
        checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
    print(f'{platform.platform()} | {platform.machine()} | {archive.name} | sha256={checksum}', flush=True)
    with tempfile.TemporaryDirectory(prefix='split2flac ') as scratch:
        work = Path(scratch) / '原生 测试'
        work.mkdir()
        if archive.suffix == '.zip':
            with zipfile.ZipFile(archive) as z:
                z.extractall(work)
        else:
            with tarfile.open(archive, 'r:gz') as t:
                t.extractall(work, filter='data')
        program = work / 'split2flac' / ('split2flac.exe' if os.name == 'nt' else 'split2flac')
        home = work / '测试 home'
        home.mkdir()
        env = dict(os.environ, HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(home),
            APPDATA=str(home), PATH=str(work / 'empty-path'), PYTHONUTF8='1')
        for name in ('SPLIT2FLAC_TOOLS', 'FFMPEG', 'FFPROBE', 'DYLD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH'):
            env.pop(name, None)
        subprocess.run([sys.executable, ROOT / 'tests/compare.py', '--bin', program], env=env, cwd=work, check=True)
        matrix(program, work, env)


if __name__ == '__main__':
    main()
