//! Raw guest boundary probes, not a substitute for source/admission integration.
use super::*;
use crate::runtime::{Capabilities, DirAuthority, Runtime, Vm};
use super::super::clock_env_rand;

type Token = Option<Rooted<ExternRef>>;
const WAT: &str = r#"(module
  (import "witchy" "clock_now" (func $now (param externref) (result i64)))
  (import "witchy" "secretstore_lookup_at" (func $lookup (param externref i32 i32) (result externref)))
  (import "witchy" "crypto_sign_into" (func $sign (param externref i32 i32 i32 i32) (result i32)))
  (import "witchy" "crypto_public_key_into" (func $pk (param externref i32 i32) (result i32)))
  (import "witchy" "crypto_reveal_len" (func $reveal (param externref) (result i32)))
  (import "witchy" "dir_make_dir_at" (func $mkdir (param externref i32 i32) (result externref)))
  (import "witchy" "cap_response_status" (func $status (param externref) (result i32)))
  (memory (export "memory") 1)
  (func (export "run"))
  (func (export "now") (param externref) (result i64) local.get 0 call $now)
  (func (export "lookup") (param externref i32 i32) (result externref)
    local.get 0 local.get 1 local.get 2 call $lookup)
  (func (export "sign") (param externref i32 i32 i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 local.get 3 local.get 4 call $sign)
  (func (export "pk") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $pk)
  (func (export "reveal") (param externref) (result i32) local.get 0 call $reveal)
  (func (export "mkdir") (param externref i32 i32) (result i32)
    local.get 0 local.get 1 local.get 2 call $mkdir call $status))"#;

// RFC 8032 test vector 1: independent expected key and empty-message signature.
const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
fn seed() -> [u8; 32] {
    let bytes: Vec<_> = (0..SEED.len()).step_by(2)
        .map(|i| u8::from_str_radix(&SEED[i..i + 2], 16).unwrap()).collect();
    bytes.try_into().unwrap()
}

fn caps() -> Capabilities {
    Capabilities { clock: true, signing_key: Some(seed()), secrets: vec![
        SecretGrant { name: "signing".into(), bytes: seed().to_vec(), sealed: true },
        SecretGrant::new("signing", vec![0; 32]),
        SecretGrant::new("hex", SEED.as_bytes().to_vec()),
        SecretGrant { name: "sealed".into(), bytes: b"do not reveal".to_vec(), sealed: true },
        SecretGrant::new("value", b"visible".to_vec()),
        SecretGrant::new("bad", vec![0; 31]),
    ], ..Capabilities::none() }
}

fn setup(mut caps: Capabilities, memory: bool) -> (tempfile::TempDir, Vm) {
    let root = tempfile::tempdir().unwrap();
    caps.dir_root = Some(root.path().to_owned());
    caps.dir_write = true;
    let mut wat = if memory { WAT.to_owned() } else { WAT.replace("(memory (export \"memory\") 1)", "") };
    if caps.signing_key.is_none() && caps.secrets.is_empty() {
        wat = wat
            .replace(r#"(import "witchy" "crypto_reveal_len" (func $reveal (param externref) (result i32)))"#, "")
            .replace(r#"(func (export "reveal") (param externref) (result i32) local.get 0 call $reveal)"#, "");
    }
    let vm = Runtime::new().unwrap().spawn(&wat, caps, 1).unwrap();
    (root, vm)
}

fn store(vm: &mut Vm) -> Token {
    let root = mint_store(vm.store.data());
    Some(ExternRef::new(&mut vm.store, root).unwrap())
}
fn key(vm: &mut Vm) -> Token {
    let key = mint_canonical_secret(vm.store.data()).unwrap();
    Some(ExternRef::new(&mut vm.store, key).unwrap())
}
fn clock(vm: &mut Vm) -> Token {
    let clock = clock_env_rand::mint_clock(vm.store.data()).unwrap();
    Some(ExternRef::new(&mut vm.store, clock).unwrap())
}
fn dir(vm: &mut Vm) -> Token {
    let authority = super::super::filesystem::dir_grant(vm.store.data(), 0).unwrap();
    Some(ExternRef::new(&mut vm.store, authority).unwrap())
}
fn write(vm: &mut Vm, at: usize, bytes: &[u8]) {
    vm.instance.get_memory(&mut vm.store, "memory").unwrap().write(&mut vm.store, at, bytes).unwrap();
}
fn memory(vm: &mut Vm) -> Vec<u8> {
    vm.instance.get_memory(&mut vm.store, "memory").unwrap().data(&vm.store).to_vec()
}
fn lookup(vm: &mut Vm, root: Token, ptr: i32, len: i32) -> Result<Token> {
    vm.instance.get_typed_func::<(Token, i32, i32), Token>(&mut vm.store, "lookup").unwrap()
        .call(&mut vm.store, (root, ptr, len))
}
fn named(vm: &mut Vm, root: Token, name: &str) -> Token {
    write(vm, 0, name.as_bytes());
    lookup(vm, root, 0, name.len() as i32).unwrap()
}
fn pk(vm: &mut Vm, key: Token, ptr: i32, cap: i32) -> Result<i32> {
    vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "pk").unwrap()
        .call(&mut vm.store, (key, ptr, cap))
}
fn sign(vm: &mut Vm, key: Token, ptr: i32, len: i32, out: i32, cap: i32) -> Result<i32> {
    vm.instance.get_typed_func::<(Token, i32, i32, i32, i32), i32>(&mut vm.store, "sign").unwrap()
        .call(&mut vm.store, (key, ptr, len, out, cap))
}
/// `dir_make_dir_at`, answered as its response's status.
fn mkdir(vm: &mut Vm, root: Token, ptr: i32, len: i32) -> Result<i32> {
    vm.instance.get_typed_func::<(Token, i32, i32), i32>(&mut vm.store, "mkdir").unwrap()
        .call(&mut vm.store, (root, ptr, len))
}
fn error(error: Error, expected: &str) {
    let message = format!("{error:#}");
    assert!(message.contains(expected), "expected {expected:?}, got {message}");
}

pub fn registry_hosts_clock_grant_kind_session_and_provider() {
    let (_root, mut vm) = setup(Capabilities::none(), false);
    assert!(clock_env_rand::mint_clock(vm.store.data()).is_err());
    let (_root2, mut other) = setup(caps(), false);
    let grant = clock_env_rand::mint_clock(other.store.data()).unwrap();
    let foreign = ExternRef::new(&mut vm.store, grant).unwrap();
    let now = vm.instance.get_typed_func::<Token, i64>(&mut vm.store, "now").unwrap();
    error(now.call(&mut vm.store, None).unwrap_err(), "null capability");
    let forged = ExternRef::new(&mut vm.store, 19u32).unwrap();
    error(now.call(&mut vm.store, Some(forged)).unwrap_err(), "wrong capability kind");
    error(now.call(&mut vm.store, Some(foreign)).unwrap_err(), "another instance");
    let grant = clock(&mut other);
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let value = other.instance.get_typed_func::<Token, i64>(&mut other.store, "now").unwrap()
        .call(&mut other.store, grant).unwrap();
    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    assert!((before..=after).contains(&value));
    assert!(other.output().is_empty());
}

pub fn registry_hosts_full_slice_preflight_and_invalid_seed_never_write() {
    let (_root, mut vm) = setup(caps(), true);
    let key = key(&mut vm);
    write(&mut vm, 0, &[0xff]);
    write(&mut vm, 1024, &[0xa5; 256]);
    let before = memory(&mut vm);
    for (ptr, len) in [(-1, 0), (0, -1), (65535, 2), (65537, 0), (0, 1)] {
        assert!(sign(&mut vm, key, ptr, len, 1024, 128).is_err());
        assert_eq!(memory(&mut vm), before);
    }
    for (ptr, cap) in [(-1, 0), (0, -1), (65500, 128), (65537, 0), (1024, i32::MAX)] {
        assert!(sign(&mut vm, key, 0, 0, ptr, cap).is_err());
        assert!(pk(&mut vm, key, ptr, cap).is_err());
        assert_eq!(memory(&mut vm), before);
    }
    assert_eq!(sign(&mut vm, key, 65536, 0, 65536, 0).unwrap(), 128);
    assert_eq!(pk(&mut vm, key, 65536, 0).unwrap(), 64);
    assert_eq!(memory(&mut vm), before);
    let root = store(&mut vm);
    for (ptr, len) in [(-1, 0), (0, -1), (65535, 2), (65537, 0), (0, 1)] {
        assert!(lookup(&mut vm, root, ptr, len).is_err());
    }
    let invalid = named(&mut vm, root, "bad");
    let before = memory(&mut vm);
    error(sign(&mut vm, invalid, 0, 0, 1024, 128).unwrap_err(), "not a signing key");
    error(pk(&mut vm, invalid, 1024, 64).unwrap_err(), "not a signing key");
    assert_eq!(memory(&mut vm), before);
}

pub fn registry_hosts_secret_kind_and_foreign_issuer_precede_memory() {
    let (_root, mut vm) = setup(caps(), false);
    let root = store(&mut vm);
    let secret = key(&mut vm);
    let clock = clock(&mut vm);
    let forged = Some(ExternRef::new(&mut vm.store, 21u32).unwrap());
    for invalid in [None, clock, forged, root] {
        let expected = if invalid.is_none() { "null capability" } else { "wrong capability kind" };
        error(sign(&mut vm, invalid, -1, -1, -1, -1).unwrap_err(), expected);
        error(pk(&mut vm, invalid, -1, -1).unwrap_err(), expected);
    }
    for invalid in [None, clock, forged, secret] {
        error(lookup(&mut vm, invalid, -1, -1).unwrap_err(),
            if invalid.is_none() { "null capability" } else { "wrong capability kind" });
    }
    vm.store.data_mut().registry_issuer = Arc::new(());
    error(lookup(&mut vm, root, -1, -1).unwrap_err(), "another instance");
    error(sign(&mut vm, secret, -1, -1, -1, -1).unwrap_err(), "another instance");
    error(pk(&mut vm, secret, -1, -1).unwrap_err(), "another instance");
}

pub fn registry_hosts_make_dir_right_and_kind_precede_memory() {
    let (_root, mut vm) = setup(Capabilities::none(), false);
    let grant = dir(&mut vm);
    let mut read_only = grant.unwrap().data(&vm.store).unwrap().unwrap().downcast_ref::<DirAuthority>().unwrap().clone();
    read_only.rights.write = false;
    let read_only = Some(ExternRef::new(&mut vm.store, read_only).unwrap());
    error(mkdir(&mut vm, read_only, -1, -1).unwrap_err(), "does not grant Write");
    error(mkdir(&mut vm, None, -1, -1).unwrap_err(), "null");
    let wrong = store(&mut vm);
    error(mkdir(&mut vm, wrong, -1, -1).unwrap_err(), "wrong host data");
}

#[cfg(unix)]
pub fn registry_hosts_make_dir_rejects_outside_symlink() {
    let (root, mut vm) = setup(Capabilities::none(), true);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let grant = dir(&mut vm);
    write(&mut vm, 0, b"escape/created");
    assert_eq!(mkdir(&mut vm, grant, 0, 14).unwrap(), 1, "a link out of the root is Denied");
    assert!(!outside.path().join("created").exists());
}

pub(in crate::runtime::host) fn capture_fixture() -> (tempfile::TempDir, Vm, [Rooted<ExternRef>; 3]) {
    let (root, mut vm) = setup(caps(), true);
    let tokens = [clock(&mut vm).unwrap(), store(&mut vm).unwrap(), key(&mut vm).unwrap()];
    (root, vm, tokens)
}
