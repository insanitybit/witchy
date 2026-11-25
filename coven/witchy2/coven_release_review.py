"""Read staged bytes only after a fresh, scoped maintainer assertion."""
import difflib
from urllib.parse import urlsplit

import coven_approval as approval
from coven_review_commit import CounterChanged
from witchy_release import parse_payload


def review_challenge(directory, digest, issue, reader):
    approval.require_digest(digest)
    policy = reader.active_policy(directory)
    issued = issue(directory, {"operation": "inspect", "policy": policy, "digest": digest})
    return {"publicKey": {"challenge": issued["token"], "rpId": urlsplit(policy["origin"]).hostname,
                          "timeout": approval.VALID_SECONDS * 1000, "userVerification": "required"}}


def inspect_snapshot(directory, token, assertion, reader, has_provenance):
    snapshot, payload = reader.inspection_snapshot(directory, token, approval.credential_id(assertion), has_provenance)
    provenance = snapshot.pop("provenance")
    release = parse_payload(payload, snapshot["name"], snapshot["version"])
    return release, provenance, snapshot | {"token": token, "assertion": assertion}


def inspect(directory, token, assertion, commit, reader, has_provenance):
    for attempt in range(2):
        release, provenance, request = inspect_snapshot(directory, token, assertion, reader, has_provenance)
        try:
            result = commit(directory, request)
        except CounterChanged:
            if attempt:
                raise
        else:
            pending = None
            previous = result["previous"]
            if previous is None:
                pending = approval.challenge_reply(request["policy"], result["token"], result["envelope"],
                                                    [approval.credential_id(assertion)])
            return request["policy"]["registry"], release, provenance, pending, (previous,) if previous else None


def display_lines(data):
    text = data.decode("utf-8", "surrogateescape")
    # Escape literal backslashes so a control cannot look like unchanged source.
    display = []
    for char in text:
        if char == "\\":
            display.append("\\\\")
        elif char.isprintable() or char in "\n\t":
            display.append(char)
        else:
            display.append(f"\\u{{{ord(char):x}}}")
    lines = "".join(display).splitlines(keepends=True)
    if lines and not lines[-1].endswith("\n"):
        lines[-1] += "\n\\ No newline at end of file\n"
    return lines


def differences(previous, release):
    before, after = dict(previous.files) if previous else {}, dict(release.files)
    hunks = []
    for file in sorted(before.keys() | after.keys()):
        old, new = before.get(file), after.get(file)
        if old == new:
            continue
        first, second = display_lines(old or b""), display_lines(new or b"")
        if len(first) + len(second) <= 256:
            diff = "".join(difflib.unified_diff(first, second, f"a/{file}", f"b/{file}"))
        else:
            diff = (f"--- a/{file}\n+++ b/{file}\n" + "".join("-" + line for line in first)
                    + "".join("+" + line for line in second))
        state = "added" if old is None else "removed" if new is None else "changed"
        hunks.append({"file": file, "state": state, "diff": diff})
    return {"previous": previous.version if previous else None,
            "previous_digest": previous.digest if previous else None,
            "entry": release.entry, "requires": release.requires, "files": hunks}
