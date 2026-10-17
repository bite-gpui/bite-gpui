//! GPU capability information shared by `gpui` and its platform backends.

/// Information about the GPU GPUI is running on.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct GpuSpecs {
    /// Whether the GPU is really a fake (like `llvmpipe`) running on the CPU.
    pub is_software_emulated: bool,
    /// The name of the device, as reported by Vulkan.
    pub device_name: String,
    /// The name of the driver, as reported by Vulkan.
    pub driver_name: String,
    /// Further information about the driver, as reported by Vulkan.
    pub driver_info: String,
}

/// The identity of an adapter, for [`GpuPreference::Device`].
///
/// This is the *selection* identity — PCI vendor and device. The producer-facing identity a
/// non-Vulkan producer matches (a device name and its DRM render node) is `gpui_interop::GpuId`;
/// mapping one to the other is the enumeration step of decision `0008`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuId {
    /// The PCI vendor id, when it is known. `ZED_DEVICE_ID` names only a device, so a preference
    /// lowered from it leaves this `None`.
    pub vendor_id: Option<u32>,
    /// The PCI device id.
    pub device_id: u32,
}

impl GpuId {
    /// A `GpuId` matched on the device id alone.
    pub const fn by_device(device_id: u32) -> Self {
        Self {
            vendor_id: None,
            device_id,
        }
    }

    /// Whether an adapter with this vendor/device pair is the one this id names.
    pub fn matches(&self, vendor_id: u32, device_id: u32) -> bool {
        self.device_id == device_id && self.vendor_id.is_none_or(|vendor| vendor == vendor_id)
    }

    /// Parse a four-hex-digit PCI device id, such as a debug override carries.
    ///
    /// The id is hexadecimal; a decimal-looking value such as `6422` parses as the hex `0x6422`,
    /// matches nothing, and silently falls through to the default adapter.
    pub fn parse_pci_id(value: &str) -> Option<u32> {
        let id = value.trim();
        let id = id
            .strip_prefix("0x")
            .or_else(|| id.strip_prefix("0X"))
            .unwrap_or(id);
        if id.len() == 4 && id.chars().all(|character| character.is_ascii_hexdigit()) {
            u32::from_str_radix(id, 16).ok()
        } else {
            None
        }
    }
}

/// Which GPU an application prefers the renderer to run on.
///
/// The device is process-wide and chosen once, before the first window (decision `0008`), so this is
/// stated on the application rather than per window. The default is [`LowPower`](Self::LowPower): on
/// a hybrid laptop the panel is wired to the integrated GPU, so rendering on the discrete one keeps
/// it powered for the window's lifetime and crosses PCIe on every frame.
#[derive(Clone, Default)]
pub enum GpuPreference {
    /// Today's ranking: the compositor's hint, then device type (discrete, integrated, other,
    /// virtual, CPU), then backend.
    Auto,
    /// Prefer the integrated adapter. The default.
    #[default]
    LowPower,
    /// Prefer the discrete adapter.
    HighPerformance,
    /// A named adapter, matched by PCI identity. One that is not present falls back to [`Auto`].
    Device(GpuId),
    /// A predicate that picks **one** adapter from the enumerated list.
    ///
    /// The closure receives every adapter the platform reports and returns the index of the one it
    /// wants, or `None` when none will do. It chooses a GPU; it does not rank them.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use gpui_platform::{GpuAdapterInfo, GpuDeviceType, GpuPreference};
    ///
    /// let preference = GpuPreference::Custom(Arc::new(|gpus: &[GpuAdapterInfo]| {
    ///     // A producer that must share a dma-buf with the renderer names its GPU first, and
    ///     // otherwise takes the integrated one — where the panel is wired on a hybrid laptop.
    ///     gpus.iter()
    ///         .position(|gpu| gpu.device_id == 0x1346)
    ///         .or_else(|| {
    ///             gpus.iter()
    ///                 .position(|gpu| gpu.device_type == GpuDeviceType::Integrated)
    ///         })
    /// }));
    /// ```
    ///
    /// The adapter named is tried first. If the display cannot be configured on it, the remaining
    /// adapters follow in [`Auto`](Self::Auto)'s order — this is a preference, not a pin. An
    /// application that must know what it got reads [`GpuAdapterInfo`] back through the app's
    /// active-GPU accessor.
    ///
    /// `None`, or an index outside the list, fails the window with an error naming the shortfall: the
    /// application saw the same list, so the two disagreeing is a bug worth surfacing rather than
    /// silently drawing on a GPU the application did not choose.
    Custom(std::sync::Arc<dyn Fn(&[GpuAdapterInfo]) -> Option<usize> + Send + Sync>),
}

impl std::fmt::Debug for GpuPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("Auto"),
            Self::LowPower => f.write_str("LowPower"),
            Self::HighPerformance => f.write_str("HighPerformance"),
            Self::Device(id) => f.debug_tuple("Device").field(id).finish(),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// The class of adapter a [`GpuAdapterInfo`] describes, without naming `wgpu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuDeviceType {
    /// A discrete GPU.
    Discrete,
    /// An integrated GPU.
    Integrated,
    /// A virtual GPU.
    Virtual,
    /// A software (CPU) implementation.
    Cpu,
    /// An adapter that is none of the above.
    Other,
}

/// The graphics API a [`GpuAdapterInfo`] runs on, without naming `wgpu`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBackend {
    /// Vulkan.
    Vulkan,
    /// Metal.
    Metal,
    /// Direct3D 12.
    Dx12,
    /// OpenGL or OpenGL ES.
    Gl,
    /// WebGPU in a browser.
    BrowserWebGpu,
    /// Anything else.
    Other,
}

/// One adapter an application can choose between.
///
/// Deliberately identity and class only. A `wgpu::Features` set and a dma-buf format/modifier list
/// both need a device, and creating one at enumeration time is the cost enumeration exists to avoid;
/// what a chosen device will import or export is the interop crate's report, asked on a device the
/// caller already has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuAdapterInfo {
    /// The adapter name the driver reports.
    pub name: String,
    /// The adapter's class.
    pub device_type: GpuDeviceType,
    /// The PCI vendor id.
    pub vendor_id: u32,
    /// The PCI device id.
    pub device_id: u32,
    /// The graphics API the adapter runs on.
    pub backend: GpuBackend,
    /// Whether the adapter is a software implementation.
    pub is_software: bool,
}

#[cfg(test)]
mod tests {
    use super::{GpuId, GpuPreference};

    #[test]
    fn the_pci_id_is_four_hex_digits_with_an_optional_prefix() {
        assert_eq!(GpuId::parse_pci_id("0xABCD"), Some(0xabcd));
        assert_eq!(GpuId::parse_pci_id("ABCD"), Some(0xabcd));
        assert_eq!(GpuId::parse_pci_id("abcd"), Some(0xabcd));
        assert_eq!(GpuId::parse_pci_id("1234"), Some(0x1234));
        assert_eq!(GpuId::parse_pci_id(" 0x1346 "), Some(0x1346));
        assert_eq!(GpuId::parse_pci_id("123"), None);
        assert_eq!(GpuId::parse_pci_id("0x12345"), None);
        assert_eq!(GpuId::parse_pci_id("zzzz"), None);
    }

    #[test]
    fn a_device_id_matches_by_device_and_leaves_the_vendor_open() {
        let by_device = GpuId::by_device(0x1346);
        assert!(by_device.matches(0x10de, 0x1346));
        assert!(by_device.matches(0x8086, 0x1346));
        assert!(!by_device.matches(0x10de, 0x1916));

        let exact = GpuId {
            vendor_id: Some(0x10de),
            device_id: 0x1346,
        };
        assert!(exact.matches(0x10de, 0x1346));
        assert!(!exact.matches(0x8086, 0x1346));
    }

    #[test]
    fn the_default_preference_is_low_power() {
        assert!(matches!(GpuPreference::default(), GpuPreference::LowPower));
    }

    #[test]
    fn a_custom_preference_is_named_in_debug() {
        use std::sync::Arc;

        let preference = GpuPreference::Custom(Arc::new(|_| Some(0)));
        assert_eq!(format!("{preference:?}"), "Custom(..)");
    }
}
