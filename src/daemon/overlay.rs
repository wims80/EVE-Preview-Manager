//! Overlay management for thumbnails (text and borders)

use anyhow::{Context, Result};
use tracing::error;
use x11rb::connection::Connection;
use x11rb::protocol::render::{ConnectionExt as RenderExt, CreatePictureAux, PictOp, Picture};
use x11rb::protocol::xproto::{
    Char2b, ConnectionExt as XprotoExt, CreateGCAux, Gcontext, ImageFormat, Pixmap,
};
use x11rb::rust_connection::RustConnection;

use crate::common::constants::x11;
use crate::common::types::Dimensions;
use crate::config::DisplayConfig;

use super::font::FontRenderer;

#[derive(Debug)]
/// Handles text and border overlay rendering for thumbnails.
///
/// This struct manages:
/// - The overlay pixmap where text and borders are drawn.
/// - The graphics context (GC) for X11 text rendering.
/// - Integration with `FontRenderer` for text glyph generation.
pub struct OverlayRenderer<'a> {
    // === X11 Resources (private, owned) ===
    /// Backing pixmap for the overlay layer.
    pub overlay_pixmap: Pixmap,
    /// X Render Picture wrapping the overlay pixmap.
    pub overlay_picture: Picture,
    overlay_gc: Gcontext,           // Graphics context for text rendering
    active_border_fill: Picture,    // Solid color fill for active border
    inactive_border_fill: Picture,  // Solid color fill for inactive border
    skipped_indicator_gc: Gcontext, // GC for drawing skipped indicator (Red X)

    // === Borrowed Dependencies ===
    conn: &'a RustConnection,
    formats: &'a crate::x11::CachedFormats,
}

impl<'a> OverlayRenderer<'a> {
    /// Creates a new `OverlayRenderer`.
    ///
    /// # Arguments
    /// * `conn` - X11 connection.
    /// * `config` - Display configuration (colors, sizes).
    /// * `formats` - X11 Render formats.
    /// * `font_renderer` - System for rendering text glyphs (used for initial render).
    /// * `root` - Root window ID (for pixmap creation).
    /// * `dimensions` - Initial size of the overlay.
    /// * `character_name` - Debug name for error logging.
    pub fn new<'b>(
        conn: &'a RustConnection,
        config: &'b DisplayConfig,
        formats: &'a crate::x11::CachedFormats,
        font_renderer: &FontRenderer,
        root: u32,
        dimensions: Dimensions,
        character_name: &str,
    ) -> Result<Self> {
        // ... (implementation of new)
        // Create overlay pixmap
        let overlay_pixmap = conn
            .generate_id()
            .context("Failed to generate ID for overlay pixmap")?;
        conn.create_pixmap(
            x11::ARGB_DEPTH,
            overlay_pixmap,
            root,
            dimensions.width,
            dimensions.height,
        )
        .context(format!(
            "Failed to create overlay pixmap for '{}'",
            character_name
        ))?;

        // Create overlay picture
        let overlay_picture = conn
            .generate_id()
            .context("Failed to generate ID for overlay picture")?;
        conn.render_create_picture(
            overlay_picture,
            overlay_pixmap,
            formats.argb,
            &CreatePictureAux::new(),
        )
        .context(format!(
            "Failed to create overlay picture for '{}'",
            character_name
        ))?;

        // Create overlay GC
        let overlay_gc = conn
            .generate_id()
            .context("Failed to generate ID for overlay graphics context")?;
        conn.create_gc(
            overlay_gc,
            overlay_pixmap,
            &CreateGCAux::new().foreground(config.text_color),
        )
        .context(format!(
            "Failed to create graphics context for '{}'",
            character_name
        ))?;

        // Create skipped indicator GC (Red)
        let skipped_indicator_gc = conn
            .generate_id()
            .context("Failed to generate ID for skipped indicator GC")?;
        conn.create_gc(
            skipped_indicator_gc,
            overlay_pixmap,
            &CreateGCAux::new()
                .foreground(0xFFFF0000) // Opaque Red
                .line_width(3), // Thicker lines for visibility
        )
        .context(format!(
            "Failed to create skipped indicator GC for '{}'",
            character_name
        ))?;

        // Create active border fill
        let active_border_fill = conn
            .generate_id()
            .context("Failed to generate ID for active border fill picture")?;
        conn.render_create_solid_fill(active_border_fill, config.active_border_color)
            .context(format!(
                "Failed to create active border fill for '{}'",
                character_name
            ))?;

        // Create inactive border fill
        let inactive_border_fill = conn
            .generate_id()
            .context("Failed to generate ID for inactive border fill picture")?;
        conn.render_create_solid_fill(inactive_border_fill, config.inactive_border_color)
            .context(format!(
                "Failed to create inactive border fill for '{}'",
                character_name
            ))?;

        let renderer = Self {
            overlay_pixmap,
            overlay_picture,
            overlay_gc,
            active_border_fill,
            inactive_border_fill,
            skipped_indicator_gc,
            conn,
            formats,
        };

        // Render initial name
        let initial_border_size = renderer.calculate_border_size(config, character_name, false);
        renderer
            .clear_content_area(dimensions, initial_border_size)
            .context(format!(
                "Failed to clear content area for initial render of '{}'",
                character_name
            ))?;

        renderer
            .update_name(
                config,
                character_name,
                dimensions,
                initial_border_size,
                font_renderer,
            )
            .context(format!(
                "Failed to render initial name for '{}'",
                character_name
            ))?;

        Ok(renderer)
    }

    /// Resizes the overlay resources.
    ///
    /// This destroys the old pixmap/picture and creates new ones with the given dimensions.
    pub fn resize(&mut self, root: u32, width: u16, height: u16) -> Result<()> {
        // Free old resources
        self.cleanup_overlay_resources();

        // Recreate resources with new dimensions
        let overlay_pixmap = self.conn.generate_id()?;
        self.conn
            .create_pixmap(x11::ARGB_DEPTH, overlay_pixmap, root, width, height)?;
        self.overlay_pixmap = overlay_pixmap;

        let overlay_picture = self.conn.generate_id()?;
        self.conn.render_create_picture(
            overlay_picture,
            overlay_pixmap,
            self.formats.argb,
            &CreatePictureAux::new(),
        )?;
        self.overlay_picture = overlay_picture;

        Ok(())
    }

    // ... (calculate_border_size unused here, implementation below)

    /// Draws the skipped indicator (diagonal red lines)
    pub fn draw_skipped_indicator(&self, dimensions: Dimensions) -> Result<()> {
        let w = dimensions.width as i16;
        let h = dimensions.height as i16;

        let segments = [
            x11rb::protocol::xproto::Segment {
                x1: 0,
                y1: 0,
                x2: w,
                y2: h,
            },
            x11rb::protocol::xproto::Segment {
                x1: w,
                y1: 0,
                x2: 0,
                y2: h,
            },
        ];

        self.conn
            .poly_segment(self.overlay_pixmap, self.skipped_indicator_gc, &segments)
            .context("Failed to draw skipped indicator segments")?;

        Ok(())
    }

    /// Calculates the effective border size implementation
    pub fn calculate_border_size(
        &self,
        config: &DisplayConfig,
        character_name: &str,
        focused: bool,
    ) -> u16 {
        if let Some(settings) = config.character_settings.get(character_name) {
            if focused {
                settings
                    .override_active_border_size
                    .unwrap_or(config.active_border_size)
            } else {
                settings
                    .override_inactive_border_size
                    .unwrap_or(config.inactive_border_size)
            }
        } else if focused {
            config.active_border_size
        } else {
            config.inactive_border_size
        }
    }

    /// Clears the center content area (inside the border).
    pub fn clear_content_area(&self, dimensions: Dimensions, border_size: u16) -> Result<()> {
        self.conn
            .render_composite(
                PictOp::CLEAR,
                self.overlay_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                border_size as i16,
                border_size as i16,
                dimensions.width.saturating_sub(border_size * 2),
                dimensions.height.saturating_sub(border_size * 2),
            )
            .context("Failed to clear content area")?;
        Ok(())
    }

    /// Renders the character name onto the overlay.
    ///
    /// Handles both direct X11 text rendering (if core fonts are used) and
    /// client-side rendering (if TrueType fonts are used via `fontdue`).
    /// NOTE: This does NOT clear the background. You must call `clear_content_area` first.
    pub fn update_name(
        &self,
        config: &DisplayConfig,
        character_name: &str,
        _dimensions: Dimensions,
        _border_size: u16,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        // Resolve settings overrides
        let (display_name, text_color) =
            if let Some(settings) = config.character_settings.get(character_name) {
                let name = settings.alias.as_deref().unwrap_or(character_name);
                let color = if let Some(hex_color) = &settings.override_text_color {
                    crate::common::color::HexColor::parse(hex_color)
                        .map(|c| c.argb32())
                        .unwrap_or(config.text_color)
                } else {
                    config.text_color
                };
                (name, color)
            } else {
                (character_name, config.text_color)
            };

        // Render text based on font renderer type
        if font_renderer.requires_direct_rendering() {
            // X11 fallback: direct rendering using ImageText8
            if let Some(font_id) = font_renderer.x11_font_id() {
                // Create GC with font
                let gc = self
                    .conn
                    .generate_id()
                    .context("Failed to generate GC ID for X11 text")?;

                // Convert ARGB color to X11 pixel value (strip alpha)
                let fg_pixel = text_color & 0x00FFFFFF;

                self.conn
                    .create_gc(
                        gc,
                        self.overlay_pixmap,
                        &CreateGCAux::new().font(font_id).foreground(fg_pixel),
                    )
                    .context(format!(
                        "Failed to create GC for X11 text rendering for '{}'",
                        character_name
                    ))?;

                // ImageText8 renders directly to drawable
                self.conn
                    .image_text8(
                        self.overlay_pixmap,
                        gc,
                        config.text_offset.x,
                        config.text_offset.y + font_renderer.size() as i16, // Baseline adjustment
                        display_name.as_bytes(),
                    )
                    .context(format!(
                        "Failed to render text via X11 for '{}'",
                        character_name
                    ))?;

                self.conn.free_gc(gc)?;
            }
        } else {
            // Fontdue: pre-rendered bitmap
            let rendered = font_renderer
                .render_text(display_name, text_color)
                .context(format!(
                    "Failed to render text '{}' with font renderer",
                    character_name
                ))?;

            if rendered.width > 0 && rendered.height > 0 {
                // Upload rendered text bitmap to X11
                // rendered.data is already in BGRA format (Little Endian ARGB)
                let text_pixmap = self
                    .conn
                    .generate_id()
                    .context("Failed to generate ID for text pixmap")?;
                self.conn
                    .create_pixmap(
                        x11::ARGB_DEPTH,
                        text_pixmap,
                        self.overlay_pixmap,
                        rendered.width as u16,
                        rendered.height as u16,
                    )
                    .context(format!(
                        "Failed to create text pixmap for '{}'",
                        character_name
                    ))?;

                self.conn
                    .put_image(
                        ImageFormat::Z_PIXMAP,
                        text_pixmap,
                        self.overlay_gc,
                        rendered.width as u16,
                        rendered.height as u16,
                        0,
                        0,
                        0,
                        x11::ARGB_DEPTH,
                        &rendered.data,
                    )
                    .context(format!(
                        "Failed to upload text image for '{}'",
                        character_name
                    ))?;

                // Create picture for the text pixmap
                let text_picture = self
                    .conn
                    .generate_id()
                    .context("Failed to generate ID for text picture")?;
                self.conn
                    .render_create_picture(
                        text_picture,
                        text_pixmap,
                        self.formats.argb,
                        &CreatePictureAux::new(),
                    )
                    .context(format!(
                        "Failed to create text picture for '{}'",
                        character_name
                    ))?;

                // Composite text onto overlay
                self.conn
                    .render_composite(
                        PictOp::OVER,
                        text_picture,
                        0u32,
                        self.overlay_picture,
                        0,
                        0,
                        0,
                        0,
                        config.text_offset.x,
                        config.text_offset.y,
                        rendered.width as u16,
                        rendered.height as u16,
                    )
                    .context(format!(
                        "Failed to composite text onto overlay for '{}'",
                        character_name
                    ))?;

                // Cleanup
                self.conn
                    .render_free_picture(text_picture)
                    .context("Failed to free text picture")?;
                self.conn
                    .free_pixmap(text_pixmap)
                    .context("Failed to free text pixmap")?;
            }
        }

        Ok(())
    }

    /// Draws the overlay content with strict Z-order:
    /// 1. Skipped Indicator (Red X) - Bottom
    /// 2. Text (Name) - Middle
    /// 3. Border - Top (covers everything at edges)
    #[allow(clippy::too_many_arguments)]
    pub fn draw_border(
        &self,
        config: &DisplayConfig,
        character_name: &str,
        dimensions: Dimensions,
        focused: bool,
        skipped: bool,
        font_renderer: &FontRenderer,
        alert: Option<super::alert::AlertVisual>,
    ) -> Result<()> {
        // 1. Clear the entire overlay first (transparent background)
        self.conn
            .render_composite(
                PictOp::CLEAR,
                self.overlay_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                dimensions.width,
                dimensions.height,
            )
            .context("Failed to clear overlay")?;

        // 2. Draw skipped indicator (Red X)
        // Drawn first so text appears on top of it
        if skipped {
            self.draw_skipped_indicator(dimensions)?;
        }

        // Determine effective border size and color source
        let effective_size = alert
            .map(|a| a.border_size)
            .unwrap_or_else(|| self.calculate_border_size(config, character_name, focused));

        // 3. Draw Text
        // We pass effective_size mainly if text positioning depended on it,
        // but currently text is positioned by config offset.
        self.update_name(
            config,
            character_name,
            dimensions,
            effective_size,
            font_renderer,
        )
        .context(format!(
            "Failed to update name overlay for '{}'",
            character_name
        ))?;

        // 4. Draw Border (Top Layer)
        // Only if size > 0 and enabled
        let should_draw_border = if alert.is_some() || focused {
            effective_size > 0
        } else {
            config.inactive_border_enabled && effective_size > 0
        };

        // The banner sits below the alert border so damage retains visual priority
        // at the lower edge when both alerts overlap.
        if alert.is_some_and(|a| a.decloak_banner) {
            self.draw_decloak_banner(dimensions, font_renderer)?;
        }

        if should_draw_border {
            let (fill_picture, temp_fill_id) = if let Some(alert) = alert {
                let pid = self.conn.generate_id()?;
                self.conn.render_create_solid_fill(pid, alert.color)?;
                (pid, Some(pid))
            } else {
                if let Some(settings) = config.character_settings.get(character_name) {
                    let override_color_hex = if focused {
                        settings.override_active_border_color.as_ref()
                    } else {
                        settings.override_inactive_border_color.as_ref()
                    };

                    if let Some(hex) = override_color_hex {
                        if let Some(color) =
                            crate::common::color::HexColor::parse(hex).map(|c| c.to_x11_color())
                        {
                            let pid = self.conn.generate_id()?;
                            self.conn.render_create_solid_fill(pid, color)?;
                            (pid, Some(pid))
                        } else if focused {
                            (self.active_border_fill, None)
                        } else {
                            (self.inactive_border_fill, None)
                        }
                    } else if focused {
                        (self.active_border_fill, None)
                    } else {
                        (self.inactive_border_fill, None)
                    }
                } else if focused {
                    (self.active_border_fill, None)
                } else {
                    (self.inactive_border_fill, None)
                }
            };

            // Draw 4 strips for the border
            let w = dimensions.width as i16;
            let h = dimensions.height as i16;
            let b = effective_size as i16;

            // Top
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                dimensions.width,
                effective_size,
            )?;
            // Bottom
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                h - b,
                dimensions.width,
                effective_size,
            )?;
            // Left
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                b,
                effective_size,
                (h - 2 * b).max(0) as u16,
            )?;
            // Right
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                w - b,
                b,
                effective_size,
                (h - 2 * b).max(0) as u16,
            )?;

            // Clean up temp fill
            if let Some(pid) = temp_fill_id {
                self.conn.render_free_picture(pid)?;
            }
        }

        Ok(())
    }

    /// Draws the "MINIMIZED" state overlay.
    pub fn draw_minimized(
        &self,
        config: &DisplayConfig,
        character_name: &str,
        dimensions: Dimensions,
        font_renderer: &FontRenderer,
        alert: Option<super::alert::AlertVisual>,
    ) -> Result<()> {
        self.draw_border(
            config,
            character_name,
            dimensions,
            false,
            false,
            font_renderer,
            alert,
        )
        .context(format!(
            "Failed to clear border for minimized window '{}'",
            character_name
        ))?;

        if !config.minimized_overlay_enabled {
            return Ok(());
        }

        let extents = self
            .conn
            .query_text_extents(
                self.overlay_gc,
                b"MINIMIZED"
                    .iter()
                    .map(|&c| Char2b { byte1: 0, byte2: c })
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
            .context("Failed to send text extents query for MINIMIZED text")?
            .reply()
            .context("Failed to get text extents for MINIMIZED text")?;
        self.conn
            .image_text8(
                self.overlay_pixmap,
                self.overlay_gc,
                (dimensions.width as i16 - extents.overall_width as i16) / 2,
                (dimensions.height as i16 + extents.font_ascent + extents.font_descent) / 2,
                b"MINIMIZED",
            )
            .context(format!(
                "Failed to render MINIMIZED text for '{}'",
                character_name
            ))?;
        Ok(())
    }

    fn draw_decloak_banner(
        &self,
        dimensions: Dimensions,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        const LABEL: &str = "▞▞▞ DECLOAKED ▞▞▞";
        let height = (font_renderer.size() as u16 + 8).min(dimensions.height);
        let y = dimensions.height.saturating_sub(height) as i16;
        let yellow = self.conn.generate_id()?;
        self.conn.render_create_solid_fill(
            yellow,
            x11rb::protocol::render::Color {
                red: 0xffff,
                green: 0xcccc,
                blue: 0,
                alpha: 0xffff,
            },
        )?;
        self.conn.render_composite(
            PictOp::SRC,
            yellow,
            0u32,
            self.overlay_picture,
            0,
            0,
            0,
            0,
            0,
            y,
            dimensions.width,
            height,
        )?;
        self.conn.render_free_picture(yellow)?;

        if font_renderer.requires_direct_rendering() {
            // Core X11 fonts cannot represent the block glyphs; retain the recognizable text.
            let full_text = b"<<< DECLOAKED >>>";
            let full_width = full_text.len() as i16 * (font_renderer.size() as i16 / 2).max(5);
            let text: &[u8] = if full_width + 8 > dimensions.width as i16 {
                b"DECLOAKED"
            } else {
                full_text
            };
            let gc = self.conn.generate_id()?;
            self.conn.create_gc(
                gc,
                self.overlay_pixmap,
                &CreateGCAux::new()
                    .foreground(0xff000000)
                    .background(0xffffcc00)
                    .font(font_renderer.x11_font_id().unwrap_or(0)),
            )?;
            let approx_width = text.len() as i16 * (font_renderer.size() as i16 / 2).max(5);
            let x = ((dimensions.width as i16 - approx_width) / 2).max(0);
            self.conn
                .image_text8(self.overlay_pixmap, gc, x, y + height as i16 - 4, text)?;
            self.conn.free_gc(gc)?;
        } else {
            let rendered = font_renderer.render_text(LABEL, 0xff000000)?;
            let available_width = dimensions.width.saturating_sub(8) as usize;
            let scale = (available_width as f32 / rendered.width.max(1) as f32).min(1.0);
            let fitted_width = (rendered.width as f32 * scale).round().max(1.0) as u16;
            let fitted_height = (rendered.height as f32 * scale).round().max(1.0) as u16;
            if rendered.width > 0 && fitted_width > 0 && fitted_height > 0 && rendered.height > 0 {
                let pixmap = self.conn.generate_id()?;
                self.conn.create_pixmap(
                    x11::ARGB_DEPTH,
                    pixmap,
                    self.overlay_pixmap,
                    rendered.width as u16,
                    rendered.height as u16,
                )?;
                self.conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    pixmap,
                    self.overlay_gc,
                    rendered.width as u16,
                    rendered.height as u16,
                    0,
                    0,
                    0,
                    x11::ARGB_DEPTH,
                    &rendered.data,
                )?;
                let picture = self.conn.generate_id()?;
                self.conn.render_create_picture(
                    picture,
                    pixmap,
                    self.formats.argb,
                    &CreatePictureAux::new(),
                )?;
                self.conn
                    .render_set_picture_filter(picture, b"bilinear", &[])?;
                self.conn.render_set_picture_transform(
                    picture,
                    x11rb::protocol::render::Transform {
                        matrix11: crate::x11::to_fixed(rendered.width as f32 / fitted_width as f32),
                        matrix22: crate::x11::to_fixed(
                            rendered.height as f32 / fitted_height as f32,
                        ),
                        matrix33: crate::x11::to_fixed(1.0),
                        ..Default::default()
                    },
                )?;
                let dest_x = ((dimensions.width - fitted_width) / 2) as i16;
                let dest_y = y + ((height as i16 - fitted_height as i16) / 2).max(0);
                self.conn.render_composite(
                    PictOp::OVER,
                    picture,
                    0u32,
                    self.overlay_picture,
                    0,
                    0,
                    0,
                    0,
                    dest_x,
                    dest_y,
                    fitted_width,
                    fitted_height,
                )?;
                self.conn.render_free_picture(picture)?;
                self.conn.free_pixmap(pixmap)?;
            }
        }
        Ok(())
    }

    fn cleanup_overlay_resources(&self) {
        if let Err(e) = self.conn.free_pixmap(self.overlay_pixmap) {
            error!(pixmap = self.overlay_pixmap, error = %e, "Failed to free overlay pixmap");
        }

        if let Err(e) = self.conn.render_free_picture(self.overlay_picture) {
            error!(picture = self.overlay_picture, error = %e, "Failed to free overlay picture");
        }
    }
}

impl Drop for OverlayRenderer<'_> {
    fn drop(&mut self) {
        self.cleanup_overlay_resources();

        if let Err(e) = self.conn.free_gc(self.overlay_gc) {
            error!(gc = self.overlay_gc, error = %e, "Failed to free GC");
        }

        if let Err(e) = self.conn.free_gc(self.skipped_indicator_gc) {
            error!(gc = self.skipped_indicator_gc, error = %e, "Failed to free skipped indicator GC");
        }

        if let Err(e) = self.conn.render_free_picture(self.active_border_fill) {
            error!(picture = self.active_border_fill, error = %e, "Failed to free active border fill picture");
        }

        if let Err(e) = self.conn.render_free_picture(self.inactive_border_fill) {
            error!(
                picture = self.inactive_border_fill,
                error = %e,
                "Failed to free inactive border fill picture"
            );
        }
    }
}
