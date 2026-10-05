//! Bounded, typed identities for checked owner families.
//!
//! The source name alone cannot identify an owner's storage: instantiated
//! fields, constructor order, and qualifiers all participate in the key.
//! This identity does not grant layout authority; the catalog and SafeWIR
//! still validate the physical representation and checked-source witness.

use witchy_syntax::ast::{Convention, Type, TypeQual};
use sha2::{Digest, Sha256};

use crate::aggregates::ScalarAggregateShape;

/// Structural identity of a normalized checked source type. Callers still
/// establish checked-source membership and physical layout correspondence.
pub struct SourceTypeKey<'a> {
    pub source: &'a Type,
}

impl SourceTypeKey<'_> {
    pub fn encoding(&self) -> Option<Vec<u8>> {
        let mut encoder = Encoder {
            bytes: b"witchy/source-type-key/v1\0".to_vec(),
            remaining: 4_096,
        };
        encoder.ty(self.source, 0)?;
        (encoder.bytes.len() <= 1_048_576).then_some(encoder.bytes)
    }

    pub fn digest(&self) -> Option<[u8; 32]> {
        Some(*blake3::hash(&self.encoding()?).as_bytes())
    }
}

/// Layout catalog identity for a normalized recursive aggregate. The source
/// key is shared with SafeWIR; neither side may supply a nominal ID by hand.
pub fn recursive_nominal_layout_key(source: &Type) -> Option<[u8; 32]> {
    let key = SourceTypeKey { source }.digest()?;
    let mut digest = Sha256::new();
    digest.update(b"witchy/canonical-layout/recursive-nominal-v2\0");
    digest.update(key);
    Some(digest.finalize().into())
}

/// A checked aggregate family after nominal normalization and shape validation.
pub struct AggregateOwnerKey<'a> {
    pub source: &'a Type,
    pub shape: &'a ScalarAggregateShape,
    /// The checked catalog's owner-field choice for every instantiated slot.
    pub owner_fields: &'a [bool],
}

impl AggregateOwnerKey<'_> {
    pub fn digest(&self) -> Option<[u8; 32]> {
        if self.shape.fields.len() > 4_096
            || self.shape.variants.len() > 4_096
            || self.owner_fields.len() != self.shape.fields.len()
        {
            return None;
        }
        let mut encoder = Encoder {
            bytes: b"witchy/aggregate-owner-key/v1\0".to_vec(),
            remaining: 4_096,
        };
        encoder.ty(self.source, 0)?;
        encoder.text(&self.shape.name)?;
        encoder.byte(u8::from(self.shape.tagged));
        encoder.byte(u8::from(self.shape.packed));
        encoder.count(self.shape.fields.len())?;
        for (field, owner_field) in self.shape.fields.iter().zip(self.owner_fields) {
            encoder.ty(field, 0)?;
            encoder.byte(u8::from(*owner_field));
        }
        encoder.count(self.shape.variants.len())?;
        for variant in self.shape.variants.iter() {
            encoder.text(&variant.name)?;
            encoder.count(variant.first_field)?;
            encoder.count(variant.arity)?;
        }
        if encoder.bytes.len() > 1_048_576 {
            return None;
        }
        Some(*blake3::hash(&encoder.bytes).as_bytes())
    }
}

/// One normalized callable family and its checked capture witness variants.
/// The corresponding owner-field classes are chosen by the layout catalog
/// from the same final opaque-family membership used for physical storage.
pub struct ClosureOwnerKey<'a> {
    pub signature: &'a Type,
    pub variants: &'a [(u32, Vec<Type>)],
    pub owner_fields: &'a [Vec<bool>],
}

impl ClosureOwnerKey<'_> {
    pub fn digest(&self) -> Option<[u8; 32]> {
        if self.variants.len() > 4_096 || self.owner_fields.len() != self.variants.len() {
            return None;
        }
        let mut encoder = Encoder {
            bytes: b"witchy/closure-owner-key/v1\0".to_vec(),
            remaining: 4_096,
        };
        encoder.ty(self.signature, 0)?;
        encoder.count(self.variants.len())?;
        // The sequence fixes physical tag order. Global witness indices are
        // checked elsewhere and do not change this family's storage shape.
        for ((_, fields), owner_fields) in self.variants.iter().zip(self.owner_fields) {
            if fields.len() > 4_096 || owner_fields.len() != fields.len() {
                return None;
            }
            encoder.count(fields.len())?;
            for (field, owner_field) in fields.iter().zip(owner_fields) {
                encoder.ty(field, 0)?;
                encoder.byte(u8::from(*owner_field));
            }
        }
        if encoder.bytes.len() > 1_048_576 {
            return None;
        }
        Some(*blake3::hash(&encoder.bytes).as_bytes())
    }
}

struct Encoder {
    bytes: Vec<u8>,
    remaining: usize,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn count(&mut self, value: usize) -> Option<()> {
        self.bytes
            .extend_from_slice(&u32::try_from(value).ok()?.to_le_bytes());
        Some(())
    }

    fn text(&mut self, value: &str) -> Option<()> {
        if value.len() > 1_024 || self.bytes.len().checked_add(value.len() + 4)? > 1_048_576 {
            return None;
        }
        self.count(value.len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Some(())
    }

    fn types(&mut self, types: &[Type], depth: usize) -> Option<()> {
        if types.len() > self.remaining {
            return None;
        }
        self.count(types.len())?;
        for ty in types {
            self.ty(ty, depth + 1)?;
        }
        Some(())
    }

    fn ty(&mut self, ty: &Type, depth: usize) -> Option<()> {
        if depth > 128 || self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        match ty {
            Type::Named(name, arguments) => {
                self.byte(0);
                self.text(name)?;
                self.types(arguments, depth)?;
            }
            Type::Tuple(fields) => {
                self.byte(1);
                self.types(fields, depth)?;
            }
            Type::Slice(element) => {
                self.byte(2);
                self.ty(element, depth + 1)?;
            }
            Type::RecordCompose { base, fields } => {
                self.byte(3);
                self.ty(base, depth + 1)?;
                self.count(fields.len())?;
                for (name, field) in fields {
                    self.text(name)?;
                    self.ty(field, depth + 1)?;
                }
            }
            Type::Fn(parameters, result, conventions, qualifiers) => {
                if !conventions.is_empty() && conventions.len() != parameters.len() {
                    return None;
                }
                self.byte(4);
                self.types(parameters, depth)?;
                self.ty(result, depth + 1)?;
                // Preserve the exact checked Type identity. Legacy empty
                // conventions and an explicit all-Let vector compare unequal
                // as Types, so their sort keys must remain distinct too.
                self.count(conventions.len())?;
                for convention in conventions {
                    self.byte(match *convention {
                        Convention::Let => 0,
                        Convention::Borrow => 1,
                        Convention::Var => 2,
                        Convention::Own => 3,
                    });
                }
                self.byte(u8::from(qualifiers.pure));
                self.byte(u8::from(qualifiers.once));
            }
            Type::Qualified(qualifier, inner) => {
                self.byte(5);
                match qualifier {
                    TypeQual::Frozen => self.byte(0),
                    TypeQual::Unique => self.byte(1),
                    TypeQual::View => self.byte(2),
                    TypeQual::Borrow(lifetime) => {
                        self.byte(3);
                        self.text(lifetime)?;
                    }
                    TypeQual::LegacyBorrow(lifetime) => {
                        self.byte(4);
                        self.text(lifetime)?;
                    }
                    TypeQual::BorrowMut(lifetime) => {
                        self.byte(5);
                        self.text(lifetime)?;
                    }
                }
                self.ty(inner, depth + 1)?;
            }
            Type::Dyn(name, arguments) => {
                self.byte(6);
                self.text(name)?;
                self.types(arguments, depth)?;
            }
        }
        Some(())
    }
}
