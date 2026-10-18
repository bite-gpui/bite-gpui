use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use gpui_platform::{
    GpuAdapterInfo, GpuBackend, GpuDeviceType, GpuId, GpuPreference, PermanentGpuId, SessionGpuId,
};
use gpui_util::ResultExt;
use itertools::Itertools;
use windows::Win32::{
    Foundation::{HMODULE, LUID},
    Graphics::{
        DXCore::{
            DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS, DXCoreAdapterProperty, IDXCoreAdapter,
            IDXCoreAdapterFactory, IDXCoreAdapterList, InstanceLuid, IsHardware, IsIntegrated,
        },
        Direct3D::{
            D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
        },
        Direct3D11::{
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG,
            D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS,
            D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
        },
        Dxgi::{
            CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_DEBUG,
            DXGI_CREATE_FACTORY_FLAGS, DXGI_ERROR_NOT_FOUND, IDXGIAdapter1, IDXGIFactory6,
        },
    },
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};
use windows::core::{GUID, HSTRING, Interface as _, Type as _};
pub(crate) fn try_to_recover_from_device_lost<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    (0..5)
        .map(|i| {
            if i > 0 {
                // Add a small delay before retrying
                std::thread::sleep(std::time::Duration::from_millis(100 + i * 10));
            }
            f()
        })
        .find_or_last(Result::is_ok)
        .unwrap()
        .context("DirectXRenderer failed to recover from lost device after multiple attempts")
}

#[derive(Clone)]
pub(crate) struct DirectXDevices {
    pub(crate) adapter: IDXGIAdapter1,
    pub(crate) dxgi_factory: IDXGIFactory6,
    pub(crate) device: ID3D11Device,
    pub(crate) device_context: ID3D11DeviceContext,
    /// The identity of `adapter`, in the same shape `Platform::enumerate_gpus` reports.
    pub(crate) gpu: GpuAdapterInfo,
}

impl DirectXDevices {
    /// Enumerate the adapters, order them for `preference`, and create the first device that works.
    ///
    /// Enumeration itself creates no device; the walk is the capability test, and a later failure
    /// (the swapchain does not configure) still falls through as it always has.
    pub(crate) fn new(preference: &GpuPreference) -> Result<Self> {
        let debug_layer_available = check_debug_layer_available();
        let (dxgi_factory, mut adapters) = enumerated_adapters(debug_layer_available)?;
        if adapters.is_empty() {
            return Err(anyhow!("Direct3D 11 reports no adapters"));
        }
        order_adapters(&mut adapters, preference)?;

        let mut rejected = Vec::new();
        for candidate in adapters {
            let name = candidate.info.name.clone();
            log::info!("Using GPU: {name}");
            let mut context: Option<ID3D11DeviceContext> = None;
            let mut feature_level = D3D_FEATURE_LEVEL::default();
            match get_device(
                &candidate.adapter,
                Some(&mut context),
                Some(&mut feature_level),
                debug_layer_available,
            ) {
                Ok(device) => {
                    log_feature_level(feature_level);
                    return Ok(Self {
                        adapter: candidate.adapter,
                        dxgi_factory,
                        device,
                        device_context: context.unwrap(),
                        gpu: candidate.info,
                    });
                }
                Err(error) => {
                    log::warn!("Adapter {name} could not create a Direct3D 11 device: {error:#}");
                    rejected.push(name);
                }
            }
        }

        Err(anyhow!(
            "No adapter could create a Direct3D 11 device (tried {})",
            rejected.join(", ")
        ))
    }
}

/// The adapters an application could render on, without creating a device.
pub(crate) fn enumerate_adapters() -> Result<Vec<GpuAdapterInfo>> {
    let (_, adapters) = enumerated_adapters(check_debug_layer_available())?;
    Ok(adapters.into_iter().map(|adapter| adapter.info).collect())
}

fn log_feature_level(feature_level: D3D_FEATURE_LEVEL) {
    match feature_level {
        D3D_FEATURE_LEVEL_11_1 => log::info!("Created device with Direct3D 11.1 feature level."),
        D3D_FEATURE_LEVEL_11_0 => log::info!("Created device with Direct3D 11.0 feature level."),
        D3D_FEATURE_LEVEL_10_1 => log::info!("Created device with Direct3D 10.1 feature level."),
        _ => unreachable!(),
    }
}

/// One adapter and the identity `Platform::enumerate_gpus` reports for it.
struct EnumeratedAdapter {
    info: GpuAdapterInfo,
    adapter: IDXGIAdapter1,
}

/// Enumerate the DXGI adapters in the OS's order, classifying each one.
///
/// The order is DXGI's, which is the OS's own preference; `rank` returns a constant under
/// [`GpuPreference::Auto`], so a stable sort under it preserves that order.
fn enumerated_adapters(
    debug_layer_available: bool,
) -> Result<(IDXGIFactory6, Vec<EnumeratedAdapter>)> {
    let dxgi_factory = get_dxgi_factory(debug_layer_available).context("Creating DXGI factory")?;
    let classes = dxcore_classes();
    let mut adapters = Vec::new();

    for index in 0.. {
        let adapter = match unsafe { dxgi_factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error).context("Enumerating DXGI adapters"),
        };
        let desc = unsafe { adapter.GetDesc1() }.context("Reading a DXGI adapter description")?;
        let name = String::from_utf16_lossy(&desc.Description)
            .trim_matches(char::from(0))
            .to_string();
        let luid = luid_key(desc.AdapterLuid);
        let facts = classes.get(&luid).copied().unwrap_or_default();
        let (is_software, device_type) = classify(desc.Flags, facts);

        adapters.push(EnumeratedAdapter {
            info: GpuAdapterInfo::new(
                GpuId {
                    permanent: PermanentGpuId::Pci {
                        vendor_id: Some(desc.VendorId),
                        device_id: desc.DeviceId,
                        subsys_id: Some(desc.SubSysId),
                        revision: Some(desc.Revision),
                    },
                    session: Some(SessionGpuId::Luid(luid)),
                },
                name,
                device_type,
                GpuBackend::Dx11,
                is_software,
            ),
            adapter,
        });
    }

    Ok((dxgi_factory, adapters))
}

type CreateAdapterFactory = unsafe extern "system" fn(
    riid: *const GUID,
    ppvfactory: *mut *mut core::ffi::c_void,
) -> windows::core::HRESULT;

/// `dxcore.dll`'s factory, loaded by hand.
///
/// DXCore only exists on Windows 10 2004 and later. A static import of
/// `DXCoreCreateAdapterFactory` would put `dxcore.dll` in the executable's import table and stop
/// the process from starting on anything older, so the entry point is resolved at run time and
/// an absent DLL is the "no classification available" answer rather than a load failure.
fn dxcore_factory() -> Option<IDXCoreAdapterFactory> {
    static CREATE: std::sync::OnceLock<Option<CreateAdapterFactory>> = std::sync::OnceLock::new();
    let create = (*CREATE.get_or_init(|| unsafe {
        let module = LoadLibraryW(&HSTRING::from("dxcore.dll")).ok()?;
        let address = GetProcAddress(module, windows::core::s!("DXCoreCreateAdapterFactory"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            CreateAdapterFactory,
        >(address))
    }))?;

    let mut factory = std::ptr::null_mut();
    if unsafe { create(&IDXCoreAdapterFactory::IID, &mut factory) }.is_err() {
        return None;
    }
    unsafe { IDXCoreAdapterFactory::from_abi(factory) }.ok()
}

/// Order `adapters` for `preference`, before any device is created.
///
/// A custom preference names one adapter, so it is not part of the ranking: it goes first and the
/// rest keep `Auto`'s order as fallbacks. Everything else goes through the shared rank.
fn order_adapters(adapters: &mut Vec<EnumeratedAdapter>, preference: &GpuPreference) -> Result<()> {
    match preference {
        GpuPreference::Custom(selector) => {
            let info: Vec<GpuAdapterInfo> = adapters
                .iter()
                .map(|adapter| adapter.info.clone())
                .collect();
            let chosen = gpui_platform::custom_choice(&info, selector.as_ref())?;
            let adapter = adapters.remove(chosen);
            adapters.sort_by_cached_key(|adapter| {
                gpui_platform::rank(&adapter.info, &GpuPreference::Auto, None)
            });
            adapters.insert(0, adapter);
        }
        _ => adapters
            .sort_by_cached_key(|adapter| gpui_platform::rank(&adapter.info, preference, None)),
    }
    Ok(())
}

/// Classify one adapter from its DXGI flags and what DXCore knows about it.
///
/// A software adapter is one DXCore calls not-hardware — the Basic Render Driver is sometimes
/// flagged and sometimes not, so neither the flag alone nor the name is enough. Where DXCore cannot
/// answer (before Windows 10 2004), `Other` is honest: the memory-size heuristic that would guess
/// here mislabels low-end discrete cards.
fn classify(flags: u32, facts: AdapterClass) -> (bool, GpuDeviceType) {
    let is_software =
        flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 || facts.is_hardware == Some(false);
    if is_software {
        return (true, GpuDeviceType::Cpu);
    }
    let device_type = match facts.is_integrated {
        Some(true) => GpuDeviceType::Integrated,
        Some(false) => GpuDeviceType::Discrete,
        None => GpuDeviceType::Other,
    };
    (false, device_type)
}

/// What DXCore knows about one adapter.
#[derive(Clone, Copy, Default)]
struct AdapterClass {
    is_hardware: Option<bool>,
    is_integrated: Option<bool>,
}

/// Classify the adapters DXCore knows, keyed by LUID.
///
/// DXCore is absent before Windows 10 2004 and `IsIntegrated` may be unsupported even where the
/// factory exists; a missing answer stays `None`, so the caller reports `Other` instead of guessing.
fn dxcore_classes() -> HashMap<u64, AdapterClass> {
    let mut classes = HashMap::new();
    let factory = match dxcore_factory() {
        Some(factory) => factory,
        None => {
            log::info!("DXCore is unavailable; adapter classes will be reported as unknown");
            return classes;
        }
    };
    let list: IDXCoreAdapterList =
        match unsafe { factory.CreateAdapterList(&[DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS]) } {
            Ok(list) => list,
            Err(error) => {
                log::warn!("DXCore could not list adapters; classes will be unknown: {error}");
                return classes;
            }
        };

    for index in 0..unsafe { list.GetAdapterCount() } {
        let adapter: IDXCoreAdapter = match unsafe { list.GetAdapter(index) } {
            Ok(adapter) => adapter,
            Err(error) => {
                log::warn!("DXCore adapter {index} could not be read: {error}");
                continue;
            }
        };
        let mut luid = LUID::default();
        if let Err(error) = unsafe {
            adapter.GetProperty(InstanceLuid, size_of::<LUID>(), &mut luid as *mut _ as _)
        } {
            log::warn!("DXCore adapter {index} has no LUID: {error}");
            continue;
        }
        classes.insert(
            luid_key(luid),
            AdapterClass {
                is_hardware: bool_property(&adapter, IsHardware),
                is_integrated: bool_property(&adapter, IsIntegrated),
            },
        );
    }

    classes
}

fn bool_property(adapter: &IDXCoreAdapter, property: DXCoreAdapterProperty) -> Option<bool> {
    // `IsIntegrated` is only supported from Windows 10 2004; asking for an unsupported property
    // is an error, so the support check is what turns "no answer" into `None` rather than `false`.
    if !unsafe { adapter.IsPropertySupported(property) } {
        return None;
    }
    let mut value = false;
    unsafe { adapter.GetProperty(property, size_of::<bool>(), &mut value as *mut _ as _) }.ok()?;
    Some(value)
}

/// A DXGI LUID packs into one `u64`; the high half is `HighPart`.
fn luid_key(luid: LUID) -> u64 {
    ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
}

#[inline]
fn check_debug_layer_available() -> bool {
    #[cfg(debug_assertions)]
    {
        use windows::Win32::Graphics::Dxgi::{DXGIGetDebugInterface1, IDXGIInfoQueue};

        unsafe { DXGIGetDebugInterface1::<IDXGIInfoQueue>(0) }
            .log_err()
            .is_some()
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

#[inline]
fn get_dxgi_factory(debug_layer_available: bool) -> Result<IDXGIFactory6> {
    let factory_flag = if debug_layer_available {
        DXGI_CREATE_FACTORY_DEBUG
    } else {
        #[cfg(debug_assertions)]
        log::warn!(
            "Failed to get DXGI debug interface. DirectX debugging features will be disabled."
        );
        DXGI_CREATE_FACTORY_FLAGS::default()
    };
    unsafe { Ok(CreateDXGIFactory2(factory_flag)?) }
}

#[inline]
fn get_device(
    adapter: &IDXGIAdapter1,
    context: Option<*mut Option<ID3D11DeviceContext>>,
    feature_level: Option<*mut D3D_FEATURE_LEVEL>,
    debug_layer_available: bool,
) -> Result<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;
    let device_flags = if debug_layer_available {
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_DEBUG
    } else {
        D3D11_CREATE_DEVICE_BGRA_SUPPORT
    };
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            device_flags,
            // 4x MSAA is required for Direct3D Feature Level 10.1 or better
            Some(&[
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
            ]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            feature_level,
            context,
        )?;
    }
    let device = device.unwrap();
    let mut data = D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS::default();
    unsafe {
        device
            .CheckFeatureSupport(
                D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS,
                &mut data as *mut _ as _,
                std::mem::size_of::<D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS>() as u32,
            )
            .context("Checking GPU device feature support")?;
    }
    if data
        .ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x
        .as_bool()
    {
        Ok(device)
    } else {
        Err(anyhow!(
            "Required feature StructuredBuffer is not supported by GPU/driver"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{AdapterClass, classify, luid_key};
    use gpui_platform::GpuDeviceType;
    use windows::Win32::{Foundation::LUID, Graphics::Dxgi::DXGI_ADAPTER_FLAG_SOFTWARE};

    #[test]
    fn a_luid_packs_its_high_half_first() {
        let luid = LUID {
            LowPart: 0x1234_5678,
            HighPart: 0x0000_002a,
        };
        assert_eq!(luid_key(luid), 0x0000_002a_1234_5678);
    }

    #[test]
    fn the_software_flag_makes_a_cpu_adapter() {
        let facts = AdapterClass {
            is_hardware: Some(true),
            is_integrated: Some(false),
        };
        let (is_software, device_type) = classify(DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32, facts);
        assert!(is_software);
        assert_eq!(device_type, GpuDeviceType::Cpu);
    }

    #[test]
    fn dxcore_can_call_an_unflagged_adapter_software() {
        let facts = AdapterClass {
            is_hardware: Some(false),
            is_integrated: None,
        };
        let (is_software, device_type) = classify(0, facts);
        assert!(is_software);
        assert_eq!(device_type, GpuDeviceType::Cpu);
    }

    #[test]
    fn dxcore_separates_integrated_from_discrete() {
        let integrated = AdapterClass {
            is_hardware: Some(true),
            is_integrated: Some(true),
        };
        assert_eq!(classify(0, integrated).1, GpuDeviceType::Integrated);

        let discrete = AdapterClass {
            is_hardware: Some(true),
            is_integrated: Some(false),
        };
        assert_eq!(classify(0, discrete).1, GpuDeviceType::Discrete);
    }

    #[test]
    fn an_unclassifiable_adapter_is_other_rather_than_a_guess() {
        let unknown = AdapterClass::default();
        assert_eq!(classify(0, unknown), (false, GpuDeviceType::Other));
    }
}
