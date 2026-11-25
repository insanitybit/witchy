# Glamour package for Coven Web

The `glamour/` directory is the Witchy `glamour 0.3.0` package used by
the Coven catalog example. Its manifest and source can be packed and
published through the usual Coven approval and TUF release workflow.
The package owns its sealed document and local-link types. The browser
filter uses its byte-oriented package-name matching.

The catalog builder encodes the expected GDL1 document from the verified
publication and checks the Witchy renderer's complete output byte for byte.
It serializes the page from that verified publication.
The browser worker lives inside an opaque iframe and receives a bounded
filter event. It returns visibility bits.
See [Coven Web](../../../coven/witchy2/web/README.md) for the build, install,
and browser checks.
