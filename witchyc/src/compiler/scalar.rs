//! One scalar descriptor supplies source naming, range semantics and future
//! storage size/alignment. Register carriers are deliberately independent of
//! address width and storage width: this pilot uses i64 for all unsigned values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Scalar {
    U8,
    U16,
    U32,
    U64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    pub name: &'static str,
    pub bits: u8,
    pub size: u8,
    pub alignment: u8,
}
impl Layout {
    pub fn maximum(self) -> u64 {
        if self.bits == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits) - 1
        }
    }
}
impl Scalar {
    pub const ALL: [Self; 4] = [Self::U8, Self::U16, Self::U32, Self::U64];
    pub const NARROW: [Self; 3] = [Self::U8, Self::U16, Self::U32];
    pub fn layout(self) -> Layout {
        let (name, bits, size) = match self {
            Self::U8 => ("U8", 8, 1),
            Self::U16 => ("U16", 16, 2),
            Self::U32 => ("U32", 32, 4),
            Self::U64 => ("U64", 64, 8),
        };
        Layout {
            name,
            bits,
            size,
            alignment: size,
        }
    }
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.layout().name == name)
    }
    pub fn narrow_index(self) -> Option<u32> {
        Self::NARROW
            .iter()
            .position(|s| *s == self)
            .map(|i| i as u32)
    }
}
