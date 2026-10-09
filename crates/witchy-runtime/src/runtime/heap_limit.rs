//! The store's memory limiter, which also remembers why a growth was refused
//! (RFC-0179). An admitted allocator traps with `unreachable` when
//! `memory.grow` fails; that trap alone says nothing about which limit the
//! program reached, so the run error names it from what was recorded here.

use wasmtime::{ResourceLimiter, StoreLimits};
use witchy_wir::HeapMode;

const GIB: usize = 1 << 30;

pub(crate) struct HeapLimiter {
    limits: StoreLimits,
    /// The admitted program's heap mode and the run's memory limit in bytes,
    /// once the launch knows them. A store without them reports no message.
    launch: Option<(HeapMode, usize)>,
    /// The limit a refused growth hit, in bytes.
    refused: Option<usize>,
}

impl From<StoreLimits> for HeapLimiter {
    fn from(limits: StoreLimits) -> Self {
        Self { limits, launch: None, refused: None }
    }
}

impl HeapLimiter {
    /// `run_limit` must be the byte limit `limits` was built with.
    pub(crate) fn set_launch(&mut self, heap: HeapMode, run_limit: usize) {
        self.launch = Some((heap, run_limit));
    }

    /// The message for a trap that follows a refused growth, if one did.
    pub(crate) fn out_of_memory(&self, error: &wasmtime::Error) -> Option<String> {
        let (heap, run_limit) = self.launch?;
        let limit = self.refused?;
        if error.downcast_ref::<wasmtime::Trap>() != Some(&wasmtime::Trap::UnreachableCodeReached) {
            return None;
        }
        let size = gib(limit);
        let reached = if limit == run_limit && (limit as u64) < heap.max_bytes() {
            format!("the {size} memory limit for a run")
        } else {
            format!("the program's {size} heap limit")
        };
        Some(match heap {
            HeapMode::Large => format!("out of memory: {reached} was reached"),
            HeapMode::Standard => format!(
                "out of memory: {reached} was reached; mark `main` with `@large_heap` to raise it to {}",
                gib(HeapMode::Large.max_bytes() as usize)
            ),
        })
    }
}

fn gib(bytes: usize) -> String {
    if bytes % GIB == 0 { format!("{} GiB", bytes / GIB) } else { format!("{} MiB", bytes >> 20) }
}

impl ResourceLimiter for HeapLimiter {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let declared = maximum.unwrap_or(usize::MAX);
        let allowed = desired <= declared && self.limits.memory_growing(current, desired, maximum)?;
        // Only the latest growth counts: a refusal the program survived (a host
        // call that reported it) must not explain a later, unrelated trap.
        self.refused = match self.launch {
            Some((_, run_limit)) if !allowed => Some(if desired > declared { declared.min(run_limit) } else { run_limit }),
            _ => None,
        };
        Ok(allowed)
    }

    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize { self.limits.instances() }
    fn tables(&self) -> usize { self.limits.tables() }
    fn memories(&self) -> usize { self.limits.memories() }
}
