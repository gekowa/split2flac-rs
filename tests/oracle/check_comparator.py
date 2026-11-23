#!/usr/bin/env python3
"""Black-box checks of the comparator's diagnostic and exit status; never edits the oracle."""
import argparse
import os
from pathlib import Path
import struct
import subprocess
import sys

from fixtures import HERE, SOURCE


def run_and_mutate(mutation, binary, args):
    """Launch the real implementation and corrupt its *actual* private test artifacts."""
    proc = subprocess.run([binary, *args])
    hidden = Path('out/00.wav')
    if mutation == 'shifted-pcm':
        with hidden.open('r+b') as f:
            f.seek(12)
            while True:
                chunk, size = struct.unpack('<4sI', f.read(8))
                if chunk == b'data':
                    pcm_offset = f.tell()
                    pcm = f.read(size)
                    assert len(pcm) == size and pcm[:4] != pcm[4:8]
                    # Rotate by one whole stereo frame. Header, file length and
                    # sample count remain intact; only the PCM hashes can catch it.
                    f.seek(pcm_offset)
                    f.write(pcm[4:] + pcm[:4])
                    break
                f.seek(size + size % 2, 1)
    elif mutation == 'missing-hidden':
        hidden.unlink()
    elif mutation == 'stray-output':
        (SOURCE / 'stray.wav').write_bytes(b'unexpected')
    elif mutation == 'deleted-input':
        Path(args[-1]).unlink()
    elif mutation == 'short-header':
        hidden.write_bytes(b'RIFF\x00\x00\x00\x00WAVEfm')
    elif mutation == 'short-format':
        hidden.write_bytes(b'RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01')
    else:
        raise ValueError(mutation)
    return proc.returncode


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bin', type=Path, required=True)
    opt = p.parse_args()
    binary = str(opt.bin.resolve(strict=True))
    probes = [
        ('shifted-pcm', 'hidden', 'hidden/outputs/00.wav/pcm_sha256: expected'),
        ('missing-hidden', 'hidden', 'hidden/outputs/00.wav: missing'),
        ('stray-output', 'cut-beyond-eof', f'cut-beyond-eof/workspace/{SOURCE}/stray.wav: unexpected'),
        ('deleted-input', 'cut-beyond-eof', f'cut-beyond-eof/workspace/{SOURCE}/image.wav: missing'),
        ('short-header', 'hidden', 'hidden: 00.wav: truncated WAVE chunk header'),
        ('short-format', 'hidden', 'hidden: 00.wav: truncated WAVE fmt chunk'),
    ]
    for mutation, case, diagnostic in probes:
        command = [sys.executable, str(HERE.parent / 'compare.py'), '--case', case, '--bin',
                   sys.executable, str(Path(__file__).resolve()), 'mutate', mutation, binary]
        proc = subprocess.run(command, capture_output=True, encoding='utf8',
                              env={**os.environ, 'PYTHONIOENCODING': 'utf8'})
        if proc.returncode != 1 or diagnostic not in proc.stderr or '0/1 cases matched' not in proc.stdout:
            sys.exit(f'{mutation}: expected {diagnostic!r} and comparison failure, got {proc.returncode}: {proc.stderr}')
        print(f'{mutation}: rejected')


if __name__ == '__main__':
    if sys.argv[1:2] == ['mutate']:
        sys.exit(run_and_mutate(sys.argv[2], sys.argv[3], sys.argv[4:]))
    main()
