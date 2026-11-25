//! Explicit value properties. Non-Copy does not imply permission to discard.
#[derive(Clone, Copy, Debug)]
pub(super) struct Properties {
    pub copy: bool,
    pub implicit_discard: bool,
}
