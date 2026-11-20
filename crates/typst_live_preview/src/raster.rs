//! Vello scene rasterization and GPU readback.
//!
//! Adapted from `tinymist-viewer`'s Vello renderer test suite (Apache-2.0).
//!
//! Vello 0.7 depends on `wgpu` 27, which is a different major version from the
//! `wgpu` GPUI uses. The two stacks cannot share a device, so this rasterizer
//! owns its own device and hands the UI plain RGBA pixels.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow};
use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::SmallVec;
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene,
    kurbo::{Affine, Rect, Size},
    peniko::{Brush, Color, Fill},
    util::RenderContext,
    wgpu::{
        BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Extent3d, MapMode, PollType,
        TexelCopyBufferInfo, TexelCopyBufferLayout, TextureDescriptor, TextureDimension,
        TextureFormat, TextureUsages, TextureViewDescriptor,
    },
};

/// Vello's coarse rasterizer supports at most 256 bins of 256x256 pixels.
///
/// A target that needs more makes Vello write outside its buffers, which has
/// hung entire GPUs. This is a hard backstop behind the view's page-size cap.
const VELLO_BIN_SIZE: u32 = 256;
const VELLO_MAX_BINS: u64 = 256;

/// How long the session thread waits for the GPU to finish a page.
///
/// A hung driver must not leave the thread blocked forever, because the
/// preview thread also owns the socket and process teardown. Ten seconds is
/// generous for an 8 MP page even on a software adapter; a longer wait only
/// lets incoming document frames pile up in memory.
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

fn vello_bins(width: u32, height: u32) -> u64 {
    u64::from(width.div_ceil(VELLO_BIN_SIZE)) * u64::from(height.div_ceil(VELLO_BIN_SIZE))
}

/// A rasterized page, in RGBA8 with straight alpha.
#[derive(Clone, Debug)]
pub struct RasterizedPage {
    /// The pixel data.
    pub image: RgbaImage,
    /// The device pixel size of the image.
    pub width: u32,
    /// The device pixel height of the image.
    pub height: u32,
    /// The scale the scene was rendered at.
    pub scale: f64,
}

impl RasterizedPage {
    /// Converts the page into GPUI's image buffer.
    ///
    /// Vello renders onto an opaque background, so premultiplied RGBA equals
    /// straight alpha here and only the channel order needs fixing: GPUI's
    /// sprite atlas wants BGRA.
    ///
    /// This is deliberately done on the preview session thread: it walks every
    /// pixel of the page, which is too much work to do while the editor is
    /// handling input.
    pub fn into_render_image(self) -> Arc<RenderImage> {
        let (width, height) = self.image.dimensions();
        // Swap the channels in place: copying into a second buffer first doubles
        // both the allocation and the memory traffic for every high-resolution
        // page, which is exactly the path a zoom-in takes.
        let mut pixels = self.image.into_raw();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        let buffer =
            image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(width, height, pixels)
                .expect("the BGRA buffer has the image's dimensions");
        Arc::new(RenderImage::new(SmallVec::from_elem(Frame::new(buffer), 1)))
    }
}

/// Rasterizes Vello scenes on a dedicated GPU device.
///
/// The device and renderer are created lazily on the first page and reused
/// afterwards. A rasterizer is not `Sync`; keep one per preview session and
/// drive it from a single thread.
pub struct SceneRasterizer {
    context: RenderContext,
    renderer: Option<Renderer>,
    device_id: Option<usize>,
}

impl Default for SceneRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneRasterizer {
    /// Creates a rasterizer with no device yet.
    pub fn new() -> Self {
        Self {
            context: RenderContext::new(),
            renderer: None,
            device_id: None,
        }
    }

    /// Creates the GPU device and compiles the shader pipelines ahead of the
    /// first page.
    ///
    /// Pipeline compilation is the slowest part of the very first render. The
    /// session thread calls this as soon as the preview socket connects, so the
    /// cost overlaps with Tinymist's first compile instead of delaying the
    /// first page after it.
    pub fn warm_up(&mut self) -> Result<()> {
        self.ensure_renderer()
    }

    fn ensure_renderer(&mut self) -> Result<()> {
        let device_id = match self.device_id {
            Some(device_id) => device_id,
            None => {
                let device_id = smol::block_on(self.context.device(None))
                    .context("no compatible WGPU device found for the Typst preview")?;
                self.device_id = Some(device_id);
                device_id
            }
        };
        if self.renderer.is_none() {
            let device = &self.context.devices[device_id].device;
            let renderer = Renderer::new(
                device,
                RendererOptions {
                    // Vello 0.7's GPU binning writes out of bounds when a
                    // target needs more than 256 bins (linebender/vello#680),
                    // which has hung the Intel and Apple graphics stacks. The
                    // page-size cap below keeps normal targets inside the
                    // limit, and the CPU coarse path is the configuration that
                    // is known to survive the edge cases.
                    use_cpu: true,
                    // One shader-compilation thread: the default heuristic is
                    // known to misbehave on macOS, and the difference here is
                    // a one-time cost per session.
                    num_init_threads: NonZeroUsize::new(1),
                    antialiasing_support: AaSupport::area_only(),
                    ..Default::default()
                },
            )
            .map_err(|err| anyhow!("failed to create the Vello renderer: {err}"))?;
            self.renderer = Some(renderer);
        }
        Ok(())
    }

    /// Renders `scene` at `scale` device pixels per page point.
    ///
    /// `page_size` is in Typst page points. The page scene is transformed by
    /// `scale`; `background` fills the full target and is also the renderer's
    /// base color.
    pub fn render_page(
        &mut self,
        scene: &Scene,
        page_size: Size,
        scale: f64,
        background: Color,
    ) -> Result<RasterizedPage> {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let width = (page_size.width * scale).round().max(1.0);
        let height = (page_size.height * scale).round().max(1.0);
        anyhow::ensure!(
            width <= f64::from(u32::MAX) && height <= f64::from(u32::MAX),
            "page is too large to rasterize: {width}x{height}"
        );
        let (width, height) = (width as u32, height as u32);
        let bins = vello_bins(width, height);
        anyhow::ensure!(
            bins <= VELLO_MAX_BINS,
            "page raster {width}x{height} px needs {bins} Vello bins; the maximum is {VELLO_MAX_BINS}"
        );

        self.ensure_renderer()?;
        let device_id = self.device_id.expect("ensure_renderer sets the device id");
        let device_handle = &mut self.context.devices[device_id];
        let device = &device_handle.device;
        let queue = &device_handle.queue;

        let limits = device.limits();
        anyhow::ensure!(
            width <= limits.max_texture_dimension_2d && height <= limits.max_texture_dimension_2d,
            "page raster {width}x{height} px exceeds the GPU's maximum texture dimension {}",
            limits.max_texture_dimension_2d
        );
        let mut renderer = self
            .renderer
            .take()
            .expect("ensure_renderer installs the renderer");

        let mut target_scene = Scene::new();
        target_scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Brush::Solid(background),
            None,
            &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        target_scene.append(scene, Some(Affine::scale(scale)));

        let size = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&TextureDescriptor {
            label: Some("typst-live-preview-target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&TextureViewDescriptor::default());
        let render_params = RenderParams {
            base_color: background,
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };

        renderer
            .render_to_texture(device, queue, &target_scene, &view, &render_params)
            .map_err(|err| anyhow!("failed to render the Vello scene: {err}"))?;

        let padded_byte_width = (width * 4).next_multiple_of(256);
        let buffer_size = u64::from(padded_byte_width) * u64::from(height);
        anyhow::ensure!(
            buffer_size <= limits.max_buffer_size,
            "Vello readback needs {buffer_size} bytes, over the GPU's maximum buffer size {}",
            limits.max_buffer_size
        );
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("typst-live-preview-readback"),
            size: buffer_size,
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("typst-live-preview-copy"),
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            TexelCopyBufferInfo {
                buffer: &buffer,
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_byte_width),
                    rows_per_image: None,
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        // A lost or hung device must not block the session thread forever: the
        // editor's socket, timers, and teardown all live on this thread.
        device
            .poll(PollType::Wait {
                submission_index: None,
                timeout: Some(GPU_WAIT_TIMEOUT),
            })
            .map_err(|err| anyhow!("the GPU did not finish the page: {err}"))?;
        receiver
            .recv_timeout(GPU_WAIT_TIMEOUT)
            .context("the GPU never completed the Vello readback")?
            .map_err(|err| anyhow!("failed to map the Vello readback buffer: {err}"))?;

        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * padded_byte_width) as usize;
            pixels.extend_from_slice(&data[start..start + (width * 4) as usize]);
        }
        drop(data);
        buffer.unmap();

        self.renderer = Some(renderer);

        RgbaImage::from_vec(width, height, pixels)
            .map(|image| RasterizedPage {
                image,
                width,
                height,
                scale,
            })
            .context("failed to build the page image")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_image_conversion_swaps_red_and_blue() {
        let image = RgbaImage::from_raw(1, 1, vec![10, 20, 30, 255]).unwrap();
        let page = RasterizedPage {
            image,
            width: 1,
            height: 1,
            scale: 1.0,
        };

        let image = page.into_render_image();

        assert_eq!(image.as_bytes(0), Some(&[30, 20, 10, 255][..]));
    }

    #[test]
    fn vello_bin_limit_catches_the_crash_size() {
        // The target that took down the Intel driver: 14 * 19 = 266 bins.
        assert_eq!(vello_bins(3423, 4841), 266);
        assert_eq!(vello_bins(4096, 4096), 256);
        assert!(vello_bins(4097, 4096) > 256);
        assert_eq!(vello_bins(1, 1), 1);
    }
}
