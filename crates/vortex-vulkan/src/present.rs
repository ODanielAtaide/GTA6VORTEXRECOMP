// SPDX-License-Identifier: GPL-3.0-or-later
//! Synchronous Win32 Vulkan presentation for the initial live backend.
//! A source must be an actual guest-produced RGBA8 image, never a fixture.
use ash::vk;
use crate::{Context,Result};

pub(crate) struct Presenter<'a> {
    context:&'a Context,
    surface_api:ash::khr::surface::Instance,
    swap_api:ash::khr::swapchain::Device,
    surface:vk::SurfaceKHR,
    swap:vk::SwapchainKHR,
    images:Vec<vk::Image>,
    rendered:Vec<vk::Semaphore>,
    acquired:vk::Semaphore,
    pool:vk::CommandPool,
    cmd:vk::CommandBuffer,
    extent:vk::Extent2D,
    queue:vk::Queue,
    hwnd:usize,
}
impl Drop for Presenter<'_>{
    fn drop(&mut self){
        let d=&self.context.device;
        unsafe{
            let _=d.device_wait_idle();
            for sem in self.rendered.drain(..){d.destroy_semaphore(sem,None);}
            d.destroy_semaphore(self.acquired,None);
            d.destroy_command_pool(self.pool,None);
            self.swap_api.destroy_swapchain(self.swap,None);
            self.surface_api.destroy_surface(self.surface,None);
        }
    }
}
impl<'a> Presenter<'a>{
    pub(crate) fn new(c:&'a Context,hwnd:usize,hinstance:usize,width:u32,height:u32)->Result<Self>{
        if hwnd==0||hinstance==0{return Err("invalid Win32 surface handles".into());}
        let win=ash::khr::win32_surface::Instance::new(&c._entry,&c.instance);
        let mut p=Self{context:c,surface_api:ash::khr::surface::Instance::new(&c._entry,&c.instance),
            swap_api:ash::khr::swapchain::Device::new(&c.instance,&c.device),
            surface:vk::SurfaceKHR::null(),swap:vk::SwapchainKHR::null(),images:vec![],rendered:vec![],
            acquired:vk::Semaphore::null(),pool:vk::CommandPool::null(),cmd:vk::CommandBuffer::null(),
            extent:vk::Extent2D::default(),queue:unsafe{c.device.get_device_queue(c.family,0)},hwnd};
        unsafe{
            p.surface=win.create_win32_surface(&vk::Win32SurfaceCreateInfoKHR::default()
                .hwnd(hwnd as isize).hinstance(hinstance as isize),None)?;
            if !p.surface_api.get_physical_device_surface_support(c.physical,c.family,p.surface)?{
                return Err("selected AMD graphics queue cannot present to Win32 surface".into());
            }
            let caps=p.surface_api.get_physical_device_surface_capabilities(c.physical,p.surface)?;
            if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST){
                return Err("surface cannot receive Vulkan transfer presentation".into());
            }
            let formats=p.surface_api.get_physical_device_surface_formats(c.physical,p.surface)?;
            let chosen=[vk::Format::R8G8B8A8_UNORM,vk::Format::B8G8R8A8_UNORM].into_iter()
                .find_map(|format| {
                    let supported=formats.iter().any(|f|(f.format==format||f.format==vk::Format::UNDEFINED)&&f.color_space==vk::ColorSpaceKHR::SRGB_NONLINEAR);
                    let props=c.instance.get_physical_device_format_properties(c.physical,format);
                    (supported&&props.optimal_tiling_features.contains(vk::FormatFeatureFlags::BLIT_DST)).then_some(format)
                }).ok_or("no compatible UNORM Vulkan swapchain format")?;
            let src=c.instance.get_physical_device_format_properties(c.physical,vk::Format::R8G8B8A8_UNORM);
            if !src.optimal_tiling_features.contains(vk::FormatFeatureFlags::BLIT_SRC){return Err("RGBA8 blit unsupported".into());}
            p.extent=if caps.current_extent.width!=u32::MAX{caps.current_extent}else{
                vk::Extent2D{width:width.clamp(caps.min_image_extent.width,caps.max_image_extent.width),
                    height:height.clamp(caps.min_image_extent.height,caps.max_image_extent.height)}
            };
            if p.extent.width==0||p.extent.height==0||p.extent.width>i32::MAX as u32||p.extent.height>i32::MAX as u32{
                return Err("surface unavailable or minimized".into());
            }
            let alpha=[vk::CompositeAlphaFlagsKHR::OPAQUE,vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
                vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,vk::CompositeAlphaFlagsKHR::INHERIT].into_iter()
                .find(|&a|caps.supported_composite_alpha.contains(a)).ok_or("no composite alpha")?;
            let mut n=caps.min_image_count.saturating_add(1);
            if caps.max_image_count!=0{n=n.min(caps.max_image_count);}
            p.swap=p.swap_api.create_swapchain(&vk::SwapchainCreateInfoKHR::default()
                .surface(p.surface).min_image_count(n).image_format(chosen).image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
                .image_extent(p.extent).image_array_layers(1).image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE).pre_transform(caps.current_transform)
                .composite_alpha(alpha).present_mode(vk::PresentModeKHR::FIFO).clipped(true),None)?;
            p.images=p.swap_api.get_swapchain_images(p.swap)?;
            for _ in &p.images{p.rendered.push(c.device.create_semaphore(&vk::SemaphoreCreateInfo::default(),None)?);}
            p.acquired=c.device.create_semaphore(&vk::SemaphoreCreateInfo::default(),None)?;
            p.pool=c.device.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(c.family),None)?;
            p.cmd=c.device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                .command_pool(p.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1))?[0];
        }
        Ok(p)
    }
    pub(crate) fn hwnd(&self)->usize{self.hwnd}
    pub(crate) fn present(&mut self,source:vk::Image,width:u32,height:u32)->Result<()>{
        let d=&self.context.device;
        unsafe{
            let(index,_)=self.swap_api.acquire_next_image(self.swap,u64::MAX,self.acquired,vk::Fence::null())?;
            let image=self.images[index as usize];let done=self.rendered[index as usize];
            d.reset_command_pool(self.pool,vk::CommandPoolResetFlags::empty())?;
            d.begin_command_buffer(self.cmd,&vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
            let range=vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
            let memory=[vk::MemoryBarrier2::default().src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE).dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
            let image_barrier=[vk::ImageMemoryBarrier2::default().image(image).subresource_range(range)
                .old_layout(vk::ImageLayout::UNDEFINED).new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .src_stage_mask(vk::PipelineStageFlags2::NONE).dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)];
            d.cmd_pipeline_barrier2(self.cmd,&vk::DependencyInfo::default().memory_barriers(&memory).image_memory_barriers(&image_barrier));
            let layers=vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
            let blit=vk::ImageBlit::default().src_subresource(layers).dst_subresource(layers)
                .src_offsets([vk::Offset3D::default(),vk::Offset3D{x:width as i32,y:height as i32,z:1}])
                .dst_offsets([vk::Offset3D::default(),vk::Offset3D{x:self.extent.width as i32,y:self.extent.height as i32,z:1}]);
            d.cmd_blit_image(self.cmd,source,vk::ImageLayout::GENERAL,image,vk::ImageLayout::TRANSFER_DST_OPTIMAL,&[blit],vk::Filter::NEAREST);
            let ready=[vk::ImageMemoryBarrier2::default().image(image).subresource_range(range)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL).new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER).src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::NONE)];
            d.cmd_pipeline_barrier2(self.cmd,&vk::DependencyInfo::default().image_memory_barriers(&ready));
            d.end_command_buffer(self.cmd)?;
            let waits=[vk::SemaphoreSubmitInfo::default().semaphore(self.acquired).stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)];
            let signals=[vk::SemaphoreSubmitInfo::default().semaphore(done).stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let cmds=[vk::CommandBufferSubmitInfo::default().command_buffer(self.cmd)];
            d.queue_submit2(self.queue,&[vk::SubmitInfo2::default().wait_semaphore_infos(&waits)
                .command_buffer_infos(&cmds).signal_semaphore_infos(&signals)],vk::Fence::null())?;
            let swaps=[self.swap];let indices=[index];let wait=[done];
            let result=self.swap_api.queue_present(self.queue,&vk::PresentInfoKHR::default()
                .wait_semaphores(&wait).swapchains(&swaps).image_indices(&indices));
            d.queue_wait_idle(self.queue)?;
            result?;
        }
        Ok(())
    }
}
