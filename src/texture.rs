use crate::dxgi::{calculate_pitch, DxgiFormat};
use crate::packages::package_manager;
use crate::render::DeviceContextSwapchain;
use crate::structure::{CafeMarker, TablePointer};
use crate::types::IVector2;
use anyhow::Context;
use binrw::BinRead;
use destiny_pkg::TagHash;
use std::io::SeekFrom;
use windows::Win32::Graphics::Direct3D::{
    WKPDID_D3DDebugObjectName, D3D11_SRV_DIMENSION_TEXTURE2D, D3D11_SRV_DIMENSION_TEXTURE3D,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11Texture3D,
};
use windows::Win32::Graphics::Dxgi::Common::*;

#[derive(BinRead, Debug)]
pub struct TextureHeader {
    pub data_size: u32,
    pub format: DxgiFormat,
    pub _unk8: u32,

    pub cafe: CafeMarker,

    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub array_size: u16,

    #[br(seek_before = SeekFrom::Start(0x24))]
    #[br(map(|v: u32| (v != u32::MAX).then_some(TagHash(v))))]
    pub large_buffer: Option<TagHash>,
}

/// Ref: 0x80809ebb
#[derive(BinRead, Debug)]
pub struct TexturePlate {
    pub file_size: u64,
    pub _unk: u64,
    pub transforms: TablePointer<TexturePlateTransform>,
}

#[derive(BinRead, Debug)]
pub struct TexturePlateTransform {
    pub texture: TagHash,
    pub translation: IVector2,
    pub dimensions: IVector2,
}

/// Ref: 0x808072d2
#[derive(BinRead, Debug)]
pub struct TexturePlateSet {
    pub file_size: u64,
    pub _unk: [u32; 7],
    pub diffuse: TagHash,
    pub normal: TagHash,
    pub gstack: TagHash,
}

pub enum TextureHandle {
    Texture2D(ID3D11Texture2D),
    // TextureCube(ID3D11Texture2D),
    Texture3D(ID3D11Texture3D),
}

pub struct Texture {
    pub view: ID3D11ShaderResourceView,
    pub handle: TextureHandle,
    pub format: DxgiFormat,
}

impl Texture {
    pub fn load(dcs: &DeviceContextSwapchain, hash: TagHash) -> anyhow::Result<Texture> {
        let texture_header_ref = package_manager().get_entry(hash)?.reference;

        let texture: TextureHeader = package_manager().read_tag_struct(hash)?;
        let mut texture_data = if let Some(t) = texture.large_buffer {
            package_manager()
                .read_tag(t)
                .context("Failed to read texture data")?
        } else {
            package_manager()
                .read_tag(texture_header_ref)
                .context("Failed to read texture data")?
                .to_vec()
        };

        let mut mips = 1;
        if texture.large_buffer.is_some() {
            let ab = package_manager()
                .read_tag(texture_header_ref)
                .context("Failed to read texture data")?
                .to_vec();

            texture_data.extend(ab);

            let mut dim = texture.width.min(texture.width) as usize;
            mips = 0;
            while dim > 1 {
                dim >>= 1;
                mips += 1;
            }

            let mut required_mip_bytes = 0;
            for i in 0..mips {
                let width = texture.width >> i;
                let height = texture.height >> i;
                let size = calculate_pitch(texture.format, width as usize, height as usize);
                if (required_mip_bytes + size.1) > texture_data.len() {
                    mips = i + 1;
                    break;
                }
                required_mip_bytes += size.1;
            }
        }

        let (tex, view) = unsafe {
            if texture.depth > 1 {
                let (pitch, slice_pitch) =
                    calculate_pitch(texture.format, texture.width as _, texture.height as _);
                let initial_data = D3D11_SUBRESOURCE_DATA {
                    pSysMem: texture_data.as_ptr() as _,
                    SysMemPitch: pitch as _,
                    SysMemSlicePitch: slice_pitch as _,
                };

                let tex = dcs
                    .device
                    .CreateTexture3D(
                        &D3D11_TEXTURE3D_DESC {
                            Width: texture.width as _,
                            Height: texture.height as _,
                            Depth: texture.depth as _,
                            MipLevels: 1,
                            Format: texture.format.into(),
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE,
                            CPUAccessFlags: Default::default(),
                            MiscFlags: Default::default(),
                        },
                        Some([initial_data].as_ptr()),
                    )
                    .context("Failed to create 3D texture")
                    .unwrap();

                let view = dcs
                    .device
                    .CreateShaderResourceView(
                        &tex,
                        Some(&D3D11_SHADER_RESOURCE_VIEW_DESC {
                            Format: texture.format.into(),
                            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE3D,
                            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                                Texture3D: D3D11_TEX3D_SRV {
                                    MostDetailedMip: 0,
                                    MipLevels: 1,
                                },
                            },
                        }),
                    )
                    .unwrap();

                (TextureHandle::Texture3D(tex), view)
            } else {
                let mut initial_data = vec![];
                let mut offset = 0;
                for i in 0..mips {
                    let width = texture.width >> i;
                    let height = texture.height >> i;
                    let (pitch, slice_pitch) =
                        calculate_pitch(texture.format, width as usize, height as usize);

                    initial_data.push(D3D11_SUBRESOURCE_DATA {
                        pSysMem: texture_data.as_ptr().add(offset) as _,
                        SysMemPitch: pitch as u32,
                        SysMemSlicePitch: 0,
                    });
                    offset += slice_pitch;
                }

                let tex = dcs
                    .device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: texture.width as _,
                            Height: texture.height as _,
                            MipLevels: mips as u32,
                            // TODO(cohae): Cubemaps
                            ArraySize: 1 as _,
                            Format: texture.format.into(),
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE,
                            CPUAccessFlags: Default::default(),
                            MiscFlags: Default::default(),
                        },
                        Some(initial_data.as_ptr()),
                    )
                    .context("Failed to create texture")
                    .unwrap();

                let name = format!("Tex {:?}/0x{:08x}\0", hash, hash.0);
                tex.SetPrivateData(
                    &WKPDID_D3DDebugObjectName,
                    name.len() as u32 - 1,
                    Some(name.as_ptr() as _),
                )
                .context("Failed to set VS name")?;

                let view = dcs
                    .device
                    .CreateShaderResourceView(
                        &tex,
                        Some(&D3D11_SHADER_RESOURCE_VIEW_DESC {
                            Format: texture.format.into(),
                            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                                Texture2D: D3D11_TEX2D_SRV {
                                    MostDetailedMip: 0,
                                    MipLevels: mips as _,
                                },
                            },
                        }),
                    )
                    .unwrap();

                (TextureHandle::Texture2D(tex), view)
            }
        };

        Ok(Texture {
            handle: tex,
            view,
            format: texture.format,
        })
    }
}
