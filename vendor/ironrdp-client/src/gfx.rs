//! rdpvncdesk patch (issue #9): RDP Graphics Pipeline (RDPGFX) support.
//!
//! Current GNOME Remote Desktop refuses clients without RDPGFX. `ironrdp-egfx` handles the
//! channel protocol (capabilities, surfaces, frame acknowledgement); this module composes what it
//! reports into one framebuffer: uncompressed/AVC bitmaps, solid fills, surface-to-surface and
//! cache copies, and RemoteFX Progressive tiles (GNOME's codec without a hardware encoder),
//! decoded with `ironrdp-graphics`.
//!
//! One session runs per process, so the state is a process-wide singleton that the session loop
//! reads when a frame completes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ironrdp_egfx::client::{BitmapUpdate, GraphicsPipelineClient, GraphicsPipelineHandler, Surface};
use ironrdp_egfx::pdu::{
    CacheToSurfacePdu, Codec1Type, Codec2Type, EvictCacheEntryPdu, GfxPdu, SolidFillPdu, SurfaceToCachePdu,
    SurfaceToSurfacePdu, WireToSurface1Pdu, WireToSurface2Pdu,
};
use ironrdp_graphics::clearcodec::ClearCodecDecoder;
use ironrdp_graphics::progressive::{SurfaceTiles, TileState};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_pdu::codecs::rfx::progressive::{
    ComponentCodecQuant, ProgressiveBlock, ProgressiveRegion, ProgressiveTile, decode_progressive_stream,
};
use tokio::sync::Notify;
use tracing::{debug, warn};

/// 0RGB pixels, row-major.
struct Pixels {
    width: u16,
    height: u16,
    data: Vec<u32>,
}

impl Pixels {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            data: vec![0; usize::from(width) * usize::from(height)],
        }
    }

    fn fill(&mut self, left: u16, top: u16, right: u16, bottom: u16, color: u32) {
        let (right, bottom) = (right.min(self.width), bottom.min(self.height));
        for y in top..bottom {
            let row = usize::from(y) * usize::from(self.width);
            self.data[row + usize::from(left)..row + usize::from(right.max(left))].fill(color);
        }
    }

    /// Copies a `w`×`h` block from `src` at (sx, sy) to (dx, dy), clipped to both.
    fn blit(&mut self, dx: u16, dy: u16, src: &Pixels, sx: u16, sy: u16, w: u16, h: u16) {
        let w = w.min(src.width.saturating_sub(sx)).min(self.width.saturating_sub(dx));
        let h = h.min(src.height.saturating_sub(sy)).min(self.height.saturating_sub(dy));
        for row in 0..h {
            let s = usize::from(sy + row) * usize::from(src.width) + usize::from(sx);
            let d = usize::from(dy + row) * usize::from(self.width) + usize::from(dx);
            self.data[d..d + usize::from(w)].copy_from_slice(&src.data[s..s + usize::from(w)]);
        }
    }

    fn crop(&self, left: u16, top: u16, right: u16, bottom: u16) -> Pixels {
        let mut out = Pixels::new(right.saturating_sub(left), bottom.saturating_sub(top));
        let (w, h) = (out.width, out.height);
        out.blit(0, 0, self, left, top, w, h);
        out
    }
}

struct GfxSurface {
    pixels: Pixels,
    origin: Option<(u32, u32)>,
    progressive: Option<SurfaceTiles>,
}

#[derive(Default)]
pub(crate) struct GfxState {
    clearcodec: ClearCodecDecoder,
    planar: BitmapStreamDecoder,
    output: Option<Pixels>,
    surfaces: HashMap<u16, GfxSurface>,
    cache: HashMap<u16, Pixels>,
    /// A frame completed since the session loop last took the output.
    dirty: bool,
}

impl GfxState {
    /// Copies the changed part of a mapped surface into the output.
    fn present(&mut self, surface_id: u16, left: u16, top: u16, right: u16, bottom: u16) {
        let Some(surface) = self.surfaces.get(&surface_id) else { return };
        let (Some((ox, oy)), Some(output)) = (surface.origin, self.output.as_mut()) else {
            return;
        };
        let (Ok(ox), Ok(oy)) = (u16::try_from(ox), u16::try_from(oy)) else { return };
        output.blit(
            ox.saturating_add(left),
            oy.saturating_add(top),
            &surface.pixels,
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        );
    }

    fn write_tile(&mut self, surface_id: u16, x: u16, y: u16, rgba: &[u8], clip: &[(u16, u16, u16, u16)]) {
        let Some(surface) = self.surfaces.get_mut(&surface_id) else { return };
        let mut tile = Pixels::new(64, 64);
        for (i, px) in rgba.chunks_exact(4).enumerate().take(64 * 64) {
            tile.data[i] = u32::from_be_bytes([0, px[0], px[1], px[2]]);
        }
        let mut touched = Vec::new();
        for &(rl, rt, rr, rb) in clip {
            let (l, t) = (rl.max(x), rt.max(y));
            let (r, b) = (rr.min(x.saturating_add(64)), rb.min(y.saturating_add(64)));
            if l < r && t < b {
                surface.pixels.blit(l, t, &tile, l - x, t - y, r - l, b - t);
                touched.push((l, t, r, b));
            }
        }
        for (l, t, r, b) in touched {
            self.present(surface_id, l, t, r, b);
        }
    }

    /// Codecs `ironrdp-egfx` leaves to the handler: ClearCodec (Windows text/UI) and Planar.
    fn wire_to_surface1(&mut self, pdu: &WireToSurface1Pdu) -> Result<(), String> {
        let r = &pdu.destination_rectangle;
        let (w, h) = (r.right.saturating_sub(r.left), r.bottom.saturating_sub(r.top));
        let mut block = Pixels::new(w, h);
        match pdu.codec_id {
            Codec1Type::ClearCodec => {
                let bgra = self
                    .clearcodec
                    .decode(&pdu.bitmap_data, w, h)
                    .map_err(|e| e.to_string())?;
                for (i, px) in bgra.chunks_exact(4).enumerate().take(block.data.len()) {
                    block.data[i] = u32::from_be_bytes([0, px[2], px[1], px[0]]);
                }
            }
            Codec1Type::Planar => {
                let mut rgb = Vec::new();
                self.planar
                    .decode_bitmap_stream_to_rgb24(&pdu.bitmap_data, &mut rgb, usize::from(w), usize::from(h))
                    .map_err(|e| e.to_string())?;
                for (i, px) in rgb.chunks_exact(3).enumerate().take(block.data.len()) {
                    block.data[i] = u32::from_be_bytes([0, px[0], px[1], px[2]]);
                }
            }
            other => return Err(format!("unsupported codec {other:?}")),
        }
        let Some(surface) = self.surfaces.get_mut(&pdu.surface_id) else {
            return Err(format!("unknown surface {}", pdu.surface_id));
        };
        surface.pixels.blit(r.left, r.top, &block, 0, 0, w, h);
        self.present(pdu.surface_id, r.left, r.top, r.right, r.bottom);
        Ok(())
    }

    fn progressive(&mut self, pdu: &WireToSurface2Pdu) -> Result<(), String> {
        let blocks = decode_progressive_stream(&pdu.bitmap_data).map_err(|e| e.to_string())?;
        for block in blocks {
            if let ProgressiveBlock::Region(region) = block {
                self.progressive_region(pdu.surface_id, &region)?;
            }
        }
        Ok(())
    }

    fn progressive_region(&mut self, surface_id: u16, region: &ProgressiveRegion<'_>) -> Result<(), String> {
        let reduce = region.uses_reduce_extrapolate();
        let clip: Vec<_> = region
            .rects
            .iter()
            .map(|r| (r.x, r.y, r.x.saturating_add(r.width), r.y.saturating_add(r.height)))
            .collect();
        let Some(surface) = self.surfaces.get_mut(&surface_id) else {
            return Err(format!("unknown surface {surface_id}"));
        };
        if surface.progressive.as_ref().is_none_or(|t| t.use_reduce_extrapolate != reduce) {
            surface.progressive = Some(
                SurfaceTiles::new(surface.pixels.width, surface.pixels.height, reduce).map_err(|e| format!("{e:?}"))?,
            );
        }
        let base = |idx: u8| region.quant_vals.get(usize::from(idx)).copied();
        let prog = |quality: u8| -> Option<[ComponentCodecQuant; 3]> {
            if quality == 0xFF {
                return Some([ComponentCodecQuant::LOSSLESS; 3]);
            }
            let q = region.quant_prog_vals.get(usize::from(quality))?;
            Some([q.y_quant, q.cb_quant, q.cr_quant])
        };

        let mut decoded = Vec::new();
        {
            let tiles = surface.progressive.as_mut().expect("set above");
            for tile in &region.tiles {
                let (x_idx, y_idx) = (tile.x_idx(), tile.y_idx());
                let Some(state) = tiles.get_or_create(x_idx, y_idx) else { continue };
                match tile {
                    ProgressiveTile::Simple(t) => {
                        let (Some(y), Some(cb), Some(cr)) = (base(t.quant_idx_y), base(t.quant_idx_cb), base(t.quant_idx_cr))
                        else {
                            continue;
                        };
                        *state = TileState::new();
                        state
                            .decode_first(
                                [t.y_data, t.cb_data, t.cr_data],
                                [&y, &cb, &cr],
                                [ComponentCodecQuant::LOSSLESS; 3],
                                [t.quant_idx_y, t.quant_idx_cb, t.quant_idx_cr],
                                0xFF,
                                reduce,
                            )
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    ProgressiveTile::First(t) => {
                        let (Some(y), Some(cb), Some(cr), Some(pq)) =
                            (base(t.quant_idx_y), base(t.quant_idx_cb), base(t.quant_idx_cr), prog(t.quality))
                        else {
                            continue;
                        };
                        *state = TileState::new();
                        state
                            .decode_first(
                                [t.y_data, t.cb_data, t.cr_data],
                                [&y, &cb, &cr],
                                pq,
                                [t.quant_idx_y, t.quant_idx_cb, t.quant_idx_cr],
                                t.quality,
                                reduce,
                            )
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    ProgressiveTile::Upgrade(t) => {
                        let Some(pq) = prog(t.quality) else { continue };
                        state.decode_upgrade(
                            [t.y_srl_data, t.cb_srl_data, t.cr_srl_data],
                            [t.y_raw_data, t.cb_raw_data, t.cr_raw_data],
                            pq,
                            t.quality,
                        );
                    }
                }
                let mut rgba = vec![0u8; 64 * 64 * 4];
                state.reconstruct_to_rgba(&mut rgba);
                decoded.push((x_idx * 64, y_idx * 64, rgba));
            }
        }
        for (x, y, rgba) in decoded {
            self.write_tile(surface_id, x, y, &rgba, &clip);
        }
        Ok(())
    }
}

fn shared() -> &'static (Mutex<GfxState>, Notify) {
    static SHARED: OnceLock<(Mutex<GfxState>, Notify)> = OnceLock::new();
    SHARED.get_or_init(|| (Mutex::new(GfxState::default()), Notify::new()))
}

/// The graphics pipeline channel to register with drdynvc.
pub(crate) fn channel() -> GraphicsPipelineClient {
    GraphicsPipelineClient::new(Box::new(Handler), None)
}

/// Resolves when a GFX frame completed.
pub(crate) async fn frame_ready() {
    shared().1.notified().await;
}

/// The composed framebuffer if a frame completed since the last call: (0RGB pixels, width, height).
pub(crate) fn take_frame() -> Option<(Vec<u32>, u16, u16)> {
    let mut state = shared().0.lock().ok()?;
    if !state.dirty {
        return None;
    }
    state.dirty = false;
    let output = state.output.as_ref()?;
    Some((output.data.clone(), output.width, output.height))
}

struct Handler;

fn with_state(f: impl FnOnce(&mut GfxState)) {
    if let Ok(mut state) = shared().0.lock() {
        f(&mut state);
    }
}

impl GraphicsPipelineHandler for Handler {
    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        debug!(width, height, "RDPGFX reset graphics");
        with_state(|s| {
            let (w, h) = (u16::try_from(width).unwrap_or(u16::MAX), u16::try_from(height).unwrap_or(u16::MAX));
            s.output = Some(Pixels::new(w, h));
            s.surfaces.clear();
            s.cache.clear();
        });
    }

    fn on_surface_created(&mut self, surface: &Surface) {
        with_state(|s| {
            s.surfaces.insert(
                surface.id,
                GfxSurface {
                    pixels: Pixels::new(surface.width, surface.height),
                    origin: None,
                    progressive: None,
                },
            );
        });
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        with_state(|s| {
            s.surfaces.remove(&surface_id);
        });
    }

    fn on_surface_mapped(&mut self, surface_id: u16, origin_x: u32, origin_y: u32) {
        with_state(|s| {
            if let Some(surface) = s.surfaces.get_mut(&surface_id) {
                surface.origin = Some((origin_x, origin_y));
                let (w, h) = (surface.pixels.width, surface.pixels.height);
                s.present(surface_id, 0, 0, w, h);
            }
        });
    }

    fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
        let r = &update.destination_rectangle;
        with_state(|s| {
            let Some(surface) = s.surfaces.get_mut(&update.surface_id) else { return };
            let mut block = Pixels::new(update.width, update.height);
            for (i, px) in update.data.chunks_exact(4).enumerate().take(block.data.len()) {
                block.data[i] = u32::from_be_bytes([0, px[0], px[1], px[2]]);
            }
            surface.pixels.blit(r.left, r.top, &block, 0, 0, block.width, block.height);
            s.present(update.surface_id, r.left, r.top, r.right, r.bottom);
        });
    }

    fn on_solid_fill(&mut self, pdu: &SolidFillPdu) {
        let c = &pdu.fill_pixel;
        let color = u32::from_be_bytes([0, c.r, c.g, c.b]);
        with_state(|s| {
            let Some(surface) = s.surfaces.get_mut(&pdu.surface_id) else { return };
            for r in &pdu.rectangles {
                surface.pixels.fill(r.left, r.top, r.right, r.bottom, color);
            }
            for r in &pdu.rectangles {
                s.present(pdu.surface_id, r.left, r.top, r.right, r.bottom);
            }
        });
    }

    fn on_surface_to_surface(&mut self, pdu: &SurfaceToSurfacePdu) {
        let r = &pdu.source_rectangle;
        with_state(|s| {
            let Some(src) = s.surfaces.get(&pdu.source_surface_id) else { return };
            let block = src.pixels.crop(r.left, r.top, r.right, r.bottom);
            let Some(dst) = s.surfaces.get_mut(&pdu.destination_surface_id) else { return };
            for p in &pdu.destination_points {
                dst.pixels.blit(p.x, p.y, &block, 0, 0, block.width, block.height);
            }
            for p in &pdu.destination_points {
                s.present(
                    pdu.destination_surface_id,
                    p.x,
                    p.y,
                    p.x.saturating_add(block.width),
                    p.y.saturating_add(block.height),
                );
            }
        });
    }

    fn on_surface_to_cache(&mut self, pdu: &SurfaceToCachePdu) {
        let r = &pdu.source_rectangle;
        with_state(|s| {
            if let Some(src) = s.surfaces.get(&pdu.surface_id) {
                let block = src.pixels.crop(r.left, r.top, r.right, r.bottom);
                s.cache.insert(pdu.cache_slot, block);
            }
        });
    }

    fn on_cache_to_surface(&mut self, pdu: &CacheToSurfacePdu) {
        with_state(|s| {
            let Some(block) = s.cache.remove(&pdu.cache_slot) else { return };
            if let Some(dst) = s.surfaces.get_mut(&pdu.surface_id) {
                for p in &pdu.destination_points {
                    dst.pixels.blit(p.x, p.y, &block, 0, 0, block.width, block.height);
                }
            }
            for p in &pdu.destination_points {
                s.present(
                    pdu.surface_id,
                    p.x,
                    p.y,
                    p.x.saturating_add(block.width),
                    p.y.saturating_add(block.height),
                );
            }
            s.cache.insert(pdu.cache_slot, block);
        });
    }

    fn on_evict_cache_entry(&mut self, pdu: &EvictCacheEntryPdu) {
        with_state(|s| {
            s.cache.remove(&pdu.cache_slot);
        });
    }

    fn on_wire_to_surface2(&mut self, pdu: &WireToSurface2Pdu) {
        if pdu.codec_id != Codec2Type::RemoteFxProgressive {
            return;
        }
        with_state(|s| {
            if let Err(e) = s.progressive(pdu) {
                warn!(error = %e, surface = pdu.surface_id, "RemoteFX Progressive decode failed");
            }
        });
    }

    fn on_unhandled_pdu(&mut self, pdu: &GfxPdu) {
        if let GfxPdu::WireToSurface1(pdu) = pdu {
            with_state(|s| {
                if let Err(e) = s.wire_to_surface1(pdu) {
                    warn!(error = %e, codec = ?pdu.codec_id, "RDPGFX decode failed");
                }
            });
        }
    }

    fn on_frame_complete(&mut self, _frame_id: u32) {
        with_state(|s| s.dirty = true);
        shared().1.notify_one();
    }
}

