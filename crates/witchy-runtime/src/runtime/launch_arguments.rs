//! Construct the catalog's owned `List(String)` entry value before guest
//! execution. No guest callback or allocator runs while this image is built.

use super::{Error, Result, Val, Vm, VmState};

struct Image {
    bytes: Vec<u8>,
    root: u32,
    end: u32,
}

fn image(base: u32, arguments: &[String]) -> Result<Image> {
    // RC prefix: count, payload size. List payload: length, capacity,
    // eight-byte owned-pointer lanes. String payload: byte length, UTF-8.
    // These are the exact catalog identities admission requires, not values
    // selected from a caller-supplied layout or guest header.
    let count = u32::try_from(arguments.len()).map_err(Error::msg)?;
    let list_size = count.checked_mul(8).and_then(|n| n.checked_add(8))
        .ok_or_else(|| Error::msg("launch argument list size overflow"))?;
    let mut total = list_size.checked_add(8)
        .ok_or_else(|| Error::msg("launch argument allocation overflow"))?;
    for argument in arguments {
        let length = u32::try_from(argument.len()).map_err(Error::msg)?;
        total = length.checked_add(12).and_then(|size| total.checked_add(size))
            .ok_or_else(|| Error::msg("launch argument allocation overflow"))?;
    }
    let end = base.checked_add(total).ok_or_else(|| Error::msg("launch argument address overflow"))?;
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
        put(&mut bytes, 16 + index * 8, base + cursor as u32 + 8);
        put(&mut bytes, cursor, 1);
        put(&mut bytes, cursor + 4, length as u32 + 4);
        put(&mut bytes, cursor + 8, length as u32);
        bytes[cursor + 12..cursor + 12 + length].copy_from_slice(argument.as_bytes());
        cursor += 12 + length;
    }
    Ok(Image { bytes, root: base + 8, end })
}

impl Vm {
    pub(super) fn materialize_entry_arguments(&mut self) -> Result<u32> {
        witchy_wir::canonical_launch::argument_layouts().map_err(Error::msg)?;
        self.materialize_image(|base, state| image(base, &state.caps.args))
    }

    /// The admitted service ABI uses the same catalog raw-byte buffer, but
    /// never decodes its contents as UTF-8 or transfers a parent pointer.
    pub(super) fn materialize_owned_bytes(&mut self, bytes: &[u8]) -> Result<u32> {
        witchy_wir::canonical_launch::argument_layouts().map_err(Error::msg)?;
        self.materialize_image(|base, _| bytes_image(base, bytes))
    }

    fn materialize_image(&mut self, build: impl FnOnce(u32, &VmState) -> Result<Image>) -> Result<u32> {
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
        let image = build(base as u32, self.store.data())?;
        let memory = self.instance.get_memory(&mut self.store, "memory")
            .ok_or_else(|| Error::msg("canonical entry lacks memory"))?;
        let pages = u64::from(image.end).div_ceil(65536);
        let current = memory.size(&self.store);
        if pages > current {
            memory.grow(&mut self.store, pages - current)?;
        }
        memory.write(&mut self.store, base as u32 as usize, &image.bytes)?;
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
        let base = global(self, witchy_wir::HEAP_BASE_EXPORT)?;
        let top = global(self, witchy_wir::HEAP_TOP_EXPORT)?;
        let memory = self.instance.get_memory(&mut self.store, "memory")
            .ok_or_else(|| Error::msg("service lacks canonical memory"))?;
        let data = memory.data(&self.store);
        let word = |offset: u32| -> Result<u32> {
            let start = offset as usize;
            let end = start.checked_add(4).ok_or_else(|| Error::msg("service header overflow"))?;
            Ok(u32::from_le_bytes(data.get(start..end)
                .ok_or_else(|| Error::msg("service header outside memory"))?.try_into().unwrap()))
        };
        if base == 0 || top < base || top as usize > data.len() || root == 0 {
            return Err(Error::msg("service result has invalid canonical heap bounds"));
        }
        // StaticData carries no RC prefix. The admission-authenticated heap
        // base, not a guessed header word, separates it from owned storage.
        if root < base {
            let length = word(root)?;
            let start = root.checked_add(4).ok_or_else(|| Error::msg("service static header overflow"))?;
            let end = start.checked_add(length).filter(|end| *end <= base)
                .ok_or_else(|| Error::msg("service static Bytes exceeds static storage"))?;
            return Ok((data[start as usize..end as usize].to_vec(), false));
        }
        let prefix = root.checked_sub(8).filter(|prefix| *prefix >= base)
            .ok_or_else(|| Error::msg("service result is not an owned Bytes allocation"))?;
        let rc = word(prefix)?;
        let size = word(prefix + 4)?;
        let end = root.checked_add(size).filter(|end| *end <= top)
            .ok_or_else(|| Error::msg("service result exceeds its heap extent"))?;
        if rc == 0 || size < 4 || end as usize > data.len() {
            return Err(Error::msg("service result has no live owned Bytes extent"));
        }
        let length = word(root)?;
        if length > size - 4 { return Err(Error::msg("service result length exceeds its RC extent")); }
        let start = root as usize + 4;
        Ok((data[start..start + length as usize].to_vec(), true))
    }
}

fn bytes_image(base: u32, bytes: &[u8]) -> Result<Image> {
    let length = u32::try_from(bytes.len()).map_err(Error::msg)?;
    let size = length.checked_add(4).ok_or_else(|| Error::msg("service byte extent overflow"))?;
    let total = size.checked_add(8).ok_or_else(|| Error::msg("service allocation overflow"))?;
    let end = base.checked_add(total).ok_or_else(|| Error::msg("service address overflow"))?;
    let mut image = Vec::new();
    image.try_reserve_exact(total as usize).map_err(Error::msg)?;
    image.extend_from_slice(&1u32.to_le_bytes());
    image.extend_from_slice(&size.to_le_bytes());
    image.extend_from_slice(&length.to_le_bytes());
    image.extend_from_slice(bytes);
    Ok(Image { bytes: image, root: base + 8, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_image_rejects_address_overflow() {
        assert!(image(u32::MAX - 7, &[]).is_err());
        assert!(image(u32::MAX - 30, &["0123456789".into()]).is_err());
    }

    #[test]
    fn service_byte_image_is_owned_binary_and_rejects_overflow() {
        let image = bytes_image(32, &[0, 255, 128]).unwrap();
        assert_eq!((image.root, image.end), (40, 47));
        assert_eq!(&image.bytes[..12], &[1, 0, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&image.bytes[12..], &[0, 255, 128]);
        assert!(bytes_image(u32::MAX - 11, &[]).is_err());
    }
}
