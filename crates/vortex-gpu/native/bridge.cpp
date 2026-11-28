// SPDX-License-Identifier: GPL-3.0-or-later
#include "shims/d3d12_com.h"
#include "shims/d3d12_compile_exit.h"
#include "shims/d3d12_xs_device.h"
#include "shims/shims.h"
#include "shims/xmem.h"

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <vector>

namespace {
using GuestAddress = bool (*)(std::uint64_t);
using Aliases = std::size_t (*)(std::uintptr_t, std::uintptr_t*, std::size_t);
std::atomic<Aliases> query_aliases{nullptr};
}

// Resolves aliases from Vortex's physical-page map when a diagnostic needs them.
void rt::xmem::geometry_aliases(std::uintptr_t address, std::vector<std::uintptr_t>& out) {
    const auto query = query_aliases.load(std::memory_order_acquire);
    if (!query) return;
    std::uintptr_t aliases[64];
    const std::size_t count = query(address, aliases, std::size(aliases));
    if (count <= std::size(aliases)) out.insert(out.end(), aliases, aliases + count);
}

extern "C" void vortex_native_gpu_configure(GuestAddress guest_address,
                                           rt::d3d12_xs::DescriptorHeapBackingAlloc alloc,
                                           rt::d3d12_xs::DescriptorHeapBackingFree free,
                                           rt::xmem::CopyPageMappingsFn map_pages,
                                           Aliases aliases) {
    rt::d3d12_xs::set_console_caller(guest_address);
    rt::d3d12_xs::set_default_descriptor_heap_allocator(alloc, free);
    rt::xmem::copy_page_mappings_fn = map_pages;
    query_aliases.store(aliases, std::memory_order_release);
    rt::shims::set_unimplemented_policy(rt::shims::UnimplementedPolicy::Stop);
}

extern "C" bool vortex_native_gpu_paths(const wchar_t* shaders, const wchar_t* pipelines) {
    // These inherited environment names are internal to the graphics sources.
    // The public Vortex CLI sets the paths before creating any devices.
    SetEnvironmentVariableW(L"LEONIDA_STUB_PIPELINES", nullptr);
    SetEnvironmentVariableW(L"LEONIDA_SKIP_COMPUTE", nullptr);
    SetEnvironmentVariableW(L"LEONIDA_ASYNC_PIPELINES", nullptr);
    SetEnvironmentVariableW(L"LEONIDA_COMPILE_HELPER", nullptr);
    return SetEnvironmentVariableW(L"LEONIDA_SHADERS", shaders)
        && SetEnvironmentVariableW(L"LEONIDA_PIPELINE_CACHE_DIR", pipelines);
}

extern "C" HRESULT vortex_native_gpu_create(const IID* iid, void** out) {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (!iid) return E_POINTER;
    ID3D12Device* device = nullptr;
    HRESULT hr = rt::d3d12::create_vulkan_device_standalone(
        __uuidof(ID3D12Device), reinterpret_cast<void**>(&device), rt::d3d12::QueueTestSettings::ignore);
    if (FAILED(hr)) return hr;
    hr = rt::d3d12_xs::create_device_proxy(device, *iid, out, /*console_lists=*/true);
    if (FAILED(hr)) {
        device->Release();
        return hr;
    }
    rt::d3d12::set_device_outer(device, static_cast<IUnknown*>(*out));
    std::fprintf(stderr, "VULKAN_GUEST_DEVICE_READY: in-process native device\n");
    return hr;
}

extern "C" void vortex_native_gpu_save_cache() {
    rt::d3d12::stop_pipeline_compiles(250);
    rt::d3d12::save_pipeline_caches_at_exit(250);
}
