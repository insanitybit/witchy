"""GitHub Actions identity and enrolled-key proof for private candidate staging."""
import re
from urllib.parse import urlsplit

from coven_approval import HOST
from coven_ci_identity import NonceKey
from coven_staging import database_path, read_file
from witchy_release import DIGEST, PAYLOAD_BYTES, digest_of

CHALLENGE = "/v1/ci/challenge"
STAGE = "/v1/ci/stage"
RECEIPT = "/v1/ci/receipt"
TOKEN_BYTES = 8192
TOKEN = re.compile(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")


def pinned_bytes(path, pin):
    data = read_file(path, 256 << 10, owner_writes_only=True)
    if digest_of(data) != pin:
        raise ValueError("CI configuration input differs from its independent pin")
    return data


def target_origin(url):
    host = urlsplit(url)
    if (host.scheme != "https" or not host.hostname or len(host.hostname) > 253 or not HOST.fullmatch(host.hostname)
            or host.port in (0, 443) or url != "https://" + host.hostname + (f":{host.port}" if host.port else "")):
        raise ValueError("CI staging requires a canonical HTTPS origin")
    return host


class Staging:
    def __init__(self, config, config_pin, jwks, directory, nonce_key, identity, storage):
        configuration = pinned_bytes(config, config_pin).decode("utf-8")
        self.keys = read_file(jwks, 256 << 10, owner_writes_only=True).decode("utf-8")
        self.identity = identity
        self.nonce_key = NonceKey(nonce_key)
        document = self.identity.check_configuration(self.nonce_key, configuration, self.keys)
        self.origin = document["origin"]
        self.authority = self.origin.removeprefix("https://")
        self.registry = document["registry"]
        self.audience = f"{self.origin}/v1/ci/{self.registry}"
        self.packages = document["packages"]
        self.storage = storage
        self.directory = database_path(directory).parent

    def challenge(self):
        return {"nonce": self.identity.challenge(self.nonce_key, self.audience)}

    def request(self, route, token, nonce, package, digest, signature, body):
        if (route not in (STAGE, RECEIPT) or not DIGEST.fullmatch(digest) or len(body) > PAYLOAD_BYTES
                or len(token) > TOKEN_BYTES or not TOKEN.fullmatch(token)):
            raise ValueError("invalid private CI request")
        request = {"token": token, "keys": self.keys, "packages": self.packages,
                   "origin": self.origin, "registry": self.registry, "route": route, "nonce": nonce,
                   "package": package, "digest": digest, "proof": signature}
        return self.storage.commit(self.directory, self.nonce_key, request, body)
