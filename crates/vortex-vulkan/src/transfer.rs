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
    // Keep one worker per independent command pool alive across rounds.
    // Dedicated reply channels also report a disconnected worker instead
    // of waiting forever on a shared reply queue.
    std::thread::scope(|scope| -> crate::Result<()> {
    let buffer = r.local.buffer;
    let mut workers = Vec::with_capacity(WORKERS);
    for worker in 0..WORKERS {
        let pool = r.pools[worker];
        let cmd = commands[worker];
        let (send, receive) = std::sync::mpsc::channel::<u64>();
        let (reply, done) = std::sync::mpsc::channel::<Result<(),vk::Result>>();
        scope.spawn(move || {
            while let Ok(epoch) = receive.recv() {
                let result = record_worker(d,pool,cmd,buffer,worker,epoch);
                if reply.send(result).is_err() { break; }
            }
        });
        workers.push((send,done));
    }
    let mut timings: [Vec<f64>;3] = std::array::from_fn(|_| Vec::new());
    // Six rounds per mode, first round of each mode is warm-up.
    for epoch in 1u64..=18 {
        let mode = ((epoch-1) % 3) as usize;
        let start = Instant::now();
        if mode == 2 {
            for (send,_) in &workers {
                send.send(epoch).map_err(|_| "persistent worker disconnected")?;
            }
            for (_,done) in &workers {
                done.recv().map_err(|_| "persistent worker reply disconnected")??;
            }
        } else if mode == 1 {
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
            let label = ["serial","transient","persistent"][mode];
            println!("{label} round {epoch}: {FILLS} fills, {WORKERS} command buffers; record {record_ms:.3} ms; submit+readback+wait {submit_ms:.3} ms");
            println!("PASS: verified all {BYTES} bytes");
        }
        if epoch > 3 { timings[mode].push(record_ms); }
    }
    for values in &mut timings { values.sort_by(f64::total_cmp); }
    println!("median serial recording: {:.3} ms; transient recording: {:.3} ms; persistent recording: {:.3} ms",
        timings[0][2],timings[1][2],timings[2][2]);
    // Dropping job senders stops all persistent workers; scope joins them
    // before Vulkan pools or the logical device can be destroyed.
    drop(workers);
    println!("PASS: serial, transient and persistent Vulkan recording verified");
    Ok(())
    })
}


// Three independent slots bound CPU/GPU work in flight. Each owns its
// command pools and backing buffers until its completion is observed.
const ASYNC_SLOTS: usize = 3;
const ASYNC_ROUNDS: u64 = 12;

struct AsyncSlot<'a> {
    resources: ParallelResources<'a>,
    commands: Vec<vk::CommandBuffer>,
    pending: Option<u64>,
}

impl<'a> AsyncSlot<'a> {
    fn new(d: &'a ash::Device, mem: &vk::PhysicalDeviceMemoryProperties,
           family: u32) -> crate::Result<Self> {
        let mut resources = ParallelResources {
            d,
            local: Buffer::new(d,mem,vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC,false)?,
            readback: Buffer::new(d,mem,vk::BufferUsageFlags::TRANSFER_DST,true)?,
            pools: Vec::new(), timeline: vk::Semaphore::null(),
        };
        let mut commands = Vec::new();
        unsafe {
            for _ in 0..=WORKERS {
                let pool = d.create_command_pool(&vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family),None)?;
                resources.pools.push(pool);
                commands.push(d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0]);
            }
        }
        Ok(Self { resources, commands, pending: None })
    }

    fn retire(&mut self, timeline: vk::Semaphore) -> crate::Result<()> {
        let Some(epoch) = self.pending else { return Ok(()); };
        let d = self.resources.d;
        unsafe {
            d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[timeline])
                .values(&[epoch]),u64::MAX)?;
            let ptr = d.map_memory(self.resources.readback.memory,0,BYTES as u64,vk::MemoryMapFlags::empty())?;
            let actual = std::slice::from_raw_parts(ptr.cast::<u32>(),BYTES/4);
            let mismatch = actual.iter().enumerate().position(|(i,&v)| v != fill_value(i/(CHUNK/4),epoch));
            d.unmap_memory(self.resources.readback.memory);
            if let Some(i) = mismatch { return Err(format!("async mismatch at word {i}, epoch {epoch}").into()); }
        }
        self.pending = None;
        Ok(())
    }

    fn submit(&mut self, queue: vk::Queue, timeline: vk::Semaphore, epoch: u64) -> crate::Result<()> {
        if self.pending.is_some() { return Err("attempt to reuse an in-flight slot".into()); }
        let r = &self.resources;
        let d = r.d;
        let tail = self.commands[WORKERS];
        unsafe {
            d.reset_command_pool(r.pools[WORKERS],vk::CommandPoolResetFlags::empty())?;
            d.begin_command_buffer(tail,&vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
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
            let commands: Vec<_> = self.commands.iter().map(|&cmd|
                vk::CommandBufferSubmitInfo::default().command_buffer(cmd)).collect();
            // Keep GPU submissions ordered; CPU recording of other slots
            // can nevertheless proceed without waiting for this epoch.
            let wait = [vk::SemaphoreSubmitInfo::default().semaphore(timeline).value(epoch-1)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signal = [vk::SemaphoreSubmitInfo::default().semaphore(timeline).value(epoch)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&commands)
                .wait_semaphore_infos(&wait).signal_semaphore_infos(&signal)];
            d.queue_submit2(queue,&submit,vk::Fence::null())?;
        }
        self.pending = Some(epoch);
        Ok(())
    }
}

/// Measure a bounded pipeline with identical workloads at depth one and three.
/// Times include recording, submission, synchronization and full CPU verification.
pub fn run_async(instance: &ash::Instance, physical: vk::PhysicalDevice,
                 d: &ash::Device, family: u32) -> crate::Result<()> {
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let mut slots = Vec::with_capacity(ASYNC_SLOTS);
    for _ in 0..ASYNC_SLOTS { slots.push(AsyncSlot::new(d,&mem,family)?); }
    let queue = unsafe { d.get_device_queue(family,0) };
    let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
    let timeline = unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)? };
    // One existing RAII owner destroys the shared timeline only after idle.
    slots[0].resources.timeline = timeline;
    type Job = (vk::CommandPool,vk::CommandBuffer,vk::Buffer,u64);
    std::thread::scope(|scope| -> crate::Result<()> {
        let mut workers = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let (send,receive) = std::sync::mpsc::channel::<Job>();
            let (reply,done) = std::sync::mpsc::channel::<Result<(),vk::Result>>();
            scope.spawn(move || {
                while let Ok((pool,cmd,buffer,epoch)) = receive.recv() {
                    if reply.send(record_worker(d,pool,cmd,buffer,worker,epoch)).is_err() { break; }
                }
            });
            workers.push((send,done));
        }
        let mut epoch = 0u64;
        let mut measurements: [Vec<f64>;2] = std::array::from_fn(|_| Vec::new());
        // Alternate modes, exclude the first pass of each as warm-up.
        for pass in 0..8 {
            let depth = if pass % 2 == 0 { 1 } else { ASYNC_SLOTS };
            let mut max_outstanding = 0usize;
            let mut record_ms = 0.;
            let start = Instant::now();
            for n in 0..ASYNC_ROUNDS {
                let index = n as usize % depth;
                // Never reset a pool or read a slot before its timeline value.
                slots[index].retire(timeline)?;
                epoch += 1;
                let slot = &mut slots[index];
                let record_start = Instant::now();
                for (worker,(send,_)) in workers.iter().enumerate() {
                    send.send((slot.resources.pools[worker],slot.commands[worker],
                        slot.resources.local.buffer,epoch)).map_err(|_| "async worker disconnected")?;
                }
                for (_,done) in &workers {
                    done.recv().map_err(|_| "async worker reply disconnected")??;
                }
                record_ms += record_start.elapsed().as_secs_f64()*1000.;
                slot.submit(queue,timeline,epoch)?;
                let completed = unsafe { d.get_semaphore_counter_value(timeline)? };
                max_outstanding = max_outstanding.max((epoch.saturating_sub(completed)) as usize);
            }
            for slot in &mut slots { slot.retire(timeline)?; }
            let elapsed = start.elapsed().as_secs_f64()*1000.;
            let per_round = elapsed / ASYNC_ROUNDS as f64;
            println!("depth {depth}, pass {}: {ASYNC_ROUNDS} verified rounds, max outstanding {max_outstanding}, record mean {:.3} ms, total mean {per_round:.3} ms",
                pass+1,record_ms/ASYNC_ROUNDS as f64);
            if pass >= 2 { measurements[pass%2].push(per_round); }
        }
        for values in &mut measurements { values.sort_by(f64::total_cmp); }
        println!("median batch mean: depth 1 {:.3} ms; depth 3 {:.3} ms",measurements[0][1],measurements[1][1]);
        drop(workers);
        println!("PASS: bounded asynchronous Vulkan submission and slot retirement verified");
        Ok(())
    })
}
