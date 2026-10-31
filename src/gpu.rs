use crate::{
    decode::Frame,
    model::{PhotoId, Settings},
    processing::gaussian_weights,
};
use anyhow::{bail, Context, Result};
use wgpu::util::DeviceExt;

struct Resident {
    id: PhotoId,
    original: wgpu::Buffer,
    filtered: Option<([u32; 2], wgpu::Buffer)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Mode, processing::Cpu};
    #[test]
    #[ignore = "requires a working native GPU backend"]
    fn gpu_matches_cpu_all_modes_filters_and_dimensions() {
        let mut gpu = Gpu::new().unwrap();
        let make = |w, h, seed: u32| Frame {
            width: w,
            height: h,
            rgb: (0..w * h * 3)
                .map(|i| (i.wrapping_mul(747796405).wrapping_add(seed) & 65535) as u16)
                .collect(),
        };
        // An odd number of RGB16 samples exercises the packed buffer's padded tail.
        let a = make(17, 13, 1234);
        let b = make(11, 9, 554);
        let mut cpu = Cpu::new();
        let mut s = Settings::default();
        s.viewport = [37, 29];
        for sigma in [0.3, 2.0, 7.5] {
            s.sigma = sigma;
            for highpass in [false, true] {
                for mode in Mode::ALL {
                    for show_b in [false, true] {
                        s.highpass = highpass;
                        s.mode = mode;
                        s.single_b = show_b;
                        let expected = cpu.render((1, &a), (2, &b), &s, show_b);
                        let actual = gpu.render((1, &a), (2, &b), &s, show_b).unwrap();
                        let delta = expected
                            .iter()
                            .zip(&actual)
                            .map(|(a, b)| a.abs_diff(*b))
                            .max()
                            .unwrap();
                        assert!(
                            delta <= 1,
                            "{mode:?}, hp={highpass}, B={show_b}, max delta={delta}"
                        );
                    }
                }
            }
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewParams {
    dims: [u32; 4],
    view: [u32; 4],
    camera: [f32; 4],
    options: [f32; 4],
}
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    render: wgpu::ComputePipeline,
    horizontal: wgpu::ComputePipeline,
    vertical: wgpu::ComputePipeline,
    render_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
    residents: Vec<Resident>,
    output: Option<(u64, wgpu::Buffer, wgpu::Buffer)>,
    scratch: Option<(u64, wgpu::Buffer)>,
    dummy: wgpu::Buffer,
    pub name: String,
}
impl Gpu {
    pub fn clear(&mut self) {
        self.residents.clear();
        self.output = None;
        self.scratch = None;
    }
    pub fn new() -> Result<Self> {
        smol::block_on(Self::init())
    }
    async fn init() -> Result<Self> {
        let backends = if cfg!(windows) {
            wgpu::Backends::DX12
        } else if cfg!(target_os = "macos") {
            wgpu::Backends::METAL
        } else {
            wgpu::Backends::VULKAN
        };
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .context("No compute adapter")?;
        let info = adapter.get_info();
        let available = adapter.limits();
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffer_binding_size = available
            .max_storage_buffer_binding_size
            .min(1024 * 1024 * 1024);
        limits.max_buffer_size = available.max_buffer_size.min(2 * 1024 * 1024 * 1024);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Lumen compute"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                ..Default::default()
            })
            .await?;
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let render_layout = Self::layout(&device, "view layout", &[true, true, true, true, false]);
        let filter_layout = Self::layout(
            &device,
            "Gaussian layout",
            &[true, false, false, true, true],
        );
        let render = Self::pipeline(
            &device,
            &render_layout,
            include_str!("render.wgsl"),
            "render",
        );
        let horizontal = Self::pipeline(
            &device,
            &filter_layout,
            include_str!("gaussian.wgsl"),
            "horizontal",
        );
        let vertical = Self::pipeline(
            &device,
            &filter_layout,
            include_str!("gaussian.wgsl"),
            "vertical",
        );
        if let Some(error) = device.pop_error_scope().await {
            bail!("GPU shader: {error}");
        }
        let dummy = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("unused filter"),
            contents: &[0; 16],
            usage: wgpu::BufferUsages::STORAGE,
        });
        Ok(Self {
            device,
            queue,
            render,
            horizontal,
            vertical,
            render_layout,
            filter_layout,
            residents: Vec::new(),
            output: None,
            scratch: None,
            dummy,
            name: format!("{:?} · {}", info.backend, info.name),
        })
    }
    fn layout(device: &wgpu::Device, label: &str, buffers: &[bool]) -> wgpu::BindGroupLayout {
        let mut entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(index, read_only)| wgpu::BindGroupLayoutEntry {
                binding: index as u32,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: *read_only,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: buffers.len() as u32,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        })
    }
    fn pipeline(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        source: &str,
        entry: &str,
    ) -> wgpu::ComputePipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(entry),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(entry),
            bind_group_layouts: &[layout],
            push_constant_ranges: &[],
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    }
    fn buffer(&self, bytes: u64, label: &str, usage: wgpu::BufferUsages) -> Result<wgpu::Buffer> {
        if bytes > self.device.limits().max_storage_buffer_binding_size as u64
            || bytes > self.device.limits().max_buffer_size
        {
            bail!("{label}: buffer exceeds GPU limits ({bytes} bytes)");
        }
        Ok(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes.max(4),
            usage,
            mapped_at_creation: false,
        }))
    }
    fn upload(&mut self, id: PhotoId, frame: &Frame) -> Result<usize> {
        if let Some(index) = self.residents.iter().position(|r| r.id == id) {
            return Ok(index);
        }
        if frame.bytes() as u64 > self.device.limits().max_storage_buffer_binding_size as u64 {
            bail!("Image exceeds GPU storage limit");
        }
        let original = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resident RGB16"),
                contents: bytemuck::cast_slice(&frame.rgb),
                usage: wgpu::BufferUsages::STORAGE,
            });
        self.residents.push(Resident {
            id,
            original,
            filtered: None,
        });
        Ok(self.residents.len() - 1)
    }
    fn group(&self, layout: &wgpu::BindGroupLayout, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &entries,
        })
    }
    fn filter(
        &mut self,
        index: usize,
        frame: &Frame,
        sigma: [f32; 2],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<()> {
        let key = sigma.map(f32::to_bits);
        if self.residents[index].filtered.as_ref().map(|x| x.0) == Some(key) {
            return Ok(());
        }
        let previous = self.residents[index].filtered.take();
        let bytes = frame.rgb.len() as u64 * 4;
        if self.scratch.as_ref().map(|x| x.0).unwrap_or(0) < bytes {
            self.scratch = None;
            self.scratch = Some((
                bytes,
                self.buffer(bytes, "Gaussian scratch", wgpu::BufferUsages::STORAGE)?,
            ));
        }
        let temporary = &self.scratch.as_ref().unwrap().1;
        let filtered = match previous {
            Some((_, buffer)) => buffer,
            None => self.buffer(bytes, "signed highpass RGB32F", wgpu::BufferUsages::STORAGE)?,
        };
        let wx = gaussian_weights(sigma[0]);
        let wy = gaussian_weights(sigma[1]);
        let weights = |label, values: &[f32]| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytemuck::cast_slice(values),
                    usage: wgpu::BufferUsages::STORAGE,
                })
        };
        let wx_buffer = weights("Gaussian weights X", &wx);
        let wy_buffer = weights("Gaussian weights Y", &wy);
        let params = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&[
                    frame.width,
                    frame.height,
                    (wx.len() / 2) as u32,
                    (wy.len() / 2) as u32,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let group = self.group(
            &self.filter_layout,
            &[
                &self.residents[index].original,
                temporary,
                &filtered,
                &wx_buffer,
                &wy_buffer,
                &params,
            ],
        );
        for pipeline in [&self.horizontal, &self.vertical] {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Gaussian"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(frame.width.div_ceil(16), frame.height.div_ceil(16), 1);
        }
        self.residents[index].filtered = Some((key, filtered));
        Ok(())
    }
    pub fn render(
        &mut self,
        a: (PhotoId, &Frame),
        b: (PhotoId, &Frame),
        settings: &Settings,
        show_b: bool,
    ) -> Result<Vec<u8>> {
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.render_inner(a, b, settings, show_b);
        let validation = smol::block_on(self.device.pop_error_scope());
        let memory = smol::block_on(self.device.pop_error_scope());
        if let Some(error) = validation.or(memory) {
            self.residents.clear();
            bail!("GPU: {error}");
        }
        result
    }
    fn render_inner(
        &mut self,
        a: (PhotoId, &Frame),
        b: (PhotoId, &Frame),
        settings: &Settings,
        show_b: bool,
    ) -> Result<Vec<u8>> {
        self.residents.retain(|r| r.id == a.0 || r.id == b.0);
        let ai = self.upload(a.0, a.1)?;
        let bi = self.upload(b.0, b.1)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("comparison frame"),
            });
        if settings.highpass {
            self.filter(ai, a.1, [settings.sigma; 2], &mut encoder)?;
            self.filter(
                bi,
                b.1,
                [
                    settings.sigma * b.1.width as f32 / a.1.width as f32,
                    settings.sigma * b.1.height as f32 / a.1.height as f32,
                ],
                &mut encoder,
            )?;
        }
        let [vw, vh] = settings.viewport;
        let bytes = vw as u64 * vh as u64 * 4;
        if self.output.as_ref().map(|x| x.0) != Some(bytes) {
            self.output = None;
            let output = self.buffer(
                bytes,
                "viewport BGRA8",
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            )?;
            let staging = self.buffer(
                bytes,
                "viewport readback",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            )?;
            self.output = Some((bytes, output, staging));
        }
        let a_gpu = &self.residents[ai];
        let b_gpu = &self.residents[bi];
        let params = ViewParams {
            dims: [a.1.width, a.1.height, b.1.width, b.1.height],
            view: [
                vw,
                vh,
                settings.mode.kernel_id() as u32,
                settings.highpass as u32,
            ],
            camera: [
                settings.camera.scale(a.1.size(), settings.viewport),
                settings.camera.center[0],
                settings.camera.center[1],
                settings.split,
            ],
            options: [settings.gain as f32, show_b as u32 as f32, 0., 0.],
        };
        let params = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("camera"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let (_, output, staging) = self.output.as_ref().unwrap();
        let group = self.group(
            &self.render_layout,
            &[
                &a_gpu.original,
                &b_gpu.original,
                a_gpu.filtered.as_ref().map(|x| &x.1).unwrap_or(&self.dummy),
                b_gpu.filtered.as_ref().map(|x| &x.1).unwrap_or(&self.dummy),
                output,
                &params,
            ],
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("viewport"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.render);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(vw.div_ceil(16), vh.div_ceil(16), 1);
        }
        encoder.copy_buffer_to_buffer(output, 0, staging, 0, bytes);
        let submission = self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(std::time::Duration::from_secs(30)),
        })?;
        rx.recv()??;
        let pixels = staging.slice(..).get_mapped_range().to_vec();
        staging.unmap();
        Ok(pixels)
    }
}
