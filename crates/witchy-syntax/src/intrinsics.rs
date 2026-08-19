//! Canonical intrinsic and primitive-operation catalog.
//!
//! This includes private compiler entry points and public std primitives that
//! cross a backend boundary. The catalog is deliberately representation-neutral
//! so syntax, type checking, interpretation, and lowering can share it without
//! reversing the compiler crate dependency graph.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicId {
    GeneratedRender,
    GeneratedListPush,
    CompilerQuoteItem,
    CompilerQuoteItemHoles,
    CompilerQuoteExpr,
    CompilerQuoteExprHoles,
    CompilerQuoteType,
    CompilerQuoteTypeHoles,
    CompilerQuotePattern,
    CompilerQuotePatternHoles,
    CompilerQuoteStmt,
    CompilerQuoteStmtHoles,
    CompilerQuoteBlock,
    CompilerQuoteBlockHoles,
    CompilerEmitItem,
    CompilerEmitExpr,
    TryContext,
    Erase,
    Unerase,
    DynamicRuntimeType,
    DynamicDescriptor,
    DynamicDescriptorId,
    DynamicFields,
    DynamicFieldStatus,
    DynamicMethods,
    DynamicCall,
    DynamicCallWith,
    DynamicImplements,
    DynamicAsTrait,
    DynamicTryDecode,
    DynamicTryDecodeTyped,
    BytesFromString,
    BytesFromList,
    BytesToString,
    BytesLength,
    BytesAt,
    BytesConcat,
    BytesSlice,
    ChannelOpen,
    ChannelSend,
    ChannelRecv,
    ChannelSelect,
    SecretStoreGet,
    SecretStoreRequire,
    MetaItem,
    MetaExpr,
    MetaFreshIdent,
    MetaExprLeaf,
    MetaPatternLeaf,
    MetaStmtLeaf,
    MetaCallSiteExpr,
    MetaCallSiteType,
    MetaCallSitePattern,
    MetaPatternCtor,
    MetaPatternTuple,
    MetaPatternList,
    MetaPatternListRest,
    MetaPatternOr,
    MetaTypeNamed,
    MetaTypeTuple,
    MetaTypeFn,
    MetaTypeQualified,
    MetaTypeExpr,
    MetaTypeCapability,
    MetaExprCall,
    MetaExprField,
    MetaExprMatch,
    MetaMatchArm,
    MetaBlock,
    MetaStmtExpr,
    MetaStmtReturn,
    MetaStmtLet,
    MetaParam,
    MetaFunctionBlock,
    MetaImplBlock,
    CompilerFootprint,
    CompilerDiff,
    CompilerDoc,
    CompilerDocResultJson,
    RegexMatchSpans,
    EncodingUtf8Lossy,
    EncodingHexEncode,
    EncodingHexEncodeBytes,
    EncodingHexDecodeLossy,
    EncodingHexDecodeBytesRaw,
    EncodingBase64Encode,
    EncodingBase64EncodeBytes,
    EncodingBase64UrlEncodeBytes,
    EncodingHexToBase64UrlLossy,
    EncodingBase64DecodeLossy,
    EncodingBase64DecodeBytesRaw,
    EncodingBase64UrlDecodeLossy,
    EncodingBase64UrlDecodeBytesRaw,
    EncodingBase64UrlToHexLossy,
    CryptoSha256,
    CryptoSha256Bytes,
    CryptoRuneHash,
    CryptoEd25519VerifyStatus,
    CryptoSign,
    CryptoPublicKey,
    CryptoReveal,
    CryptoEcdsaP256VerifyStatus,
    CryptoEcdsaP256VerifyHexStatus,
    CryptoRsaPkcs1Sha256VerifyStatus,
    CryptoSha512,
    CryptoSha3_256,
    CryptoHmacSha256,
    CryptoShake128,
    CryptoShake256,
    StringLength,
    StringCharCount,
    StringChars,
    StringFromCode,
    StringSplit,
    StringContains,
    StringStartsWith,
    StringEndsWith,
    StringFind,
    StringReplace,
    StringSubstring,
    StringToUpper,
    StringToLower,
    StringTrim,
    StringToInt,
    MathToFloat,
    MathToInt,
    MathSqrt,
    ListLength,
    ListAt,
    ListPush,
    ListSetAt,
    ListConcat,
    ListPopExtract,
    ListWithCapacity,
    DictNew,
    DictInsert,
    DictInsertExtract,
    DictGetOr,
    DictAt,
    DictUpdate,
    DictContainsKey,
    DictRemove,
    DictRemoveExtract,
    DictKeys,
    DictValues,
    DictPairs,
    DictLength,
}

/// A representation-neutral type recipe interpreted by `witchy-types`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicSignature {
    GenericRender,
    GenericListPush,
    CompilerQuoteItem,
    CompilerQuoteItemHoles,
    CompilerQuoteExpr,
    CompilerQuoteExprHoles,
    CompilerQuoteType,
    CompilerQuoteTypeHoles,
    CompilerQuotePattern,
    CompilerQuotePatternHoles,
    CompilerQuoteStmt,
    CompilerQuoteStmtHoles,
    CompilerQuoteBlock,
    CompilerQuoteBlockHoles,
    CompilerEmitItem,
    CompilerEmitExpr,
    TryContext,
    GenericToMessage,
    MessageToGeneric,
    StringStringToRuntimeType,
    GenericToRuntimeType,
    GenericToInt,
    RuntimeTypeToListRuntimeField,
    RuntimeTypeStringToDynamicFieldStatus,
    RuntimeTypeToListRuntimeMethod,
    DynamicStringListDynamicToResultDynamicDynamicError,
    DynamicStringListDynamicGenericToResultDynamicDynamicError,
    DynamicRuntimeTypeToBool,
    DynamicRuntimeTypeToResultDynamicDynamicError,
    DynamicToOptionGeneric,
    DynamicIntToOptionGeneric,
    StringToBytes,
    ListIntToBytes,
    BytesToString,
    BytesToInt,
    BytesIntToInt,
    BytesBytesToBytes,
    BytesIntIntToBytes,
    BytesIntToBytes,
    SecretStoreStringToOptionSecret,
    SecretStoreStringToSecret,
    StringToString,
    StringStringToString,
    StringToInt,
    StringStringToInt,
    StringStringToBool,
    StringToListString,
    StringStringToListString,
    StringStringStringToString,
    StringStringStringToInt,
    StringIntIntToString,
    ListStringListStringToString,
    /// (RFC-0121) A by-handle secret op: asks only for `Seal`, so a `Secret[Seal]`
    /// satisfies it and a bare `Secret` narrows into it (`crypto.sign`).
    SealedSecretStringToString,
    /// (RFC-0121) A by-handle secret op of arity one (`crypto.public_key`).
    SealedSecretToString,
    /// (RFC-0121) Reading a secret's bytes: needs the `Reveal` right, so a
    /// `Secret[Seal]` is a check-time error (`crypto.reveal`).
    RevealSecretToString,
    IntToString,
    IntToFloat,
    FloatToInt,
    FloatToFloat,
    GenericListToInt,
    GenericListIndex,
    GenericListSetAt,
    GenericListConcat,
    GenericListPopExtract,
    GenericListWithCapacity,
    GenericDictNew,
    GenericDictInsert,
    GenericDictInsertExtract,
    GenericDictGetOr,
    GenericDictIndex,
    GenericDictUpdate,
    GenericDictContainsKey,
    GenericDictRemove,
    GenericDictRemoveExtract,
    GenericDictKeys,
    GenericDictValues,
    GenericDictPairs,
    GenericDictToInt,
    DeclaredInSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntrinsicTraitBound {
    pub parameter: usize,
    pub trait_name: &'static str,
}

const DICT_KEY_EQ_BOUND: &[IntrinsicTraitBound] = &[IntrinsicTraitBound {
    parameter: 1,
    trait_name: "Eq",
}];

impl IntrinsicSignature {
    pub fn returns_string(self) -> bool {
        matches!(
            self,
            Self::GenericRender
                | Self::BytesToString
                | Self::StringToString
                | Self::StringStringToString
                | Self::StringStringStringToString
                | Self::StringIntIntToString
                | Self::ListStringListStringToString
                | Self::SealedSecretStringToString
                | Self::SealedSecretToString
                | Self::RevealSecretToString
                | Self::IntToString
        )
    }

    pub fn returns_int(self) -> bool {
        matches!(
            self,
            Self::BytesToInt
                | Self::BytesIntToInt
                | Self::StringToInt
                | Self::StringStringToInt
                | Self::StringStringStringToInt
                | Self::FloatToInt
                | Self::GenericListToInt
                | Self::GenericDictToInt
        )
    }

    pub fn returns_bool(self) -> bool {
        matches!(self, Self::StringStringToBool | Self::GenericDictContainsKey)
    }

    pub fn returns_bytes(self) -> bool {
        matches!(
            self,
            Self::StringToBytes
                | Self::ListIntToBytes
                | Self::BytesBytesToBytes
                | Self::BytesIntIntToBytes
                | Self::BytesIntToBytes
        )
    }

    pub fn returns_float(self) -> bool {
        matches!(self, Self::IntToFloat | Self::FloatToFloat)
    }

    pub fn returns_list(self) -> bool {
        matches!(
            self,
            Self::GenericListPush
                | Self::GenericListSetAt
                | Self::GenericListConcat
                | Self::GenericListWithCapacity
                | Self::GenericDictKeys
                | Self::GenericDictValues
                | Self::GenericDictPairs
        )
    }

    pub fn returns_list_element(self) -> bool {
        matches!(self, Self::GenericListIndex)
    }

    pub fn returns_dict(self) -> bool {
        matches!(
            self,
            Self::GenericDictNew
                | Self::GenericDictInsert
                | Self::GenericDictUpdate
                | Self::GenericDictRemove
        )
    }

    pub fn returns_dict_value(self) -> bool {
        matches!(self, Self::GenericDictGetOr | Self::GenericDictIndex)
    }

    pub fn returns_dict_keys(self) -> bool {
        matches!(self, Self::GenericDictKeys)
    }

    pub fn returns_dict_values(self) -> bool {
        matches!(self, Self::GenericDictValues)
    }

    pub fn returns_dict_pairs(self) -> bool {
        matches!(self, Self::GenericDictPairs)
    }

    pub fn trait_bounds(self) -> &'static [IntrinsicTraitBound] {
        match self {
            Self::GenericDictInsert
            | Self::GenericDictInsertExtract
            | Self::GenericDictGetOr
            | Self::GenericDictIndex
            | Self::GenericDictUpdate
            | Self::GenericDictContainsKey
            | Self::GenericDictRemove
            | Self::GenericDictRemoveExtract => DICT_KEY_EQ_BOUND,
            _ => &[],
        }
    }

    pub fn unique_parameters(self) -> &'static [usize] {
        match self {
            Self::GenericDictInsert | Self::GenericDictRemove => &[0],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicEffect {
    Pure,
    WriteBack,
    ControlFlow,
    Task,
    Toolchain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityEffect {
    None,
    ReadsSecretStore,
    UsesSecret,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicLowering {
    FrontendGenerated,
    Identity,
    Builtin,
    SourceFunction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicRuntime {
    InterpreterBuiltin,
    SourceFunction,
    Native,
}

/// The flat-buffer representation a selected WIR host helper must decode.
/// This is separate from the source signature because representation bridges
/// such as lossy UTF-8 receive raw bytes at the host boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WirHostInput {
    String,
    Bytes,
    LossyUtf8Bytes,
}

/// A selector-based call through one shared WIR helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WirHostCall {
    pub helper: &'static str,
    pub selector: i32,
    pub input: WirHostInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntrinsicSpec {
    pub id: IntrinsicId,
    pub name: &'static str,
    pub arity: usize,
    pub signature: IntrinsicSignature,
    pub effect: IntrinsicEffect,
    pub capability_effect: CapabilityEffect,
    pub lowering: IntrinsicLowering,
    pub runtime: IntrinsicRuntime,
    /// Statically named WIR helpers this operation may request.
    pub wir_helpers: &'static [&'static str],
    /// The operation may additionally synthesize a shape-specific helper.
    pub dynamic_wir_helpers: bool,
    /// Selector metadata when multiple operations share one WIR host helper.
    pub wir_host_call: Option<WirHostCall>,
    pub diagnostic_name: &'static str,
    pub private_callers: &'static [&'static str],
}

const NO_HELPERS: &[&str] = &[];
const NO_PRIVATE_CALLERS: &[&str] = &[];
const MESSAGE_BRIDGE_CALLERS: &[&str] = &["chan", "task"];
const DYNAMIC_BRIDGE_CALLERS: &[&str] = &["dynamic"];
const BYTES_BRIDGE_CALLERS: &[&str] = &["bytes"];
const META_BRIDGE_CALLERS: &[&str] = &["meta"];
const ENCODING_HELPERS: &[&str] = &["encoding"];

const fn encoding_host_call(selector: i32, input: WirHostInput) -> Option<WirHostCall> {
    Some(WirHostCall { helper: "encoding", selector, input })
}

pub(crate) const GENERATED_RENDER: &str = "@render";
pub const GENERATED_LIST_PUSH: &str = "@list_push";
pub const COMPILER_QUOTE_ITEM: &str = "@quote_item";
pub const COMPILER_QUOTE_ITEM_HOLES: &str = "@quote_item_holes";
pub const COMPILER_QUOTE_EXPR: &str = "@quote_expr";
pub const COMPILER_QUOTE_EXPR_HOLES: &str = "@quote_expr_holes";
pub const COMPILER_QUOTE_TYPE: &str = "@quote_type";
pub const COMPILER_QUOTE_TYPE_HOLES: &str = "@quote_type_holes";
pub const COMPILER_QUOTE_PATTERN: &str = "@quote_pattern";
pub const COMPILER_QUOTE_PATTERN_HOLES: &str = "@quote_pattern_holes";
pub const COMPILER_QUOTE_STMT: &str = "@quote_stmt";
pub const COMPILER_QUOTE_STMT_HOLES: &str = "@quote_stmt_holes";
pub const COMPILER_QUOTE_BLOCK: &str = "@quote_block";
pub const COMPILER_QUOTE_BLOCK_HOLES: &str = "@quote_block_holes";
pub const COMPILER_EMIT_ITEM: &str = "@emit_item";
pub const COMPILER_EMIT_EXPR: &str = "@emit_expr";
pub(crate) const RETIRED_SOURCE_RENDER: &str = "__render";
pub const TRY_CONTEXT: &str = "__try_ctx";

pub const ERASE: &str = "__erase";
pub const UNERASE: &str = "__unerase";
pub const DYNAMIC_RUNTIME_TYPE: &str = "__dynamic_runtime_type";
pub const DYNAMIC_DESCRIPTOR: &str = "__dynamic_descriptor";
pub const DYNAMIC_DESCRIPTOR_ID: &str = "__dynamic_descriptor_id";
pub const DYNAMIC_FIELDS: &str = "__dynamic_fields";
pub const DYNAMIC_FIELD_STATUS: &str = "__dynamic_field_status";
pub const DYNAMIC_METHODS: &str = "__dynamic_methods";
pub const DYNAMIC_CALL: &str = "__dynamic_call";
pub const DYNAMIC_CALL_WITH: &str = "__dynamic_call_with";
pub const DYNAMIC_IMPLEMENTS: &str = "__dynamic_implements";
pub const DYNAMIC_AS_TRAIT: &str = "__dynamic_as_trait";
pub const DYNAMIC_TRY_DECODE: &str = "__dynamic_try_decode";
pub const DYNAMIC_TRY_DECODE_TYPED: &str = "__dynamic_try_decode_typed";

pub const BYTES_FROM_STRING: &str = "__bytes_from_string";
pub const BYTES_FROM_LIST: &str = "__bytes_from_list";
pub const BYTES_TO_STRING: &str = "__bytes_to_string";
pub const BYTES_LENGTH: &str = "__bytes_length";
pub const BYTES_AT: &str = "__bytes_at";
pub(crate) const BYTES_AT_PUBLIC: &str = "bytes.at";
pub const BYTES_CONCAT: &str = "__bytes_concat";
pub const BYTES_SLICE: &str = "__bytes_slice";

pub(crate) const CHANNEL_OPEN: &str = "__channel_open";
pub(crate) const CHANNEL_SEND: &str = "__channel_send";
pub(crate) const CHANNEL_RECV: &str = "__channel_recv";
pub(crate) const CHANNEL_SELECT: &str = "__channel_select";

pub const SECRETSTORE_GET: &str = "secretstore.get";
pub const SECRETSTORE_REQUIRE: &str = "secretstore.require";
pub(crate) const META_ITEM: &str = "__meta_item";
pub(crate) const META_EXPR: &str = "__meta_expr";
pub(crate) const META_FRESH_IDENT: &str = "__meta_fresh_ident";
pub(crate) const META_EXPR_LEAF: &str = "__meta_expr_leaf";
pub(crate) const META_PATTERN_LEAF: &str = "__meta_pattern_leaf";
pub(crate) const META_STMT_LEAF: &str = "__meta_stmt_leaf";
pub(crate) const META_CALL_SITE_EXPR: &str = "__meta_call_site_expr";
pub(crate) const META_CALL_SITE_TYPE: &str = "__meta_call_site_type";
pub(crate) const META_CALL_SITE_PATTERN: &str = "__meta_call_site_pattern";
pub(crate) const META_PATTERN_CTOR: &str = "__meta_pattern_ctor";
pub(crate) const META_PATTERN_TUPLE: &str = "__meta_pattern_tuple";
pub(crate) const META_PATTERN_LIST: &str = "__meta_pattern_list";
pub(crate) const META_PATTERN_LIST_REST: &str = "__meta_pattern_list_rest";
pub(crate) const META_PATTERN_OR: &str = "__meta_pattern_or";
pub(crate) const META_TYPE_NAMED: &str = "__meta_type_named";
pub(crate) const META_TYPE_TUPLE: &str = "__meta_type_tuple";
pub(crate) const META_TYPE_FN: &str = "__meta_type_fn";
pub(crate) const META_TYPE_QUALIFIED: &str = "__meta_type_qualified";
pub(crate) const META_TYPE_EXPR: &str = "__meta_type_expr";
pub(crate) const META_TYPE_CAPABILITY: &str = "__meta_type_capability";
pub(crate) const META_EXPR_CALL: &str = "__meta_expr_call";
pub(crate) const META_EXPR_FIELD: &str = "__meta_expr_field";
pub(crate) const META_EXPR_MATCH: &str = "__meta_expr_match";
pub(crate) const META_MATCH_ARM: &str = "__meta_match_arm";
pub(crate) const META_BLOCK: &str = "__meta_block";
pub(crate) const META_STMT_EXPR: &str = "__meta_stmt_expr";
pub(crate) const META_STMT_RETURN: &str = "__meta_stmt_return";
pub(crate) const META_STMT_LET: &str = "__meta_stmt_let";
pub(crate) const META_PARAM: &str = "__meta_param";
pub(crate) const META_FUNCTION_BLOCK: &str = "__meta_function_block";
pub(crate) const META_IMPL_BLOCK: &str = "__meta_impl_block";

pub const COMPILER_FOOTPRINT: &str = "compiler.footprint";
pub const COMPILER_DIFF: &str = "compiler.diff";
pub const COMPILER_DOC: &str = "compiler.doc";
pub const COMPILER_DOC_RESULT_JSON: &str = "compiler.__doc_result_json";

pub const REGEX_MATCH_SPANS: &str = "regex.match_spans";

pub const ENCODING_UTF8_LOSSY: &str = "encoding.utf8_lossy";
pub const ENCODING_HEX_ENCODE: &str = "encoding.hex_encode";
pub const ENCODING_HEX_ENCODE_BYTES: &str = "encoding.hex_encode_bytes";
pub const ENCODING_HEX_DECODE_LOSSY: &str = "encoding.hex_decode_lossy";
pub const ENCODING_HEX_DECODE_BYTES_RAW: &str = "encoding.hex_decode_bytes_raw";
pub const ENCODING_BASE64_ENCODE: &str = "encoding.base64_encode";
pub const ENCODING_BASE64_ENCODE_BYTES: &str = "encoding.base64_encode_bytes";
pub const ENCODING_BASE64URL_ENCODE_BYTES: &str = "encoding.base64url_encode_bytes";
pub const ENCODING_HEX_TO_BASE64URL_LOSSY: &str = "encoding.hex_to_base64url_lossy";
pub const ENCODING_BASE64_DECODE_LOSSY: &str = "encoding.base64_decode_lossy";
pub const ENCODING_BASE64_DECODE_BYTES_RAW: &str = "encoding.base64_decode_bytes_raw";
pub const ENCODING_BASE64URL_DECODE_LOSSY: &str = "encoding.base64url_decode_lossy";
pub const ENCODING_BASE64URL_DECODE_BYTES_RAW: &str = "encoding.base64url_decode_bytes_raw";
pub const ENCODING_BASE64URL_TO_HEX_LOSSY: &str = "encoding.base64url_to_hex_lossy";

pub const CRYPTO_SHA256: &str = "crypto.sha256";
pub const CRYPTO_SHA256_BYTES: &str = "crypto.sha256_bytes";
pub const CRYPTO_RUNE_HASH: &str = "crypto.rune_hash";
pub const CRYPTO_ED25519_VERIFY_STATUS: &str = "crypto.__ed25519_verify_status";
pub const CRYPTO_SIGN: &str = "crypto.sign";
pub const CRYPTO_PUBLIC_KEY: &str = "crypto.public_key";
pub const CRYPTO_REVEAL: &str = "crypto.reveal";
pub const CRYPTO_ECDSA_P256_VERIFY_STATUS: &str = "crypto.__ecdsa_p256_verify_status";
pub const CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS: &str =
    "crypto.__ecdsa_p256_verify_hex_status";
pub const CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS: &str =
    "crypto.__rsa_pkcs1_sha256_verify_status";
pub const CRYPTO_SHA512: &str = "crypto.sha512";
pub const CRYPTO_SHA3_256: &str = "crypto.sha3_256";
pub const CRYPTO_HMAC_SHA256: &str = "crypto.hmac_sha256";
// (RFC-0106) Native-only SHAKE XOFs: variable-length raw-byte output, native and
// interpreter targets only. The browser host deliberately omits these imports.
pub const CRYPTO_SHAKE128: &str = "crypto.__shake128";
pub const CRYPTO_SHAKE256: &str = "crypto.__shake256";

pub const STRING_LENGTH: &str = "string.length";
pub const STRING_CHAR_COUNT: &str = "string.char_count";
pub const STRING_CHARS: &str = "string.chars";
pub const STRING_FROM_CODE: &str = "string.from_code";
pub const STRING_SPLIT: &str = "string.split";
pub const STRING_CONTAINS: &str = "string.contains";
pub const STRING_STARTS_WITH: &str = "string.starts_with";
pub const STRING_ENDS_WITH: &str = "string.ends_with";
pub const STRING_FIND: &str = "string.find";
pub const STRING_REPLACE: &str = "string.replace";
pub const STRING_SUBSTRING: &str = "string.substring";
pub const STRING_TO_UPPER: &str = "string.to_upper";
pub const STRING_TO_LOWER: &str = "string.to_lower";
pub const STRING_TRIM: &str = "string.trim";
pub const STRING_TO_INT: &str = "string.to_int";

pub const MATH_TO_FLOAT: &str = "math.to_float";
pub const MATH_TO_INT: &str = "math.to_int";
pub const MATH_SQRT: &str = "math.sqrt";

pub const LIST_LENGTH: &str = "list.length";
pub const LIST_AT: &str = "list.at";
pub const LIST_PUSH: &str = "list.__push";
pub const LIST_SET_AT: &str = "list.__set_at";
/// Compiler-owned assignment through an explicit opt-mode reference. The parser
/// alone emits this call for `*reference = value`; it is not source-callable.
/// Backends consume it as a place write, never as an ordinary function call.
pub const REFERENCE_WRITE: &str = "@reference_write";
pub const LIST_CONCAT: &str = "list.concat";
pub const LIST_POP_EXTRACT: &str = "list.__pop_extract";
pub const LIST_WITH_CAPACITY: &str = "list.with_capacity";

pub const DICT_NEW: &str = "dict.new";
pub const DICT_INSERT: &str = "dict.__insert";
pub const DICT_INSERT_EXTRACT: &str = "dict.__insert_extract";
pub const DICT_GET_OR: &str = "dict.get_or";
pub const DICT_AT: &str = "dict.at";
pub const DICT_UPDATE: &str = "dict.__update";
pub const DICT_CONTAINS_KEY: &str = "dict.contains_key";
pub const DICT_REMOVE: &str = "dict.__remove";
pub const DICT_REMOVE_EXTRACT: &str = "dict.__remove_extract";
pub const DICT_KEYS: &str = "dict.keys";
pub const DICT_VALUES: &str = "dict.values";
pub const DICT_PAIRS: &str = "dict.pairs";
pub const DICT_LENGTH: &str = "dict.length";

pub const ALL: &[IntrinsicSpec] = &[
    IntrinsicSpec {
        id: IntrinsicId::GeneratedRender,
        name: GENERATED_RENDER,
        arity: 1,
        signature: IntrinsicSignature::GenericRender,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["int_to_string", "float_to_str", "concat"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "string interpolation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::GeneratedListPush,
        name: GENERATED_LIST_PUSH,
        arity: 2,
        signature: IntrinsicSignature::GenericListPush,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_push", "list_push_cap"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "list.push",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteItem,
        name: COMPILER_QUOTE_ITEM,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuoteItem,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned item quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteItemHoles,
        name: COMPILER_QUOTE_ITEM_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteItemHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned item quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteExpr,
        name: COMPILER_QUOTE_EXPR,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuoteExpr,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned expression quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteExprHoles,
        name: COMPILER_QUOTE_EXPR_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteExprHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned expression quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteType,
        name: COMPILER_QUOTE_TYPE,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuoteType,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned type quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteTypeHoles,
        name: COMPILER_QUOTE_TYPE_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteTypeHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned type quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuotePattern,
        name: COMPILER_QUOTE_PATTERN,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuotePattern,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned pattern quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuotePatternHoles,
        name: COMPILER_QUOTE_PATTERN_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuotePatternHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned pattern quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteStmt,
        name: COMPILER_QUOTE_STMT,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuoteStmt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned statement quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteStmtHoles,
        name: COMPILER_QUOTE_STMT_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteStmtHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned statement quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteBlock,
        name: COMPILER_QUOTE_BLOCK,
        arity: 2,
        signature: IntrinsicSignature::CompilerQuoteBlock,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned block quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteBlockHoles,
        name: COMPILER_QUOTE_BLOCK_HOLES,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteBlockHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned block quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerEmitItem,
        name: COMPILER_EMIT_ITEM,
        arity: 1,
        signature: IntrinsicSignature::CompilerEmitItem,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned item emission",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerEmitExpr,
        name: COMPILER_EMIT_EXPR,
        arity: 1,
        signature: IntrinsicSignature::CompilerEmitExpr,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler-owned expression emission",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::TryContext,
        name: TRY_CONTEXT,
        arity: 2,
        signature: IntrinsicSignature::TryContext,
        effect: IntrinsicEffect::ControlFlow,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "? context",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::Erase,
        name: ERASE,
        arity: 1,
        signature: IntrinsicSignature::GenericToMessage,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Identity,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "message erasure",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::Unerase,
        name: UNERASE,
        arity: 1,
        signature: IntrinsicSignature::MessageToGeneric,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Identity,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "message recovery",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicRuntimeType,
        name: DYNAMIC_RUNTIME_TYPE,
        arity: 2,
        signature: IntrinsicSignature::StringStringToRuntimeType,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "runtime type descriptor",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicDescriptor,
        name: DYNAMIC_DESCRIPTOR,
        arity: 1,
        signature: IntrinsicSignature::GenericToRuntimeType,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic descriptor construction",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicDescriptorId,
        name: DYNAMIC_DESCRIPTOR_ID,
        arity: 1,
        signature: IntrinsicSignature::GenericToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic descriptor identity",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicFields,
        name: DYNAMIC_FIELDS,
        arity: 1,
        signature: IntrinsicSignature::RuntimeTypeToListRuntimeField,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic field enumeration",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicFieldStatus,
        name: DYNAMIC_FIELD_STATUS,
        arity: 2,
        signature: IntrinsicSignature::RuntimeTypeStringToDynamicFieldStatus,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic field lookup",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicMethods,
        name: DYNAMIC_METHODS,
        arity: 1,
        signature: IntrinsicSignature::RuntimeTypeToListRuntimeMethod,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic method enumeration",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicCall,
        name: DYNAMIC_CALL,
        arity: 3,
        signature: IntrinsicSignature::DynamicStringListDynamicToResultDynamicDynamicError,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic method call",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicCallWith,
        name: DYNAMIC_CALL_WITH,
        arity: 4,
        signature: IntrinsicSignature::DynamicStringListDynamicGenericToResultDynamicDynamicError,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic method call with explicit capabilities",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicImplements,
        name: DYNAMIC_IMPLEMENTS,
        arity: 2,
        signature: IntrinsicSignature::DynamicRuntimeTypeToBool,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic trait membership query",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicAsTrait,
        name: DYNAMIC_AS_TRAIT,
        arity: 2,
        signature: IntrinsicSignature::DynamicRuntimeTypeToResultDynamicDynamicError,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic checked trait view",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicTryDecode,
        name: DYNAMIC_TRY_DECODE,
        arity: 1,
        signature: IntrinsicSignature::DynamicToOptionGeneric,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "Dynamic decoding",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicTryDecodeTyped,
        name: DYNAMIC_TRY_DECODE_TYPED,
        arity: 2,
        signature: IntrinsicSignature::DynamicIntToOptionGeneric,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "typed Dynamic decoding",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesFromString,
        name: BYTES_FROM_STRING,
        arity: 1,
        signature: IntrinsicSignature::StringToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Identity,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.from_string",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesFromList,
        name: BYTES_FROM_LIST,
        arity: 1,
        signature: IntrinsicSignature::ListIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["bytes_from_list"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.from_list",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesToString,
        name: BYTES_TO_STRING,
        arity: 1,
        signature: IntrinsicSignature::BytesToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["bytes_to_string"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.to_string",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesLength,
        name: BYTES_LENGTH,
        arity: 1,
        signature: IntrinsicSignature::BytesToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.length",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesAt,
        name: BYTES_AT,
        arity: 2,
        signature: IntrinsicSignature::BytesIntToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["bytes_at"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.at",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesConcat,
        name: BYTES_CONCAT,
        arity: 2,
        signature: IntrinsicSignature::BytesBytesToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["concat"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.concat",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesSlice,
        name: BYTES_SLICE,
        arity: 3,
        signature: IntrinsicSignature::BytesIntIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["bytes_slice"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "bytes.slice",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ChannelOpen,
        name: CHANNEL_OPEN,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Task,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::SourceFunction,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "channel open",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ChannelSend,
        name: CHANNEL_SEND,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Task,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::SourceFunction,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "channel send",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ChannelRecv,
        name: CHANNEL_RECV,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Task,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::SourceFunction,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "channel receive",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ChannelSelect,
        name: CHANNEL_SELECT,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Task,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::SourceFunction,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "channel select",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::SecretStoreGet,
        name: SECRETSTORE_GET,
        arity: 2,
        signature: IntrinsicSignature::SecretStoreStringToOptionSecret,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::ReadsSecretStore,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["secretstore_lookup"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: SECRETSTORE_GET,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::SecretStoreRequire,
        name: SECRETSTORE_REQUIRE,
        arity: 2,
        signature: IntrinsicSignature::SecretStoreStringToSecret,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::ReadsSecretStore,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["secretstore_lookup"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: SECRETSTORE_REQUIRE,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaItem,
        name: META_ITEM,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.item",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExpr,
        name: META_EXPR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.expr_raw",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaFreshIdent,
        name: META_FRESH_IDENT,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.fresh",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprLeaf,
        name: META_EXPR_LEAF,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta expression builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternLeaf,
        name: META_PATTERN_LEAF,
        arity: 4,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta pattern builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtLeaf,
        name: META_STMT_LEAF,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta statement builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaCallSiteExpr,
        name: META_CALL_SITE_EXPR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.call_site",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaCallSiteType,
        name: META_CALL_SITE_TYPE,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.call_site",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaCallSitePattern,
        name: META_CALL_SITE_PATTERN,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.call_site",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternCtor,
        name: META_PATTERN_CTOR,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.pattern_ctor",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternTuple,
        name: META_PATTERN_TUPLE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.pattern_tuple",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternList,
        name: META_PATTERN_LIST,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.pattern_list",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternListRest,
        name: META_PATTERN_LIST_REST,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.pattern_list_rest",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternOr,
        name: META_PATTERN_OR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.pattern_or",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeNamed,
        name: META_TYPE_NAMED,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_named",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeTuple,
        name: META_TYPE_TUPLE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_tuple",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeFn,
        name: META_TYPE_FN,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_fn",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeQualified,
        name: META_TYPE_QUALIFIED,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_qualified",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeExpr,
        name: META_TYPE_EXPR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_expr",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeCapability,
        name: META_TYPE_CAPABILITY,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.type_capability",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprCall,
        name: META_EXPR_CALL,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.expr_call",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprField,
        name: META_EXPR_FIELD,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.expr_field",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprMatch,
        name: META_EXPR_MATCH,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.expr_match",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtExpr,
        name: META_STMT_EXPR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_expr",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtReturn,
        name: META_STMT_RETURN,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_return",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaMatchArm,
        name: META_MATCH_ARM,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.match_arm",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtLet,
        name: META_STMT_LET,
        arity: 4,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_let",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaBlock,
        name: META_BLOCK,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaParam,
        name: META_PARAM,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.param",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaFunctionBlock,
        name: META_FUNCTION_BLOCK,
        arity: 5,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.function_block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaImplBlock,
        name: META_IMPL_BLOCK,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "meta.impl_block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerFootprint,
        name: COMPILER_FOOTPRINT,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["compiler_footprint"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler.footprint",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerDiff,
        name: COMPILER_DIFF,
        arity: 2,
        signature: IntrinsicSignature::StringStringToString,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["compiler_diff"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler.diff",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerDoc,
        name: COMPILER_DOC,
        arity: 2,
        signature: IntrinsicSignature::StringStringToString,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["compiler_doc"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler.doc",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerDocResultJson,
        name: COMPILER_DOC_RESULT_JSON,
        arity: 2,
        signature: IntrinsicSignature::StringStringToString,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["compiler_doc_result_json"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "compiler doc result encoding",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::RegexMatchSpans,
        name: REGEX_MATCH_SPANS,
        arity: 2,
        signature: IntrinsicSignature::StringStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["regex_match_spans"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: REGEX_MATCH_SPANS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingUtf8Lossy,
        name: ENCODING_UTF8_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(7, WirHostInput::LossyUtf8Bytes),
        diagnostic_name: "lossy UTF-8 decode",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingHexEncode,
        name: ENCODING_HEX_ENCODE,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(0, WirHostInput::String),
        diagnostic_name: "encoding.hex_encode",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingHexEncodeBytes,
        name: ENCODING_HEX_ENCODE_BYTES,
        arity: 1,
        signature: IntrinsicSignature::BytesToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(8, WirHostInput::Bytes),
        diagnostic_name: "encoding.hex_encode_bytes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingHexDecodeLossy,
        name: ENCODING_HEX_DECODE_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(1, WirHostInput::String),
        diagnostic_name: "encoding.hex_decode_lossy",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingHexDecodeBytesRaw,
        name: ENCODING_HEX_DECODE_BYTES_RAW,
        arity: 1,
        signature: IntrinsicSignature::StringToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(11, WirHostInput::String),
        diagnostic_name: "encoding.hex_decode_bytes_raw",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64Encode,
        name: ENCODING_BASE64_ENCODE,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(2, WirHostInput::String),
        diagnostic_name: "encoding.base64_encode",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64EncodeBytes,
        name: ENCODING_BASE64_ENCODE_BYTES,
        arity: 1,
        signature: IntrinsicSignature::BytesToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(9, WirHostInput::Bytes),
        diagnostic_name: "encoding.base64_encode_bytes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64UrlEncodeBytes,
        name: ENCODING_BASE64URL_ENCODE_BYTES,
        arity: 1,
        signature: IntrinsicSignature::BytesToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(10, WirHostInput::Bytes),
        diagnostic_name: "encoding.base64url_encode_bytes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingHexToBase64UrlLossy,
        name: ENCODING_HEX_TO_BASE64URL_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(4, WirHostInput::String),
        diagnostic_name: "encoding.hex_to_base64url_lossy",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64DecodeLossy,
        name: ENCODING_BASE64_DECODE_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(3, WirHostInput::String),
        diagnostic_name: "encoding.base64_decode_lossy",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64DecodeBytesRaw,
        name: ENCODING_BASE64_DECODE_BYTES_RAW,
        arity: 1,
        signature: IntrinsicSignature::StringToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(12, WirHostInput::String),
        diagnostic_name: "encoding.base64_decode_bytes_raw",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64UrlDecodeLossy,
        name: ENCODING_BASE64URL_DECODE_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(5, WirHostInput::String),
        diagnostic_name: "encoding.base64url_decode_lossy",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64UrlDecodeBytesRaw,
        name: ENCODING_BASE64URL_DECODE_BYTES_RAW,
        arity: 1,
        signature: IntrinsicSignature::StringToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(13, WirHostInput::String),
        diagnostic_name: "encoding.base64url_decode_bytes_raw",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::EncodingBase64UrlToHexLossy,
        name: ENCODING_BASE64URL_TO_HEX_LOSSY,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: ENCODING_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: encoding_host_call(6, WirHostInput::String),
        diagnostic_name: "encoding.base64url_to_hex_lossy",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoSha256,
        name: CRYPTO_SHA256,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_sha256"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHA256,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoSha256Bytes,
        name: CRYPTO_SHA256_BYTES,
        arity: 1,
        signature: IntrinsicSignature::BytesToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_sha256_bytes"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHA256_BYTES,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoRuneHash,
        name: CRYPTO_RUNE_HASH,
        arity: 2,
        signature: IntrinsicSignature::ListStringListStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_rune_hash"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_RUNE_HASH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoEd25519VerifyStatus,
        name: CRYPTO_ED25519_VERIFY_STATUS,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_ed25519_verify_status"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_ED25519_VERIFY_STATUS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoSign,
        name: CRYPTO_SIGN,
        arity: 2,
        signature: IntrinsicSignature::SealedSecretStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::UsesSecret,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_sign"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SIGN,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoPublicKey,
        name: CRYPTO_PUBLIC_KEY,
        arity: 1,
        signature: IntrinsicSignature::SealedSecretToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::UsesSecret,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_public_key"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_PUBLIC_KEY,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoReveal,
        name: CRYPTO_REVEAL,
        arity: 1,
        signature: IntrinsicSignature::RevealSecretToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::UsesSecret,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_reveal"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_REVEAL,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoEcdsaP256VerifyStatus,
        name: CRYPTO_ECDSA_P256_VERIFY_STATUS,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_ecdsa_p256_verify_status"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_ECDSA_P256_VERIFY_STATUS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoEcdsaP256VerifyHexStatus,
        name: CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_ecdsa_p256_verify_hex_status"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoRsaPkcs1Sha256VerifyStatus,
        name: CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_rsa_pkcs1_sha256_verify_status"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoSha512,
        name: CRYPTO_SHA512,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_sha512"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHA512,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoSha3_256,
        name: CRYPTO_SHA3_256,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_sha3_256"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHA3_256,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoHmacSha256,
        name: CRYPTO_HMAC_SHA256,
        arity: 2,
        signature: IntrinsicSignature::StringStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_hmac_sha256"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_HMAC_SHA256,
        private_callers: NO_PRIVATE_CALLERS,
    },
    // (RFC-0106) SHAKE128/256 XOF: (Bytes, Int) -> Bytes. Native-only; the browser
    // host omits the imports so a module reaching them cannot instantiate there.
    IntrinsicSpec {
        id: IntrinsicId::CryptoShake128,
        name: CRYPTO_SHAKE128,
        arity: 2,
        signature: IntrinsicSignature::BytesIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_shake128"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHAKE128,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoShake256,
        name: CRYPTO_SHAKE256,
        arity: 2,
        signature: IntrinsicSignature::BytesIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["crypto_shake256"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: CRYPTO_SHAKE256,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringLength,
        name: STRING_LENGTH,
        arity: 1,
        signature: IntrinsicSignature::StringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_LENGTH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringCharCount,
        name: STRING_CHAR_COUNT,
        arity: 1,
        signature: IntrinsicSignature::StringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["char_count"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_CHAR_COUNT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringChars,
        name: STRING_CHARS,
        arity: 1,
        signature: IntrinsicSignature::StringToListString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["str_chars"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_CHARS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringFromCode,
        name: STRING_FROM_CODE,
        arity: 1,
        signature: IntrinsicSignature::IntToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_helpers: &["string_from_code"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_FROM_CODE,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringSplit,
        name: STRING_SPLIT,
        arity: 2,
        signature: IntrinsicSignature::StringStringToListString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["split"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_SPLIT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringContains,
        name: STRING_CONTAINS,
        arity: 2,
        signature: IntrinsicSignature::StringStringToBool,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["find_byte"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_CONTAINS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringStartsWith,
        name: STRING_STARTS_WITH,
        arity: 2,
        signature: IntrinsicSignature::StringStringToBool,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["starts_with"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_STARTS_WITH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringEndsWith,
        name: STRING_ENDS_WITH,
        arity: 2,
        signature: IntrinsicSignature::StringStringToBool,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["ends_with"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_ENDS_WITH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringFind,
        name: STRING_FIND,
        arity: 2,
        signature: IntrinsicSignature::StringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["str_index_of"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_FIND,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringReplace,
        name: STRING_REPLACE,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["replace"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_REPLACE,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringSubstring,
        name: STRING_SUBSTRING,
        arity: 3,
        signature: IntrinsicSignature::StringIntIntToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["str_substring"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_SUBSTRING,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringToUpper,
        name: STRING_TO_UPPER,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["ascii_case"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_TO_UPPER,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringToLower,
        name: STRING_TO_LOWER,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["ascii_case"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_TO_LOWER,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringTrim,
        name: STRING_TRIM,
        arity: 1,
        signature: IntrinsicSignature::StringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["trim"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_TRIM,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringToInt,
        name: STRING_TO_INT,
        arity: 1,
        signature: IntrinsicSignature::StringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["str_to_int"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: STRING_TO_INT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MathToFloat,
        name: MATH_TO_FLOAT,
        arity: 1,
        signature: IntrinsicSignature::IntToFloat,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: MATH_TO_FLOAT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MathToInt,
        name: MATH_TO_INT,
        arity: 1,
        signature: IntrinsicSignature::FloatToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["float_to_int"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: MATH_TO_INT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MathSqrt,
        name: MATH_SQRT,
        arity: 1,
        signature: IntrinsicSignature::FloatToFloat,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: MATH_SQRT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListLength,
        name: LIST_LENGTH,
        arity: 1,
        signature: IntrinsicSignature::GenericListToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_len_view"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: LIST_LENGTH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListAt,
        name: LIST_AT,
        arity: 2,
        signature: IntrinsicSignature::GenericListIndex,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_at", "list_at_view"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: LIST_AT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListPush,
        name: LIST_PUSH,
        arity: 2,
        signature: IntrinsicSignature::GenericListPush,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_push", "list_push_cap"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "list.push",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListSetAt,
        name: LIST_SET_AT,
        arity: 3,
        signature: IntrinsicSignature::GenericListSetAt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_at", "list_set_cap"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "list.set_at",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListConcat,
        name: LIST_CONCAT,
        arity: 2,
        signature: IntrinsicSignature::GenericListConcat,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_concat"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: LIST_CONCAT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListPopExtract,
        name: LIST_POP_EXTRACT,
        arity: 1,
        signature: IntrinsicSignature::GenericListPopExtract,
        effect: IntrinsicEffect::WriteBack,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_pop_extract"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: "list.pop",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ListWithCapacity,
        name: LIST_WITH_CAPACITY,
        arity: 1,
        signature: IntrinsicSignature::GenericListWithCapacity,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["list_with_capacity"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: LIST_WITH_CAPACITY,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictNew,
        name: DICT_NEW,
        arity: 0,
        signature: IntrinsicSignature::GenericDictNew,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_new"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: DICT_NEW,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictInsert,
        name: DICT_INSERT,
        arity: 3,
        signature: IntrinsicSignature::GenericDictInsert,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_insert", "dict_insert_cap"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "dict.insert",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictInsertExtract,
        name: DICT_INSERT_EXTRACT,
        arity: 3,
        signature: IntrinsicSignature::GenericDictInsertExtract,
        effect: IntrinsicEffect::WriteBack,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_insert_extract"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "dict.insert",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictGetOr,
        name: DICT_GET_OR,
        arity: 3,
        signature: IntrinsicSignature::GenericDictGetOr,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_get_or"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: DICT_GET_OR,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictAt,
        name: DICT_AT,
        arity: 2,
        signature: IntrinsicSignature::GenericDictIndex,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_at"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: DICT_AT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictUpdate,
        name: DICT_UPDATE,
        arity: 4,
        signature: IntrinsicSignature::GenericDictUpdate,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_update", "dict_update_cap"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "dict.update",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictContainsKey,
        name: DICT_CONTAINS_KEY,
        arity: 2,
        signature: IntrinsicSignature::GenericDictContainsKey,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_has"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: DICT_CONTAINS_KEY,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictRemove,
        name: DICT_REMOVE,
        arity: 2,
        signature: IntrinsicSignature::GenericDictRemove,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_remove"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "dict.remove",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictRemoveExtract,
        name: DICT_REMOVE_EXTRACT,
        arity: 2,
        signature: IntrinsicSignature::GenericDictRemoveExtract,
        effect: IntrinsicEffect::WriteBack,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_remove_extract"],
        dynamic_wir_helpers: true,
        wir_host_call: None,
        diagnostic_name: "dict.remove",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictKeys,
        name: DICT_KEYS,
        arity: 1,
        signature: IntrinsicSignature::GenericDictKeys,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_keys"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: DICT_KEYS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictValues,
        name: DICT_VALUES,
        arity: 1,
        signature: IntrinsicSignature::GenericDictValues,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_values"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: DICT_VALUES,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictPairs,
        name: DICT_PAIRS,
        arity: 1,
        signature: IntrinsicSignature::GenericDictPairs,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: &["dict_pairs"],
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: DICT_PAIRS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictLength,
        name: DICT_LENGTH,
        arity: 1,
        signature: IntrinsicSignature::GenericDictToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_helpers: NO_HELPERS,
        dynamic_wir_helpers: false,
        wir_host_call: None,
        diagnostic_name: DICT_LENGTH,
        private_callers: NO_PRIVATE_CALLERS,
    },
];

pub const ERASURE_BRIDGES: &[&str] = &[ERASE, UNERASE];
pub const BYTES_BRIDGES: &[&str] = &[
    BYTES_FROM_STRING,
    BYTES_FROM_LIST,
    BYTES_TO_STRING,
    BYTES_LENGTH,
    BYTES_AT,
    BYTES_CONCAT,
    BYTES_SLICE,
];

/// Public or compatibility spellings that share one canonical operation row.
/// Consumers should canonicalize only through this table: synthesized
/// monomorphized names still carry representation information of their own.
pub(crate) const OPERATION_ALIASES: &[(&str, &str)] = &[(BYTES_AT_PUBLIC, BYTES_AT)];

pub const CHANNEL_BRIDGES: &[&str] = &[
    CHANNEL_OPEN,
    CHANNEL_SEND,
    CHANNEL_RECV,
    CHANNEL_SELECT,
];

pub const SECRETSTORE_OPERATIONS: &[&str] = &[SECRETSTORE_GET, SECRETSTORE_REQUIRE];

pub const ENCODING_OPERATIONS: &[&str] = &[
    ENCODING_UTF8_LOSSY,
    ENCODING_HEX_ENCODE,
    ENCODING_HEX_ENCODE_BYTES,
    ENCODING_HEX_DECODE_LOSSY,
    ENCODING_HEX_DECODE_BYTES_RAW,
    ENCODING_BASE64_ENCODE,
    ENCODING_BASE64_ENCODE_BYTES,
    ENCODING_BASE64URL_ENCODE_BYTES,
    ENCODING_HEX_TO_BASE64URL_LOSSY,
    ENCODING_BASE64_DECODE_LOSSY,
    ENCODING_BASE64_DECODE_BYTES_RAW,
    ENCODING_BASE64URL_DECODE_LOSSY,
    ENCODING_BASE64URL_DECODE_BYTES_RAW,
    ENCODING_BASE64URL_TO_HEX_LOSSY,
];

pub const CRYPTO_OPERATIONS: &[&str] = &[
    CRYPTO_SHA256,
    CRYPTO_SHA256_BYTES,
    CRYPTO_RUNE_HASH,
    CRYPTO_ED25519_VERIFY_STATUS,
    CRYPTO_SIGN,
    CRYPTO_PUBLIC_KEY,
    CRYPTO_REVEAL,
    CRYPTO_ECDSA_P256_VERIFY_STATUS,
    CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
    CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
    CRYPTO_SHA512,
    CRYPTO_SHA3_256,
    CRYPTO_HMAC_SHA256,
    CRYPTO_SHAKE128,
    CRYPTO_SHAKE256,
];

pub const REGEX_OPERATIONS: &[&str] = &[REGEX_MATCH_SPANS];

pub const STRING_OPERATIONS: &[&str] = &[
    STRING_LENGTH,
    STRING_CHAR_COUNT,
    STRING_CHARS,
    STRING_FROM_CODE,
    STRING_SPLIT,
    STRING_CONTAINS,
    STRING_STARTS_WITH,
    STRING_ENDS_WITH,
    STRING_FIND,
    STRING_REPLACE,
    STRING_SUBSTRING,
    STRING_TO_UPPER,
    STRING_TO_LOWER,
    STRING_TRIM,
    STRING_TO_INT,
];

pub const MATH_OPERATIONS: &[&str] = &[MATH_TO_FLOAT, MATH_TO_INT, MATH_SQRT];

pub const LIST_OPERATIONS: &[&str] = &[
    LIST_LENGTH,
    LIST_AT,
    LIST_PUSH,
    LIST_SET_AT,
    LIST_CONCAT,
    LIST_POP_EXTRACT,
    LIST_WITH_CAPACITY,
];

pub const DICT_OPERATIONS: &[&str] = &[
    DICT_NEW,
    DICT_INSERT,
    DICT_INSERT_EXTRACT,
    DICT_GET_OR,
    DICT_AT,
    DICT_UPDATE,
    DICT_CONTAINS_KEY,
    DICT_REMOVE,
    DICT_REMOVE_EXTRACT,
    DICT_KEYS,
    DICT_VALUES,
    DICT_PAIRS,
    DICT_LENGTH,
];

// The interpreter consults `lookup` on EVERY call expression (a user-function
// call is a whole-table miss), and typeck/lowering query it per expression —
// a linear scan of the catalog was ~30% of interpreter CPU on call-dense
// programs. One map, built on first use; first-wins on duplicates mirrors the
// old `find`.
static LOOKUP_TABLE: std::sync::OnceLock<foldhash::HashMap<&'static str, &'static IntrinsicSpec>> =
    std::sync::OnceLock::new();

pub fn lookup(name: &str) -> Option<&'static IntrinsicSpec> {
    let table = LOOKUP_TABLE.get_or_init(|| {
        use foldhash::HashMapExt as _;
        let mut table = foldhash::HashMap::with_capacity(ALL.len() + OPERATION_ALIASES.len());
        for spec in ALL {
            table.entry(spec.name).or_insert(spec);
        }
        for (alias, canonical) in OPERATION_ALIASES {
            let spec = *table
                .get(canonical)
                .unwrap_or_else(|| panic!("operation alias `{alias}` targets missing row `{canonical}`"));
            table.entry(alias).or_insert(spec);
        }
        table
    });
    if let Some(spec) = table.get(name) {
        return Some(*spec);
    }
    // A synthesized monomorphized variant (`<base>__<suffix>`) resolves to its
    // base spec.
    if let Some(canonical) = [LIST_POP_EXTRACT, DICT_INSERT_EXTRACT, DICT_REMOVE_EXTRACT]
        .into_iter()
        .find(|base| {
            name.strip_prefix(base)
                .is_some_and(|suffix| suffix.starts_with("__"))
        })
    {
        return table.get(canonical).copied();
    }
    let (owner, bare) = name.rsplit_once('.')?;
    let spec = table.get(bare).copied()?;
    spec.private_callers.contains(&owner).then_some(spec)
}

pub fn canonical_operation_name(name: &str) -> &str {
    OPERATION_ALIASES
        .iter()
        .find_map(|(alias, canonical)| (*alias == name).then_some(*canonical))
        .unwrap_or(name)
}

pub fn arity_diagnostic(spec: &IntrinsicSpec, actual: usize) -> String {
    let noun = if spec.arity == 1 { "argument" } else { "arguments" };
    format!(
        "`{}` expects {} {noun}, got {actual}",
        spec.diagnostic_name, spec.arity
    )
}

pub fn sole_wir_helper(name: &str) -> Option<&'static str> {
    match lookup(name)?.wir_helpers {
        [helper] => Some(*helper),
        _ => None,
    }
}

pub fn declared_wir_helper(name: &str, helper: &str) -> Option<&'static str> {
    lookup(name)?.wir_helpers.iter().copied().find(|declared| *declared == helper)
}

pub fn wir_host_call(name: &str) -> Option<WirHostCall> {
    lookup(name)?.wir_host_call
}

pub fn lookup_wir_host_selector(helper: &str, selector: i32) -> Option<&'static IntrinsicSpec> {
    ALL.iter().find(|spec| {
        spec.wir_host_call
            .is_some_and(|call| call.helper == helper && call.selector == selector)
    })
}

pub(crate) fn is_render(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::GeneratedRender)
}

pub fn is_erasure_bridge(name: &str) -> bool {
    lookup(name).is_some_and(|spec| matches!(spec.id, IntrinsicId::Erase | IntrinsicId::Unerase))
}

pub fn is_bytes_bridge(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::BytesFromString
                | IntrinsicId::BytesFromList
                | IntrinsicId::BytesToString
                | IntrinsicId::BytesLength
                | IntrinsicId::BytesAt
                | IntrinsicId::BytesConcat
                | IntrinsicId::BytesSlice
        )
    })
}

pub(crate) fn is_channel_bridge(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::ChannelOpen
                | IntrinsicId::ChannelSend
                | IntrinsicId::ChannelRecv
                | IntrinsicId::ChannelSelect
        )
    })
}

pub fn is_string_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::StringLength
                | IntrinsicId::StringCharCount
                | IntrinsicId::StringChars
                | IntrinsicId::StringFromCode
                | IntrinsicId::StringSplit
                | IntrinsicId::StringContains
                | IntrinsicId::StringStartsWith
                | IntrinsicId::StringEndsWith
                | IntrinsicId::StringFind
                | IntrinsicId::StringReplace
                | IntrinsicId::StringSubstring
                | IntrinsicId::StringToUpper
                | IntrinsicId::StringToLower
                | IntrinsicId::StringTrim
                | IntrinsicId::StringToInt
        )
    })
}

pub fn is_crypto_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::CryptoSha256
                | IntrinsicId::CryptoSha256Bytes
                | IntrinsicId::CryptoRuneHash
                | IntrinsicId::CryptoEd25519VerifyStatus
                | IntrinsicId::CryptoSign
                | IntrinsicId::CryptoPublicKey
                | IntrinsicId::CryptoReveal
                | IntrinsicId::CryptoEcdsaP256VerifyStatus
                | IntrinsicId::CryptoEcdsaP256VerifyHexStatus
                | IntrinsicId::CryptoRsaPkcs1Sha256VerifyStatus
                | IntrinsicId::CryptoSha512
                | IntrinsicId::CryptoSha3_256
                | IntrinsicId::CryptoHmacSha256
                | IntrinsicId::CryptoShake128
                | IntrinsicId::CryptoShake256
        )
    })
}

pub fn is_regex_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::RegexMatchSpans)
}

pub fn is_math_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(spec.id, IntrinsicId::MathToFloat | IntrinsicId::MathToInt | IntrinsicId::MathSqrt)
    })
}

pub fn is_list_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::ListLength
                | IntrinsicId::ListAt
                | IntrinsicId::ListPush
                | IntrinsicId::ListSetAt
                | IntrinsicId::ListConcat
                | IntrinsicId::ListPopExtract
                | IntrinsicId::ListWithCapacity
        )
    })
}

pub fn is_list_pop_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::ListPopExtract)
}

pub fn is_dict_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::DictNew
                | IntrinsicId::DictInsert
                | IntrinsicId::DictInsertExtract
                | IntrinsicId::DictGetOr
                | IntrinsicId::DictAt
                | IntrinsicId::DictUpdate
                | IntrinsicId::DictContainsKey
                | IntrinsicId::DictRemove
                | IntrinsicId::DictRemoveExtract
                | IntrinsicId::DictKeys
                | IntrinsicId::DictValues
                | IntrinsicId::DictPairs
                | IntrinsicId::DictLength
        )
    })
}

pub fn is_secretstore_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(spec.id, IntrinsicId::SecretStoreGet | IntrinsicId::SecretStoreRequire)
    })
}

pub fn is_dict_insert_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::DictInsertExtract)
}

pub fn is_dict_remove_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::DictRemoveExtract)
}

pub fn is_meta_fresh_ident(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFreshIdent)
}

pub fn is_meta_expr_leaf(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprLeaf)
}

pub fn is_meta_pattern_leaf(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternLeaf)
}

pub fn is_meta_stmt_leaf(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtLeaf)
}

pub fn is_meta_call_site_expr(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaCallSiteExpr)
}

pub fn is_meta_call_site_type(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaCallSiteType)
}

pub fn is_meta_call_site_pattern(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaCallSitePattern)
}

pub fn is_meta_pattern_ctor(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternCtor)
}

pub fn is_meta_pattern_tuple(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternTuple)
}

pub fn is_meta_pattern_list(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternList)
}

pub fn is_meta_pattern_list_rest(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternListRest)
}

pub fn is_meta_pattern_or(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternOr)
}

pub fn is_meta_type_named(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeNamed)
}

pub fn is_meta_type_tuple(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeTuple)
}

pub fn is_meta_type_fn(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeFn)
}

pub fn is_meta_type_qualified(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeQualified)
}

pub fn is_meta_type_expr(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeExpr)
}

pub fn is_meta_type_capability(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeCapability)
}

pub fn is_meta_expr_call(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprCall)
}

pub fn is_meta_expr_field(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprField)
}

pub fn is_meta_expr_match(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprMatch)
}

pub fn is_meta_match_arm(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaMatchArm)
}

pub fn is_meta_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaBlock)
}

pub fn is_meta_stmt_expr(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtExpr)
}

pub fn is_meta_item(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaItem)
}

pub fn is_meta_expr(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExpr)
}

pub fn is_meta_stmt_return(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtReturn)
}

pub fn is_meta_stmt_let(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtLet)
}

pub fn is_meta_param(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaParam)
}

pub fn is_meta_function_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFunctionBlock)
}

pub fn is_meta_impl_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaImplBlock)
}

pub(crate) fn private_intrinsic_callers(bare_name: &str) -> Option<&'static [&'static str]> {
    if canonical_operation_name(bare_name) != bare_name {
        return None;
    }
    let callers = lookup(bare_name)?.private_callers;
    (!callers.is_empty()).then_some(callers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn catalog_names_and_ids_are_unique() {
        let mut names = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for spec in ALL {
            assert!(names.insert(spec.name), "duplicate intrinsic name {}", spec.name);
            assert!(ids.insert(format!("{:?}", spec.id)), "duplicate intrinsic id {:?}", spec.id);
            assert!(!spec.diagnostic_name.is_empty());
        }
    }

    #[test]
    fn bridge_catalog_has_expected_privacy_owners() {
        for name in ERASURE_BRIDGES.iter().chain(CHANNEL_BRIDGES.iter()) {
            assert_eq!(private_intrinsic_callers(name), Some(MESSAGE_BRIDGE_CALLERS));
        }
        for name in BYTES_BRIDGES {
            assert_eq!(private_intrinsic_callers(name), Some(BYTES_BRIDGE_CALLERS));
        }
        assert_eq!(private_intrinsic_callers(META_ITEM), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_EXPR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_FRESH_IDENT), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_EXPR_LEAF), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_LEAF), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_STMT_LEAF), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_CALL_SITE_EXPR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_CALL_SITE_TYPE), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_CALL_SITE_PATTERN), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_CTOR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_TUPLE), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_LIST), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_LIST_REST), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PATTERN_OR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_NAMED), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_TUPLE), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_FN), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_QUALIFIED), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_EXPR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_TYPE_CAPABILITY), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_EXPR_CALL), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_EXPR_FIELD), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_EXPR_MATCH), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_MATCH_ARM), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_BLOCK), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_STMT_EXPR), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_STMT_RETURN), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_STMT_LET), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_PARAM), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_FUNCTION_BLOCK), Some(META_BRIDGE_CALLERS));
        assert_eq!(private_intrinsic_callers(META_IMPL_BLOCK), Some(META_BRIDGE_CALLERS));
        assert_eq!(lookup("meta.__meta_item"), lookup(META_ITEM));
        assert_eq!(lookup("meta.__meta_expr"), lookup(META_EXPR));
        assert_eq!(lookup("meta.__meta_fresh_ident"), lookup(META_FRESH_IDENT));
        assert_eq!(lookup("meta.__meta_expr_leaf"), lookup(META_EXPR_LEAF));
        assert_eq!(lookup("meta.__meta_pattern_leaf"), lookup(META_PATTERN_LEAF));
        assert_eq!(lookup("meta.__meta_stmt_leaf"), lookup(META_STMT_LEAF));
        assert_eq!(lookup("meta.__meta_call_site_expr"), lookup(META_CALL_SITE_EXPR));
        assert_eq!(lookup("meta.__meta_call_site_type"), lookup(META_CALL_SITE_TYPE));
        assert_eq!(
            lookup("meta.__meta_call_site_pattern"),
            lookup(META_CALL_SITE_PATTERN)
        );
        assert_eq!(lookup("meta.__meta_pattern_ctor"), lookup(META_PATTERN_CTOR));
        assert_eq!(lookup("meta.__meta_pattern_tuple"), lookup(META_PATTERN_TUPLE));
        assert_eq!(lookup("meta.__meta_pattern_list"), lookup(META_PATTERN_LIST));
        assert_eq!(lookup("meta.__meta_pattern_list_rest"), lookup(META_PATTERN_LIST_REST));
        assert_eq!(lookup("meta.__meta_pattern_or"), lookup(META_PATTERN_OR));
        assert_eq!(lookup("meta.__meta_type_named"), lookup(META_TYPE_NAMED));
        assert_eq!(lookup("meta.__meta_type_tuple"), lookup(META_TYPE_TUPLE));
        assert_eq!(lookup("meta.__meta_type_fn"), lookup(META_TYPE_FN));
        assert_eq!(lookup("meta.__meta_type_qualified"), lookup(META_TYPE_QUALIFIED));
        assert_eq!(lookup("meta.__meta_type_expr"), lookup(META_TYPE_EXPR));
        assert_eq!(lookup("meta.__meta_type_capability"), lookup(META_TYPE_CAPABILITY));
        assert_eq!(lookup("meta.__meta_expr_call"), lookup(META_EXPR_CALL));
        assert_eq!(lookup("meta.__meta_expr_field"), lookup(META_EXPR_FIELD));
        assert_eq!(lookup("meta.__meta_expr_match"), lookup(META_EXPR_MATCH));
        assert_eq!(lookup("meta.__meta_match_arm"), lookup(META_MATCH_ARM));
        assert_eq!(lookup("meta.__meta_block"), lookup(META_BLOCK));
        assert_eq!(lookup("meta.__meta_stmt_expr"), lookup(META_STMT_EXPR));
        assert_eq!(lookup("meta.__meta_stmt_return"), lookup(META_STMT_RETURN));
        assert_eq!(lookup("meta.__meta_stmt_let"), lookup(META_STMT_LET));
        assert_eq!(lookup("meta.__meta_param"), lookup(META_PARAM));
        assert_eq!(lookup("meta.__meta_function_block"), lookup(META_FUNCTION_BLOCK));
        assert_eq!(lookup("meta.__meta_impl_block"), lookup(META_IMPL_BLOCK));
        assert_eq!(lookup("other.__meta_fresh_ident"), None);
    }

    #[test]
    fn public_constants_resolve_to_catalog_entries() {
        let names = [
            GENERATED_RENDER,
            GENERATED_LIST_PUSH,
            COMPILER_QUOTE_ITEM,
            COMPILER_QUOTE_ITEM_HOLES,
            COMPILER_QUOTE_EXPR,
            COMPILER_QUOTE_EXPR_HOLES,
            COMPILER_QUOTE_TYPE,
            COMPILER_QUOTE_PATTERN,
            COMPILER_QUOTE_STMT,
            COMPILER_QUOTE_BLOCK,
            COMPILER_EMIT_ITEM,
            COMPILER_EMIT_EXPR,
            TRY_CONTEXT,
            ERASE,
            UNERASE,
            BYTES_FROM_STRING,
            BYTES_FROM_LIST,
            BYTES_TO_STRING,
            BYTES_LENGTH,
            BYTES_AT,
            BYTES_CONCAT,
            BYTES_SLICE,
            CHANNEL_OPEN,
            CHANNEL_SEND,
            CHANNEL_RECV,
            CHANNEL_SELECT,
            META_ITEM,
            META_EXPR,
            META_FRESH_IDENT,
            META_EXPR_LEAF,
            META_PATTERN_LEAF,
            META_STMT_LEAF,
            META_CALL_SITE_EXPR,
            META_CALL_SITE_TYPE,
            META_CALL_SITE_PATTERN,
            META_PATTERN_CTOR,
            META_PATTERN_TUPLE,
            META_PATTERN_LIST,
            META_PATTERN_LIST_REST,
            META_PATTERN_OR,
            META_TYPE_NAMED,
            META_TYPE_TUPLE,
            META_TYPE_FN,
            META_TYPE_QUALIFIED,
            META_TYPE_EXPR,
            META_TYPE_CAPABILITY,
            META_EXPR_CALL,
            META_EXPR_FIELD,
            META_EXPR_MATCH,
            META_MATCH_ARM,
            META_BLOCK,
            META_STMT_EXPR,
            META_STMT_RETURN,
            META_STMT_LET,
            META_PARAM,
            META_FUNCTION_BLOCK,
            META_IMPL_BLOCK,
            COMPILER_FOOTPRINT,
            COMPILER_DIFF,
            COMPILER_DOC,
            COMPILER_DOC_RESULT_JSON,
            REGEX_MATCH_SPANS,
            ENCODING_UTF8_LOSSY,
            ENCODING_HEX_ENCODE,
            ENCODING_HEX_ENCODE_BYTES,
            ENCODING_HEX_DECODE_LOSSY,
            ENCODING_HEX_DECODE_BYTES_RAW,
            ENCODING_BASE64_ENCODE,
            ENCODING_BASE64_ENCODE_BYTES,
            ENCODING_BASE64URL_ENCODE_BYTES,
            ENCODING_HEX_TO_BASE64URL_LOSSY,
            ENCODING_BASE64_DECODE_LOSSY,
            ENCODING_BASE64_DECODE_BYTES_RAW,
            ENCODING_BASE64URL_DECODE_LOSSY,
            ENCODING_BASE64URL_DECODE_BYTES_RAW,
            ENCODING_BASE64URL_TO_HEX_LOSSY,
            CRYPTO_SHA256,
            CRYPTO_SHA256_BYTES,
            CRYPTO_RUNE_HASH,
            CRYPTO_ED25519_VERIFY_STATUS,
            CRYPTO_SIGN,
            CRYPTO_PUBLIC_KEY,
            CRYPTO_REVEAL,
            CRYPTO_ECDSA_P256_VERIFY_STATUS,
            CRYPTO_ECDSA_P256_VERIFY_HEX_STATUS,
            CRYPTO_RSA_PKCS1_SHA256_VERIFY_STATUS,
            CRYPTO_SHA512,
            CRYPTO_SHA3_256,
            CRYPTO_HMAC_SHA256,
            STRING_LENGTH,
            STRING_CHAR_COUNT,
            STRING_CHARS,
            STRING_FROM_CODE,
            STRING_SPLIT,
            STRING_CONTAINS,
            STRING_STARTS_WITH,
            STRING_ENDS_WITH,
            STRING_FIND,
            STRING_REPLACE,
            STRING_SUBSTRING,
            STRING_TO_UPPER,
            STRING_TO_LOWER,
            STRING_TRIM,
            STRING_TO_INT,
            MATH_TO_FLOAT,
            MATH_TO_INT,
            MATH_SQRT,
            LIST_LENGTH,
            LIST_AT,
            LIST_PUSH,
            LIST_SET_AT,
            LIST_CONCAT,
            LIST_POP_EXTRACT,
            DICT_NEW,
            DICT_INSERT,
            DICT_INSERT_EXTRACT,
            DICT_GET_OR,
            DICT_AT,
            DICT_UPDATE,
            DICT_CONTAINS_KEY,
            DICT_REMOVE,
            DICT_REMOVE_EXTRACT,
            DICT_KEYS,
            DICT_VALUES,
            DICT_PAIRS,
            DICT_LENGTH,
        ];
        for name in names {
            assert_eq!(lookup(name).map(|spec| spec.name), Some(name));
        }
    }

    #[test]
    fn operation_aliases_are_unique_and_resolve_to_canonical_rows() {
        let mut aliases = BTreeSet::new();
        for (alias, canonical) in OPERATION_ALIASES {
            assert!(aliases.insert(*alias), "duplicate operation alias {alias}");
            assert_ne!(alias, canonical, "operation alias must use a distinct spelling");
            assert_eq!(canonical_operation_name(alias), *canonical);
            assert_eq!(lookup(alias), lookup(canonical));
            assert!(ALL.iter().all(|spec| spec.name != *alias), "alias {alias} owns a second row");
        }
        assert_eq!(canonical_operation_name("user.function"), "user.function");
        assert_eq!(private_intrinsic_callers(BYTES_AT_PUBLIC), None);
    }

    #[test]
    fn diagnostics_use_catalog_names_and_arities() {
        let at = lookup(BYTES_AT).expect("bytes.at intrinsic");
        assert_eq!(arity_diagnostic(at, 1), "`bytes.at` expects 2 arguments, got 1");

        let from_string = lookup(BYTES_FROM_STRING).expect("bytes.from_string intrinsic");
        assert_eq!(
            arity_diagnostic(from_string, 0),
            "`bytes.from_string` expects 1 argument, got 0"
        );
    }

    #[test]
    fn bytes_operation_family_has_complete_semantic_metadata() {
        let expected_helpers = [
            (BYTES_FROM_STRING, None, IntrinsicLowering::Identity),
            (BYTES_FROM_LIST, Some("bytes_from_list"), IntrinsicLowering::Builtin),
            (BYTES_TO_STRING, Some("bytes_to_string"), IntrinsicLowering::Builtin),
            (BYTES_LENGTH, None, IntrinsicLowering::Builtin),
            (BYTES_AT, Some("bytes_at"), IntrinsicLowering::Builtin),
            (BYTES_CONCAT, Some("concat"), IntrinsicLowering::Builtin),
            (BYTES_SLICE, Some("bytes_slice"), IntrinsicLowering::Builtin),
        ];
        let expected: BTreeSet<_> = BYTES_BRIDGES.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_bytes_bridge(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);

        for (name, helper, lowering) in expected_helpers {
            let spec = lookup(name).expect("bytes operation");
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.capability_effect, CapabilityEffect::None);
            assert_eq!(spec.runtime, IntrinsicRuntime::InterpreterBuiltin);
            assert_eq!(spec.lowering, lowering);
            assert_eq!(spec.private_callers, BYTES_BRIDGE_CALLERS);
            assert!(!spec.dynamic_wir_helpers);
            assert!(spec.wir_host_call.is_none());
            assert_eq!(sole_wir_helper(name), helper);
        }

        assert_eq!(lookup(BYTES_AT_PUBLIC), lookup(BYTES_AT));
        assert_eq!(lookup(BYTES_AT_PUBLIC).map(|spec| spec.arity), Some(2));
    }

    #[test]
    fn bytes_at_public_alias_signature_matches_source() {
        use crate::ast::{Item, Type};

        let module = crate::parser::parse_module(include_str!("../../../std/bytes.witchy"))
            .expect("parse std/bytes");
        let function = module.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == "at" => Some(function),
            _ => None,
        });
        let function = function.expect("bytes.at source function");
        let bytes = Type::Named("Bytes".into(), Vec::new());
        let int = Type::Named("Int".into(), Vec::new());
        assert_eq!(function.params.len(), lookup(BYTES_AT_PUBLIC).expect("alias row").arity);
        assert_eq!(function.params[0].ty.as_ref(), Some(&bytes));
        assert_eq!(function.params[1].ty.as_ref(), Some(&int));
        assert_eq!(function.ret.as_ref(), Some(&int));
    }

    #[test]
    fn secretstore_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = SECRETSTORE_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_secretstore_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 2);

        for name in SECRETSTORE_OPERATIONS {
            let spec = lookup(name).expect("SecretStore operation");
            assert_eq!(spec.arity, 2);
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.capability_effect, CapabilityEffect::ReadsSecretStore);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime, IntrinsicRuntime::InterpreterBuiltin);
            assert_eq!(sole_wir_helper(name), Some("secretstore_lookup"));
            assert!(!spec.dynamic_wir_helpers);
            assert!(spec.wir_host_call.is_none());
            assert_eq!(spec.diagnostic_name, *name);
            assert!(spec.private_callers.is_empty());
        }
        assert_eq!(
            lookup(SECRETSTORE_GET).expect("secretstore.get").signature,
            IntrinsicSignature::SecretStoreStringToOptionSecret
        );
        assert_eq!(
            lookup(SECRETSTORE_REQUIRE).expect("secretstore.require").signature,
            IntrinsicSignature::SecretStoreStringToSecret
        );
        assert_eq!(
            arity_diagnostic(lookup(SECRETSTORE_REQUIRE).expect("secretstore.require"), 1),
            "`secretstore.require` expects 2 arguments, got 1"
        );
    }

    #[test]
    fn secretstore_source_signatures_match_catalog() {
        use crate::ast::{Item, Type};

        let module = crate::parser::parse_module(include_str!("../../../std/secretstore.witchy"))
            .expect("parse std/secretstore");
        let store = Type::Named("SecretStore".into(), Vec::new());
        let string = Type::Named("String".into(), Vec::new());
        let secret = Type::Named("Secret".into(), Vec::new());

        for name in SECRETSTORE_OPERATIONS {
            let spec = lookup(name).expect("SecretStore operation");
            let bare_name = name.rsplit_once('.').expect("qualified SecretStore name").1;
            let function = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(function) if function.name == bare_name => Some(function),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} missing from std/secretstore"));
            assert_eq!(function.params.len(), spec.arity, "arity drift for {name}");
            assert_eq!(
                function.params.iter().map(|param| param.ty.as_ref()).collect::<Vec<_>>(),
                vec![Some(&store), Some(&string)],
                "parameter drift for {name}"
            );
            let expected_result = match spec.signature {
                IntrinsicSignature::SecretStoreStringToOptionSecret => {
                    Type::Named("Option".into(), vec![secret.clone()])
                }
                IntrinsicSignature::SecretStoreStringToSecret => secret.clone(),
                other => panic!("unexpected SecretStore signature {other:?}"),
            };
            assert_eq!(function.ret.as_ref(), Some(&expected_result), "return drift for {name}");
        }
    }

    #[test]
    fn compiler_operation_family_has_native_and_wir_hooks() {
        for name in [
            COMPILER_FOOTPRINT,
            COMPILER_DIFF,
            COMPILER_DOC,
            COMPILER_DOC_RESULT_JSON,
        ] {
            let spec = lookup(name).expect("compiler operation");
            assert_eq!(spec.effect, IntrinsicEffect::Toolchain);
            assert_eq!(spec.runtime, IntrinsicRuntime::Native);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert!(spec.signature.returns_string());
            assert!(sole_wir_helper(name).is_some());
        }
    }

    #[test]
    fn encoding_operation_family_has_unique_host_selectors() {
        let mut selectors = BTreeSet::new();
        for name in ENCODING_OPERATIONS {
            let spec = lookup(name).expect("encoding operation");
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.runtime, IntrinsicRuntime::Native);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.arity, 1);
            let call = spec.wir_host_call.expect("encoding WIR host call");
            assert_eq!(call.helper, "encoding");
            assert!(selectors.insert(call.selector), "duplicate encoding selector {}", call.selector);
            assert_eq!(lookup_wir_host_selector(call.helper, call.selector), Some(spec));
            assert!(spec.signature.returns_string() || spec.signature.returns_bytes());
        }
        assert_eq!(selectors, (0..=13).collect());
    }

    #[test]
    fn regex_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = REGEX_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_regex_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 1);

        let spec = lookup(REGEX_MATCH_SPANS).expect("regex operation");
        assert_eq!(spec.arity, 2);
        assert_eq!(spec.signature, IntrinsicSignature::StringStringToString);
        assert_eq!(spec.effect, IntrinsicEffect::Pure);
        assert_eq!(spec.capability_effect, CapabilityEffect::None);
        assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
        assert_eq!(spec.runtime, IntrinsicRuntime::Native);
        assert_eq!(sole_wir_helper(spec.name), Some("regex_match_spans"));
        assert!(!spec.dynamic_wir_helpers);
        assert!(spec.wir_host_call.is_none());
    }

    #[test]
    fn regex_source_primitive_signature_matches_catalog() {
        use crate::ast::{Item, Type};

        let module = crate::parser::parse_module(include_str!("../../../std/regex.witchy"))
            .expect("parse std/regex");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "match_spans" => Some(function),
                _ => None,
            })
            .expect("regex.match_spans missing from std/regex");
        let string = Type::Named("String".into(), Vec::new());
        let spec = lookup(REGEX_MATCH_SPANS).expect("regex operation");

        assert_eq!(function.params.len(), spec.arity);
        assert_eq!(
            function
                .params
                .iter()
                .map(|param| param.ty.as_ref())
                .collect::<Vec<_>>(),
            vec![Some(&string), Some(&string)]
        );
        assert_eq!(function.ret.as_ref(), Some(&string));
    }

    #[test]
    fn crypto_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = CRYPTO_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_crypto_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 15);

        for name in CRYPTO_OPERATIONS {
            let spec = lookup(name).expect("crypto operation");
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime, IntrinsicRuntime::Native);
            assert_eq!(spec.wir_helpers.len(), 1);
            assert!(!spec.dynamic_wir_helpers);
            assert!(spec.wir_host_call.is_none());
            assert_eq!(
                spec.capability_effect,
                if matches!(*name, CRYPTO_SIGN | CRYPTO_PUBLIC_KEY | CRYPTO_REVEAL) {
                    CapabilityEffect::UsesSecret
                } else {
                    CapabilityEffect::None
                }
            );
        }
    }

    #[test]
    fn crypto_source_primitive_signatures_match_catalog() {
        use crate::ast::{Item, Type};

        fn named(name: &str) -> Type {
            Type::Named(name.into(), Vec::new())
        }

        /// (RFC-0121) A rights-bearing `Secret[...]` receiver, so `std/crypto`'s
        /// source and the catalog agree on the right each primitive requires.
        fn secret_with(right: &str) -> Type {
            Type::Named("Secret".into(), vec![named(right)])
        }

        fn expected(signature: IntrinsicSignature) -> (Vec<Type>, Type) {
            match signature {
                IntrinsicSignature::StringToString => {
                    (vec![named("String")], named("String"))
                }
                IntrinsicSignature::StringStringToString => (
                    vec![named("String"), named("String")],
                    named("String"),
                ),
                IntrinsicSignature::StringStringStringToInt => (
                    vec![named("String"), named("String"), named("String")],
                    named("Int"),
                ),
                IntrinsicSignature::ListStringListStringToString => (
                    vec![
                        Type::Named("List".into(), vec![named("String")]),
                        Type::Named("List".into(), vec![named("String")]),
                    ],
                    named("String"),
                ),
                // (RFC-0121) The by-handle ops declare the narrowed receiver;
                // `reveal` declares the right it actually needs.
                IntrinsicSignature::SealedSecretStringToString => (
                    vec![secret_with("Seal"), named("String")],
                    named("String"),
                ),
                IntrinsicSignature::SealedSecretToString => {
                    (vec![secret_with("Seal")], named("String"))
                }
                IntrinsicSignature::RevealSecretToString => {
                    (vec![secret_with("Reveal")], named("String"))
                }
                // (RFC-0106) SHAKE XOFs: __shake128/__shake256(Bytes, Int) -> Bytes.
                IntrinsicSignature::BytesIntToBytes => {
                    (vec![named("Bytes"), named("Int")], named("Bytes"))
                }
                // (RFC-0095) crypto.sha256_bytes(Bytes) -> String: hashes raw
                // decoded bytes (the trusted-exe artifact), not a UTF-8 String.
                IntrinsicSignature::BytesToString => {
                    (vec![named("Bytes")], named("String"))
                }
                other => panic!("unexpected crypto signature {other:?}"),
            }
        }

        let module = crate::parser::parse_module(include_str!("../../../std/crypto.witchy"))
            .expect("parse std/crypto");
        for name in CRYPTO_OPERATIONS {
            let spec = lookup(name).expect("crypto operation");
            let bare_name = spec.name.rsplit_once('.').expect("qualified crypto name").1;
            let function = module.items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == bare_name => Some(function),
                _ => None,
            });
            let function =
                function.unwrap_or_else(|| panic!("{} missing from std/crypto", spec.name));
            let (params, result) = expected(spec.signature);
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(
                function
                    .params
                    .iter()
                    .map(|param| param.ty.as_ref())
                    .collect::<Vec<_>>(),
                params.iter().map(Some).collect::<Vec<_>>(),
                "parameter drift for {}",
                spec.name
            );
            assert_eq!(function.ret.as_ref(), Some(&result), "return drift for {}", spec.name);
        }
    }

    #[test]
    fn string_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = STRING_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_string_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 15);

        for name in STRING_OPERATIONS {
            let spec = lookup(name).expect("string operation");
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.capability_effect, CapabilityEffect::None);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime == IntrinsicRuntime::Native, *name == STRING_FROM_CODE);
            assert!(spec.wir_host_call.is_none());
            assert!(!spec.dynamic_wir_helpers);
        }
        assert!(lookup(STRING_LENGTH).expect("string.length").wir_helpers.is_empty());
    }

    #[test]
    fn string_source_primitive_signatures_match_catalog() {
        use crate::ast::Type;

        fn named(name: &str) -> Type {
            Type::Named(name.into(), Vec::new())
        }

        fn expected(signature: IntrinsicSignature) -> (Vec<Type>, Type) {
            let string = || named("String");
            let int = || named("Int");
            let bool_ = || named("Bool");
            let list_string = || Type::Named("List".into(), vec![string()]);
            match signature {
                IntrinsicSignature::StringToString => (vec![string()], string()),
                IntrinsicSignature::StringToInt => (vec![string()], int()),
                IntrinsicSignature::StringStringToInt => {
                    (vec![string(), string()], int())
                }
                IntrinsicSignature::StringStringToBool => {
                    (vec![string(), string()], bool_())
                }
                IntrinsicSignature::StringToListString => {
                    (vec![string()], list_string())
                }
                IntrinsicSignature::StringStringToListString => {
                    (vec![string(), string()], list_string())
                }
                IntrinsicSignature::StringStringStringToString => {
                    (vec![string(), string(), string()], string())
                }
                IntrinsicSignature::StringIntIntToString => {
                    (vec![string(), int(), int()], string())
                }
                IntrinsicSignature::IntToString => (vec![int()], string()),
                other => panic!("unexpected string signature {other:?}"),
            }
        }

        let module = crate::parser::parse_module(include_str!("../../../std/string.witchy"))
            .expect("parse std/string");
        for name in STRING_OPERATIONS {
            let spec = lookup(name).expect("string operation");
            let bare_name = spec.name.rsplit_once('.').expect("qualified string name").1;
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => {
                    Some(function)
                }
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/string", spec.name));
            let (params, result) = expected(spec.signature);
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(
                function.params.iter().map(|param| param.ty.clone()).collect::<Vec<_>>(),
                params.into_iter().map(Some).collect::<Vec<_>>(),
                "parameter drift for {}",
                spec.name
            );
            assert_eq!(function.ret.as_ref(), Some(&result), "return drift for {}", spec.name);
        }
    }

    #[test]
    fn math_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = MATH_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_math_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 3);

        for name in MATH_OPERATIONS {
            let spec = lookup(name).expect("math operation");
            assert_eq!(spec.arity, 1);
            assert_eq!(spec.effect, IntrinsicEffect::Pure);
            assert_eq!(spec.capability_effect, CapabilityEffect::None);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime, IntrinsicRuntime::InterpreterBuiltin);
            assert!(spec.wir_host_call.is_none());
            assert!(!spec.dynamic_wir_helpers);
        }
        assert_eq!(sole_wir_helper(MATH_TO_INT), Some("float_to_int"));
        assert!(lookup(MATH_TO_FLOAT).expect("math.to_float").wir_helpers.is_empty());
        assert!(lookup(MATH_SQRT).expect("math.sqrt").wir_helpers.is_empty());
    }

    #[test]
    fn math_source_primitive_signatures_match_catalog() {
        use crate::ast::Type;

        let module = crate::parser::parse_module(include_str!("../../../std/math.witchy"))
            .expect("parse std/math");
        let int = Type::Named("Int".into(), Vec::new());
        let float = Type::Named("Float".into(), Vec::new());
        for name in MATH_OPERATIONS {
            let spec = lookup(name).expect("math operation");
            let bare_name = spec.name.rsplit_once('.').expect("qualified math name").1;
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => {
                    Some(function)
                }
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/math", spec.name));
            let (param, result) = match spec.signature {
                IntrinsicSignature::IntToFloat => (&int, &float),
                IntrinsicSignature::FloatToInt => (&float, &int),
                IntrinsicSignature::FloatToFloat => (&float, &float),
                other => panic!("unexpected math signature {other:?}"),
            };
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(function.params[0].ty.as_ref(), Some(param), "parameter drift for {}", spec.name);
            assert_eq!(function.ret.as_ref(), Some(result), "return drift for {}", spec.name);
        }
    }

    #[test]
    fn list_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = LIST_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_list_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 7);

        for name in LIST_OPERATIONS {
            let spec = lookup(name).expect("list operation");
            assert_eq!(spec.capability_effect, CapabilityEffect::None);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime, IntrinsicRuntime::InterpreterBuiltin);
            assert!(spec.wir_host_call.is_none());
            assert!(!spec.dynamic_wir_helpers);
            assert_eq!(
                spec.effect,
                if *name == LIST_POP_EXTRACT {
                    IntrinsicEffect::WriteBack
                } else {
                    IntrinsicEffect::Pure
                }
            );
        }

        assert!(lookup(LIST_LENGTH).expect("list.length").signature.returns_int());
        assert!(lookup(LIST_AT).expect("list.at").signature.returns_list_element());
        for name in [LIST_PUSH, LIST_SET_AT, LIST_CONCAT, LIST_WITH_CAPACITY] {
            assert!(lookup(name).expect("list-producing operation").signature.returns_list());
        }
        assert_eq!(declared_wir_helper(LIST_PUSH, "list_push"), Some("list_push"));
        assert_eq!(declared_wir_helper(LIST_PUSH, "list_push_cap"), Some("list_push_cap"));
        assert_eq!(sole_wir_helper(LIST_CONCAT), Some("list_concat"));
        assert_eq!(sole_wir_helper(LIST_WITH_CAPACITY), Some("list_with_capacity"));
        assert_eq!(sole_wir_helper(LIST_POP_EXTRACT), Some("list_pop_extract"));
        assert_eq!(
            lookup("list.__pop_extract__String").map(|spec| spec.name),
            Some(LIST_POP_EXTRACT)
        );
        assert_eq!(declared_wir_helper(LIST_AT, "list_at_view"), Some("list_at_view"));
        assert_eq!(declared_wir_helper(LIST_SET_AT, "list_set_cap"), Some("list_set_cap"));
    }

    #[test]
    fn list_source_primitive_signatures_match_catalog() {
        use crate::ast::{Convention, Type};

        fn named(name: &str) -> Type {
            Type::Named(name.into(), Vec::new())
        }

        fn expected(signature: IntrinsicSignature) -> (Vec<Type>, Type) {
            let elem = || named("a");
            let list = || Type::Named("List".into(), vec![elem()]);
            match signature {
                IntrinsicSignature::GenericListToInt => (vec![list()], named("Int")),
                IntrinsicSignature::GenericListIndex => (vec![list(), named("Int")], elem()),
                IntrinsicSignature::GenericListPush => {
                    (vec![list(), elem()], list())
                }
                IntrinsicSignature::GenericListSetAt => {
                    (vec![list(), named("Int"), elem()], list())
                }
                IntrinsicSignature::GenericListConcat => {
                    (vec![list(), list()], list())
                }
                IntrinsicSignature::GenericListPopExtract => {
                    (vec![list()], Type::Named("Option".into(), vec![elem()]))
                }
                IntrinsicSignature::GenericListWithCapacity => (vec![named("Int")], list()),
                other => panic!("unexpected list signature {other:?}"),
            }
        }

        let module = crate::parser::parse_module(include_str!("../../../std/list.witchy"))
            .expect("parse std/list");
        let source_primitives: BTreeSet<_> = module
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::Function(function)
                    if matches!(
                        function.body.stmts.as_slice(),
                        [crate::ast::Stmt::Expr(crate::ast::Expr::Call { name, .. })]
                            if name.strip_prefix("list.") == Some(function.name.as_str())
                    ) =>
                {
                    let crate::ast::Stmt::Expr(crate::ast::Expr::Call { name, .. }) =
                        &function.body.stmts[0]
                    else {
                        unreachable!()
                    };
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        let catalog_primitives: BTreeSet<_> =
            LIST_OPERATIONS.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            source_primitives, catalog_primitives,
            "every self-recursive list primitive must have exactly one catalog row"
        );
        for name in LIST_OPERATIONS {
            let spec = lookup(name).expect("list operation");
            let bare_name = spec.name.rsplit_once('.').expect("qualified list name").1;
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => {
                    Some(function)
                }
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/list", spec.name));
            let (params, result) = expected(spec.signature);
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(
                function.params.iter().map(|param| param.ty.clone()).collect::<Vec<_>>(),
                params.into_iter().map(Some).collect::<Vec<_>>(),
                "parameter drift for {}",
                spec.name
            );
            assert_eq!(function.ret.as_ref(), Some(&result), "return drift for {}", spec.name);
            assert_eq!(
                function.params[0].convention,
                if *name == LIST_POP_EXTRACT { Convention::Var } else { Convention::Let },
                "receiver convention drift for {}",
                spec.name
            );
        }
    }

    #[test]
    fn dict_operation_family_has_complete_semantic_metadata() {
        let expected: BTreeSet<_> = DICT_OPERATIONS.iter().copied().collect();
        let actual: BTreeSet<_> = ALL
            .iter()
            .filter(|spec| is_dict_operation(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 13);

        for name in DICT_OPERATIONS {
            let spec = lookup(name).expect("dict operation");
            assert_eq!(spec.capability_effect, CapabilityEffect::None);
            assert_eq!(spec.lowering, IntrinsicLowering::Builtin);
            assert_eq!(spec.runtime, IntrinsicRuntime::InterpreterBuiltin);
            assert!(spec.wir_host_call.is_none());
            assert_eq!(spec.dynamic_wir_helpers, !spec.signature.trait_bounds().is_empty());
            assert_eq!(
                spec.effect,
                if matches!(*name, DICT_INSERT_EXTRACT | DICT_REMOVE_EXTRACT) {
                    IntrinsicEffect::WriteBack
                } else {
                    IntrinsicEffect::Pure
                }
            );
        }

        assert!(lookup(DICT_LENGTH).expect("dict.length").signature.returns_int());
        assert!(lookup(DICT_CONTAINS_KEY).expect("dict.contains_key").signature.returns_bool());
        for name in [DICT_NEW, DICT_INSERT, DICT_UPDATE, DICT_REMOVE] {
            assert!(lookup(name).expect("dict-producing operation").signature.returns_dict());
        }
        for name in [DICT_GET_OR, DICT_AT] {
            assert!(lookup(name).expect("dict value read").signature.returns_dict_value());
        }
        assert_eq!(declared_wir_helper(DICT_INSERT, "dict_insert"), Some("dict_insert"));
        assert_eq!(declared_wir_helper(DICT_INSERT, "dict_insert_cap"), Some("dict_insert_cap"));
        assert_eq!(declared_wir_helper(DICT_UPDATE, "dict_update"), Some("dict_update"));
        assert_eq!(declared_wir_helper(DICT_UPDATE, "dict_update_cap"), Some("dict_update_cap"));
        assert_eq!(
            lookup("dict.__insert_extract__String__Int").map(|spec| spec.name),
            Some(DICT_INSERT_EXTRACT)
        );
        assert_eq!(
            lookup("dict.__remove_extract__String__Int").map(|spec| spec.name),
            Some(DICT_REMOVE_EXTRACT)
        );
    }

    #[test]
    fn dict_source_primitive_signatures_match_catalog() {
        use crate::ast::{Convention, Type, TypeQual};

        fn named(name: &str) -> Type {
            Type::Named(name.into(), Vec::new())
        }

        fn expected(signature: IntrinsicSignature) -> (Vec<Type>, Type) {
            let key = || named("k");
            let value = || named("v");
            let dict = || Type::Named("Dict".into(), vec![key(), value()]);
            match signature {
                IntrinsicSignature::GenericDictNew => (vec![], dict()),
                IntrinsicSignature::GenericDictInsert => {
                    (vec![dict(), key(), value()], dict())
                }
                IntrinsicSignature::GenericDictInsertExtract => (
                    vec![dict(), key(), value()],
                    Type::Named("Option".into(), vec![value()]),
                ),
                IntrinsicSignature::GenericDictGetOr => {
                    (vec![dict(), key(), value()], value())
                }
                IntrinsicSignature::GenericDictIndex => (vec![dict(), key()], value()),
                IntrinsicSignature::GenericDictUpdate => (
                    vec![
                        dict(),
                        key(),
                        value(),
                        Type::Fn(
                            vec![value()],
                            Box::new(value()),
                            vec![Convention::Let],
                        ),
                    ],
                    dict(),
                ),
                IntrinsicSignature::GenericDictContainsKey => {
                    (vec![dict(), key()], named("Bool"))
                }
                IntrinsicSignature::GenericDictRemove => (vec![dict(), key()], dict()),
                IntrinsicSignature::GenericDictRemoveExtract => (
                    vec![dict(), key()],
                    Type::Named("Option".into(), vec![value()]),
                ),
                IntrinsicSignature::GenericDictKeys => (
                    vec![dict()],
                    Type::Named("List".into(), vec![key()]),
                ),
                IntrinsicSignature::GenericDictValues => (
                    vec![dict()],
                    Type::Named("List".into(), vec![value()]),
                ),
                IntrinsicSignature::GenericDictPairs => (
                    vec![dict()],
                    Type::Named("List".into(), vec![Type::Tuple(vec![key(), value()])]),
                ),
                IntrinsicSignature::GenericDictToInt => (vec![dict()], named("Int")),
                other => panic!("unexpected dict signature {other:?}"),
            }
        }

        let module = crate::parser::parse_module(include_str!("../../../std/dict.witchy"))
            .expect("parse std/dict");
        let source_primitives: BTreeSet<_> = module
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::Function(function)
                    if matches!(
                        function.body.stmts.as_slice(),
                        [crate::ast::Stmt::Expr(crate::ast::Expr::Call { name, .. })]
                            if name.strip_prefix("dict.") == Some(function.name.as_str())
                    ) =>
                {
                    let crate::ast::Stmt::Expr(crate::ast::Expr::Call { name, .. }) =
                        &function.body.stmts[0]
                    else {
                        unreachable!()
                    };
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        let catalog_primitives: BTreeSet<_> =
            DICT_OPERATIONS.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            source_primitives, catalog_primitives,
            "every self-recursive dict primitive must have exactly one catalog row"
        );

        for name in DICT_OPERATIONS {
            let spec = lookup(name).expect("dict operation");
            let bare_name = spec.name.rsplit_once('.').expect("qualified dict name").1;
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => {
                    Some(function)
                }
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/dict", spec.name));
            let (params, result) = expected(spec.signature);
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(
                function
                    .params
                    .iter()
                    .map(|param| param.ty.as_ref().map(Type::unqualified))
                    .collect::<Vec<_>>(),
                params.iter().map(Type::unqualified).map(Some).collect::<Vec<_>>(),
                "parameter drift for {}",
                spec.name
            );
            assert_eq!(function.ret.as_ref(), Some(&result), "return drift for {}", spec.name);
            assert_eq!(
                function.params.first().map(|param| param.convention),
                (!function.params.is_empty()).then_some(
                    if matches!(*name, DICT_INSERT_EXTRACT | DICT_REMOVE_EXTRACT) {
                        Convention::Var
                    } else {
                        Convention::Let
                    }
                ),
                "receiver convention drift for {}",
                spec.name
            );
            let source_unique: Vec<_> = function
                .params
                .iter()
                .enumerate()
                .filter_map(|(index, param)| {
                    matches!(param.ty, Some(Type::Qualified(TypeQual::Unique, _)))
                        .then_some(index)
                })
                .collect();
            assert_eq!(source_unique, spec.signature.unique_parameters());
            let source_bounds: Vec<_> = function
                .bounds
                .iter()
                .map(|(parameter, trait_name, args)| {
                    (parameter.as_str(), trait_name.as_str(), args.as_slice())
                })
                .collect();
            let expected_bounds: Vec<_> = spec
                .signature
                .trait_bounds()
                .iter()
                .map(|bound| ("k", bound.trait_name, &[][..]))
                .collect();
            assert_eq!(source_bounds, expected_bounds, "trait-bound drift for {}", spec.name);
        }
    }

    #[test]
    fn source_function_arities_match_task_module() {
        let module = crate::parser::parse_module(include_str!("../../../std/task.witchy"))
            .expect("parse std/task");
        for spec in ALL.iter().filter(|spec| spec.runtime == IntrinsicRuntime::SourceFunction) {
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == spec.name => Some(function),
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/task", spec.name));
            assert_eq!(
                function.params.len(),
                spec.arity,
                "catalog arity drifted from std/task for {}",
                spec.name
            );
        }
    }

    #[test]
    fn compiler_native_source_placeholders_match_catalog_signatures() {
        let module = crate::parser::parse_module(include_str!("../../../std/compiler.witchy"))
            .expect("parse std/compiler");
        let string = crate::ast::Type::Named("String".into(), Vec::new());
        for spec in ALL.iter().filter(|spec| {
            spec.runtime == IntrinsicRuntime::Native && spec.name.starts_with("compiler.")
        }) {
            let bare_name = spec.name.rsplit_once('.').map_or(spec.name, |(_, bare)| bare);
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => {
                    Some(function)
                }
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/compiler", spec.name));
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert!(
                function.params.iter().all(|param| param.ty.as_ref() == Some(&string)),
                "parameter type drift for {}",
                spec.name
            );
            assert_eq!(function.ret.as_ref(), Some(&string), "return type drift for {}", spec.name);
        }
    }

    #[test]
    fn encoding_native_source_signatures_match_catalog() {
        let module = crate::parser::parse_module(include_str!("../../../std/encoding.witchy"))
            .expect("parse std/encoding");
        let string = crate::ast::Type::Named("String".into(), Vec::new());
        let bytes = crate::ast::Type::Named("Bytes".into(), Vec::new());
        for spec in ALL.iter().filter(|spec| {
            spec.runtime == IntrinsicRuntime::Native
                && spec.name.starts_with("encoding.")
                && spec.name != ENCODING_UTF8_LOSSY
        }) {
            let bare_name = spec.name.rsplit_once('.').map_or(spec.name, |(_, bare)| bare);
            let function = module.items.iter().find_map(|item| match item {
                crate::ast::Item::Function(function) if function.name == bare_name => Some(function),
                _ => None,
            });
            let function = function.unwrap_or_else(|| panic!("{} missing from std/encoding", spec.name));
            let (param, result) = match spec.signature {
                IntrinsicSignature::StringToString => (&string, &string),
                IntrinsicSignature::StringToBytes => (&string, &bytes),
                IntrinsicSignature::BytesToString => (&bytes, &string),
                other => panic!("unexpected encoding signature {other:?} for {}", spec.name),
            };
            assert_eq!(function.params.len(), spec.arity, "arity drift for {}", spec.name);
            assert_eq!(function.params[0].ty.as_ref(), Some(param), "parameter drift for {}", spec.name);
            assert_eq!(function.ret.as_ref(), Some(result), "return drift for {}", spec.name);
        }
    }

    #[test]
    fn generated_frontend_intrinsics_are_not_std_bridges() {
        assert_eq!(private_intrinsic_callers(GENERATED_RENDER), None);
        assert_eq!(private_intrinsic_callers(GENERATED_LIST_PUSH), None);
        assert_eq!(private_intrinsic_callers(COMPILER_QUOTE_ITEM), None);
        assert_eq!(private_intrinsic_callers(COMPILER_EMIT_ITEM), None);
        assert_eq!(private_intrinsic_callers(RETIRED_SOURCE_RENDER), None);
        assert_eq!(private_intrinsic_callers(TRY_CONTEXT), None);
        assert_eq!(private_intrinsic_callers(COMPILER_DOC_RESULT_JSON), None);
    }
}
