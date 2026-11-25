//! Identity checks shared by input and file capability binding.
#[cfg(target_os = "linux")]
use super::linux_stat::Stat;
#[cfg(target_os = "macos")]
use super::seatbelt::Stat;

pub(super) fn secret(info: &Stat) -> bool {
    #[cfg(witchy_native_aws_lc)]
    {
        unsafe extern "C" {
            safe fn witchy_secret_source(device: u64, inode: u64) -> bool;
        }
        witchy_secret_source(info.device as u64, info.inode)
    }
    #[cfg(not(witchy_native_aws_lc))]
    {
        let _ = info;
        false
    }
}

pub(super) fn safe_file(info: &Stat) -> bool {
    info.mode & 0o170000 == 0o100000 && info.links == 1 && !secret(info)
}
