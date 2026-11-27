// SPDX-License-Identifier: GPL-3.0-or-later
//! First reusable Vulkan command executor, separate from diagnostic fixtures.
//! Commands operate on session-local resource IDs, never guest pointers.
//! Synchronous completion is intentional until title lifetime tracking is wired.

use ash::vk;
use std::collections::BTreeMap;
use crate::{Context,Result};

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct BufferId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum MemoryKind { DeviceLocal, HostVisible }

#[derive(Debug,Clone,Copy)]
pub enum Command {
    Fill { dst: BufferId, offset: u64, bytes: u64, value: u32 },
    Copy { src: BufferId, src_offset: u64, dst: BufferId, dst_offset: u64, bytes: u64 },
}

struct Buffer<'a> {
    d: &'a ash::Device,
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
    bytes: u64,
    kind: MemoryKind,
}

impl Drop for Buffer<'_> {
    fn drop(&mut self) {
        unsafe {
            self.d.destroy_buffer(self.handle,None);
            self.d.free_memory(self.memory,None);
        }
    }
}

fn check_range(size: u64, offset: u64, bytes: u64) -> Result<()> {
    if bytes == 0 || offset % 4 != 0 || bytes % 4 != 0
        || offset.checked_add(bytes).is_none_or(|end| end > size) {
        return Err("buffer range must be nonempty, four-byte aligned and in bounds".into());
    }
    Ok(())
}

impl<'a> Buffer<'a> {
    fn new(d: &'a ash::Device, mem: &vk::PhysicalDeviceMemoryProperties,
           bytes: u64, kind: MemoryKind) -> Result<Self> {
        check_range(bytes,0,bytes)?;
        if bytes > isize::MAX as u64 { return Err("buffer too large for host addressing".into()); }
        let mut b = Self { d, handle:vk::Buffer::null(),memory:vk::DeviceMemory::null(),bytes,kind };
        unsafe {
            b.handle = d.create_buffer(&vk::BufferCreateInfo::default().size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),None)?;
            let req = d.get_buffer_memory_requirements(b.handle);
            use vk::MemoryPropertyFlags as F;
            let index = (0..mem.memory_type_count)
                .filter(|&i| req.memory_type_bits & (1u32<<i) != 0)
                .filter(|&i| {
                    let flags = mem.memory_types[i as usize].property_flags;
                    !flags.intersects(F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD)
                    && match kind {
                        MemoryKind::DeviceLocal => flags.contains(F::DEVICE_LOCAL) && !flags.contains(F::HOST_VISIBLE),
                        MemoryKind::HostVisible => flags.contains(F::HOST_VISIBLE | F::HOST_COHERENT)
                            && !flags.contains(F::DEVICE_LOCAL),
                    }
                })
                .max_by_key(|&i| mem.memory_types[i as usize].property_flags.contains(F::HOST_CACHED))
                .ok_or("No compatible buffer memory")?;
            b.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
                .allocation_size(req.size).memory_type_index(index),None)?;
            d.bind_buffer_memory(b.handle,b.memory,0)?;
        }
        Ok(b)
    }
}

/// Owns resources and a command queue while exclusively borrowing Context.
/// IDs are valid only in the session that created them. Failed submissions
/// poison the session; no resource is reused before completion is known.
pub struct Session<'a> {
    d: &'a ash::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    timeline: vk::Semaphore,
    completed: u64,
    next_id: u64,
    poisoned: bool,
    buffers: BTreeMap<BufferId,Buffer<'a>>,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.d.device_wait_idle();
            self.d.destroy_command_pool(self.pool,None);
            self.d.destroy_semaphore(self.timeline,None);
        }
        // Buffers drop after the device is idle; Context outlives the session.
    }
}

impl<'a> Session<'a> {
    pub(crate) fn new(context: &'a mut Context) -> Result<Self> {
        let d = &context.device;
        let mut session = Self {
            d, memory:unsafe { context.instance.get_physical_device_memory_properties(context.physical) },
            queue:unsafe { d.get_device_queue(context.family,0) },
            pool:vk::CommandPool::null(),command:vk::CommandBuffer::null(),
            timeline:vk::Semaphore::null(),completed:0,next_id:1,poisoned:false,buffers:BTreeMap::new(),
        };
        unsafe {
            session.pool = d.create_command_pool(&vk::CommandPoolCreateInfo::default()
                .queue_family_index(context.family),None)?;
            session.command = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                .command_pool(session.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
            let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
            session.timeline = d.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty),None)?;
        }
        Ok(session)
    }

    fn ready(&self) -> Result<()> {
        if self.poisoned { return Err("Vulkan command session poisoned".into()); }
        Ok(())
    }

    fn buffer(&self,id:BufferId) -> Result<&Buffer<'a>> {
        self.buffers.get(&id).ok_or_else(|| "unknown or retired buffer ID".into())
    }

    pub fn create_buffer(&mut self,bytes:u64,kind:MemoryKind) -> Result<BufferId> {
        self.ready()?;
        let id = BufferId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("buffer ID exhausted")?;
        let buffer = Buffer::new(self.d,&self.memory,bytes,kind)?;
        self.buffers.insert(id,buffer);
        // Initialize every byte, preventing undefined contents on readback.
        self.submit(&[Command::Fill { dst:id,offset:0,bytes,value:0 }])?;
        Ok(id)
    }

    pub fn destroy_buffer(&mut self,id:BufferId) -> Result<()> {
        self.ready()?;
        // Every successful submit has completed before this method returns.
        self.buffers.remove(&id).ok_or("unknown or retired buffer ID")?;
        Ok(())
    }

    pub fn write_words(&mut self,id:BufferId,offset:u64,words:&[u32]) -> Result<()> {
        self.ready()?;
        let b = self.buffer(id)?;
        let bytes = (words.len() as u64).checked_mul(4).ok_or("write size overflow")?;
        check_range(b.bytes,offset,bytes)?;
        if b.kind != MemoryKind::HostVisible { return Err("write requires host-visible buffer".into()); }
        unsafe {
            let ptr = self.d.map_memory(b.memory,0,b.bytes,vk::MemoryMapFlags::empty())?;
            std::ptr::copy_nonoverlapping(words.as_ptr(),ptr.cast::<u8>().add(offset as usize).cast::<u32>(),words.len());
            self.d.unmap_memory(b.memory);
        }
        Ok(())
    }

    pub fn read_words(&mut self,id:BufferId,offset:u64,words:usize) -> Result<Vec<u32>> {
        self.ready()?;
        let b = self.buffer(id)?;
        let bytes = (words as u64).checked_mul(4).ok_or("read size overflow")?;
        check_range(b.bytes,offset,bytes)?;
        if b.kind != MemoryKind::HostVisible { return Err("read requires host-visible buffer".into()); }
        let mut result = vec![0u32;words];
        unsafe {
            let ptr = self.d.map_memory(b.memory,0,b.bytes,vk::MemoryMapFlags::empty())?;
            std::ptr::copy_nonoverlapping(ptr.cast::<u8>().add(offset as usize).cast::<u32>(),result.as_mut_ptr(),words);
            self.d.unmap_memory(b.memory);
        }
        Ok(result)
    }

    /// Validate the entire list before recording. No partial execution of
    /// an invalid command list. Same-buffer copies are not supported yet.
    fn validate(&self,commands:&[Command]) -> Result<()> {
        for &command in commands {
            match command {
                Command::Fill { dst,offset,bytes,.. } => check_range(self.buffer(dst)?.bytes,offset,bytes)?,
                Command::Copy { src,src_offset,dst,dst_offset,bytes } => {
                    if src == dst { return Err("same-buffer copy is not implemented".into()); }
                    check_range(self.buffer(src)?.bytes,src_offset,bytes)?;
                    check_range(self.buffer(dst)?.bytes,dst_offset,bytes)?;
                }
            }
        }
        Ok(())
    }

    pub fn submit(&mut self,commands:&[Command]) -> Result<u64> {
        self.ready()?;
        self.validate(commands)?;
        if commands.is_empty() { return Ok(self.completed); }
        let epoch = self.completed.checked_add(1).ok_or("timeline exhausted")?;
        // Any error after beginning recording blocks subsequent reuse.
        self.poisoned = true;
        let d = self.d;
        let cmd = self.command;
        unsafe {
            d.reset_command_pool(self.pool,vk::CommandPoolResetFlags::empty())?;
            d.begin_command_buffer(cmd,&vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
            for (index,&command) in commands.iter().enumerate() {
                if index != 0 {
                    // Conservative transfer dependency, including WAR/WAW/RAW.
                    let barrier = [vk::MemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)];
                    d.cmd_pipeline_barrier2(cmd,&vk::DependencyInfo::default().memory_barriers(&barrier));
                }
                match command {
                    Command::Fill { dst,offset,bytes,value } => {
                        d.cmd_fill_buffer(cmd,self.buffer(dst)?.handle,offset,bytes,value);
                    }
                    Command::Copy { src,src_offset,dst,dst_offset,bytes } => {
                        d.cmd_copy_buffer(cmd,self.buffer(src)?.handle,self.buffer(dst)?.handle,
                            &[vk::BufferCopy::default().src_offset(src_offset).dst_offset(dst_offset).size(bytes)]);
                    }
                }
            }
            let host = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ | vk::AccessFlags2::HOST_WRITE)];
            d.cmd_pipeline_barrier2(cmd,&vk::DependencyInfo::default().memory_barriers(&host));
            d.end_command_buffer(cmd)?;
            let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let wait = [vk::SemaphoreSubmitInfo::default().semaphore(self.timeline).value(self.completed)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signal = [vk::SemaphoreSubmitInfo::default().semaphore(self.timeline).value(epoch)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            d.queue_submit2(self.queue,&[vk::SubmitInfo2::default().command_buffer_infos(&commands)
                .wait_semaphore_infos(&wait).signal_semaphore_infos(&signal)],vk::Fence::null())?;
            d.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&[self.timeline]).values(&[epoch]),u64::MAX)?;
        }
        self.completed = epoch;
        self.poisoned = false;
        Ok(epoch)
    }
}

pub(crate) fn verify(context:&mut Context) -> Result<()> {
    let mut s = Session::new(context)?;
    let upload = s.create_buffer(64,MemoryKind::HostVisible)?;
    let local = s.create_buffer(64,MemoryKind::DeviceLocal)?;
    let output = s.create_buffer(64,MemoryKind::HostVisible)?;
    let words: Vec<u32> = (0..16).map(|i| 100+i).collect();
    s.write_words(upload,0,&words)?;
    let epoch = s.submit(&[
        Command::Copy { src:upload,src_offset:0,dst:local,dst_offset:0,bytes:64 },
        Command::Fill { dst:local,offset:16,bytes:16,value:0xfeedbeef },
        Command::Copy { src:local,src_offset:0,dst:output,dst_offset:0,bytes:64 },
    ])?;
    let mut expected = words;
    expected[4..8].fill(0xfeedbeef);
    if s.read_words(output,0,16)? != expected { return Err("command executor output mismatch".into()); }
    println!("PASS: ordered upload, fill and readback at timeline {epoch}");
    // An invalid second operation must reject the entire list.
    if s.submit(&[
        Command::Fill { dst:output,offset:0,bytes:4,value:0 },
        Command::Copy { src:local,src_offset:0,dst:output,dst_offset:60,bytes:8 },
    ]).is_ok() { return Err("accepted invalid copy".into()); }
    if s.read_words(output,0,16)? != expected { return Err("invalid list changed output".into()); }
    s.destroy_buffer(local)?;
    if s.submit(&[Command::Fill { dst:local,offset:0,bytes:4,value:0 }]).is_ok() {
        return Err("accepted retired buffer ID".into());
    }
    let replacement = s.create_buffer(64,MemoryKind::DeviceLocal)?;
    if replacement == local { return Err("resource ID reused".into()); }
    s.submit(&[Command::Copy { src:replacement,src_offset:0,dst:output,dst_offset:0,bytes:64 }])?;
    if s.read_words(output,0,16)? != vec![0;16] { return Err("new buffer not initialized".into()); }
    println!("PASS: command validation, retired resource rejection and initialized replacement verified");
    println!("PASS: reusable Vulkan buffer command executor verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_validation() {
        assert!(check_range(64,60,4).is_ok());
        for (offset,bytes) in [(0,0),(2,4),(0,3),(60,8),(u64::MAX-3,8)] {
            assert!(check_range(64,offset,bytes).is_err());
        }
    }
}
