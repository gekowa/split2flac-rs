#!/usr/bin/env python3
"""Assemble relocatable releases; Python 3.12+, Cargo, bash/GNU make, pkg-config, C compiler.

  python packaging/package.py --target TARGET --fetch-only
  python packaging/package.py --target TARGET --offline --tools-only
  cargo fetch --locked --target TARGET
  python packaging/package.py --target TARGET --offline [--bin PATH]

Use MSYS2 UCRT64 bash on Windows (--bash PATH); install make, tar, diffutils,
gawk, grep, sed, mingw-w64-ucrt-x86_64-gcc and mingw-w64-ucrt-x86_64-pkgconf.
On Darwin use Xcode CLT and pkg-config; MinGW-w64 supports Windows cross builds.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
from urllib.request import urlopen
import zipfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
LOCK = HERE / 'tools.lock.json'
RECIPE = HERE / 'build-codecs.sh'
CONFIG = json.loads(LOCK.read_text(encoding='utf-8'))
STAGE = ROOT / 'target/packaging'
ALIASES = ('split2m4a', 'split2mp3', 'split2ogg', 'split2opus', 'split2wav')


def digest(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def sources(cache, offline):
    cache.mkdir(parents=True, exist_ok=True)
    result = {}
    for name, spec in CONFIG['sources'].items():
        path = cache / spec['url'].rsplit('/', 1)[1]
        if not path.exists():
            if offline:
                raise SystemExit(f'Offline cache missing {path}; run --fetch-only first')
            print(f'Fetching {spec["url"]}', flush=True)
            with tempfile.NamedTemporaryFile(dir=cache, delete=False) as f:
                tmp = Path(f.name)
            try:
                with urlopen(spec['url'], timeout=180) as response, tmp.open('wb') as out:
                    shutil.copyfileobj(response, out)
                if digest(tmp) != spec['sha256']:
                    raise SystemExit(f'{name}: download SHA-256 mismatch')
                tmp.replace(path)
            finally:
                tmp.unlink(missing_ok=True)
        if digest(path) != spec['sha256']:
            raise SystemExit(f'{path}: cached SHA-256 mismatch; remove it and fetch again')
        result[name] = path
    return result


def win(target):
    return '-windows-' in target


def binaries(target):
    suffix = '.exe' if win(target) else ''
    return ('ffmpeg' + suffix, 'ffprobe' + suffix)


def command_output(*cmd):
    return subprocess.check_output(cmd, text=True, errors='replace')


def check_binary(path, target):
    if win(target):
        with path.open('rb') as f:
            header = f.read(64)
            if header[:2] != b'MZ':
                raise ValueError(f'{path}: not PE')
            f.seek(struct.unpack_from('<I', header, 60)[0])
            coff = f.read(26)
            if coff[:6] != b'PE\0\0\x64\x86' or coff[24:26] != b'\x0b\x02':
                raise ValueError(f'{path}: not AMD64 PE32+')
        # Reject DLLs that just happen to be installed on the build machine.
        tool = shutil.which('x86_64-w64-mingw32-objdump') or shutil.which('llvm-objdump') or shutil.which('objdump')
        if not tool:
            raise SystemExit('MinGW objdump or llvm-objdump is required to validate Windows imports')
        info = command_output(tool, '-p', path)
        dlls = re.findall(r'DLL Name:\s*(\S+)', info, flags=re.I)
        system = {'advapi32', 'bcrypt', 'bcryptprimitives', 'comdlg32', 'crypt32', 'dwmapi', 'gdi32', 'iphlpapi', 'kernel32', 'msvcrt', 'ntdll', 'ole32', 'oleaut32', 'psapi', 'secur32', 'shell32', 'shlwapi', 'synchronization', 'ucrtbase', 'user32', 'userenv', 'version', 'ws2_32', 'winmm', 'normaliz', 'rpcrt4', 'setupapi', 'cfgmgr32'}
        if not dlls:
            raise ValueError(f'{path}: cannot identify imported DLLs')
        for dll in dlls:
            stem = Path(dll).stem.lower()
            if stem not in system and not stem.startswith(('api-ms-win-', 'ext-ms-win-')):
                raise ValueError(f'{path}: unexpected non-system DLL {dll}')
    else:
        arch = CONFIG['targets'][target]['machine']
        if command_output('/usr/bin/lipo', '-archs', path).split() != [arch]:
            raise ValueError(f'{path}: expected thin native {arch}')
        for line in command_output('/usr/bin/otool', '-L', path).splitlines()[1:]:
            library = line.strip().split(' (', 1)[0]
            if not library.startswith(('/usr/lib/', '/System/Library/')):
                raise ValueError(f'{path}: non-system dylib {library}')


def build_tools(target, archives, bash):
    # Cached objects are target-, recipe-, and source-specific, and rehashed on
    # reuse. Only source archives, never arbitrary downloaded binaries, enter here.
    key = hashlib.sha256(LOCK.read_bytes() + RECIPE.read_bytes()).hexdigest()
    directory = STAGE / 'tools' / target / key
    receipt = directory / 'hashes.json'
    if not receipt.exists():
        directory.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory.parent) as tmp:
            out = Path(tmp)
            script = str(RECIPE)
            if os.name == 'nt':
                script = RECIPE.as_posix()
            subprocess.run([bash, script, target, out, *(archives[k] for k in ('ffmpeg','ogg','vorbis','lame','opus','zlib'))], check=True)
            files = {p.name: digest(p) for p in out.iterdir() if p.is_file()}
            (out / 'hashes.json').write_text(json.dumps(files, indent=2), encoding='utf-8')
            out.rename(directory)
    files = json.loads(receipt.read_text(encoding='utf-8'))
    for name in (*binaries(target), 'config.mak', 'config.h', 'compiler.txt'):
        if digest(directory / name) != files[name]:
            raise ValueError(f'Changed cached media toolchain: {directory / name}')
    for name in binaries(target):
        check_binary(directory / name, target)
    enabled = set(re.findall(r'^CONFIG_(\w+)=yes$', (directory / 'config.mak').read_text(encoding='utf-8'), re.M))
    required = set('FFMPEG FFPROBE APE_DECODER FLAC_DECODER TTA_DECODER WAVPACK_DECODER PCM_S16LE_DECODER '
                   'FLAC_ENCODER AAC_ENCODER LIBMP3LAME_ENCODER LIBVORBIS_ENCODER LIBOPUS_ENCODER PCM_S16LE_ENCODER '
                   'APE_DEMUXER FLAC_DEMUXER TTA_DEMUXER WV_DEMUXER WAV_DEMUXER FLAC_MUXER IPOD_MUXER MP3_MUXER '
                   'OGG_MUXER OPUS_MUXER WAV_MUXER MJPEG_ENCODER PNG_DECODER SCALE_FILTER'.split())
    missing, forbidden = required - enabled, {'GPL', 'NONFREE', 'NETWORK'} & enabled
    if missing or forbidden:
        raise ValueError(f'{target}: missing {sorted(missing)}; forbidden {sorted(forbidden)}')
    return directory


def install_sources(bundle, target, archives, tools):
    root = bundle / 'source'
    media = root / 'codecs'
    media.mkdir(parents=True)
    for name, path in archives.items():
        spec = CONFIG['sources'][name]
        shutil.copyfile(path, media / path.name)
        notices = bundle / 'licenses' / name
        notices.mkdir(parents=True)
        with tarfile.open(path) as archive:
            for notice in spec['notices']:
                with archive.extractfile(spec['root'] + '/' + notice) as src, (notices / notice).open('wb') as dst:
                    shutil.copyfileobj(src, dst)
    for file in (RECIPE, LOCK, Path(__file__)):
        shutil.copyfile(file, media / file.name)
    for name in ('config.h', 'config.mak', 'compiler.txt', 'hashes.json'):
        shutil.copyfile(tools / name, media / name)
    # Registry archives include actual source AND original copyright/license files
    # (including MPL Symphonia), not just a list of URLs or SPDX identifiers.
    rust = root / 'rust'
    (rust / 'crates').mkdir(parents=True)
    meta = json.loads(command_output('cargo', 'metadata', '--locked', '--offline', '--format-version=1', '--filter-platform', target, '--manifest-path', ROOT / 'Cargo.toml'))
    crates = []
    import tomllib
    checksums = {(v['name'], v['version']):v['checksum'] for v in tomllib.loads((ROOT / 'Cargo.lock').read_text(encoding='utf-8'))['package'] if 'checksum' in v}
    for crate in meta['packages']:
        if crate['source'] is None and Path(crate['manifest_path']).resolve() == ROOT / 'Cargo.toml':
            continue
        src = Path(crate['manifest_path']).parent
        name = f'{crate["name"]}-{crate["version"]}'
        cache = src.parents[2] / 'cache' / src.parent.name / (name + '.crate')
        if not cache.is_file() or digest(cache) != checksums[(crate['name'], crate['version'])]:
            raise ValueError(f'{name}: missing or changed registry source {cache}; fetch locked crates first')
        shutil.copyfile(cache, rust / 'crates' / cache.name)
        crates.append(f'{name}: {crate["license"] or crate["license_file"]}; source and original notices: source/rust/crates/{cache.name}')
    (bundle / 'licenses' / 'rust-crates.txt').write_text('\n'.join(sorted(crates)) + '\n', encoding='utf-8')
    std = Path(command_output('rustc', '--print', 'sysroot').strip()) / 'share/doc/rust'
    std_notices = bundle / 'licenses' / 'rust-stdlib'
    std_notices.mkdir()
    if not (std / 'COPYRIGHT-library.html').is_file():
        raise SystemExit('Rust standard-library notices missing; install rustup component rust-docs before packaging')
    shutil.copyfile(std / 'COPYRIGHT-library.html', std_notices / 'COPYRIGHT-library.html')
    shutil.copytree(std / 'licenses', std_notices / 'licenses')
    if win(target):
        shutil.copytree(HERE / 'licenses/mingw-w64', bundle / 'licenses/mingw-w64')
    for filename in ('Cargo.toml', 'Cargo.lock'):
        shutil.copyfile(ROOT / filename, rust / filename)
    shutil.copytree(ROOT / 'src', rust / 'src')
    if (ROOT / 'build.rs').exists():
        shutil.copyfile(ROOT / 'build.rs', rust / 'build.rs')
    shutil.copyfile(HERE / 'SOURCE.txt', root / 'README.txt')
    shutil.copyfile(HERE / 'UPSTREAM-LICENSE.txt', bundle / 'licenses' / 'split2flac-original.txt')


def package(args, archives, tools):
    app = args.bin
    if app is None:
        env = os.environ.copy()
        if args.target.endswith('-msvc'):
            env.setdefault('CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', '-C target-feature=+crt-static')
        # Use the executable Cargo actually produced; target-dir is configurable.
        artifacts = subprocess.check_output([
            'cargo', 'build', '--release', '--locked', '--offline', '--target', args.target,
            '--bin', 'split2flac', '--message-format=json-render-diagnostics',
        ], cwd=ROOT, env=env, text=True)
        executables = [v['executable'] for v in map(json.loads, artifacts.splitlines())
                       if v.get('reason') == 'compiler-artifact' and v.get('executable')
                       and v['target']['name'] == 'split2flac']
        if len(executables) != 1:
            raise RuntimeError('Cargo did not report exactly one split2flac executable')
        app = Path(executables[0])
    check_binary(app, args.target)
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.out) as temp:
        bundle = Path(temp) / 'split2flac'
        (bundle / 'tools').mkdir(parents=True)
        suffix = '.exe' if win(args.target) else ''
        main = bundle / ('split2flac' + suffix)
        shutil.copy2(app, main)
        for alias in ALIASES:
            if win(args.target):
                shutil.copyfile(main, bundle / (alias + suffix))
            else:
                (bundle / alias).symlink_to(main.name)
        for name in binaries(args.target):
            shutil.copy2(tools / name, bundle / 'tools' / name)
        shutil.copyfile(ROOT / 'README.md', bundle / 'README.md')
        for lic in ('LICENSE', 'LICENSE.txt', 'COPYING'):
            if (ROOT / lic).is_file():
                shutil.copyfile(ROOT / lic, bundle / lic)
        (bundle / 'completions').mkdir()
        for name in ('split2flac.bash', '_split2flac', 'split2flac.ps1'):
            shutil.copyfile(ROOT / 'completions' / name, bundle / 'completions' / name)
        install_sources(bundle, args.target, archives, tools)
        archive_name = f'split2flac-{args.target}.' + CONFIG['targets'][args.target]['container']
        path = Path(temp) / archive_name
        if win(args.target):
            with zipfile.ZipFile(path, 'w', compression=zipfile.ZIP_DEFLATED) as z:
                for file in sorted(bundle.rglob('*')):
                    if file.is_file():
                        z.write(file, file.relative_to(bundle.parent))
        else:
            with tarfile.open(path, 'w:gz') as t:
                t.add(bundle, arcname=bundle.name)
        checksum = digest(path)
        output = args.out / archive_name
        path.replace(output)
        (args.out / (archive_name + '.sha256')).write_text(f'{checksum}  {archive_name}\n', encoding='ascii')
        print(f'{output}  sha256:{checksum}', flush=True)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--target', required=True, choices=CONFIG['targets'])
    p.add_argument('--cache', type=Path, default=STAGE / 'sources')
    p.add_argument('--bin', type=Path, help='prebuilt Rust split2flac executable')
    p.add_argument('--out', type=Path, default=ROOT / 'dist')
    p.add_argument('--bash', default='bash')
    p.add_argument('--offline', action='store_true', help='do not fetch missing codec sources')
    g = p.add_mutually_exclusive_group()
    g.add_argument('--fetch-only', action='store_true')
    g.add_argument('--tools-only', action='store_true')
    args = p.parse_args()
    archives = sources(args.cache.resolve(), args.offline)
    if not args.fetch_only:
        tools = build_tools(args.target, archives, args.bash)
        print(f'Verified media toolchain: {tools}', flush=True)
        if not args.tools_only:
            package(args, archives, tools)


if __name__ == '__main__':
    main()
