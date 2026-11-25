"""Command-line arguments for Grimoire."""

import argparse
from pathlib import Path


def argument_parser(test_options=None):
    cli = argparse.ArgumentParser(prog="grimoire")
    commands = cli.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="build and run a local Witchy 2 executable")
    run.add_argument("project", type=Path, nargs="?", default=Path("."),
                     help="project directory or manifest (default: ./witchy.toml)")
    run.add_argument("--module-pin", metavar="SHA384", help="run only if the rebuilt Wasm matches this sha384: digest")
    run.add_argument("--tuf-snapshot", nargs=2, metavar=("VERSION", "SHA384"),
                     help="require this authenticated TUF snapshot for the dependency graph")
    run.add_argument("--launch-pins", nargs=2, metavar=("MANIFEST_SHA384", "GRANTS_SHA384"),
                     help="require approved digests for the project manifest and grant document")
    run.add_argument("--record", type=Path, help="write what was built, and from what, by digest, as JSON")
    run.add_argument("--raw-output", action="store_true",
                     help="pass the program's output on byte for byte (for a tool that reads it); by default control characters are written out as text")
    run.add_argument("--allow-write-root", action="append", default=[], metavar="ABSOLUTE_PATH",
                     help="for this launch, let the grant document's writable directory be this one, outside the project")
    run.add_argument("--allow-read-root", action="append", default=[], metavar="ABSOLUTE_PATH",
                     help="for this launch, let the grant document's read-only directory be this one, outside the project")
    run.add_argument("--allow-read-file", action="append", default=[], metavar="ABSOLUTE_PATH",
                     help="for this launch, let the grant document's file or input be this one, outside the project")
    run.add_argument("--bind", action="append", default=[], metavar="NAME=SOURCE",
                     help="read secret slot NAME from SOURCE, one of its declared from/also_from sources")
    run.add_argument("--quiet", action="store_true",
                     help="one line in place of the grant: and limit: lines when `grimoire review --accept` recorded exactly these on this machine; all of them otherwise")
    build = commands.add_parser("build", help="build the locked executable or checked library into target/")
    build.add_argument("project", type=Path, nargs="?", default=Path("."))
    build.add_argument("-o", "--output", type=Path)
    web = commands.add_parser("web", help="build the read-only Coven Web catalog")
    web_commands = web.add_subparsers(dest="web_action", required=True)
    web_build = web_commands.add_parser("build", help="build a checked Glamour generation")
    web_build.add_argument("project", type=Path, nargs="?", default=Path("."))
    test = commands.add_parser("test", help="run the project's declared executable tests")
    test.add_argument("project", type=Path, nargs="?", default=Path("."))
    if test_options is not None:
        test_options(test)
    keygen = commands.add_parser("keygen", help="create a publisher seed and its public key locally")
    keygen.add_argument("key", type=Path)
    publish = commands.add_parser("publish", help="sign and publish a library to its Coven URL")
    publish.add_argument("project", type=Path, nargs="?", default=Path("."))
    publish.add_argument("--key", type=Path, required=True)
    publish.add_argument("--recovery-key", type=Path)
    lock = commands.add_parser("lock", help="pin each registry dependency's content digest in witchy.lock")
    lock.add_argument("project", type=Path, nargs="?", default=Path("."),
                      help="project directory or manifest (default: ./witchy.toml)")
    lock.add_argument("--toolchain", action="store_true", help="pin the compiler and host in use by digest")
    lock.add_argument("--trust-first-use", action="store_true",
                      help="trust the owner keys, and the operator key, the registry names now where nothing has anchored them (prints them as [owners] and [registry] lines)")
    lock.add_argument("--recover-tuf-snapshot", metavar="SHA384",
                      help="verify an independently provisioned replacement TUF snapshot and preserve the locked dependency graph")
    review = commands.add_parser("review", help="show what each locked file uses, against the policy")
    review.add_argument("project", type=Path, nargs="?", default=Path("."),
                        help="project directory or manifest (default: ./witchy.toml)")
    review.add_argument("--accept", action="store_true",
                        help="pin the policy's digests to the reviewed code (no capability changes)")
    review.add_argument("--write-policy", action="store_true",
                        help="write the drafted policy beside the entry when there is none, or over the one `grimoire new` wrote")
    review.add_argument("--replace", action="store_true",
                        help="with --write-policy: draft the policy again over an edited one")
    new = commands.add_parser("new", help="create a project that builds and passes its test")
    new.add_argument("name")
    kind = new.add_mutually_exclusive_group()
    kind.add_argument("--lib", action="store_true", help="a library package with a native test")
    kind.add_argument("--exe", action="store_true", help="an executable with its grant document and policy (the default)")
    kind.add_argument("--service", action="store_true", help="a service run once for each connection, its state in a granted directory")
    kind.add_argument("--tool", action="store_true", help="a tool in a pipe: its arguments and standard input")
    trust = commands.add_parser("trust", help="show this machine's trust store for the project's registry")
    trust.add_argument("project", type=Path, nargs="?", default=Path("."),
                       help="project directory or manifest (default: ./witchy.toml)")
    trust.add_argument("--forget", metavar="NAME", help="forget a package's owners and conflict (floor and revocations kept)")
    trust.add_argument("--forget-operator", action="store_true", help="forget the registry's operator (each key's snapshot floor and the retired keys kept)")
    trust.add_argument("--reestablish-operator", metavar="KEY", help="accept an operator key a record without provenance named, for every project (check it with the registry's operator first); it still signs nothing if that record saw it hand the role on")
    trust.add_argument("--reinstate-signer", metavar="KEY", help="let an established operator key that a record without provenance saw hand the role on sign again (check with the registry's operator that it is still the current signing key)")
    trust.add_argument("--forget-unkeyed-floor", action="store_true", help="drop the snapshot floor kept from an operator record forgotten before floors were per key (it binds every key)")
    return cli
