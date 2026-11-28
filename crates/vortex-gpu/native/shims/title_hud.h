#pragma once
// VORTEX_TITLE_HUD=1 shows live counters in the title bar of the Vulkan window:
//   rate and timing of accepted Vulkan presents (average and worst frame in the last half second), or how long
//   since the last frame when there was none;
//   the process's private memory (its commit charge) and working set;
//   its GPU memory usage and budget from VK_EXT_memory_budget; "n/a" when the Vulkan driver has no figures;
//   graphics and compute pipelines created, and the seconds spent in vkCreate*Pipelines in the last interval,
//   which is where an inline compile shows up as a hitch (needs LEONIDA_MEM_PROBE, src/mem_probe.h; else "-");
//   time since the first present.
// The text is set from a thread of its own with SendMessageTimeout, so a busy or hung window thread never holds
// the present path.

#include <windows.h>
#include <psapi.h>

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cwchar>
#include <functional>
#include <string>
#include <thread>
#include <utility>

#include "../src/mem_probe.h"
#include "../src/memory_figures.h"

namespace rt::d3d12_xs {

class TitleHud {
public:
    static bool enabled() {
        static const bool on = [] {
            char v[2] = {};
            return GetEnvironmentVariableA("VORTEX_TITLE_HUD", v, sizeof v) == 1 && v[0] != '0';
        }();
        return on;
    }

    // The VRAM figure's source is called on the HUD thread. By default, use the last Vulkan budget published by the device.
    using VramSource = std::function<leonida::VramBudget()>;

    explicit TitleHud(HWND window, VramSource vram = {}) : window_(window), vram_(std::move(vram)) {
        QueryPerformanceFrequency(&frequency_);
        stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (stop_) thread_ = std::thread([this] { run(); });
    }
    TitleHud(const TitleHud&) = delete;
    TitleHud& operator=(const TitleHud&) = delete;
    ~TitleHud() {
        if (stop_) SetEvent(stop_);
        if (thread_.joinable()) thread_.join();
        if (stop_) CloseHandle(stop_);
    }

    // On every accepted Vulkan present.
    void note_present() {
        LARGE_INTEGER now;
        QueryPerformanceCounter(&now);
        const std::int64_t previous = last_present_.exchange(now.QuadPart, std::memory_order_relaxed);
        if (previous) {
            const std::int64_t gap = now.QuadPart - previous;
            std::int64_t worst = worst_gap_.load(std::memory_order_relaxed);
            while (gap > worst && !worst_gap_.compare_exchange_weak(worst, gap, std::memory_order_relaxed)) {}
        } else {
            first_present_.store(now.QuadPart, std::memory_order_relaxed);
        }
        presents_.fetch_add(1, std::memory_order_relaxed);
    }

private:
    static constexpr DWORD interval_ms = 500;

    void run() {
        std::uint64_t last_presents = 0, last_compile_ns = 0;
        LARGE_INTEGER last;
        QueryPerformanceCounter(&last);
        while (WaitForSingleObject(stop_, interval_ms) == WAIT_TIMEOUT) {
            if (!IsWindow(window_)) return;
            LARGE_INTEGER now;
            QueryPerformanceCounter(&now);
            const double seconds = double(now.QuadPart - last.QuadPart) / double(frequency_.QuadPart);
            last = now;
            const std::uint64_t presents = presents_.load(std::memory_order_relaxed);
            const std::uint64_t frames = presents - last_presents;
            last_presents = presents;
            const double worst_ms =
                double(worst_gap_.exchange(0, std::memory_order_relaxed)) * 1000.0 / double(frequency_.QuadPart);
            const double fps = seconds > 0 ? double(frames) / seconds : 0.0;
            const double frame_ms = frames ? seconds * 1000.0 / double(frames) : 0.0;

            namespace mf = leonida::memory_figures;
            const std::wstring memory = widen(mf::ram_text(mf::process_memory()));
            leonida::VramBudget vram = vram_ ? vram_() : mf::latest_vram();
            const std::wstring video = widen(mf::vram_text(vram));

            wchar_t pipelines[128] = L"PSO -";
            auto& probe = leonida::mem_probe::state();
            if (probe.installed) {
                std::uint64_t graphics, compute, ns;
                {
                    std::lock_guard lock(probe.mutex);
                    graphics = probe.graphics_made;
                    compute = probe.compute_made;
                    ns = probe.graphics_ns + probe.compute_ns;
                }
                const double compiling = double(ns - last_compile_ns) / 1e9;
                last_compile_ns = ns;
                std::swprintf(pipelines, 128, L"PSO %llug %lluc, compiling %.1f s", (unsigned long long)graphics,
                              (unsigned long long)compute, compiling);
            }

            const std::int64_t first = first_present_.load(std::memory_order_relaxed);
            const unsigned up = first ? unsigned((now.QuadPart - first) / frequency_.QuadPart) : 0;

            // No present in this interval: how long since the last one, which tells a hang from a slow frame.
            wchar_t frame[96];
            const std::int64_t last_present = last_present_.load(std::memory_order_relaxed);
            if (frames || !last_present)
                std::swprintf(frame, 96, L"%.0f queued/s %.1f ms (worst %.0f)", fps, frame_ms, worst_ms);
            else
                std::swprintf(frame, 96, L"no Vulkan present for %.1f s",
                              double(now.QuadPart - last_present) / double(frequency_.QuadPart));
            wchar_t text[512];
            std::swprintf(text, 512, L"Vortex Vulkan | %ls | %ls | %ls | %ls | %u:%02u", frame, memory.c_str(),
                          video.c_str(), pipelines, up / 60, up % 60);
            DWORD_PTR ignored = 0;
            SendMessageTimeoutW(window_, WM_SETTEXT, 0, reinterpret_cast<LPARAM>(text), SMTO_ABORTIFHUNG,
                                interval_ms, &ignored);
        }
    }

    // memory_figures' text is ASCII.
    static std::wstring widen(const std::string& text) { return std::wstring(text.begin(), text.end()); }

    HWND window_;
    VramSource vram_;
    HANDLE stop_ = nullptr;
    std::thread thread_;
    LARGE_INTEGER frequency_{};
    std::atomic<std::int64_t> last_present_{0}, first_present_{0}, worst_gap_{0};
    std::atomic<std::uint64_t> presents_{0};
};

}  // namespace rt::d3d12_xs
