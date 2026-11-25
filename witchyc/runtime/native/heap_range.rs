//! Occupancy checks used before lending a native payload address.

pub fn permits(bitmap: Option<&[u8]>, prefix: usize, from: usize, end: usize, mode: u8) -> bool {
    if from > end || mode > 2 {
        return false;
    }
    if from == end {
        return true;
    }

    let Some(bitmap) = bitmap else {
        return match mode {
            0 => from == prefix,
            1 => end == prefix,
            _ => end <= prefix,
        };
    };
    let needs_occupied = mode != 0;
    let first = if needs_occupied {
        from.max(prefix)
    } else {
        from
    };
    (first..end).all(|index| {
        bitmap
            .get(index / 8)
            .is_some_and(|byte| (byte & (1 << (index % 8)) != 0) == needs_occupied)
    })
}

#[cfg(test)]
#[test]
fn sparse_ranges_match_cell_by_cell_policy() {
    for bitmap in 0u8..=255 {
        let prefix = bitmap.trailing_ones() as usize;
        for from in 0..=8 {
            for end in from..=8 {
                for mode in 0..=2 {
                    let expected =
                        (from..end).all(|index| (bitmap & (1 << index) != 0) == (mode != 0));
                    assert_eq!(permits(Some(&[bitmap]), prefix, from, end, mode), expected);
                }
            }
        }
    }
}

/// Apply a transition whose occupancy preconditions were checked by the caller.
/// Returns the new prefix and occupied count.
pub fn mark(
    bitmap: Option<&mut [u8]>,
    prefix: u32,
    occupied: u32,
    from: u32,
    end: u32,
    mode: u8,
) -> Option<(u32, u32)> {
    let count = end.checked_sub(from)?;
    if mode > 2 {
        return None;
    }
    if count == 0 || mode == 2 {
        return Some((prefix, occupied));
    }
    let Some(bitmap) = bitmap else {
        return Some((if mode == 0 { end } else { from }, occupied));
    };
    if ((end - 1) / 8) as usize >= bitmap.len() {
        return None;
    }
    let next = if mode == 0 {
        (
            if from == prefix { end } else { prefix },
            occupied.checked_add(count)?,
        )
    } else {
        (prefix.min(from), occupied.checked_sub(count)?)
    };
    for index in from..end {
        let byte = bitmap.get_mut((index / 8) as usize)?;
        let mask = 1 << (index % 8);
        if mode == 0 {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
    Some(next)
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(12)]
fn sparse_transitions_preserve_occupancy() {
    let initial: u8 = kani::any();
    let mut bitmap = [initial];
    let mut model = initial;
    let mut prefix = initial.trailing_ones();
    let mut occupied = initial.count_ones();

    for _ in 0..3 {
        let from: u8 = kani::any();
        let end: u8 = kani::any();
        let mode: u8 = kani::any();
        kani::assume(from <= end && end <= 8 && mode <= 2);
        let mask = ((1u16 << end) - (1u16 << from)) as u8;
        let expected = if mode == 0 {
            model & mask == 0
        } else {
            model & mask == mask
        };
        let allowed = permits(
            Some(&bitmap),
            prefix as usize,
            from as usize,
            end as usize,
            mode,
        );
        assert_eq!(allowed, expected);
        if allowed {
            let next = mark(
                Some(&mut bitmap),
                prefix,
                occupied,
                from.into(),
                end.into(),
                mode,
            );
            assert!(next.is_some());
            (prefix, occupied) = next.unwrap();
            if mode == 0 {
                model |= mask;
            } else if mode == 1 {
                model &= !mask;
            }
        }
        assert_eq!(bitmap[0], model);
        assert_eq!(occupied, model.count_ones());
        assert!(prefix <= model.trailing_ones());
    }
}

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(5)]
fn dense_transitions_preserve_initialized_prefix() {
    let mut prefix: u32 = kani::any();
    kani::assume(prefix <= 8);
    for _ in 0..3 {
        let from: u8 = kani::any();
        let end: u8 = kani::any();
        let mode: u8 = kani::any();
        kani::assume(from <= end && end <= 8 && mode <= 2);
        let empty = from == end;
        let expected = empty
            || match mode {
                0 => u32::from(from) == prefix,
                1 => u32::from(end) == prefix,
                _ => u32::from(end) <= prefix,
            };
        let allowed = permits(None, prefix as usize, from as usize, end as usize, mode);
        assert_eq!(allowed, expected);
        if allowed {
            let next = mark(None, prefix, 0, from.into(), end.into(), mode);
            assert!(next.is_some());
            let (new_prefix, occupied) = next.unwrap();
            assert_eq!(occupied, 0);
            let expected_prefix = if empty || mode == 2 {
                prefix
            } else if mode == 0 {
                u32::from(end)
            } else {
                u32::from(from)
            };
            assert_eq!(new_prefix, expected_prefix);
            assert!(new_prefix <= 8);
            prefix = new_prefix;
        }
    }
}

#[cfg(test)]
#[test]
fn sparse_transitions_match_bitmap_model() {
    for initial in 0u8..=255 {
        let prefix = initial.trailing_ones();
        let occupied = initial.count_ones();
        for from in 0..=8 {
            for end in from..=8 {
                for mode in 0..=1 {
                    if !permits(
                        Some(&[initial]),
                        prefix as usize,
                        from as usize,
                        end as usize,
                        mode,
                    ) {
                        continue;
                    }
                    let mut bitmap = [initial];
                    let (next_prefix, next_count) =
                        mark(Some(&mut bitmap), prefix, occupied, from, end, mode).unwrap();
                    let mask = ((1u16 << end) - (1u16 << from)) as u8;
                    let expected = if mode == 0 {
                        initial | mask
                    } else {
                        initial & !mask
                    };
                    assert_eq!(bitmap[0], expected);
                    assert_eq!(next_count, expected.count_ones());
                    assert!(next_prefix <= expected.trailing_ones());
                }
            }
        }
    }
}
