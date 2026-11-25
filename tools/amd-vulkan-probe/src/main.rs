// SPDX-License-Identifier: GPL-3.0-or-later
// Independent synthetic capability probe; does not load a title.
mod compute;
mod graphics;
use ash::{vk, Entry};
use std::{error::Error, ffi::CStr};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let entry = unsafe { Entry::load()? };
    let loader_version = unsafe { entry.try_enumerate_instance_version()? }
        .unwrap_or(vk::API_VERSION_1_0);
    if loader_version < vk::API_VERSION_1_3 {
        return Err("Vulkan 1.3 loader required".into());
    }
    let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
    let create = vk::InstanceCreateInfo::default().application_info(&app);
    let instance = unsafe { entry.create_instance(&create, None)? };
    let result = probe(&instance);
    unsafe { instance.destroy_instance(None) };
    result
}

fn probe(instance: &ash::Instance) -> Result<()> {
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
    let mut enabled13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).dynamic_rendering(true);
    let device_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues).push_next(&mut enabled12).push_next(&mut enabled13);
    let device = unsafe { instance.create_device(physical, &device_info, None)? };
    println!("PASS: AMD Vulkan logical device initialized");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [flag, vert, frag] if flag == "--graphics" => graphics::run(
            instance, physical, &device, family, std::path::Path::new(vert), std::path::Path::new(frag)),
        [path] => compute::run(instance, physical, &device, family, std::path::Path::new(path)),
        [] => Ok(()),
        _ => Err("Usage: amd-vulkan-probe [compute.spv | --graphics vertex.spv fragment.spv]".into()),
    };
    unsafe { device.destroy_device(None) };
    result
}
