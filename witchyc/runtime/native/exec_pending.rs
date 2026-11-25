//! Bounded request bytes awaiting a single Exec run.
use core::{ffi::c_void, ptr, slice};

const LIMIT: usize = 1 << 20;

unsafe extern "C" {
    fn realloc(address: *mut c_void, size: usize) -> *mut c_void;
    fn free(address: *mut c_void);
}

pub(super) struct Buffer {
    data: *mut u8,
    len: usize,
    capacity: usize,
}

impl Buffer {
    pub(super) const EMPTY: Self = Self {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    };

    pub(super) fn append(&mut self, bytes: &[u8]) -> bool {
        let Some(length) = self
            .len
            .checked_add(bytes.len())
            .and_then(|n| n.checked_add(1))
        else {
            return false;
        };
        if length > self.capacity {
            let capacity = length.max(self.capacity.saturating_mul(2)).max(64);
            let data = unsafe { realloc(self.data.cast(), capacity) }.cast::<u8>();
            if data.is_null() {
                return false;
            }
            self.data = data;
            self.capacity = capacity;
        }
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), self.data.add(self.len), bytes.len());
        }
        self.len += bytes.len();
        true
    }

    pub(super) fn bytes(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.data, self.len) }
        }
    }

    pub(super) fn clear(&mut self) {
        unsafe {
            free(self.data.cast());
        }
        self.data = ptr::null_mut();
        self.len = 0;
        self.capacity = 0;
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.clear();
    }
}

pub(super) enum Kind {
    Arguments,
    Input,
    Environment,
}

pub(super) struct Pending {
    arguments: Buffer,
    input: Buffer,
    environment: Buffer,
}

impl Pending {
    pub(super) const EMPTY: Self = Self {
        arguments: Buffer::EMPTY,
        input: Buffer::EMPTY,
        environment: Buffer::EMPTY,
    };

    pub(super) fn append(&mut self, kind: Kind, bytes: &[u8]) -> bool {
        let total = self.arguments.len + self.input.len + self.environment.len;
        if bytes.len() > LIMIT || total > LIMIT - bytes.len() {
            return false;
        }
        match kind {
            Kind::Arguments => &mut self.arguments,
            Kind::Input => &mut self.input,
            Kind::Environment => &mut self.environment,
        }
        .append(bytes)
    }

    pub(super) fn arguments(&self) -> &[u8] {
        self.arguments.bytes()
    }
    pub(super) fn input(&self) -> &[u8] {
        self.input.bytes()
    }
    pub(super) fn environment(&self) -> &[u8] {
        self.environment.bytes()
    }

    pub(super) fn clear(&mut self) {
        self.arguments.clear();
        self.input.clear();
        self.environment.clear();
    }
}

#[cfg(test)]
#[test]
fn budgets_all_three_streams_together_and_clears_them() {
    let mut pending = Pending::EMPTY;
    let data = vec![0x41; LIMIT - 2];
    assert!(pending.append(Kind::Arguments, &data));
    assert!(pending.append(Kind::Input, b"x"));
    assert!(pending.append(Kind::Environment, b"y"));
    assert!(!pending.append(Kind::Input, b"z"));
    assert_eq!(pending.arguments().len(), LIMIT - 2);
    assert_eq!(pending.input(), b"x");
    assert_eq!(pending.environment(), b"y");
    pending.clear();
    assert!(
        pending.arguments().is_empty()
            && pending.input().is_empty()
            && pending.environment().is_empty()
    );
    assert!(pending.append(Kind::Input, b"next"));
}
