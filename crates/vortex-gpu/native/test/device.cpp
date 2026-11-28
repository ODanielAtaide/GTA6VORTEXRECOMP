// SPDX-License-Identifier: GPL-3.0-or-later
#include <windows.h>
#include <d3d12.h>

extern "C" HRESULT vortex_native_gpu_create(const IID*, void**);

int main() {
    if (vortex_native_gpu_create(nullptr, nullptr) != E_POINTER) return 1;
    const IID iid = __uuidof(ID3D12Device);
    void* device = nullptr;
    if (FAILED(vortex_native_gpu_create(&iid, &device)) || !device) return 2;
    static_cast<IUnknown*>(device)->Release();
    if (GetModuleHandleW(L"d3d12.dll") || GetModuleHandleW(L"dxgi.dll")) return 3;
    return 0;
}
