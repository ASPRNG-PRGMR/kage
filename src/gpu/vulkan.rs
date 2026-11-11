//! Vulkan backend: the renderers as a GLSL compute shader.
//!
//! `shaders/render.comp` is compiled to SPIR-V when the backend starts (by
//! `naga`, so no external shader compiler is needed) and run as a compute
//! pipeline. The two coverage planes, the parameter block and the result
//! live in host-visible buffers: inputs are written straight into mapped
//! memory, one dispatch covers the glyph in 8×8 groups, and the result is
//! read straight back. No images, no swapchain, no window.

use std::ffi::CStr;

use ash::vk;

use super::{shaders, GpuRenderer, RenderJob, ShaderParams};
use crate::subpixel::SubpixelGrid;

/// Side of the compute shader's work group (`local_size_x/y` in the shader).
const GROUP: u32 = 8;

/// A host-visible buffer, kept mapped for its whole life.
struct MappedBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    size: usize,
}

/// A Vulkan device with the KAGE compute pipeline.
pub struct VulkanRenderer {
    // `_entry` keeps the Vulkan library loaded while the handles below live.
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    memory_types: vk::PhysicalDeviceMemoryProperties,
    shader: vk::ShaderModule,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Coverage (pixel), coverage (supersampled), result, parameters — in
    /// binding order. Recreated when a job needs more room.
    buffers: Option<[MappedBuffer; 4]>,
    name: String,
    failed: bool,
}

/// Compile the compute shader to SPIR-V.
fn compile_shader() -> Result<Vec<u32>, String> {
    use naga::back::spv;
    let source = shaders::compute();
    let module = naga::front::glsl::Frontend::default()
        .parse(&naga::front::glsl::Options::from(naga::ShaderStage::Compute), &source)
        .map_err(|e| format!("compute shader failed to parse: {}", e.emit_to_string(&source)))?;
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| format!("compute shader failed validation: {}", e.emit_to_string(&source)))?;
    let pipeline = spv::PipelineOptions { shader_stage: naga::ShaderStage::Compute, entry_point: "main".into() };
    spv::write_vec(&module, &info, &spv::Options::default(), Some(&pipeline))
        .map_err(|e| format!("compute shader failed to compile to SPIR-V: {e}"))
}

/// Preference order for devices: a real GPU before a software one, and an
/// integrated GPU first — for work this small, waking a discrete GPU costs
/// more than it saves.
fn device_rank(kind: vk::PhysicalDeviceType) -> u32 {
    match kind {
        vk::PhysicalDeviceType::INTEGRATED_GPU => 0,
        vk::PhysicalDeviceType::DISCRETE_GPU => 1,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
        vk::PhysicalDeviceType::CPU => 3,
        _ => 4,
    }
}

impl VulkanRenderer {
    /// Open a device and build the compute pipeline.
    pub fn new() -> Result<Self, String> {
        let spirv = compile_shader()?;
        let vk_err = |what: &str| {
            let what = what.to_owned();
            move |e: vk::Result| format!("{what}: {e}")
        };

        // SAFETY: standard Vulkan object creation; every handle is destroyed
        // in `Drop` (or on the error paths below) in reverse order.
        unsafe {
            let entry = ash::Entry::load().map_err(|e| format!("cannot load the Vulkan library: {e}"))?;
            let app = vk::ApplicationInfo::default()
                .application_name(c"kage")
                .api_version(vk::API_VERSION_1_1);
            let instance = entry
                .create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)
                .map_err(vk_err("vkCreateInstance"))?;

            // Pick the best device that has a compute queue.
            let candidates = match instance.enumerate_physical_devices() {
                Ok(list) => list,
                Err(e) => {
                    instance.destroy_instance(None);
                    return Err(format!("vkEnumeratePhysicalDevices: {e}"));
                }
            };
            let chosen = candidates
                .into_iter()
                .filter_map(|physical| {
                    let family = instance
                        .get_physical_device_queue_family_properties(physical)
                        .iter()
                        .position(|q| q.queue_flags.contains(vk::QueueFlags::COMPUTE))?;
                    let properties = instance.get_physical_device_properties(physical);
                    Some((device_rank(properties.device_type), physical, family as u32, properties))
                })
                .min_by_key(|candidate| candidate.0);
            let Some((_, physical, family, properties)) = chosen else {
                instance.destroy_instance(None);
                return Err("no Vulkan device with a compute queue".into());
            };
            let name = format!(
                "{} (Vulkan {}.{})",
                properties.device_name_as_c_str().unwrap_or(c"unknown device").to_string_lossy(),
                vk::api_version_major(properties.api_version),
                vk::api_version_minor(properties.api_version)
            );
            let memory_types = instance.get_physical_device_memory_properties(physical);

            let priorities = [1.0f32];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
            let device = match instance.create_device(physical, &vk::DeviceCreateInfo::default().queue_create_infos(&queues), None) {
                Ok(device) => device,
                Err(e) => {
                    instance.destroy_instance(None);
                    return Err(format!("vkCreateDevice: {e}"));
                }
            };
            let queue = device.get_device_queue(family, 0);

            // From here on, build a partially-initialised renderer so that a
            // failure tears down whatever exists through `Drop`.
            let mut renderer = Self {
                _entry: entry,
                instance,
                device,
                queue,
                memory_types,
                shader: vk::ShaderModule::null(),
                set_layout: vk::DescriptorSetLayout::null(),
                pipeline_layout: vk::PipelineLayout::null(),
                pipeline: vk::Pipeline::null(),
                descriptor_pool: vk::DescriptorPool::null(),
                descriptor_set: vk::DescriptorSet::null(),
                command_pool: vk::CommandPool::null(),
                command_buffer: vk::CommandBuffer::null(),
                fence: vk::Fence::null(),
                buffers: None,
                name,
                failed: false,
            };
            let device = &renderer.device;

            renderer.shader = device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&spirv), None)
                .map_err(vk_err("vkCreateShaderModule"))?;

            // Bindings 0–2: storage buffers; binding 3: the parameter block.
            let binding = |index: u32, kind: vk::DescriptorType| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(index)
                    .descriptor_type(kind)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            };
            let bindings = [
                binding(0, vk::DescriptorType::STORAGE_BUFFER),
                binding(1, vk::DescriptorType::STORAGE_BUFFER),
                binding(2, vk::DescriptorType::STORAGE_BUFFER),
                binding(3, vk::DescriptorType::UNIFORM_BUFFER),
            ];
            renderer.set_layout = device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
                .map_err(vk_err("vkCreateDescriptorSetLayout"))?;
            let set_layouts = [renderer.set_layout];
            renderer.pipeline_layout = device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts), None)
                .map_err(vk_err("vkCreatePipelineLayout"))?;

            let entry_point: &CStr = c"main";
            let stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(renderer.shader)
                .name(entry_point);
            let pipeline_info = [vk::ComputePipelineCreateInfo::default().stage(stage).layout(renderer.pipeline_layout)];
            renderer.pipeline = device
                .create_compute_pipelines(vk::PipelineCache::null(), &pipeline_info, None)
                .map_err(|(_, e)| format!("vkCreateComputePipelines: {e}"))?[0];

            let pool_sizes = [
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(3),
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::UNIFORM_BUFFER).descriptor_count(1),
            ];
            renderer.descriptor_pool = device
                .create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&pool_sizes), None)
                .map_err(vk_err("vkCreateDescriptorPool"))?;
            renderer.descriptor_set = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default().descriptor_pool(renderer.descriptor_pool).set_layouts(&set_layouts),
                )
                .map_err(vk_err("vkAllocateDescriptorSets"))?[0];

            renderer.command_pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(vk_err("vkCreateCommandPool"))?;
            renderer.command_buffer = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(renderer.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(vk_err("vkAllocateCommandBuffers"))?[0];
            renderer.fence = device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(vk_err("vkCreateFence"))?;

            Ok(renderer)
        }
    }

    /// Create a host-visible, mapped buffer of at least `size` bytes.
    unsafe fn create_buffer(&self, size: usize, usage: vk::BufferUsageFlags) -> Result<MappedBuffer, String> {
        let device = &self.device;
        let info = vk::BufferCreateInfo::default()
            .size(size.max(16) as u64)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = device.create_buffer(&info, None).map_err(|e| format!("vkCreateBuffer: {e}"))?;
        let requirements = device.get_buffer_memory_requirements(buffer);
        let wanted = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory_type = (0..self.memory_types.memory_type_count).find(|&i| {
            requirements.memory_type_bits & (1 << i) != 0
                && self.memory_types.memory_types[i as usize].property_flags.contains(wanted)
        });
        let Some(memory_type) = memory_type else {
            device.destroy_buffer(buffer, None);
            return Err("no host-visible memory type on this device".into());
        };
        let allocation = vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(memory_type);
        let memory = match device.allocate_memory(&allocation, None) {
            Ok(memory) => memory,
            Err(e) => {
                device.destroy_buffer(buffer, None);
                return Err(format!("vkAllocateMemory: {e}"));
            }
        };
        let mapped = device
            .bind_buffer_memory(buffer, memory, 0)
            .and_then(|()| device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()));
        match mapped {
            Ok(ptr) => Ok(MappedBuffer { buffer, memory, ptr: ptr.cast(), size: size.max(16) }),
            Err(e) => {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
                Err(format!("mapping buffer memory: {e}"))
            }
        }
    }

    unsafe fn destroy_buffers(&mut self) {
        if let Some(buffers) = self.buffers.take() {
            for b in buffers {
                self.device.destroy_buffer(b.buffer, None);
                self.device.free_memory(b.memory, None);
            }
        }
    }

    /// Make sure the four buffers can hold a job of these byte sizes, and
    /// point the descriptor set at them.
    unsafe fn ensure_buffers(&mut self, sizes: [usize; 4]) -> Result<(), String> {
        if let Some(existing) = &self.buffers {
            if existing.iter().zip(sizes).all(|(b, need)| b.size >= need) {
                return Ok(());
            }
        }
        self.destroy_buffers();
        let usages = [
            vk::BufferUsageFlags::STORAGE_BUFFER,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
        ];
        let mut created: Vec<MappedBuffer> = Vec::with_capacity(4);
        for (size, usage) in sizes.into_iter().zip(usages) {
            match self.create_buffer(size, usage) {
                Ok(buffer) => created.push(buffer),
                Err(e) => {
                    for b in created {
                        self.device.destroy_buffer(b.buffer, None);
                        self.device.free_memory(b.memory, None);
                    }
                    return Err(e);
                }
            }
        }
        let buffers: [MappedBuffer; 4] = created.try_into().unwrap_or_else(|_| unreachable!("four buffers were created"));

        let infos: Vec<[vk::DescriptorBufferInfo; 1]> = buffers
            .iter()
            .map(|b| [vk::DescriptorBufferInfo::default().buffer(b.buffer).offset(0).range(vk::WHOLE_SIZE)])
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(self.descriptor_set)
                    .dst_binding(i as u32)
                    .descriptor_type(if i == 3 { vk::DescriptorType::UNIFORM_BUFFER } else { vk::DescriptorType::STORAGE_BUFFER })
                    .buffer_info(info)
            })
            .collect();
        self.device.update_descriptor_sets(&writes, &[]);
        self.buffers = Some(buffers);
        Ok(())
    }
}

impl GpuRenderer for VulkanRenderer {
    fn render(&mut self, job: &RenderJob) -> Result<SubpixelGrid, String> {
        if self.failed { return Err("Vulkan renderer is unavailable after a submission failure".into()); }
        let (w, h) = job.size();
        if w == 0 || h == 0 {
            return Ok(SubpixelGrid::new(w, h));
        }
        let cov = job.coverage;
        let float = std::mem::size_of::<f32>();
        let result_len = (w * h * 4) as usize;
        let sizes = [
            cov.lo.data.len() * float,
            cov.hi.data.len() * float,
            result_len * float,
            std::mem::size_of::<ShaderParams>(),
        ];
        let vk_err = |what: &'static str| move |e: vk::Result| format!("{what}: {e}");

        // SAFETY: buffers are at least `sizes` bytes (ensure_buffers) and
        // stay mapped; the GPU is idle whenever the host touches them,
        // because every submission is waited for before returning.
        unsafe {
            self.ensure_buffers(sizes)?;
            let buffers = self.buffers.as_ref().expect("ensure_buffers succeeded");
            std::ptr::copy_nonoverlapping(cov.lo.data.as_ptr().cast::<u8>(), buffers[0].ptr, sizes[0]);
            std::ptr::copy_nonoverlapping(cov.hi.data.as_ptr().cast::<u8>(), buffers[1].ptr, sizes[1]);
            std::ptr::copy_nonoverlapping(job.params.as_bytes().as_ptr(), buffers[3].ptr, sizes[3]);

            let device = &self.device;
            let cmd = self.command_buffer;
            device
                .begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
                .map_err(vk_err("vkBeginCommandBuffer"))?;
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            device.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline_layout, 0, &[self.descriptor_set], &[]);
            device.cmd_dispatch(cmd, w.div_ceil(GROUP), h.div_ceil(GROUP), 1);
            // Make the shader's writes visible to the host read below.
            let barrier = [vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)];
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &barrier,
                &[],
                &[],
            );
            device.end_command_buffer(cmd).map_err(vk_err("vkEndCommandBuffer"))?;

            let commands = [cmd];
            let submit = [vk::SubmitInfo::default().command_buffers(&commands)];
            device.reset_fences(&[self.fence]).map_err(vk_err("vkResetFences"))?;
            self.failed = true;
            device.queue_submit(self.queue, &submit, self.fence).map_err(vk_err("vkQueueSubmit"))?;
            device.wait_for_fences(&[self.fence], true, 10_000_000_000).map_err(vk_err("waiting for the GPU"))?;

            self.failed = false;
            let mut rgba = vec![0.0f32; result_len];
            std::ptr::copy_nonoverlapping(buffers[2].ptr, rgba.as_mut_ptr().cast::<u8>(), sizes[2]);
            Ok(SubpixelGrid::from_rgba(w, h, &rgba))
        }
    }

    fn device(&self) -> &str {
        &self.name
    }
}

impl Drop for VulkanRenderer {
    fn drop(&mut self) {
        // SAFETY: handles are destroyed once, children before parents;
        // destroying a null handle is a no-op in Vulkan.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.destroy_buffers();
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_descriptor_pool(self.descriptor_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.pipeline_layout, None);
            self.device.destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_shader_module(self.shader, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_shader_compiles_to_spirv() {
        let spirv = compile_shader().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spirv[0], 0x0723_0203, "SPIR-V magic number");
    }

    /// Needs a Vulkan driver; skipped (with a note) without one.
    #[test]
    fn matches_the_cpu_renderers() {
        match VulkanRenderer::new() {
            Ok(mut renderer) => crate::gpu::tests::assert_matches_cpu(&mut renderer),
            Err(e) => eprintln!("skipping: no Vulkan backend on this machine ({e})"),
        }
    }
}
