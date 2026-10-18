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
/// It is two facts, not one. `permanent` is what a config stores and what should still name the
/// same hardware after a restart; `session` is what the backend can match in this process. A
/// per-process handle (a LUID, a Metal registry id, a DRM node) does not survive a reboot, and a
/// PCI pair does not distinguish two identical cards — so neither alone is a complete identity.
///
/// The permanent part is serialised; the session is not, because a config cannot know it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GpuId {
    /// What survives a restart, where the platform has anything that does.
    pub permanent: PermanentGpuId,
    /// The backend's handle for this adapter in this process. `None` for a stored id.
    #[serde(skip)]
    pub session: Option<SessionGpuId>,
}

impl GpuId {
    /// A `GpuId` matched on the PCI device id alone, with no session.
    ///
    /// This is what `ZED_DEVICE_ID` lowers to: the variable names a device only, so the vendor is
    /// left open and the permanent part is the whole preference.
    pub const fn by_device(device_id: u32) -> Self {
        Self {
            permanent: PermanentGpuId::Pci {
                vendor_id: None,
                device_id,
                subsys_id: None,
                revision: None,
            },
            session: None,
        }
    }

    /// The permanent and session identity of a PCI adapter.
    pub const fn pci(vendor_id: u32, device_id: u32) -> Self {
        Self {
            permanent: PermanentGpuId::Pci {
                vendor_id: Some(vendor_id),
                device_id,
                subsys_id: None,
                revision: None,
            },
            session: Some(SessionGpuId::Pci {
                vendor_id,
                device_id,
            }),
        }
    }

    /// The identity of the one adapter on a single-GPU platform.
    pub const fn single() -> Self {
        Self {
            permanent: PermanentGpuId::Single,
            session: Some(SessionGpuId::Default),
        }
    }

    /// Whether `adapter` is the one this id names.
    ///
    /// A session is the stronger claim: when it is present, only the adapter with that session
    /// matches. Without one — a config, or a caller that knows only the model — the permanent part
    /// decides, and two identical cards both match.
    pub fn matches(&self, adapter: &GpuAdapterInfo) -> bool {
        match &self.session {
            Some(session) => adapter.id.session.as_ref() == Some(session),
            None => self.permanent.matches(&adapter.id.permanent),
        }
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

/// The stable part of a [`GpuId`]. Every adapter `enumerate_gpus` reports carries one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum PermanentGpuId {
    /// PCI vendor/device, plus the subsystem/revision that distinguishes board variants.
    ///
    /// Stable across restarts; two identical cards share it. `None` is a wildcard — `ZED_DEVICE_ID`
    /// names a device only.
    Pci {
        /// The PCI vendor id, or `None` when the id names a device only.
        vendor_id: Option<u32>,
        /// The PCI device id.
        device_id: u32,
        /// The PCI subsystem id, when the backend reports one.
        subsys_id: Option<u32>,
        /// The PCI revision, when the backend reports one.
        revision: Option<u32>,
    },
    /// The Windows PnP device instance id — the one documented persistent per-adapter identity.
    /// DXGI does not expose it; a later `SetupAPI` read fills it in.
    Pnp(String),
    /// The adapter/driver name, where the platform has nothing better. Best-effort: two identical
    /// cards share it.
    Name(String),
    /// A platform with one GPU, or a backend that cannot enumerate: there is only "the GPU".
    Single,
}

impl PermanentGpuId {
    /// Whether an adapter's permanent part is the one this id names.
    ///
    /// An `Option` field that is `None` in `self` is a wildcard; one that is `None` in `other` and
    /// required by `self` does not match, because the backend cannot confirm it.
    pub fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Pci {
                    vendor_id,
                    device_id,
                    subsys_id,
                    revision,
                },
                Self::Pci {
                    vendor_id: other_vendor,
                    device_id: other_device,
                    subsys_id: other_subsys,
                    revision: other_revision,
                },
            ) => {
                wildcard(*vendor_id, *other_vendor)
                    && device_id == other_device
                    && wildcard(*subsys_id, *other_subsys)
                    && wildcard(*revision, *other_revision)
            }
            (Self::Pnp(want), Self::Pnp(have)) => want == have,
            (Self::Name(want), Self::Name(have)) => want == have,
            (Self::Single, Self::Single) => true,
            _ => false,
        }
    }

    /// The PCI vendor id, when this id has one.
    pub fn vendor_id(&self) -> Option<u32> {
        match self {
            Self::Pci { vendor_id, .. } => *vendor_id,
            _ => None,
        }
    }

    /// The PCI device id, when this id has one.
    pub fn device_id(&self) -> Option<u32> {
        match self {
            Self::Pci { device_id, .. } => Some(*device_id),
            _ => None,
        }
    }
}

fn wildcard(want: Option<u32>, have: Option<u32>) -> bool {
    want.is_none_or(|want| have == Some(want))
}

/// The backend's handle for an adapter in this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionGpuId {
    /// A DRM render node (Linux). The node number is assigned per boot and is not a hardware id.
    Drm {
        /// The node's minor number: `renderD<minor>`.
        render_minor: u32,
    },
    /// A DXGI adapter LUID (Windows), packed from `Luid.HighPart`/`LowPart`. It lasts for the
    /// adapter's lifetime and changes on reboot, driver upgrade, or device disable/enable.
    Luid(u64),
    /// A Metal registry id (macOS) — an IOKit registry entry id, valid only until reboot.
    Metal {
        /// The IOKit registry id.
        registry_id: u64,
    },
    /// The PCI pair, where the backend has no per-boot handle; identical cards share it.
    Pci {
        /// The PCI vendor id.
        vendor_id: u32,
        /// The PCI device id.
        device_id: u32,
    },
    /// A platform with one GPU, or a backend whose enumeration cannot name one.
    Default,
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
    /// A named adapter, matched by its permanent part and, when it carries one, its session handle.
    /// One that is not present falls back to [`Auto`].
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
    ///         .position(|gpu| gpu.device_id() == Some(0x1346))
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
    /// The permanent and session identity of the adapter.
    pub id: GpuId,
    /// The adapter name the driver reports.
    pub name: String,
    /// The adapter's class.
    pub device_type: GpuDeviceType,
    /// The graphics API the adapter runs on.
    pub backend: GpuBackend,
    /// Whether the adapter is a software implementation.
    pub is_software: bool,
}

impl GpuAdapterInfo {
    /// Describe one adapter the backend enumerated.
    pub fn new(
        id: GpuId,
        name: impl Into<String>,
        device_type: GpuDeviceType,
        backend: GpuBackend,
        is_software: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            device_type,
            backend,
            is_software,
        }
    }

    /// The one adapter a platform without enumeration reports.
    pub fn single() -> Self {
        Self::new(
            GpuId::single(),
            "default",
            GpuDeviceType::Other,
            GpuBackend::Other,
            false,
        )
    }

    /// The PCI vendor id, when this adapter has one.
    pub fn vendor_id(&self) -> Option<u32> {
        self.id.permanent.vendor_id()
    }

    /// The PCI device id, when this adapter has one.
    pub fn device_id(&self) -> Option<u32> {
        self.id.permanent.device_id()
    }
}

/// The sort key for one adapter under a [`GpuPreference`].
///
/// Slots are compared in order: the named match, the preference, the compositor match and the
/// backend. The meanings are documented on [`rank`]. The fields are private so a backend cannot
/// invent an order the shared ranking does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GpuRank {
    matched: u8,
    preference: u8,
    compositor: u8,
    backend: u8,
}

impl GpuRank {
    fn new(matched: u8, preference: u8, compositor: u8, backend: u8) -> Self {
        Self {
            matched,
            preference,
            compositor,
            backend,
        }
    }
}

/// Rank one adapter under `preference`.
///
/// `LowPower`, `HighPerformance`, `Device` and `Custom` are portable; `Auto` leaves the ordering to
/// the backend, so callers establish their own order first and a stable sort under this key
/// preserves it. `compositor` is the Linux compositor hint, expressed in the same identity type; a
/// backend with no equivalent passes `None`.
pub fn rank(
    adapter: &GpuAdapterInfo,
    preference: &GpuPreference,
    compositor: Option<&PermanentGpuId>,
) -> GpuRank {
    let compositor_match: u8 = match compositor {
        Some(hint) if hint.matches(&adapter.id.permanent) => 0,
        _ => 1,
    };
    let auto_type: u8 = match adapter.device_type {
        GpuDeviceType::Discrete => 0,
        GpuDeviceType::Integrated => 1,
        GpuDeviceType::Other => 2,
        GpuDeviceType::Virtual => 3,
        GpuDeviceType::Cpu => 4,
    };
    let backend_priority: u8 = match adapter.backend {
        GpuBackend::Vulkan | GpuBackend::Metal | GpuBackend::Dx12 => 0,
        _ => 1,
    };
    match preference {
        GpuPreference::Auto => GpuRank::new(0, compositor_match, auto_type, backend_priority),
        GpuPreference::Device(gpu_id) => GpuRank::new(
            if gpu_id.matches(adapter) { 0 } else { 1 },
            compositor_match,
            auto_type,
            backend_priority,
        ),
        GpuPreference::LowPower => GpuRank::new(
            0,
            type_priority(adapter.device_type, true),
            compositor_match,
            backend_priority,
        ),
        GpuPreference::HighPerformance => GpuRank::new(
            0,
            type_priority(adapter.device_type, false),
            compositor_match,
            backend_priority,
        ),
        // Resolved by `custom_choice` before any sorting; equal keys keep the backend's order.
        GpuPreference::Custom(_) => GpuRank::new(0, 0, 0, 0),
    }
}

/// The device-type order under an explicit power preference. A CPU adapter ranks last in both: it is
/// a fallback, not a low-power device.
fn type_priority(device_type: GpuDeviceType, low_power: bool) -> u8 {
    match (device_type, low_power) {
        (GpuDeviceType::Integrated, true) => 0,
        (GpuDeviceType::Discrete, false) => 0,
        (GpuDeviceType::Other, _) => 1,
        (GpuDeviceType::Integrated, false) => 2,
        (GpuDeviceType::Discrete, true) => 2,
        (GpuDeviceType::Virtual, _) => 3,
        (GpuDeviceType::Cpu, _) => 4,
    }
}

/// Resolve a [`GpuPreference::Custom`] selector against the adapters that were enumerated.
///
/// The selector chooses one adapter out of the whole list — it does not rank them. `None`, or an
/// index outside the list, is an error rather than a fallback: the application and this code saw the
/// same enumeration, so a disagreement is a bug worth naming instead of silently drawing on a GPU
/// the application did not choose.
pub fn custom_choice(
    adapters: &[GpuAdapterInfo],
    selector: &dyn Fn(&[GpuAdapterInfo]) -> Option<usize>,
) -> anyhow::Result<usize> {
    match selector(adapters) {
        Some(index) if index < adapters.len() => Ok(index),
        Some(index) => anyhow::bail!(
            "The application's custom GPU preference chose adapter {index}, but only {} adapter(s) \
             were enumerated",
            adapters.len()
        ),
        None => anyhow::bail!("The application's custom GPU preference chose no adapter"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        GpuAdapterInfo, GpuBackend, GpuDeviceType, GpuId, GpuPreference, PermanentGpuId,
        SessionGpuId, custom_choice, rank,
    };

    fn adapter(name: &str, device_type: GpuDeviceType, vendor: u32, device: u32) -> GpuAdapterInfo {
        GpuAdapterInfo::new(
            GpuId::pci(vendor, device),
            name,
            device_type,
            GpuBackend::Vulkan,
            false,
        )
    }

    fn hint(vendor: u32, device: u32) -> PermanentGpuId {
        PermanentGpuId::Pci {
            vendor_id: Some(vendor),
            device_id: device,
            subsys_id: None,
            revision: None,
        }
    }

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
        let integrated = adapter("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916);
        let discrete = adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);

        let by_device = GpuId::by_device(0x1346);
        assert!(by_device.matches(&discrete));
        assert!(!by_device.matches(&integrated));

        let exact = GpuId {
            permanent: hint(0x10de, 0x1346),
            session: None,
        };
        assert!(exact.matches(&discrete));
        assert!(!exact.matches(&integrated));
    }

    #[test]
    fn a_permanent_id_survives_a_restart_and_a_session_does_not() {
        let mut enumerated = adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);

        let stored = GpuId {
            permanent: enumerated.id.permanent.clone(),
            session: None,
        };
        assert!(
            stored.matches(&enumerated),
            "the permanent part still names the card after the session is gone"
        );

        enumerated.id.session = Some(SessionGpuId::Luid(1));
        let stale = GpuId {
            permanent: enumerated.id.permanent.clone(),
            session: Some(SessionGpuId::Luid(2)),
        };
        assert!(
            !stale.matches(&enumerated),
            "a session from another boot is not the same card"
        );
    }

    #[test]
    fn a_gpu_id_serialises_without_its_session() {
        let enumerated = adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);
        let json = serde_json::to_value(&enumerated.id).expect("serialise a GpuId");
        assert!(
            json.get("session").is_none(),
            "the session is per process and must not reach a config: {json}"
        );

        let restored: GpuId = serde_json::from_value(json).expect("deserialise a GpuId");
        assert_eq!(restored.session, None);
        assert!(restored.matches(&enumerated));
    }

    #[test]
    fn a_single_gpu_platform_reports_one_identity() {
        let only = GpuAdapterInfo::single();
        assert_eq!(only.id.permanent, PermanentGpuId::Single);
        assert_eq!(only.id.session, Some(SessionGpuId::Default));
        assert!(GpuId::single().matches(&only));
        assert!(
            GpuId {
                permanent: PermanentGpuId::Single,
                session: None,
            }
            .matches(&only)
        );
    }

    #[test]
    fn the_preference_orders_the_adapters_it_names() {
        use super::GpuPreference;

        let integrated = adapter("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916);
        let discrete = adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);
        let on_the_discrete = hint(0x10de, 0x1346);

        assert!(
            rank(
                &integrated,
                &GpuPreference::LowPower,
                Some(&on_the_discrete)
            ) < rank(&discrete, &GpuPreference::LowPower, Some(&on_the_discrete)),
            "LowPower takes the integrated adapter even when the compositor is on the discrete one",
        );
        assert!(
            rank(
                &discrete,
                &GpuPreference::HighPerformance,
                Some(&on_the_discrete)
            ) < rank(
                &integrated,
                &GpuPreference::HighPerformance,
                Some(&on_the_discrete)
            ),
            "HighPerformance takes the discrete adapter",
        );
        assert!(
            rank(&discrete, &GpuPreference::Auto, None)
                < rank(&integrated, &GpuPreference::Auto, None),
            "Auto keeps the historical discrete-first order",
        );
        assert!(
            rank(
                &integrated,
                &GpuPreference::Auto,
                Some(&hint(0x8086, 0x1916))
            ) < rank(&discrete, &GpuPreference::Auto, Some(&hint(0x8086, 0x1916))),
            "Auto still follows the compositor's hint",
        );
    }

    #[test]
    fn a_named_device_ranks_first_and_an_absent_one_falls_back() {
        let integrated = adapter("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916);
        let discrete = adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);

        let named = GpuPreference::Device(GpuId {
            permanent: hint(0x8086, 0x1916),
            session: None,
        });
        assert!(rank(&integrated, &named, None) < rank(&discrete, &named, None));

        let absent = GpuPreference::Device(GpuId::by_device(0x9999));
        assert!(
            rank(&discrete, &absent, None) < rank(&integrated, &absent, None),
            "an absent device falls back to Auto's discrete-first order",
        );
    }

    #[test]
    fn a_custom_preference_chooses_one_adapter() {
        let adapters = [
            adapter("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916),
            adapter("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346),
        ];

        let choose_discrete =
            |gpus: &[GpuAdapterInfo]| gpus.iter().position(|gpu| gpu.device_id() == Some(0x1346));
        assert_eq!(custom_choice(&adapters, &choose_discrete).unwrap(), 1);

        let chose_nothing = |_: &[GpuAdapterInfo]| None;
        assert!(
            custom_choice(&adapters, &chose_nothing).is_err(),
            "choosing no adapter is an error, not a silent fallback",
        );

        let out_of_range = |_: &[GpuAdapterInfo]| Some(2);
        let error =
            custom_choice(&adapters, &out_of_range).expect_err("an index past the list is refused");
        assert!(
            error.to_string().contains("only 2 adapter(s)"),
            "the refusal names the shortfall: {error}",
        );
    }

    #[test]
    fn the_default_preference_is_low_power() {
        assert!(matches!(GpuPreference::default(), GpuPreference::LowPower));
    }

    #[test]
    fn a_custom_preference_is_named_in_debug() {
        let preference = GpuPreference::Custom(Arc::new(|_| Some(0)));
        assert_eq!(format!("{preference:?}"), "Custom(..)");
    }
}
