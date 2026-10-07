//! Paint an external surface through the `surface()` element: the element-level path.
//!
//! ```sh
//! cargo run -p gpui --example surface
//! ```
//!
//! `path_a` reaches a surface through `GpuCanvas`'s paint-time callback. This paints one the way an
//! application does: a `surface(...)` element built during render and placed inside an ordinary
//! `div()`, so it lays out and stacks like any other child.
//!
//! A texture has to be made on the device the window's renderer draws through, so the producer asks
//! the window for that device (`Window::device_any`), creates an `ID3D11Texture2D` on it, clears it
//! to a known colour, and hands the **texture** to `surface(...)`. The producer makes no shader
//! resource view: the renderer — which owns the device — makes the view, which is the
//! `DirectXSource::Texture` arm, the ergonomic default.
//!
//! A second surface shows the **view** arm (`DirectXSource::View`): the producer makes the shader
//! resource view itself, which is what an application that already holds an
//! `ID3D11ShaderResourceView` — a Media Foundation decoder, a Direct3D engine — would hand over.
//!
//! Neither surface sets `object_fit`, so each source is fitted with the default, `ObjectFit::Contain`,
//! which letterboxes it inside the element's bounds without distorting it.
//!
//! Both arms are Direct3D 11's, so on every other host this example is a no-op. For the canvas-based
//! route to the same pixels, see `cargo run -p gpui --example path_a`.

#[cfg(not(target_os = "windows"))]
fn main() {
    // The Direct3D 11 surface arms `surface()` reaches on Windows are what this demonstrates;
    // there is nothing to show on a host whose renderer has no Direct3D device.
}

#[cfg(target_os = "windows")]
fn main() {
    demo::run();
}

#[cfg(target_os = "windows")]
mod demo {
    use anyhow::Context as _;
    use gpui::{
        App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
        prelude::*, px, rgb, size, surface,
    };
    use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    /// The producer's texture, which the sampler stretches over the surface's bounds.
    const TEXTURE: u32 = 256;

    /// The colour each arm is cleared to, in BGRA — the order the renderer's `B8G8R8A8_UNORM`
    /// target is in. Two known, distinct colours, so a window that shows both proves each arm's
    /// composite worked.
    const TEXTURE_ARM_COLOR: [f32; 4] = [0.10, 0.55, 0.95, 1.0];
    const VIEW_ARM_COLOR: [f32; 4] = [0.65, 0.85, 0.10, 1.0];

    pub fn run() {
        gpui::application().run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(640.), px(640.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| SurfaceExample::new()),
            )
            .expect("a window to composite into");
            cx.activate(true);
        });
    }

    struct SurfaceExample {
        /// `None` until the first render, when the window's renderer — and so its device — exists.
        producer: Option<Producer>,
    }

    impl SurfaceExample {
        fn new() -> Self {
            Self { producer: None }
        }
    }

    impl Render for SurfaceExample {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            // Built at first render rather than at construction: a texture has to be made on the
            // window's renderer's device, which exists once the window does.
            if self.producer.is_none() {
                self.producer = Some(Producer::new(window));
            }
            let producer = self.producer.as_ref().expect("the producer was just built");

            let mut frame = div()
                .size_full()
                .bg(rgb(0x101014))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4();

            match producer {
                Producer::DirectX(producer) => {
                    frame = frame.child(div().text_color(rgb(0xffffff)).child(
                        "An external Direct3D 11 texture, painted through surface() inside a div()",
                    ));
                    frame = frame.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_6()
                            // The texture arm: `surface()` carries the texture itself, and the
                            // renderer makes the view on the device it owns.
                            .child(panel(
                                surface(producer.texture_arm.clone()).size_full(),
                                "surface(texture) — the app hands the texture; the renderer makes the view",
                            ))
                            // The view arm: `surface()` carries a view the producer made.
                            .child(panel(
                                surface(producer.view_arm.clone()).size_full(),
                                "surface(view) — the app made the view",
                            )),
                    );
                }
                Producer::Unavailable(reason) => {
                    frame = frame.child(
                        div()
                            .text_color(rgb(0xffffff))
                            .child(format!("no Direct3D device: {reason}")),
                    );
                }
            }

            frame
        }
    }

    /// A square surface with a caption under it, so each arm's result is labelled the way it was
    /// produced.
    fn panel(content: impl IntoElement, caption: &'static str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .w(px(280.))
            .child(div().w(px(280.)).h(px(280.)).child(content))
            .child(div().text_color(rgb(0xd8d8d8)).text_center().child(caption))
    }

    /// The producer, which is either a live Direct3D producer or the reason there is none.
    enum Producer {
        DirectX(DirectXProducer),
        Unavailable(String),
    }

    impl Producer {
        fn new(window: &Window) -> Self {
            match DirectXProducer::new(window) {
                Ok(producer) => Self::DirectX(producer),
                Err(error) => {
                    log::error!("surface: cannot produce a Direct3D texture: {error:#}");
                    Self::Unavailable(format!("{error:#}"))
                }
            }
        }
    }

    /// The texture made on the window's renderer's device, and the two things `surface()` can carry.
    struct DirectXProducer {
        /// The texture arm's resource. The producer makes no view of it.
        texture_arm: ID3D11Texture2D,
        /// The view arm's resource: a view the producer made.
        view_arm: ID3D11ShaderResourceView,
    }

    impl DirectXProducer {
        fn new(window: &Window) -> anyhow::Result<Self> {
            // The renderer owns the device a surface texture has to be made on, and lends it.
            let device = window
                .device_any()
                .and_then(|any| any.downcast::<ID3D11Device>().ok())
                .context("the window's renderer lends an ID3D11Device")?;

            // The texture arm: the producer holds the resource and clears it to a known colour. It
            // makes no shader resource view — the renderer, which owns the device, makes that.
            let texture_arm = create_texture(&device)?;
            clear(&device, &texture_arm, TEXTURE_ARM_COLOR)?;
            log::info!("surface: the texture arm is cleared to {TEXTURE_ARM_COLOR:?} (BGRA)");

            // The view arm: the producer is the authority on its own format and makes the view
            // itself. An application that already holds an `ID3D11ShaderResourceView` hands that
            // over instead of making one here.
            let view_arm_texture = create_texture(&device)?;
            clear(&device, &view_arm_texture, VIEW_ARM_COLOR)?;
            let view_arm = shader_resource_view(&device, &view_arm_texture)?;
            log::info!("surface: the view arm is cleared to {VIEW_ARM_COLOR:?} (BGRA)");

            Ok(Self {
                texture_arm,
                view_arm,
            })
        }
    }

    /// An offscreen texture on `device`, in the one format the renderer's view rule accepts.
    ///
    /// `RENDER_TARGET` so it can be cleared; `SHADER_RESOURCE` because both arms sample it. The
    /// format is the renderer's own target format, `B8G8R8A8_UNORM`: the renderer views the texture
    /// as non-sRGB and the fragment samples its bytes straight through, so anything else would
    /// composite with its channels reordered rather than failing.
    fn create_texture(device: &ID3D11Device) -> anyhow::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: TEXTURE,
            Height: TEXTURE,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        texture.context("CreateTexture2D returned no texture")
    }

    /// Clears `texture` to `color`.
    ///
    /// A render target view is not a shader resource view, so clearing does not give the producer an
    /// SRV — the texture arm still hands over the resource alone. The immediate context is the one
    /// the renderer draws through, so the clear is ordered before the draw that samples this:
    /// nothing to submit and nothing to wait on.
    fn clear(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
        color: [f32; 4],
    ) -> anyhow::Result<()> {
        unsafe {
            let mut render_target = None;
            device.CreateRenderTargetView(texture, None, Some(&mut render_target))?;
            let render_target = render_target.context("CreateRenderTargetView returned no view")?;
            let context = device
                .GetImmediateContext()
                .context("the device has no immediate context")?;
            context.ClearRenderTargetView(&render_target, &color);
        }
        Ok(())
    }

    /// A non-sRGB `B8G8R8A8_UNORM` view of `texture`, the view arm's payload.
    fn shader_resource_view(
        device: &ID3D11Device,
        texture: &ID3D11Texture2D,
    ) -> anyhow::Result<ID3D11ShaderResourceView> {
        unsafe {
            let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };
            let mut view = None;
            device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view))?;
            view.context("CreateShaderResourceView returned no view")
        }
    }
}
