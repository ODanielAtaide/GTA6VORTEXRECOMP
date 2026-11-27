// SPDX-License-Identifier: GPL-3.0-or-later
//! First reusable Vulkan command executor, separate from diagnostic fixtures.
//! Commands operate on session-local resource IDs, never guest pointers.
//! Synchronous completion is intentional until title lifetime tracking is wired.

use ash::vk;
use std::collections::BTreeMap;
use crate::{Context,Result};

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct BufferId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct ImageId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct PipelineId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct IndexId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct ComputeId(u64);

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub struct ComputeBindingId(u64);

/// Initial compute ABI: local_size_x=64, one read/write storage buffer,
/// and two u32 push constants. No other descriptors.
const COMPUTE_LANES: u32 = 64;

/// Fixed initial graphics ABI: position xyz and color rgba, seven f32s.
const VERTEX_BYTES: u64 = 28;

/// Tightly packed RGBA8 texels, mip 0, layer 0.
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct ImageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum MemoryKind { DeviceLocal, HostVisible }

/// Initial attachment formats; all supported texels occupy four bytes.
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ImageKind { Rgba8, Depth32 }

impl ImageKind {
    fn format(self) -> vk::Format {
        match self { Self::Rgba8 => vk::Format::R8G8B8A8_UNORM, Self::Depth32 => vk::Format::D32_SFLOAT }
    }
    fn aspect(self) -> vk::ImageAspectFlags {
        match self { Self::Rgba8 => vk::ImageAspectFlags::COLOR, Self::Depth32 => vk::ImageAspectFlags::DEPTH }
    }
    fn usage(self) -> vk::ImageUsageFlags {
        let transfer = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        transfer | match self {
            Self::Rgba8 => vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            Self::Depth32 => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        }
    }
}

#[derive(Debug,Clone,Copy)]
pub enum Command {
    Fill { dst: BufferId, offset: u64, bytes: u64, value: u32 },
    Copy { src: BufferId, src_offset: u64, dst: BufferId, dst_offset: u64, bytes: u64 },
    ClearImage { dst: ImageId, rgba: [f32;4] },
    ClearDepth { dst: ImageId, depth: f32 },
    /// Clear and store color and depth attachments over a render rectangle.
    ClearAttachments { color: ImageId, depth: ImageId, region: ImageRegion, rgba: [f32;4], z: f32 },
    Draw { pipeline: PipelineId, color: ImageId, depth: ImageId, region: ImageRegion,
        vertices: BufferId, offset: u64, count: u32 },
    DrawIndexed { pipeline: PipelineId, color: ImageId, depth: ImageId, region: ImageRegion,
        vertices: BufferId, offset: u64, indices: IndexId, first: u32, count: u32, base_vertex: i32 },
    Dispatch { binding: ComputeBindingId, groups: u32, constants: [u32;2] },
    UploadImage { src: BufferId, offset: u64, dst: ImageId, region: ImageRegion },
    ReadImage { src: ImageId, region: ImageRegion, dst: BufferId, offset: u64 },
    CopyImage { src: ImageId, region: ImageRegion, dst: ImageId, x: u32, y: u32 },
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
                    | vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::INDEX_BUFFER)
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

fn image_range(kind: ImageKind) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default().aspect_mask(kind.aspect())
        .level_count(1).layer_count(1)
}
fn image_layers(kind: ImageKind) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default().aspect_mask(kind.aspect()).layer_count(1)
}
fn valid_color(rgba: [f32;4]) -> bool {
    rgba.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
fn valid_depth(z: f32) -> bool { z.is_finite() && (0.0..=1.0).contains(&z) }
fn image_offset(region: ImageRegion) -> vk::Offset3D {
    vk::Offset3D { x:region.x as i32,y:region.y as i32,z:0 }
}
fn image_extent(region: ImageRegion) -> vk::Extent3D {
    vk::Extent3D { width:region.width,height:region.height,depth:1 }
}
fn region_bytes(width:u32,height:u32,region:ImageRegion) -> Result<u64> {
    if region.width == 0 || region.height == 0
        || region.x > i32::MAX as u32 || region.y > i32::MAX as u32
        || region.x.checked_add(region.width).is_none_or(|end| end > width)
        || region.y.checked_add(region.height).is_none_or(|end| end > height) {
        return Err("image rectangle is empty or out of bounds".into());
    }
    u64::from(region.width).checked_mul(u64::from(region.height))
        .and_then(|n| n.checked_mul(4)).ok_or_else(|| "image byte count overflow".into())
}
fn buffer_image_copy(offset:u64,region:ImageRegion,kind:ImageKind) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default().buffer_offset(offset)
        .image_subresource(image_layers(kind)).image_offset(image_offset(region))
        .image_extent(image_extent(region))
}

struct Image<'a> {
    d: &'a ash::Device,
    handle: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    kind: ImageKind,
    width: u32,
    height: u32,
    initialized: bool,
}
impl Drop for Image<'_> {
    fn drop(&mut self) {
        unsafe {
            self.d.destroy_image_view(self.view,None);
            self.d.destroy_image(self.handle,None);
            self.d.free_memory(self.memory,None);
        }
    }
}
impl<'a> Image<'a> {
    fn new(d:&'a ash::Device,mem:&vk::PhysicalDeviceMemoryProperties,width:u32,height:u32,kind:ImageKind) -> Result<Self> {
        let mut image = Self { d,handle:vk::Image::null(),memory:vk::DeviceMemory::null(),
            view:vk::ImageView::null(),kind,width,height,initialized:false };
        unsafe {
            image.handle = d.create_image(&vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D).format(kind.format())
                .extent(vk::Extent3D { width,height,depth:1 }).mip_levels(1).array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::OPTIMAL)
                .usage(kind.usage()).sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),None)?;
            let req = d.get_image_memory_requirements(image.handle);
            use vk::MemoryPropertyFlags as F;
            let index = (0..mem.memory_type_count)
                .filter(|&i| req.memory_type_bits & (1u32 << i) != 0)
                .find(|&i| {
                    let flags = mem.memory_types[i as usize].property_flags;
                    flags.contains(F::DEVICE_LOCAL)
                        && !flags.intersects(F::HOST_VISIBLE | F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD)
                }).ok_or("No dedicated device-local image memory")?;
            image.memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
                .allocation_size(req.size).memory_type_index(index),None)?;
            d.bind_image_memory(image.handle,image.memory,0)?;
            image.view = d.create_image_view(&vk::ImageViewCreateInfo::default()
                .image(image.handle).view_type(vk::ImageViewType::TYPE_2D)
                .format(kind.format()).subresource_range(image_range(kind)),None)?;
        }
        Ok(image)
    }

    fn check(&self,region:ImageRegion) -> Result<u64> {
        region_bytes(self.width,self.height,region)
    }
}

// SAFETY: invoked only for the single initial clear of a newly allocated
// image; its previous layout is UNDEFINED and no other operation owns it.
fn initialize_layout(d:&ash::Device,cmd:vk::CommandBuffer,image:&Image<'_>) {
    if image.initialized { return; }
    let barriers = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::NONE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED).new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image.handle).subresource_range(image_range(image.kind))];
    unsafe { d.cmd_pipeline_barrier2(cmd,&vk::DependencyInfo::default().image_memory_barriers(&barriers)); }
}

struct GraphicsPipeline<'a> {
    d: &'a ash::Device,
    vert: vk::ShaderModule,
    frag: vk::ShaderModule,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}
impl Drop for GraphicsPipeline<'_> {
    fn drop(&mut self) {
        unsafe {
            self.d.destroy_pipeline(self.pipeline,None);
            self.d.destroy_pipeline_layout(self.layout,None);
            self.d.destroy_shader_module(self.vert,None);
            self.d.destroy_shader_module(self.frag,None);
        }
    }
}
impl<'a> GraphicsPipeline<'a> {
    fn new(d:&'a ash::Device,vert:&[u32],frag:&[u32]) -> Result<Self> {
        let mut p = Self { d,vert:vk::ShaderModule::null(),frag:vk::ShaderModule::null(),
            layout:vk::PipelineLayout::null(),pipeline:vk::Pipeline::null() };
        unsafe {
            p.vert = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(vert),None)?;
            p.frag = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(frag),None)?;
            p.layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(),None)?;
            let entry = c"main";
            let stages = [
                vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX)
                    .module(p.vert).name(entry),
                vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(p.frag).name(entry),
            ];
            let bindings = [vk::VertexInputBindingDescription::default().binding(0)
                .stride(VERTEX_BYTES as u32).input_rate(vk::VertexInputRate::VERTEX)];
            let attributes = [
                vk::VertexInputAttributeDescription::default().location(0).binding(0)
                    .format(vk::Format::R32G32B32_SFLOAT).offset(0),
                vk::VertexInputAttributeDescription::default().location(1).binding(0)
                    .format(vk::Format::R32G32B32A32_SFLOAT).offset(12),
            ];
            let vertex = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings).vertex_attribute_descriptions(&attributes);
            let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE)
                .front_face(vk::FrontFace::COUNTER_CLOCKWISE).line_width(1.);
            let samples = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let depth = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(true).depth_write_enable(true).depth_compare_op(vk::CompareOp::LESS);
            let blends = [vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blends);
            let dynamic_states = [vk::DynamicState::VIEWPORT,vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let formats = [ImageKind::Rgba8.format()];
            let mut rendering = vk::PipelineRenderingCreateInfo::default()
                .color_attachment_formats(&formats).depth_attachment_format(ImageKind::Depth32.format());
            let info = vk::GraphicsPipelineCreateInfo::default().stages(&stages)
                .vertex_input_state(&vertex).input_assembly_state(&assembly).viewport_state(&viewport)
                .rasterization_state(&raster).multisample_state(&samples).depth_stencil_state(&depth)
                .color_blend_state(&blend).dynamic_state(&dynamic).layout(p.layout)
                .push_next(&mut rendering);
            p.pipeline = match d.create_graphics_pipelines(vk::PipelineCache::null(),&[info],None) {
                Ok(pipelines) => pipelines[0],
                Err((pipelines,e)) => {
                    for pipeline in pipelines { d.destroy_pipeline(pipeline,None); }
                    return Err(e.into());
                }
            };
        }
        Ok(p)
    }
}

/// Immutable UINT32 index buffer. The CPU reference is retained for validation;
/// neither ordinary buffer writes nor GPU commands can change its contents.
struct IndexBuffer<'a> {
    buffer: Buffer<'a>,
    words: Vec<u32>,
}

fn indexed_vertex_bytes(words:&[u32],first:u32,count:u32,base_vertex:i32) -> Result<u64> {
    if count == 0 || count % 3 != 0 { return Err("invalid indexed triangle count".into()); }
    let end = first.checked_add(count).ok_or("index range overflow")?;
    let selected = words.get(first as usize..end as usize).ok_or("index range out of bounds")?;
    let min = i64::from(*selected.iter().min().ok_or("empty index range")?) + i64::from(base_vertex);
    let max = i64::from(*selected.iter().max().ok_or("empty index range")?) + i64::from(base_vertex);
    if min < 0 || max > i64::from(u32::MAX) { return Err("base vertex out of bounds".into()); }
    (max as u64 + 1).checked_mul(VERTEX_BYTES).ok_or_else(|| "vertex size overflow".into())
}

struct ComputePipeline<'a> {
    d: &'a ash::Device,
    shader: vk::ShaderModule,
    descriptors: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}
impl Drop for ComputePipeline<'_> {
    fn drop(&mut self) {
        unsafe {
            self.d.destroy_pipeline(self.pipeline,None);
            self.d.destroy_pipeline_layout(self.layout,None);
            self.d.destroy_descriptor_set_layout(self.descriptors,None);
            self.d.destroy_shader_module(self.shader,None);
        }
    }
}
impl<'a> ComputePipeline<'a> {
    fn new(d:&'a ash::Device,words:&[u32]) -> Result<Self> {
        let mut p = Self { d,shader:vk::ShaderModule::null(),descriptors:vk::DescriptorSetLayout::null(),
            layout:vk::PipelineLayout::null(),pipeline:vk::Pipeline::null() };
        unsafe {
            p.shader = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(words),None)?;
            let bindings = [vk::DescriptorSetLayoutBinding::default().binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)];
            p.descriptors = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default()
                .bindings(&bindings),None)?;
            let sets = [p.descriptors];
            let push = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0).size(8)];
            p.layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&sets).push_constant_ranges(&push),None)?;
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE)
                .module(p.shader).name(c"main");
            let info = vk::ComputePipelineCreateInfo::default().stage(stage).layout(p.layout);
            p.pipeline = match d.create_compute_pipelines(vk::PipelineCache::null(),&[info],None) {
                Ok(pipelines) => pipelines[0],
                Err((pipelines,e)) => {
                    for pipeline in pipelines { d.destroy_pipeline(pipeline,None); }
                    return Err(e.into());
                }
            };
        }
        Ok(p)
    }
}
/// Immutable descriptor binding. Its buffer and pipeline may not be retired
/// while this object exists. Every session submission completes synchronously.
struct ComputeBinding<'a> {
    d: &'a ash::Device,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    pipeline: ComputeId,
    buffer: BufferId,
    bytes: u64,
}
impl Drop for ComputeBinding<'_> {
    fn drop(&mut self) { unsafe { self.d.destroy_descriptor_pool(self.pool,None); } }
}
impl<'a> ComputeBinding<'a> {
    fn new(d:&'a ash::Device,pipeline:ComputeId,buffer:BufferId,layout:vk::DescriptorSetLayout,
           handle:vk::Buffer,offset:u64,bytes:u64) -> Result<Self> {
        let mut b = Self { d,pool:vk::DescriptorPool::null(),set:vk::DescriptorSet::null(),
            pipeline,buffer,bytes };
        unsafe {
            let sizes = [vk::DescriptorPoolSize { ty:vk::DescriptorType::STORAGE_BUFFER,descriptor_count:1 }];
            b.pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default()
                .max_sets(1).pool_sizes(&sizes),None)?;
            b.set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(b.pool).set_layouts(&[layout]))?[0];
            let infos = [vk::DescriptorBufferInfo::default().buffer(handle).offset(offset).range(bytes)];
            d.update_descriptor_sets(&[vk::WriteDescriptorSet::default().dst_set(b.set).dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&infos)],&[]);
        }
        Ok(b)
    }
}

fn dispatch_bytes(groups:u32,limit:u32,bytes:u64) -> Result<()> {
    if groups == 0 || groups > limit || u64::from(groups)*u64::from(COMPUTE_LANES)*4 > bytes {
        return Err("dispatch exceeds group or storage buffer bounds".into());
    }
    Ok(())
}

/// Owns resources and a command queue while exclusively borrowing Context.
/// IDs are valid only in the session that created them. Failed submissions
/// poison the session; no resource is reused before completion is known.
pub struct Session<'a> {
    d: &'a ash::Device,
    instance: &'a ash::Instance,
    physical: vk::PhysicalDevice,
    memory: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    timeline: vk::Semaphore,
    completed: u64,
    next_id: u64,
    poisoned: bool,
    compute_bindings: BTreeMap<ComputeBindingId,ComputeBinding<'a>>,
    compute_pipelines: BTreeMap<ComputeId,ComputePipeline<'a>>,
    buffers: BTreeMap<BufferId,Buffer<'a>>,
    images: BTreeMap<ImageId,Image<'a>>,
    pipelines: BTreeMap<PipelineId,GraphicsPipeline<'a>>,
    indices: BTreeMap<IndexId,IndexBuffer<'a>>,
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
            d, instance:&context.instance, physical:context.physical,
            memory:unsafe { context.instance.get_physical_device_memory_properties(context.physical) },
            queue:unsafe { d.get_device_queue(context.family,0) },
            pool:vk::CommandPool::null(),command:vk::CommandBuffer::null(),
            timeline:vk::Semaphore::null(),completed:0,next_id:1,poisoned:false,buffers:BTreeMap::new(),images:BTreeMap::new(),pipelines:BTreeMap::new(),indices:BTreeMap::new(),compute_bindings:BTreeMap::new(),compute_pipelines:BTreeMap::new(),
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
        if self.compute_bindings.values().any(|b| b.buffer == id) {
            return Err("buffer retained by a compute binding".into());
        }
        self.buffers.remove(&id).ok_or("unknown or retired buffer ID")?;
        Ok(())
    }

    fn image(&self,id:ImageId) -> Result<&Image<'a>> {
        self.images.get(&id).ok_or_else(|| "unknown or retired image ID".into())
    }

    /// Allocate an RGBA8 color image, compatible with rendering and transfers.
    pub fn create_image(&mut self,width:u32,height:u32) -> Result<ImageId> {
        self.create_image_kind(width,height,ImageKind::Rgba8)
    }

    pub fn create_depth_image(&mut self,width:u32,height:u32) -> Result<ImageId> {
        self.create_image_kind(width,height,ImageKind::Depth32)
    }

    fn create_image_kind(&mut self,width:u32,height:u32,kind:ImageKind) -> Result<ImageId> {
        self.ready()?;
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err("invalid image dimensions".into());
        }
        let properties = unsafe { self.instance.get_physical_device_image_format_properties(
            self.physical,kind.format(),vk::ImageType::TYPE_2D,vk::ImageTiling::OPTIMAL,
            kind.usage(),vk::ImageCreateFlags::empty())? };
        let limits = unsafe { self.instance.get_physical_device_properties(self.physical) }.limits;
        if width > properties.max_extent.width || height > properties.max_extent.height
            || width > limits.max_framebuffer_width || height > limits.max_framebuffer_height {
            return Err("image dimensions exceed device limits".into());
        }
        let id = ImageId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("image ID exhausted")?;
        let image = Image::new(self.d,&self.memory,width,height,kind)?;
        self.images.insert(id,image);
        // The ID is not exposed until this one-command initial clear completes.
        let clear = match kind {
            ImageKind::Rgba8 => Command::ClearImage { dst:id,rgba:[0.;4] },
            ImageKind::Depth32 => Command::ClearDepth { dst:id,depth:1. },
        };
        self.submit(&[clear])?;
        self.images.get_mut(&id).expect("created image").initialized = true;
        Ok(id)
    }

    fn pipeline(&self,id:PipelineId) -> Result<&GraphicsPipeline<'a>> {
        self.pipelines.get(&id).ok_or_else(|| "unknown or retired graphics pipeline".into())
    }

    /// Create a pipeline using trusted, validated SPIR-V with the fixed vertex ABI,
    /// no descriptors or push constants, and a single RGBA8 output.
    pub fn create_graphics_pipeline(&mut self,vert:&[u32],frag:&[u32]) -> Result<PipelineId> {
        self.ready()?;
        let id = PipelineId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("pipeline ID exhausted")?;
        let p = GraphicsPipeline::new(self.d,vert,frag)?;
        self.pipelines.insert(id,p);
        Ok(id)
    }

    pub fn destroy_graphics_pipeline(&mut self,id:PipelineId) -> Result<()> {
        self.ready()?;
        self.pipelines.remove(&id).ok_or("unknown or retired graphics pipeline")?;
        Ok(())
    }

    fn compute_pipeline(&self,id:ComputeId) -> Result<&ComputePipeline<'a>> {
        self.compute_pipelines.get(&id).ok_or_else(|| "unknown or retired compute pipeline".into())
    }

    fn compute_binding(&self,id:ComputeBindingId) -> Result<&ComputeBinding<'a>> {
        self.compute_bindings.get(&id).ok_or_else(|| "unknown or retired compute binding".into())
    }

    /// Trusted, validated SPIR-V matching the initial compute ABI. The shader
    /// must restrict storage accesses to its own global invocation index.
    pub fn create_compute_pipeline(&mut self,words:&[u32]) -> Result<ComputeId> {
        self.ready()?;
        let limits = unsafe { self.instance.get_physical_device_properties(self.physical) }.limits;
        if limits.max_compute_work_group_invocations < COMPUTE_LANES
            || limits.max_compute_work_group_size[0] < COMPUTE_LANES {
            return Err("compute local size unsupported".into());
        }
        let id = ComputeId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("compute ID exhausted")?;
        let p = ComputePipeline::new(self.d,words)?;
        self.compute_pipelines.insert(id,p);
        Ok(id)
    }

    /// Decode native GFX10 words, build SSA, and lower the supported diagnostic
    /// subset directly to SPIR-V. This is not a general shader ABI: scalar
    /// programs must be closed and vector programs use the documented synthetic
    /// v0/v1/v2/EXEC inputs. Unsupported instructions are returned as errors,
    /// before creating Vulkan objects.
    pub fn create_native_compute_pipeline(&mut self,code:&[u8],wave64:bool,vgprs:u16,
                                          output:vortex_shader::ir::Reg) -> Result<ComputeId> {
        use vortex_shader::{ir,spirv_scalar,spirv_vector};
        self.ready()?;
        let program = ir::build(code,wave64,vgprs)?;
        let words = match output {
            ir::Reg::S(_) | ir::Reg::Scc => spirv_scalar::emit_scalar(&program,output)?,
            ir::Reg::V(_) => spirv_vector::emit_vector(&program,output)?,
        };
        self.create_compute_pipeline(&words)
    }

    pub fn create_compute_binding(&mut self,pipeline:ComputeId,buffer:BufferId,offset:u64,bytes:u64) -> Result<ComputeBindingId> {
        self.ready()?;
        let p = self.compute_pipeline(pipeline)?;
        let b = self.buffer(buffer)?;
        check_range(b.bytes,offset,bytes)?;
        let limits = unsafe { self.instance.get_physical_device_properties(self.physical) }.limits;
        if offset % limits.min_storage_buffer_offset_alignment.max(1) != 0
            || bytes > u64::from(limits.max_storage_buffer_range) {
            return Err("invalid storage buffer descriptor range".into());
        }
        let binding = ComputeBinding::new(self.d,pipeline,buffer,p.descriptors,b.handle,offset,bytes)?;
        let id = ComputeBindingId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("compute binding ID exhausted")?;
        self.compute_bindings.insert(id,binding);
        Ok(id)
    }

    pub fn destroy_compute_binding(&mut self,id:ComputeBindingId) -> Result<()> {
        self.ready()?;
        self.compute_bindings.remove(&id).ok_or("unknown or retired compute binding")?;
        Ok(())
    }

    pub fn destroy_compute_pipeline(&mut self,id:ComputeId) -> Result<()> {
        self.ready()?;
        if self.compute_bindings.values().any(|b| b.pipeline == id) {
            return Err("pipeline retained by a compute binding".into());
        }
        self.compute_pipelines.remove(&id).ok_or("unknown or retired compute pipeline")?;
        Ok(())
    }

    fn index(&self,id:IndexId) -> Result<&IndexBuffer<'a>> {
        self.indices.get(&id).ok_or_else(|| "unknown or retired index buffer".into())
    }

    /// Upload immutable UINT32 indices through the existing command executor.
    /// Ownership moves into a separate registry only after the copy completes.
    pub fn create_index_buffer(&mut self,words:&[u32]) -> Result<IndexId> {
        self.ready()?;
        if words.is_empty() || words.len() > u32::MAX as usize {
            return Err("invalid index buffer length".into());
        }
        let limits = unsafe { self.instance.get_physical_device_properties(self.physical) }.limits;
        // fullDrawIndexUint32 is not enabled by Context; respect the 24-bit baseline.
        if words.iter().any(|&i| i > limits.max_draw_indexed_index_value.min(0x00ff_ffff)) {
            return Err("index exceeds physical device limit".into());
        }
        let id = IndexId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or("index ID exhausted")?;
        let bytes = words.len() as u64 * 4;
        let staging = self.create_buffer(bytes,MemoryKind::HostVisible)?;
        let local = match self.create_buffer(bytes,MemoryKind::DeviceLocal) {
            Ok(id) => id,
            Err(e) => {
                if !self.poisoned { self.buffers.remove(&staging); }
                return Err(e);
            }
        };
        if let Err(e) = self.write_words(staging,0,words) {
            self.buffers.remove(&staging);
            self.buffers.remove(&local);
            return Err(e);
        }
        // On GPU submission failure, retain both buffers until session drop
        // has waited for idle. Never free potentially in-flight resources.
        self.submit(&[Command::Copy { src:staging,src_offset:0,dst:local,dst_offset:0,bytes }])?;
        self.destroy_buffer(staging)?;
        let buffer = self.buffers.remove(&local).ok_or("missing index backing buffer")?;
        self.indices.insert(id,IndexBuffer { buffer,words:words.to_vec() });
        Ok(id)
    }

    pub fn destroy_index_buffer(&mut self,id:IndexId) -> Result<()> {
        self.ready()?;
        self.indices.remove(&id).ok_or("unknown or retired index buffer")?;
        Ok(())
    }

    pub fn destroy_image(&mut self,id:ImageId) -> Result<()> {
        self.ready()?;
        self.images.remove(&id).ok_or("unknown or retired image ID")?;
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
                Command::ClearImage { dst,rgba } => {
                    if self.image(dst)?.kind != ImageKind::Rgba8 || !valid_color(rgba) {
                        return Err("invalid color image or clear color".into());
                    }
                }
                Command::ClearDepth { dst,depth } => {
                    if self.image(dst)?.kind != ImageKind::Depth32 || !valid_depth(depth) {
                        return Err("invalid depth image or clear value".into());
                    }
                }
                Command::ClearAttachments { color,depth,region,rgba,z } => {
                    let c = self.image(color)?;
                    let d = self.image(depth)?;
                    if c.kind != ImageKind::Rgba8 || d.kind != ImageKind::Depth32
                        || (c.width,c.height) != (d.width,d.height)
                        || !valid_color(rgba) || !valid_depth(z) {
                        return Err("incompatible render attachments or clear values".into());
                    }
                    c.check(region)?;
                    d.check(region)?;
                }
                Command::Draw { pipeline,color,depth,region,vertices,offset,count } => {
                    self.pipeline(pipeline)?;
                    let c = self.image(color)?;
                    let d = self.image(depth)?;
                    if c.kind != ImageKind::Rgba8 || d.kind != ImageKind::Depth32
                        || (c.width,c.height) != (d.width,d.height)
                        || count == 0 || count % 3 != 0 {
                        return Err("invalid draw attachments or triangle count".into());
                    }
                    c.check(region)?;
                    d.check(region)?;
                    check_range(self.buffer(vertices)?.bytes,offset,u64::from(count)*VERTEX_BYTES)?;
                }
                Command::DrawIndexed { pipeline,color,depth,region,vertices,offset,indices,first,count,base_vertex } => {
                    self.pipeline(pipeline)?;
                    let c = self.image(color)?;
                    let d = self.image(depth)?;
                    if c.kind != ImageKind::Rgba8 || d.kind != ImageKind::Depth32
                        || (c.width,c.height) != (d.width,d.height) {
                        return Err("invalid indexed draw attachments".into());
                    }
                    c.check(region)?;
                    d.check(region)?;
                    let bytes = indexed_vertex_bytes(&self.index(indices)?.words,first,count,base_vertex)?;
                    check_range(self.buffer(vertices)?.bytes,offset,bytes)?;
                }
                Command::Dispatch { binding,groups,.. } => {
                    let b = self.compute_binding(binding)?;
                    self.compute_pipeline(b.pipeline)?;
                    self.buffer(b.buffer)?;
                    let limits = unsafe { self.instance.get_physical_device_properties(self.physical) }.limits;
                    dispatch_bytes(groups,limits.max_compute_work_group_count[0],b.bytes)?;
                }
                Command::UploadImage { src,offset,dst,region } => {
                    let image = self.image(dst)?;
                    if image.kind != ImageKind::Rgba8 { return Err("depth upload not implemented".into()); }
                    let bytes = image.check(region)?;
                    check_range(self.buffer(src)?.bytes,offset,bytes)?;
                }
                Command::ReadImage { src,region,dst,offset } => {
                    let bytes = self.image(src)?.check(region)?;
                    check_range(self.buffer(dst)?.bytes,offset,bytes)?;
                }
                Command::CopyImage { src,region,dst,x,y } => {
                    if src == dst { return Err("same-image copy is not implemented".into()); }
                    let from = self.image(src)?;
                    let to = self.image(dst)?;
                    if from.kind != to.kind { return Err("image copy format mismatch".into()); }
                    from.check(region)?;
                    to.check(ImageRegion { x,y,..region })?;
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
                    // Conservative cross-command dependency covers transfers
                    // and dynamic rendering attachment operations.
                    let barrier = [vk::MemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .src_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)];
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
                    Command::ClearImage { dst,rgba } => {
                        let image = self.image(dst)?;
                        initialize_layout(d,cmd,image);
                        d.cmd_clear_color_image(cmd,image.handle,vk::ImageLayout::GENERAL,
                            &vk::ClearColorValue { float32:rgba },&[image_range(image.kind)]);
                    }
                    Command::ClearDepth { dst,depth } => {
                        let image = self.image(dst)?;
                        initialize_layout(d,cmd,image);
                        d.cmd_clear_depth_stencil_image(cmd,image.handle,vk::ImageLayout::GENERAL,
                            &vk::ClearDepthStencilValue { depth,stencil:0 },&[image_range(image.kind)]);
                    }
                    Command::ClearAttachments { color,depth,region,rgba,z } => {
                        let color = self.image(color)?;
                        let depth = self.image(depth)?;
                        let colors = [vk::RenderingAttachmentInfo::default().image_view(color.view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
                            .clear_value(vk::ClearValue { color:vk::ClearColorValue { float32:rgba } })];
                        let depth_attachment = vk::RenderingAttachmentInfo::default().image_view(depth.view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
                            .clear_value(vk::ClearValue { depth_stencil:vk::ClearDepthStencilValue { depth:z,stencil:0 } });
                        let area = vk::Rect2D {
                            offset:vk::Offset2D { x:region.x as i32,y:region.y as i32 },
                            extent:vk::Extent2D { width:region.width,height:region.height },
                        };
                        d.cmd_begin_rendering(cmd,&vk::RenderingInfo::default().render_area(area)
                            .layer_count(1).color_attachments(&colors).depth_attachment(&depth_attachment));
                        d.cmd_end_rendering(cmd);
                    }
                    Command::Draw { pipeline,color,depth,region,vertices,offset,count }
                    | Command::DrawIndexed { pipeline,color,depth,region,vertices,offset,count,.. } => {
                        let p = self.pipeline(pipeline)?;
                        let c = self.image(color)?;
                        let z = self.image(depth)?;
                        let colors = [vk::RenderingAttachmentInfo::default().image_view(c.view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .load_op(vk::AttachmentLoadOp::LOAD).store_op(vk::AttachmentStoreOp::STORE)];
                        let depth_attachment = vk::RenderingAttachmentInfo::default().image_view(z.view)
                            .image_layout(vk::ImageLayout::GENERAL)
                            .load_op(vk::AttachmentLoadOp::LOAD).store_op(vk::AttachmentStoreOp::STORE);
                        let area = vk::Rect2D {
                            offset:vk::Offset2D { x:region.x as i32,y:region.y as i32 },
                            extent:vk::Extent2D { width:region.width,height:region.height },
                        };
                        let viewport = [vk::Viewport { x:region.x as f32,y:region.y as f32,
                            width:region.width as f32,height:region.height as f32,min_depth:0.,max_depth:1. }];
                        d.cmd_begin_rendering(cmd,&vk::RenderingInfo::default().render_area(area)
                            .layer_count(1).color_attachments(&colors).depth_attachment(&depth_attachment));
                        d.cmd_bind_pipeline(cmd,vk::PipelineBindPoint::GRAPHICS,p.pipeline);
                        d.cmd_set_viewport(cmd,0,&viewport);
                        d.cmd_set_scissor(cmd,0,&[area]);
                        d.cmd_bind_vertex_buffers(cmd,0,&[self.buffer(vertices)?.handle],&[offset]);
                        match command {
                            Command::DrawIndexed { indices,first,base_vertex,.. } => {
                                d.cmd_bind_index_buffer(cmd,self.index(indices)?.buffer.handle,0,vk::IndexType::UINT32);
                                d.cmd_draw_indexed(cmd,count,1,first,base_vertex,0);
                            }
                            _ => d.cmd_draw(cmd,count,1,0,0),
                        }
                        d.cmd_end_rendering(cmd);
                    }
                    Command::Dispatch { binding,groups,constants } => {
                        let b = self.compute_binding(binding)?;
                        let p = self.compute_pipeline(b.pipeline)?;
                        let mut bytes = [0u8;8];
                        bytes[..4].copy_from_slice(&constants[0].to_ne_bytes());
                        bytes[4..].copy_from_slice(&constants[1].to_ne_bytes());
                        d.cmd_bind_pipeline(cmd,vk::PipelineBindPoint::COMPUTE,p.pipeline);
                        d.cmd_bind_descriptor_sets(cmd,vk::PipelineBindPoint::COMPUTE,p.layout,0,&[b.set],&[]);
                        d.cmd_push_constants(cmd,p.layout,vk::ShaderStageFlags::COMPUTE,0,&bytes);
                        d.cmd_dispatch(cmd,groups,1,1);
                    }
                    Command::UploadImage { src,offset,dst,region } => {
                        d.cmd_copy_buffer_to_image(cmd,self.buffer(src)?.handle,self.image(dst)?.handle,
                            vk::ImageLayout::GENERAL,&[buffer_image_copy(offset,region,self.image(dst)?.kind)]);
                    }
                    Command::ReadImage { src,region,dst,offset } => {
                        d.cmd_copy_image_to_buffer(cmd,self.image(src)?.handle,vk::ImageLayout::GENERAL,
                            self.buffer(dst)?.handle,&[buffer_image_copy(offset,region,self.image(src)?.kind)]);
                    }
                    Command::CopyImage { src,region,dst,x,y } => {
                        let copy = vk::ImageCopy::default().src_subresource(image_layers(self.image(src)?.kind))
                            .src_offset(image_offset(region)).dst_subresource(image_layers(self.image(dst)?.kind))
                            .dst_offset(image_offset(ImageRegion { x,y,..region })).extent(image_extent(region));
                        d.cmd_copy_image(cmd,self.image(src)?.handle,vk::ImageLayout::GENERAL,
                            self.image(dst)?.handle,vk::ImageLayout::GENERAL,&[copy]);
                    }
                }
            }
            let host = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
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

pub(crate) fn verify_images(context:&mut Context) -> Result<()> {
    let mut s = Session::new(context)?;
    let full = ImageRegion { x:0,y:0,width:8,height:8 };
    let part = ImageRegion { x:2,y:1,width:3,height:2 };
    let upload = s.create_buffer(32,MemoryKind::HostVisible)?;
    let output = s.create_buffer(256,MemoryKind::HostVisible)?;
    let a = s.create_image(8,8)?;
    let b = s.create_image(8,8)?;
    s.submit(&[Command::ReadImage { src:a,region:full,dst:output,offset:0 }])?;
    if s.read_words(output,0,64)? != vec![0;64] {
        return Err("initial image clear mismatch".into());
    }
    let pixels = [0xff112233,0xff223344,0xff334455,0xff445566,0xff556677,0xff667788];
    s.write_words(upload,4,&pixels)?;
    let epoch = s.submit(&[
        Command::ClearImage { dst:a,rgba:[0.,0.,0.,1.] },
        Command::UploadImage { src:upload,offset:4,dst:a,region:part },
        Command::ClearImage { dst:b,rgba:[1.,0.,0.,1.] },
        Command::CopyImage { src:a,region:part,dst:b,x:4,y:5 },
        Command::ReadImage { src:b,region:full,dst:output,offset:0 },
    ])?;
    let mut expected = vec![u32::from_le_bytes([255,0,0,255]);64];
    for row in 0..2usize {
        let start = (5+row)*8+4;
        expected[start..start+3].copy_from_slice(&pixels[row*3..row*3+3]);
    }
    if s.read_words(output,0,64)? != expected {
        return Err("image copy or untouched texel mismatch".into());
    }
    println!("PASS: RGBA8 clear, partial upload, image copy and full readback at timeline {epoch}");
    if s.submit(&[
        Command::ClearImage { dst:b,rgba:[0.;4] },
        Command::UploadImage { src:upload,offset:4,dst:b,region:ImageRegion { x:7,..part } },
    ]).is_ok() { return Err("accepted out-of-bounds image copy".into()); }
    s.submit(&[Command::ReadImage { src:b,region:full,dst:output,offset:0 }])?;
    if s.read_words(output,0,64)? != expected { return Err("rejected list altered image".into()); }
    s.destroy_image(a)?;
    if s.submit(&[Command::ReadImage { src:a,region:full,dst:output,offset:0 }]).is_ok() {
        return Err("accepted retired image ID".into());
    }
    println!("PASS: invalid image list and retired resource rejected");
    println!("PASS: reusable Vulkan image command executor verified");
    Ok(())
}

pub(crate) fn verify_attachments(context:&mut Context) -> Result<()> {
    let mut s = Session::new(context)?;
    let full = ImageRegion { x:0,y:0,width:8,height:8 };
    let partial = ImageRegion { x:2,y:3,width:4,height:2 };
    let color = s.create_image(8,8)?;
    let depth = s.create_depth_image(8,8)?;
    let output = s.create_buffer(512,MemoryKind::HostVisible)?;
    let epoch = s.submit(&[
        Command::ClearAttachments { color,depth,region:full,rgba:[0.,0.,1.,1.],z:0.75 },
        Command::ClearAttachments { color,depth,region:partial,rgba:[1.,0.,0.,1.],z:0.25 },
        Command::ReadImage { src:color,region:full,dst:output,offset:0 },
        Command::ReadImage { src:depth,region:full,dst:output,offset:256 },
    ])?;
    let mut expected = Vec::with_capacity(128);
    for y in 0..8 {
        for x in 0..8 {
            let inside = (2..6).contains(&x) && (3..5).contains(&y);
            expected.push(u32::from_le_bytes(if inside { [255,0,0,255] } else { [0,0,255,255] }));
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let inside = (2..6).contains(&x) && (3..5).contains(&y);
            expected.push(if inside { 0.25f32.to_bits() } else { 0.75f32.to_bits() });
        }
    }
    if s.read_words(output,0,128)? != expected {
        return Err("dynamic rendering color/depth rectangle mismatch".into());
    }
    println!("PASS: dynamic rendering RGBA8 and D32 color/depth attachment clears at timeline {epoch}");
    let bad = s.create_depth_image(4,4)?;
    if s.submit(&[
        Command::ClearImage { dst:color,rgba:[0.;4] },
        Command::ClearAttachments { color,depth:bad,region:full,rgba:[0.;4],z:0. },
    ]).is_ok() { return Err("accepted mismatched attachments".into()); }
    if s.submit(&[Command::ClearDepth { dst:color,depth:0. }]).is_ok()
        || s.submit(&[Command::ClearDepth { dst:depth,depth:f32::NAN }]).is_ok() {
        return Err("accepted invalid depth clear".into());
    }
    s.submit(&[
        Command::ReadImage { src:color,region:full,dst:output,offset:0 },
        Command::ReadImage { src:depth,region:full,dst:output,offset:256 },
    ])?;
    if s.read_words(output,0,128)? != expected { return Err("invalid render list changed attachments".into()); }
    println!("PASS: mismatched attachments and invalid depth values rejected");
    println!("PASS: reusable Vulkan color and depth attachment executor verified");
    Ok(())
}

pub(crate) fn verify_draws(context:&mut Context,vert:&std::path::Path,frag:&std::path::Path) -> Result<()> {
    let load = |path:&std::path::Path| -> Result<Vec<u32>> {
        Ok(ash::util::read_spv(&mut std::io::Cursor::new(std::fs::read(path)?))?)
    };
    let vert = load(vert)?;
    let frag = load(frag)?;
    let mut s = Session::new(context)?;
    let p = s.create_graphics_pipeline(&vert,&frag)?;
    let color = s.create_image(8,8)?;
    let depth = s.create_depth_image(8,8)?;
    let full = ImageRegion { x:0,y:0,width:8,height:8 };
    let partial = ImageRegion { x:2,y:2,width:4,height:4 };
    let mut data = Vec::new();
    for (z,rgba) in [(0.5f32,[1.,0.,0.,1.]),(0.75,[0.,1.,0.,1.]),(0.25,[0.,0.,1.,1.])] {
        for (x,y) in [(-1f32,-1f32),(3.,-1.),(-1.,3.)] {
            data.extend([x,y,z,rgba[0],rgba[1],rgba[2],rgba[3]].map(f32::to_bits));
        }
    }
    let upload = s.create_buffer(data.len() as u64*4,MemoryKind::HostVisible)?;
    let vertices = s.create_buffer(data.len() as u64*4,MemoryKind::DeviceLocal)?;
    let output = s.create_buffer(512,MemoryKind::HostVisible)?;
    s.write_words(upload,0,&data)?;
    let draw = |offset,region| Command::Draw { pipeline:p,color,depth,region,vertices,offset,count:3 };
    let epoch = s.submit(&[
        Command::Copy { src:upload,src_offset:0,dst:vertices,dst_offset:0,bytes:data.len() as u64*4 },
        Command::ClearAttachments { color,depth,region:full,rgba:[0.;4],z:1. },
        draw(0,full),
        draw(3*VERTEX_BYTES,full),
        draw(6*VERTEX_BYTES,partial),
        Command::ReadImage { src:color,region:full,dst:output,offset:0 },
        Command::ReadImage { src:depth,region:full,dst:output,offset:256 },
    ])?;
    let mut expected = Vec::new();
    for y in 0..8 { for x in 0..8 {
        let inside = (2..6).contains(&x) && (2..6).contains(&y);
        expected.push(u32::from_le_bytes(if inside { [0,0,255,255] } else { [255,0,0,255] }));
    }}
    for y in 0..8 { for x in 0..8 {
        let inside = (2..6).contains(&x) && (2..6).contains(&y);
        expected.push(if inside { 0.25f32.to_bits() } else { 0.5f32.to_bits() });
    }}
    if s.read_words(output,0,128)? != expected { return Err("draw color/depth mismatch".into()); }
    println!("PASS: three draws on one reusable pipeline; farther triangle rejected; nearer rectangle verified at timeline {epoch}");
    if s.submit(&[Command::ClearImage { dst:color,rgba:[0.;4] },draw(7*VERTEX_BYTES,full)]).is_ok() {
        return Err("accepted invalid vertex range".into());
    }
    s.submit(&[Command::ReadImage { src:color,region:full,dst:output,offset:0 }])?;
    if s.read_words(output,0,64)? != expected[..64] { return Err("invalid draw list modified color".into()); }
    s.destroy_graphics_pipeline(p)?;
    if s.submit(&[draw(0,full)]).is_ok() { return Err("accepted retired pipeline".into()); }
    println!("PASS: invalid vertex ranges and retired graphics pipeline rejected");
    println!("PASS: reusable Vulkan graphics pipeline and depth-tested draw executor verified");
    Ok(())
}

pub(crate) fn verify_indexed(context:&mut Context,vert:&std::path::Path,frag:&std::path::Path) -> Result<()> {
    let load = |path:&std::path::Path| -> Result<Vec<u32>> {
        Ok(ash::util::read_spv(&mut std::io::Cursor::new(std::fs::read(path)?))?)
    };
    let mut s = Session::new(context)?;
    let p = s.create_graphics_pipeline(&load(vert)?,&load(frag)?)?;
    let color = s.create_image(8,8)?;
    let depth = s.create_depth_image(8,8)?;
    let full = ImageRegion { x:0,y:0,width:8,height:8 };
    // Four vertices form two triangles. Indices deliberately refer to
    // vertices 1..4 through a negative base vertex, and skip three entries.
    let mut data = Vec::new();
    for (x,y) in [(-1f32,-1f32),(1.,-1.),(1.,1.),(-1.,1.)] {
        data.extend([x,y,0.375,0.,1.,0.,1.].map(f32::to_bits));
    }
    let upload = s.create_buffer(data.len() as u64*4,MemoryKind::HostVisible)?;
    let vertices = s.create_buffer(data.len() as u64*4,MemoryKind::DeviceLocal)?;
    let output = s.create_buffer(512,MemoryKind::HostVisible)?;
    s.write_words(upload,0,&data)?;
    let indices = s.create_index_buffer(&[0,0,0,1,2,3,1,3,4])?;
    let draw = |first,count,base_vertex| Command::DrawIndexed {
        pipeline:p,color,depth,region:full,vertices,offset:0,indices,first,count,base_vertex
    };
    let epoch = s.submit(&[
        Command::Copy { src:upload,src_offset:0,dst:vertices,dst_offset:0,bytes:data.len() as u64*4 },
        Command::ClearAttachments { color,depth,region:full,rgba:[0.;4],z:1. },
        draw(3,6,-1),
        Command::ReadImage { src:color,region:full,dst:output,offset:0 },
        Command::ReadImage { src:depth,region:full,dst:output,offset:256 },
    ])?;
    let mut expected = vec![u32::from_le_bytes([0,255,0,255]);64];
    expected.extend([0.375f32.to_bits();64]);
    if s.read_words(output,0,128)? != expected { return Err("indexed quad color/depth mismatch".into()); }
    println!("PASS: indexed quad, 4 vertices, 6 indices, first index 3 and base vertex -1 at timeline {epoch}");
    for invalid in [draw(6,6,-1),draw(3,6,0),draw(3,6,-2),draw(3,5,-1)] {
        if s.submit(&[Command::ClearImage { dst:color,rgba:[0.;4] },invalid]).is_ok() {
            return Err("accepted invalid indexed draw".into());
        }
    }
    s.submit(&[Command::ReadImage { src:color,region:full,dst:output,offset:0 }])?;
    if s.read_words(output,0,64)? != expected[..64] { return Err("invalid indexed list modified color".into()); }
    s.destroy_index_buffer(indices)?;
    if s.submit(&[draw(3,6,-1)]).is_ok() { return Err("accepted retired index buffer".into()); }
    println!("PASS: invalid index ranges, base vertices and retired index buffer rejected");
    println!("PASS: reusable Vulkan indexed draw executor verified");
    Ok(())
}

pub(crate) fn verify_dispatch(context:&mut Context,path:&std::path::Path) -> Result<()> {
    let words = ash::util::read_spv(&mut std::io::Cursor::new(std::fs::read(path)?))?;
    let mut s = Session::new(context)?;
    const N:usize = 1024;
    const BYTES:u64 = N as u64*4;
    let p = s.create_compute_pipeline(&words)?;
    let a = s.create_buffer(BYTES,MemoryKind::DeviceLocal)?;
    let b = s.create_buffer(BYTES,MemoryKind::DeviceLocal)?;
    let output = s.create_buffer(BYTES*2,MemoryKind::HostVisible)?;
    let first = s.create_compute_binding(p,a,0,BYTES)?;
    let second = s.create_compute_binding(p,b,0,BYTES)?;
    let dispatch = |binding,groups,constants| Command::Dispatch { binding,groups,constants };
    let epoch = s.submit(&[
        Command::Fill { dst:a,offset:0,bytes:BYTES,value:3 },
        Command::Fill { dst:b,offset:0,bytes:BYTES,value:7 },
        dispatch(first,16,[5,11]),
        dispatch(second,16,[2,19]),
        dispatch(first,8,[3,1]),
        Command::Copy { src:a,src_offset:0,dst:output,dst_offset:0,bytes:BYTES },
        Command::Copy { src:b,src_offset:0,dst:output,dst_offset:BYTES,bytes:BYTES },
    ])?;
    let mut expected = Vec::with_capacity(2*N);
    for i in 0..N as u32 {
        let value = 3u32*5+11+i;
        expected.push(if i<512 { value*3+1+i } else { value });
    }
    expected.extend((0..N as u32).map(|i| 7*2+19+i));
    if s.read_words(output,0,2*N)? != expected { return Err("dispatch output mismatch".into()); }
    println!("PASS: three dispatches, two immutable descriptor sets, one compute pipeline at timeline {epoch}");
    if s.destroy_buffer(a).is_ok() || s.destroy_compute_pipeline(p).is_ok() {
        return Err("destroyed retained compute resource".into());
    }
    if s.submit(&[Command::Fill { dst:a,offset:0,bytes:BYTES,value:0 },dispatch(first,17,[1,0])]).is_ok() {
        return Err("accepted out-of-range dispatch".into());
    }
    s.submit(&[Command::Copy { src:a,src_offset:0,dst:output,dst_offset:0,bytes:BYTES }])?;
    if s.read_words(output,0,N)? != expected[..N] { return Err("invalid list modified compute output".into()); }
    s.destroy_compute_binding(first)?;
    if s.submit(&[dispatch(first,1,[1,0])]).is_ok() { return Err("accepted retired compute binding".into()); }
    s.destroy_compute_binding(second)?;
    s.destroy_compute_pipeline(p)?;
    s.destroy_buffer(a)?;
    s.destroy_buffer(b)?;
    println!("PASS: dispatch bounds and compute resource lifetime validation");
    println!("PASS: reusable Vulkan compute pipeline and dispatch executor verified");
    Ok(())
}

/// End-to-end native instruction decoding through the reusable session.
/// These original fixtures deliberately exercise only the supported diagnostic
/// ABI; the reference functions do not depend on generated SPIR-V.
pub(crate) fn verify_native_dispatch(context:&mut Context) -> Result<()> {
    use vortex_shader::ir::Reg;
    const N:usize = 1_048_576;
    const BYTES:u64 = N as u64*4;
    let mut s = Session::new(context)?;
    let local = s.create_buffer(BYTES,MemoryKind::DeviceLocal)?;
    let output = s.create_buffer(BYTES,MemoryKind::HostVisible)?;
    let scalar = [0xbe80_0385u32,0xbe81_0387,0x8002_0100,0x8903_8302,0xbf81_0000];
    let vector = [0x7e02_0300u32,0x4a02_0287,0x3a04_0283,0x3404_0481,0x4c04_04c0,0xbf81_0000];
    let encode = |words:&[u32]| -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() };
    let scalar_code = encode(&scalar);
    let vector_code = encode(&vector);
    for (label,code,wave64,vgprs,reg) in [
        ("scalar",scalar_code.as_slice(),false,0,Reg::S(3)),
        ("vector32 logical lanes",vector_code.as_slice(),false,3,Reg::V(2)),
        ("vector64 logical lanes",vector_code.as_slice(),true,3,Reg::V(2)),
    ] {
        let started = std::time::Instant::now();
        let p = s.create_native_compute_pipeline(code,wave64,vgprs,reg)?;
        let compile_ms = started.elapsed().as_secs_f64()*1000.;
        let binding = s.create_compute_binding(p,local,0,BYTES)?;
        let epoch = s.submit(&[
            Command::Fill { dst:local,offset:0,bytes:BYTES,value:0xdeadbeef },
            Command::Dispatch { binding,groups:N as u32/COMPUTE_LANES,constants:[0,0] },
            Command::Copy { src:local,src_offset:0,dst:output,dst_offset:0,bytes:BYTES },
        ])?;
        let actual = s.read_words(output,0,N)?;
        for (i,&v) in actual.iter().enumerate() {
            let i = i as u32;
            let expected = match reg {
                Reg::S(3) => 15,
                _ if if wave64 { (i&1==0)==(i&32==0) } else { i&1==0 } =>
                    64u32.wrapping_sub(((i+7)^3)<<1),
                _ => 0xcafe_babe,
            };
            if v != expected { return Err(format!("{label}: mismatch at {i}: {v:#x} != {expected:#x}").into()); }
        }
        println!("PASS: {label}: decoded GFX10 -> SSA -> SPIR-V -> Vulkan Dispatch; {N} words; pipeline {:.3} ms; timeline {epoch}",compile_ms);
        s.destroy_compute_binding(binding)?;
        s.destroy_compute_pipeline(p)?;
    }
    // Live scalar input is outside the closed scalar diagnostic ABI.
    let unsupported = encode(&[0xbe80_0301,0xbf81_0000]);
    if s.create_native_compute_pipeline(&unsupported,false,0,Reg::S(0)).is_ok() {
        return Err("unsupported native shader was accepted".into());
    }
    println!("PASS: unsupported native shader rejected before pipeline creation");
    println!("PASS: native GFX10 diagnostic compiler integrated with Vulkan command executor");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatch_validation() {
        assert!(dispatch_bytes(16,65535,4096).is_ok());
        assert!(dispatch_bytes(17,65535,4096).is_err());
        assert!(dispatch_bytes(16,15,4096).is_err());
        assert!(dispatch_bytes(0,65535,4096).is_err());
    }

    #[test]
    fn indexed_vertex_validation() {
        let indices = [0,0,0,1,2,3,1,3,4];
        assert_eq!(indexed_vertex_bytes(&indices,3,6,-1).unwrap(),4*VERTEX_BYTES);
        assert!(indexed_vertex_bytes(&indices,3,6,-2).is_err());
        assert!(indexed_vertex_bytes(&indices,6,6,-1).is_err());
        assert!(indexed_vertex_bytes(&indices,u32::MAX,3,0).is_err());
        assert!(indexed_vertex_bytes(&indices,0,0,0).is_err());
    }

    #[test]
    fn image_region_validation() {
        let r = ImageRegion { x:2,y:1,width:3,height:2 };
        assert_eq!(region_bytes(8,8,r).unwrap(),24);
        assert!(region_bytes(8,8,ImageRegion { x:7,..r }).is_err());
        assert!(region_bytes(8,8,ImageRegion { width:0,..r }).is_err());
        assert!(region_bytes(u32::MAX,8,ImageRegion { x:u32::MAX,..r }).is_err());
    }

    #[test]
    fn range_validation() {
        assert!(check_range(64,60,4).is_ok());
        for (offset,bytes) in [(0,0),(2,4),(0,3),(60,8),(u64::MAX-3,8)] {
            assert!(check_range(64,offset,bytes).is_err());
        }
    }
}
