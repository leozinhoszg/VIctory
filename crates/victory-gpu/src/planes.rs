//! Presentation of more than one plane. The console's display hardware
//! composes the planes a title presents: Grand Theft Auto VI in Series S
//! mode presents its scene (1920x1080) and its interface (3840x2160) as two
//! planes, where Series X mode presents one. Each plane is drawn over the
//! whole back buffer, the first opaque and the others over it by their
//! alpha (premultiplied, as an interface drawn into a cleared target is).
//!
//! shortcut: source rectangles and placements are not read (the title
//! passes whole textures over the whole screen); no colour space handling.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{PCSTR, s};

use crate::host::transition;

/// Planes drawn in one presentation; more are left out.
pub(crate) const MAX_PLANES: usize = 4;

const SOURCE: &str = r#"
#define RS "DescriptorTable(SRV(t0), visibility=SHADER_VISIBILITY_PIXEL), StaticSampler(s0, filter=FILTER_MIN_MAG_LINEAR_MIP_POINT, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, visibility=SHADER_VISIBILITY_PIXEL)"
Texture2D plane : register(t0);
SamplerState linear_clamp : register(s0);
struct Corner { float4 position : SV_Position; float2 uv : TEXCOORD0; };
[RootSignature(RS)]
Corner vertex(uint id : SV_VertexID) {
    Corner c;
    c.uv = float2((id << 1) & 2, id & 2);
    c.position = float4(c.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return c;
}
[RootSignature(RS)]
float4 pixel(Corner c) : SV_Target {
    return plane.Sample(linear_clamp, c.uv);
}
"#;

/// The pipelines that draw a plane into a back buffer of one format.
pub(crate) struct Composer {
    format: DXGI_FORMAT,
    root: ID3D12RootSignature,
    opaque: ID3D12PipelineState,
    over: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    view_size: u32,
}

fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<ID3DBlob> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("planes"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize())
            };
            tracing::warn!("plane composer: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    code.ok_or_else(windows::core::Error::empty)
}

impl Composer {
    pub(crate) fn format(&self) -> DXGI_FORMAT {
        self.format
    }

    /// The composer for back buffers of `format`.
    pub(crate) fn new(device: &ID3D12Device, format: DXGI_FORMAT) -> windows::core::Result<Self> {
        let vertex = compile(s!("vertex"), s!("vs_5_1"))?;
        let pixel = compile(s!("pixel"), s!("ps_5_1"))?;
        // SAFETY: the blobs' own bytes; plain object creation.
        unsafe {
            let bytes = |b: &ID3DBlob| D3D12_SHADER_BYTECODE {
                pShaderBytecode: b.GetBufferPointer(),
                BytecodeLength: b.GetBufferSize(),
            };
            let root: ID3D12RootSignature = device.CreateRootSignature(
                0,
                std::slice::from_raw_parts(pixel.GetBufferPointer().cast::<u8>(), pixel.GetBufferSize()),
            )?;
            let pipeline = |blend: bool| -> windows::core::Result<ID3D12PipelineState> {
                let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    VS: bytes(&vertex),
                    PS: bytes(&pixel),
                    SampleMask: u32::MAX,
                    RasterizerState: D3D12_RASTERIZER_DESC {
                        FillMode: D3D12_FILL_MODE_SOLID,
                        CullMode: D3D12_CULL_MODE_NONE,
                        ..Default::default()
                    },
                    PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                    NumRenderTargets: 1,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    ..Default::default()
                };
                desc.RTVFormats[0] = format;
                desc.BlendState.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: blend.into(),
                    SrcBlend: D3D12_BLEND_ONE,
                    DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D12_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D12_BLEND_ONE,
                    DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D12_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
                    ..Default::default()
                };
                device.CreateGraphicsPipelineState(&desc)
            };
            let views: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: MAX_PLANES as u32,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            Ok(Self {
                format,
                opaque: pipeline(false)?,
                over: pipeline(true)?,
                root,
                views,
                view_size: device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV),
            })
        }
    }

    /// Records, in `list`, the planes (each with the host state it is in
    /// and stays in, and the format its bytes are read as) drawn in order
    /// over all of `back` (`width` by `height`), which is in the present
    /// state and is left as a copy source, for the frame to be read back.
    ///
    /// # Safety
    /// An open list of `device`; live resources in the states given.
    pub(crate) unsafe fn compose(
        &self,
        device: &ID3D12Device,
        list: &ID3D12GraphicsCommandList,
        (back, target): (&ID3D12Resource, D3D12_CPU_DESCRIPTOR_HANDLE),
        (width, height): (u32, u32),
        planes: &[(&ID3D12Resource, u32, DXGI_FORMAT)],
    ) {
        const SHADER_RESOURCE: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
        let planes = &planes[..planes.len().min(MAX_PLANES)];
        // SAFETY: the caller's.
        unsafe {
            let mut barriers = vec![transition(
                back,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )];
            barriers.extend(
                planes
                    .iter()
                    .filter(|p| p.1 as i32 != SHADER_RESOURCE.0)
                    .map(|&(t, state, _)| transition(t, D3D12_RESOURCE_STATES(state as i32), SHADER_RESOURCE)),
            );
            list.ResourceBarrier(&barriers);
            list.SetDescriptorHeaps(&[Some(self.views.clone())]);
            list.SetGraphicsRootSignature(&self.root);
            list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: width as f32,
                Height: height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            }]);
            list.OMSetRenderTargets(1, Some(&target), false, None);
            list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let cpu = self.views.GetCPUDescriptorHandleForHeapStart();
            let gpu = self.views.GetGPUDescriptorHandleForHeapStart();
            for (i, &(texture, _, format)) in planes.iter().enumerate() {
                let view = D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: format,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MipLevels: 1,
                            ..Default::default()
                        },
                    },
                };
                let at = i * self.view_size as usize;
                device.CreateShaderResourceView(
                    texture,
                    Some(&view),
                    D3D12_CPU_DESCRIPTOR_HANDLE { ptr: cpu.ptr + at },
                );
                list.SetPipelineState(if i == 0 { &self.opaque } else { &self.over });
                list.SetGraphicsRootDescriptorTable(0, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: gpu.ptr + at as u64 });
                list.DrawInstanced(3, 1, 0, 0);
            }
            let mut barriers = vec![transition(
                back,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )];
            barriers.extend(
                planes
                    .iter()
                    .filter(|p| p.1 as i32 != SHADER_RESOURCE.0)
                    .map(|&(t, state, _)| transition(t, SHADER_RESOURCE, D3D12_RESOURCE_STATES(state as i32))),
            );
            list.ResourceBarrier(&barriers);
        }
    }
}
