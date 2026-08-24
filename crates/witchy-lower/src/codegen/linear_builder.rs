//! RFC-0151 Linear build-plan infrastructure.
//!
//! A `LinearBuildPlan` is derived from typed ownership, layout, range, and
//! escape facts. It applies across eligible contiguous buffers (`Bytes`, `String`,
//! and packed sequences) whose intrinsic metadata declares the same operation
//! class (`ContiguousRead`, `ContiguousWrite`, `AppendScalar`, `AppendSlice`,
//! `Reserve`, `Finish`).
//!
//! No benchmark name, source filename, biological alphabet, output record, or
//! specific standard-library method is an optimization input.

use super::*;
use witchy_syntax::intrinsics::{ContiguousOpClass, ScalarValidation};
use witchy_wir::layout::LayoutId;

/// The representation class of the contiguous buffer under construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinearValueKind {
    String,
    Bytes,
    PackedSequence(LayoutId),
}

/// A proven integer value range bound for a named variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeFact {
    pub var_name: String,
    pub min: i64,
    pub max: i64,
}

/// (RFC-0151) Typed linear build plan for contiguous owned buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearBuildPlan {
    /// The unique root variable / place owning the buffer.
    pub root_place: String,
    /// The representation kind of the buffer payload.
    pub value_kind: LinearValueKind,
    /// Byte width of a single element (e.g. 1 for Bytes/String).
    pub payload_width: u32,
    /// Optional cached length register / local name.
    pub length_local: Option<String>,
    /// Optional cached capacity register / local name.
    pub capacity_local: Option<String>,
    /// Optional cached write cursor register / local name.
    pub cursor_local: Option<String>,
    /// The shadow ownership token (`__cap`) variable for in-place mutation.
    pub ownership_token: Option<String>,
    /// Proven integer range facts available within the builder's scope.
    pub validated_range_facts: Vec<RangeFact>,
    /// Monotonic epoch counter at plan creation, used for conservative escape invalidation.
    pub escape_epoch: u32,
}

impl LinearBuildPlan {
    /// Create a new linear build plan for a root place.
    pub fn new(
        root_place: String,
        value_kind: LinearValueKind,
        payload_width: u32,
        escape_epoch: u32,
    ) -> Self {
        Self {
            root_place,
            value_kind,
            payload_width,
            length_local: None,
            capacity_local: None,
            cursor_local: None,
            ownership_token: None,
            validated_range_facts: Vec::new(),
            escape_epoch,
        }
    }

    /// Attach register/local cache variable names.
    pub fn with_locals(
        mut self,
        length: Option<String>,
        capacity: Option<String>,
        cursor: Option<String>,
        ownership_token: Option<String>,
    ) -> Self {
        self.length_local = length;
        self.capacity_local = capacity;
        self.cursor_local = cursor;
        self.ownership_token = ownership_token;
        self
    }

    /// Check if this plan is still valid at the given escape epoch.
    pub fn is_valid_at_epoch(&self, current_epoch: u32) -> bool {
        self.escape_epoch == current_epoch
    }

    /// Record a proven range fact for a variable.
    pub fn record_range_fact(&mut self, var_name: String, min: i64, max: i64) {
        if let Some(existing) = self.validated_range_facts.iter_mut().find(|f| f.var_name == var_name) {
            existing.min = existing.min.max(min);
            existing.max = existing.max.min(max);
        } else {
            self.validated_range_facts.push(RangeFact { var_name, min, max });
        }
    }

    /// Check if a variable is proven to be within valid byte range `0..=255`.
    pub fn range_is_byte_proven(&self, var_name: &str) -> bool {
        self.validated_range_facts
            .iter()
            .any(|f| f.var_name == var_name && f.min >= 0 && f.max <= 255)
    }

    /// Whether this plan describes a direct byte buffer (`Bytes` with 1-byte elements).
    pub fn is_direct_byte_eligible(&self) -> bool {
        matches!(self.value_kind, LinearValueKind::Bytes) && self.payload_width == 1
    }

    /// Whether this plan describes a string builder (`String` with 1-byte UTF-8 elements).
    pub fn is_string_builder_eligible(&self) -> bool {
        matches!(self.value_kind, LinearValueKind::String) && self.payload_width == 1
    }

    /// Check if a contiguous operation class is applicable to this plan.
    pub fn contiguous_op_applicable(&self, op: ContiguousOpClass) -> bool {
        match op {
            ContiguousOpClass::ContiguousRead { width } | ContiguousOpClass::ContiguousWrite { width } => {
                self.payload_width == width
            }
            ContiguousOpClass::AppendScalar { width, validation } => {
                if self.payload_width != width {
                    return false;
                }
                match validation {
                    ScalarValidation::None => true,
                    ScalarValidation::ByteRange => matches!(self.value_kind, LinearValueKind::Bytes),
                    ScalarValidation::Utf8Scalar => matches!(self.value_kind, LinearValueKind::String),
                }
            }
            ContiguousOpClass::AppendSlice { width } => self.payload_width == width,
            ContiguousOpClass::Reserve | ContiguousOpClass::Finish => true,
        }
    }
}

#[allow(dead_code)]
impl<'types> Codegen<'types> {
    /// Note an active linear build plan for a root variable.
    pub(crate) fn note_linear_builder(&mut self, plan: LinearBuildPlan) {
        self.linear_build_plans.insert(plan.root_place.clone(), plan);
    }

    /// Invalidate all active linear build plans (advances the escape epoch).
    pub(crate) fn invalidate_linear_builders(&mut self) {
        self.linear_build_epoch = self.linear_build_epoch.wrapping_add(1);
        self.linear_build_plans.clear();
    }

    /// Query the active linear build plan for a root place, if valid at the current epoch.
    pub(crate) fn active_linear_build_plan(&self, root: &str) -> Option<&LinearBuildPlan> {
        self.linear_build_plans.get(root).filter(|p| p.is_valid_at_epoch(self.linear_build_epoch))
    }

    /// Mutably access the active linear build plan for a root place, if valid at the current epoch.
    pub(crate) fn active_linear_build_plan_mut(&mut self, root: &str) -> Option<&mut LinearBuildPlan> {
        let epoch = self.linear_build_epoch;
        self.linear_build_plans.get_mut(root).filter(|p| p.is_valid_at_epoch(epoch))
    }
}
