//! In-process Vulkan device behind the console-facing COM ABI.

use crate::object::{HRESULT, Iid, This};
use std::ffi::c_void;

pub type GuestAddress = extern "C" fn(u64) -> bool;
pub type Alloc = extern "C" fn(usize, u64) -> *mut c_void;
pub type Free = extern "C" fn(*mut c_void, u64);
pub type MapPages = extern "C" fn(*mut c_void, *const c_void, u64, bool) -> bool;
pub type Aliases = unsafe extern "C" fn(usize, *mut usize, usize) -> usize;

unsafe extern "C" {
    fn vortex_native_gpu_configure(
        guest_address: GuestAddress,
        alloc: Alloc,
        free: Free,
        map_pages: MapPages,
        aliases: Aliases,
    );
    fn vortex_native_gpu_create(iid: *const Iid, out: *mut This) -> HRESULT;
    fn vortex_native_gpu_paths(shaders: *const u16, pipelines: *const u16) -> bool;
    fn vortex_native_gpu_save_cache();
}

pub fn configure(guest_address: GuestAddress, alloc: Alloc, free: Free, map_pages: MapPages, aliases: Aliases) {
    // SAFETY: these static entry points use the matching x64 C ABI.
    unsafe { vortex_native_gpu_configure(guest_address, alloc, free, map_pages, aliases) };
}

/// # Safety
///
/// The guest must pass a readable IID and a writable COM result pointer.
pub unsafe fn create(iid: *const Iid, out: *mut This) -> HRESULT {
    // SAFETY: forwarded under the caller's contract.
    unsafe { vortex_native_gpu_create(iid, out) }
}

pub fn use_paths(shaders: &std::path::Path, pipelines: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |path: &std::path::Path| {
        path.as_os_str().encode_wide().chain([0]).collect::<Vec<_>>()
    };
    let shaders = wide(shaders);
    let pipelines = wide(pipelines);
    // SAFETY: both buffers contain a terminating NUL and stay alive through the call.
    if unsafe { vortex_native_gpu_paths(shaders.as_ptr(), pipelines.as_ptr()) } {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn save_cache() {
    // SAFETY: cache synchronization and device lifetimes are managed by the renderer.
    unsafe { vortex_native_gpu_save_cache() };
}
