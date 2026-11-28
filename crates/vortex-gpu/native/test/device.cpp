// SPDX-License-Identifier: GPL-3.0-or-later
#include "../shims/d3d12_com.h"
#include "../src/title_va_alias.h"
#include "../src/title_page_watch.h"
#include "../src/gfx10_descriptors.h"

#include <cstdio>
#include <filesystem>
#include <fstream>

extern "C" HRESULT vortex_native_gpu_create(const IID*, void**);
extern "C" void vortex_native_gpu_track_guest_memory(std::uintptr_t, std::size_t, bool);
extern "C" void vortex_native_gpu_release_guest_memory(std::uintptr_t);

static_assert(leonida::gfx10::format_from_vulkan(VK_FORMAT_D16_UNORM_S8_UINT) == 7);
static_assert(leonida::gfx10::vulkan_view_format(VK_FORMAT_D16_UNORM_S8_UINT, 7) == VK_FORMAT_D16_UNORM_S8_UINT);

int main() {
    constexpr std::size_t bytes = 1 << 16;
    void* memory = VirtualAlloc(nullptr, bytes, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE);
    if (!memory) return 1;
    const auto base = reinterpret_cast<std::uintptr_t>(memory);
    auto& ranges = leonida::title_va_aliases();
    std::uint64_t first = 0, last = 0;
    vortex_native_gpu_track_guest_memory(base, bytes, true);
    const bool tracked = ranges.title_memory(base + bytes - 1, first, last) &&
                         first == base && last == base + bytes &&
                         !ranges.title_memory(base + bytes, first, last);
    VirtualFree(memory, 0, MEM_RELEASE);
    vortex_native_gpu_release_guest_memory(base);
    if (!tracked || ranges.title_memory(base, first, last)) return 2;

    namespace views = leonida::title_gpu_views;
    constexpr std::uintptr_t cpu = 0x20000000, gpu = 0x30000000;
    if (views::note_reservation(cpu, bytes, false) || !views::note_reservation(gpu, bytes, true) ||
        views::to_cpu(gpu + 32) != cpu + 32 || views::other_view(cpu + 32) != gpu + 32) return 9;
    views::note_gpu_mappings(gpu + 4096, gpu + 8192);
    if (views::to_cpu(gpu + 32) != gpu + 32 || views::other_view(cpu + 32) ||
        views::to_host_view(gpu + 32) != gpu + 32 || !views::independently_mapped(gpu + 32, 16)) return 10;
    vortex_native_gpu_release_guest_memory(cpu);
    if (views::count() || views::independently_mapped(gpu + 32, 16)) return 11;

    if (vortex_native_gpu_create(nullptr, nullptr) != E_POINTER) return 3;
    const IID iid = __uuidof(ID3D12Device);
    ID3D12Device* device = nullptr;
    if (FAILED(vortex_native_gpu_create(&iid, reinterpret_cast<void**>(&device))) || !device) return 4;
    D3D12_RESOURCE_DESC depth{};
    depth.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    depth.Width = 32;
    depth.Height = 16;
    depth.DepthOrArraySize = 1;
    depth.MipLevels = 1;
    depth.Format = DXGI_FORMAT_R32_TYPELESS;
    depth.SampleDesc.Count = 1;
    depth.Layout = D3D12_TEXTURE_LAYOUT_UNKNOWN;
    depth.Flags = D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    void* placement = VirtualAlloc(nullptr, bytes, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    if (!placement) return 12;
    const auto methods = *reinterpret_cast<void* const* const*>(device);
    using Place = HRESULT(STDMETHODCALLTYPE*)(ID3D12Device*, std::uint64_t, const D3D12_RESOURCE_DESC*,
                                              std::uint32_t, const D3D12_CLEAR_VALUE*, const IID*, void**);
    const auto place = reinterpret_cast<Place>(methods[133]); // Xbox CreatePlacedResourceX
    const IID resource_iid = __uuidof(ID3D12Resource);
    ID3D12Resource* texture = nullptr;
    const HRESULT placed = place(device, reinterpret_cast<std::uintptr_t>(placement), &depth,
                                 D3D12_BARRIER_LAYOUT_COMMON, nullptr, &resource_iid,
                                 reinterpret_cast<void**>(&texture));
    const bool writable_depth = SUCCEEDED(placed) && texture &&
                                (texture->GetDesc().Flags & depth.Flags) == depth.Flags;
    if (texture) texture->Release();
    VirtualFree(placement, 0, MEM_RELEASE);
    if (!writable_depth) return 12;
    const auto folder = std::filesystem::temp_directory_path() /
                        ("vortex-vulkan-probe-" + std::to_string(GetCurrentProcessId()));
    const auto csv = folder / "frames.csv";
    if (_wputenv_s(L"VORTEX_BENCHMARK_CSV", csv.c_str()) != 0) return 5;
    auto presenter = rt::d3d12::create_window_presenter(device, nullptr, true, folder.string().c_str());
    device->Release();
    if (!presenter) return 5;
    bool notified = false;
    presenter->set_present_observer([&] { notified = true; });
    if (FAILED(presenter->present_blank(1)) || notified) return 5;
    std::vector<std::uint8_t> pixels;
    VkExtent2D extent{};
    VkFormat format{};
    const bool copied = presenter->read_back(pixels, &extent, &format);
    const bool no_presents = presenter->presented() == 0;
    presenter.reset();
    std::ifstream benchmark(csv);
    std::string header, row;
    std::getline(benchmark, header);
    std::getline(benchmark, row);
    benchmark.close();
    const bool logged = header == "run_unix_us,output,frame,elapsed_us,interval_us,width,height" &&
                        row.find(",offscreen_submit,1,0,,1280,720") != std::string::npos;
    std::filesystem::remove(csv);
    std::filesystem::remove(folder / "frame_00001.ppm");
    std::filesystem::remove(folder / "index.txt");
    std::filesystem::remove(folder);
    if (!logged || !copied || !no_presents || format != VK_FORMAT_B8G8R8A8_UNORM || !extent.width || !extent.height ||
        pixels.size() != static_cast<std::size_t>(extent.width) * extent.height * 4) return 6;
    for (std::size_t i = 0; i < pixels.size(); i += 4)
        if (pixels[i] || pixels[i + 1] || pixels[i + 2] || pixels[i + 3] != 255) return 7;

    if (!GetModuleHandleW(L"vulkan-1.dll") || GetModuleHandleW(L"d3d12.dll")) return 8;
    std::fprintf(stderr, "Win32 interop modules: dxgi=%d, d3d11=%d\n",
                 int(GetModuleHandleW(L"dxgi.dll") != nullptr), int(GetModuleHandleW(L"d3d11.dll") != nullptr));
    return 0;
}
