#!/usr/bin/env python3
"""Install this checkout's Python client into a new isolated environment."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def install(destination):
    target = Path(os.path.abspath(os.path.expanduser(str(destination))))
    # Reserve a fresh directory before venv can modify anything at this path.
    try:
        target.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        raise ValueError(f'{target} already exists; choose a new path with --venv') from None
    subprocess.run([sys.executable, '-m', 'venv', str(target)], check=True)
    python = target / ('Scripts/python.exe' if os.name == 'nt' else 'bin/python')
    # Older bundled pip versions copy the entire checkout, including local data.
    subprocess.run([str(python), '-m', 'pip', 'install', '--upgrade', 'pip>=23'], check=True)
    subprocess.run([str(python), '-m', 'pip', 'install', str(ROOT)], check=True)
    print(f'Installed. Run Python with: {python}')
    print('No environment activation is needed. The database server is installed separately.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--venv', type=Path, default=ROOT / '.venv-brainwire',
                        help='new environment directory (default: .venv-brainwire in this checkout)')
    args = parser.parse_args()
    try:
        install(args.venv)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'Setup failed: {error}', file=sys.stderr)
        print('Python must include venv and pip. Downloading dependencies may need internet access.\n'
              'An incomplete environment is kept for inspection; retry with a new --venv path.', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
