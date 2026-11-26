//! "Frosted Glass" look for the connection manager (issues #7, #8): a light, translucent window
//! with the desktop blurred behind it (macOS vibrancy, Windows Acrylic), rounded controls and a
//! blue accent. Where the OS can't blur (Linux, or if it fails) a soft opaque gradient is painted
//! instead, with the same layout.

use eframe::egui::{
    self, Color32, CornerRadius, Margin, Pos2, Shape, Stroke, TextStyle, Vec2, epaint::Mesh,
};

pub const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
pub const TEXT: Color32 = Color32::from_rgb(15, 23, 42);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(71, 85, 105);
pub const DANGER: Color32 = Color32::from_rgb(185, 28, 28);

/// Whether this platform can blur the desktop behind a transparent window.
pub const fn blur_supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Applies the native blur behind the window. Returns true when it worked.
pub fn apply_window_blur(cc: &eframe::CreationContext<'_>) -> bool {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState};
        match window_vibrancy::apply_vibrancy(
            cc,
            NSVisualEffectMaterial::Sidebar,
            Some(NSVisualEffectState::Active),
            None,
        ) {
            Ok(()) => return true,
            Err(e) => tracing::warn!(error = %e, "macOS vibrancy unavailable"),
        }
    }
    #[cfg(windows)]
    {
        match window_vibrancy::apply_acrylic(cc, Some((245, 247, 255, 120))) {
            Ok(()) => return true,
            Err(e) => tracing::warn!(error = %e, "Windows acrylic unavailable"),
        }
    }
    let _ = cc;
    false
}

/// Light, rounded style; `glass` makes the surfaces translucent so the blur shows through.
pub fn apply_style(ctx: &egui::Context, glass: bool) {
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::light();
    let v = &mut style.visuals;

    let surface = if glass {
        Color32::from_rgba_unmultiplied(255, 255, 255, 120)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 170)
    };
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 235);
    v.window_corner_radius = CornerRadius::same(16);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200));
    v.extreme_bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 200); // text fields
    v.faint_bg_color = surface;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.error_fg_color = DANGER;

    let radius = CornerRadius::same(10);
    let field_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(15, 23, 42, 30));
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 180);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 180);
    v.widgets.inactive.bg_stroke = field_stroke;
    v.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 230);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 230);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    v.widgets.active.bg_fill = Color32::WHITE;
    v.widgets.active.weak_bg_fill = Color32::WHITE;
    v.widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);

    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    style.text_styles.insert(TextStyle::Heading, egui::FontId::proportional(22.0));
    style.text_styles.insert(TextStyle::Body, egui::FontId::proportional(14.0));
    style.text_styles.insert(TextStyle::Button, egui::FontId::proportional(14.0));
    style.text_styles.insert(TextStyle::Small, egui::FontId::proportional(12.0));
    ctx.set_global_style(style);
}

/// Soft pastel gradient behind everything, for when there is no OS blur.
pub fn paint_backdrop(ctx: &egui::Context) {
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let mut mesh = Mesh::default();
    let corners = [
        (rect.left_top(), Color32::from_rgb(199, 210, 254)),
        (rect.right_top(), Color32::from_rgb(204, 251, 241)),
        (rect.right_bottom(), Color32::from_rgb(251, 207, 232)),
        (rect.left_bottom(), Color32::from_rgb(224, 231, 255)),
    ];
    for (pos, color) in corners {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// Translucent surface for the sidebar.
pub fn sidebar_frame(glass: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, if glass { 70 } else { 110 }))
        .inner_margin(Margin {
            left: 12,
            right: 12,
            // Room for the macOS traffic lights over the content.
            top: if cfg!(target_os = "macos") { 34 } else { 14 },
            bottom: 12,
        })
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 150)))
}

/// Main content surface.
pub fn content_frame(glass: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, if glass { 110 } else { 150 }))
        .inner_margin(Margin::symmetric(28, 24))
}

/// One host row in the sidebar: name and "host · user", filled with the accent when selected.
pub fn host_row(ui: &mut egui::Ui, title: &str, subtitle: &str, selected: bool) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 50.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, title_color, sub_color) = if selected {
            (ACCENT, Color32::WHITE, Color32::from_rgba_unmultiplied(255, 255, 255, 215))
        } else if response.hovered() {
            (Color32::from_rgba_unmultiplied(255, 255, 255, 150), TEXT, TEXT_MUTED)
        } else {
            (Color32::TRANSPARENT, TEXT, TEXT_MUTED)
        };
        let painter = ui.painter();
        if selected {
            painter.rect_filled(
                rect.translate(Vec2::new(0.0, 3.0)).shrink(2.0),
                CornerRadius::same(10),
                ACCENT.gamma_multiply(0.25),
            );
        }
        painter.rect_filled(rect, CornerRadius::same(10), fill);
        let left = rect.left() + 12.0;
        painter.text(
            Pos2::new(left, rect.top() + 9.0),
            egui::Align2::LEFT_TOP,
            title,
            egui::FontId::proportional(14.0),
            title_color,
        );
        painter.text(
            Pos2::new(left, rect.top() + 28.0),
            egui::Align2::LEFT_TOP,
            subtitle,
            egui::FontId::proportional(12.0),
            sub_color,
        );
    }
    response
}

/// Filled accent button (Connect).
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(12))
            .min_size(Vec2::new(112.0, 38.0)),
    )
}

/// Plain translucent button (Save).
pub fn secondary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(text)
            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 190))
            .corner_radius(CornerRadius::same(12))
            .min_size(Vec2::new(90.0, 38.0)),
    )
}

/// Text-only red button (Delete).
pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(DANGER))
            .frame(false)
            .min_size(Vec2::new(70.0, 38.0)),
    )
}

/// A small caption above a field.
pub fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(12.0).color(TEXT_MUTED).strong());
}

/// Tinted box for an error message.
pub fn error_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(239, 68, 68, 26))
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(220, 38, 38, 64)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 8))
}
