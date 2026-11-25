// SPDX-License-Identifier: GPL-3.0-or-later
use ash::vk;
use std::{ffi::CStr, io::Cursor, path::Path, time::Instant};

const W: u32 = 512;
const H: u32 = 512;
const BYTES: u64 = W as u64 * H as u64 * 4;
const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

#[derive(Default)]
struct Handles {
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    view: vk::ImageView,
    buffer: vk::Buffer,
    buffer_memory: vk::DeviceMemory,
    mapped: bool,
    vert: vk::ShaderModule,
    frag: vk::ShaderModule,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    commands: vk::CommandPool,
    fence: vk::Fence,
}

struct Resources<'a> {
    d: &'a ash::Device,
    h: Handles,
}

impl Drop for Resources<'_> {
    fn drop(&mut self) {
        let d = self.d;
        let h = &self.h;
        unsafe {
            let _ = d.device_wait_idle();
            if h.mapped { d.unmap_memory(h.buffer_memory); }
            d.destroy_fence(h.fence, None);
            d.destroy_command_pool(h.commands, None);
            d.destroy_pipeline(h.pipeline, None);
            d.destroy_pipeline_layout(h.layout, None);
            d.destroy_shader_module(h.vert, None);
            d.destroy_shader_module(h.frag, None);
            d.destroy_image_view(h.view, None);
            d.destroy_image(h.image, None);
            d.free_memory(h.image_memory, None);
            d.destroy_buffer(h.buffer, None);
            d.free_memory(h.buffer_memory, None);
        }
    }
}

fn memory_type(mem: &vk::PhysicalDeviceMemoryProperties, bits: u32,
               required: vk::MemoryPropertyFlags, preferred: vk::MemoryPropertyFlags) -> crate::Result<u32> {
    use vk::MemoryPropertyFlags as F;
    let excluded = F::DEVICE_COHERENT_AMD | F::DEVICE_UNCACHED_AMD;
    (0..mem.memory_type_count)
        .filter(|&i| bits & (1u32 << i) != 0)
        .filter(|&i| {
            let f = mem.memory_types[i as usize].property_flags;
            f.contains(required) && !f.intersects(excluded)
        })
        .max_by_key(|&i| (mem.memory_types[i as usize].property_flags & preferred).as_raw().count_ones())
        .ok_or_else(|| "No compatible graphics memory type".into())
}

pub fn run(instance: &ash::Instance, physical: vk::PhysicalDevice, d: &ash::Device,
           family: u32, vert_path: &Path, frag_path: &Path) -> crate::Result<()> {
    let draws: u32 = std::env::var("AMD_VK_DRAWS").unwrap_or_else(|_| "1".to_owned()).parse()?;
    if !(1..=4096).contains(&draws) { return Err("AMD_VK_DRAWS must be 1..=4096".into()); }
    let mode = std::env::var("AMD_VK_DRAW_MODE").unwrap_or_else(|_| "direct".to_owned());
    if !matches!(mode.as_str(), "direct" | "instanced") { return Err("Unknown AMD_VK_DRAW_MODE".into()); }
    println!("draw mode: {mode}; triangles: {draws}");
    let vert_words = ash::util::read_spv(&mut Cursor::new(std::fs::read(vert_path)?))?;
    let frag_words = ash::util::read_spv(&mut Cursor::new(std::fs::read(frag_path)?))?;
    let mut resources = Resources { d, h: Handles::default() };
    let h = &mut resources.h;
    let mem = unsafe { instance.get_physical_device_memory_properties(physical) };
    let format = unsafe { instance.get_physical_device_format_properties(physical, FORMAT) };
    if !format.optimal_tiling_features.contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::TRANSFER_SRC) {
        return Err("RGBA8 render target unsupported".into());
    }
    let extent = vk::Extent2D { width: W, height: H };
    let area = vk::Rect2D { offset: vk::Offset2D::default(), extent };
    let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1).layer_count(1);

    unsafe {
        h.image = d.create_image(&vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D).format(FORMAT)
            .extent(vk::Extent3D { width: W, height: H, depth: 1 })
            .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE).initial_layout(vk::ImageLayout::UNDEFINED), None)?;
        let req = d.get_image_memory_requirements(h.image);
        let index = memory_type(&mem, req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        h.image_memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
            .allocation_size(req.size).memory_type_index(index), None)?;
        d.bind_image_memory(h.image, h.image_memory, 0)?;
        h.view = d.create_image_view(&vk::ImageViewCreateInfo::default().image(h.image)
            .view_type(vk::ImageViewType::TYPE_2D).format(FORMAT).subresource_range(range), None)?;

        h.buffer = d.create_buffer(&vk::BufferCreateInfo::default().size(BYTES)
            .usage(vk::BufferUsageFlags::TRANSFER_DST).sharing_mode(vk::SharingMode::EXCLUSIVE), None)?;
        let req = d.get_buffer_memory_requirements(h.buffer);
        let index = memory_type(&mem, req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            vk::MemoryPropertyFlags::HOST_CACHED)?;
        h.buffer_memory = d.allocate_memory(&vk::MemoryAllocateInfo::default()
            .allocation_size(req.size).memory_type_index(index), None)?;
        d.bind_buffer_memory(h.buffer, h.buffer_memory, 0)?;

        h.vert = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vert_words), None)?;
        h.frag = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&frag_words), None)?;
        let entry = CStr::from_bytes_with_nul(b"main\0")?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(h.vert).name(entry),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(h.frag).name(entry),
        ];
        h.layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None)?;
        let vertex = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewports = [vk::Viewport { x: 0., y: 0., width: W as f32, height: H as f32, min_depth: 0., max_depth: 1. }];
        let scissors = [area];
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewports(&viewports).scissors(&scissors);
        let raster = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE).front_face(vk::FrontFace::COUNTER_CLOCKWISE).line_width(1.);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments = [vk::PipelineColorBlendAttachmentState::default().color_write_mask(
            vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B | vk::ColorComponentFlags::A)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let formats = [FORMAT];
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let pipeline = vk::GraphicsPipelineCreateInfo::default().stages(&stages)
            .vertex_input_state(&vertex).input_assembly_state(&assembly)
            .viewport_state(&viewport).rasterization_state(&raster)
            .multisample_state(&multisample).color_blend_state(&blend)
            .layout(h.layout).push_next(&mut rendering);
        let props = instance.get_physical_device_properties(physical);
        let cache = crate::pipeline_cache::Cache::open(d, &props);
        let pipeline_start = Instant::now();
        h.pipeline = match d.create_graphics_pipelines(cache.handle(), &[pipeline], None) {
            Ok(pipelines) => pipelines[0],
            Err((pipelines, err)) => {
                for p in pipelines { d.destroy_pipeline(p, None); }
                return Err(err.into());
            }
        };

        println!("graphics pipeline creation: {:.3} ms", pipeline_start.elapsed().as_secs_f64() * 1000.);
        cache.save();
        h.commands = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family), None)?;
        let cmd = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
            .command_pool(h.commands).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        h.fence = d.create_fence(&vk::FenceCreateInfo::default(), None)?;
        let record_start = Instant::now();
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        let to_color = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED).new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(h.image).subresource_range(range)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&to_color));

        let colors = [vk::RenderingAttachmentInfo::default().image_view(h.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue { color: vk::ClearColorValue { float32: [0., 0., 0., 1.] } })];
        d.cmd_begin_rendering(cmd, &vk::RenderingInfo::default().render_area(area).layer_count(1).color_attachments(&colors));
        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, h.pipeline);
        let draw_start = Instant::now();
        if mode == "instanced" {
            d.cmd_draw(cmd, 3, draws, 0, 0);
        } else {
            for _ in 0..draws { d.cmd_draw(cmd, 3, 1, 0, 0); }
        }
        let draw_record_ms = draw_start.elapsed().as_secs_f64() * 1000.;
        d.cmd_end_rendering(cmd);

        let to_copy = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY).dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL).new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(h.image).subresource_range(range)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&to_copy));
        let copy = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
            .image_extent(vk::Extent3D { width: W, height: H, depth: 1 })];
        d.cmd_copy_image_to_buffer(cmd, h.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, h.buffer, &copy);
        let host = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY).src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST).dst_access_mask(vk::AccessFlags2::HOST_READ)];
        d.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().memory_barriers(&host));
        d.end_command_buffer(cmd)?;
        println!("draw recording: {:.3} ms", draw_record_ms);
        println!("total command recording: {:.3} ms", record_start.elapsed().as_secs_f64() * 1000.);
        let commands = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&commands)];
        let queue = d.get_device_queue(family, 0);
        let start = Instant::now();
        d.queue_submit2(queue, &submit, h.fence)?;
        d.wait_for_fences(&[h.fence], true, u64::MAX)?;
        println!("graphics submit + wait: {:.3} ms", start.elapsed().as_secs_f64() * 1000.);
        let ptr = d.map_memory(h.buffer_memory, 0, BYTES, vk::MemoryMapFlags::empty())?;
        h.mapped = true;
        let pixels = std::slice::from_raw_parts(ptr.cast::<u8>(), BYTES as usize);
        for (i, pixel) in pixels.chunks_exact(4).enumerate() {
            if pixel != [255, 0, 255, 255] {
                return Err(format!("Pixel mismatch at {i}: {pixel:?}").into());
            }
        }
        println!("PASS: verified {} pixels ({}x{}) via Vulkan dynamic rendering", W * H, W, H);
    }
    Ok(())
}
