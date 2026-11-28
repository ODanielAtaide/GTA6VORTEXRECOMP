// SPDX-License-Identifier: GPL-3.0-or-later
#include "shims/d3d12_com.h"
#include "shims/d3d12_compile_exit.h"
#include "shims/d3d12_xs_device.h"
#include "shims/shims.h"
#include "shims/xmem.h"
#include "src/title_page_watch.h"
#include "src/title_geometry_watch.h"
#include "src/title_va_alias.h"

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
    leonida::geometry_writes::aliases.store(&rt::xmem::geometry_aliases, std::memory_order_release);
    rt::shims::set_unimplemented_policy(rt::shims::UnimplementedPolicy::Stop);
}

extern "C" bool vortex_native_gpu_paths(const wchar_t* shaders, const wchar_t* pipelines) {
    // These inherited environment names are internal to the graphics sources.
    // The public Vortex CLI sets the paths before creating any devices.
    for (const wchar_t* name : {L"LEONIDA_STUB_PIPELINES", L"LEONIDA_SKIP_COMPUTE",
                               L"LEONIDA_ASYNC_PIPELINES", L"LEONIDA_COMPILE_HELPER"}) {
        _wputenv_s(name, L"");
    }
    const auto set_default = [](const wchar_t* name, const wchar_t* value) {
        if (!GetEnvironmentVariableW(name, nullptr, 0)) _wputenv_s(name, value);
    };
    set_default(L"LEONIDA_GRAPHICS_STAGE_LAYOUT", L"1");
    set_default(L"LEONIDA_FRAME_PACING", L"1");
    set_default(L"LEONIDA_PIPELINE_COMPILE", L"on-demand");
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

extern "C" void vortex_native_gpu_track_guest_memory(std::uintptr_t base, std::size_t bytes, bool watched) {
    if (!base || !bytes || bytes > UINTPTR_MAX - base) return;
    leonida::title_memory_map::changed(base, base + bytes);
    leonida::title_va_aliases().add_title_memory(base, bytes);
    if (watched) leonida::title_write_watch::add(base, bytes);
    else leonida::title_write_watch::mark(base, base + bytes);
}

extern "C" void vortex_native_gpu_release_guest_memory(std::uintptr_t base) {
    leonida::title_write_watch::remove(base);
    leonida::title_va_aliases().remove_title_memory(base);
    leonida::title_memory_map::changed();
}

extern "C" void vortex_native_gpu_remap_guest_memory(std::uintptr_t base, std::size_t bytes) {
    if (!base || !bytes || bytes > UINTPTR_MAX - base) return;
    leonida::title_memory_map::changed(base, base + bytes);
    leonida::title_write_watch::mark(base, base + bytes);
    leonida::geometry_writes::notify(base, base + bytes, leonida::geometry_writes::Event::replaced);
}

extern "C" std::uint64_t vortex_native_gpu_begin_host_write(std::uintptr_t base, std::size_t size,
                                                           const OVERLAPPED* request) {
    if (size > UINTPTR_MAX - base) return 0;
    return leonida::kernel_writes::begin_write(reinterpret_cast<const void*>(base), size, request);
}

extern "C" void vortex_native_gpu_end_host_write(std::uint64_t pin, bool pending) {
    leonida::kernel_writes::end_write(pin, pending);
}

extern "C" void vortex_native_gpu_note_texture_layout(std::uint32_t dimension, std::uint32_t format,
                                                        std::uint64_t width, std::uint32_t height,
                                                        std::uint32_t depth, std::uint32_t mips, std::uint32_t layout) {
    leonida::title_xg_layouts::note({dimension, format, height, depth, mips, width}, layout);
}

extern "C" void vortex_native_gpu_save_cache() {
    rt::d3d12::stop_pipeline_compiles(250);
    rt::d3d12::save_pipeline_caches_at_exit(250);
}
