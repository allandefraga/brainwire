#!/usr/bin/env python3
import os,re,sys,uuid,subprocess
from pathlib import Path
args=sys.argv[1:]
config=Path(args[args.index('--config')+1])
port=re.search(r'^listen = "127\.0\.0\.1:(\d+)"',config.read_text(),re.M).group(1)
command=['docker','run','--rm','--init','--name','brainwire-memory-'+uuid.uuid4().hex[:10],
         '--user',f'{os.getuid()}:{os.getgid()}', '--env',f'BRAINWIRE_SERVER_DATA_DIR={config.parent}/data', '--mount',f'type=bind,src={config.parent},dst={config.parent}']
if args[0]=='serve':
    command += ['--publish',f'127.0.0.1:{port}:{port}','--env',f'BRAINWIRE_SERVER_LISTEN=0.0.0.0:{port}']
command += ['brainwire:20261003-session2-release',*args]
if args[0] != 'serve':
    result=subprocess.run(command,capture_output=True)
    if result.returncode:
        Path('/tmp/brainwire-docker-wrapper-error-20261003.log').write_bytes(result.stderr)
    sys.stdout.buffer.write(result.stdout)
    sys.stderr.buffer.write(result.stderr)
    raise SystemExit(result.returncode)
os.execvp(command[0],command)
