#![no_main]

#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/heap.rs"]
mod heap;
#[path = "../../../runtime/native/heap_layout.rs"]
mod heap_layout;
#[path = "../../../runtime/native/heap_range.rs"]
mod heap_range;
#[path = "../../../runtime/native/heap_slots.rs"]
mod heap_slots;

use heap_slots::Slots;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut slots = Slots::<u8, 4>::new();
    let mut live = [None; 4];
    let mut seen = Vec::new();
    let mut limits = (31u64, u64::MAX);
    for (i, bytes) in data.chunks_exact(2).enumerate() {
        let selector = usize::from(bytes[1]);
        match bytes[0] % 4 {
            0 => {
                let issued = slots.insert(bytes[1]);
                assert_eq!(issued.is_some(), live.iter().any(Option::is_none));
                if let Some(handle) = issued {
                    assert!(!seen.iter().any(|&(old, _)| old == handle));
                    seen.push((handle, bytes[1]));
                    let index = (handle as u32 as usize).checked_sub(1).unwrap();
                    assert!(live[index].replace((handle, bytes[1])).is_none());
                }
            }
            1 | 2 => {
                let handle = seen
                    .get(selector % seen.len().max(1))
                    .map_or(0, |&(h, _)| h);
                let expected = live
                    .iter_mut()
                    .find(|value| value.is_some_and(|(h, _)| h == handle));
                if bytes[0] % 4 == 1 {
                    assert_eq!(
                        slots.get(handle),
                        expected.and_then(|slot| slot.map(|(_, v)| v))
                    );
                } else {
                    assert_eq!(
                        slots.remove(handle),
                        expected.and_then(|slot| slot.take().map(|(_, v)| v))
                    );
                }
            }
            _ => {
                let charge = u64::from(bytes[1]);
                let next = budget::debit(limits.0, limits.1, charge);
                assert_eq!(
                    next,
                    limits.0.checked_sub(charge).map(|total| (total, u64::MAX))
                );
                if let Some(value) = next {
                    limits = value;
                }
                if i % 7 == 0 {
                    limits = (charge, u64::MAX);
                }
            }
        }
        assert_eq!(slots.has_room(), live.iter().any(Option::is_none));
    }
    let expected = if data == b"unlimited" {
        Some(u64::MAX)
    } else if data.iter().all(u8::is_ascii_digit) {
        core::str::from_utf8(data)
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .filter(|value| *value <= 1 << 40)
    } else {
        None
    };
    assert_eq!(budget::parse_limit(data), expected);
    occupancy_sequence(data, false);
    occupancy_sequence(data, true);
    allocator_sequence(data);
});

// Exercise the production allocator and its actual pointers; the independent
// model tracks values and occupancy without inspecting allocator metadata.
fn allocator_sequence(data: &[u8]) {
    heap::witchy_allow_allocator();
    let mut handles = [0u64; 2];
    for handle in &mut handles {
        *handle = heap::witchy_allocate(2, 1, 1, 8);
        assert!(*handle > u64::from(u32::MAX));
    }
    let mut model = [[None::<u8>; 8]; 2];
    for action in data.chunks_exact(3) {
        let side = usize::from(action[0] & 1);
        let index = usize::from(action[1] % 8);
        match (action[0] >> 1) % 4 {
            0 if model[side][index].is_none() => {
                heap::witchy_init_word(handles[side], 1, index, 1, u64::from(action[2]));
                model[side][index] = Some(action[2]);
            }
            1 if model[side][index].is_some() => {
                assert_eq!(
                    heap::witchy_read_word(handles[side], 1, index, 1),
                    u64::from(model[side][index].unwrap())
                );
            }
            2 if model[side][index].is_some() => {
                assert!(heap::witchy_cell(handles[side], 1, index, 1) > 2);
                model[side][index] = None;
            }
            3 => {
                let source = side;
                let target = 1 - side;
                let count = usize::from(action[2] % 9);
                if index + count <= 8
                    && model[source][..count].iter().all(Option::is_some)
                    && model[target][index..index + count]
                        .iter()
                        .all(Option::is_none)
                {
                    heap::witchy_transfer(handles[source], handles[target], 1, index, count, 1);
                    for cell in 0..count {
                        model[target][index + cell] = model[source][cell].take();
                    }
                }
            }
            _ => {}
        }
    }
    for side in 0..2 {
        assert_eq!(heap::witchy_cell(handles[side], 1, 8, 2), 1);
        for index in 0..8 {
            if let Some(value) = model[side][index] {
                assert_eq!(
                    heap::witchy_read_word(handles[side], 1, index, 1),
                    u64::from(value)
                );
                assert!(heap::witchy_cell(handles[side], 1, index, 1) > 2);
            } else {
                assert_eq!(heap::witchy_cell(handles[side], 1, index, 2), 2);
            }
        }
        assert_eq!(heap::witchy_release(handles[side], 1), 0);
    }
}

fn occupancy_sequence(data: &[u8], dense: bool) {
    const CELLS: usize = 24;
    let mut model = [false; CELLS];
    let mut bitmap = [0; CELLS / 8];
    let (mut prefix, mut occupied) = (0u32, 0u32);

    for action in data.chunks_exact(3) {
        let from = usize::from(action[0]) % (CELLS + 1);
        let end = usize::from(action[1]) % (CELLS + 1);
        let mode = action[2] % 4;
        let head = model.iter().take_while(|&&cell| cell).count();
        let allowed = if from > end || mode > 2 {
            false
        } else if from == end {
            true
        } else if dense {
            match mode {
                0 => from == head,
                1 => end == head && from < head,
                _ => end <= head,
            }
        } else {
            model[from..end].iter().all(|&cell| cell == (mode != 0))
        };
        assert_eq!(
            heap_range::permits(
                (!dense).then_some(&bitmap[..]),
                prefix as usize,
                from,
                end,
                mode
            ),
            allowed,
        );
        if !allowed {
            continue;
        }
        (prefix, occupied) = heap_range::mark(
            (!dense).then_some(&mut bitmap[..]),
            prefix,
            occupied,
            from as u32,
            end as u32,
            mode,
        )
        .expect("admitted transitions must succeed");

        if mode != 2 {
            model[from..end].fill(mode == 0);
        }
        let head = model.iter().take_while(|&&cell| cell).count();
        if dense {
            assert_eq!(prefix as usize, head);
            assert_eq!(occupied, 0);
        } else {
            assert!(prefix as usize <= head);
            assert_eq!(
                occupied as usize,
                model.iter().filter(|&&cell| cell).count()
            );
            for (index, &cell) in model.iter().enumerate() {
                assert_eq!(bitmap[index / 8] & (1 << (index % 8)) != 0, cell);
            }
        }
    }
}
