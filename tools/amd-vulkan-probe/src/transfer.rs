// SPDX-License-Identifier: GPL-3.0-or-later
//! Original Vulkan transfer regression. Full readback is for verification only.
use ash::vk;
use std::time::Instant;
use crate::dirty_upload::{DirtyUpload, PAGE_BYTES};

const BYTES: usize = 16 * 1024 * 1024;

struct Buffer<'a> {
    d: &'a ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}
impl Drop for Buffer<'_> {
    fn drop(&mut self) {
        unsafe {
            self.d.destroy_buffer(self.buffer, None);
            self.d.free_memory(self.memory, None);
        }
    }
}
impl<'a> Buffer<'a> {
    fn new(d: &'a ash::Device, mem: &vk::PhysicalDeviceMemoryProperties,
           usage: vk::BufferUsageFlags, host: bool) -> crate::Result<Self> {
        use vk::MemoryPropertyFlags as F;
        let mut b = Self { d, buffer: vk::Buffer::null(), memory: vk::DeviceMemory::null() };
        unsafe {
            b.buffer = d.create_buffer(&vk::BufferCreateInfo::default().size(BYTES as u64)
                .usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE), None)?;
            let req = d.get_buffer_memory_requirements(b.buffer);
            let required = if host { F::HOST_VISIBLE | F::HOST_COHERENT } else { F::DEVICE_LOCAL };
            let index = (0..mem.memory_type_count)
                .filter(|&i| req.memory_type_bits & (1u32 << i) != 0)
                .filter(|&i| {
                    let f = mem.memory_types[i as usize].property_flags;
                    f.contains(required)
                        && !f.intersects(F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD)
                        && if host { !f.contains(F::DEVICE_LOCAL) } else { !f.contains(F::HOST_VISIBLE) }
                })
                .max_by_key(|&i| host && mem.memory_types[i as usize].property_flags.contains(F::HOST_CACHED))
                .ok_or("No compatible transfer memory")?;
            b.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
                .allocation_size(req.size).memory_type_index(index), None)?;
            d.bind_buffer_memory(b.buffer, b.memory, 0)?;
            println!("transfer memory type: {index}; {:?}", mem.memory_types[index as usize].property_flags);
        }
        Ok(b)
    }
}

struct Resources<'a> {
    d: &'a ash::Device,
    upload: Buffer<'a>,
    local: Buffer<'a>,
    readback: Buffer<'a>,
    pool: vk::CommandPool,
    timeline: vk::Semaphore,
}
impl Drop for Resources<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.d.device_wait_idle();
            self.d.destroy_command_pool(self.pool, None);
            self.d.destroy_semaphore(self.timeline, None);
        }
    }
}

fn plan_error(e: crate::dirty_upload::UploadError) -> Box<dyn std::error::Error> {
    format!("dirty upload planning: {e:?}").into()
}

fn round(r: &Resources<'_>, queue: vk::Queue, cmd: vk::CommandBuffer,
         timeline: u64, tracker: &mut DirtyUpload, cpu: &mut [u8],
         change_during_submit: Option<(usize,u8)>) -> crate::Result<usize> {
    let ranges = tracker.begin().map_err(plan_error)?.to_vec();
    let bytes: usize = ranges.iter().map(|x| x.len()).sum();
    if ranges.is_empty() {
        tracker.finish(true).map_err(plan_error)?;
        return Ok(0);
    }
    let expected = cpu.to_vec();
    let copies: Vec<_> = ranges.iter().map(|x| vk::BufferCopy::default()
        .src_offset(x.start as u64).dst_offset(x.start as u64).size(x.len() as u64)).collect();
    let d = r.d;
    unsafe {
        let ptr = d.map_memory(r.upload.memory, 0, BYTES as u64, vk::MemoryMapFlags::empty())?.cast::<u8>();
        for x in &ranges {
            std::ptr::copy_nonoverlapping(cpu.as_ptr().add(x.start), ptr.add(x.start), x.len());
        }
        d.unmap_memory(r.upload.memory);
        d.reset_command_pool(r.pool, vk::CommandPoolResetFlags::empty())?;
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        // Serialize prior readback against subsequent writes to the local buffer.
        let before = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&before));
        d.cmd_copy_buffer(cmd, r.upload.buffer, r.local.buffer, &copies);
        let after = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY).src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY).dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&after));
        d.cmd_copy_buffer(cmd, r.local.buffer, r.readback.buffer,
            &[vk::BufferCopy::default().size(BYTES as u64)]);
        let host = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY).src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST).dst_access_mask(vk::AccessFlags2::HOST_READ)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&host));
        d.end_command_buffer(cmd)?;
        let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let signals = [vk::SemaphoreSubmitInfo::default().semaphore(r.timeline).value(timeline)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&commands).signal_semaphore_infos(&signals)];
        let start = Instant::now();
        d.queue_submit2(queue, &submits, vk::Fence::null())?;
        // This changes CPU shadow only, never staging still used by the GPU.
        if let Some((offset,value)) = change_during_submit {
            cpu[offset] = value;
            tracker.mark(offset, 1).map_err(plan_error)?;
        }
        d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[r.timeline])
            .values(&[timeline]), u64::MAX)?;
        println!("upload {timeline}: {bytes} bytes, {} regions; submit + readback + wait {:.3} ms",
            copies.len(), start.elapsed().as_secs_f64()*1000.);
        tracker.finish(true).map_err(plan_error)?;
        let ptr = d.map_memory(r.readback.memory,0,BYTES as u64,vk::MemoryMapFlags::empty())?;
        let mismatch = std::slice::from_raw_parts(ptr.cast::<u8>(),BYTES)
            .iter().zip(&expected).position(|(a,b)| a != b);
        d.unmap_memory(r.readback.memory);
        if let Some(i) = mismatch { return Err(format!("VRAM mismatch at byte {i} in upload {timeline}").into()); }
        println!("PASS: verified all {BYTES} bytes after upload {timeline}");
    }
    Ok(bytes)
}

pub fn run(instance: &ash::Instance, physical: vk::PhysicalDevice,
           d: &ash::Device, family: u32) -> crate::Result<()> {
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let mut r = Resources {
        d,
        upload: Buffer::new(d,&mem,vk::BufferUsageFlags::TRANSFER_SRC,true)?,
        local: Buffer::new(d,&mem,vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC,false)?,
        readback: Buffer::new(d,&mem,vk::BufferUsageFlags::TRANSFER_DST,true)?,
        pool: vk::CommandPool::null(), timeline: vk::Semaphore::null(),
    };
    let (queue,cmd) = unsafe {
        r.pool = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family),None)?;
        let cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(r.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
        r.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)?;
        (d.get_device_queue(family,0),cmd)
    };
    let mut cpu: Vec<u8> = (0..BYTES).map(|i| ((i*13) ^ (i>>11)) as u8).collect();
    let mut tracker = DirtyUpload::new(BYTES).map_err(plan_error)?;
    if round(&r,queue,cmd,1,&mut tracker,&mut cpu,None)? != BYTES {
        return Err("initial upload was not complete".into());
    }
    let p = PAGE_BYTES;
    for i in [p-1,p,128*p+9] {
        cpu[i] ^= 0xff;
        tracker.mark(i,1).map_err(plan_error)?;
    }
    let late = p+17;
    let late_value = cpu[late] ^ 0xff;
    if round(&r,queue,cmd,2,&mut tracker,&mut cpu,Some((late,late_value)))? != 3*p {
        return Err("sparse upload was not three pages".into());
    }
    if round(&r,queue,cmd,3,&mut tracker,&mut cpu,None)? != p {
        return Err("late CPU write was not retained as one dirty page".into());
    }
    if round(&r,queue,cmd,4,&mut tracker,&mut cpu,None)? != 0 {
        return Err("clean buffer incorrectly planned a copy".into());
    }
    println!("PASS: full, sparse, late-write and zero-copy rounds verified on Vulkan");
    Ok(())
}


// Independent command pools allow simultaneous CPU recording. Queue
// submission remains externally synchronized on the calling thread.
const WORKERS: usize = 4;
const CHUNK: usize = 4096;
const FILLS: usize = BYTES / CHUNK;

struct ParallelResources<'a> {
    d: &'a ash::Device,
    local: Buffer<'a>,
    readback: Buffer<'a>,
    pools: Vec<vk::CommandPool>,
    timeline: vk::Semaphore,
}
impl Drop for ParallelResources<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.d.device_wait_idle();
            for pool in self.pools.drain(..) { self.d.destroy_command_pool(pool,None); }
            self.d.destroy_semaphore(self.timeline,None);
        }
    }
}

fn fill_value(index: usize, epoch: u64) -> u32 {
    (index as u32).wrapping_mul(0x9e3779b9) ^ (epoch as u32).wrapping_mul(0x85ebca6b)
}

fn record_worker(d: &ash::Device, pool: vk::CommandPool, cmd: vk::CommandBuffer,
                 buffer: vk::Buffer, worker: usize, epoch: u64) -> Result<(),vk::Result> {
    let first = worker * (FILLS / WORKERS);
    let end = first + FILLS / WORKERS;
    // SAFETY: one worker exclusively owns this pool and command buffer;
    // the prior submission has completed. Each writes a disjoint range.
    unsafe {
        d.reset_command_pool(pool,vk::CommandPoolResetFlags::empty())?;
        d.begin_command_buffer(cmd,&vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        for index in first..end {
            d.cmd_fill_buffer(cmd,buffer,(index * CHUNK) as u64,CHUNK as u64,fill_value(index,epoch));
        }
        d.end_command_buffer(cmd)?;
    }
    Ok(())
}

/// Compare identical serial and parallel recording workloads. All submitted
/// bytes are verified, including unchanged boundaries between worker regions.
pub fn run_parallel(instance: &ash::Instance, physical: vk::PhysicalDevice,
                    d: &ash::Device, family: u32) -> crate::Result<()> {
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let mut r = ParallelResources {
        d,
        local: Buffer::new(d,&mem,vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC,false)?,
        readback: Buffer::new(d,&mem,vk::BufferUsageFlags::TRANSFER_DST,true)?,
        pools: Vec::new(), timeline: vk::Semaphore::null(),
    };
    let mut commands = Vec::new();
    let queue = unsafe {
        for _ in 0..=WORKERS {
            let pool = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family),None)?;
            r.pools.push(pool);
            commands.push(d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                .command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0]);
        }
        let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
        r.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)?;
        d.get_device_queue(family,0)
    };
    let mut serial = Vec::new();
    let mut parallel = Vec::new();
    for epoch in 1u64..=8 {
        let is_parallel = epoch % 2 == 0;
        let start = Instant::now();
        let buffer = r.local.buffer;
        if is_parallel {
            std::thread::scope(|scope| -> crate::Result<()> {
                let mut jobs = Vec::new();
                for worker in 0..WORKERS {
                    let pool = r.pools[worker];
                    let cmd = commands[worker];
                    jobs.push(scope.spawn(move || record_worker(d,pool,cmd,buffer,worker,epoch)));
                }
                let mut failure = None;
                for job in jobs {
                    match job.join() {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => { failure = Some(format!("worker recording failed: {e:?}")); }
                        Err(_) => { failure = Some("worker panicked".to_owned()); }
                    }
                }
                if let Some(e) = failure { return Err(e.into()); }
                Ok(())
            })?;
        } else {
            for worker in 0..WORKERS {
                record_worker(d,r.pools[worker],commands[worker],buffer,worker,epoch)?;
            }
        }
        let record_ms = start.elapsed().as_secs_f64()*1000.;
        unsafe {
            let tail = commands[WORKERS];
            d.reset_command_pool(r.pools[WORKERS],vk::CommandPoolResetFlags::empty())?;
            d.begin_command_buffer(tail,&vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
            // ALL_TRANSFER includes vkCmdFillBuffer (CLEAR stage), not just COPY.
            let ready = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
            d.cmd_pipeline_barrier2(tail,&vk::DependencyInfo::default().memory_barriers(&ready));
            d.cmd_copy_buffer(tail,r.local.buffer,r.readback.buffer,&[vk::BufferCopy::default().size(BYTES as u64)]);
            let host = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY).src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST).dst_access_mask(vk::AccessFlags2::HOST_READ)];
            d.cmd_pipeline_barrier2(tail,&vk::DependencyInfo::default().memory_barriers(&host));
            d.end_command_buffer(tail)?;
            let submit_commands: Vec<_> = commands.iter().map(|&cmd|
                vk::CommandBufferSubmitInfo::default().command_buffer(cmd)).collect();
            let wait = [vk::SemaphoreSubmitInfo::default().semaphore(r.timeline).value(epoch-1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signal = [vk::SemaphoreSubmitInfo::default().semaphore(r.timeline).value(epoch)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&submit_commands)
                .wait_semaphore_infos(&wait).signal_semaphore_infos(&signal)];
            let start = Instant::now();
            d.queue_submit2(queue,&submit,vk::Fence::null())?;
            d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[r.timeline])
                .values(&[epoch]),u64::MAX)?;
            let submit_ms = start.elapsed().as_secs_f64()*1000.;
            let ptr = d.map_memory(r.readback.memory,0,BYTES as u64,vk::MemoryMapFlags::empty())?;
            let actual = std::slice::from_raw_parts(ptr.cast::<u32>(),BYTES/4);
            let mismatch = actual.iter().enumerate().position(|(i,&v)| v != fill_value(i/(CHUNK/4),epoch));
            d.unmap_memory(r.readback.memory);
            if let Some(i) = mismatch { return Err(format!("parallel regression mismatch at word {i}, epoch {epoch}").into()); }
            let mode = if is_parallel { "parallel" } else { "serial" };
            println!("{mode} round {epoch}: {FILLS} fills, {WORKERS} command buffers; record {record_ms:.3} ms; submit+readback+wait {submit_ms:.3} ms");
            println!("PASS: verified all {BYTES} bytes");
        }
        // First round of each mode is a warm-up, not included in the median.
        if epoch > 2 {
            if is_parallel { parallel.push(record_ms); } else { serial.push(record_ms); }
        }
    }
    serial.sort_by(f64::total_cmp);
    parallel.sort_by(f64::total_cmp);
    println!("median serial recording: {:.3} ms; parallel recording: {:.3} ms",serial[1],parallel[1]);
    println!("PASS: parallel Vulkan recording and ordered submission verified");
    Ok(())
}
