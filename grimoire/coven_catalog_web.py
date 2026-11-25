"""The first-party Coven application for Glamour's checked web build."""
from html import escape
import re
import subprocess
import tempfile

NAME = re.compile(r"[a-z][a-z0-9-]{0,63}")
VERSION = re.compile(r"(?:0|[1-9][0-9]{0,8})(?:\.(?:0|[1-9][0-9]{0,8})){2}")
DIGEST = re.compile(r"sha384:[0-9a-f]{96}")


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def catalog(grimoire, location, pin, *, ca_pin=None):
    if isinstance(location, grimoire.TufRegistry):
        command = [grimoire.tool("COVEN", "coven"), "tuf", "catalog", *location.mirror_arguments(ca_pin=ca_pin)]
        timeout = min(60, location.resolution_deadline_ms / 1000)
        value = read_catalog(grimoire, command, timeout)
        if location.url.startswith("https://"):
            effective = value.pop("ca_pin", None) if isinstance(value, dict) else None
            expected = ca_pin or location.ca_pin
            if not matches(DIGEST, effective) or expected is not None and effective != expected:
                raise ValueError("verified catalog HTTPS CA bundle is missing or differs from its pin")
        else:
            effective = None
        validate_catalog(value, location.registry_id, tuf=True)
        return value, effective

    # Use the same trust store as dependency resolution, including its
    # remembered operator handovers, and bind the catalog to those exact bytes.
    with tempfile.TemporaryDirectory(prefix="grimoire-catalog-") as folder:
        sequence, signer, snapshot, registry = grimoire.verified_legacy_snapshot(location, pin, folder)
        digest = grimoire.file_digest(snapshot)
    command = [grimoire.tool("COVEN", "coven"), "catalog", "--registry", location,
               "--operator", signer, "--registry-id", registry, "--min-snapshot", str(sequence)]
    value = read_catalog(grimoire, command, 20)
    validate_catalog(value, registry, tuf=False)
    if value["sequence"] != sequence or value["snapshot"] != digest:
        raise ValueError("invalid verified catalog result")
    return value, None


def read_catalog(grimoire, command, timeout):
    try:
        result = grimoire.run_coven(command, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise ValueError("verified catalog exceeded the build deadline") from error
    grimoire.relay(result.stderr)
    result.check_returncode()
    return grimoire.read_coven_receipt(result.stdout)


def validate_catalog(value, registry, *, tuf):
    fields = {"registry", "snapshot", "releases"}
    fields |= {"snapshot_version", "timestamp"} if tuf else {"sequence"}
    if (not isinstance(value, dict) or set(value) != fields or value["registry"] != registry
            or not matches(DIGEST, value["snapshot"])
            or not isinstance(value["releases"], list) or len(value["releases"]) > 64
            or not tuf and (type(value["sequence"]) is not int or value["sequence"] < 1)):
        raise ValueError("invalid verified catalog result or more than 64 releases for the web catalog")
    if tuf and (type(value["snapshot_version"]) is not int or not 1 <= value["snapshot_version"] < 2**53
                or not matches(DIGEST, value["timestamp"])):
        raise ValueError("invalid verified catalog publication binding")
    release_fields = {"name", "version", "target", "length"} if tuf else {"name", "version", "digest"}
    digest_field = "target" if tuf else "digest"
    coordinates = set()
    for release in value["releases"]:
        if (not isinstance(release, dict) or set(release) != release_fields
                or not matches(NAME, release["name"]) or not matches(VERSION, release["version"])
                or not matches(DIGEST, release[digest_field])
                or tuf and (type(release["length"]) is not int or not 1 <= release["length"] <= (2 << 20))):
            raise ValueError("invalid verified release")
        key = release["name"], release["version"]
        if key in coordinates:
            raise ValueError("duplicate verified release")
        coordinates.add(key)


def render_pages(execute, released):
    """Check the guest's canonical bytes and render the verified model."""
    releases = released["releases"]
    tuf = "sequence" not in released
    snapshot_label = (f'Verified TUF snapshot <code>{escape(released["snapshot"])}</code>' if tuf
                      else f'Verified snapshot {released["sequence"]}')
    catalog_fields = ["Package", "Version"]
    rows = [("T", *catalog_fields)]
    for release in releases:
        name, version = release["name"], release["version"]
        path = f"/packages/{name}/{version}/"
        catalog_fields.extend((name, version, path))
        rows.append(("L", name, version, path))
    rows.append(("E",))
    if execute("C", catalog_fields, 32768) != glamour_document.encode(rows):
        raise ValueError("renderer did not emit the expected catalog table")
    index = ('<header class="page-intro"><h1>Package registry</h1>'
             f'<p class="snapshot">{snapshot_label} of '
             f'<code>{escape(released["registry"])}</code></p></header>'
             '<section class="catalog-panel" aria-label="Published packages">'
             '<div class="filter-bar">'
             '<label for="catalog-filter">Filter packages</label>\n'
             '<input type="search" id="catalog-filter" maxlength="64" disabled '
             'placeholder="Search by name" '
             'data-glamour-filter="catalog" aria-describedby="filter-status">\n'
             '<p id="filter-status" role="status" hidden></p></div>'
             f'<div id="catalog">{glamour_document.render(rows)}</div></section>')
    yield "index.html", "Coven", index
    for release in releases:
        name, version = release["name"], release["version"]
        label, digest = ("Sealed target digest", release["target"]) if tuf else ("Release digest", release["digest"])
        fields = (name, "Version", version, label, digest, "Back to Coven", "/")
        expected = (("H", name), ("D", "Version", version), ("D", label, digest),
                    ("A", "Back to Coven", "/"))
        if execute("D", fields, 8192) != glamour_document.encode(expected):
            raise ValueError("renderer did not emit the expected package detail")
        yield f"packages/{name}/{version}/index.html", f"{name} {version} · Coven", glamour_document.render(expected)
