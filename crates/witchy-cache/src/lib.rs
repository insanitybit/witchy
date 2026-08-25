//! Authenticated, bounded executable-cache records.
//!
//! The cache is an optimization only. Credential or storage failures are
//! represented as misses so callers can compile from current trusted inputs.

#![deny(unsafe_code)]

use std::fmt;
use std::fs;
use std::hash::Hasher;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const MAGIC: &[u8; 16] = b"WITCHY-CACHE-V1\0";
const SCHEMA: u16 = 1;
const ALGORITHM_BLAKE3_KEYED: u8 = 1;
const KEY_EPOCH: u32 = 1;
const DIGEST_BYTES: usize = 32;
const HEADER_BYTES: usize = 128;
const TAG_BYTES: usize = 32;
const ENVELOPE_BYTES: usize = HEADER_BYTES + TAG_BYTES;
const MAC_DOMAIN: &[u8] = b"witchy executable cache record mac v1\0";
const MAC_INPUT_BYTES: usize = MAC_DOMAIN.len() + HEADER_BYTES;
const AUTHENTICATOR_PROBE: &[u8] = b"witchy executable cache availability probe v1\0";
const LOOKUP_DOMAIN: &[u8] = b"witchy executable cache lookup v1\0";
const KEYRING_SERVICE: &str = "dev.witchy.executable-cache";
const KEYRING_ACCOUNT: &str = "epoch-1";

/// Distinct authentication domains for executable-producing caches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CacheKind {
    SourceWasm = 1,
    EmbeddedWasm = 2,
    OptimizedWasm = 3,
    NativeArtifact = 4,
    StdlibAst = 5,
    BuildResult = 6,
}

impl TryFrom<u8> for CacheKind {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::SourceWasm),
            2 => Ok(Self::EmbeddedWasm),
            3 => Ok(Self::OptimizedWasm),
            4 => Ok(Self::NativeArtifact),
            5 => Ok(Self::StdlibAst),
            6 => Ok(Self::BuildResult),
            _ => Err(()),
        }
    }
}

/// A BLAKE3 digest of frozen canonical bytes.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Digest([u8; DIGEST_BYTES]);

impl Digest {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Digest(")?;
        for byte in &self.0[..4] {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str("...)")
    }
}

/// Streaming construction of a digest without materializing a canonical AST or
/// recipe rendering in memory.
#[derive(Clone)]
pub struct DigestBuilder(blake3::Hasher);

impl DigestBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    #[must_use]
    pub fn finish(self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

impl Default for DigestBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for DigestBuilder {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.update(text.as_bytes());
        Ok(())
    }
}

impl io::Write for DigestBuilder {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// BLAKE3 sink for trusted `Hash` implementations such as Wasmtime's engine
/// compatibility identity.
pub struct DigestHasher(DigestBuilder);

impl DigestHasher {
    #[must_use]
    pub fn new() -> Self {
        Self(DigestBuilder::new())
    }

    #[must_use]
    pub fn finish_digest(self) -> Digest {
        self.0.finish()
    }
}

impl Default for DigestHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for DigestHasher {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finish();
        u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("digest prefix length"))
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn write_u8(&mut self, value: u8) {
        self.write(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn write_u128(&mut self, value: u128) {
        self.write(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write(&(value as u64).to_le_bytes());
    }

    fn write_i8(&mut self, value: i8) {
        self.write(&value.to_le_bytes());
    }

    fn write_i16(&mut self, value: i16) {
        self.write(&value.to_le_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    fn write_i128(&mut self, value: i128) {
        self.write(&value.to_le_bytes());
    }

    fn write_isize(&mut self, value: isize) {
        self.write(&(value as i64).to_le_bytes());
    }
}

/// Cache identity derived only from current trusted input and compilation
/// recipe. It intentionally carries no capability grant or program identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheContext {
    kind: CacheKind,
    epoch: u32,
    input: Digest,
    recipe: Digest,
}

impl CacheContext {
    #[must_use]
    pub fn new(kind: CacheKind, frozen_input: &[u8], canonical_recipe: &[u8]) -> Self {
        Self::from_digests(kind, Digest::of(frozen_input), Digest::of(canonical_recipe))
    }

    #[must_use]
    pub const fn from_digests(kind: CacheKind, input: Digest, recipe: Digest) -> Self {
        Self {
            kind,
            epoch: KEY_EPOCH,
            input,
            recipe,
        }
    }

    #[must_use]
    pub fn file_name(self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(LOOKUP_DOMAIN);
        hasher.update(&[self.kind as u8]);
        hasher.update(&self.epoch.to_le_bytes());
        hasher.update(self.input.as_bytes());
        hasher.update(self.recipe.as_bytes());
        format!("{}.wc1", hasher.finalize().to_hex())
    }

    #[cfg(test)]
    const fn with_epoch(mut self, epoch: u32) -> Self {
        self.epoch = epoch;
        self
    }
}

/// The platform cannot provide a non-interactive cache authenticator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticationUnavailable;

/// Host-only MAC operation. No implementation is exposed to the guest ABI.
pub trait Authenticator: Send + Sync {
    fn tag(&self, canonical_record: &[u8]) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable>;
}

/// Process-global platform credential provider.
pub struct PlatformAuthenticator {
    service: &'static str,
    account: &'static str,
    key: OnceLock<Result<Zeroizing<[u8; DIGEST_BYTES]>, AuthenticationUnavailable>>,
}

impl PlatformAuthenticator {
    pub const fn new() -> Self {
        Self {
            service: KEYRING_SERVICE,
            account: KEYRING_ACCOUNT,
            key: OnceLock::new(),
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub const fn isolated_for_test(account: &'static str) -> Self {
        Self {
            service: "dev.witchy.executable-cache.test",
            account,
            key: OnceLock::new(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn delete_isolated_test_credential(&self) -> Result<(), AuthenticationUnavailable> {
        with_non_interactive_credentials(|| {
            let entry = keyring::Entry::new(self.service, self.account)
                .map_err(|_| AuthenticationUnavailable)?;
            entry
                .delete_credential()
                .map_err(|_| AuthenticationUnavailable)
        })
    }
}

impl Default for PlatformAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PlatformAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformAuthenticator")
            .field("initialized", &self.key.get().is_some())
            .finish_non_exhaustive()
    }
}

impl Authenticator for PlatformAuthenticator {
    fn tag(&self, canonical_record: &[u8]) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
        let key = self
            .key
            .get_or_init(|| load_or_create_platform_key(self.service, self.account));
        let key = key.as_ref().map_err(|error| *error)?;
        Ok(keyed_tag(key, canonical_record))
    }
}

static PLATFORM_AUTHENTICATOR: PlatformAuthenticator = PlatformAuthenticator::new();

#[must_use]
pub fn platform_authenticator() -> &'static PlatformAuthenticator {
    &PLATFORM_AUTHENTICATOR
}

/// Deterministic providers for downstream adversarial tests. This module is
/// absent from production builds unless the explicit test feature is enabled.
#[cfg(feature = "test-support")]
pub mod testing {
    use super::*;

    pub struct FixedAuthenticator(Zeroizing<[u8; DIGEST_BYTES]>);

    impl FixedAuthenticator {
        #[must_use]
        pub fn new(key: [u8; DIGEST_BYTES]) -> Self {
            Self(Zeroizing::new(key))
        }
    }

    impl fmt::Debug for FixedAuthenticator {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("FixedAuthenticator([REDACTED])")
        }
    }

    impl Authenticator for FixedAuthenticator {
        fn tag(
            &self,
            canonical_record: &[u8],
        ) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
            Ok(keyed_tag(&self.0, canonical_record))
        }
    }

    #[derive(Debug)]
    pub struct UnavailableAuthenticator;

    impl Authenticator for UnavailableAuthenticator {
        fn tag(
            &self,
            _: &[u8],
        ) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
            Err(AuthenticationUnavailable)
        }
    }
}

/// Why an existing entry was not accepted. All reasons are safe cache misses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissReason {
    NotFound,
    Io,
    TooLarge,
    Malformed,
    Context,
    Authentication,
    ArtifactDigest,
}

/// Payload bytes whose complete record was authenticated for the current
/// context. Construction is private so trust-requiring consumers can make the
/// authentication ordering part of their type signature.
#[derive(Eq, PartialEq)]
pub struct AuthenticatedPayload {
    kind: CacheKind,
    bytes: Vec<u8>,
}

impl AuthenticatedPayload {
    #[must_use]
    pub const fn kind(&self) -> CacheKind {
        self.kind
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Debug for AuthenticatedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedPayload")
            .field("kind", &self.kind)
            .field("length", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Lookup {
    Hit(AuthenticatedPayload),
    Miss(MissReason),
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOutcome {
    Stored,
    Unavailable,
    Io,
}

/// Loads one authenticated single-file record from the exact handle that was
/// bounded and read. Credential unavailability is determined before any cache
/// path is accessed.
#[must_use]
pub fn load(
    directory: &Path,
    context: CacheContext,
    maximum_payload_bytes: usize,
    authenticator: &dyn Authenticator,
) -> Lookup {
    if authenticator.tag(AUTHENTICATOR_PROBE).is_err() {
        return Lookup::Unavailable;
    }

    let directory = match Dir::open_ambient_dir(directory, ambient_authority()) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Lookup::Miss(MissReason::NotFound);
        }
        Err(_) => return Lookup::Miss(MissReason::Io),
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match directory.open_with(context.file_name(), &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Lookup::Miss(MissReason::NotFound);
        }
        Err(_) => return Lookup::Miss(MissReason::Io),
    };

    let maximum_envelope_bytes = match maximum_payload_bytes.checked_add(ENVELOPE_BYTES) {
        Some(maximum) => maximum,
        None => return Lookup::Miss(MissReason::TooLarge),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return Lookup::Miss(MissReason::Io),
    };
    if !metadata.is_file() {
        return Lookup::Miss(MissReason::Io);
    }
    let metadata_length = metadata.len();
    if metadata_length > maximum_envelope_bytes as u64 {
        return Lookup::Miss(MissReason::TooLarge);
    }

    let mut envelope = [0_u8; ENVELOPE_BYTES];
    if file.read_exact(&mut envelope).is_err() {
        return Lookup::Miss(MissReason::Malformed);
    }
    let parsed = match parse_header(&envelope, context, maximum_payload_bytes, authenticator) {
        Ok(parsed) => parsed,
        Err(result) => return result,
    };
    let expected_file_bytes = match parsed.payload_length.checked_add(ENVELOPE_BYTES) {
        Some(expected) => expected,
        None => return Lookup::Miss(MissReason::TooLarge),
    };
    if metadata_length != expected_file_bytes as u64 {
        return Lookup::Miss(MissReason::TooLarge);
    }

    let mut payload = vec![0_u8; parsed.payload_length];
    if file.read_exact(&mut payload).is_err() {
        return Lookup::Miss(MissReason::Io);
    }
    let mut trailing = [0_u8; 1];
    match file.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Lookup::Miss(MissReason::TooLarge),
        Err(_) => return Lookup::Miss(MissReason::Io),
    }
    if Digest::of(&payload) != parsed.artifact {
        return Lookup::Miss(MissReason::ArtifactDigest);
    }

    Lookup::Hit(AuthenticatedPayload {
        kind: parsed.kind,
        bytes: payload,
    })
}

/// Authenticates before creating or opening anything in the cache directory,
/// then publishes one complete envelope by rename.
#[must_use]
pub fn store(
    directory: &Path,
    context: CacheContext,
    payload: &[u8],
    maximum_payload_bytes: usize,
    authenticator: &dyn Authenticator,
) -> StoreOutcome {
    if payload.len() > maximum_payload_bytes {
        return StoreOutcome::Io;
    }
    if authenticator.tag(AUTHENTICATOR_PROBE).is_err() {
        return StoreOutcome::Unavailable;
    }

    let header = encode_header(context, Digest::of(payload), payload.len());
    let tag = match record_tag(authenticator, &header) {
        Ok(tag) => tag,
        Err(_) => return StoreOutcome::Unavailable,
    };

    if fs::create_dir_all(directory).is_err() {
        return StoreOutcome::Io;
    }

    let directory = match Dir::open_ambient_dir(directory, ambient_authority()) {
        Ok(directory) => directory,
        Err(_) => return StoreOutcome::Io,
    };
    let destination = context.file_name();
    let (temporary_name, mut temporary) = match create_temporary(&directory, &destination) {
        Ok(result) => result,
        Err(_) => return StoreOutcome::Io,
    };
    let write_result = temporary
        .write_all(&header)
        .and_then(|()| temporary.write_all(&tag))
        .and_then(|()| temporary.write_all(payload))
        .and_then(|()| temporary.flush());
    drop(temporary);
    if write_result.is_err() {
        let _ = directory.remove_file(&temporary_name);
        return StoreOutcome::Io;
    }

    if directory
        .rename(&temporary_name, &directory, &destination)
        .is_err()
    {
        // Windows does not replace an existing destination. Removing an
        // attacker-controlled entry can race, but authentication keeps every
        // outcome safe; the worst result is a cache miss.
        let _ = directory.remove_file(&destination);
        if directory
            .rename(&temporary_name, &directory, &destination)
            .is_err()
        {
            let _ = directory.remove_file(&temporary_name);
            return StoreOutcome::Io;
        }
    }
    StoreOutcome::Stored
}

#[cfg(test)]
fn decode(
    bytes: &[u8],
    expected: CacheContext,
    maximum_payload_bytes: usize,
    authenticator: &dyn Authenticator,
) -> Lookup {
    let Some(envelope) = bytes.get(..ENVELOPE_BYTES) else {
        return Lookup::Miss(MissReason::Malformed);
    };
    let parsed = match parse_header(envelope, expected, maximum_payload_bytes, authenticator) {
        Ok(parsed) => parsed,
        Err(result) => return result,
    };
    if parsed.payload_length.checked_add(ENVELOPE_BYTES) != Some(bytes.len()) {
        return Lookup::Miss(MissReason::TooLarge);
    }
    let payload = &bytes[ENVELOPE_BYTES..];
    if Digest::of(payload) != parsed.artifact {
        return Lookup::Miss(MissReason::ArtifactDigest);
    }
    Lookup::Hit(AuthenticatedPayload {
        kind: parsed.kind,
        bytes: payload.to_vec(),
    })
}

struct ParsedHeader {
    kind: CacheKind,
    artifact: Digest,
    payload_length: usize,
}

fn parse_header(
    envelope: &[u8],
    expected: CacheContext,
    maximum_payload_bytes: usize,
    authenticator: &dyn Authenticator,
) -> Result<ParsedHeader, Lookup> {
    if envelope.len() != ENVELOPE_BYTES || &envelope[..MAGIC.len()] != MAGIC {
        return Err(Lookup::Miss(MissReason::Malformed));
    }
    if read_u16(envelope, 16) != Some(SCHEMA)
        || envelope.get(18).copied() != Some(ALGORITHM_BLAKE3_KEYED)
    {
        return Err(Lookup::Miss(MissReason::Malformed));
    }
    let Some(kind) = envelope
        .get(19)
        .copied()
        .and_then(|kind| CacheKind::try_from(kind).ok())
    else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(epoch) = read_u32(envelope, 20) else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(input) = read_digest(envelope, 24) else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(artifact) = read_digest(envelope, 56) else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(recipe) = read_digest(envelope, 88) else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(payload_length) = read_u64(envelope, 120)
        .and_then(|length| usize::try_from(length).ok())
    else {
        return Err(Lookup::Miss(MissReason::TooLarge));
    };

    if kind != expected.kind
        || epoch != expected.epoch
        || input != expected.input
        || recipe != expected.recipe
    {
        return Err(Lookup::Miss(MissReason::Context));
    }
    if payload_length > maximum_payload_bytes {
        return Err(Lookup::Miss(MissReason::TooLarge));
    }

    let Some(stored_tag) = envelope.get(HEADER_BYTES..ENVELOPE_BYTES) else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let Some(header) = envelope
        .get(..HEADER_BYTES)
        .and_then(|header| header.try_into().ok())
    else {
        return Err(Lookup::Miss(MissReason::Malformed));
    };
    let expected_tag = match record_tag(authenticator, header) {
        Ok(tag) => tag,
        Err(_) => return Err(Lookup::Unavailable),
    };
    if !bool::from(expected_tag.ct_eq(stored_tag)) {
        return Err(Lookup::Miss(MissReason::Authentication));
    }

    Ok(ParsedHeader {
        kind,
        artifact,
        payload_length,
    })
}

fn encode_header(context: CacheContext, artifact: Digest, payload_length: usize) -> [u8; HEADER_BYTES] {
    let mut header = [0_u8; HEADER_BYTES];
    header[..16].copy_from_slice(MAGIC);
    header[16..18].copy_from_slice(&SCHEMA.to_le_bytes());
    header[18] = ALGORITHM_BLAKE3_KEYED;
    header[19] = context.kind as u8;
    header[20..24].copy_from_slice(&context.epoch.to_le_bytes());
    header[24..56].copy_from_slice(context.input.as_bytes());
    header[56..88].copy_from_slice(artifact.as_bytes());
    header[88..120].copy_from_slice(context.recipe.as_bytes());
    header[120..128].copy_from_slice(&(payload_length as u64).to_le_bytes());
    header
}

fn record_tag(
    authenticator: &dyn Authenticator,
    header: &[u8; HEADER_BYTES],
) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
    let mut canonical = [0_u8; MAC_INPUT_BYTES];
    canonical[..MAC_DOMAIN.len()].copy_from_slice(MAC_DOMAIN);
    canonical[MAC_DOMAIN.len()..].copy_from_slice(header);
    authenticator.tag(&canonical)
}

fn keyed_tag(key: &[u8; DIGEST_BYTES], message: &[u8]) -> [u8; TAG_BYTES] {
    *blake3::keyed_hash(key, message).as_bytes()
}

fn create_temporary(directory: &Dir, destination_name: &str) -> io::Result<(String, cap_std::fs::File)> {
    for _ in 0..16 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = format!(".{destination_name}.{:016x}.tmp", u64::from_le_bytes(random));
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        match directory.open_with(&name, &options) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create unique cache temporary file",
    ))
}

fn read_digest(bytes: &[u8], offset: usize) -> Option<Digest> {
    let raw: [u8; DIGEST_BYTES] = bytes.get(offset..offset + DIGEST_BYTES)?.try_into().ok()?;
    Some(Digest(raw))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn load_or_create_platform_key(
    service: &str,
    account: &str,
) -> Result<Zeroizing<[u8; DIGEST_BYTES]>, AuthenticationUnavailable> {
    with_non_interactive_credentials(|| {
        let entry = keyring::Entry::new(service, account)
            .map_err(|_| AuthenticationUnavailable)?;
        match entry.get_secret() {
            Ok(secret) => decode_key(secret),
            Err(keyring::Error::NoEntry) => {
                let mut candidate = Zeroizing::new([0_u8; DIGEST_BYTES]);
                getrandom::fill(candidate.as_mut()).map_err(|_| AuthenticationUnavailable)?;
                entry
                    .set_secret(candidate.as_ref())
                    .map_err(|_| AuthenticationUnavailable)?;
                // Re-read to narrow concurrent first-use creation races. A
                // still-later writer can invalidate this process's first cache
                // records, but that outcome is only a safe cache miss.
                decode_key(entry.get_secret().map_err(|_| AuthenticationUnavailable)?)
            }
            Err(_) => Err(AuthenticationUnavailable),
        }
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn load_or_create_platform_key() -> Result<Zeroizing<[u8; DIGEST_BYTES]>, AuthenticationUnavailable> {
    Err(AuthenticationUnavailable)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn decode_key(secret: Vec<u8>) -> Result<Zeroizing<[u8; DIGEST_BYTES]>, AuthenticationUnavailable> {
    let secret = Zeroizing::new(secret);
    let key: [u8; DIGEST_BYTES] = secret
        .as_slice()
        .try_into()
        .map_err(|_| AuthenticationUnavailable)?;
    Ok(Zeroizing::new(key))
}

#[cfg(target_os = "macos")]
fn with_non_interactive_credentials<T>(
    operation: impl FnOnce() -> Result<T, AuthenticationUnavailable>,
) -> Result<T, AuthenticationUnavailable> {
    use std::sync::Mutex;

    use security_framework::os::macos::keychain::SecKeychain;

    static KEYCHAIN_INTERACTION: Mutex<()> = Mutex::new(());
    let _serialized = KEYCHAIN_INTERACTION
        .lock()
        .map_err(|_| AuthenticationUnavailable)?;
    let _disabled = SecKeychain::disable_user_interaction()
        .map_err(|_| AuthenticationUnavailable)?;
    operation()
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn with_non_interactive_credentials<T>(
    operation: impl FnOnce() -> Result<T, AuthenticationUnavailable>,
) -> Result<T, AuthenticationUnavailable> {
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedAuthenticator([u8; DIGEST_BYTES]);

    impl Authenticator for FixedAuthenticator {
        fn tag(
            &self,
            canonical_record: &[u8],
        ) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
            Ok(keyed_tag(&self.0, canonical_record))
        }
    }

    struct UnavailableAuthenticator;

    impl Authenticator for UnavailableAuthenticator {
        fn tag(
            &self,
            _: &[u8],
        ) -> Result<[u8; TAG_BYTES], AuthenticationUnavailable> {
            Err(AuthenticationUnavailable)
        }
    }

    const AUTHENTICATOR: FixedAuthenticator = FixedAuthenticator([0x5a; DIGEST_BYTES]);

    fn context() -> CacheContext {
        CacheContext::new(CacheKind::OptimizedWasm, b"input wasm", b"recipe")
    }

    #[test]
    fn authenticated_record_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            store(directory.path(), context(), b"optimized wasm", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        assert_eq!(
            load(directory.path(), context(), 1024, &AUTHENTICATOR),
            Lookup::Hit(AuthenticatedPayload {
                kind: CacheKind::OptimizedWasm,
                bytes: b"optimized wasm".to_vec(),
            })
        );
    }

    #[test]
    fn self_consistent_unkeyed_record_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let payload = b"malicious wasm";
        let header = encode_header(context(), Digest::of(payload), payload.len());
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(blake3::hash(&header).as_bytes());
        bytes.extend_from_slice(payload);
        fs::write(directory.path().join(context().file_name()), bytes).unwrap();

        assert_eq!(
            load(directory.path(), context(), 1024, &AUTHENTICATOR),
            Lookup::Miss(MissReason::Authentication)
        );
    }

    #[test]
    fn payload_replacement_after_authentication_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            store(directory.path(), context(), b"good artifact", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        let path = directory.path().join(context().file_name());
        let mut bytes = fs::read(&path).unwrap();
        bytes[ENVELOPE_BYTES] ^= 0xff;
        fs::write(path, bytes).unwrap();

        assert_eq!(
            load(directory.path(), context(), 1024, &AUTHENTICATOR),
            Lookup::Miss(MissReason::ArtifactDigest)
        );
    }

    #[test]
    fn record_from_another_authenticator_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            store(directory.path(), context(), b"artifact", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        let other = FixedAuthenticator([0xa5; DIGEST_BYTES]);
        assert_eq!(
            load(directory.path(), context(), 1024, &other),
            Lookup::Miss(MissReason::Authentication)
        );
    }

    #[test]
    fn authenticated_store_repairs_a_corrupt_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(context().file_name());
        fs::write(&path, b"attacker bytes").unwrap();
        assert_eq!(
            store(directory.path(), context(), b"artifact", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        assert_eq!(
            load(directory.path(), context(), 1024, &AUTHENTICATOR),
            Lookup::Hit(AuthenticatedPayload {
                kind: CacheKind::OptimizedWasm,
                bytes: b"artifact".to_vec(),
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn final_component_symlink_is_not_followed() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            store(directory.path(), context(), b"artifact", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        let entry = directory.path().join(context().file_name());
        let target = directory.path().join("valid-but-aliased.wc1");
        fs::rename(&entry, &target).unwrap();
        symlink(&target, &entry).unwrap();

        assert_eq!(
            load(directory.path(), context(), 1024, &AUTHENTICATOR),
            Lookup::Miss(MissReason::Io)
        );
    }

    #[test]
    fn context_and_epoch_replays_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            store(directory.path(), context(), b"artifact", 1024, &AUTHENTICATOR),
            StoreOutcome::Stored
        );
        let source = directory.path().join(context().file_name());

        for other in [
            CacheContext::new(CacheKind::SourceWasm, b"input wasm", b"recipe"),
            CacheContext::new(CacheKind::OptimizedWasm, b"other input", b"recipe"),
            CacheContext::new(CacheKind::OptimizedWasm, b"input wasm", b"other recipe"),
            context().with_epoch(KEY_EPOCH + 1),
        ] {
            fs::copy(&source, directory.path().join(other.file_name())).unwrap();
            assert_eq!(
                load(directory.path(), other, 1024, &AUTHENTICATOR),
                Lookup::Miss(MissReason::Context)
            );
        }
    }

    #[test]
    fn unknown_schema_algorithm_and_kind_fail_closed() {
        let payload = b"artifact";
        let original = encode_header(context(), Digest::of(payload), payload.len());
        for (offset, value) in [(16, 2_u8), (18, 99), (19, 99)] {
            let mut bytes = original.to_vec();
            bytes[offset] = value;
            bytes.extend_from_slice(&[0_u8; TAG_BYTES]);
            bytes.extend_from_slice(payload);
            assert_eq!(
                decode(&bytes, context(), 1024, &AUTHENTICATOR),
                Lookup::Miss(MissReason::Malformed)
            );
        }
    }

    #[test]
    fn unavailable_authenticator_never_touches_cache_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("must-not-exist");
        assert_eq!(
            load(&directory, context(), 1024, &UnavailableAuthenticator),
            Lookup::Unavailable
        );
        assert_eq!(
            store(
                &directory,
                context(),
                b"artifact",
                1024,
                &UnavailableAuthenticator
            ),
            StoreOutcome::Unavailable
        );
        assert!(!directory.exists());
    }

    #[test]
    fn oversized_payload_is_rejected_before_allocation() {
        let payload = b"artifact";
        let mut bytes = encode_header(context(), Digest::of(payload), payload.len()).to_vec();
        bytes.extend_from_slice(&[0_u8; TAG_BYTES]);
        bytes.extend_from_slice(payload);
        assert_eq!(
            decode(&bytes, context(), payload.len() - 1, &AUTHENTICATOR),
            Lookup::Miss(MissReason::TooLarge)
        );
    }
}
