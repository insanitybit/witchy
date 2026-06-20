# files

Reads a file through the filesystem capability: `main` receives a `Dir`, narrows
it to a subdirectory with `subdir`, and `read`s a file from there. Authority flows
by passing the capability down — there is no ambient filesystem access, so reading
elsewhere would need a `Dir` for that location.

**Shows:** the `Dir` and `Console` capabilities, capability narrowing with
`subdir`, and `read`.

## Run

```sh
witchy run                                # from this directory
witchy examples/files/src/files.witchy    # or by file, from the repo root
```
