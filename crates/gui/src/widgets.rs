//! Small widgets shared by the panels and dialogs.

use eframe::egui::{self, Color32, RichText};

/// A dialog over the window: the rest of the app is dimmed and does not react
/// until it is closed. It has a title bar with a close button, and scrolls when
/// taller than the window. Returns the content's value, and whether the user
/// closed it (the button or Esc; a click outside does nothing, so work in it
/// is not lost by accident).
pub fn dialog<R>(ctx: &egui::Context, id: &str, title: &str, width: f32, content: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let max_height = ctx.content_rect().height() - 140.0;
    let mut closed = false;
    let response = egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_width(width.min(ctx.content_rect().width() - 60.0));
        ui.horizontal(|ui| {
            ui.heading(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                closed = ui.add(egui::Button::new("✖").frame(false)).on_hover_text("Close (Esc)").clicked();
            });
        });
        ui.separator();
        // Grows with its content up to max_height, also when the content grows.
        egui::ScrollArea::vertical().id_salt(id).max_height(max_height).min_scrolled_height(max_height).show(ui, content).inner
    });
    let outside = response.backdrop_response.clicked();
    let close = closed || (!outside && response.should_close());
    (response.inner, close)
}

/// The margin around the bars and panels of the main window.
pub const MARGIN: i8 = 6;

/// The height of every button of the window's bars and toolbars, in whole
/// pixels so rows side by side line up at any display scale.
pub fn bar_height(ui: &egui::Ui) -> f32 {
    // From the window's style: menus have a style of their own.
    let style = ui.ctx().global_style();
    let spacing = &style.spacing;
    let text = egui::TextStyle::Button.resolve(&style);
    let height = (ui.fonts_mut(|f| f.row_height(&text)) + 2.0 * spacing.button_padding.y + 4.0).max(spacing.interact_size.y);
    let ppp = ui.ctx().pixels_per_point();
    (height * ppp).round() / ppp
}

/// A button of a bar, as tall as the others; at least `width` wide.
pub fn bar_button(ui: &mut egui::Ui, button: egui::Button<'_>, width: f32) -> egui::Response {
    let height = bar_height(ui);
    ui.add(button.min_size(egui::vec2(width, height)))
}

/// The main action of a dialog, in the accent colour.
pub fn primary(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let selection = ui.visuals().selection;
    egui::Button::new(RichText::new(text).strong().color(selection.stroke.color)).fill(selection.bg_fill)
}

/// An action that throws something away, in red.
pub fn danger(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let fill = if ui.visuals().dark_mode { Color32::from_rgb(0xB0, 0x30, 0x30) } else { Color32::from_rgb(0xC6, 0x28, 0x28) };
    egui::Button::new(RichText::new(text).strong().color(Color32::WHITE)).fill(fill)
}

/// The title of a part of a window, set apart from the part above.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    let size = egui::TextStyle::Body.resolve(ui.style()).size * 1.15;
    ui.label(RichText::new(title).strong().size(size));
    ui.add_space(2.0);
}

/// A line of secondary explanation.
pub fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).weak());
}

/// A selectable list row as wide as the space: the name on the left,
/// shortened if need be, and weaker details on the right when they fit.
pub fn list_row(ui: &mut egui::Ui, selected: bool, name: &str, details: &str) -> egui::Response {
    let padding = ui.spacing().button_padding;
    let text_style = egui::TextStyle::Button;
    let height = ui.text_style_height(&text_style) + 2.0 * padding.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    // Screen readers (and tests) get the details too.
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, format!("{name}  {details}")));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    let inner = rect.shrink2(padding);
    let gap = 3.0 * ui.spacing().item_spacing.x;
    let weak = ui.visuals().weak_text_color();
    let details = egui::WidgetText::from(egui::RichText::new(details).color(weak)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, text_style.clone());
    let name_width = |with_details: bool| if with_details { inner.width() - details.size().x - gap } else { inner.width() };
    // Details only beside a name that still reads well.
    let show_details = name_width(true) >= 60.0f32.min(inner.width() / 2.0);
    let name = egui::WidgetText::from(name).into_galley(ui, Some(egui::TextWrapMode::Truncate), name_width(show_details).max(0.0), text_style);
    let center = inner.center().y;
    ui.painter().galley(egui::pos2(inner.left(), center - name.size().y / 2.0), name, visuals.text_color());
    if show_details {
        ui.painter().galley(egui::pos2(inner.right() - details.size().x, center - details.size().y / 2.0), details, weak);
    }
    response
}

/// In a wrapping row, starts a new line unless a combo box showing `text`
/// fits in what is left (egui does not wrap combo boxes by itself).
pub fn fit_combo(ui: &mut egui::Ui, text: &str) {
    let spacing = ui.spacing();
    let needed = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x
        + 2.0 * spacing.button_padding.x
        + spacing.icon_width
        + 2.0 * spacing.item_spacing.x;
    // In a wrapping row the available width is the whole row's: what is left
    // runs from the cursor to the right edge.
    let left = ui.max_rect().right() - ui.cursor().left();
    if left < needed {
        ui.end_row();
    }
}

/// The height of a list inside a dialog: as tall as it was designed, less on a
/// short window so the dialog's buttons stay on screen.
pub fn dialog_list_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 300.0).clamp(80.0, 260.0)
}
