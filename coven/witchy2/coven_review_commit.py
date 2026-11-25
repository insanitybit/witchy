"""Run the pinned Witchy maintainer approval transaction."""
from contextlib import contextmanager
from pathlib import Path
import subprocess

from coven_staging import DATABASE, canonical, database_path, read_file, read_json
from coven_worker import check_worker_pin, run_with_lifeline, snapshot_worker_arguments
from witchy_release import DIGEST, PAYLOAD_BYTES

BUILD = ("--heap-pages=64", "--slots=4096", "--stack-bytes=65536")
MODULE_BYTES = 131072
INPUT_BYTES = 262144
POLICY_BYTES = 65536
APPROVAL_REPLY = INPUT_BYTES + 1
RECEIPT_REPLY = 1310976
APPROVAL_SNAPSHOT_REPLY = 1835264
INSPECTION_HEADER_BYTES = INPUT_BYTES + (16 << 10)
COUNTER_CHANGED = {b"maintainer counter changed during approval\n", b"maintainer counter changed during review\n"}


class CounterChanged(ValueError):
    pass


class PolicyChanged(ValueError):
    pass


class ReviewTransaction:
    def __init__(self, module_args, host, grants, *, memory_pages=100, fuel=60000000, output_bytes=2048,
                 input_bytes=INPUT_BYTES):
        self.command = (host, *module_args, "--sqlite-request-stdin", "--output", "--allocator",
                        f"--module-bytes={MODULE_BYTES}", f"--input-bytes={input_bytes}",
                        f"--output-bytes={output_bytes}", f"--memory-pages={memory_pages}", f"--fuel={fuel}",
                        "--deadline-ms=5000", *grants)

    def __call__(self, directory, request):
        return self.response(self.exchange(directory, request))

    def exchange(self, directory, request):
        data = canonical(request) + b"\n"
        if len(data) > INPUT_BYTES:
            raise ValueError("maintainer assertion refused")
        return self.exchange_bytes(Path(directory).resolve(strict=True) / DATABASE, data)

    def exchange_bytes(self, database, data):
        try:
            result = run_with_lifeline(subprocess.run, (*self.command, f"--sqlite={database}"), input=data,
                                       capture_output=True, timeout=6)
        except (OSError, subprocess.TimeoutExpired):
            raise ValueError("maintainer approval transaction unavailable") from None
        if result.returncode:
            check_worker_pin(result, "review transaction")
            raise ValueError("maintainer approval transaction stopped")
        return result.stdout

    @staticmethod
    def response(output):
        status, _, reply = output.partition(b"\n")
        if status == b"refused" and reply.endswith(b"\n") and reply.count(b"\n") == 1:
            if reply in COUNTER_CHANGED:
                raise CounterChanged(reply[:-1].decode("ascii"))
            if reply == b"release policy changed during installation\n":
                raise PolicyChanged(reply[:-1].decode("ascii"))
            raise ValueError(reply[:-1].decode("ascii"))
        if status == b"committed" and reply.endswith(b"\n") and reply.count(b"\n") == 1:
            return reply[:-1]
        if output in (b"maintainer assertion refused\n", b"invalid release policy\n",
                             b"release policy transaction unavailable\n",
                             b"maintainer enrollment transaction unavailable\n",
                             b"maintainer policy exceeds the size limit\n",
                             b"enrollment requires a fresh credential ID and public key\n",
                             b"release policy revision must increase\n",
                             b"release policy cannot change this store's registry or origin\n",
                             b"no enrolled maintainer for this package\n", b"maintainer enrollment requires HTTPS\n",
                             b"maintainer policy revision is exhausted\n", b"maintainer credential table is full\n",
                             b"replacement credential is absent\n", b"replacement must keep the existing package scope\n",
                             b"invalid maintainer challenge request\n", b"invalid review challenge request\n"):
            raise ValueError(output[:-1].decode("ascii"))
        raise ValueError("maintainer approval transaction unavailable")


class ApprovalCommit(ReviewTransaction):
    def __init__(self, module_args, host):
        super().__init__(module_args, host, (
            "--sqlite-read=policy,challenges,candidates,browser_approvals,counters,approvals",
            "--sqlite-insert=counters,approvals", "--sqlite-delete=counters,challenges,browser_approvals",
            f"--sqlite-reply-bytes={APPROVAL_REPLY}"), output_bytes=INPUT_BYTES + 11)
        self.reader = ReviewTransaction(module_args, host, (
            "--sqlite-read=policy,challenges,candidates,counters",
            f"--sqlite-reply-bytes={APPROVAL_SNAPSHOT_REPLY}"), output_bytes=INPUT_BYTES + 11, fuel=500000000)
        self.policy_reader = ReviewTransaction(module_args, host, (
            "--sqlite-read=policy", "--sqlite-reply-bytes=65537"), output_bytes=INPUT_BYTES + 11, fuel=500000000)
        self.candidate_reader = ReviewTransaction(module_args, host, (
            "--sqlite-read=policy,candidates", f"--sqlite-reply-bytes={RECEIPT_REPLY}"),
            output_bytes=INPUT_BYTES + 11, fuel=500000000)
        self.inspections = {
            present: ReviewTransaction(module_args, host, (
                "--sqlite-read=policy,challenges,candidates,counters" + (",ci_receipts" if present else ""),
                f"--sqlite-reply-bytes={APPROVAL_SNAPSHOT_REPLY}"),
                output_bytes=INSPECTION_HEADER_BYTES + PAYLOAD_BYTES + 10, fuel=700000000)
            for present in (False, True)
        }

    def active_policy(self, directory):
        return read_json(self.policy_reader(directory, {"mode": "challenge-policy"}))

    def staged_release(self, directory, digest):
        wire = self.candidate_reader(directory, {"mode": "staged-release", "digest": digest})
        release = read_json(wire)
        if not isinstance(release, dict) or release.keys() != {"policy", "name", "version", "allowed"}:
            raise ValueError("invalid staged release snapshot")
        return release

    def snapshot(self, directory, token, credential):
        wire = self.reader(directory, {"mode": "snapshot", "token": token, "credential": credential})
        snapshot = read_json(wire)
        if not isinstance(snapshot, dict) or snapshot.keys() != {"policy", "envelope", "previous"}:
            raise ValueError("invalid maintainer approval snapshot")
        return snapshot

    def inspection_snapshot(self, directory, token, credential, has_provenance):
        mode = "inspection-snapshot" if has_provenance else "legacy-inspection-snapshot"
        reader = self.inspections[has_provenance]
        output = reader.exchange(directory, {"mode": mode, "token": token, "credential": credential})
        if not output.startswith(b"snapshot\n"):
            reader.response(output)
            raise ValueError("invalid maintainer inspection snapshot")
        header, separator, payload = output[len(b"snapshot\n"):].partition(b"\n")
        if not separator or len(header) > INSPECTION_HEADER_BYTES or not 0 < len(payload) <= PAYLOAD_BYTES:
            raise ValueError("invalid maintainer inspection snapshot")
        snapshot = read_json(header)
        if not isinstance(snapshot, dict) or snapshot.keys() != {"policy", "envelope", "previous", "name", "version", "provenance"}:
            raise ValueError("invalid maintainer inspection snapshot")
        return snapshot, payload

    def __call__(self, directory, request):
        receipt = archived_receipt(super().__call__(directory, request))
        if receipt["envelope"] != request["envelope"] or receipt["assertion"] != request["assertion"]:
            raise ValueError("invalid maintainer approval transaction result")
        return receipt


class ReceiptRead(ReviewTransaction):
    def __init__(self, module_args, host):
        super().__init__(module_args, host, ("--sqlite-read=candidates,approvals", f"--sqlite-reply-bytes={RECEIPT_REPLY}"),
                         output_bytes=INPUT_BYTES + 11, fuel=400000000)

    def __call__(self, directory, digest):
        return archived_receipt(super().__call__(directory, {"digest": digest}))


def archived_receipt(wire):
    receipt = read_json(wire)
    if not isinstance(receipt, dict) or receipt.keys() != {"approved_at", "assertion", "envelope", "format"}:
        raise ValueError("invalid maintainer approval receipt")
    return receipt


class InspectCommit(ReviewTransaction):
    def __init__(self, module_args, host):
        super().__init__(module_args, host, ("--rand", "--clock",
            "--sqlite-read=policy,challenges,candidates,browser_approvals,counters,approvals",
            "--sqlite-insert=counters,challenges,browser_approvals",
            "--sqlite-delete=counters,challenges,browser_approvals"))

    def __call__(self, directory, request):
        result = read_json(super().__call__(directory, request))
        if not isinstance(result, dict) or result.keys() != {"previous", "token", "envelope"}:
            raise ValueError("invalid maintainer review transaction result")
        return result


class PolicyCommit(ReviewTransaction):
    def __init__(self, module_args, host):
        super().__init__(module_args, host, ("--sqlite-read=policy,counters", "--sqlite-insert=policy",
                                       "--sqlite-delete=policy,challenges,browser_approvals", "--sqlite-reply-bytes=256"),
                         memory_pages=256, fuel=1000000000, input_bytes=2 * POLICY_BYTES + 8)
        self.reader = ReviewTransaction(module_args, host, ("--sqlite-read=policy",
                                        f"--sqlite-reply-bytes={POLICY_BYTES + 2}"),
                                        memory_pages=256, input_bytes=1, output_bytes=POLICY_BYTES + 10)

    def __call__(self, directory, path):
        policy = read_file(path, POLICY_BYTES)
        database = database_path(directory)
        for attempt in range(2):
            saved = self.snapshot(database)
            try:
                return self.install(database, saved, policy)
            except PolicyChanged:
                if attempt:
                    raise

    def snapshot(self, database):
        reply = self.reader.exchange_bytes(database, b"R")
        prefix = b"snapshot\n"
        if not reply.startswith(prefix):
            self.reader.response(reply)
            raise ValueError("invalid maintainer policy snapshot")
        snapshot = reply[len(prefix):]
        if not 0 < len(snapshot) <= POLICY_BYTES + 1:
            raise ValueError("invalid maintainer policy snapshot")
        return snapshot

    def install(self, database, saved, policy):
        header = b"W" + len(saved).to_bytes(3, "big") + len(policy).to_bytes(3, "big")
        reply = read_json(self.response(self.exchange_bytes(database, header + saved + policy)))
        if (not isinstance(reply, dict) or reply.keys() != {"revision", "digest"}
                or type(reply["revision"]) is not int or not isinstance(reply["digest"], str)
                or not DIGEST.fullmatch(reply["digest"])):
            raise ValueError("invalid maintainer policy transaction result")
        return reply


class ChallengeIssue(ReviewTransaction):
    def __init__(self, module_args, host, *, browser):
        readable = "policy,challenges,browser_approvals" if browser else "policy,challenges,browser_approvals,candidates,approvals"
        super().__init__(module_args, host, ("--rand", "--clock", f"--sqlite-read={readable}",
                                       "--sqlite-insert=challenges", "--sqlite-delete=challenges,browser_approvals",
                                       "--sqlite-reply-bytes=2048"),
                         memory_pages=256, fuel=500000000)

    def __call__(self, directory, request):
        result = read_json(super().__call__(directory, request))
        if not isinstance(result, dict) or not isinstance(result.get("token"), str):
            raise ValueError("invalid maintainer challenge result")
        if request["operation"] == "release":
            valid = result.keys() == {"token", "envelope"} and isinstance(result["envelope"], dict)
        else:
            valid = result.keys() == {"token", "nonce", "expires"} and type(result["expires"]) is int
        if not valid:
            raise ValueError("invalid maintainer challenge result")
        return result


@contextmanager
def transaction_worker(build, host, program, label, transaction, options=BUILD, *, source=None, pin=None):
    with snapshot_worker_arguments(build, program, options, MODULE_BYTES, f"review {label}",
                                   source=source, pin=pin) as module_args:
        yield transaction(module_args, host)


def commit_worker(build, host, *, source=None, pin=None):
    return transaction_worker(build, host, "review_commit", "commit", ApprovalCommit, source=source, pin=pin)


def receipt_reader(build, host, *, source=None, pin=None):
    return transaction_worker(build, host, "review_receipt", "receipt", ReceiptRead, source=source, pin=pin)


def built_inspection(build, host):
    return transaction_worker(build, host, "review_inspect", "inspection", InspectCommit)


CHALLENGE_BUILD = ("--heap-pages=128", "--slots=65536", "--stack-bytes=65536")


def policy_worker(build, host, *, source=None, pin=None):
    return transaction_worker(build, host, "review_set_policy", "policy", PolicyCommit, CHALLENGE_BUILD,
                              source=source, pin=pin)


def issuer_worker(build, host, *, browser=False, source=None, pin=None):
    program = "review_read_challenge" if browser else "review_issue_challenge"
    label = "read challenge" if browser else "challenge issuer"
    return transaction_worker(build, host, program, label,
                              lambda module_args, engine: ChallengeIssue(module_args, engine, browser=browser),
                              CHALLENGE_BUILD, source=source, pin=pin)


def pinned_transaction(source, pin, host, label, transaction):
    return transaction_worker(None, host, "review", label, transaction, source=source, pin=pin)


def pinned_commit(source, pin, host):
    return pinned_transaction(source, pin, host, "commit", ApprovalCommit)


def pinned_inspection(source, pin, host):
    return pinned_transaction(source, pin, host, "inspection", InspectCommit)


def pinned_read_challenge(source, pin, host):
    return pinned_transaction(source, pin, host, "read challenge",
                              lambda module_args, engine: ChallengeIssue(module_args, engine, browser=True))
