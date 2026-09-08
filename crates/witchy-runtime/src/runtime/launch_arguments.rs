//! Construct the catalog's owned `List(String)` entry value before guest
//! execution. No guest callback or allocator runs while this image is built.

use super::{Error, Result, Val, Vm};

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
        let image = image(base as u32, &self.store.data().caps.args)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_image_rejects_address_overflow() {
        assert!(image(u32::MAX - 7, &[]).is_err());
        assert!(image(u32::MAX - 30, &["0123456789".into()]).is_err());
    }
}
