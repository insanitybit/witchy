//! Construct the catalog's owned `List(String)` entry value before guest
//! execution. No guest callback or allocator runs while this image is built.

use super::{Error, Result, Val, Vm, VmState};

struct Image {
    bytes: Vec<u8>,
    root: u32,
    end: u32,
}

/// RFC-0179: how a guest reference names a byte. A standard heap's references
/// are payload byte addresses. A large heap's count eight-byte units and name
/// the unit of the object's RC header, its payload eight bytes later, so every
/// object starts on a unit boundary and its RC size is a whole number of
/// units. The allocator globals hold units: a header's unit under a large
/// heap, a byte under a standard one.
#[derive(Clone, Copy)]
pub(super) struct Refs {
    shift: u32,
}

impl Refs {
    pub(super) fn for_heap(heap: witchy_wir::HeapMode) -> Self {
        match heap {
            witchy_wir::HeapMode::Standard => Self { shift: 0 },
            witchy_wir::HeapMode::Large => Self { shift: 3 },
        }
    }

    /// The payload's first byte of the object `reference` names.
    pub(super) fn byte(self, reference: u32) -> u64 {
        match self.shift {
            0 => u64::from(reference),
            shift => (u64::from(reference) << shift) + 8,
        }
    }

    /// The first byte of an allocator unit (a global's value).
    pub(super) fn unit_byte(self, unit: u32) -> u64 {
        u64::from(unit) << self.shift
    }

    /// The reference to the object whose payload starts at `byte`.
    fn reference(self, byte: u64) -> Result<u32> {
        let unit = match self.shift { 0 => byte, shift => (byte - 8) >> shift };
        u32::try_from(unit).map_err(|_| Error::msg("launch argument address overflow"))
    }

    /// The allocator unit that starts at `byte`.
    fn unit(self, byte: u64) -> Result<u32> {
        u32::try_from(byte >> self.shift).map_err(|_| Error::msg("launch argument address overflow"))
    }

    /// An object's RC size: whole units.
    fn round(self, size: u32) -> Result<u32> {
        let unit = (1u32 << self.shift) - 1;
        size.checked_add(unit).map(|size| size & !unit)
            .ok_or_else(|| Error::msg("launch argument allocation overflow"))
    }
}

fn image(base: u32, refs: Refs, arguments: &[String]) -> Result<Image> {
    // `base` is the allocator frontier: the unit the image starts at.
    // RC prefix: count, payload size. List payload: length, capacity,
    // eight-byte owned-pointer lanes. String payload: byte length, UTF-8.
    // These are the exact catalog identities admission requires, not values
    // selected from a caller-supplied layout or guest header.
    let count = u32::try_from(arguments.len()).map_err(Error::msg)?;
    let list_size = count.checked_mul(8).and_then(|n| n.checked_add(8))
        .ok_or_else(|| Error::msg("launch argument list size overflow"))?;
    let list_size = refs.round(list_size)?;
    let mut total = list_size.checked_add(8)
        .ok_or_else(|| Error::msg("launch argument allocation overflow"))?;
    for argument in arguments {
        let length = u32::try_from(argument.len()).map_err(Error::msg)?;
        let size = length.checked_add(4).ok_or_else(|| Error::msg("launch argument allocation overflow"))?;
        total = refs.round(size)?.checked_add(8).and_then(|size| total.checked_add(size))
            .ok_or_else(|| Error::msg("launch argument allocation overflow"))?;
    }
    let first = refs.unit_byte(base);
    let end = refs.unit(first + u64::from(total))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(total as usize).map_err(Error::msg)?;
    bytes.resize(total as usize, 0);
    let put = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(&mut bytes, 0, 1);
    put(&mut bytes, 4, list_size);
    put(&mut bytes, 8, count);
    put(&mut bytes, 12, count);
    let mut cursor = list_size as usize + 8;
    for (index, argument) in arguments.iter().enumerate() {
        let length = argument.len();
        let size = refs.round(length as u32 + 4)?;
        put(&mut bytes, 16 + index * 8, refs.reference(first + cursor as u64 + 8)?);
        put(&mut bytes, cursor, 1);
        put(&mut bytes, cursor + 4, size);
        put(&mut bytes, cursor + 8, length as u32);
        bytes[cursor + 12..cursor + 12 + length].copy_from_slice(argument.as_bytes());
        cursor += 8 + size as usize;
    }
    Ok(Image { bytes, root: refs.reference(first + 8)?, end })
}

impl Vm {
    pub(super) fn materialize_entry_arguments(&mut self) -> Result<u32> {
        witchy_wir::canonical_launch::argument_layouts().map_err(Error::msg)?;
        self.materialize_image(|base, refs, state| image(base, refs, &state.caps.args))
    }

    /// The admitted service ABI uses the same catalog raw-byte buffer, but
    /// never decodes its contents as UTF-8 or transfers a parent pointer.
    pub(super) fn materialize_owned_bytes(&mut self, bytes: &[u8]) -> Result<u32> {
        witchy_wir::canonical_launch::argument_layouts().map_err(Error::msg)?;
        self.materialize_image(|base, refs, _| bytes_image(base, refs, bytes))
    }

    fn materialize_image(&mut self, build: impl FnOnce(u32, Refs, &VmState) -> Result<Image>) -> Result<u32> {
        let refs = Refs::for_heap(self.store.data().module.heap_mode());
        let top = self.instance.get_global(&mut self.store, witchy_wir::HEAP_TOP_EXPORT)
            .ok_or_else(|| Error::msg("canonical entry lacks allocator frontier"))?;
        let heap_base = self.instance.get_global(&mut self.store, witchy_wir::HEAP_BASE_EXPORT)
            .ok_or_else(|| Error::msg("canonical entry lacks heap base"))?;
        let Val::I32(base) = top.get(&mut self.store) else {
            return Err(Error::msg("canonical allocator frontier is not i32"));
        };
        let Val::I32(minimum) = heap_base.get(&mut self.store) else {
            return Err(Error::msg("canonical heap base is not i32"));
        };
        if minimum == 0 || (base as u32) < (minimum as u32) {
            return Err(Error::msg("canonical allocator frontier precedes heap"));
        }
        let image = build(base as u32, refs, self.store.data())?;
        let memory = self.instance.get_memory(&mut self.store, "memory")
            .ok_or_else(|| Error::msg("canonical entry lacks memory"))?;
        let pages = refs.unit_byte(image.end).div_ceil(65536);
        let current = memory.size(&self.store);
        if pages > current {
            memory.grow(&mut self.store, pages - current)?;
        }
        let at = usize::try_from(refs.unit_byte(base as u32)).map_err(Error::msg)?;
        memory.write(&mut self.store, at, &image.bytes)?;
        top.set(&mut self.store, Val::I32(image.end as i32))?;
        Ok(image.root)
    }

    /// Called only for an owned Bytes result of the sealed callback ABI.
    /// Admission supplies the type; headers merely bound this runtime read.
    pub(super) fn copy_owned_bytes(&mut self, root: u32) -> Result<(Vec<u8>, bool)> {
        let global = |vm: &mut Self, name: &str| -> Result<u32> {
            vm.instance.get_global(&mut vm.store, name)
                .and_then(|global| global.get(&mut vm.store).i32())
                .map(|value| value as u32).ok_or_else(|| Error::msg("service lacks canonical heap bounds"))
        };
        let refs = Refs::for_heap(self.store.data().module.heap_mode());
        let base = global(self, witchy_wir::HEAP_BASE_EXPORT)?;
        let top = global(self, witchy_wir::HEAP_TOP_EXPORT)?;
        if base == 0 || top < base || root == 0 {
            return Err(Error::msg("service result has invalid canonical heap bounds"));
        }
        let (base, top, root) = (refs.unit_byte(base), refs.unit_byte(top), refs.byte(root));
        let memory = self.instance.get_memory(&mut self.store, "memory")
            .ok_or_else(|| Error::msg("service lacks canonical memory"))?;
        let data = memory.data(&self.store);
        let word = |offset: u64| -> Result<u32> {
            let start = usize::try_from(offset).map_err(|_| Error::msg("service header overflow"))?;
            let end = start.checked_add(4).ok_or_else(|| Error::msg("service header overflow"))?;
            Ok(u32::from_le_bytes(data.get(start..end)
                .ok_or_else(|| Error::msg("service header outside memory"))?.try_into().unwrap()))
        };
        if top > data.len() as u64 {
            return Err(Error::msg("service result has invalid canonical heap bounds"));
        }
        // StaticData carries no RC prefix. The admission-authenticated heap
        // base, not a guessed header word, separates it from owned storage.
        if root < base {
            let length = word(root)?;
            let start = root.checked_add(4).ok_or_else(|| Error::msg("service static header overflow"))?;
            let end = start.checked_add(u64::from(length)).filter(|end| *end <= base)
                .ok_or_else(|| Error::msg("service static Bytes exceeds static storage"))?;
            return Ok((data[start as usize..end as usize].to_vec(), false));
        }
        let prefix = root.checked_sub(8).filter(|prefix| *prefix >= base)
            .ok_or_else(|| Error::msg("service result is not an owned Bytes allocation"))?;
        let rc = word(prefix)?;
        let size = word(prefix + 4)?;
        let end = root.checked_add(u64::from(size)).filter(|end| *end <= top)
            .ok_or_else(|| Error::msg("service result exceeds its heap extent"))?;
        if rc == 0 || size < 4 || end > data.len() as u64 {
            return Err(Error::msg("service result has no live owned Bytes extent"));
        }
        let length = word(root)?;
        if length > size - 4 { return Err(Error::msg("service result length exceeds its RC extent")); }
        let start = root as usize + 4;
        Ok((data[start..start + length as usize].to_vec(), true))
    }
}

fn bytes_image(base: u32, refs: Refs, bytes: &[u8]) -> Result<Image> {
    let length = u32::try_from(bytes.len()).map_err(Error::msg)?;
    let size = length.checked_add(4).ok_or_else(|| Error::msg("service byte extent overflow"))?;
    let size = refs.round(size)?;
    let total = size.checked_add(8).ok_or_else(|| Error::msg("service allocation overflow"))?;
    let first = refs.unit_byte(base);
    let end = refs.unit(first + u64::from(total))?;
    let mut image = Vec::new();
    image.try_reserve_exact(total as usize).map_err(Error::msg)?;
    image.extend_from_slice(&1u32.to_le_bytes());
    image.extend_from_slice(&size.to_le_bytes());
    image.extend_from_slice(&length.to_le_bytes());
    image.extend_from_slice(bytes);
    image.resize(total as usize, 0);
    Ok(Image { bytes: image, root: refs.reference(first + 8)?, end })
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod tests {
    use super::*;

    const BYTE: Refs = Refs { shift: 0 };
    const UNITS: Refs = Refs { shift: 3 };

    pub fn argument_image_rejects_address_overflow() {
        assert!(image(u32::MAX - 7, BYTE, &[]).is_err());
        assert!(image(u32::MAX - 30, BYTE, &["0123456789".into()]).is_err());
        assert!(image(u32::MAX - 1, UNITS, &[]).is_err());
    }

    pub fn large_heap_argument_image_is_unit_aligned() {
        // Base unit 4 is byte 32. The list (one lane) is 16 bytes; "abc"
        // needs 7 and rounds to one unit. A reference names its header's
        // unit: the list is unit 4, the string (header at byte 56) unit 7.
        let image = image(4, UNITS, &["abc".into()]).unwrap();
        assert_eq!((image.root, image.end), (4, 9));
        assert_eq!(&image.bytes[..24], &[1, 0, 0, 0, 16, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&image.bytes[24..], &[1, 0, 0, 0, 8, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0]);
    }

    pub fn service_byte_image_is_owned_binary_and_rejects_overflow() {
        let image = bytes_image(32, BYTE, &[0, 255, 128]).unwrap();
        assert_eq!((image.root, image.end), (40, 47));
        assert_eq!(&image.bytes[..12], &[1, 0, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&image.bytes[12..], &[0, 255, 128]);
        assert!(bytes_image(u32::MAX - 11, BYTE, &[]).is_err());
        let image = bytes_image(4, UNITS, &[0, 255, 128]).unwrap();
        assert_eq!((image.root, image.end), (4, 6));
        assert_eq!(&image.bytes[..], &[1, 0, 0, 0, 8, 0, 0, 0, 3, 0, 0, 0, 0, 255, 128, 0]);
    }
}
