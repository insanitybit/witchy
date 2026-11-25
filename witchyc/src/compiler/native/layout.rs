use super::{Result, error};
use crate::compiler::{
    logical_body::{Pointee, Ty},
    semantic_types::{Aggregate, Kind, Sum, Types},
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// The native ABI uses aligned 64-bit lanes. A sum stores its tag followed by
/// room for its largest payload. A buffer owner is one runtime handle.
pub(super) struct Layout<'a> {
    pub types: &'a Types,
    storage: RefCell<BTreeMap<Shape, Rc<Storage>>>,
}

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum Shape {
    Scalar(u8),
    Words(u32),
    Aggregate(u32),
}

pub(super) struct Cell {
    pub offset: u32,
    pub bytes: u8,
}

impl Cell {
    pub fn integer_type(&self) -> cranelift_codegen::ir::Type {
        cranelift_codegen::ir::Type::int(u16::from(self.bytes) * 8).expect("native scalar width")
    }
}

#[derive(Clone, Copy)]
pub(super) struct Field {
    pub ty: Ty,
    pub byte_offset: u32,
    pub lane_offset: u32,
}

pub(super) struct Storage {
    pub size: u32,
    pub align: u32,
    pub cells: Vec<Cell>,
    fields: Vec<Field>,
}

impl Storage {
    fn empty() -> Self {
        Self {
            size: 0,
            align: 1,
            cells: Vec::new(),
            fields: Vec::new(),
        }
    }

    fn append(&mut self, child: &Self) -> u32 {
        let start = self.size.next_multiple_of(child.align);
        self.cells.extend(child.cells.iter().map(|c| Cell {
            offset: start + c.offset,
            bytes: c.bytes,
        }));
        self.size = start + child.size;
        self.align = self.align.max(child.align);
        start
    }
}

impl<'a> Layout<'a> {
    pub fn new(types: &'a Types) -> Self {
        Self {
            types,
            storage: Default::default(),
        }
    }

    /// Byte offsets are shared by every addressable instance of a type, including
    /// stack locals, buffer cells, borrowed fields and array/slice elements.
    pub fn storage(&self, ty: Ty) -> Result<Rc<Storage>> {
        let shape = match ty {
            Ty::Unsigned(s) => Shape::Scalar(s.layout().size),
            Ty::Bool => Shape::Scalar(1),
            Ty::Aggregate(id) => Shape::Aggregate(id),
            _ => Shape::Words(self.lanes(ty)?),
        };
        if let Some(layout) = self.storage.borrow().get(&shape) {
            return Ok(layout.clone());
        }
        let mut layout = Storage::empty();
        match shape {
            Shape::Scalar(bytes) => {
                layout.size = u32::from(bytes);
                layout.align = layout.size;
                layout.cells.push(Cell { offset: 0, bytes });
            }
            Shape::Words(n) => {
                layout.size = n * 8;
                if n != 0 {
                    layout.align = 8;
                }
                layout.cells.extend((0..n).map(|i| Cell {
                    offset: i * 8,
                    bytes: 8,
                }));
            }
            Shape::Aggregate(_) => {
                self.lanes(ty)?; // enforce the existing per-value limit before expanding fields
                let aggregate = self.aggregate(ty)?;
                // The semantic field list also includes every fixed-array element.
                for field in &aggregate.fields {
                    let child = self.storage(field.ty)?;
                    let lane_offset = layout.cells.len() as u32;
                    let byte_offset = layout.append(&child);
                    layout.fields.push(Field {
                        ty: field.ty,
                        byte_offset,
                        lane_offset,
                    });
                }
                layout.size = layout.size.next_multiple_of(layout.align);
            }
        }
        let layout = Rc::new(layout);
        self.storage.borrow_mut().insert(shape, layout.clone());
        Ok(layout)
    }
}

impl Layout<'_> {
    pub fn aggregate(&self, ty: Ty) -> Result<&Aggregate> {
        self.types
            .get(ty)
            .ok_or_else(|| error("expected fixed array or record"))
    }

    pub fn sum(&self, ty: Ty) -> Result<&Sum> {
        self.types.sum(ty).ok_or_else(|| error("expected sum type"))
    }

    /// Used by the alias barriers and forwarding, where every argument must
    /// be independent of caller storage and carry no access to the heap.
    pub fn is_plain_data(&self, ty: Ty) -> bool {
        match ty {
            Ty::Unit | Ty::Unsigned(_) | Ty::I64 | Ty::F64 | Ty::Bool => true,
            Ty::Aggregate(_) => self.types.get(ty).is_some_and(|a| match a.kind {
                Kind::Array(element, _) => self.is_plain_data(element),
                Kind::Record(_) => a.fields.iter().all(|f| self.is_plain_data(f.ty)),
            }),
            Ty::Sum(_) => self
                .types
                .sum(ty)
                .is_some_and(|s| s.arms.iter().all(|t| self.is_plain_data(*t))),
            _ => false,
        }
    }

    pub fn lanes(&self, ty: Ty) -> Result<u32> {
        let count = match ty {
            Ty::Unit => 0,
            Ty::Input | Ty::Borrow { pointee: Pointee::Input, .. } if super::BROKERED => 1,
            Ty::Unsigned(_)
            | Ty::I64
            | Ty::F64
            | Ty::Bool
            | Ty::Console
            | Ty::Allocator
            | Ty::Buffer(_) | Ty::Boxed(_) | Ty::Weak(_) => 1,
            Ty::Host(host) | Ty::Borrow { pointee: Pointee::Host(host), .. }
                if super::host_grant(host).is_some()
                    || (super::BROKERED && matches!(host, crate::compiler::host::Host::Socket | crate::compiler::host::Host::Group | crate::compiler::host::Host::Run))
                    || host == crate::compiler::host::Host::P384Key
                    || host == crate::compiler::host::Host::Task => 1,
            Ty::Borrow {
                pointee: Pointee::Slice(_),
                ..
            } => 2,
            Ty::Borrow { pointee, .. }
                if !matches!(
                    pointee,
                    Pointee::Host(_)
                        | Pointee::Input
                        | Pointee::Param(..)
                ) =>
            {
                1
            }
            Ty::Aggregate(_) => match self.aggregate(ty)?.kind {
                Kind::Array(element, length) => self.lanes(element)? * length,
                Kind::Record(_) => self
                    .aggregate(ty)?
                    .fields
                    .iter()
                    .try_fold(0, |n, field| -> Result<u32> {
                        Ok(n + self.lanes(field.ty)?)
                    })?,
            },
            Ty::Sum(_) => {
                let largest = self
                    .sum(ty)?
                    .arms
                    .iter()
                    .try_fold(0, |n, arm| -> Result<u32> { Ok(n.max(self.lanes(*arm)?)) })?;
                1 + largest
            }
            _ => {
                let name = self
                    .types
                    .type_name(ty)
                    .unwrap_or_else(|_| format!("{ty:?}"));
                return Err(error(format_args!(
                    "unsupported type {name}; use the Wasm target"
                )));
            }
        };
        if count > 256 {
            return Err(error("fixed storage exceeds 256 native lanes"));
        }
        Ok(count)
    }

    pub fn scalar_pair(&self, ty: Ty) -> Result<bool> {
        if !matches!(ty, Ty::Aggregate(_)) {
            return Ok(false);
        }
        let aggregate = self.aggregate(ty)?;
        Ok(matches!(aggregate.kind, Kind::Record(_))
            && aggregate.fields.len() == 2
            && aggregate.fields.iter().all(|field| matches!(field.ty,
                Ty::Unsigned(_) | Ty::I64 | Ty::F64 | Ty::Bool)))
    }

    pub fn float_pair(&self, ty: Ty) -> Result<bool> {
        if !matches!(ty, Ty::Aggregate(_)) || !matches!(self.aggregate(ty)?.kind, Kind::Record(_)) {
            return Ok(false);
        }
        let storage = self.storage(ty)?;
        Ok(matches!(storage.fields.as_slice(), [first, second]
            if first.ty == Ty::F64 && second.ty == Ty::F64))
    }

    pub fn field(&self, ty: Ty, ordinal: u32) -> Result<Field> {
        self.storage(ty)?
            .fields
            .get(ordinal as usize)
            .copied()
            .ok_or_else(|| error("invalid native field projection"))
    }

    pub fn array(&self, ty: Ty, length: u32) -> Result<Ty> {
        match self.aggregate(ty)?.kind {
            Kind::Array(element, count) if count == length => Ok(element),
            _ => Err(error("expected fixed array")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Layout, Ty, Types};
    use crate::compiler::scalar::Scalar;

    #[test]
    fn storage_packs_nested_elements_and_aligns_fields_independently_of_abi_lanes() {
        let mut types = Types::default();
        let bytes = types.array(Ty::Unsigned(Scalar::U8), 3).unwrap();
        let record = types
            .anonymous(vec![
                ("a".into(), Ty::Bool),
                ("b".into(), Ty::Unsigned(Scalar::U16)),
                ("c".into(), bytes),
                ("d".into(), Ty::Unsigned(Scalar::U32)),
            ])
            .unwrap();
        let array = types.array(record, 2).unwrap();
        let layout = Layout::new(&types);
        let stored = layout.storage(record).unwrap();
        assert_eq!((stored.size, stored.align), (12, 4));
        assert_eq!(
            stored
                .fields
                .iter()
                .map(|f| f.byte_offset)
                .collect::<Vec<_>>(),
            [0, 2, 4, 8]
        );
        assert_eq!(layout.storage(array).unwrap().size, 24);
        assert_eq!(layout.lanes(array).unwrap(), 12);
    }
}
