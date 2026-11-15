"""Transport options and service-token credentials for Bolt examples."""
from ipaddress import ip_address
import json
import os
import re
import stat
from urllib.parse import urlsplit


def bolt_connection_options(uri, tls_ca=None):
    """Validate transport before loading credentials or opening a driver."""
    message = "use a bolt:// or verified bolt+s:// URI without embedded credentials"
    try:
        address = urlsplit(uri)
        port = address.port
    except ValueError:
        raise ValueError(message) from None
    if (address.scheme not in ("bolt", "bolt+s") or address.hostname is None
            or address.username is not None or address.path not in ("", "/")
            or address.query or address.fragment or port == 0):
        raise ValueError(message)
    if tls_ca is not None:
        if address.scheme != "bolt" or not tls_ca.strip():
            raise ValueError("use bolt:// with a PEM CA file for --tls-ca")
        from neo4j import TrustCustomCAs

        return uri, {"encrypted": True, "trusted_certificates": TrustCustomCAs(tls_ca)}
    if address.scheme == "bolt+s":
        return uri, {}

    host = "127.0.0.1" if address.hostname == "localhost" else address.hostname
    try:
        loopback = ip_address(host)
    except ValueError:
        raise ValueError("use verified TLS for non-loopback Bolt endpoints") from None
    if not loopback.is_loopback:
        raise ValueError("use verified TLS for non-loopback Bolt endpoints")
    literal = f"[{loopback}]" if loopback.version == 6 else str(loopback)
    return f"bolt://{literal}:{7687 if port is None else port}", {}


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
        # Inspect the opened target so mounted secret symlinks remain supported.
        with os.fdopen(os.open(path, os.O_RDONLY | getattr(os, 'O_NONBLOCK', 0)), 'rb') as stream:
            metadata = os.fstat(stream.fileno())
            if not stat.S_ISREG(metadata.st_mode) or \
                    os.name == 'posix' and metadata.st_mode & 0o022:
                raise ValueError('invalid Bolt credentials file')
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
