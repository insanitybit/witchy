//! RFC-0111 canonical layouts for closed specialized values.
//!
//! Logical type resolution stays upstream. WIR consumes a typed resolver and
//! walks the actual resolved AST fields itself, so callers never provide child
//! layout ids for nominal fields. Descriptors are immutable products of that
//! walk or of the validated canonical artifact decoder.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{effective_type_def_params, Type, TypeDef, TypeQual};

const LAYOUT_MAGIC: &[u8; 4] = b"WLAY";
const MAX_DESCRIPTOR_ITEMS: usize = 1 << 20;

/// Canonical descriptor encoding version. Any encoding or semantic change
/// increments this number; old artifacts reject before descriptor use.
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutId([u8; 32]);

impl LayoutId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut result = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
        }
        result
    }
}

impl fmt::Debug for LayoutId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "LayoutId({})", self.to_hex())
    }
}

impl fmt::Display for LayoutId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// The physical-layout portion of a callable signature.
///
/// `None` means the value uses its ordinary scalar/reference ABI. `Some(id)`
/// means callers and callees must agree on the exact canonical descriptor
/// before the logical signature is considered link-compatible. Ownership and
/// access remain separate facts (RFC-0110); later first-class-call lowering can
/// pair this value with the access signature without reconstructing either.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallableLayoutSignature {
    parameters: Vec<Option<LayoutId>>,
    result: Option<LayoutId>,
}

impl CallableLayoutSignature {
    pub fn new(parameters: Vec<Option<LayoutId>>, result: Option<LayoutId>) -> Self {
        Self { parameters, result }
    }

    pub fn parameters(&self) -> &[Option<LayoutId>] {
        &self.parameters
    }

    pub fn result(&self) -> Option<LayoutId> {
        self.result
    }

    pub fn has_specialized_layout(&self) -> bool {
        self.result.is_some() || self.parameters.iter().any(Option::is_some)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Bool,
    Int,
    Float,
    Duration,
    U32,
    Tag8,
    Tag16,
    Tag32,
}

impl ScalarKind {
    pub const fn size(self) -> u32 {
        match self {
            Self::Bool | Self::Tag8 => 1,
            Self::Tag16 => 2,
            Self::U32 | Self::Tag32 => 4,
            Self::Int | Self::Float | Self::Duration => 8,
        }
    }

    pub const fn alignment(self) -> u32 {
        self.size()
    }

    const fn is_source_value(self) -> bool {
        matches!(self, Self::Bool | Self::Int | Self::Float | Self::Duration)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSize {
    Fixed(u32),
    Dynamic { base: u32, stride: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Scalar(ScalarKind),
    Inline(LayoutId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutField {
    offset: u32,
    kind: FieldKind,
}

impl LayoutField {
    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayout {
    fields: Vec<LayoutField>,
    payload_size: u32,
    payload_alignment: u32,
}

impl VariantLayout {
    pub fn fields(&self) -> &[LayoutField] {
        &self.fields
    }

    pub fn payload_size(&self) -> u32 {
        self.payload_size
    }

    pub fn payload_alignment(&self) -> u32 {
        self.payload_alignment
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcHeader {
    Required,
    Elided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderLayout {
    None,
    PackedList {
        rc: RcHeader,
        length_offset: u32,
        capacity_offset: u32,
        data_offset: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipPosition {
    RootBuffer,
    Field(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationShape {
    None,
    Scalar(ScalarKind),
    Fields(Vec<LayoutId>),
    Variants {
        tag: ScalarKind,
        variants: Vec<Vec<LayoutId>>,
    },
    PackedElements {
        element: LayoutId,
        stride: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationShapes {
    copy: OperationShape,
    duplicate: OperationShape,
    drop: OperationShape,
    equality: OperationShape,
    render: OperationShape,
    serialization: OperationShape,
}

impl OperationShapes {
    pub fn copy(&self) -> &OperationShape {
        &self.copy
    }

    pub fn duplicate(&self) -> &OperationShape {
        &self.duplicate
    }

    pub fn drop(&self) -> &OperationShape {
        &self.drop
    }

    pub fn equality(&self) -> &OperationShape {
        &self.equality
    }

    pub fn render(&self) -> &OperationShape {
        &self.render
    }

    pub fn serialization(&self) -> &OperationShape {
        &self.serialization
    }
}

/// The minimal child identity needed to reconstruct every derived descriptor
/// invariant. Offsets, padding, operation shapes, and headers are not supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    Scalar(ScalarKind),
    Tuple { fields: Vec<LayoutId> },
    PackedRecord { fields: Vec<LayoutId> },
    PackedList { element: LayoutId, rc: RcHeader },
    ClosedSum { variants: Vec<Vec<LayoutId>> },
}

/// An immutable validated physical descriptor. There is no public field or raw
/// constructor; values come from [`LayoutInterner::intern_type`] or the checked
/// canonical decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutDescriptor {
    schema_version: u32,
    kind: LayoutKind,
    size: LayoutSize,
    alignment: u32,
    fields: Vec<LayoutField>,
    variants: Vec<VariantLayout>,
    ownership: Vec<OwnershipPosition>,
    header: HeaderLayout,
    operations: OperationShapes,
}

impl LayoutDescriptor {
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn kind(&self) -> &LayoutKind {
        &self.kind
    }

    pub fn size(&self) -> LayoutSize {
        self.size
    }

    pub fn alignment(&self) -> u32 {
        self.alignment
    }

    pub fn fields(&self) -> &[LayoutField] {
        &self.fields
    }

    pub fn variant_layouts(&self) -> &[VariantLayout] {
        &self.variants
    }

    pub fn ownership(&self) -> &[OwnershipPosition] {
        &self.ownership
    }

    pub fn header(&self) -> HeaderLayout {
        self.header
    }

    pub fn operations(&self) -> &OperationShapes {
        &self.operations
    }

    /// The sole descriptor wire representation, shared by caches, artifacts,
    /// workers, and host boundary metadata.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::default();
        encoder.bytes(LAYOUT_MAGIC);
        encoder.u32(self.schema_version);
        encoder.descriptor(self);
        encoder.output
    }

    pub fn layout_id(&self) -> LayoutId {
        LayoutId(sha256(&self.canonical_bytes()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Owning,
    BorrowedView,
    Function,
    ExternRef,
    GcReference,
    Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    OwningReference,
    BorrowedView,
    FunctionReference,
    ExternRef,
    GcReference,
    CapabilityReference,
}

impl ReferenceKind {
    pub const fn storage_class(self) -> StorageClass {
        match self {
            Self::Owning => StorageClass::OwningReference,
            Self::BorrowedView => StorageClass::BorrowedView,
            Self::Function => StorageClass::FunctionReference,
            Self::ExternRef => StorageClass::ExternRef,
            Self::GcReference => StorageClass::GcReference,
            Self::Capability => StorageClass::CapabilityReference,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Root(String),
    Field(String),
    Variant(String),
    Tuple(usize),
    ListElement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPath(Vec<PathSegment>);

impl LayoutPath {
    fn root(ty: &Type) -> Self {
        let name = match ty {
            Type::Named(name, _) => name.clone(),
            Type::Tuple(_) => "<tuple>".to_owned(),
            Type::Qualified(_, inner) => return Self::root(inner),
            Type::Fn(..) => "<function>".to_owned(),
            Type::Dyn(name, _) => format!("dyn {name}"),
            Type::RecordCompose { .. } => "<record composition>".to_owned(),
        };
        Self(vec![PathSegment::Root(name)])
    }

    fn field(&self, name: String) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Field(name));
        path
    }

    fn variant(&self, name: String) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Variant(name));
        path
    }

    fn tuple(&self, index: usize) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::Tuple(index));
        path
    }

    fn list_element(&self) -> Self {
        let mut path = self.clone();
        path.0.push(PathSegment::ListElement);
        path
    }
}

impl fmt::Display for LayoutPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.0 {
            match segment {
                PathSegment::Root(name) => formatter.write_str(name)?,
                PathSegment::Field(name) => write!(formatter, ".{name}")?,
                PathSegment::Variant(name) => write!(formatter, "::{name}")?,
                PathSegment::Tuple(index) => write!(formatter, ".{index}")?,
                PathSegment::ListElement => formatter.write_str(".element")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    UnresolvedType { path: LayoutPath, name: String },
    ResolvedTypeMismatch { path: LayoutPath, expected: String, actual: String },
    TypeArgumentCount { path: LayoutPath, expected: usize, actual: usize },
    UnsupportedType { path: LayoutPath, reason: &'static str },
    NotPackedRecord { path: LayoutPath, name: String },
    CapabilityDefinition { path: LayoutPath, name: String },
    InvalidClosedSum { path: LayoutPath, name: String },
    InlineNominalCycle { path: LayoutPath, definition: String },
    ReferenceNotInline {
        path: LayoutPath,
        kind: ReferenceKind,
        class: StorageClass,
    },
    UnknownLayout(LayoutId),
    DynamicInlineField(LayoutId),
    ArithmeticOverflow,
    UnsupportedSchema { found: u32 },
    Decode { offset: usize, reason: &'static str },
    TrailingBytes { offset: usize },
    NonCanonicalEncoding,
    DescriptorInvariant(&'static str),
    DigestMismatch { expected: LayoutId, actual: LayoutId },
    DigestCollision(LayoutId),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedType { path, name } => {
                write!(formatter, "{path}: unresolved closed layout type `{name}`")
            }
            Self::ResolvedTypeMismatch { path, expected, actual } => {
                write!(formatter, "{path}: resolver returned `{actual}` for `{expected}`")
            }
            Self::TypeArgumentCount { path, expected, actual } => {
                write!(formatter, "{path}: expected {expected} closed type arguments, got {actual}")
            }
            Self::UnsupportedType { path, reason } => write!(formatter, "{path}: {reason}"),
            Self::NotPackedRecord { path, name } => {
                write!(formatter, "{path}: `{name}` is not a one-variant packed record")
            }
            Self::CapabilityDefinition { path, name } => {
                write!(formatter, "{path}: capability `{name}` requires reference-safe storage")
            }
            Self::InvalidClosedSum { path, name } => {
                write!(formatter, "{path}: `{name}` is not a non-empty closed sum")
            }
            Self::InlineNominalCycle { path, definition } => {
                write!(formatter, "{path}: inline layout cycle re-enters `{definition}`")
            }
            Self::ReferenceNotInline { path, kind, class } => {
                write!(formatter, "{path}: {kind:?} is {class:?} and cannot be stored inline")
            }
            Self::UnknownLayout(id) => write!(formatter, "unknown child layout `{id}`"),
            Self::DynamicInlineField(id) => {
                write!(formatter, "dynamic layout `{id}` cannot be stored inline")
            }
            Self::ArithmeticOverflow => formatter.write_str("physical layout size overflow"),
            Self::UnsupportedSchema { found } => {
                write!(formatter, "unsupported layout schema {found}")
            }
            Self::Decode { offset, reason } => {
                write!(formatter, "invalid layout bytes at {offset}: {reason}")
            }
            Self::TrailingBytes { offset } => {
                write!(formatter, "trailing layout bytes at {offset}")
            }
            Self::NonCanonicalEncoding => formatter.write_str("non-canonical layout encoding"),
            Self::DescriptorInvariant(reason) => {
                write!(formatter, "invalid layout descriptor: {reason}")
            }
            Self::DigestMismatch { expected, actual } => {
                write!(formatter, "layout digest mismatch: expected {expected}, got {actual}")
            }
            Self::DigestCollision(id) => write!(formatter, "layout digest collision for `{id}`"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// A type checker's closed, typed decision for a named logical type. The
/// interner still walks and substitutes every actual [`TypeDef`] field itself.
pub enum ResolvedNamed<'a> {
    Scalar(ScalarKind),
    PackedRecord(&'a TypeDef),
    PackedList { rc: RcHeader },
    ClosedSum(&'a TypeDef),
    Reference(ReferenceKind),
}

/// Authoritative bridge from resolved AST identity into physical classes. This
/// is intentionally typed: no child layout ids or field arrays cross it.
pub trait ClosedTypeResolver {
    fn resolve_named<'a>(&'a self, name: &str, arguments: &[Type]) -> Option<ResolvedNamed<'a>>;
}

#[derive(Debug, Default)]
pub struct LayoutInterner {
    descriptors: BTreeMap<LayoutId, LayoutDescriptor>,
}

impl LayoutInterner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    pub fn get(&self, id: LayoutId) -> Option<&LayoutDescriptor> {
        self.descriptors.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (LayoutId, &LayoutDescriptor)> {
        self.descriptors.iter().map(|(id, descriptor)| (*id, descriptor))
    }

    /// Resolve and intern one complete closed logical type. Nominal children are
    /// derived from substituted `TypeDef` fields, never accepted as caller ids.
    pub fn intern_type(
        &mut self,
        ty: &Type,
        resolver: &impl ClosedTypeResolver,
    ) -> Result<LayoutId, LayoutError> {
        let path = LayoutPath::root(ty);
        self.intern_resolved(ty, resolver, &path, &mut BTreeSet::new())
    }

    /// Decode and fully validate canonical bytes without mutating the interner.
    /// Every referenced child must already be present.
    pub fn decode_canonical(&self, bytes: &[u8]) -> Result<LayoutDescriptor, LayoutError> {
        let descriptor = decode_descriptor(bytes)?;
        if descriptor.canonical_bytes() != bytes {
            return Err(LayoutError::NonCanonicalEncoding);
        }
        self.validate_descriptor(&descriptor)?;
        Ok(descriptor)
    }

    pub fn validate_canonical(
        &self,
        expected: LayoutId,
        bytes: &[u8],
    ) -> Result<(), LayoutError> {
        let descriptor = self.decode_canonical(bytes)?;
        let actual = descriptor.layout_id();
        if actual != expected {
            return Err(LayoutError::DigestMismatch { expected, actual });
        }
        Ok(())
    }

    /// Validate and import an artifact/host descriptor. Child descriptors must
    /// be imported first, giving the descriptor graph a deterministic order.
    pub fn import_canonical(
        &mut self,
        expected: LayoutId,
        bytes: &[u8],
    ) -> Result<LayoutId, LayoutError> {
        let descriptor = self.decode_canonical(bytes)?;
        let actual = descriptor.layout_id();
        if actual != expected {
            return Err(LayoutError::DigestMismatch { expected, actual });
        }
        if let Some(existing) = self.descriptors.get(&actual) {
            if existing != &descriptor {
                return Err(LayoutError::DigestCollision(actual));
            }
            return Ok(actual);
        }
        self.descriptors.insert(actual, descriptor);
        Ok(actual)
    }

    fn intern_resolved(
        &mut self,
        ty: &Type,
        resolver: &impl ClosedTypeResolver,
        path: &LayoutPath,
        active_nominals: &mut BTreeSet<String>,
    ) -> Result<LayoutId, LayoutError> {
        match ty {
            Type::Qualified(TypeQual::Borrow(_), _) => {
                Err(reference_error(path, ReferenceKind::BorrowedView))
            }
            Type::Qualified(_, inner) => {
                self.intern_resolved(inner, resolver, path, active_nominals)
            }
            Type::Tuple(fields) => {
                let mut children = Vec::with_capacity(fields.len());
                for (index, field) in fields.iter().enumerate() {
                    children.push(self.intern_resolved(
                        field,
                        resolver,
                        &path.tuple(index),
                        active_nominals,
                    )?);
                }
                self.intern_kind(LayoutKind::Tuple { fields: children })
            }
            Type::Fn(..) => Err(reference_error(path, ReferenceKind::Function)),
            Type::Dyn(..) => Err(reference_error(path, ReferenceKind::GcReference)),
            Type::RecordCompose { .. } => Err(LayoutError::UnsupportedType {
                path: path.clone(),
                reason: "structural record composition must be normalized before layout",
            }),
            Type::Named(name, arguments) => {
                let resolved = resolver.resolve_named(name, arguments).ok_or_else(|| {
                    LayoutError::UnresolvedType { path: path.clone(), name: name.clone() }
                })?;
                match resolved {
                    ResolvedNamed::Scalar(scalar) => {
                        if !arguments.is_empty() {
                            return Err(LayoutError::TypeArgumentCount {
                                path: path.clone(),
                                expected: 0,
                                actual: arguments.len(),
                            });
                        }
                        if !scalar.is_source_value() {
                            return Err(LayoutError::UnsupportedType {
                                path: path.clone(),
                                reason: "internal header/tag scalar cannot be a source field",
                            });
                        }
                        self.intern_kind(LayoutKind::Scalar(scalar))
                    }
                    ResolvedNamed::PackedList { rc } => {
                        if arguments.len() != 1 {
                            return Err(LayoutError::TypeArgumentCount {
                                path: path.clone(),
                                expected: 1,
                                actual: arguments.len(),
                            });
                        }
                        let element = self.intern_resolved(
                            &arguments[0],
                            resolver,
                            &path.list_element(),
                            active_nominals,
                        )?;
                        self.intern_kind(LayoutKind::PackedList { element, rc })
                    }
                    ResolvedNamed::Reference(kind) => Err(reference_error(path, kind)),
                    ResolvedNamed::PackedRecord(definition) => {
                        self.check_definition_identity(name, definition, path)?;
                        if definition.is_capability {
                            return Err(LayoutError::CapabilityDefinition {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !definition.packed || definition.variants.len() != 1 {
                            return Err(LayoutError::NotPackedRecord {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !active_nominals.insert(definition.name.clone()) {
                            return Err(LayoutError::InlineNominalCycle {
                                path: path.clone(),
                                definition: definition.name.clone(),
                            });
                        }
                        let result = (|| {
                            let variants = instantiate_fields(definition, arguments, path)?;
                            let variant = &definition.variants[0];
                            let mut children = Vec::with_capacity(variants[0].len());
                            for (index, field) in variants[0].iter().enumerate() {
                                let field_name = variant
                                    .field_names
                                    .get(index)
                                    .cloned()
                                    .unwrap_or_else(|| index.to_string());
                                children.push(self.intern_resolved(
                                    field,
                                    resolver,
                                    &path.field(field_name),
                                    active_nominals,
                                )?);
                            }
                            self.intern_kind(LayoutKind::PackedRecord { fields: children })
                        })();
                        active_nominals.remove(&definition.name);
                        result
                    }
                    ResolvedNamed::ClosedSum(definition) => {
                        self.check_definition_identity(name, definition, path)?;
                        if definition.is_capability {
                            return Err(LayoutError::CapabilityDefinition {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if definition.variants.is_empty() {
                            return Err(LayoutError::InvalidClosedSum {
                                path: path.clone(),
                                name: definition.name.clone(),
                            });
                        }
                        if !active_nominals.insert(definition.name.clone()) {
                            return Err(LayoutError::InlineNominalCycle {
                                path: path.clone(),
                                definition: definition.name.clone(),
                            });
                        }
                        let result = (|| {
                            let fields = instantiate_fields(definition, arguments, path)?;
                            let mut variants = Vec::with_capacity(fields.len());
                            for (variant_index, variant_fields) in fields.iter().enumerate() {
                                let variant = &definition.variants[variant_index];
                                let variant_path = path.variant(variant.name.clone());
                                let mut children = Vec::with_capacity(variant_fields.len());
                                for (field_index, field) in variant_fields.iter().enumerate() {
                                    let field_name = variant
                                        .field_names
                                        .get(field_index)
                                        .cloned()
                                        .unwrap_or_else(|| field_index.to_string());
                                    children.push(self.intern_resolved(
                                        field,
                                        resolver,
                                        &variant_path.field(field_name),
                                        active_nominals,
                                    )?);
                                }
                                variants.push(children);
                            }
                            self.intern_kind(LayoutKind::ClosedSum { variants })
                        })();
                        active_nominals.remove(&definition.name);
                        result
                    }
                }
            }
        }
    }

    fn check_definition_identity(
        &self,
        expected: &str,
        definition: &TypeDef,
        path: &LayoutPath,
    ) -> Result<(), LayoutError> {
        if definition.name != expected {
            return Err(LayoutError::ResolvedTypeMismatch {
                path: path.clone(),
                expected: expected.to_owned(),
                actual: definition.name.clone(),
            });
        }
        Ok(())
    }

    fn intern_kind(&mut self, kind: LayoutKind) -> Result<LayoutId, LayoutError> {
        let descriptor = build_descriptor(kind, |id| self.descriptors.get(&id))?;
        // This is deliberately a release-mode check. No id is computed until
        // the complete descriptor reproduces from its minimal kind and children.
        self.validate_descriptor(&descriptor)?;
        let id = descriptor.layout_id();
        if let Some(existing) = self.descriptors.get(&id) {
            if existing != &descriptor {
                return Err(LayoutError::DigestCollision(id));
            }
            return Ok(id);
        }
        self.descriptors.insert(id, descriptor);
        Ok(id)
    }

    fn validate_descriptor(&self, descriptor: &LayoutDescriptor) -> Result<(), LayoutError> {
        if descriptor.schema_version != LAYOUT_SCHEMA_VERSION {
            return Err(LayoutError::UnsupportedSchema { found: descriptor.schema_version });
        }
        validate_descriptor_item_counts(descriptor)?;
        if descriptor.alignment == 0 || !descriptor.alignment.is_power_of_two() {
            return Err(LayoutError::DescriptorInvariant(
                "alignment must be a non-zero power of two",
            ));
        }
        let expected = build_descriptor(descriptor.kind.clone(), |id| self.descriptors.get(&id))?;
        if descriptor != &expected {
            return Err(LayoutError::DescriptorInvariant(
                "size, alignment, fields, header, ownership, or operation shape disagrees with kind",
            ));
        }
        Ok(())
    }
}

fn validate_descriptor_item_counts(descriptor: &LayoutDescriptor) -> Result<(), LayoutError> {
    fn check(count: usize) -> Result<(), LayoutError> {
        if count > MAX_DESCRIPTOR_ITEMS || u32::try_from(count).is_err() {
            Err(LayoutError::DescriptorInvariant(
                "descriptor item count exceeds canonical limit",
            ))
        } else {
            Ok(())
        }
    }

    match &descriptor.kind {
        LayoutKind::Scalar(_) | LayoutKind::PackedList { .. } => {}
        LayoutKind::Tuple { fields } | LayoutKind::PackedRecord { fields } => {
            check(fields.len())?;
        }
        LayoutKind::ClosedSum { variants } => {
            check(variants.len())?;
            for variant in variants {
                check(variant.len())?;
            }
        }
    }
    check(descriptor.fields.len())?;
    check(descriptor.variants.len())?;
    for variant in &descriptor.variants {
        check(variant.fields.len())?;
    }
    check(descriptor.ownership.len())?;
    for position in &descriptor.ownership {
        if let OwnershipPosition::Field(path) = position {
            check(path.len())?;
        }
    }
    for operation in [
        &descriptor.operations.copy,
        &descriptor.operations.duplicate,
        &descriptor.operations.drop,
        &descriptor.operations.equality,
        &descriptor.operations.render,
        &descriptor.operations.serialization,
    ] {
        match operation {
            OperationShape::None
            | OperationShape::Scalar(_)
            | OperationShape::PackedElements { .. } => {}
            OperationShape::Fields(fields) => check(fields.len())?,
            OperationShape::Variants { variants, .. } => {
                check(variants.len())?;
                for variant in variants {
                    check(variant.len())?;
                }
            }
        }
    }
    Ok(())
}

fn reference_error(path: &LayoutPath, kind: ReferenceKind) -> LayoutError {
    LayoutError::ReferenceNotInline {
        path: path.clone(),
        kind,
        class: kind.storage_class(),
    }
}

fn instantiate_fields(
    definition: &TypeDef,
    arguments: &[Type],
    path: &LayoutPath,
) -> Result<Vec<Vec<Type>>, LayoutError> {
    let parameters = effective_type_def_params(definition);
    if arguments.len() != parameters.len() {
        return Err(LayoutError::TypeArgumentCount {
            path: path.clone(),
            expected: parameters.len(),
            actual: arguments.len(),
        });
    }
    let bindings = parameters
        .into_iter()
        .zip(arguments.iter().cloned())
        .collect::<BTreeMap<_, _>>();
    Ok(definition
        .variants
        .iter()
        .map(|variant| {
            variant
                .fields
                .iter()
                .map(|field| substitute_type(field, &bindings))
                .collect()
        })
        .collect())
}

fn substitute_type(ty: &Type, bindings: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => {
            bindings.get(name).cloned().unwrap_or_else(|| ty.clone())
        }
        Type::Named(name, arguments) => Type::Named(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_type(argument, bindings))
                .collect(),
        ),
        Type::Tuple(fields) => Type::Tuple(
            fields
                .iter()
                .map(|field| substitute_type(field, bindings))
                .collect(),
        ),
        Type::Qualified(qualifier, inner) => Type::Qualified(
            qualifier.clone(),
            Box::new(substitute_type(inner, bindings)),
        ),
        Type::Fn(parameters, result, conventions) => Type::Fn(
            parameters
                .iter()
                .map(|parameter| substitute_type(parameter, bindings))
                .collect(),
            Box::new(substitute_type(result, bindings)),
            conventions.clone(),
        ),
        Type::Dyn(name, arguments) => Type::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| substitute_type(argument, bindings))
                .collect(),
        ),
        Type::RecordCompose { base, fields } => Type::RecordCompose {
            base: Box::new(substitute_type(base, bindings)),
            fields: fields
                .iter()
                .map(|(name, field)| (name.clone(), substitute_type(field, bindings)))
                .collect(),
        },
    }
}

fn build_descriptor<'a>(
    kind: LayoutKind,
    mut lookup: impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<LayoutDescriptor, LayoutError> {
    match &kind {
        LayoutKind::Scalar(scalar) => {
            let operation = OperationShape::Scalar(*scalar);
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(scalar.size()),
                alignment: scalar.alignment(),
                fields: Vec::new(),
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: OperationShape::None,
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::Tuple { fields } | LayoutKind::PackedRecord { fields } => {
            let (physical_fields, size, alignment) = aggregate_fields(fields, 0, &mut lookup)?;
            let operation = OperationShape::Fields(fields.clone());
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment,
                fields: physical_fields,
                variants: Vec::new(),
                ownership: Vec::new(),
                header: HeaderLayout::None,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::PackedList { element, rc } => {
            let element_descriptor = lookup(*element).ok_or(LayoutError::UnknownLayout(*element))?;
            let element_size = match element_descriptor.size {
                LayoutSize::Fixed(size) => size,
                LayoutSize::Dynamic { .. } => {
                    return Err(LayoutError::DynamicInlineField(*element));
                }
            };
            let stride = align_up(element_size, element_descriptor.alignment)?;
            let data_offset = align_up(8, element_descriptor.alignment)?;
            let alignment = element_descriptor.alignment.max(4);
            let operation = OperationShape::PackedElements {
                element: *element,
                stride,
            };
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Dynamic { base: data_offset, stride },
                alignment,
                fields: vec![
                    LayoutField { offset: 0, kind: FieldKind::Scalar(ScalarKind::U32) },
                    LayoutField { offset: 4, kind: FieldKind::Scalar(ScalarKind::U32) },
                    LayoutField { offset: data_offset, kind: FieldKind::Inline(*element) },
                ],
                variants: Vec::new(),
                ownership: vec![OwnershipPosition::RootBuffer],
                header: HeaderLayout::PackedList {
                    rc: *rc,
                    length_offset: 0,
                    capacity_offset: 4,
                    data_offset,
                },
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
        LayoutKind::ClosedSum { variants } => {
            let tag = tag_kind(variants.len())?;
            let mut payload_alignment = 1;
            let mut payload_size = 0;
            for variant in variants {
                let (_, size, alignment) = aggregate_fields(variant, 0, &mut lookup)?;
                payload_alignment = payload_alignment.max(alignment);
                payload_size = payload_size.max(size);
            }
            let payload_offset = align_up(tag.size(), payload_alignment)?;
            let alignment = tag.alignment().max(payload_alignment);
            let size = align_up(
                payload_offset
                    .checked_add(payload_size)
                    .ok_or(LayoutError::ArithmeticOverflow)?,
                alignment,
            )?;
            let mut variant_layouts = Vec::with_capacity(variants.len());
            for variant in variants {
                let (fields, size, alignment) =
                    aggregate_fields(variant, payload_offset, &mut lookup)?;
                variant_layouts.push(VariantLayout {
                    fields,
                    payload_size: size,
                    payload_alignment: alignment,
                });
            }
            let operation = OperationShape::Variants {
                tag,
                variants: variants.clone(),
            };
            Ok(LayoutDescriptor {
                schema_version: LAYOUT_SCHEMA_VERSION,
                kind: kind.clone(),
                size: LayoutSize::Fixed(size),
                alignment,
                fields: vec![LayoutField { offset: 0, kind: FieldKind::Scalar(tag) }],
                variants: variant_layouts,
                ownership: Vec::new(),
                header: HeaderLayout::None,
                operations: OperationShapes {
                    copy: operation.clone(),
                    duplicate: operation.clone(),
                    drop: operation.clone(),
                    equality: operation.clone(),
                    render: operation.clone(),
                    serialization: operation,
                },
            })
        }
    }
}

fn aggregate_fields<'a>(
    children: &[LayoutId],
    base: u32,
    lookup: &mut impl FnMut(LayoutId) -> Option<&'a LayoutDescriptor>,
) -> Result<(Vec<LayoutField>, u32, u32), LayoutError> {
    let mut offset = 0;
    let mut alignment = 1;
    let mut fields = Vec::with_capacity(children.len());
    for child in children {
        let descriptor = lookup(*child).ok_or(LayoutError::UnknownLayout(*child))?;
        let size = match descriptor.size {
            LayoutSize::Fixed(size) => size,
            LayoutSize::Dynamic { .. } => return Err(LayoutError::DynamicInlineField(*child)),
        };
        offset = align_up(offset, descriptor.alignment)?;
        let field_offset = base.checked_add(offset).ok_or(LayoutError::ArithmeticOverflow)?;
        fields.push(LayoutField {
            offset: field_offset,
            kind: match descriptor.kind {
                LayoutKind::Scalar(scalar) => FieldKind::Scalar(scalar),
                _ => FieldKind::Inline(*child),
            },
        });
        offset = offset.checked_add(size).ok_or(LayoutError::ArithmeticOverflow)?;
        alignment = alignment.max(descriptor.alignment);
    }
    Ok((fields, align_up(offset, alignment)?, alignment))
}

fn align_up(value: u32, alignment: u32) -> Result<u32, LayoutError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(LayoutError::DescriptorInvariant(
            "alignment must be a non-zero power of two",
        ));
    }
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or(LayoutError::ArithmeticOverflow)
}

fn tag_kind(variants: usize) -> Result<ScalarKind, LayoutError> {
    let maximum = variants
        .checked_sub(1)
        .ok_or(LayoutError::DescriptorInvariant("closed sum has no variants"))?;
    if maximum <= u8::MAX as usize {
        Ok(ScalarKind::Tag8)
    } else if maximum <= u16::MAX as usize {
        Ok(ScalarKind::Tag16)
    } else if maximum <= u32::MAX as usize {
        Ok(ScalarKind::Tag32)
    } else {
        Err(LayoutError::ArithmeticOverflow)
    }
}

#[derive(Default)]
struct Encoder {
    output: Vec<u8>,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.output.push(value);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.output.extend_from_slice(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn id(&mut self, id: LayoutId) {
        self.bytes(id.as_bytes());
    }

    fn len(&mut self, value: usize) {
        debug_assert!(value <= MAX_DESCRIPTOR_ITEMS);
        self.u32(value as u32);
    }

    fn ids(&mut self, ids: &[LayoutId]) {
        self.len(ids.len());
        for id in ids {
            self.id(*id);
        }
    }

    fn scalar(&mut self, scalar: ScalarKind) {
        self.byte(match scalar {
            ScalarKind::Bool => 0,
            ScalarKind::Int => 1,
            ScalarKind::Float => 2,
            ScalarKind::Duration => 3,
            ScalarKind::U32 => 4,
            ScalarKind::Tag8 => 5,
            ScalarKind::Tag16 => 6,
            ScalarKind::Tag32 => 7,
        });
    }

    fn field(&mut self, field: &LayoutField) {
        self.u32(field.offset);
        match field.kind {
            FieldKind::Scalar(scalar) => {
                self.byte(0);
                self.scalar(scalar);
            }
            FieldKind::Inline(id) => {
                self.byte(1);
                self.id(id);
            }
        }
    }

    fn fields(&mut self, fields: &[LayoutField]) {
        self.len(fields.len());
        for field in fields {
            self.field(field);
        }
    }

    fn kind(&mut self, kind: &LayoutKind) {
        match kind {
            LayoutKind::Scalar(scalar) => {
                self.byte(0);
                self.scalar(*scalar);
            }
            LayoutKind::Tuple { fields } => {
                self.byte(1);
                self.ids(fields);
            }
            LayoutKind::PackedRecord { fields } => {
                self.byte(2);
                self.ids(fields);
            }
            LayoutKind::PackedList { element, rc } => {
                self.byte(3);
                self.id(*element);
                self.byte(match rc {
                    RcHeader::Required => 0,
                    RcHeader::Elided => 1,
                });
            }
            LayoutKind::ClosedSum { variants } => {
                self.byte(4);
                self.len(variants.len());
                for variant in variants {
                    self.ids(variant);
                }
            }
        }
    }

    fn operation(&mut self, operation: &OperationShape) {
        match operation {
            OperationShape::None => self.byte(0),
            OperationShape::Scalar(scalar) => {
                self.byte(1);
                self.scalar(*scalar);
            }
            OperationShape::Fields(fields) => {
                self.byte(2);
                self.ids(fields);
            }
            OperationShape::Variants { tag, variants } => {
                self.byte(3);
                self.scalar(*tag);
                self.len(variants.len());
                for variant in variants {
                    self.ids(variant);
                }
            }
            OperationShape::PackedElements { element, stride } => {
                self.byte(4);
                self.id(*element);
                self.u32(*stride);
            }
        }
    }

    fn descriptor(&mut self, descriptor: &LayoutDescriptor) {
        self.kind(&descriptor.kind);
        match descriptor.size {
            LayoutSize::Fixed(size) => {
                self.byte(0);
                self.u32(size);
            }
            LayoutSize::Dynamic { base, stride } => {
                self.byte(1);
                self.u32(base);
                self.u32(stride);
            }
        }
        self.u32(descriptor.alignment);
        self.fields(&descriptor.fields);
        self.len(descriptor.variants.len());
        for variant in &descriptor.variants {
            self.fields(&variant.fields);
            self.u32(variant.payload_size);
            self.u32(variant.payload_alignment);
        }
        self.len(descriptor.ownership.len());
        for position in &descriptor.ownership {
            match position {
                OwnershipPosition::RootBuffer => self.byte(0),
                OwnershipPosition::Field(path) => {
                    self.byte(1);
                    self.len(path.len());
                    for index in path {
                        self.u32(*index);
                    }
                }
            }
        }
        match descriptor.header {
            HeaderLayout::None => self.byte(0),
            HeaderLayout::PackedList {
                rc,
                length_offset,
                capacity_offset,
                data_offset,
            } => {
                self.byte(1);
                self.byte(match rc {
                    RcHeader::Required => 0,
                    RcHeader::Elided => 1,
                });
                self.u32(length_offset);
                self.u32(capacity_offset);
                self.u32(data_offset);
            }
        }
        for operation in [
            &descriptor.operations.copy,
            &descriptor.operations.duplicate,
            &descriptor.operations.drop,
            &descriptor.operations.equality,
            &descriptor.operations.render,
            &descriptor.operations.serialization,
        ] {
            self.operation(operation);
        }
    }
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn byte(&mut self) -> Result<u8, LayoutError> {
        let byte = self.input.get(self.offset).copied().ok_or(LayoutError::Decode {
            offset: self.offset,
            reason: "unexpected end of descriptor",
        })?;
        self.offset += 1;
        Ok(byte)
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], LayoutError> {
        let end = self.offset.checked_add(count).ok_or(LayoutError::Decode {
            offset: self.offset,
            reason: "descriptor offset overflow",
        })?;
        let bytes = self.input.get(self.offset..end).ok_or(LayoutError::Decode {
            offset: self.offset,
            reason: "unexpected end of descriptor",
        })?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, LayoutError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn len(&mut self) -> Result<usize, LayoutError> {
        let length = self.u32()? as usize;
        if length > MAX_DESCRIPTOR_ITEMS {
            return Err(LayoutError::Decode {
                offset: self.offset - 4,
                reason: "descriptor item count exceeds limit",
            });
        }
        Ok(length)
    }

    fn id(&mut self) -> Result<LayoutId, LayoutError> {
        let mut id = [0; 32];
        id.copy_from_slice(self.bytes(32)?);
        Ok(LayoutId(id))
    }

    fn ids(&mut self) -> Result<Vec<LayoutId>, LayoutError> {
        let count = self.len()?;
        let mut ids = Vec::with_capacity(count.min(self.input.len() / 32));
        for _ in 0..count {
            ids.push(self.id()?);
        }
        Ok(ids)
    }

    fn scalar(&mut self) -> Result<ScalarKind, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(ScalarKind::Bool),
            1 => Ok(ScalarKind::Int),
            2 => Ok(ScalarKind::Float),
            3 => Ok(ScalarKind::Duration),
            4 => Ok(ScalarKind::U32),
            5 => Ok(ScalarKind::Tag8),
            6 => Ok(ScalarKind::Tag16),
            7 => Ok(ScalarKind::Tag32),
            _ => Err(LayoutError::Decode { offset, reason: "unknown scalar kind" }),
        }
    }

    fn rc(&mut self) -> Result<RcHeader, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(RcHeader::Required),
            1 => Ok(RcHeader::Elided),
            _ => Err(LayoutError::Decode { offset, reason: "unknown RC header kind" }),
        }
    }

    fn field(&mut self) -> Result<LayoutField, LayoutError> {
        let offset = self.u32()?;
        let tag_offset = self.offset;
        let kind = match self.byte()? {
            0 => FieldKind::Scalar(self.scalar()?),
            1 => FieldKind::Inline(self.id()?),
            _ => {
                return Err(LayoutError::Decode {
                    offset: tag_offset,
                    reason: "unknown field kind",
                });
            }
        };
        Ok(LayoutField { offset, kind })
    }

    fn fields(&mut self) -> Result<Vec<LayoutField>, LayoutError> {
        let count = self.len()?;
        let mut fields = Vec::with_capacity(count.min(self.input.len()));
        for _ in 0..count {
            fields.push(self.field()?);
        }
        Ok(fields)
    }

    fn kind(&mut self) -> Result<LayoutKind, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(LayoutKind::Scalar(self.scalar()?)),
            1 => Ok(LayoutKind::Tuple { fields: self.ids()? }),
            2 => Ok(LayoutKind::PackedRecord { fields: self.ids()? }),
            3 => Ok(LayoutKind::PackedList { element: self.id()?, rc: self.rc()? }),
            4 => {
                let count = self.len()?;
                let mut variants = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    variants.push(self.ids()?);
                }
                Ok(LayoutKind::ClosedSum { variants })
            }
            _ => Err(LayoutError::Decode { offset, reason: "unknown layout kind" }),
        }
    }

    fn operation(&mut self) -> Result<OperationShape, LayoutError> {
        let offset = self.offset;
        match self.byte()? {
            0 => Ok(OperationShape::None),
            1 => Ok(OperationShape::Scalar(self.scalar()?)),
            2 => Ok(OperationShape::Fields(self.ids()?)),
            3 => {
                let tag = self.scalar()?;
                let count = self.len()?;
                let mut variants = Vec::with_capacity(count.min(self.input.len()));
                for _ in 0..count {
                    variants.push(self.ids()?);
                }
                Ok(OperationShape::Variants { tag, variants })
            }
            4 => Ok(OperationShape::PackedElements {
                element: self.id()?,
                stride: self.u32()?,
            }),
            _ => Err(LayoutError::Decode { offset, reason: "unknown operation shape" }),
        }
    }

    fn descriptor(&mut self, schema_version: u32) -> Result<LayoutDescriptor, LayoutError> {
        let kind = self.kind()?;
        let size_offset = self.offset;
        let size = match self.byte()? {
            0 => LayoutSize::Fixed(self.u32()?),
            1 => LayoutSize::Dynamic { base: self.u32()?, stride: self.u32()? },
            _ => {
                return Err(LayoutError::Decode {
                    offset: size_offset,
                    reason: "unknown layout size kind",
                });
            }
        };
        let alignment = self.u32()?;
        let fields = self.fields()?;
        let variant_count = self.len()?;
        let mut variants = Vec::with_capacity(variant_count.min(self.input.len()));
        for _ in 0..variant_count {
            variants.push(VariantLayout {
                fields: self.fields()?,
                payload_size: self.u32()?,
                payload_alignment: self.u32()?,
            });
        }
        let ownership_count = self.len()?;
        let mut ownership = Vec::with_capacity(ownership_count.min(self.input.len()));
        for _ in 0..ownership_count {
            let offset = self.offset;
            ownership.push(match self.byte()? {
                0 => OwnershipPosition::RootBuffer,
                1 => {
                    let count = self.len()?;
                    let mut path = Vec::with_capacity(count.min(self.input.len() / 4));
                    for _ in 0..count {
                        path.push(self.u32()?);
                    }
                    OwnershipPosition::Field(path)
                }
                _ => {
                    return Err(LayoutError::Decode {
                        offset,
                        reason: "unknown ownership position",
                    });
                }
            });
        }
        let header_offset = self.offset;
        let header = match self.byte()? {
            0 => HeaderLayout::None,
            1 => HeaderLayout::PackedList {
                rc: self.rc()?,
                length_offset: self.u32()?,
                capacity_offset: self.u32()?,
                data_offset: self.u32()?,
            },
            _ => {
                return Err(LayoutError::Decode {
                    offset: header_offset,
                    reason: "unknown header layout",
                });
            }
        };
        let operations = OperationShapes {
            copy: self.operation()?,
            duplicate: self.operation()?,
            drop: self.operation()?,
            equality: self.operation()?,
            render: self.operation()?,
            serialization: self.operation()?,
        };
        Ok(LayoutDescriptor {
            schema_version,
            kind,
            size,
            alignment,
            fields,
            variants,
            ownership,
            header,
            operations,
        })
    }
}

fn decode_descriptor(bytes: &[u8]) -> Result<LayoutDescriptor, LayoutError> {
    let mut decoder = Decoder::new(bytes);
    let magic = decoder.bytes(LAYOUT_MAGIC.len())?;
    if magic != LAYOUT_MAGIC {
        return Err(LayoutError::Decode { offset: 0, reason: "invalid layout magic" });
    }
    let schema_version = decoder.u32()?;
    if schema_version != LAYOUT_SCHEMA_VERSION {
        return Err(LayoutError::UnsupportedSchema { found: schema_version });
    }
    let descriptor = decoder.descriptor(schema_version)?;
    if decoder.offset != bytes.len() {
        return Err(LayoutError::TrailingBytes { offset: decoder.offset });
    }
    Ok(descriptor)
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let upper_e = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let first = h
                .wrapping_add(upper_e)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let upper_a = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = upper_a.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0u8; 32];
    for (word, bytes) in state.iter().zip(digest.chunks_exact_mut(4)) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_implementation_matches_the_standard_vector() {
        let digest = LayoutId::from_bytes(sha256(b"abc"));
        assert_eq!(
            digest.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn release_validation_rejects_forged_scalar_alignment() {
        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.alignment = 3;
        let interner = LayoutInterner::new();
        assert_eq!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(
                "alignment must be a non-zero power of two"
            ))
        );
    }

    #[test]
    fn release_validation_rejects_forged_size_header_and_unknown_child() {
        let interner = LayoutInterner::new();
        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.size = LayoutSize::Fixed(4);
        assert!(matches!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let mut descriptor = build_descriptor(LayoutKind::Scalar(ScalarKind::Int), |_| None)
            .expect("scalar descriptor");
        descriptor.header = HeaderLayout::PackedList {
            rc: RcHeader::Required,
            length_offset: 0,
            capacity_offset: 4,
            data_offset: 8,
        };
        assert!(matches!(
            interner.validate_descriptor(&descriptor),
            Err(LayoutError::DescriptorInvariant(_))
        ));

        let missing = LayoutId::from_bytes([9; 32]);
        assert_eq!(
            build_descriptor(LayoutKind::Tuple { fields: vec![missing] }, |_| None),
            Err(LayoutError::UnknownLayout(missing))
        );
    }
}
