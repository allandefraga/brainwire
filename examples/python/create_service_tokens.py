"""Create a private, exclusive service-token directory for one graph.

This beta authenticates random service tokens, not human passwords or JWTs.
No token is printed. Rotation/revocation requires a server restart.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets


def create(directory, graph, include_observer=False, acl_writer=False):
    if not isinstance(graph, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', graph):
        raise ValueError('invalid graph name')
    if type(include_observer) is not bool or type(acl_writer) is not bool:
        raise ValueError('include_observer and acl_writer must be booleans')
    directory = Path(directory)
    directory.mkdir(mode=0o700)  # Refuse reuse rather than silently rotating existing clients.
    principals = []
    roles = [('writer', 'write'), ('reader', 'read'), ('health', 'read')]
    if include_observer:
        roles.append(('observer', 'read'))
    for name, access in roles:
        token = secrets.token_hex(32)
        principal = dict(id=name, token_sha256=hashlib.sha256(token.encode()).hexdigest(),
                         graphs={graph: access},
                         instance_admin=name == 'observer' or (acl_writer and name == 'writer'))
        principals.append(principal)
        write_private(directory/(name+'.json'), dict(principal=name, credentials=token))
    write_private(directory/'policy.json', dict(principals=principals))
    return directory


def write_private(path, value):
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory')
    parser.add_argument('--graph', required=True)
    parser.add_argument('--include-observer', action='store_true',
                        help='Also create a graph reader with instance memory telemetry permission')
    parser.add_argument('--acl-writer', action='store_true',
                        help='Also make writer an instance admin so it can stamp node ACLs (db.setNodeAcls)')
    args = parser.parse_args()
    directory = create(args.directory, args.graph, args.include_observer, args.acl_writer)
    extra = ', observer.json' if args.include_observer else ''
    print(f'Created policy.json, writer.json, reader.json and health.json{extra} in {directory}; no credentials printed.')
