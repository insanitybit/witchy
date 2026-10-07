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
    BytesWithCapacity,
    BytesRepeat,
    BytesPush,
    BytesAppend,
    BytesReserve,
    BytesSetAt,
    BytesSwap,
    ChannelOpen,
    ChannelSend,
    ChannelRecv,
    ChannelSelect,
    ChannelSelect2,
    SecretStoreGet,
    SecretStoreRequire,
    MetaItemWithImports,
    MetaFreshIdent,
    MetaExprLeaf,
    MetaExprComposeWire,
    MetaExprTagHole,
    MetaExprTagged,
    MetaPatternLeaf,
    MetaStmtLeaf,
    MetaCallSiteExpr,
    MetaCallSiteType,
    MetaCallSiteTypeWire,
    MetaCallSitePattern,
    MetaPatternCtor,
    MetaPatternTuple,
    MetaPatternList,
    MetaPatternListRest,
    MetaPatternOr,
    MetaTypeNamed,
    MetaTypeNamedWire,
    MetaRecordTypeWire,
    MetaTypeTuple,
    MetaTypeFn,
    MetaTypeQualified,
    MetaTypeExpr,
    MetaTypeExprWire,
    MetaTypeCapability,
    MetaExprCall,
    MetaExprNamedCall,
    MetaExprTry,
    MetaExprLambda,
    MetaExprMethodCall,
    MetaExprQualifiedCall,
    MetaExprQualifiedCallPath,
    MetaExprDynamicDescriptor,
    MetaExprField,
    MetaExprIndex,
    MetaExprBinary,
    MetaExprUnary,
    MetaExprRange,
    MetaExprList,
    MetaExprTuple,
    MetaExprRecord,
    MetaExprMatch,
    MetaExprBlock,
    MetaExprIf,
    MetaExprFor,
    MetaExprWhile,
    MetaExprWhileLet,
    MetaExprAwait,
    MetaMatchArm,
    MetaBlock,
    MetaBlockRegion,
    MetaStmtExpr,
    MetaStmtReturn,
    MetaStmtLet,
    MetaStmtLetPattern,
    MetaStmtAssign,
    MetaStmtYield,
    MetaStatementWire,
    MetaPatternWire,
    MetaMatchArmWire,
    MetaBlockWire,
    MetaParam,
    MetaParamWire,
    MetaFunctionBlock,
    MetaFunctionExprWire,
    MetaFunctionBlockWire,
    MetaImplBlock,
    MetaInherentImpl,
    MetaInherentImplWire,
    MetaImplBlockWhere,
    MetaImplBlockWhereWire,
    CompilerFootprint,
    CompilerDiff,
    CompilerDoc,
    CompilerDocResultJson,
    RegexMatchSpans,
    RegexCount,
    RegexReplaceAll,
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
    StringCharOffset,
    StringChars,
    StringFromCode,
    StringSplit,
    StringContains,
    StringStartsWith,
    StringEndsWith,
    StringFind,
    StringReplace,
    StringSubstring,
    StringAsStr,
    StringSlice,
    StringToString,
    StringLen,
    StringHashView,
    StringEqualViews,
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
    DictWithCapacity,
    DictInsert,
    DictInsertExtract,
    DictInsertValueExtract,
    DictGetOr,
    DictAt,
    DictUpdate,
    DictContainsKey,
    DictContainsBy,
    DictRemove,
    DictRemoveExtract,
    DictPopExtract,
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
    IntToBytes,
    IntIntToBytes,
    SecretStoreStringToOptionSecret,
    SecretStoreStringToSecret,
    StringToString,
    StringStringToString,
    StringToInt,
    StringStringToInt,
    StringIntToInt,
    StringStringIntToInt,
    StringStringToBool,
    StringToListString,
    StringStringToListString,
    StringStringStringToString,
    StringStringStringToInt,
    StringIntIntToString,
    StringToStr,
    StrIntIntToStr,
    StrToString,
    StrToInt,
    StrStrToBool,
    ListStringListStringToString,
    /// (RFC-0121) A by-handle secret op: asks only for `Seal`, so a `Secret[Seal]`
    /// satisfies it and a bare `Secret` narrows into it (`crypto.sign`).
    SealedSecretStringToString,
    /// (RFC-0121) A by-handle secret op of arity one (`crypto.public_key`).
    SealedSecretToString,
    /// (RFC-0121) Reading a secret's bytes: needs the `Reveal` right, so a
    /// `Secret[Seal]` is a check-time error (`crypto.reveal`).
    /// `Secret[Reveal] -> Result(String, CapError)` (RFC-0126 rule 3).
    RevealSecret,
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
    GenericDictWithCapacity,
    GenericDictInsert,
    GenericDictInsertExtract,
    GenericDictInsertValueExtract,
    GenericDictGetOr,
    GenericDictIndex,
    GenericDictUpdate,
    GenericDictContainsKey,
    GenericDictContainsBy,
    GenericDictRemove,
    GenericDictRemoveExtract,
    GenericDictPopExtract,
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

const DICT_KEY_HASH_BOUND: &[IntrinsicTraitBound] = &[IntrinsicTraitBound {
    parameter: 1,
    trait_name: "Hash",
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
                | Self::IntToString
        )
    }

    pub fn returns_int(self) -> bool {
        matches!(
            self,
            Self::BytesToInt
                | Self::BytesIntToInt
                | Self::StringToInt
                | Self::StrToInt
                | Self::StringStringToInt
                | Self::StringIntToInt
                | Self::StringStringIntToInt
                | Self::StringStringStringToInt
                | Self::FloatToInt
                | Self::GenericListToInt
                | Self::GenericDictToInt
        )
    }

    pub fn returns_bool(self) -> bool {
        matches!(self, Self::StringStringToBool | Self::GenericDictContainsKey | Self::GenericDictContainsBy)
    }

    pub fn returns_bytes(self) -> bool {
        matches!(
            self,
            Self::StringToBytes
                | Self::ListIntToBytes
                | Self::BytesBytesToBytes
                | Self::BytesIntIntToBytes
                | Self::BytesIntToBytes
                | Self::IntToBytes
                | Self::IntIntToBytes
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

    /// The root-preserving slice carrier supports only a length query.  Element
    /// extraction would return arbitrary `a` by value and can therefore
    /// duplicate affine or must-consume payloads; `&items[i]` is the explicit
    /// rooted-view spelling for that operation instead.  Mutators remain
    /// List-only: a view never grants write access to its retained root.
    pub fn accepts_rooted_slice_view(self) -> bool {
        matches!(self, Self::GenericListToInt)
    }

    pub fn returns_dict(self) -> bool {
        matches!(
            self,
            Self::GenericDictNew
                | Self::GenericDictWithCapacity
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
            | Self::GenericDictInsertValueExtract
            | Self::GenericDictGetOr
            | Self::GenericDictIndex
            | Self::GenericDictUpdate
            | Self::GenericDictContainsKey
            | Self::GenericDictRemove
            | Self::GenericDictRemoveExtract => DICT_KEY_HASH_BOUND,
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
    /// Runtime type reflection or dynamic dispatch. These operations
    /// may recover behavior whose callable contract is not statically visible.
    Dynamic,
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

/// (RFC-0151) Validation requirements for contiguous scalar append operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarValidation {
    None,
    ByteRange,
    Utf8Scalar,
}

/// (RFC-0151) Compiler intrinsic operation classification for contiguous buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContiguousOpClass {
    ContiguousRead { width: u32 },
    ContiguousWrite { width: u32 },
    AppendScalar { width: u32, validation: ScalarValidation },
    AppendSlice { width: u32 },
    Reserve,
    Finish,
}

impl IntrinsicId {
    pub fn contiguous_op(self) -> Option<ContiguousOpClass> {
        match self {
            Self::BytesAt => Some(ContiguousOpClass::ContiguousRead { width: 1 }),
            Self::BytesSetAt => Some(ContiguousOpClass::ContiguousWrite { width: 1 }),
            Self::BytesPush => Some(ContiguousOpClass::AppendScalar {
                width: 1,
                validation: ScalarValidation::ByteRange,
            }),
            Self::BytesAppend => Some(ContiguousOpClass::AppendSlice { width: 1 }),
            Self::BytesReserve => Some(ContiguousOpClass::Reserve),
            Self::BytesToString => Some(ContiguousOpClass::Finish),
            _ => None,
        }
    }
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
    /// Selector metadata when multiple operations share one WIR host helper.
    pub wir_host_call: Option<WirHostCall>,
    pub diagnostic_name: &'static str,
    pub private_callers: &'static [&'static str],
}

impl IntrinsicSpec {
    pub fn contiguous_op(&self) -> Option<ContiguousOpClass> {
        self.id.contiguous_op()
    }
}

const NO_PRIVATE_CALLERS: &[&str] = &[];
const MESSAGE_BRIDGE_CALLERS: &[&str] = &["chan", "task"];
// These are ordinary std source functions, not new backend intrinsics. Their
// checked source declarations remain the typing/lowering contract; only call
// authority belongs here. Raw scheduler IDs and unbound message decoders must
// not let external callers forge authority or bypass typed endpoint agreement.
const PRIVATE_SCHEDULER_SOURCE_BRIDGES: &[&str] = &[
    "task.__spawn_raw",
    "task.__join_raw",
    "task.__cancel_raw",
    "task.__handle_from_raw",
    "task.__handle_into_raw",
    "task.__channel_send_i64",
    "task.__channel_recv_i64",
    "task.__channel_select2_map",
    "task.channel_receive_frame",
    "task.__prepare_send",
    "task.__producer_frame",
    "chan.select2_frame_payload",
    "chan.select2_frame_decode",
    "chan.select3_frame_payload",
    "chan.select3_frame_decode",
    "chan.select4_frame_payload",
    "chan.select4_frame_decode",
];
const DYNAMIC_BRIDGE_CALLERS: &[&str] = &["dynamic"];
const BYTES_BRIDGE_CALLERS: &[&str] = &["bytes"];
const META_BRIDGE_CALLERS: &[&str] = &["meta"];

const fn encoding_host_call(selector: i32, input: WirHostInput) -> Option<WirHostCall> {
    Some(WirHostCall { helper: "encoding", selector, input })
}

pub const GENERATED_RENDER: &str = "@render";
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

impl IntrinsicSignature {
    /// Parser-issued quotes carry an explicit final source-line operand.
    /// Payload handles may deduplicate identical syntax from different sites;
    /// evaluator control flow must never select a quotation's definition span.
    pub fn has_quote_site(self) -> bool {
        matches!(self, Self::CompilerQuoteItem | Self::CompilerQuoteItemHoles
            | Self::CompilerQuoteExpr | Self::CompilerQuoteExprHoles
            | Self::CompilerQuoteType | Self::CompilerQuoteTypeHoles
            | Self::CompilerQuotePattern | Self::CompilerQuotePatternHoles
            | Self::CompilerQuoteStmt | Self::CompilerQuoteStmtHoles
            | Self::CompilerQuoteBlock | Self::CompilerQuoteBlockHoles)
    }
}
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
pub const BYTES_WITH_CAPACITY: &str = "__bytes_with_capacity";
pub const BYTES_REPEAT: &str = "__bytes_repeat";
pub const BYTES_PUSH: &str = "__bytes_push";
pub const BYTES_APPEND: &str = "__bytes_append";
pub const BYTES_RESERVE: &str = "__bytes_reserve";
pub const BYTES_SET_AT: &str = "__bytes_set_at";
pub const BYTES_SWAP: &str = "__bytes_swap";

pub(crate) const CHANNEL_OPEN: &str = "__channel_open";
pub(crate) const CHANNEL_SEND: &str = "__channel_send";
pub(crate) const CHANNEL_RECV: &str = "__channel_recv";
pub(crate) const CHANNEL_SELECT: &str = "__channel_select";
pub(crate) const CHANNEL_SELECT2: &str = "__channel_select2";

pub const SECRETSTORE_GET: &str = "secretstore.get";
pub const SECRETSTORE_REQUIRE: &str = "secretstore.require";
pub(crate) const META_ITEM_WITH_IMPORTS: &str = "__meta_item_with_imports";
pub(crate) const META_FRESH_IDENT: &str = "__meta_fresh_ident";
pub(crate) const META_EXPR_LEAF: &str = "__meta_expr_leaf";
pub(crate) const META_EXPR_COMPOSE_WIRE: &str = "__meta_expr_compose_wire";
pub(crate) const META_EXPR_TAG_HOLE: &str = "__meta_expr_tag_hole";
pub(crate) const META_EXPR_TAGGED: &str = "__meta_expr_tagged";
pub(crate) const META_PATTERN_LEAF: &str = "__meta_pattern_leaf";
pub(crate) const META_STMT_LEAF: &str = "__meta_stmt_leaf";
pub(crate) const META_CALL_SITE_EXPR: &str = "__meta_call_site_expr";
pub(crate) const META_CALL_SITE_TYPE: &str = "__meta_call_site_type";
pub(crate) const META_CALL_SITE_TYPE_WIRE: &str = "__meta_call_site_type_wire";
pub(crate) const META_CALL_SITE_PATTERN: &str = "__meta_call_site_pattern";
pub(crate) const META_PATTERN_CTOR: &str = "__meta_pattern_ctor";
pub(crate) const META_PATTERN_TUPLE: &str = "__meta_pattern_tuple";
pub(crate) const META_PATTERN_LIST: &str = "__meta_pattern_list";
pub(crate) const META_PATTERN_LIST_REST: &str = "__meta_pattern_list_rest";
pub(crate) const META_PATTERN_OR: &str = "__meta_pattern_or";
pub(crate) const META_TYPE_NAMED: &str = "__meta_type_named";
pub(crate) const META_TYPE_NAMED_WIRE: &str = "__meta_type_named_wire";
pub(crate) const META_RECORD_TYPE_WIRE: &str = "__meta_record_type_wire";
pub fn is_meta_record_type_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaRecordTypeWire)
}
pub(crate) const META_TYPE_TUPLE: &str = "__meta_type_tuple";
pub(crate) const META_TYPE_FN: &str = "__meta_type_fn";
pub(crate) const META_TYPE_QUALIFIED: &str = "__meta_type_qualified";
pub(crate) const META_TYPE_EXPR: &str = "__meta_type_expr";
pub(crate) const META_TYPE_EXPR_WIRE: &str = "__meta_type_expr_wire";
pub(crate) const META_TYPE_CAPABILITY: &str = "__meta_type_capability";
pub(crate) const META_EXPR_CALL: &str = "__meta_expr_call";
pub(crate) const META_EXPR_NAMED_CALL: &str = "__meta_expr_named_call";
pub(crate) const META_EXPR_TRY: &str = "__meta_expr_try";
pub(crate) const META_EXPR_LAMBDA: &str = "__meta_expr_lambda";
pub(crate) const META_EXPR_METHOD_CALL: &str = "__meta_expr_method_call";
pub(crate) const META_EXPR_QUALIFIED_CALL: &str = "__meta_expr_qualified_call";
pub(crate) const META_EXPR_QUALIFIED_CALL_PATH: &str = "__meta_expr_qualified_call_path";
pub(crate) const META_EXPR_DYNAMIC_DESCRIPTOR: &str = "__meta_expr_dynamic_descriptor";
pub(crate) const META_EXPR_FIELD: &str = "__meta_expr_field";
pub(crate) const META_EXPR_INDEX: &str = "__meta_expr_index";
pub(crate) const META_EXPR_BINARY: &str = "__meta_expr_binary";
pub(crate) const META_EXPR_UNARY: &str = "__meta_expr_unary";
pub(crate) const META_EXPR_RANGE: &str = "__meta_expr_range";
pub(crate) const META_EXPR_LIST: &str = "__meta_expr_list";
pub(crate) const META_EXPR_TUPLE: &str = "__meta_expr_tuple";
pub(crate) const META_EXPR_RECORD: &str = "__meta_expr_record";
pub(crate) const META_EXPR_MATCH: &str = "__meta_expr_match";
pub(crate) const META_EXPR_BLOCK: &str = "__meta_expr_block";
pub(crate) const META_EXPR_IF: &str = "__meta_expr_if";
pub(crate) const META_EXPR_FOR: &str = "__meta_expr_for";
pub(crate) const META_EXPR_WHILE: &str = "__meta_expr_while";
pub(crate) const META_EXPR_WHILE_LET: &str = "__meta_expr_while_let";
pub(crate) const META_EXPR_AWAIT: &str = "__meta_expr_await";
pub(crate) const META_MATCH_ARM: &str = "__meta_match_arm";
pub(crate) const META_BLOCK: &str = "__meta_block";
pub(crate) const META_BLOCK_REGION: &str = "__meta_block_region";
pub(crate) const META_STMT_EXPR: &str = "__meta_stmt_expr";
pub(crate) const META_STMT_RETURN: &str = "__meta_stmt_return";
pub(crate) const META_STMT_LET: &str = "__meta_stmt_let";
pub(crate) const META_STMT_LET_PATTERN: &str = "__meta_stmt_let_pattern";
pub(crate) const META_STMT_ASSIGN: &str = "__meta_stmt_assign";
pub(crate) const META_STMT_YIELD: &str = "__meta_stmt_yield";
pub(crate) const META_STATEMENT_WIRE: &str = "__meta_statement_wire";
pub(crate) const META_PATTERN_WIRE: &str = "__meta_pattern_wire";
pub(crate) const META_MATCH_ARM_WIRE: &str = "__meta_match_arm_wire";
pub(crate) const META_BLOCK_WIRE: &str = "__meta_block_wire";
pub(crate) const META_PARAM: &str = "__meta_param";
pub(crate) const META_PARAM_WIRE: &str = "__meta_param_wire";
pub(crate) const META_FUNCTION_BLOCK: &str = "__meta_function_block";
pub(crate) const META_FUNCTION_EXPR_WIRE: &str = "__meta_function_expr_wire";
pub(crate) const META_FUNCTION_BLOCK_WIRE: &str = "__meta_function_block_wire";
pub(crate) const META_IMPL_BLOCK: &str = "__meta_impl_block";
pub(crate) const META_INHERENT_IMPL: &str = "__meta_inherent_impl";
pub(crate) const META_INHERENT_IMPL_WIRE: &str = "__meta_inherent_impl_wire";
pub(crate) const META_IMPL_BLOCK_WHERE: &str = "__meta_impl_block_where";
pub(crate) const META_IMPL_BLOCK_WHERE_WIRE: &str = "__meta_impl_block_where_wire";

pub const COMPILER_FOOTPRINT: &str = "compiler.footprint";
pub const COMPILER_DIFF: &str = "compiler.diff";
pub const COMPILER_DOC: &str = "compiler.doc";
pub const COMPILER_DOC_RESULT_JSON: &str = "compiler.__doc_result_json";

pub const REGEX_MATCH_SPANS: &str = "regex.match_spans";
pub const REGEX_COUNT: &str = "regex.count";
pub const REGEX_REPLACE_ALL: &str = "regex.replace_all";

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
pub const STRING_CHAR_OFFSET: &str = "string.char_offset";
pub const STRING_CHARS: &str = "string.chars";
pub const STRING_FROM_CODE: &str = "string.from_code";
pub const STRING_SPLIT: &str = "string.split";
pub const STRING_CONTAINS: &str = "string.contains";
pub const STRING_STARTS_WITH: &str = "string.starts_with";
pub const STRING_ENDS_WITH: &str = "string.ends_with";
pub const STRING_FIND: &str = "string.find";
pub const STRING_REPLACE: &str = "string.replace";
pub const STRING_SUBSTRING: &str = "string.substring";
pub const STRING_AS_STR: &str = "string.as_str";
pub const STRING_SLICE: &str = "string.slice";
pub const STRING_TO_STRING: &str = "string.to_string";
pub const STRING_LEN: &str = "string.len";
pub const STRING_HASH_VIEW: &str = "string.hash_view";
pub const STRING_EQUAL_VIEWS: &str = "string.equal_views";
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
pub const DICT_WITH_CAPACITY: &str = "dict.with_capacity";
pub const DICT_INSERT: &str = "dict.__insert";
pub const DICT_INSERT_EXTRACT: &str = "dict.__insert_extract";
pub const DICT_INSERT_VALUE_EXTRACT: &str = "dict.__insert_value_extract";
pub const DICT_GET_OR: &str = "dict.get_or";
pub const DICT_AT: &str = "dict.at";
pub const DICT_UPDATE: &str = "dict.__update";
pub const DICT_CONTAINS_KEY: &str = "dict.contains_key";
pub const DICT_CONTAINS_BY: &str = "dict.__contains_by";
pub const DICT_REMOVE: &str = "dict.__remove";
pub const DICT_REMOVE_EXTRACT: &str = "dict.__remove_extract";
pub const DICT_POP_EXTRACT: &str = "dict.__pop_extract";
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
        wir_host_call: None,
        diagnostic_name: "list.push",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteItem,
        name: COMPILER_QUOTE_ITEM,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteItem,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned item quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteItemHoles,
        name: COMPILER_QUOTE_ITEM_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuoteItemHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned item quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteExpr,
        name: COMPILER_QUOTE_EXPR,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteExpr,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned expression quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteExprHoles,
        name: COMPILER_QUOTE_EXPR_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuoteExprHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned expression quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteType,
        name: COMPILER_QUOTE_TYPE,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteType,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned type quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteTypeHoles,
        name: COMPILER_QUOTE_TYPE_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuoteTypeHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned type quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuotePattern,
        name: COMPILER_QUOTE_PATTERN,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuotePattern,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned pattern quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuotePatternHoles,
        name: COMPILER_QUOTE_PATTERN_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuotePatternHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned pattern quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteStmt,
        name: COMPILER_QUOTE_STMT,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteStmt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned statement quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteStmtHoles,
        name: COMPILER_QUOTE_STMT_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuoteStmtHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned statement quotation with holes",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteBlock,
        name: COMPILER_QUOTE_BLOCK,
        arity: 3,
        signature: IntrinsicSignature::CompilerQuoteBlock,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "compiler-owned block quotation",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CompilerQuoteBlockHoles,
        name: COMPILER_QUOTE_BLOCK_HOLES,
        arity: 4,
        signature: IntrinsicSignature::CompilerQuoteBlockHoles,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "message recovery",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicRuntimeType,
        name: DYNAMIC_RUNTIME_TYPE,
        arity: 2,
        signature: IntrinsicSignature::StringStringToRuntimeType,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "runtime type descriptor",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicDescriptor,
        name: DYNAMIC_DESCRIPTOR,
        arity: 1,
        signature: IntrinsicSignature::GenericToRuntimeType,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic descriptor construction",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicDescriptorId,
        name: DYNAMIC_DESCRIPTOR_ID,
        arity: 1,
        signature: IntrinsicSignature::GenericToInt,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic descriptor identity",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicFields,
        name: DYNAMIC_FIELDS,
        arity: 1,
        signature: IntrinsicSignature::RuntimeTypeToListRuntimeField,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic field enumeration",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicFieldStatus,
        name: DYNAMIC_FIELD_STATUS,
        arity: 2,
        signature: IntrinsicSignature::RuntimeTypeStringToDynamicFieldStatus,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic field lookup",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicMethods,
        name: DYNAMIC_METHODS,
        arity: 1,
        signature: IntrinsicSignature::RuntimeTypeToListRuntimeMethod,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic method enumeration",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicCall,
        name: DYNAMIC_CALL,
        arity: 3,
        signature: IntrinsicSignature::DynamicStringListDynamicToResultDynamicDynamicError,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic method call",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicCallWith,
        name: DYNAMIC_CALL_WITH,
        arity: 4,
        signature: IntrinsicSignature::DynamicStringListDynamicGenericToResultDynamicDynamicError,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic method call with explicit capabilities",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicImplements,
        name: DYNAMIC_IMPLEMENTS,
        arity: 2,
        signature: IntrinsicSignature::DynamicRuntimeTypeToBool,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic trait membership query",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicAsTrait,
        name: DYNAMIC_AS_TRAIT,
        arity: 2,
        signature: IntrinsicSignature::DynamicRuntimeTypeToResultDynamicDynamicError,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic checked trait view",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicTryDecode,
        name: DYNAMIC_TRY_DECODE,
        arity: 1,
        signature: IntrinsicSignature::DynamicToOptionGeneric,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "Dynamic decoding",
        private_callers: DYNAMIC_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DynamicTryDecodeTyped,
        name: DYNAMIC_TRY_DECODE_TYPED,
        arity: 2,
        signature: IntrinsicSignature::DynamicIntToOptionGeneric,
        effect: IntrinsicEffect::Dynamic,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "bytes.slice",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesWithCapacity,
        name: BYTES_WITH_CAPACITY,
        arity: 1,
        signature: IntrinsicSignature::IntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.with_capacity",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesRepeat,
        name: BYTES_REPEAT,
        arity: 2,
        signature: IntrinsicSignature::IntIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.repeat",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesPush,
        name: BYTES_PUSH,
        arity: 2,
        signature: IntrinsicSignature::BytesIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.push",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesAppend,
        name: BYTES_APPEND,
        arity: 2,
        signature: IntrinsicSignature::BytesBytesToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.append",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesReserve,
        name: BYTES_RESERVE,
        arity: 2,
        signature: IntrinsicSignature::BytesIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.reserve",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesSetAt,
        name: BYTES_SET_AT,
        arity: 3,
        signature: IntrinsicSignature::BytesIntIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.set_at",
        private_callers: BYTES_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::BytesSwap,
        name: BYTES_SWAP,
        arity: 3,
        signature: IntrinsicSignature::BytesIntIntToBytes,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "bytes.swap",
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
        wir_host_call: None,
        diagnostic_name: "channel select",
        private_callers: MESSAGE_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::ChannelSelect2,
        name: CHANNEL_SELECT2,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Task,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::SourceFunction,
        wir_host_call: None,
        diagnostic_name: "channel select 2",
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
        wir_host_call: None,
        diagnostic_name: SECRETSTORE_REQUIRE,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaItemWithImports,
        name: META_ITEM_WITH_IMPORTS,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.item_with_imports",
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
        wir_host_call: None,
        diagnostic_name: "meta expression builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprComposeWire,
        name: META_EXPR_COMPOSE_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta expression composition",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprTagHole,
        name: META_EXPR_TAG_HOLE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_tag_hole_typed",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprTagged,
        name: META_EXPR_TAGGED,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_tagged",
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
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.call_site",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaCallSiteTypeWire,
        name: META_CALL_SITE_TYPE_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.type_named",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeNamedWire,
        name: META_TYPE_NAMED_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "meta.type_tuple",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeFn,
        name: META_TYPE_FN,
        arity: 5,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "meta.type_expr",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaTypeExprWire,
        name: META_TYPE_EXPR_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "meta.expr_call",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprNamedCall,
        name: META_EXPR_NAMED_CALL,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_named_call",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprTry,
        name: META_EXPR_TRY,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_try",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprLambda,
        name: META_EXPR_LAMBDA,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_lambda",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprMethodCall,
        name: META_EXPR_METHOD_CALL,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_method_call",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprQualifiedCall,
        name: META_EXPR_QUALIFIED_CALL,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_qualified_call",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprQualifiedCallPath,
        name: META_EXPR_QUALIFIED_CALL_PATH,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_qualified_call_path",
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
        wir_host_call: None,
        diagnostic_name: "meta.expr_field",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprIndex,
        name: META_EXPR_INDEX,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_index",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprDynamicDescriptor,
        name: META_EXPR_DYNAMIC_DESCRIPTOR,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_dynamic_descriptor",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprBinary,
        name: META_EXPR_BINARY,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_binary",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprUnary,
        name: META_EXPR_UNARY,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_unary",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprRange,
        name: META_EXPR_RANGE,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_range",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprList,
        name: META_EXPR_LIST,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_list",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprTuple,
        name: META_EXPR_TUPLE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_tuple",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprRecord,
        name: META_EXPR_RECORD,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_record",
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
        wir_host_call: None,
        diagnostic_name: "meta.expr_match",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprBlock,
        name: META_EXPR_BLOCK,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprIf,
        name: META_EXPR_IF,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_if",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprFor,
        name: META_EXPR_FOR,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_for",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprWhile,
        name: META_EXPR_WHILE,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_while",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprWhileLet,
        name: META_EXPR_WHILE_LET,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_while_let",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaExprAwait,
        name: META_EXPR_AWAIT,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.expr_await",
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
        wir_host_call: None,
        diagnostic_name: "meta.stmt_return",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtYield,
        name: META_STMT_YIELD,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_yield",
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
        wir_host_call: None,
        diagnostic_name: "meta.stmt_let",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtLetPattern,
        name: META_STMT_LET_PATTERN,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_let_pattern",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStmtAssign,
        name: META_STMT_ASSIGN,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.stmt_assign",
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
        wir_host_call: None,
        diagnostic_name: "meta.block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaBlockRegion,
        name: META_BLOCK_REGION,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.block_region",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaParam,
        name: META_PARAM,
        arity: 3,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.param",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaParamWire,
        name: META_PARAM_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.param",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaRecordTypeWire,
        name: META_RECORD_TYPE_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.record_type",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaFunctionExprWire,
        name: META_FUNCTION_EXPR_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.function",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaStatementWire,
        name: META_STATEMENT_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta statement builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaPatternWire,
        name: META_PATTERN_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta pattern builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaMatchArmWire,
        name: META_MATCH_ARM_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta match arm builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaBlockWire,
        name: META_BLOCK_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta block builder",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaFunctionBlockWire,
        name: META_FUNCTION_BLOCK_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.function_block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaFunctionBlock,
        name: META_FUNCTION_BLOCK,
        arity: 7,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: "meta.impl_block",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaInherentImpl,
        name: META_INHERENT_IMPL,
        arity: 2,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.inherent_impl",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaInherentImplWire,
        name: META_INHERENT_IMPL_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.inherent_impl",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaImplBlockWhere,
        name: META_IMPL_BLOCK_WHERE,
        arity: 4,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::SourceFunction,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.impl_block_where",
        private_callers: META_BRIDGE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::MetaImplBlockWhereWire,
        name: META_IMPL_BLOCK_WHERE_WIRE,
        arity: 1,
        signature: IntrinsicSignature::DeclaredInSource,
        effect: IntrinsicEffect::Toolchain,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::FrontendGenerated,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "meta.impl_block_where",
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
        wir_host_call: None,
        diagnostic_name: REGEX_MATCH_SPANS,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::RegexCount,
        name: REGEX_COUNT,
        arity: 2,
        signature: IntrinsicSignature::StringStringToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_host_call: None,
        diagnostic_name: REGEX_COUNT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::RegexReplaceAll,
        name: REGEX_REPLACE_ALL,
        arity: 3,
        signature: IntrinsicSignature::StringStringStringToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
        wir_host_call: None,
        diagnostic_name: REGEX_REPLACE_ALL,
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
        wir_host_call: None,
        diagnostic_name: CRYPTO_PUBLIC_KEY,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::CryptoReveal,
        name: CRYPTO_REVEAL,
        arity: 1,
        signature: IntrinsicSignature::RevealSecret,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::UsesSecret,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::Native,
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
        wir_host_call: None,
        diagnostic_name: STRING_CHAR_COUNT,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringCharOffset,
        name: STRING_CHAR_OFFSET,
        arity: 2,
        signature: IntrinsicSignature::StringIntToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_CHAR_OFFSET,
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
        wir_host_call: None,
        diagnostic_name: STRING_ENDS_WITH,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringFind,
        name: STRING_FIND,
        arity: 3,
        signature: IntrinsicSignature::StringStringIntToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: STRING_SUBSTRING,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringAsStr,
        name: STRING_AS_STR,
        arity: 1,
        signature: IntrinsicSignature::StringToStr,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_AS_STR,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringSlice,
        name: STRING_SLICE,
        arity: 3,
        signature: IntrinsicSignature::StrIntIntToStr,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_SLICE,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringToString,
        name: STRING_TO_STRING,
        arity: 1,
        signature: IntrinsicSignature::StrToString,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_TO_STRING,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringLen,
        name: STRING_LEN,
        arity: 1,
        signature: IntrinsicSignature::StrToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_LEN,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringHashView,
        name: STRING_HASH_VIEW,
        arity: 1,
        signature: IntrinsicSignature::StrToInt,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_HASH_VIEW,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::StringEqualViews,
        name: STRING_EQUAL_VIEWS,
        arity: 2,
        signature: IntrinsicSignature::StrStrToBool,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: STRING_EQUAL_VIEWS,
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
        wir_host_call: None,
        diagnostic_name: DICT_NEW,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictWithCapacity,
        name: DICT_WITH_CAPACITY,
        arity: 1,
        signature: IntrinsicSignature::GenericDictWithCapacity,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: DICT_WITH_CAPACITY,
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
        wir_host_call: None,
        diagnostic_name: "dict.insert",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictInsertValueExtract,
        name: DICT_INSERT_VALUE_EXTRACT,
        arity: 3,
        signature: IntrinsicSignature::GenericDictInsertValueExtract,
        effect: IntrinsicEffect::WriteBack,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
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
        wir_host_call: None,
        diagnostic_name: DICT_CONTAINS_KEY,
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictContainsBy,
        name: DICT_CONTAINS_BY,
        arity: 2,
        signature: IntrinsicSignature::GenericDictContainsBy,
        effect: IntrinsicEffect::Pure,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: DICT_CONTAINS_BY,
        private_callers: &["dict"],
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
        wir_host_call: None,
        diagnostic_name: "dict.remove",
        private_callers: NO_PRIVATE_CALLERS,
    },
    IntrinsicSpec {
        id: IntrinsicId::DictPopExtract,
        name: DICT_POP_EXTRACT,
        arity: 1,
        signature: IntrinsicSignature::GenericDictPopExtract,
        effect: IntrinsicEffect::WriteBack,
        capability_effect: CapabilityEffect::None,
        lowering: IntrinsicLowering::Builtin,
        runtime: IntrinsicRuntime::InterpreterBuiltin,
        wir_host_call: None,
        diagnostic_name: "dict.pop",
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
    BYTES_WITH_CAPACITY,
    BYTES_REPEAT,
    BYTES_PUSH,
    BYTES_APPEND,
    BYTES_RESERVE,
    BYTES_SET_AT,
    BYTES_SWAP,
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

pub const REGEX_OPERATIONS: &[&str] = &[REGEX_MATCH_SPANS, REGEX_COUNT, REGEX_REPLACE_ALL];

pub const STRING_OPERATIONS: &[&str] = &[
    STRING_LENGTH,
    STRING_CHAR_COUNT,
    STRING_CHAR_OFFSET,
    STRING_CHARS,
    STRING_FROM_CODE,
    STRING_SPLIT,
    STRING_CONTAINS,
    STRING_STARTS_WITH,
    STRING_ENDS_WITH,
    STRING_FIND,
    STRING_REPLACE,
    STRING_SUBSTRING,
    STRING_AS_STR,
    STRING_SLICE,
    STRING_TO_STRING,
    STRING_LEN,
    STRING_HASH_VIEW,
    STRING_EQUAL_VIEWS,
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
    DICT_WITH_CAPACITY,
    DICT_INSERT,
    DICT_INSERT_EXTRACT,
    DICT_INSERT_VALUE_EXTRACT,
    DICT_GET_OR,
    DICT_AT,
    DICT_UPDATE,
    DICT_CONTAINS_KEY,
    DICT_CONTAINS_BY,
    DICT_REMOVE,
    DICT_REMOVE_EXTRACT,
    DICT_POP_EXTRACT,
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
    if let Some(canonical) = [LIST_POP_EXTRACT, DICT_INSERT_EXTRACT, DICT_INSERT_VALUE_EXTRACT, DICT_REMOVE_EXTRACT, DICT_POP_EXTRACT]
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
                | IntrinsicId::BytesWithCapacity
                | IntrinsicId::BytesRepeat
                | IntrinsicId::BytesPush
                | IntrinsicId::BytesAppend
                | IntrinsicId::BytesReserve
                | IntrinsicId::BytesSetAt
                | IntrinsicId::BytesSwap
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
                | IntrinsicId::ChannelSelect2
        )
    })
}

pub fn is_string_operation(name: &str) -> bool {
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::StringLength
                | IntrinsicId::StringCharCount
                | IntrinsicId::StringCharOffset
                | IntrinsicId::StringChars
                | IntrinsicId::StringFromCode
                | IntrinsicId::StringSplit
                | IntrinsicId::StringContains
                | IntrinsicId::StringStartsWith
                | IntrinsicId::StringEndsWith
                | IntrinsicId::StringFind
                | IntrinsicId::StringReplace
                | IntrinsicId::StringSubstring
                | IntrinsicId::StringAsStr
                | IntrinsicId::StringSlice
                | IntrinsicId::StringToString
                | IntrinsicId::StringLen
                | IntrinsicId::StringHashView
                | IntrinsicId::StringEqualViews
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
    lookup(name).is_some_and(|spec| {
        matches!(
            spec.id,
            IntrinsicId::RegexMatchSpans | IntrinsicId::RegexCount | IntrinsicId::RegexReplaceAll
        )
    })
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
                | IntrinsicId::DictWithCapacity
                | IntrinsicId::DictInsert
                | IntrinsicId::DictInsertExtract
                | IntrinsicId::DictInsertValueExtract
                | IntrinsicId::DictGetOr
                | IntrinsicId::DictAt
                | IntrinsicId::DictUpdate
                | IntrinsicId::DictContainsKey
                | IntrinsicId::DictContainsBy
                | IntrinsicId::DictRemove
                | IntrinsicId::DictRemoveExtract
                | IntrinsicId::DictPopExtract
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
    lookup(name).is_some_and(|spec| matches!(spec.id,
        IntrinsicId::DictInsertExtract | IntrinsicId::DictInsertValueExtract))
}

pub fn is_dict_insert_value_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::DictInsertValueExtract)
}

pub fn is_dict_remove_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::DictRemoveExtract)
}

pub fn is_dict_pop_extract(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::DictPopExtract)
}

pub fn is_meta_fresh_ident(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFreshIdent)
}

pub fn is_meta_expr_leaf(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprLeaf)
}

pub fn is_meta_expr_compose_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprComposeWire)
}

pub fn is_meta_expr_tag_hole(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprTagHole)
}

pub fn is_meta_expr_tagged(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprTagged)
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

pub fn is_meta_call_site_type_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaCallSiteTypeWire)
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

pub fn is_meta_type_named_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeNamedWire)
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

pub fn is_meta_type_expr_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeExprWire)
}

pub fn is_meta_type_capability(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaTypeCapability)
}

pub fn is_meta_expr_call(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprCall)
}

pub fn is_meta_expr_named_call(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprNamedCall)
}

pub fn is_meta_expr_try(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprTry)
}

pub fn is_meta_expr_lambda(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprLambda)
}

pub fn is_meta_expr_method_call(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprMethodCall)
}

pub fn is_meta_expr_qualified_call(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprQualifiedCall)
}

pub fn is_meta_expr_qualified_call_path(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprQualifiedCallPath)
}

pub fn is_meta_expr_dynamic_descriptor(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprDynamicDescriptor)
}

pub fn is_meta_expr_field(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprField)
}

pub fn is_meta_expr_index(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprIndex)
}

pub fn is_meta_expr_binary(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprBinary)
}

pub fn is_meta_expr_unary(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprUnary)
}

pub fn is_meta_expr_range(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprRange)
}

pub fn is_meta_expr_list(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprList)
}

pub fn is_meta_expr_tuple(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprTuple)
}

pub fn is_meta_expr_record(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprRecord)
}

pub fn is_meta_expr_match(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprMatch)
}

pub fn is_meta_expr_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprBlock)
}

pub fn is_meta_expr_if(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprIf)
}

pub fn is_meta_expr_for(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprFor)
}

pub fn is_meta_expr_while(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprWhile)
}

pub fn is_meta_expr_while_let(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprWhileLet)
}

pub fn is_meta_expr_await(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaExprAwait)
}

pub fn is_meta_match_arm(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaMatchArm)
}

pub fn is_meta_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaBlock)
}

pub fn is_meta_block_region(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaBlockRegion)
}

pub fn is_meta_stmt_expr(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtExpr)
}

pub fn is_meta_item_with_imports(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaItemWithImports)
}

pub fn is_meta_stmt_return(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtReturn)
}

pub fn is_meta_stmt_yield(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtYield)
}

pub fn is_meta_stmt_let(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtLet)
}

pub fn is_meta_stmt_let_pattern(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtLetPattern)
}

pub fn is_meta_stmt_assign(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStmtAssign)
}

pub fn is_meta_param(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaParam)
}

pub fn is_meta_param_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaParamWire)
}

pub fn is_meta_function_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFunctionBlock)
}

pub fn is_meta_function_expr_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFunctionExprWire)
}
pub fn is_meta_statement_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaStatementWire)
}
pub fn is_meta_pattern_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaPatternWire)
}
pub fn is_meta_match_arm_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaMatchArmWire)
}
pub fn is_meta_block_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaBlockWire)
}
pub fn is_meta_function_block_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaFunctionBlockWire)
}

pub fn is_meta_impl_block(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaImplBlock)
}

pub fn is_meta_inherent_impl(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaInherentImpl)
}

pub fn is_meta_inherent_impl_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaInherentImplWire)
}

pub fn is_meta_impl_block_where(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaImplBlockWhere)
}

pub fn is_meta_impl_block_where_wire(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.id == IntrinsicId::MetaImplBlockWhereWire)
}

pub(crate) fn private_intrinsic_callers(bare_name: &str) -> Option<&'static [&'static str]> {
    if canonical_operation_name(bare_name) != bare_name {
        return None;
    }
    let callers = lookup(bare_name)?.private_callers;
    (!callers.is_empty()).then_some(callers)
}

/// Ordinary source functions have declaration identity, not a reserved global
/// intrinsic spelling. Call this only after resolving the target's module.
pub fn private_source_bridge_callers(resolved_name: &str) -> Option<&'static [&'static str]> {
    PRIVATE_SCHEDULER_SOURCE_BRIDGES.contains(&resolved_name).then_some(MESSAGE_BRIDGE_CALLERS)
}
