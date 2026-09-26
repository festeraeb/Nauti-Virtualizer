//! wgpu add kernel used by the M10 remote GPU server.

use wgpu::util::DeviceExt;

pub fn add_f32(a: &[f32], b: &[f32]) -> Result<(Vec<f32>, String, String), String> {
    if a.len() != b.len() {
        return Err("a/b length mismatch".into());
    }
    pollster::block_on(add_f32_async(a, b))
}

async fn add_f32_async(a: &[f32], b: &[f32]) -> Result<(Vec<f32>, String, String), String> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| "no wgpu adapter (Vulkan/GPU missing)".to_string())?;
    let info = adapter.get_info();
    let adapter_name = format!("{} ({:?})", info.name, info.backend);
    let backend = format!("{:?}", info.backend);

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("nauti-wgpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        )
        .await
        .map_err(|e| e.to_string())?;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("nauti-add"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("add.wgsl"))),
    });

    let a_bytes: &[u8] = bytemuck::cast_slice(a);
    let b_bytes: &[u8] = bytemuck::cast_slice(b);
    let out_size = a_bytes.len() as u64;

    let a_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("a"),
        contents: a_bytes,
        usage: wgpu::BufferUsages::STORAGE,
    });
    let b_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("b"),
        contents: b_bytes,
        usage: wgpu::BufferUsages::STORAGE,
    });
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: out_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read"),
        size: out_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("add-layout"),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, true),
            storage_entry(2, false),
        ],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("add-bind"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: a_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: b_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("add-pl"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("add-pipe"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: "main",
        compilation_options: Default::default(),
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("add-enc"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("add-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        let groups = (a.len() as u32 + 63) / 64;
        pass.dispatch_workgroups(groups, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, out_size);
    queue.submit(Some(encoder.finish()));

    let slice = read_buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    read_buf.unmap();
    Ok((out, backend, adapter_name))
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
