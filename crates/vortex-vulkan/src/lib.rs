// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared experimental AMD Vulkan host library. Not yet a title graphics backend.
pub mod commands;
mod compute;
mod graphics;
mod transfer;
mod descriptors;
#[path = "../../vortex-gpu/src/vulkan_descriptors.rs"]
mod descriptor_journal;
// Share the exact tested planner without linking the legacy D3D12 runtime.
#[path = "../../vortex-gpu/src/vulkan_upload.rs"]
mod dirty_upload;
mod pipeline_cache;
pub mod spirv;
use ash::{vk, Entry};
use std::{error::Error, ffi::CStr, path::Path};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Owns the Vulkan loader, instance and logical device in dependency order.
/// The loader must remain alive until both Vulkan objects are destroyed.
pub struct Context {
    _entry: Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    family: u32,
}

impl Context {
    pub fn new() -> Result<Self> {
        let entry = unsafe { Entry::load()? };
        let loader_version = unsafe { entry.try_enumerate_instance_version()? }
            .unwrap_or(vk::API_VERSION_1_0);
        if loader_version < vk::API_VERSION_1_3 {
            return Err("Vulkan 1.3 loader required".into());
        }
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
        let create = vk::InstanceCreateInfo::default().application_info(&app);
        let instance = unsafe { entry.create_instance(&create, None)? };
        match select_device(&instance) {
            Ok((physical, device, family)) => Ok(Self {
                _entry: entry, instance, physical, device, family,
            }),
            Err(error) => {
                unsafe { instance.destroy_instance(None) };
                Err(error)
            }
        }
    }

    /// Create a reusable buffer command session with exclusive device access.
    pub fn command_session(&mut self) -> Result<commands::Session<'_>> {
        commands::Session::new(self)
    }

    /// Verify reusable buffer and image command execution.
    pub fn verify_commands(&mut self) -> Result<()> {
        commands::verify(self)?;
        commands::verify_images(self)?;
        commands::verify_attachments(self)
    }

    /// Verify the reusable graphics pipeline with trusted fixture SPIR-V.
    pub fn verify_draws(&mut self,vert:&Path,frag:&Path) -> Result<()> {
        commands::verify_draws(self,vert,frag)?;
        commands::verify_indexed(self,vert,frag)
    }

    /// Verify reusable compute pipelines and ordered dispatches.
    pub fn verify_dispatch(&mut self,path:&Path) -> Result<()> {
        commands::verify_dispatch(self,path)
    }

    /// Verify direct native diagnostic shader compilation and dispatch.
    pub fn verify_native_dispatch(&mut self) -> Result<()> {
        commands::verify_native_dispatch(self)?;
        commands::verify_native_buffers(self)?;
        commands::verify_native_memory(self)?;
        commands::verify_native_records(self)
    }

    /// Emit advanced native shader fixtures without initializing a device.
    pub fn emit_advanced(dir:&Path) -> Result<()> {
        commands::emit_advanced(dir)
    }

    /// Verify the bounded native CFG and advanced compute operations.
    pub fn verify_advanced(&mut self) -> Result<()> {
        commands::verify_advanced(self)
    }

    /// Emit original native vertex and fragment modules without a GPU.
    pub fn emit_native_graphics(dir:&Path) -> Result<()> {
        commands::emit_native_graphics(dir)
    }

    /// Verify native graphics stage lowering and indexed rasterization.
    pub fn verify_native_graphics(&mut self) -> Result<()> {
        commands::verify_native_graphics(self)?;
        commands::verify_native_textures(self)
    }

    /// Verify sparse non-uniform runtime descriptor arrays on supported hardware.
    pub fn verify_bindless(&mut self, path: &Path) -> Result<()> {
        descriptors::run_bindless(&self.instance, self.physical, &self.device, self.family, path)
    }

    /// Verify selective Vulkan storage buffer descriptor publications.
    pub fn verify_descriptors(&mut self, path: &Path) -> Result<()> {
        descriptors::run(&self.instance, self.physical, &self.device, self.family, path)
    }

    /// Verify bounded asynchronous Vulkan submission and safe slot reuse.
    pub fn verify_async(&mut self) -> Result<()> {
        transfer::run_async(&self.instance, self.physical, &self.device, self.family)
    }

    /// Compare serial and parallel Vulkan command recording with full readback.
    pub fn verify_parallel(&mut self) -> Result<()> {
        transfer::run_parallel(&self.instance, self.physical, &self.device, self.family)
    }

    /// Exercise real Vulkan transfers with the shared dirty upload planner.
    pub fn verify_transfers(&mut self) -> Result<()> {
        transfer::run(&self.instance, self.physical, &self.device, self.family)
    }

    /// Execute and verify the original synthetic compute shader.
    /// The shader input must be a trusted, validated version of write.comp.
    pub fn compute(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            |i| i.wrapping_mul(1664525).wrapping_add(1013904223))
    }

    /// Verify the original mixed arithmetic and bitwise shader.
    pub fn compute_mixed(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            spirv::mixed_expected)
    }

    /// Verify the synthetic shift and unsigned conditional-selection shader.
    pub fn compute_conditional(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            spirv::conditional_expected)
    }

    /// Verify the synthetic finite FP32 arithmetic shader bit for bit.
    pub fn compute_float(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            spirv::float_expected)
    }

    /// Verify decoded CMPX filtering and mask restoration.
    pub fn compute_cmpx_demo(&mut self, path: &Path, wave64: bool) -> Result<()> {
        let expected: fn(u32) -> u32 = if wave64 {
            vortex_shader::spirv_vector::cmpx_expected64
        } else { vortex_shader::spirv_vector::cmpx_expected32 };
        compute::run_with_wave(&self.instance, self.physical, &self.device, self.family, path,
            expected, Some(if wave64 { 64 } else { 32 }))
    }

    /// Verify decoded EXEC mask changes and restoration.
    pub fn compute_exec_demo(&mut self, path: &Path, wave64: bool) -> Result<()> {
        let expected: fn(u32) -> u32 = if wave64 {
            vortex_shader::spirv_vector::exec_expected64
        } else { vortex_shader::spirv_vector::exec_expected32 };
        compute::run_with_wave(&self.instance, self.physical, &self.device, self.family, path,
            expected, Some(if wave64 { 64 } else { 32 }))
    }

    /// Verify per-invocation vector arithmetic and the diagnostic EXEC mask.
    pub fn compute_vector_demo(&mut self, path: &Path, wave64: bool) -> Result<()> {
        let expected: fn(u32) -> u32 = if wave64 {
            vortex_shader::spirv_vector::expected64
        } else { vortex_shader::spirv_vector::expected32 };
        compute::run_with_wave(&self.instance, self.physical, &self.device, self.family, path,
            expected, Some(if wave64 { 64 } else { 32 }))
    }

    /// Verify original scalar arithmetic and condition flags.
    pub fn compute_carry_demo(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path, |_| 42)
    }

    /// Verify the closed scalar fixture decoded by vortex-shader.
    pub fn compute_scalar_demo(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path, |_| 15)
    }

    /// Verify the original synthetic bounded-loop shader.
    pub fn compute_loop(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            spirv::loop_expected)
    }

    /// Verify structured control flow in the original synthetic shader.
    pub fn compute_branch(&mut self, path: &Path) -> Result<()> {
        compute::run(&self.instance, self.physical, &self.device, self.family, path,
            spirv::branch_expected)
    }

    /// Verify subgroup size and reduction on the supplied original wave.comp shader.
    pub fn compute_wave(&mut self, path: &Path, size: u32) -> Result<()> {
        let expected: fn(u32) -> u32 = match size {
            32 => |_| (32 << 16) | 32,
            64 => |_| (64 << 16) | 64,
            _ => return Err("Wave size must be 32 or 64".into()),
        };
        compute::run_with_wave(&self.instance, self.physical, &self.device,
            self.family, path, expected, Some(size))
    }

    /// Verify indexed original geometry with sampled UV attributes.
    pub fn graphics_indexed(&mut self, vert: &Path, frag: &Path) -> Result<()> {
        graphics::run_indexed(&self.instance, self.physical, &self.device, self.family, vert, frag)
    }

    /// Verify upload and sampling of an original synthetic RGBA8 texture.
    pub fn graphics_texture(&mut self, vert: &Path, frag: &Path) -> Result<()> {
        graphics::run_texture(&self.instance, self.physical, &self.device, self.family, vert, frag)
    }

    /// Execute and verify the original synthetic full-screen graphics workload.
    /// Inputs must be trusted, validated versions of the supplied shaders.
    pub fn graphics(&mut self, vert: &Path, frag: &Path) -> Result<()> {
        graphics::run(&self.instance, self.physical, &self.device, self.family, vert, frag)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn select_device(instance: &ash::Instance) -> Result<(vk::PhysicalDevice, ash::Device, u32)> {
    let devices = unsafe { instance.enumerate_physical_devices()? };
    let mut selected = None;
    for physical in devices {
        let props = unsafe { instance.get_physical_device_properties(physical) };
        let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy();
        println!("adapter: {name}; vendor={:04x}; device={:04x}", props.vendor_id, props.device_id);
        if props.vendor_id != 0x1002
            || props.device_type != vk::PhysicalDeviceType::DISCRETE_GPU
            || props.api_version < vk::API_VERSION_1_3
        {
            continue;
        }
        let queues = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let Some(family) = queues.iter().position(|q| {
            q.queue_count > 0 && q.queue_flags.contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
        }) else { continue };
        if selected.is_none() {
            selected = Some((physical, props, family as u32));
        }
    }
    let (physical, props, family) = selected.ok_or("No compatible discrete AMD Vulkan 1.3 adapter")?;
    let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy();
    println!("selected: {name}; queue family={family}");
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut f12).push_next(&mut f13);
    unsafe { instance.get_physical_device_features2(physical, &mut features) };
    println!("timeline semaphore: {}", f12.timeline_semaphore != 0);
    println!("descriptor indexing: {}", f12.descriptor_indexing != 0);
    println!("buffer device address: {}", f12.buffer_device_address != 0);
    println!("synchronization2: {}", f13.synchronization2 != 0);
    println!("dynamic rendering: {}", f13.dynamic_rendering != 0);
    println!("subgroup size control: {}", f13.subgroup_size_control != 0);
    let mut subgroup = vk::PhysicalDeviceSubgroupSizeControlProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut subgroup);
    unsafe { instance.get_physical_device_properties2(physical, &mut properties) };
    println!("subgroup size range: {}..{}", subgroup.min_subgroup_size, subgroup.max_subgroup_size);
    let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
    for (i, heap) in memory.memory_heaps[..memory.memory_heap_count as usize].iter().enumerate() {
        println!("heap {i}: {} MiB; {:?}", heap.size >> 20, heap.flags);
    }
    for (i, ty) in memory.memory_types[..memory.memory_type_count as usize].iter().enumerate() {
        println!("memory type {i}: heap {}; {:?}", ty.heap_index, ty.property_flags);
    }
    if f12.timeline_semaphore == 0 || f13.synchronization2 == 0 || f13.dynamic_rendering == 0 {
        return Err("Required device features missing".into());
    }

    let priority = [1.0];
    let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priority)];
    let bindless = f12.descriptor_indexing != 0 && f12.runtime_descriptor_array != 0
        && f12.shader_storage_buffer_array_non_uniform_indexing != 0
        && f12.descriptor_binding_partially_bound != 0;
    println!("bindless storage buffer runtime arrays: {bindless}");
    let mut enabled12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true)
        .descriptor_indexing(bindless)
        .runtime_descriptor_array(bindless)
        .shader_storage_buffer_array_non_uniform_indexing(bindless)
        .descriptor_binding_partially_bound(bindless);
    let mut enabled13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).dynamic_rendering(true)
        .subgroup_size_control(f13.subgroup_size_control != 0)
        .compute_full_subgroups(f13.compute_full_subgroups != 0);
    let device_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues).push_next(&mut enabled12).push_next(&mut enabled13);
    let device = unsafe { instance.create_device(physical, &device_info, None)? };
    println!("PASS: AMD Vulkan logical device initialized");
    Ok((physical, device, family))
}
