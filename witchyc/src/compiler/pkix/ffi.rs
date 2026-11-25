//! AWS-LC 0.14.4 declarations shared by the standalone native runtime
//! and the Wasmtime host. These signatures match the pinned headers.

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};

#[repr(transparent)]
pub(crate) struct X509(c_void);
#[repr(transparent)]
pub(crate) struct Store(c_void);
#[repr(transparent)]
pub(crate) struct Context(c_void);
#[repr(transparent)]
pub(crate) struct Parameters(c_void);
#[repr(transparent)]
pub(crate) struct Stack(c_void);
#[repr(transparent)]
pub(crate) struct PublicKey(c_void);
#[repr(transparent)]
pub(crate) struct EcKey(c_void);
#[repr(transparent)]
pub(crate) struct EcGroup(c_void);
#[repr(transparent)]
pub(crate) struct DigestContext(c_void);
#[repr(transparent)]
pub(crate) struct Digest(c_void);

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_d2i_X509"]
    pub(super) fn parse(out: *mut *mut X509, bytes: *mut *const u8, len: c_long) -> *mut X509;
    #[link_name = "aws_lc_fips_0_14_4_X509_free"]
    pub(super) fn free_certificate(certificate: *mut X509);
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_new"]
    pub(super) fn new_store() -> *mut Store;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_free"]
    pub(super) fn free_store(store: *mut Store);
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_add_cert"]
    pub(super) fn add_cert(store: *mut Store, cert: *mut X509) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_sk_new_null"]
    pub(super) fn new_stack() -> *mut Stack;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_sk_free"]
    pub(super) fn free_stack(stack: *mut Stack);
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_sk_push"]
    pub(super) fn push(stack: *mut Stack, cert: *mut c_void) -> usize;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_sk_num"]
    pub(super) fn count(stack: *const Stack) -> usize;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_sk_value"]
    pub(super) fn element(stack: *const Stack, index: usize) -> *mut c_void;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_new"]
    pub(super) fn new_context() -> *mut Context;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_free"]
    pub(super) fn free_context(context: *mut Context);
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_init"]
    pub(super) fn init_context(context: *mut Context, store: *mut Store, leaf: *mut X509, chain: *mut Stack) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_set_purpose"]
    pub(super) fn set_purpose(context: *mut Context, purpose: c_int) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_get0_param"]
    pub(super) fn parameters(context: *mut Context) -> *mut Parameters;
    #[link_name = "aws_lc_fips_0_14_4_X509_VERIFY_PARAM_set_time_posix"]
    pub(super) fn set_time(parameters: *mut Parameters, seconds: i64);
    #[link_name = "aws_lc_fips_0_14_4_X509_VERIFY_PARAM_set_hostflags"]
    pub(super) fn set_host_flags(parameters: *mut Parameters, flags: c_uint);
    #[link_name = "aws_lc_fips_0_14_4_X509_VERIFY_PARAM_set1_host"]
    pub(super) fn set_host(parameters: *mut Parameters, bytes: *const c_char, len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_X509_verify_cert"]
    pub(super) fn verify_chain(context: *mut Context) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_X509_STORE_CTX_get0_chain"]
    pub(super) fn verified_chain(context: *mut Context) -> *mut Stack;
    #[link_name = "aws_lc_fips_0_14_4_X509_get_key_usage"]
    pub(super) fn key_usage(cert: *mut X509) -> u32;
    #[link_name = "aws_lc_fips_0_14_4_X509_get0_pubkey"]
    pub(super) fn public_key(cert: *mut X509) -> *mut PublicKey;
    #[link_name = "aws_lc_fips_0_14_4_X509_get_signature_info"]
    pub(super) fn signature_info(cert: *mut X509, digest: *mut c_int, public_key: *mut c_int,
        security_bits: *mut c_int, flags: *mut u32) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_id"]
    pub(super) fn key_id(key: *const PublicKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_bits"]
    pub(super) fn key_bits(key: *const PublicKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_get0_EC_KEY"]
    pub(crate) fn ec_key(key: *const PublicKey) -> *mut EcKey;
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_get0_group"]
    pub(crate) fn ec_group(key: *const EcKey) -> *const EcGroup;
    #[link_name = "aws_lc_fips_0_14_4_EC_GROUP_get_curve_name"]
    pub(super) fn curve_name(group: *const EcGroup) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_MD_CTX_new"]
    pub(crate) fn new_digest_context() -> *mut DigestContext;
    #[link_name = "aws_lc_fips_0_14_4_EVP_MD_CTX_free"]
    pub(crate) fn free_digest_context(context: *mut DigestContext);
    #[link_name = "aws_lc_fips_0_14_4_EVP_sha384"]
    pub(crate) fn sha384() -> *const Digest;
    #[link_name = "aws_lc_fips_0_14_4_EVP_DigestVerifyInit"]
    pub(crate) fn digest_verify_init(context: *mut DigestContext, key_context: *mut *mut c_void,
        digest: *const Digest, engine: *mut c_void, key: *mut PublicKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_DigestVerify"]
    pub(crate) fn digest_verify(context: *mut DigestContext, signature: *const u8, signature_len: usize,
        message: *const u8, message_len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_ERR_clear_error"]
    pub(crate) fn clear_error();
}

pub(super) const RSA: c_int = 6;
pub(super) const RSA_PSS: c_int = 912;
pub(super) const EC: c_int = 408;
pub(super) const P256: c_int = 415;
pub(crate) const P384: c_int = 715;
pub(super) const P521: c_int = 716;
pub(super) const SHA256: c_int = 672;
pub(super) const SHA384: c_int = 673;
pub(super) const SHA512: c_int = 674;
pub(super) const DIGITAL_SIGNATURE: u32 = 0x80;
pub(super) const NEVER_CHECK_SUBJECT: c_uint = 0x20;
pub(super) const SSL_SERVER: c_int = 2;
