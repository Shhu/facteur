#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Started by the self-update (selfupdate.rs) while the previous instance quits: wait for it
    // first, or the single-instance guard would hand this launch to it and exit.
    #[cfg(windows)]
    if let Some(pid) = after_update_pid(std::env::args()) {
        wait_for_exit(pid);
    }
    facteur_app_lib::run()
}

#[cfg(windows)]
fn after_update_pid(mut args: impl Iterator<Item = String>) -> Option<u32> {
    args.find(|a| a == "--after-update")?;
    args.next()?.parse().ok()
}

#[cfg(windows)]
fn wait_for_exit(pid: u32) {
    use std::ffi::c_void;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // SAFETY: plain Win32 calls; the handle is checked, then closed once.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        WaitForSingleObject(handle, 30_000);
        CloseHandle(handle);
    }
}
