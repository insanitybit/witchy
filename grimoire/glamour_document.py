"""Encode the first-party catalog's document and serialize its HTML."""
from html import escape


def encode(document):
    """Canonical GDL1 bytes for the catalog's expected document."""
    wire = bytearray(b"GDL1\n")
    for opcode, *values in document:
        wire.extend(opcode.encode("ascii"))
        for value in values:
            data = value.encode("utf-8")
            wire.extend(str(len(data)).encode("ascii"))
            wire.append(10)
            wire.extend(data)
    wire.extend(b"Z")
    return bytes(wire)


def render(document):
    """Serialize the catalog's own document into the closed set of HTML elements."""
    fragments = []
    for opcode, *values in document:
        fields = tuple(escape(value) for value in values)
        match opcode:
            case "H":
                fragments.append(f"<h1>{fields[0]}</h1>")
            case "D":
                fragments.append(f"<dl><dt>{fields[0]}</dt><dd>{fields[1]}</dd></dl>")
            case "A":
                fragments.append(f'<a href="{fields[1]}">{fields[0]}</a>')
            case "T":
                fragments.append(f'<table><thead><tr><th scope="col">{fields[0]}</th>'
                                 f'<th scope="col">{fields[1]}</th></tr></thead><tbody>')
            case "L":
                fragments.append(f'<tr><td><a href="{fields[2]}">{fields[0]}</a></td><td>{fields[1]}</td></tr>')
            case "E":
                fragments.append("</tbody></table>")
            case _:
                raise ValueError("invalid Glamour render operation")
    return "".join(fragments)
