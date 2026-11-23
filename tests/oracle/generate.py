#!/usr/bin/env python3
"""Freeze the pinned, unmodified upstream's observations. Never runs the Rust port.

Preload the linux/amd64 IMAGE pinned below with `docker pull --platform linux/amd64 IMAGE`.
python3 tests/oracle/generate.py                  # regenerate public reference, offline
python3 tests/oracle/generate.py --check          # regenerate without overwriting
python3 tests/oracle/generate.py --private /path/to/local-album
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from fixtures import (EMBEDDED, EXPECTED, PRIVATE_EXPECTED, build_wave, cases, cue,
                      differences, duration_errors, file_hash, milliseconds, observe, prepare)

IMAGE = 'mikenye/split2flac@sha256:47e6cb8d835a05f08efae20f7bf196552754df09e9dc957cc473b870f4dbc8ba'
SCRIPT = '/split2flac/split2flac'
SHA256 = '9150d8ccc6a47e0ea35bcdd4d5c790ce03c8cb70302f64a5512d7f78e0e79822'
COMMIT = '491591d21e2f97419e74751864b76b234b0fa2f1'


def run(root, tool, args=(), check=True):
    command = ['docker', 'run', '--rm', '--pull=never', '--network', 'none', '--platform', 'linux/amd64',
               '--user', f'{os.getuid()}:{os.getgid()}', '-e', 'HOME=/work/home',
               '-e', 'XDG_CONFIG_HOME=/work/home/config', '-e', 'LC_ALL=C.UTF-8',
               '-v', f'{root.resolve()}:/work', '-w', '/work', '--entrypoint', tool, IMAGE, *args]
    p = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, timeout=900)
    if (check and p.returncode) or p.returncode not in (0, 1, 2, 3):
        raise RuntimeError(f'{tool} exited {p.returncode}: {(p.stdout + p.stderr).decode(errors="replace")[-4000:]}')
    return p


def freeze_case(name, c, private, flac):
    with tempfile.TemporaryDirectory(prefix='split-oracle-case-') as tmp:
        root = Path(tmp)
        args, inputs = prepare(root, c, private, flac)
        proc = run(root, SCRIPT, args, check=False)
        record = observe(root, proc.returncode, inputs)
        outputs = record['outputs']
        # These sanity checks prevent accepting broken tooling as a new baseline.
        if name in ('plain', 'hidden', 'single-pre-post', 'anonymous-disc', 'private-disc',
                    'embedded', 'stem-over-embedded', 'suffix-over-stem', 'explicit-over-suffix'):
            if proc.returncode or record['ordered_pcm_sha256'] != inputs['audio']['pcm_sha256']:
                raise RuntimeError(f'{name}: upstream did not preserve source PCM:\n{(proc.stdout + proc.stderr).decode(errors="replace")[-3000:]}')
        print(f'{name}: exit={proc.returncode} {list(outputs)}', flush=True)
        return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='compare without changing frozen artifacts')
    parser.add_argument('--private', type=Path, metavar='ALBUM_DIR', help='record only local, unpublished album fixture')
    opt = parser.parse_args()
    all_cases = cases()
    target = PRIVATE_EXPECTED if opt.private else EXPECTED
    with tempfile.TemporaryDirectory(prefix='split-oracle-') as tmp:
        root = Path(tmp)
        (root / 'home').mkdir()
        sha = run(root, 'sha256sum', [SCRIPT]).stdout.decode().split()[0]
        if sha != SHA256:
            raise RuntimeError(f'unexpected script SHA-256 {sha}')
        versions = {tool: run(root, tool, ['--version']).stdout.decode().splitlines()[0]
                    for tool in ('cueprint', 'flac')}
        # shntool writes its banner to stderr. Redirect inside the container
        # so Docker's own stderr never becomes part of the frozen provenance.
        versions['shntool'] = run(root, '/bin/sh', ['-c', 'exec shntool -v 2>&1']).stdout.decode().splitlines()[0]
        provenance = {'image': IMAGE, 'platform': 'linux/amd64', 'commit': COMMIT, 'script_sha256': sha,
                      'locale': 'C.UTF-8', 'tools': versions}
        if opt.private:
            if not (opt.private / 'album.cue').stat().st_size:
                raise RuntimeError('the original CUE has no data')
            selected = {'private-disc': all_cases['anonymous-disc']}
            flac = None
        else:
            selected = all_cases
            flac = root / 'embedded.flac'
            c = all_cases['embedded']
            build_wave(root / 'image.wav', c)
            (root / 'internal.cue').write_bytes(cue({**c, 'tracks': ['INDEX 01 00:00:00', 'INDEX 01 00:00:14']}, 'Embedded'))
            run(root, 'flac', ['--silent', '--no-padding', '--no-seektable', '-o', flac.name, 'image.wav'])
            run(root, 'metaflac', ['--set-tag-from-file=CUESHEET=internal.cue', flac.name])
        with ThreadPoolExecutor(max_workers=3) as pool:
            records = dict(zip(selected, pool.map(lambda item: freeze_case(*item, opt.private, flac), selected.items())))
        disc = next(k for k in records if k in ('private-disc', 'anonymous-disc'))
        table = milliseconds(opt.private) if opt.private else selected[disc]['durations_ms']
        errors = duration_errors(records[disc], table)
        result = {'schema': 1, 'reference': provenance, 'cases': records,
                  'duration_check': {'case': disc, 'table_ms': table, 'mismatches': errors,
                                     'eof_basis': selected[disc]['eof_basis']}}
        if errors:
            print('\n'.join(errors), file=sys.stderr)
        if opt.check:
            changes = list(differences(json.loads(target.read_text(encoding='utf8')), result))
            if flac and file_hash(flac) != file_hash(EMBEDDED):
                changes.append('embedded.flac: input fixture SHA-256 differs')
            if changes:
                print('\n'.join(changes), file=sys.stderr)
                return 1
        else:
            if flac:
                shutil.copyfile(flac, EMBEDDED)
            target.write_text(json.dumps(result, indent=2, ensure_ascii=False, sort_keys=True)+'\n', encoding='utf8')
        print(f'{target.name}: {len(records)} cases; {len(errors)} duration mismatches. {selected[disc]["eof_basis"]}')
        return bool(errors)


if __name__ == '__main__':
    sys.exit(main())
