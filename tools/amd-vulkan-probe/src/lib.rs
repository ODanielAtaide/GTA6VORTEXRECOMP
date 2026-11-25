// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent AMD Vulkan context for synthetic workloads.
mod compute;
mod graphics;
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
    let mut enabled12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
    let mut enabled13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).dynamic_rendering(true)
        .subgroup_size_control(f13.subgroup_size_control != 0)
        .compute_full_subgroups(f13.compute_full_subgroups != 0);
    let device_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues).push_next(&mut enabled12).push_next(&mut enabled13);
    let device = unsafe { instance.create_device(physical, &device_info, None)? };
    println!("PASS: AMD Vulkan logical device initialized");
    Ok((physical, device, family))
}
