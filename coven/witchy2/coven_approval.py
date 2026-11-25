"""Bind maintainer user verification to the bytes of one staged release."""
import base64
from hashlib import sha384
import re
from urllib.parse import urlsplit

from coven_review_commit import CounterChanged
from coven_staging import canonical, read_file, read_json
from witchy_release import DIGEST, NAME

REGISTRY_ID = re.compile(r"[a-z0-9][a-z0-9.-]{0,63}")
HOST = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*")
B64 = re.compile(r"[A-Za-z0-9_-]+")
JSON_LIMIT = 65536
VALID_SECONDS = 300


def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def unb64(text):
    if not isinstance(text, str) or len(text) > JSON_LIMIT or not B64.fullmatch(text):
        raise ValueError("invalid base64url input")
    raw = base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
    if b64(raw) != text:
        raise ValueError("noncanonical base64url input")
    return raw


def checked_policy(document):
    if (not isinstance(document, dict) or set(document) != {"registry", "revision", "origin", "credentials"}
            or not isinstance(document["registry"], str) or not REGISTRY_ID.fullmatch(document["registry"])
            or type(document["revision"]) is not int or not 0 < document["revision"] < (1 << 63)):
        raise ValueError("invalid release policy")
    origin = document["origin"]
    if not isinstance(origin, str) or len(origin) > 300:
        raise ValueError("invalid release origin")
    parsed = urlsplit(origin)
    hostname = parsed.hostname
    canonical_origin = f"{parsed.scheme}://{hostname}" + (f":{parsed.port}" if parsed.port else "")
    if (not hostname or not HOST.fullmatch(hostname) or origin != canonical_origin
            or (parsed.scheme != "https" and (parsed.scheme, hostname) != ("http", "localhost"))
            or parsed.port == (443 if parsed.scheme == "https" else 80)):
        raise ValueError("release origin must be a canonical HTTPS origin or local http://localhost")
    credentials = document["credentials"]
    if not isinstance(credentials, dict) or len(credentials) > 256:
        raise ValueError("invalid credential policy")
    for ident, record in credentials.items():
        key = record.get("public_key") if isinstance(record, dict) else None
        scope = record.get("packages") if isinstance(record, dict) else None
        if (not isinstance(record, dict) or set(record) != {"public_key", "packages"}
                or not 1 <= len(unb64(ident)) <= 1023 or not 1 <= len(unb64(key)) <= 4096
                or not isinstance(scope, list) or not scope
                or not all(isinstance(name, str) and NAME.fullmatch(name) for name in scope)
                or len(set(scope)) != len(scope)):
            raise ValueError("invalid credential scope")
    return document


def challenge_for(envelope):
    return b64(sha384(b"coven release approval 1\x00" + canonical(envelope)).digest())


def require_digest(digest):
    if not DIGEST.fullmatch(digest):
        raise ValueError("expected a staged SHA-384 digest")


def challenge(directory, digest, issue, reader):
    require_digest(digest)
    release = reader.staged_release(directory, digest)
    policy, name, version = release["policy"], release["name"], release["version"]
    issued = issue(directory, {"operation": "release", "policy": policy, "digest": digest,
                               "name": name, "version": version})
    return challenge_reply(policy, issued["token"], issued["envelope"], release["allowed"])


def challenge_reply(policy, token, envelope, allowed):
    return {"envelope": envelope, "origin": policy["origin"],
            "publicKey": {"challenge": token, "rpId": urlsplit(policy["origin"]).hostname,
                          "timeout": VALID_SECONDS * 1000, "userVerification": "required",
                          "allowCredentials": [{"id": ident, "type": "public-key"} for ident in allowed]}}


def approve(directory, token, assertion_file, commit):
    return approve_assertion(directory, token, read_json(read_file(assertion_file, JSON_LIMIT)), commit)


def credential_id(assertion):
    if not isinstance(assertion, dict):
        raise ValueError("invalid maintainer assertion")
    ident = assertion.get("rawId")
    if not isinstance(ident, str) or ident != assertion.get("id"):
        raise ValueError("invalid maintainer assertion")
    return ident


def approval_snapshot(directory, token, assertion, browser, commit):
    snapshot = commit.snapshot(directory, token, credential_id(assertion))
    return snapshot | {"token": token, "assertion": assertion, "browser": browser}


def approve_assertion(directory, token, assertion, commit, *, browser=False):
    for attempt in range(2):
        request = approval_snapshot(directory, token, assertion, browser, commit)
        try:
            return commit(directory, request)
        except CounterChanged:
            if attempt:
                raise
