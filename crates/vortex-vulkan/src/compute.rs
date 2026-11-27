// SPDX-License-Identifier: GPL-3.0-or-later
use ash::vk;
use std::{ffi::CStr, io::Cursor, path::Path, time::Instant};

const WORDS: usize = 1 << 20;
const BYTES: u64 = (WORDS * 4) as u64;

// Own all child objects even if setup or dispatch fails.
struct Resources<'a> {
    device: &'a ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    readback: vk::Buffer,
    readback_memory: vk::DeviceMemory,
    mapped_memory: vk::DeviceMemory,
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    shader: vk::ShaderModule,
    pipeline: vk::Pipeline,
    commands: vk::CommandPool,
    timeline: vk::Semaphore,
    queries: vk::QueryPool,
}

impl Drop for Resources<'_> {
    fn drop(&mut self) {
        let d = self.device;
        unsafe {
            let _ = d.device_wait_idle();
            if self.mapped_memory != vk::DeviceMemory::null() { d.unmap_memory(self.mapped_memory); }
            d.destroy_query_pool(self.queries, None);
            d.destroy_semaphore(self.timeline, None);
            d.destroy_command_pool(self.commands, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_shader_module(self.shader, None);
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_descriptor_pool(self.pool, None);
            d.destroy_descriptor_set_layout(self.layout, None);
            d.destroy_buffer(self.readback, None);
            d.free_memory(self.readback_memory, None);
            d.destroy_buffer(self.buffer, None);
            d.free_memory(self.memory, None);
        }
    }
}

pub fn run(instance: &ash::Instance, physical: vk::PhysicalDevice, d: &ash::Device,
           family: u32, path: &Path, expected_value: fn(u32) -> u32) -> crate::Result<()> {
    run_with_wave(instance, physical, d, family, path, expected_value, None)
}

pub fn run_with_wave(instance: &ash::Instance, physical: vk::PhysicalDevice, d: &ash::Device,
           family: u32, path: &Path, expected_value: fn(u32) -> u32, wave: Option<u32>) -> crate::Result<()> {
    if let Some(size) = wave {
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut features13);
        unsafe { instance.get_physical_device_features2(physical, &mut features) };
        let mut sizes = vk::PhysicalDeviceSubgroupSizeControlProperties::default();
        let mut subgroup = vk::PhysicalDeviceSubgroupProperties::default();
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut sizes).push_next(&mut subgroup);
        unsafe { instance.get_physical_device_properties2(physical, &mut props) };
        if !matches!(size, 32 | 64)
            || features13.subgroup_size_control == 0 || features13.compute_full_subgroups == 0
            || size < sizes.min_subgroup_size || size > sizes.max_subgroup_size
            || !sizes.required_subgroup_size_stages.contains(vk::ShaderStageFlags::COMPUTE)
            || !subgroup.supported_stages.contains(vk::ShaderStageFlags::COMPUTE)
            || !subgroup.supported_operations.contains(vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::ARITHMETIC)
            || sizes.max_compute_workgroup_subgroups < 64 / size
        {
            return Err(format!("Required full compute subgroup size {size} unsupported").into());
        }
        println!("required compute subgroup size: {size}");
    }
    let mode = std::env::var("AMD_VK_MEMORY").unwrap_or_else(|_| "host".to_owned());
    if !matches!(mode.as_str(), "host" | "bar" | "local") {
        return Err("AMD_VK_MEMORY must be host, bar or local".into());
    }
    let staged = mode == "local";
    println!("memory mode: {mode}");
    let words = ash::util::read_spv(&mut Cursor::new(std::fs::read(path)?))?;
    let mut r = Resources {
        device: d, buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(),
        readback: vk::Buffer::null(), readback_memory: vk::DeviceMemory::null(),
        mapped_memory: vk::DeviceMemory::null(), layout: vk::DescriptorSetLayout::null(),
        pool: vk::DescriptorPool::null(), pipeline_layout: vk::PipelineLayout::null(),
        shader: vk::ShaderModule::null(), pipeline: vk::Pipeline::null(),
        commands: vk::CommandPool::null(), timeline: vk::Semaphore::null(),
        queries: vk::QueryPool::null(),
    };

    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let props = unsafe { instance.get_physical_device_properties(physical) };
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
    let bits = families[family as usize].timestamp_valid_bits;
    let queue = unsafe { d.get_device_queue(family, 0) };

    unsafe {
        r.buffer = d.create_buffer(&vk::BufferCreateInfo::default()
            .size(BYTES).usage(vk::BufferUsageFlags::STORAGE_BUFFER | if staged { vk::BufferUsageFlags::TRANSFER_SRC } else { vk::BufferUsageFlags::empty() })
            .sharing_mode(vk::SharingMode::EXCLUSIVE), None)?;
        let req = d.get_buffer_memory_requirements(r.buffer);
        let index = memory_type(&mem, req.memory_type_bits, &mode)
            .ok_or_else(|| format!("No compatible memory for mode {mode}"))?;
        println!("compute memory type: {index}; {:?}", mem.memory_types[index as usize].property_flags);
        r.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
            .allocation_size(req.size).memory_type_index(index), None)?;
        d.bind_buffer_memory(r.buffer, r.memory, 0)?;
        if staged {
            r.readback = d.create_buffer(&vk::BufferCreateInfo::default()
                .size(BYTES).usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE), None)?;
            let req = d.get_buffer_memory_requirements(r.readback);
            let index = memory_type(&mem, req.memory_type_bits, "host")
                .ok_or("No host-coherent staging memory")?;
            r.readback_memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
                .allocation_size(req.size).memory_type_index(index), None)?;
            d.bind_buffer_memory(r.readback, r.readback_memory, 0)?;
            println!("readback memory type: {index}");
        }

        let bindings = [vk::DescriptorSetLayoutBinding::default().binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)];
        r.layout = d.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 1 }];
        r.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None)?;
        let layouts = [r.layout];
        let set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(r.pool).set_layouts(&layouts))?[0];
        let buffers = [vk::DescriptorBufferInfo::default().buffer(r.buffer).offset(0).range(BYTES)];
        d.update_descriptor_sets(&[vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&buffers)], &[]);
        r.pipeline_layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts), None)?;
        r.shader = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?;
        let mut required_size = vk::PipelineShaderStageRequiredSubgroupSizeCreateInfo::default().required_subgroup_size(wave.unwrap_or(32));
        let mut stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE).module(r.shader)
            .name(CStr::from_bytes_with_nul(b"main\0")?);
        if wave.is_some() {
            stage = stage.flags(vk::PipelineShaderStageCreateFlags::REQUIRE_FULL_SUBGROUPS).push_next(&mut required_size);
        }
        let info = vk::ComputePipelineCreateInfo::default().stage(stage).layout(r.pipeline_layout);
        let cache = crate::pipeline_cache::Cache::open(d, &props);
        let pipeline_start = Instant::now();
        r.pipeline = match d.create_compute_pipelines(cache.handle(), &[info], None) {
            Ok(pipelines) => pipelines[0],
            Err((pipelines, err)) => {
                for p in pipelines { d.destroy_pipeline(p, None); }
                return Err(err.into());
            }
        };

        println!("compute pipeline creation: {:.3} ms", pipeline_start.elapsed().as_secs_f64() * 1000.);
        cache.save();
        r.commands = d.create_command_pool(&vk::CommandPoolCreateInfo::default()
            .queue_family_index(family), None)?;
        let cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(r.commands).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
        r.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty), None)?;
        if bits > 0 {
            r.queries = d.create_query_pool(&vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP).query_count(2), None)?;
        }
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        if bits > 0 {
            d.cmd_reset_query_pool(cmd, r.queries, 0, 2);
            d.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, r.queries, 0);
        }
        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, r.pipeline);
        d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, r.pipeline_layout, 0, &[set], &[]);
        d.cmd_dispatch(cmd, (WORDS / 64) as u32, 1, 1);
        if bits > 0 {
            d.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, r.queries, 1);
        }
        if staged {
            let transfer = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
            d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&transfer));
            d.cmd_copy_buffer(cmd, r.buffer, r.readback, &[vk::BufferCopy::default().size(BYTES)]);
        }
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(if staged { vk::PipelineStageFlags2::COPY } else { vk::PipelineStageFlags2::COMPUTE_SHADER })
            .src_access_mask(if staged { vk::AccessFlags2::TRANSFER_WRITE } else { vk::AccessFlags2::SHADER_WRITE })
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&barrier));
        d.end_command_buffer(cmd)?;

        let command = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let signal = [vk::SemaphoreSubmitInfo::default().semaphore(r.timeline).value(1)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&command).signal_semaphore_infos(&signal)];
        let start = Instant::now();
        d.queue_submit2(queue, &submits, vk::Fence::null())?;
        d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[r.timeline]).values(&[1]), u64::MAX)?;
        println!("submit + wait: {:.3} ms", start.elapsed().as_secs_f64() * 1000.0);

        if bits > 0 {
            let mut timestamps = [0u64; 2];
            d.get_query_pool_results(r.queries, 0, &mut timestamps, vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT)?;
            let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
            let ticks = timestamps[1].wrapping_sub(timestamps[0]) & mask;
            println!("GPU dispatch timestamp interval: {:.3} ms", ticks as f64 * props.limits.timestamp_period as f64 / 1e6);
        }

        let read_start = Instant::now();
        let readable = if staged { r.readback_memory } else { r.memory };
        let ptr = d.map_memory(readable, 0, BYTES, vk::MemoryMapFlags::empty())?;
        r.mapped_memory = readable;
        let output = std::slice::from_raw_parts(ptr.cast::<u32>(), WORDS);
        for (i, &actual) in output.iter().enumerate() {
            let expected = expected_value(i as u32);
            if actual != expected {
                return Err(format!("mismatch at {i}: {actual} != {expected}").into());
            }
        }
        println!("CPU map + verify: {:.3} ms", read_start.elapsed().as_secs_f64() * 1000.0);
        println!("PASS: verified {WORDS} SPIR-V compute outputs ({BYTES} bytes)");
    }
    Ok(())
}

fn memory_type(mem: &vk::PhysicalDeviceMemoryProperties, bits: u32, mode: &str) -> Option<u32> {
    use vk::MemoryPropertyFlags as F;
    let host = F::HOST_VISIBLE | F::HOST_COHERENT;
    let excluded = F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD;
    let required = match mode {
        "host" => host,
        "bar" => host | F::DEVICE_LOCAL,
        "local" => F::DEVICE_LOCAL,
        _ => return None,
    };
    (0..mem.memory_type_count)
        .filter(|&i| (bits & (1u32 << i)) != 0)
        .filter(|&i| {
            let f = mem.memory_types[i as usize].property_flags;
            f.contains(required) && !f.intersects(excluded)
                && (mode != "host" || !f.contains(F::DEVICE_LOCAL))
                && (mode != "local" || !f.contains(F::HOST_VISIBLE))
        })
        .max_by_key(|&i| mem.memory_types[i as usize].property_flags.contains(F::HOST_CACHED))
}
