//! Allocation geometry for native typed storage.

pub const BYTE_LIMIT: u64 = 512 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Layout {
    pub payload: u64,
    pub bitmap: u64,
    pub total: u64,
}

pub fn layout(stride: u64, count: u64, dense: bool, header: u64) -> Option<Layout> {
    let payload = stride.checked_mul(count)?;
    let bitmap = if dense { 0 } else { count.div_ceil(8) };
    let total = header.checked_add(payload)?.checked_add(bitmap)?;
    if total > BYTE_LIMIT {
        return None;
    }
    Some(Layout {
        payload,
        bitmap,
        total,
    })
}

#[cfg(kani)]
#[kani::proof]
fn layout_matches_wide_arithmetic() {
    let stride: u64 = kani::any();
    let count: u64 = kani::any();
    let dense: bool = kani::any();
    let payload = u128::from(stride) * u128::from(count);
    let bitmap = if dense {
        0
    } else {
        u128::from(count).div_ceil(8)
    };
    let total = 40 + payload + bitmap;
    let expected = (total <= u128::from(BYTE_LIMIT)).then_some(Layout {
        payload: payload as u64,
        bitmap: bitmap as u64,
        total: total as u64,
    });
    assert_eq!(layout(stride, count, dense, 40), expected);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_and_sparse_layouts_check_header_and_bitmap() {
        assert_eq!(
            layout(8, 9, false, 40),
            Some(Layout {
                payload: 72,
                bitmap: 2,
                total: 114,
            })
        );
        assert_eq!(layout(8, 9, true, 40).map(|size| size.total), Some(112));
        assert_eq!(
            layout(1, BYTE_LIMIT - 40, true, 40).map(|size| size.total),
            Some(BYTE_LIMIT)
        );
        assert_eq!(layout(1, BYTE_LIMIT - 40, false, 40), None);
        assert_eq!(layout(2, u64::MAX, true, 40), None);
    }
}
