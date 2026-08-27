use witchy_worker_protocol::WorkerLimits;

use crate::WorkerBoundaryError;

#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
pub(crate) fn apply(limits: WorkerLimits) -> Result<(), WorkerBoundaryError> {
    use rustix::process::Resource;

    tighten(Resource::Core, 0, "core-file")?;
    tighten(
        Resource::Cpu,
        limits.cpu_time_ms.saturating_add(999) / 1_000,
        "CPU-time",
    )?;
    tighten(Resource::As, limits.memory_bytes, "address-space")?;
    tighten(
        Resource::Fsize,
        limits.max_response_bytes,
        "output-file-size",
    )?;
    // A small fixed descriptor budget covers enforcement-provider internals.
    // The source bundle is already in memory and default confinement denies
    // every new filesystem open regardless of this structural ceiling.
    tighten(Resource::Nofile, 16, "open-file-count")?;
    Ok(())
}

#[cfg(not(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
)))]
pub(crate) fn apply(_limits: WorkerLimits) -> Result<(), WorkerBoundaryError> {
    Err(WorkerBoundaryError::UnsupportedPlatform)
}

#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
fn tighten(
    resource: rustix::process::Resource,
    requested: u64,
    name: &'static str,
) -> Result<(), WorkerBoundaryError> {
    use rustix::process::{Rlimit, getrlimit, setrlimit};

    let current = getrlimit(resource);
    let maximum = minimum_limit(current.maximum, requested);
    let effective = minimum_limit(current.current, maximum);
    setrlimit(
        resource,
        Rlimit {
            current: Some(effective),
            maximum: Some(maximum),
        },
    )
    .map_err(|error| WorkerBoundaryError::ResourceLimit {
        resource: name,
        detail: error.to_string(),
    })
}

#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )
))]
fn minimum_limit(existing: Option<u64>, requested: u64) -> u64 {
    existing.map_or(requested, |existing| existing.min(requested))
}

#[cfg(test)]
mod tests {
    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    ))]
    #[test]
    fn a_requested_limit_never_widens_an_existing_limit() {
        assert_eq!(super::minimum_limit(None, 100), 100);
        assert_eq!(super::minimum_limit(Some(200), 100), 100);
        assert_eq!(super::minimum_limit(Some(50), 100), 50);
    }
}
