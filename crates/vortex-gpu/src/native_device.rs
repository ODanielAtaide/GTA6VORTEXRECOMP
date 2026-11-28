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
    fn vortex_native_gpu_precompile_compute(jobs: u32, limit: usize, filter: *const std::ffi::c_char) -> i32;
    fn vortex_native_gpu_track_guest_memory(base: usize, bytes: usize, watched: bool);
    fn vortex_native_gpu_release_guest_memory(base: usize);
    fn vortex_native_gpu_note_guest_reservation(base: usize, bytes: usize, gpu_only: bool);
    fn vortex_native_gpu_note_gpu_mappings(base: usize, bytes: usize);
    fn vortex_native_gpu_remap_guest_memory(base: usize, bytes: usize);
    fn vortex_native_gpu_begin_host_write(base: usize, bytes: usize, request: *const c_void) -> u64;
    fn vortex_native_gpu_end_host_write(pin: u64, pending: bool);
    fn vortex_native_gpu_note_texel_write(base: usize, bytes: usize);
    fn vortex_native_gpu_note_texture_layout(
        dimension: u32, format: u32, width: u64, height: u32, depth: u32, mips: u32, layout: u32,
    );
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

pub fn track_guest_memory(base: usize, bytes: usize, watched: bool) {
    // SAFETY: the renderer records address ranges without dereferencing an address here.
    unsafe { vortex_native_gpu_track_guest_memory(base, bytes, watched) };
}

pub fn release_guest_memory(base: usize) {
    // SAFETY: the renderer removes the recorded range without accessing its former backing.
    unsafe { vortex_native_gpu_release_guest_memory(base) };
}

pub fn note_guest_reservation(base: usize, bytes: usize, gpu_only: bool) {
    // SAFETY: only address metadata crosses the bridge.
    unsafe { vortex_native_gpu_note_guest_reservation(base, bytes, gpu_only) };
}

pub fn note_gpu_mappings(base: usize, bytes: usize) {
    // SAFETY: only address metadata crosses the bridge.
    unsafe { vortex_native_gpu_note_gpu_mappings(base, bytes) };
}

pub fn remap_guest_memory(base: usize, bytes: usize) {
    // SAFETY: the renderer receives only an address-range notification.
    unsafe { vortex_native_gpu_remap_guest_memory(base, bytes) };
}

/// # Safety
///
/// A non-null request is a live Windows OVERLAPPED. Its caller owns it until
/// the I/O completes. The buffer must remain live under the host API's contract.
pub unsafe fn begin_host_write(base: usize, bytes: usize, request: *const c_void) -> u64 {
    // SAFETY: the caller supplies the lifetime of the buffer and request.
    unsafe { vortex_native_gpu_begin_host_write(base, bytes, request) }
}

pub fn end_host_write(pin: u64, pending: bool) {
    // SAFETY: this releases or transfers a pin created by begin_host_write.
    unsafe { vortex_native_gpu_end_host_write(pin, pending) };
}

pub fn note_texel_write(base: usize, bytes: usize) {
    // SAFETY: the renderer records the dirty address span without accessing it here.
    unsafe { vortex_native_gpu_note_texel_write(base, bytes) };
}

pub fn note_texture_layout(
    dimension: u32, format: u32, width: u64, height: u32, depth: u32, mips: u32, layout: u32,
) {
    // SAFETY: the renderer receives a validated shape and swizzle word by value.
    unsafe { vortex_native_gpu_note_texture_layout(dimension, format, width, height, depth, mips, layout) };
}

pub fn precompile_compute(jobs: u32, limit: usize, filter: Option<&std::ffi::CStr>) -> i32 {
    let filter = filter.map_or(std::ptr::null(), std::ffi::CStr::as_ptr);
    // SAFETY: a non-null filter remains a live C string for the call.
    unsafe { vortex_native_gpu_precompile_compute(jobs, limit, filter) }
}

pub fn save_cache() {
    // SAFETY: cache synchronization and device lifetimes are managed by the renderer.
    unsafe { vortex_native_gpu_save_cache() };
}
