//! One descriptor drives concrete aggregate typing, ABI shapes and frame storage.
//! Record ownership follows one field spine; physical cells never grant Copy.
use super::ir::Ty;
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeSet, HashMap};

type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: impl Into<std::borrow::Cow<'static, str>>) -> Diagnostic {
    Diagnostic::new("W0403", 0..0, message)
}
/// The most record, array, sum and Buffer types one program may lay out, counted together.
/// Nothing is sized by it: type ids are `u32` in every table, receipt and proof name, and no
/// encoding packs one into fewer bits. It bounds the compiler's work only (the tables are
/// searched linearly), as the other program size limits do (docs/decisions.md, 2026-11-22).
pub(super) const MAX_DESCRIPTORS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Record(String),
    Array(Ty, u32),
}
#[derive(Clone, Debug)]
pub(super) struct Field {
    pub name: String,
    pub ty: Ty,
    pub slot: u32,
    pub byte_offset: u64,
}
#[derive(Clone, Debug)]
pub(super) struct Aggregate {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub flat: Vec<Ty>,
    pub size: u64,
    pub alignment: u64,
    /// It holds a reference, so it lives in locals only and has no stored form.
    pub view: bool,
}
#[derive(Default, Debug)]
pub(super) struct Layouts {
    pub aggregates: Vec<Aggregate>,
    sums: Vec<super::sum::Sum>,
    console_used: bool,
    input_used: bool,
    allocator_used: bool,
    heap_operations: bool,
    pub boxed_operations: bool,
    buffers: Vec<super::buffer::Buffer>,
    /// Each slice type's element and its stride in bytes (docs/design-slices.md).
    slices: Vec<(Ty, u64)>,
    pub static_text: bool,
    templates: Option<String>,
    traits: HashMap<Ty, super::properties::Properties>,
}
impl Layouts {
    pub fn initialize(&mut self, types: &super::semantic_types::Types) {
        self.console_used=types.uses_console(); self.input_used=types.uses_input(); self.allocator_used=types.uses_allocator(); self.heap_operations=types.uses_heap();
        self.templates=types.templates_receipt();
    }
    /// Whether the emitted program holds the input grant.
    pub fn input(&mut self, used: bool) { self.input_used = used; }
    pub fn output(&mut self, used: bool) { self.console_used = used; }
    /// Whether the emitted program holds an allocator, and whether it uses the heap at all.
    pub fn heap(&mut self, allocator: bool, heap: bool) { self.allocator_used = allocator; self.heap_operations = heap; }
    pub fn remember(&mut self, ty: Ty, properties: super::properties::Properties) {
        self.traits.entry(ty).or_insert(properties);
        self.allocator_used |= ty==Ty::Allocator;
    }
    /// A Buffer type whose element is laid out afterwards by `buffer_element`: the
    /// handle's own shape does not depend on what the cells hold.
    /// The slice type of these elements: `&[T]` is the address of the first and the count.
    pub fn slice(&mut self, element: Ty) -> Result<Ty> {
        if let Some(i) = self.slices.iter().position(|(e, _)| *e == element) { return Ok(Ty::Slice(i as u32)); }
        let (size, alignment) = self.storage(element)?;
        let stride = size.checked_add(alignment - 1).ok_or_else(|| error("layout overflow"))? / alignment * alignment;
        self.slices.push((element, stride.max(1)));
        Ok(Ty::Slice(self.slices.len() as u32 - 1))
    }
    /// Room for one more type. The count is of the whole program, the library files it imports
    /// included, and of every type argument list a generic type is used with; it has no one
    /// place in the source, so the message says so and gives the numbers.
    fn room(&self) -> Result<()> {
        let (records, sums, buffers) = (self.aggregates.len(), self.sums.len(), self.buffers.len());
        if records + sums + buffers < MAX_DESCRIPTORS { return Ok(()); }
        Err(error(format!("the whole program lays out more than {MAX_DESCRIPTORS} types ({records} records and arrays, {sums} sum types, {buffers} Buffer types, counting each imported file's and each type argument list a generic type is given): this is a limit on the compiler's work, not on one file; use fewer distinct instances of generic types, or split the program")))
    }
    pub fn slice_element(&self, id: u32) -> (Ty, u64) { self.slices[id as usize] }
    pub fn buffer(&mut self, element_name: String) -> Result<Ty> {
        self.room()?;
        let index = self.buffers.len() as u32;
        self.buffers.push(super::buffer::Buffer { element: Ty::Unit, element_name, stride: 0, alignment: 1 });
        Ok(Ty::Buffer(index))
    }
    pub fn buffer_element(&mut self, buffer: Ty, element: Ty) -> Result<()> {
        let Ty::Buffer(index) = buffer else { unreachable!("a reserved Buffer type") };
        if !(self.properties(element).copy || self.supported_linear(element)) || matches!(element, Ty::Unit | Ty::Tag | Ty::Borrow { .. }) {
            return Err(error("Buffer elements require concrete non-unit storage: Copy values or one owner"));
        }
        let (stride, alignment) = self.storage(element)?;
        if stride == 0 { return Err(error("Buffer elements require nonzero storage size")); }
        if alignment > 8 { return Err(error("Buffer element alignment exceeds 8")); }
        let entry = &mut self.buffers[index as usize];
        (entry.element, entry.stride, entry.alignment) = (element, stride, alignment);
        Ok(())
    }
    pub fn supported_linear(&self, ty: Ty) -> bool {
        match ty {
            // A task must be closed: it holds its memory, and may hold owners that are paused.
            Ty::Boxed | Ty::Buffer(_) | Ty::Host(super::host::Host::File(super::host::Right::Write) | super::host::Host::Task | super::host::Host::P384Key) => true,
            Ty::Aggregate(_) => self.owner_field(ty).is_some(),
            Ty::Sum(index) => self.sums[index as usize].arms.iter().any(|ty| self.supported_linear(*ty)),
            _ => false,
        }
    }
    pub fn buffers_receipt(&self) -> Option<String> {
        if self.buffers.is_empty() { return None; }
        let entries = self.buffers.iter().enumerate().map(|(id, buffer)| {
            format!("{{\"id\":{id},\"name\":\"Buffer<{}>\",\"ownership\":\"linear\",\"carrier\":\"i64\",\"frame_storage\":true,\"element\":\"{}\",\"element_stride\":{},\"element_alignment\":{}}}",
                buffer.element_name, buffer.element_name, buffer.stride, buffer.alignment)
        }).collect::<Vec<_>>().join(",");
        Some(format!("[{entries}]"))
    }
    fn nonzero_owner(&self, ty: Ty) -> Option<Ty> {
        match ty {
            Ty::Buffer(_) => Some(ty), // every successful allocation token is encoded plus one
            Ty::Aggregate(_) => {
                let [field] = self.get(ty)?.fields.as_slice() else { return None };
                self.nonzero_owner(field.ty)
            }
            _ => None,
        }
    }
    /// One tag, then each variant's own carriers side by side.
    pub fn sum_layout(&mut self, name: String, arms: Vec<Ty>, names: Vec<String>) -> Result<Ty> {
        if arms.iter().any(|ty| matches!(ty, Ty::Tag)) {
            return Err(error("a variant cannot hold a tag"));
        }
        if arms.iter().any(|ty| { let p = self.properties(*ty); !p.copy && !p.implicit_discard && !self.supported_linear(*ty) }) {
            return Err(error("unsupported nondiscardable Result payload"));
        }
        if let Some(index) = self.sums.iter().position(|sum| sum.arms == arms && sum.names == names && sum.name == name) { return Ok(Ty::Sum(index as u32)); }
        self.room()?;
        if let [left, right] = arms.as_slice() {
            let eligible = match (*left, *right) {
                (Ty::Unit, payload) => self.nonzero_owner(payload).map(|word| (0, word)),
                (payload, Ty::Unit) => self.nonzero_owner(payload).map(|word| (1, word)),
                _ => None,
            };
            if let Some((zero, word)) = eligible {
                let (bytes, alignment) = self.storage(word)?;
                let counts = arms.iter().map(|arm| u32::from(*arm != Ty::Unit)).collect();
                let id = self.sums.len() as u32;
                self.sums.push(super::sum::Sum {
                    name, arms, names, starts: vec![0, 0], counts, carriers: vec![word],
                    stored: Some((vec![0], bytes, alignment)), arm_offsets: vec![0, 0], zero_arm: Some(zero),
                });
                return Ok(Ty::Sum(id));
            }
        }
        let mut carriers = vec![Ty::Tag];
        let (mut starts, mut counts) = (Vec::new(), Vec::new());
        for arm in &arms {
            let flat = self.flat(*arm);
            starts.push(carriers.len() as u32);
            counts.push(flat.len() as u32);
            carriers.extend(flat);
        }
        if carriers.len() > 32 { return Err(error("a sum value exceeds 32 private carriers including its tag")); }
        // In memory: the tag, then each variant's value laid out as its own type, all of them in
        // the one space after the tag: a value holds one variant, so they share it. Writes of
        // a variant's cells are conditional on the tag (`guards`), so one never overwrites another.
        let mut stored = Some((vec![0u64], 4u64, 4u64));
        let mut arm_offsets = Vec::new();
        for arm in &arms {
            let Some((offsets, size, alignment)) = &mut stored else { break };
            if *arm == Ty::Unit { arm_offsets.push(4); continue; }
            let (Ok((bytes, align)), Ok(cells)) = (self.storage(*arm), self.cells(*arm)) else { stored = None; break };
            *alignment = (*alignment).max(align);
            let offset = 4u64.div_ceil(align) * align;
            arm_offsets.push(offset);
            offsets.extend(cells.iter().map(|cell| offset + cell.byte_offset));
            *size = (*size).max(offset + bytes);
        }
        if let Some((offsets, size, alignment)) = &mut stored {
            debug_assert_eq!(offsets.len(), carriers.len());
            *size = size.div_ceil(*alignment) * *alignment;
        }
        let index = self.sums.len() as u32;
        self.sums.push(super::sum::Sum { name, arms, names, starts, counts, carriers, stored, arm_offsets, zero_arm: None });
        Ok(Ty::Sum(index))
    }
    /// For each carrier slot of a value of `ty`: the (tag slot, tag) pairs that must all hold for
    /// the slot to be part of the value. A slot of a variant is live only while its sum's tag
    /// says that variant, at every level of nesting. Stored variants share bytes, so a cell is
    /// written to memory only when it is live.
    pub fn guards(&self, ty: Ty) -> Vec<Vec<(u32, u8)>> {
        let mut out = vec![Vec::new(); self.flat(ty).len()];
        self.guards_into(ty, 0, &[], &mut out, 0);
        out
    }
    fn guards_into(&self, ty: Ty, base: u32, outer: &[(u32, u8)], out: &mut [Vec<(u32, u8)>], depth: usize) {
        if depth > 64 { return; }
        if let Some(layout) = self.get(ty) {
            for field in &layout.fields { self.guards_into(field.ty, base + field.slot, outer, out, depth + 1); }
        } else if let Some(sum) = self.sum(ty) {
            out[base as usize] = outer.to_vec();
            // Only variants that share bytes with another need a condition.
            let shared = sum.counts.iter().filter(|count| **count > 0).count() >= 2;
            for (tag, arm) in sum.arms.iter().enumerate() {
                let mut inner = outer.to_vec();
                if shared { inner.push((base, tag as u8)); }
                self.guards_into(*arm, base + sum.starts[tag], &inner, out, depth + 1);
            }
        } else {
            for slot in base..base + self.flat(ty).len() as u32 { out[slot as usize] = outer.to_vec(); }
        }
    }
    pub fn sum(&self, ty: Ty) -> Option<&super::sum::Sum> {
        if let Ty::Sum(index) = ty { self.sums.get(index as usize) } else { None }
    }
    pub fn sums_receipt(&self) -> Option<String> {
        if self.sums.is_empty() { return None; }
        let entries = self.sums.iter().enumerate().map(|(id, sum)| {
            let variants = sum.names.iter().enumerate().map(|(tag, name)| {
                format!("{{\"name\":\"{name}\",\"tag\":{tag},\"type\":\"{:?}\",\"carrier_start\":{},\"carrier_count\":{}}}",
                    sum.arms[tag], sum.starts[tag], sum.counts[tag])
            }).collect::<Vec<_>>().join(",");
            let properties = self.properties(Ty::Sum(id as u32));
            let ownership = if properties.copy { "copyable" } else if properties.implicit_discard { "affine" } else { "linear" };
            let kind = if sum.names == ["Ok", "Err"] { "Result" } else { "Sum" };
            // In locals a sum is carriers; in a buffer or behind a reference it has the stored layout, when it has one.
            let storage = match &sum.stored { Some((_, size, alignment)) => format!("true,\"storage_bytes\":{size},\"alignment\":{alignment}"), None => "false".into() };
            // Key order as documented (docs/wasm-results.md): representation, storage, then how the variant is told.
            let (representation, tag) = match sum.zero_arm {
                Some(zero) => ("zero-niche", format!("\"zero_variant\":{zero}")),
                None => ("private-registers", "\"tag_carrier\":\"i32\",\"tag_slot\":0".into()),
            };
            format!("{{\"id\":{id},\"kind\":\"{kind}\",\"representation\":\"{representation}\",\"storage\":{storage},{tag},\"carrier_count\":{},\"variants\":[{variants}],\"ownership\":\"{ownership}\",\"implicit_discard\":{}}}", sum.carriers.len(), properties.implicit_discard)
        }).collect::<Vec<_>>().join(",");
        Some(format!("[{entries}]"))
    }
    /// Template coverage is separate from the concrete layouts in `receipt`.
    /// No layout is manufactured for an unused generic record.
    pub fn templates_receipt(&self) -> Option<String> { self.templates.clone() }
    pub fn aggregate(&mut self, kind: Kind, fields: Vec<(String, Ty)>) -> Result<Ty> {
        // A record has at most 32 fields and 32 numbers in all; an array alone up to 256 of each.
        let most = if matches!(kind, Kind::Array(..)) { 256 } else { 32 };
        // A brand (docs/design-brands.md) is a type argument only: nothing holds one as a value.
        if fields.is_empty() && matches!(&kind, Kind::Record(name) if name.starts_with("Brand#")) {
            return Err(error("a brand is a type argument only, never a value: a type that holds a value of its type parameter (`Option<D>`, `Vec<D>`) cannot take a brand"));
        }
        if fields.is_empty() || fields.len() > most {
            return Err(error("aggregate shape limit"));
        }
        self.room()?;
        let (mut offset, mut alignment) = (0u64, 1u64);
        let (mut named, mut flat, mut output) = (BTreeSet::new(), Vec::new(), Vec::new());
        let mut view = false;
        for (name, ty) in fields {
            // A capability is affine but owns no storage: a record may carry it, and is not Copy.
            let capability = matches!(ty, Ty::Console | Ty::Input | Ty::Allocator | Ty::Host(_));
            if !self.properties(ty).copy && !capability {
                if !matches!(kind, Kind::Record(_)) || !self.supported_linear(ty) {
                    return Err(error("aggregate owner fields require one supported record spine"));
                }
            }
            if capability && !matches!(kind, Kind::Record(_)) { return Err(error("a capability sits in a named record, not an array or anonymous record")); }
            if matches!(ty, Ty::Unit | Ty::Tag | Ty::Borrow { mutable: true, .. }) || !named.insert(name.clone()) {
                return Err(error("unit or duplicate aggregate field"));
            }
            // A view is never in memory. Its size is nominal, so that the descriptor is whole.
            let inner = self.get(ty).filter(|inner| inner.view).map(|inner| (inner.size, inner.alignment));
            let reference = matches!(ty, Ty::Borrow { .. });
            if (reference || inner.is_some()) && !matches!(kind, Kind::Record(_)) { return Err(error("an array holds stored values, not references")); }
            view |= reference || inner.is_some();
            // A `&[T]` is an address and a count: 16 bytes, at 8.
            let slice = matches!(ty, Ty::Borrow { pointee: super::borrow::Pointee::Slice(_), .. });
            let (size, align) = if slice { (16, 8) } else if reference { (4, 4) } else if let Some(extent) = inner { extent } else { self.storage(ty)? };
            alignment = alignment.max(align);
            offset = offset
                .checked_add(align - 1)
                .ok_or_else(|| error("layout overflow"))?
                / align
                * align;
            let slot = flat.len() as u32;
            flat.extend(self.flat(ty));
            if flat.len() > most {
                return Err(error(super::semantic_types::SLOTS));
            }
            output.push(Field {
                name,
                ty,
                slot,
                byte_offset: offset,
            });
            offset = offset
                .checked_add(size)
                .ok_or_else(|| error("layout overflow"))?;
        }
        let size = offset
            .checked_add(alignment - 1)
            .ok_or_else(|| error("layout overflow"))?
            / alignment
            * alignment;
        let index = self.aggregates.len() as u32;
        self.aggregates.push(Aggregate {
            kind,
            fields: output,
            flat,
            size,
            alignment,
            view,
        });
        Ok(Ty::Aggregate(index))
    }
    pub fn get(&self, ty: Ty) -> Option<&Aggregate> {
        if let Ty::Aggregate(index) = ty {
            self.aggregates.get(index as usize)
        } else {
            None
        }
    }
    pub fn flat(&self, ty: Ty) -> Vec<Ty> {
        match ty {
            Ty::Unit => vec![],
            Ty::Aggregate(index) => self.aggregates[index as usize].flat.clone(),
            Ty::Sum(index) => self.sums[index as usize].carriers.clone(),
            // `&[T]`: a reference to the first element, then how many there are.
            Ty::Borrow { mutable, pointee: super::borrow::Pointee::Slice(i) } => {
                let element = self.slices[i as usize].0;
                let first = super::borrow::Pointee::from_ty(element).expect("a slice's element is storable");
                vec![Ty::Borrow { mutable, pointee: first }, Ty::Unsigned(super::scalar::Scalar::U64)]
            }
            _ => vec![ty],
        }
    }
    pub fn receipt(&self) -> String {
        let entries = self.aggregates.iter().enumerate().map(|(index, layout)| {
            let kind = match &layout.kind { Kind::Record(name) => format!("record:{name}"), Kind::Array(_, count) => format!("array:{count}") };
            let fields = layout.fields.iter().map(|field| format!("{{\"name\":\"{}\",\"type\":\"{:?}\",\"slot\":{},\"byte_offset\":{}}}", field.name, field.ty, field.slot, field.byte_offset)).collect::<Vec<_>>().join(",");
            // Every field that holds an owner, not only the first.
            let owners = layout.fields.iter().filter(|field| !self.properties(field.ty).copy).map(|field| format!("\"{}\"", field.name)).collect::<Vec<_>>();
            let ownership = if owners.is_empty() { String::new() } else { format!(",\"ownership\":\"linear\",\"implicit_discard\":false,\"owner_fields\":[{}]", owners.join(",")) };
            format!("{{\"id\":{index},\"kind\":\"{kind}\",\"storage_bytes\":{},\"alignment\":{},\"fields\":[{fields}]{ownership}}}", layout.size, layout.alignment)
        }).collect::<Vec<_>>().join(",");
        format!("[{entries}]")
    }
    /// Bounded leaf traversal preserves nested/trailing padding through field
    /// byte offsets; it never infers storage by packing flattened registers.
    pub fn cells(&self, root: Ty) -> Result<Vec<super::cell::Cell>> {
        let mut pending = vec![(root, 0u32, 0u64)];
        let mut cells = Vec::new();
        let mut visits = 0;
        while let Some((ty, slot, byte_offset)) = pending.pop() {
            visits += 1;
            if visits > 4096 { return Err(error("initialized-cell traversal limit")); }
            if let Some(layout) = self.get(ty) {
                for field in layout.fields.iter().rev() {
                    pending.push((field.ty, slot + field.slot, byte_offset.checked_add(field.byte_offset)
                        .ok_or_else(|| error("cell offset overflow"))?));
                }
            } else if let Some(sum) = self.sum(ty) {
                let (offsets, ..) = sum.stored.as_ref().ok_or_else(|| error("a sum that holds a reference has no stored form"))?;
                for (index, carrier) in sum.carriers.iter().enumerate().rev() {
                    pending.push((*carrier, slot + index as u32, byte_offset.checked_add(offsets[index])
                        .ok_or_else(|| error("cell offset overflow"))?));
                }
            } else if let Ty::Borrow { pointee: super::borrow::Pointee::Slice(_), .. } = ty {
                // `&[T]`: its first word, the address, at the start; the count 8 bytes on.
                let words = self.flat(ty);
                if cells.len() + 2 > 256 { return Err(error("at most 256 initialized cells")); }
                cells.push(super::cell::Cell { slot, ty: words[0], byte_offset });
                cells.push(super::cell::Cell { slot: slot + 1, ty: words[1], byte_offset: byte_offset.checked_add(8).ok_or_else(|| error("cell offset overflow"))? });
            } else if matches!(ty, Ty::Unsigned(_) | Ty::I64 | Ty::F64 | Ty::Bool | Ty::Console | Ty::Input | Ty::Allocator | Ty::Host(_) | Ty::Boxed | Ty::Buffer(_) | Ty::Tag | Ty::Borrow { .. }) {
                if cells.len() >= 256 { return Err(error("at most 256 initialized cells")); }
                cells.push(super::cell::Cell { slot, ty, byte_offset });
            } else { return Err(error("initialized cells require non-unit copyable storage")); }
        }
        Ok(cells)
    }
    pub fn properties(&self, ty: Ty) -> super::properties::Properties {
        self.traits.get(&ty).copied().unwrap_or_else(|| {
            // Tag is private physical padding, never a semantic/source type.
            if ty==Ty::Tag { super::properties::Properties{copy:true,implicit_discard:true} }
            else { panic!("concrete descriptor missing semantic properties") }
        })
    }
    pub fn owner_field(&self, ty: Ty) -> Option<&Field> {
        let layout = self.get(ty)?;
        if !matches!(layout.kind, Kind::Record(_)) { return None; }
        layout.fields.iter().find(|field| !self.properties(field.ty).copy)
    }
    pub fn buffers(&self) -> &[super::buffer::Buffer] { &self.buffers }
    pub fn uses_heap(&self) -> bool { self.allocator_used || self.heap_operations }
    pub fn capabilities_receipt(&self) -> Option<String> {
        let mut entries = Vec::new();
        for (used, ty, name, entry) in [(self.console_used, Ty::Console, "Console<Write>", "main-output-grant"),
            (self.input_used, Ty::Input, "Console<Read>", "main-input-grant"),
            (self.allocator_used, Ty::Allocator, "Allocator", "main-allocator-grant")] {
            if used {
                let (bytes, alignment) = self.storage(ty).expect("opaque capability frame representation");
                let carrier = if ty == Ty::Input { "i64" } else { "i32" };
                entries.push(format!("{{\"name\":\"{name}\",\"ownership\":\"affine\",\"carrier\":\"{carrier}\",\"frame_bytes\":{bytes},\"frame_alignment\":{alignment},\"record_storage\":false,\"entry\":\"{entry}\"}}"));
            }
        }
        if entries.is_empty() { None } else { Some(format!("[{}]", entries.join(","))) }
    }
    pub fn storage(&self, ty: Ty) -> Result<(u64, u64)> {
        Ok(match ty {
            Ty::Unsigned(s) => {
                let l = s.layout();
                (l.size as u64, l.alignment as u64)
            }
            Ty::I64 | Ty::F64 => (8, 8),
            Ty::Bool => (1, 1),
            Ty::Console | Ty::Allocator => (4, 4),
            Ty::Input | Ty::Host(_) => (8, 8),
            Ty::Boxed => (4, 4),
            Ty::Buffer(_) => (8, 8),
            Ty::Unit => (0, 1),
            Ty::Tag => (4, 4),
            Ty::Sum(i) => {
                let (_, size, alignment) = self.sums[i as usize].stored.as_ref().ok_or_else(|| error("a sum that holds a reference has no stored form"))?;
                (*size, *alignment)
            }
            Ty::Borrow { .. } => return Err(error("references have no storage layout")),
            Ty::Slice(_) => return Err(error("a slice `[T]` has no size of its own: it is used behind `&`")),
            Ty::Aggregate(i) => {
                let a = &self.aggregates[i as usize];
                // A view is stored only where a shared reference to it needs an address
                // (docs/design-references-to-views.md); the checker keeps it out of heap cells.
                (a.size, a.alignment)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::scalar::Scalar;
    use crate::ast::Type;
    use super::super::concrete::test_layouts;
    fn named(n:&str)->Type {Type::Named(n.into(),vec![])}
    fn result(a:Type,b:Type)->Type {Type::Named("Result".into(),vec![a,b])}
    fn buffer(t:Type)->Type {Type::Named("Buffer".into(),vec![t])}
    #[test]
    fn stored_variants_share_the_bytes_after_the_tag_and_writes_are_guarded_by_it() {
        let (layouts,t)=test_layouts("",&[result(named("U64"),named("U64")),result(named("U8"),named("U64")),result(Type::Unit,named("U64"))]).unwrap();
        // Tag, padding to 8, one U64 for whichever variant it is: 16 bytes, not 24.
        assert_eq!(layouts.storage(t[0]).unwrap(),(16,8));
        let cells=layouts.cells(t[0]).unwrap();
        assert_eq!(cells.iter().map(|c|c.byte_offset).collect::<Vec<_>>(),vec![0,8,8]);
        assert_eq!(layouts.guards(t[0]),vec![vec![],vec![(0,0)],vec![(0,1)]]);
        // A U8 variant sits right after the tag; the U64 one at 8; the value is 16 bytes.
        assert_eq!(layouts.cells(t[1]).unwrap().iter().map(|c|c.byte_offset).collect::<Vec<_>>(),vec![0,4,8]);
        assert_eq!(layouts.storage(t[1]).unwrap(),(16,8));
        // One variant with a value shares with nothing: no condition on its write.
        assert_eq!(layouts.guards(t[2]),vec![vec![],vec![]]);
    }
    #[test]
    fn a_sum_is_stored_as_its_tag_and_carriers_and_unit_has_no_payload_carrier() {
        let (layouts,t)=test_layouts("",&[result(Type::Unit,Type::Unit),result(Type::Unit,named("U64")),result(named("U64"),Type::Unit)]).unwrap();
        assert_eq!(layouts.flat(t[0]),vec![Ty::Tag]);
        assert_eq!(layouts.sum(t[0]).unwrap().starts,[1,1]);
        assert_eq!(layouts.sum(t[0]).unwrap().counts,[0,0]);
        assert_eq!(layouts.sum(t[1]).unwrap().starts,[1,1]);
        assert_eq!(layouts.sum(t[1]).unwrap().counts,[0,1]);
        assert_eq!(layouts.sum(t[2]).unwrap().starts,[1,2]);
        assert_eq!(layouts.sum(t[2]).unwrap().counts,[1,0]);
        assert_eq!(layouts.storage(t[0]).unwrap(),(4,4));
        assert_eq!(layouts.storage(t[1]).unwrap(),(16,8));
        let cells=layouts.cells(t[1]).unwrap();
        assert_eq!(cells.iter().map(|c|(c.slot,c.ty,c.byte_offset)).collect::<Vec<_>>(),vec![(0,Ty::Tag,0),(1,Ty::U64,8)]);
    }
    #[test]
    fn combined_descriptor_cap_is_enforced_by_both_interners() {
        // 1024 arrays and enough records to leave room for exactly one more type.
        let mut requests=Vec::new();
        for s in Scalar::ALL {for n in 1..=256 {requests.push(Type::Array{element:Box::new(named(s.layout().name)),length:n});}}
        let records=|n:usize|(0..n).map(|i|format!("type R{i}:\n    v: U8\n")).collect::<String>();
        let source=records(MAX_DESCRIPTORS-requests.len()-1);
        let first=result(named("U8"),named("U64"));
        requests.push(first);
        test_layouts(&source,&requests).unwrap();
        requests.push(result(named("Bool"),named("U64")));
        assert!(test_layouts(&source,&requests).is_err());
        // The same with a record as the type too many.
        requests.pop();
        assert!(test_layouts(&records(MAX_DESCRIPTORS-requests.len()+1),&requests).is_err());
    }
    #[test]
    fn buffer_is_a_nominal_linear_frame_token_and_result_inherits_its_duty() {
        let (layouts,t)=test_layouts("",&[buffer(named("U8")),buffer(named("U16")),buffer(named("U8")),result(buffer(named("U8")),named("U64"))]).unwrap();
        assert_ne!(t[0],t[1]);assert_eq!(t[0],t[2]);
        let p=layouts.properties(t[0]);assert!(!p.copy&&!p.implicit_discard);
        assert_eq!(layouts.storage(t[0]).unwrap(),(8,8));
        let cells=layouts.cells(t[0]).unwrap();assert_eq!(cells.len(),1);assert_eq!(cells[0].ty,t[0]);
        assert!(super::super::borrow::Pointee::from_ty(t[0]).is_some());
        let p=layouts.properties(t[3]);assert!(!p.copy&&!p.implicit_discard);
        assert!(layouts.sums_receipt().unwrap().contains("\"ownership\":\"linear\",\"implicit_discard\":false"));
    }
    #[test]
    fn buffer_descriptors_share_the_existing_total_type_budget() {
        // Records, arrays, sums and Buffers count together, to `MAX_DESCRIPTORS`.
        let mut requests=vec![buffer(named("U8"))];
        for s in Scalar::ALL {for n in 1..=256 {requests.push(Type::Array{element:Box::new(named(s.layout().name)),length:n});}}
        let records=|n:usize|(0..n).map(|i|format!("type R{i}:\n    v: U8\n")).collect::<String>();
        let fill=MAX_DESCRIPTORS-requests.len();
        test_layouts(&records(fill),&requests).unwrap();
        let refused=test_layouts(&records(fill+1),&requests).unwrap_err();
        assert!(refused.message.contains("the whole program lays out more than 4096 types"),"{}",refused.message);
        assert!(refused.message.contains("sum types, 1 Buffer types"),"{}",refused.message);
        // What the old limit of 128 refused: a server's imports and a library's own types.
        let source=(0..300).map(|i|format!("type R{i}:\n    value: U8\n")).collect::<String>();
        test_layouts(&source,&[buffer(named("U8"))]).unwrap();
    }
}
