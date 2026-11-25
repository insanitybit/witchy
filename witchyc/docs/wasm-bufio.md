# Buffered binary I/O

`import bufio` provides a byte buffer backed by an initialized `Vec<U8>`. Move
a vector into `bufio.byte_buffer`. Its length fixes the buffer capacity. The
buffer holds no I/O capability and never allocates while transferring bytes.

- `bufio_fs.read_file`, `write_file`, and `flush_file` take a file handle with
  the rights they need. [The file-copy fixture](../tests/fixtures/bufio/files.wy)
  shows a complete transfer between explicitly granted files.
- `bufio_net.read_socket`, `write_socket`, and `flush_socket` take a connected
  socket. Each direction uses a separate buffer. Use a dedicated buffer for
  each stream so prefetched or pending bytes stay with the right handle.

A read returns the number of bytes copied into the caller's slice. A nonempty
read returning zero means EOF. With no bytes buffered, a sufficiently large
read goes straight to the handle. Zero-capacity buffers use direct I/O for
both reads and writes.

A write reports how much of its input it accepted. Repeat with the remaining
prefix when necessary, and flush after the final write. If a flush delivers a
prefix and then fails, the buffer retains only the unsent suffix. Socket and
file operations report their host status as `SourceFailed` or `SinkFailed` when
the host supplies one. `NoProgress` means the sink accepted zero bytes during
a nonempty write.

To build another adapter, fill `writable()` and `commit(count)`, or send
`readable()` and `consume(count)`. These methods borrow the storage in place.
`append` and `drain` copy across a caller's slice. `ByteBuffer.close` releases
storage and discards pending bytes. Close the corresponding handle separately,
and finish a file write only after flushing.

On macOS ARM64, native executables can use the file adapters with
`--file=PATH` and `--write-file=PATH`. For the file-copy fixture, pass
both grants along with `--allocator` for the byte buffers and `--output`
for its success message. Flush before calling `finish()`. The broker
atomically installs the staged file on success. `abandon()` discards the
staged bytes. The buffer itself works on native Linux ARM64, where the
file and socket grants still require Wasm.
