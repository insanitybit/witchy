# SEC-087: Public import-contract authentication allowed authority minting

## Confirmed bug

`SafeImportContract::authenticate` was public and self-hashed a caller-supplied
import name and signature. Lowering code could therefore mint a purported
host-import authority without checked-source authorization or host
authentication. The hash established contract identity, not authority.

**Reachability:** reachable to lowering code and any Rust caller able to use
the public WIR API; ordinary Witchy source could not directly invoke the Rust
constructor. **Severity:** High, because forged contracts could cross the
host-import authority boundary.

## Remediation

Fixed on `impl/rfc0162-0163-unified` by `c977881ff`: the constructor is sealed
as `pub(crate)`, so external lowering crates cannot mint native import
contracts. An authenticated checked-source adapter remains required to create
production authority.

