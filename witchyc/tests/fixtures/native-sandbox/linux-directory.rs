#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/broker_directory.rs"]
mod broker_directory;
#[path = "../../../runtime/native/broker_path.rs"]
mod broker_path;
#[path = "../../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../../runtime/native/entry_budget.rs"]
mod entry_budget;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/launch_args.rs"]
mod launch_args;
#[path = "../../../runtime/native/linux_stat.rs"]
mod linux_stat;
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_guarded() -> bool {
    false
}
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_program(_device: u64, _inode: u64) -> bool {
    false
}
