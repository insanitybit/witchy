"""Operator-issued passkey enrollment and replacement."""
from coven_approval import JSON_LIMIT, credential_id
from coven_review_commit import ReviewTransaction, INPUT_BYTES, transaction_worker
from coven_staging import read_file, read_json
from witchy_release import digest_of

OPERATION = "enroll-maintainer"
FORMAT = "coven-enrollment-1"


def begin(directory, packages, replace, issue, enrollment):
    scope = sorted(set(packages))
    if replace == "":
        raise ValueError("replacement credential is absent")
    policy = enrollment.policy(directory)
    issued = issue(directory, {"operation": OPERATION, "policy": policy,
                               "packages": scope, "replace": replace or ""})
    return {"format": FORMAT, "registry": policy["registry"], "origin": policy["origin"], "packages": scope,
            "replace": replace or "", "challenge": issued["token"], "expires": issued["expires"],
            "url": policy["origin"] + "/enroll"}


def enrollment_snapshot(directory, token, key, assertion, enrollment):
    snapshot = enrollment.snapshot(directory, token)
    return snapshot | {"token": token, "public_key": key, "assertion": assertion}


def complete(directory, response, commit):
    result = read_json(read_file(response, JSON_LIMIT))
    if (not isinstance(result, dict) or set(result) != {"format", "challenge", "public_key", "assertion"}
            or result["format"] != FORMAT or not isinstance(result["public_key"], str)):
        raise ValueError("invalid maintainer enrollment response")
    assertion = result["assertion"]
    ident = credential_id(assertion)
    request = enrollment_snapshot(directory, result["challenge"], result["public_key"], assertion, commit)
    encoded = commit(directory, request)
    policy = read_json(encoded)
    return {"revision": policy["revision"], "digest": digest_of(encoded), "credential": ident, "policy": policy}


class EnrollmentCommit(ReviewTransaction):
    def __init__(self, module_args, host):
        super().__init__(module_args, host, ("--sqlite-read=policy,challenges,counters",
                         "--sqlite-insert=counters,policy", "--sqlite-delete=policy,challenges,browser_approvals"),
                         output_bytes=JSON_LIMIT + 11, fuel=100000000)
        limits = {"output_bytes": INPUT_BYTES + 11, "fuel": 100000000}
        self.policy_reader = ReviewTransaction(module_args, host, ("--sqlite-read=policy",
                                                f"--sqlite-reply-bytes={INPUT_BYTES}"), **limits)
        self.snapshot_reader = ReviewTransaction(module_args, host, ("--sqlite-read=policy,challenges",
                                                   f"--sqlite-reply-bytes={INPUT_BYTES}"), **limits)

    def policy(self, directory):
        return read_json(self.policy_reader(directory, {"mode": "policy"}))

    def snapshot(self, directory, token):
        result = read_json(self.snapshot_reader(directory, {"mode": "snapshot", "token": token}))
        if not isinstance(result, dict) or result.keys() != {"policy", "envelope"}:
            raise ValueError("invalid maintainer enrollment snapshot")
        return result


def enrollment_worker(build, host, *, source=None, pin=None):
    return transaction_worker(build, host, "review_enroll", "enrollment", EnrollmentCommit, source=source, pin=pin)
