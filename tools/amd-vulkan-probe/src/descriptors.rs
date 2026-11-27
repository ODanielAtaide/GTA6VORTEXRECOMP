// SPDX-License-Identifier: GPL-3.0-or-later
//! Real descriptor updates without update-after-bind or in-flight set mutation.
use ash::vk;
use std::{ffi::CStr, io::Cursor, path::Path, time::Instant};
use crate::descriptor_journal::{BufferBinding, Journal, JournalError};

struct Resources<'a> {
    d: &'a ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    pipeline_layout: vk::PipelineLayout,
    shader: vk::ShaderModule,
    pipeline: vk::Pipeline,
    commands: vk::CommandPool,
    timeline: vk::Semaphore,
}
impl Drop for Resources<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.d.device_wait_idle();
            self.d.destroy_command_pool(self.commands,None);
            self.d.destroy_semaphore(self.timeline,None);
            self.d.destroy_pipeline(self.pipeline,None);
            self.d.destroy_shader_module(self.shader,None);
            self.d.destroy_pipeline_layout(self.pipeline_layout,None);
            self.d.destroy_descriptor_pool(self.pool,None);
            self.d.destroy_descriptor_set_layout(self.layout,None);
            self.d.destroy_buffer(self.buffer,None);
            self.d.free_memory(self.memory,None);
        }
    }
}

fn plan_error(e: JournalError) -> Box<dyn std::error::Error> {
    format!("descriptor journal: {e:?}").into()
}

fn publish(r: &Resources<'_>, set: vk::DescriptorSet, j: &mut Journal) -> crate::Result<usize> {
    let batch = j.begin().map_err(plan_error)?.to_vec();
    if batch.iter().any(|w| w.binding.resource != 1 || w.binding.generation != 1) {
        j.finish(false).map_err(plan_error)?;
        return Err("unknown resource generation in fixture".into());
    }
    let infos: Vec<_> = batch.iter().map(|w| vk::DescriptorBufferInfo::default()
        .buffer(r.buffer).offset(w.binding.offset).range(w.binding.range)).collect();
    let writes: Vec<_> = batch.iter().zip(&infos).map(|(w,info)|
        vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0)
        .dst_array_element(w.slot as u32).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(std::slice::from_ref(info))).collect();
    let start = Instant::now();
    if !writes.is_empty() {
        // All prior commands using this set have completed before this call.
        unsafe { r.d.update_descriptor_sets(&writes,&[]); }
    }
    println!("descriptor publication: {} writes; {:.3} ms",writes.len(),start.elapsed().as_secs_f64()*1000.);
    j.finish(true).map_err(plan_error)?;
    Ok(writes.len())
}

fn execute(r: &Resources<'_>, set: vk::DescriptorSet, queue: vk::Queue,
           cmd: vk::CommandBuffer, epoch: u64, bytes: u64, output_offset: u64,
           expected: [u32;4]) -> crate::Result<()> {
    let d = r.d;
    unsafe {
        d.reset_command_pool(r.commands,vk::CommandPoolResetFlags::empty())?;
        d.begin_command_buffer(cmd,&vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        d.cmd_bind_pipeline(cmd,vk::PipelineBindPoint::COMPUTE,r.pipeline);
        d.cmd_bind_descriptor_sets(cmd,vk::PipelineBindPoint::COMPUTE,r.pipeline_layout,0,&[set],&[]);
        d.cmd_dispatch(cmd,1,1,1);
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER).src_access_mask(vk::AccessFlags2::SHADER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST).dst_access_mask(vk::AccessFlags2::HOST_READ)];
        d.cmd_pipeline_barrier2(cmd,&vk::DependencyInfo::default().memory_barriers(&barrier));
        d.end_command_buffer(cmd)?;
        let command = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let signal = [vk::SemaphoreSubmitInfo::default().semaphore(r.timeline).value(epoch)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&command).signal_semaphore_infos(&signal)];
        d.queue_submit2(queue,&submits,vk::Fence::null())?;
        d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[r.timeline]).values(&[epoch]),u64::MAX)?;
        let ptr = d.map_memory(r.memory,0,bytes,vk::MemoryMapFlags::empty())?.cast::<u8>();
        let mut actual = [0u32;4];
        std::ptr::copy_nonoverlapping(ptr.add(output_offset as usize).cast::<u32>(),actual.as_mut_ptr(),4);
        d.unmap_memory(r.memory);
        if actual != expected { return Err(format!("descriptor round {epoch}: {actual:?} != {expected:?}").into()); }
        println!("PASS: descriptor round {epoch}: {actual:?}");
    }
    Ok(())
}

pub fn run(instance: &ash::Instance, physical: vk::PhysicalDevice,
           d: &ash::Device, family: u32, path: &Path) -> crate::Result<()> {
    let props = unsafe { instance.get_physical_device_properties(physical) };
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let stride = props.limits.min_storage_buffer_offset_alignment.max(16);
    let output_offset = 8*stride;
    let bytes = output_offset + 16;
    if props.limits.max_per_stage_descriptor_storage_buffers < 5
        || props.limits.max_descriptor_set_storage_buffers < 5 {
        return Err("Five storage buffer descriptors required".into());
    }
    let words = ash::util::read_spv(&mut Cursor::new(std::fs::read(path)?))?;
    let mut r = Resources { d, buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(),
        layout: vk::DescriptorSetLayout::null(),pool: vk::DescriptorPool::null(),
        pipeline_layout: vk::PipelineLayout::null(),shader: vk::ShaderModule::null(),
        pipeline: vk::Pipeline::null(),commands: vk::CommandPool::null(),timeline: vk::Semaphore::null() };
    let (set,queue,cmd) = unsafe {
        r.buffer = d.create_buffer(&vk::BufferCreateInfo::default().size(bytes)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER).sharing_mode(vk::SharingMode::EXCLUSIVE),None)?;
        let req = d.get_buffer_memory_requirements(r.buffer);
        use vk::MemoryPropertyFlags as F;
        let index = (0..mem.memory_type_count).filter(|&i| req.memory_type_bits & (1u32<<i) != 0)
            .filter(|&i| {
                let f = mem.memory_types[i as usize].property_flags;
                f.contains(F::HOST_VISIBLE | F::HOST_COHERENT)
                    && !f.intersects(F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD)
            }).max_by_key(|&i| mem.memory_types[i as usize].property_flags.contains(F::HOST_CACHED))
            .ok_or("No host-coherent memory")?;
        r.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size)
            .memory_type_index(index),None)?;
        d.bind_buffer_memory(r.buffer,r.memory,0)?;
        let ptr = d.map_memory(r.memory,0,bytes,vk::MemoryMapFlags::empty())?.cast::<u8>();
        std::ptr::write_bytes(ptr,0,bytes as usize);
        for (i,v) in [11u32,22,33,44,55,66,77,88].into_iter().enumerate() {
            std::ptr::write(ptr.add(i*stride as usize).cast::<u32>(),v);
        }
        d.unmap_memory(r.memory);
        let bindings = [
            vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(4).stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        r.layout = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),None)?;
        let sizes = [vk::DescriptorPoolSize { ty:vk::DescriptorType::STORAGE_BUFFER,descriptor_count:5 }];
        r.pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),None)?;
        let layouts = [r.layout];
        let set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(r.pool).set_layouts(&layouts))?[0];
        let out = [vk::DescriptorBufferInfo::default().buffer(r.buffer).offset(output_offset).range(16)];
        d.update_descriptor_sets(&[vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&out)],&[]);
        r.pipeline_layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts),None)?;
        r.shader = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words),None)?;
        let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE)
            .module(r.shader).name(CStr::from_bytes_with_nul(b"main\0")?);
        let info = vk::ComputePipelineCreateInfo::default().stage(stage).layout(r.pipeline_layout);
        r.pipeline = match d.create_compute_pipelines(vk::PipelineCache::null(),&[info],None) {
            Ok(p) => p[0],
            Err((p,e)) => { for pipeline in p { d.destroy_pipeline(pipeline,None); } return Err(e.into()); }
        };
        r.commands = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family),None)?;
        let cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(r.commands).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
        r.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)?;
        (set,d.get_device_queue(family,0),cmd)
    };
    let binding = |i: u64| BufferBinding { resource:1,generation:1,offset:i*stride,range:4 };
    let mut journal = Journal::new(4);
    for i in 0..4 { journal.set(i,binding(i as u64)).map_err(plan_error)?; }
    if publish(&r,set,&mut journal)? != 4 { return Err("expected four initial writes".into()); }
    execute(&r,set,queue,cmd,1,bytes,output_offset,[11,22,33,44])?;
    journal.set(1,binding(4)).map_err(plan_error)?;
    journal.set(3,binding(6)).map_err(plan_error)?;
    journal.set(0,binding(0)).map_err(plan_error)?;
    journal.set(2,binding(5)).map_err(plan_error)?;
    journal.set(2,binding(2)).map_err(plan_error)?;
    if publish(&r,set,&mut journal)? != 2 { return Err("expected two changed descriptors".into()); }
    execute(&r,set,queue,cmd,2,bytes,output_offset,[11,55,33,77])?;
    if publish(&r,set,&mut journal)? != 0 { return Err("unexpected descriptor writes".into()); }
    execute(&r,set,queue,cmd,3,bytes,output_offset,[11,55,33,77])?;
    println!("PASS: selective Vulkan descriptors, unchanged slots and zero-update round verified");
    Ok(())
}

/// Verify a partially bound runtime descriptor array with non-uniform indices.
pub fn run_bindless(instance: &ash::Instance, physical: vk::PhysicalDevice,
           d: &ash::Device, family: u32, path: &Path) -> crate::Result<()> {
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut f12);
    unsafe { instance.get_physical_device_features2(physical,&mut features); }
    if f12.descriptor_indexing == 0 || f12.runtime_descriptor_array == 0
        || f12.shader_storage_buffer_array_non_uniform_indexing == 0
        || f12.descriptor_binding_partially_bound == 0 {
        return Err("Required bindless storage buffer features unavailable".into());
    }
    const SLOTS: u32 = 64;
    let props = unsafe { instance.get_physical_device_properties(physical) };
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let stride = props.limits.min_storage_buffer_offset_alignment.max(16);
    let output_offset = 8*stride;
    let bytes = output_offset + 16;
    if props.limits.max_per_stage_descriptor_storage_buffers < SLOTS+1
        || props.limits.max_descriptor_set_storage_buffers < SLOTS+1
        || props.limits.max_per_stage_resources < SLOTS+1 {
        return Err("65 storage buffer descriptors required".into());
    }
    let words = ash::util::read_spv(&mut Cursor::new(std::fs::read(path)?))?;
    let mut r = Resources { d, buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null(),
        layout: vk::DescriptorSetLayout::null(),pool: vk::DescriptorPool::null(),
        pipeline_layout: vk::PipelineLayout::null(),shader: vk::ShaderModule::null(),
        pipeline: vk::Pipeline::null(),commands: vk::CommandPool::null(),timeline: vk::Semaphore::null() };
    let (set,queue,cmd) = unsafe {
        r.buffer = d.create_buffer(&vk::BufferCreateInfo::default().size(bytes)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER).sharing_mode(vk::SharingMode::EXCLUSIVE),None)?;
        let req = d.get_buffer_memory_requirements(r.buffer);
        use vk::MemoryPropertyFlags as F;
        let index = (0..mem.memory_type_count).filter(|&i| req.memory_type_bits & (1u32<<i) != 0)
            .filter(|&i| {
                let f = mem.memory_types[i as usize].property_flags;
                f.contains(F::HOST_VISIBLE | F::HOST_COHERENT)
                    && !f.intersects(F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD)
            }).max_by_key(|&i| mem.memory_types[i as usize].property_flags.contains(F::HOST_CACHED))
            .ok_or("No host-coherent memory")?;
        r.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size)
            .memory_type_index(index),None)?;
        d.bind_buffer_memory(r.buffer,r.memory,0)?;
        let ptr = d.map_memory(r.memory,0,bytes,vk::MemoryMapFlags::empty())?.cast::<u8>();
        std::ptr::write_bytes(ptr,0,bytes as usize);
        for (i,v) in [11u32,22,33,44,55,66,77,88].into_iter().enumerate() {
            std::ptr::write(ptr.add(i*stride as usize).cast::<u32>(),v);
        }
        d.unmap_memory(r.memory);
        let bindings = [
            vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(SLOTS).stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        let flags = [vk::DescriptorBindingFlags::PARTIALLY_BOUND,vk::DescriptorBindingFlags::empty()];
        let mut binding_flags = vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&flags);
        r.layout = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default()
            .bindings(&bindings).push_next(&mut binding_flags),None)?;
        let sizes = [vk::DescriptorPoolSize { ty:vk::DescriptorType::STORAGE_BUFFER,descriptor_count:SLOTS+1 }];
        r.pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),None)?;
        let layouts = [r.layout];
        let set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(r.pool).set_layouts(&layouts))?[0];
        let out = [vk::DescriptorBufferInfo::default().buffer(r.buffer).offset(output_offset).range(16)];
        d.update_descriptor_sets(&[vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&out)],&[]);
        r.pipeline_layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts),None)?;
        r.shader = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words),None)?;
        let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE)
            .module(r.shader).name(CStr::from_bytes_with_nul(b"main\0")?);
        let info = vk::ComputePipelineCreateInfo::default().stage(stage).layout(r.pipeline_layout);
        r.pipeline = match d.create_compute_pipelines(vk::PipelineCache::null(),&[info],None) {
            Ok(p) => p[0],
            Err((p,e)) => { for pipeline in p { d.destroy_pipeline(pipeline,None); } return Err(e.into()); }
        };
        r.commands = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family),None)?;
        let cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(r.commands).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
        r.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)?;
        (set,d.get_device_queue(family,0),cmd)
    };
    let binding = |i: u64| BufferBinding { resource:1,generation:1,offset:i*stride,range:4 };
    let mut journal = Journal::new(SLOTS as usize);
    // Only dynamically accessed descriptors are populated. All other
    // elements deliberately remain unbound, not null descriptors.
    for (i,slot) in [1usize,9,33,63].into_iter().enumerate() {
        journal.set(slot,binding(i as u64)).map_err(plan_error)?;
    }
    println!("bindless array: {SLOTS} slots, 4 populated; non-uniform indexing");
    if publish(&r,set,&mut journal)? != 4 { return Err("expected four initial writes".into()); }
    execute(&r,set,queue,cmd,1,bytes,output_offset,[11,22,33,44])?;
    journal.set(9,binding(4)).map_err(plan_error)?;
    journal.set(63,binding(6)).map_err(plan_error)?;
    journal.set(1,binding(0)).map_err(plan_error)?;
    journal.set(33,binding(5)).map_err(plan_error)?;
    journal.set(33,binding(2)).map_err(plan_error)?;
    if publish(&r,set,&mut journal)? != 2 { return Err("expected two changed descriptors".into()); }
    execute(&r,set,queue,cmd,2,bytes,output_offset,[11,55,33,77])?;
    if publish(&r,set,&mut journal)? != 0 { return Err("unexpected descriptor writes".into()); }
    execute(&r,set,queue,cmd,3,bytes,output_offset,[11,55,33,77])?;
    println!("PASS: bindless Vulkan runtime array, non-uniform selection and sparse publication verified");
    Ok(())
}
