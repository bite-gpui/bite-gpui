//! Probe 2 of `spi/spike-windows-path-a.md`: create a raw D3D12 texture with
//! `D3D12_HEAP_FLAG_SHARED`, share it, and open it on a D3D11 device to build a
//! shader-resource view. If this succeeds, cross-API interop is mechanically
//! possible; if it fails, Path A on Windows needs the wgpu renderer.

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("windows only");
}

#[cfg(target_os = "windows")]
fn main() -> windows::core::Result<()> {
    imp::run()
}

#[cfg(target_os = "windows")]
mod imp {
    use windows::core::{Interface, PCWSTR, Result};
    use windows::Win32::Foundation::{HANDLE, HMODULE};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_12_0,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11ShaderResourceView, ID3D11Texture2D,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Direct3D12::{
        D3D12CreateDevice, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
        D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_STATE_COMMON,
        D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12Device, ID3D12DeviceChild, ID3D12Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    const WIDTH: u64 = 64;
    const HEIGHT: u32 = 64;
    // GENERIC_ALL; passed numerically to avoid pulling in the security feature for one
    // constant.
    const GENERIC_ALL: u32 = 0x1000_0000;

    pub fn run() -> Result<()> {
        // --- producer side: a D3D12 texture created shareable -------------------
        let mut device12: Option<ID3D12Device> = None;
        unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device12)? };
        let device12 = device12.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        println!("d3d12 device acquired");

        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: WIDTH,
            Height: HEIGHT,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        };

        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device12.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )?
        };
        let resource = resource.ok_or_else(|| {
            windows::core::Error::from(windows::Win32::Foundation::E_FAIL)
        })?;
        println!("d3d12 shared-heap texture created");

        // `CreateSharedHandle` takes an `ID3D12DeviceChild`, which a resource is via
        // `ID3D12Pageable`.
        let child: ID3D12DeviceChild = resource.cast()?;
        let handle: HANDLE =
            unsafe { device12.CreateSharedHandle(&child, None, GENERIC_ALL, PCWSTR::null())? };
        println!("CreateSharedHandle -> {handle:?}");

        // --- consumer side: open it on a D3D11 device --------------------------
        let mut device11: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device11),
                None,
                None,
            )?
        };
        let device11 = device11.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let device11_1: ID3D11Device1 = device11.cast()?;

        let texture: ID3D11Texture2D = unsafe { device11_1.OpenSharedResource1(handle)? };
        println!("ID3D11Device1::OpenSharedResource1 -> texture");

        let mut srv: Option<ID3D11ShaderResourceView> = None;
        unsafe { device11.CreateShaderResourceView(&texture, None, Some(&mut srv))? };
        println!("CreateShaderResourceView -> srv");
        println!("PROBE 2: interop OK");
        Ok(())
    }
}
