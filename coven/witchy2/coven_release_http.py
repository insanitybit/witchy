"""Launch Witchy HTTPS and answer its private maintainer action requests."""
from pathlib import Path
import os
import tempfile
from urllib.parse import urlsplit

import coven_approval as approval
from coven_https import require_loopback, run_listener
from coven_review_baseline import pinned_baseline
from coven_review_commit import pinned_commit, pinned_inspection, pinned_read_challenge
from coven_release_review import differences, inspect, review_challenge
from coven_staging import canonical, has_ci_provenance, read_file, read_json
from coven_target import TRUST_BYTES, parse_trust

CHALLENGE = 3
INSPECT = 4
APPROVE = 5
REPLY_LIMIT = 20 << 20
SMALL_REPLY_LIMIT = 16 << 10


def review_url(directory, digest, reader):
    approval.require_digest(digest)
    policy = reader.staged_release(directory, digest)["policy"]
    if urlsplit(policy["origin"]).scheme != "https":
        raise ValueError("maintainer browser review requires HTTPS")
    return policy["origin"] + "/review/" + digest.removeprefix("sha384:")


class Actions:
    def __init__(self, directory, mirror, trust, trust_pin, inspection, baseline, commit, issue):
        policy = commit.active_policy(directory)
        self.has_provenance = has_ci_provenance(directory)
        origin = urlsplit(policy["origin"])
        if origin.scheme != "https" or not mirror.is_dir() or mirror.is_symlink():
            raise ValueError("maintainer service needs HTTPS and a read-only published mirror directory")
        self.directory, self.mirror = directory.resolve(), mirror.resolve()
        if self.directory.is_relative_to(self.mirror) or self.mirror.is_relative_to(self.directory):
            raise ValueError("private staging and the published mirror must be separate trees")
        self.trust = read_file(trust, TRUST_BYTES)
        self.trust_pin = trust_pin
        self.registry = policy["registry"]
        self.inspection = inspection
        self.baseline = baseline
        self.commit = commit
        self.issue = issue
        if parse_trust(self.trust, self.trust_pin)["policy"]["registry"] != self.registry:
            raise ValueError("release trust names another registry")
        self.authority = origin.netloc

    def dispatch(self, packet):
        if not 2 <= len(packet) <= approval.JSON_LIMIT + 1 or packet[0] not in (CHALLENGE, INSPECT, APPROVE):
            return b"\1"
        route = packet[0]
        try:
            data = read_json(packet[1:])
            response = b"\0" + canonical(self.action(route, data))
        except (ValueError, OSError, KeyError):
            return b"\1"
        limit = REPLY_LIMIT if route == INSPECT else SMALL_REPLY_LIMIT
        return response if len(response) <= limit else b"\2"

    def action(self, route, data):
        if route == CHALLENGE:
            if not isinstance(data, dict) or set(data) != {"digest"} or not isinstance(data["digest"], str):
                raise ValueError("invalid review reference")
            return review_challenge(self.directory, data["digest"], self.issue, self.commit)
        if not isinstance(data, dict) or set(data) != {"challenge", "assertion"}:
            raise ValueError("invalid maintainer assertion request")
        if route == APPROVE:
            receipt = approval.approve_assertion(self.directory, data["challenge"], data["assertion"], self.commit, browser=True)
            return {"digest": receipt["envelope"]["intent"]["digest"]}
        registry, release, provenance, pending, previous_approval = inspect(
            self.directory, data["challenge"], data["assertion"], self.inspection, self.commit, self.has_provenance)
        previous = self.baseline(self.directory, self.mirror, release, self.trust, self.trust_pin, registry)
        comparison = differences(previous, release)
        return {"registry": registry, "name": release.name, "version": release.version, "digest": release.digest,
                "provenance": provenance, "approval": pending, "comparison": comparison,
                "approved": previous_approval == (release.digest,)}


def write_assets(public, authority, chain):
    assets = Path(__file__).resolve().with_name("release_web")
    (public / "authority").write_bytes(authority.encode("ascii"))
    (public / "chain").write_bytes(chain)
    for name, limit in (("review.mjs", 16384), ("enroll.mjs", 16384), ("review.css", 8192)):
        (public / name).write_bytes(read_file(assets / name, limit))


def serve(args, host):
    require_loopback(args.listen)
    chain = read_file(args.chain, 65733)
    with (pinned_inspection(args.inspect_module, args.inspect_pin, host) as inspection,
          pinned_read_challenge(args.challenge_module, args.challenge_pin, host) as issue,
          pinned_commit(args.commit_module, args.commit_pin, host) as commit,
          pinned_baseline(args.baseline_module, args.baseline_pin, host, args.root, args.root_pin) as baseline,
          tempfile.TemporaryDirectory(prefix="coven-review-", dir=os.path.realpath("/tmp")) as path):
        actions = Actions(args.staging_dir, args.mirror, args.trust, args.trust_pin, inspection, baseline, commit, issue)
        public = Path(path) / "public"
        public.mkdir(mode=0o700)
        write_assets(public, actions.authority, chain)
        flags = ("--connection-deadline-ms=20000", "--waiting-connections=16",
                 "--memory-pages=408", "--module-bytes=131072", "--fuel=250000000",
                 "--net-bytes=unlimited", "--net-bytes-per-connection=45000000",
                 "--fs-bytes=unlimited", "--fs-bytes-per-connection=100000",
                 f"--local-service-reply-bytes={REPLY_LIMIT}")
        run_listener(host, args.module, args.listen, args.tls_key, public, actions.dispatch,
                     approval.JSON_LIMIT + 1, REPLY_LIMIT, flags, module_pin=args.module_pin)
