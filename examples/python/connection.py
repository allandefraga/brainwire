"""Optional service-token credentials shared by Python and native Bolt clients."""
import json
import os
import re


def auth_from_env():
    path = os.environ.get('BRAINWIRE_BOLT_AUTH_FILE')
    if path is None:
        return None
    if not path.strip():
        raise ValueError('BRAINWIRE_BOLT_AUTH_FILE must name a credentials file')
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('invalid Bolt credentials file')
            result[key] = value
        return result
    try:
        with open(path, 'rb') as stream:
            content = stream.read(65537)
        if len(content) > 65536:
            raise ValueError('invalid Bolt credentials file')
        value = json.loads(content, object_pairs_hook=unique_object)
        if not isinstance(value, dict) or set(value) != {'principal', 'credentials'}:
            raise ValueError('invalid Bolt credentials file')
        principal, token = value['principal'], value['credentials']
        if not isinstance(principal, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', principal):
            raise ValueError('invalid Bolt credentials file')
        if not isinstance(token, str) or not re.fullmatch(r'[A-Fa-f0-9]{64}', token):
            raise ValueError('invalid Bolt credentials file')
        return principal, token
    except (OSError, ValueError, TypeError):
        raise ValueError('cannot load valid Bolt credentials file') from None
