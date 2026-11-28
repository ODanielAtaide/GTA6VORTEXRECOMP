cmake_minimum_required(VERSION 3.24)
project(vortex_native_graphics LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC OR NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "Vortex native graphics requires a Windows x64 MSVC target")
endif()
find_package(Vulkan REQUIRED)
find_package(Python3 REQUIRED COMPONENTS Interpreter)
get_filename_component(WINDOWS_KITS
  "[HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows Kits\\Installed Roots;KitsRoot10]"
  ABSOLUTE)
set(VORTEX_SDK_D3D12_HEADER
  "${WINDOWS_KITS}/Include/${CMAKE_VS_WINDOWS_TARGET_PLATFORM_VERSION}/um/d3d12.h"
  CACHE FILEPATH "Windows SDK header describing the guest-facing COM interfaces")
if(NOT EXISTS "${VORTEX_SDK_D3D12_HEADER}")
  message(FATAL_ERROR "Set VORTEX_SDK_D3D12_HEADER to the Windows SDK's d3d12.h")
endif()
set(STUBS "${CMAKE_CURRENT_BINARY_DIR}/generated/d3d12_stubs.h")
add_custom_command(OUTPUT "${STUBS}"
  COMMAND "${Python3_EXECUTABLE}" -I "${CMAKE_CURRENT_SOURCE_DIR}/gen_d3d12_stubs.py"
    --header "${VORTEX_SDK_D3D12_HEADER}" --out "${STUBS}"
    ID3D12GraphicsCommandList8 ID3D12CommandQueue ID3D12CommandAllocator ID3D12Fence1
    ID3D12Device11 ID3D12DescriptorHeap ID3D12Heap ID3D12Resource ID3D12QueryHeap
    ID3D12RootSignature ID3D12PipelineState ID3D12CommandSignature ID3D12PipelineLibrary1
    ID3D12ShaderCacheSession
  DEPENDS gen_d3d12_stubs.py report_paths.py "${VORTEX_SDK_D3D12_HEADER}"
  VERBATIM)

add_library(vortex_native_graphics STATIC
  bridge.cpp
  src/root_signature.cpp
  src/vortex_root_signature.cpp
  src/vulkan_aftermath.cpp
  shims/d3d12_com.cpp
  shims/d3d12_vulkan_device.cpp
  shims/d3d12_xs_device.cpp
  shims/d3d12_xs_pso_stream.cpp
  shims/dxgi_com.cpp
  shims/package_splash.cpp
  shims/registry.cpp
  shims/xbox_present.cpp
  "${STUBS}")
target_compile_features(vortex_native_graphics PRIVATE cxx_std_20)
target_compile_definitions(vortex_native_graphics PRIVATE
  WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE LEONIDA_VULKAN_DEVICE _CRT_SECURE_NO_WARNINGS)
target_compile_options(vortex_native_graphics PRIVATE /EHsc /permissive- /utf-8 /bigobj)
target_include_directories(vortex_native_graphics PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/generated")
target_link_libraries(vortex_native_graphics PRIVATE
  Vulkan::Headers ole32 windowscodecs xmllite shlwapi user32 gdi32 psapi)

option(VORTEX_NATIVE_GRAPHICS_TESTS "Build the in-tree device smoke test" OFF)
if(VORTEX_NATIVE_GRAPHICS_TESTS)
  enable_testing()
  add_executable(vortex_native_device_test test/device.cpp)
  target_compile_features(vortex_native_device_test PRIVATE cxx_std_20)
  target_compile_definitions(vortex_native_device_test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
  target_link_libraries(vortex_native_device_test PRIVATE vortex_native_graphics Vulkan::Headers)
  add_test(NAME vortex_native_device COMMAND vortex_native_device_test)
endif()
