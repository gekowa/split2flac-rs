#!/usr/bin/env python3
"""Compare a native CLI against frozen upstream observations; no Docker or PATH codecs.

python3 tests/compare.py --bin /absolute/path/to/split2flac-rs
python3 tests/compare.py --bin /absolute/path/to/split2flac-rs --private /path/to/local-album
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from oracle.fixtures import (EXPECTED, PRIVATE_EXPECTED, cases, differences,
                             duration_errors, milliseconds, observe, prepare, workspace_state)


def compare_record(name, expected, actual):
    return list(differences(expected, actual, name))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bin', nargs='+', required=True, metavar='COMMAND', help='native executable (or launcher and arguments)')
    p.add_argument('--private', type=Path, metavar='ALBUM_DIR', help='use the unmodified local CUE and independent result.md')
    p.add_argument('--case', action='append', metavar='NAME', help='only run named case (repeatable)')
    p.add_argument('--expected', type=Path, help='alternate read-only manifest')
    opts = p.parse_args()
    command = [str(Path(opts.bin[0]).resolve(strict=True)), *opts.bin[1:]]
    manifest = opts.expected or (PRIVATE_EXPECTED if opts.private else EXPECTED)
    if not manifest.is_file():
        p.error(f'{manifest} not found; generate it with tests/oracle/generate.py first')
    frozen = json.loads(manifest.read_text(encoding='utf8'))
    all_cases = cases()
    selected = {'private-disc': all_cases['anonymous-disc']} if opts.private else all_cases
    if set(selected) != set(frozen['cases']):
        p.error('fixture and expectation case lists differ')
    if opts.case:
        unknown = set(opts.case) - selected.keys()
        if unknown:
            p.error(f'unknown case(s): {sorted(unknown)}')
        selected = {k: selected[k] for k in opts.case}
    failures = 0
    for name, case in selected.items():
        with tempfile.TemporaryDirectory(prefix='split2flac-') as tmp:
            root = Path(tmp) / '原生 oracle'
            root.mkdir()
            args, inputs = prepare(root, case, opts.private)
            home = root / 'home'
            env = {**os.environ, 'HOME': str(home), 'USERPROFILE': str(home), 'APPDATA': str(home),
                   'XDG_CONFIG_HOME': str(home / 'config'), 'LC_ALL': 'C.UTF-8'}
            ref = frozen['cases'][name]
            errors = list(differences(ref['inputs'], inputs, name+'/inputs'))
            if errors:
                proc = None  # different input bytes cannot be compared to this recording
            else:
                before = workspace_state(root, inputs['files'])
                proc = subprocess.run([*command, *args], cwd=root, env=env, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=900)
                try:
                    errors.extend(differences(before, workspace_state(root), name+'/workspace'))
                    obs = observe(root, proc.returncode, inputs)
                    errors.extend(compare_record(name, ref, obs))
                    if 'durations_ms' in case:
                        table = milliseconds(opts.private) if opts.private else case['durations_ms']
                        errors.extend(f'{name}/{s}' for s in duration_errors(obs, table))
                        for n, ms in enumerate(table, 1):
                            track = obs['outputs'].get(f'{n:02} - Track.wav', {})
                            print(f'{name}/{n:02}: table={ms} ms, observed={track.get("duration_ms")} ms, frames={track.get("frames")}')
                        print(case['eof_basis'])
                except (ValueError, OSError) as exc:
                    errors.append(f'{name}: {exc}')
            if errors:
                failures += 1
                print('\n'.join(errors), file=sys.stderr, flush=True)
                if proc is not None:
                    print(proc.stdout.decode(errors='replace')[-3000:], file=sys.stderr, flush=True)
            else:
                print(f'{name}: matched exit {ref["exit"]} and {len(ref["outputs"])} files', flush=True)
    print(f'{len(selected)-failures}/{len(selected)} cases matched')
    return int(bool(failures))


if __name__ == '__main__':
    sys.exit(main())
