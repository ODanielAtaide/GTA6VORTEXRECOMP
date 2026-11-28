//! `d3d12_xs.dll`: virtual memory setup, device creation, root signature
//! serialization. The device and everything it creates live in
//! `vortex-gpu`.

use std::ffi::c_void;

use vortex_gpu::device;
use vortex_gpu::object::{HRESULT, Iid, Object};

use crate::{HleExport, HleModule};

/// Device creation; first hands the device the title's general allocator
/// (`XMemAlloc`, which honours the title's hooks) for resource memory.
extern "system" fn create_device(
    adapter: *const c_void,
    parameters: *const u32,
    iid: *const Iid,
    out: *mut *const Object,
) -> HRESULT {
    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
    if vortex_gpu::vulkan_live::enabled() {
        use vortex_gpu::native_device;
        static CONFIGURE: std::sync::Once = std::sync::Once::new();
        CONFIGURE.call_once(|| native_device::configure(
            crate::runtime::is_guest_code, allocate_guest, free_guest,
            super::xmem::gpu_copy_pages, super::xmem::gpu_page_aliases,
        ));
        // SAFETY: this is the guest's DeviceX import, whose IID and result are live for the call.
        return unsafe { native_device::create(iid, out) };
    }
    vortex_gpu::resource::use_allocator(super::xmem::XMemAlloc, super::xmem::XMemFree);
    vortex_gpu::resource::use_page_mapper(super::xmem::map_alias);
    #[cfg(windows)]
    vortex_gpu::on_failure(host_gpu_failed);
    device::create(adapter, parameters, iid, out)
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
extern "C" fn allocate_guest(size: usize, attributes: u64) -> *mut c_void {
    super::xmem::XMemAlloc(size, attributes)
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
extern "C" fn free_guest(address: *mut c_void, attributes: u64) {
    super::xmem::XMemFree(address, attributes);
}

/// A host GPU failure ends the run like a crash: reported to the crash log,
/// then the process ends as the console ends a title.
#[cfg(windows)]
fn host_gpu_failed(what: &str) -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    vortex_loader::log_crash(&format!("Vortex: host GPU failure: {what}"));
    const DXGI_ERROR_DEVICE_REMOVED: u32 = 0x887A_0005;
    // SAFETY: ends our own process.
    unsafe { TerminateProcess(GetCurrentProcess(), DXGI_ERROR_DEVICE_REMOVED) };
    unreachable!("the process has ended")
}

/// Keeps the host's translations of the title's shaders in `dir` (a
/// folder of the cache) across runs, and its compiled pipelines beside it
/// (`pipelines`).
#[cfg(all(windows,feature = "vulkan"))]
pub fn enable_native_vulkan(){vortex_gpu::vulkan_live::enable();}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
pub fn use_native_paths(shaders: &std::path::Path, pipelines: &std::path::Path) -> std::io::Result<()> {
    vortex_gpu::native_device::use_paths(shaders, pipelines)
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc", feature = "vulkan"))]
pub fn precompile_native_compute(jobs: u32, limit: usize, filter: Option<&std::ffi::CStr>) -> i32 {
    vortex_gpu::native_device::precompile_compute(jobs, limit, filter)
}

pub fn use_shader_cache(dir: std::path::PathBuf) {
    #[cfg(all(windows,feature = "vulkan"))]
    if vortex_gpu::vulkan_live::enabled(){return;}
    #[cfg(windows)]
    {
        vortex_gpu::pipeline::use_pipeline_cache(dir.with_file_name("pipelines"));
        vortex_gpu::shader_cache::use_shader_cache(dir);
    }
    #[cfg(not(windows))]
    let _ = dir;
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "d3d12x",
        dlls: &["d3d12_xs.dll"],
        exports: vec![
            HleExport {
                name: "D3D12ConfigureVirtualMemoryX",
                address: device::configure_virtual_memory as *const () as usize,
            },
            HleExport {
                name: "D3D12XboxCreateDevice",
                address: create_device as *const () as usize,
            },
            HleExport {
                name: "D3D12SerializeRootSignature",
                address: vortex_gpu::root::serialize as *const () as usize,
            },
            HleExport {
                name: "D3D12SerializeVersionedRootSignature",
                address: vortex_gpu::root::serialize_versioned as *const () as usize,
            },
            // The AMD upscaler's (Series S).
            HleExport {
                name: "D3D12CreateRootSignatureDeserializer",
                address: vortex_gpu::root::create_deserializer as *const () as usize,
            },
            // A data import: the title reads the id at this address.
            HleExport {
                name: "IID_ID3D12Device",
                address: (&raw const device::IID) as usize,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HleResolver, HostPolicy};
    use vortex_loader::ImportResolver;
    use vortex_loader::Resolution;
    use vortex_pe::ImportSymbol;

    #[test]
    fn the_interface_id_import_is_bound_to_data() {
        let symbol = ImportSymbol::Name {
            hint: 0,
            name: "IID_ID3D12Device".into(),
        };
        let r = HleResolver::new(HostPolicy::none());
        let Resolution::Found { address, .. } = r.resolve("d3d12_xs.dll", &symbol) else {
            panic!("not bound");
        };
        // SAFETY: the address is our static id.
        assert_eq!(unsafe { *(address as *const [u8; 16]) }, device::IID);
    }
}
