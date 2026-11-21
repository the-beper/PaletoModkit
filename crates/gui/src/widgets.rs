//! Small widgets shared by the panels and dialogs.

use eframe::egui::{self, Color32, RichText};

/// KDE's small, medium and large spacing.
pub const SPACE_S: f32 = 4.0;
pub const SPACE_M: f32 = 6.0;
pub const SPACE_L: f32 = 8.0;

/// The widths of a dialog: a short question or a form, text with a list, and a table or a log. At the Body size the widest gives a line of about 85 characters.
pub const DIALOG_NARROW: f32 = 480.0;
pub const DIALOG_MEDIUM: f32 = 560.0;
pub const DIALOG_WIDE: f32 = 640.0;

/// A dialog over the window: the rest of the app is dimmed and does not react
/// until it is closed. It has a title bar with a close button, and scrolls when
/// taller than the window. Returns the content's value, and whether the user
/// closed it (the button or Esc; a click outside does nothing, so work in it
/// is not lost by accident).
pub fn dialog<R>(ctx: &egui::Context, id: &str, title: &str, width: f32, content: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    dialog_sized(ctx, id, title, width, true, content)
}

/// [`dialog`], and with `fill` false it is only as tall as its content, up to the window.
pub fn dialog_sized<R>(ctx: &egui::Context, id: &str, title: &str, width: f32, fill: bool, content: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let max_height = ctx.content_rect().height() - 140.0;
    let mut closed = false;
    let response = modal(ctx, id).show(ctx, |ui| {
        ui.set_width(dialog_width(ctx, width));
        ui.horizontal(|ui| {
            ui.heading(title_text(title));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let side = ui.spacing().interact_size.y;
                closed = ui.add(egui::Button::new("✖").frame(false).min_size(egui::Vec2::splat(side))).on_hover_text("Close (Esc)").clicked();
            });
        });
        ui.separator();
        // Grows with its content up to max_height, also when the content grows.
        let area = egui::ScrollArea::vertical().id_salt(id).max_height(max_height);
        if fill { area.min_scrolled_height(max_height).show(ui, content).inner } else { area.show(ui, content).inner }
    });
    let outside = response.backdrop_response.clicked();
    let close = closed || (!outside && response.should_close());
    (response.inner, close)
}

/// The margin around the bars and panels of the main window, and inside every dialog.
pub const MARGIN: i8 = 8;

/// The frame of a dialog: the popup frame with the same padding all round.
pub fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::popup(&ctx.global_style()).inner_margin(MARGIN)
}

/// The modal a dialog is shown in.
pub fn modal(ctx: &egui::Context, id: &str) -> egui::Modal {
    egui::Modal::new(egui::Id::unique(id)).frame(modal_frame(ctx))
}

/// `width`, less on a window too narrow for it.
pub fn dialog_width(ctx: &egui::Context, width: f32) -> f32 {
    width.min(ctx.content_rect().width() - 60.0)
}

/// The title of a dialog, for `ui.heading`: strong, like the titles of its sections, so that it is not weaker than the text under it.
pub fn title_text(title: &str) -> RichText {
    RichText::new(title).strong()
}

/// The width `ui.button(text)` has, to leave room for it beside a field.
pub fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    (galley.size().x + 2.0 * ui.spacing().button_padding.x).max(ui.spacing().interact_size.x)
}

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

/// A bar button whose label, icon included, is centred in it. The label is
/// painted at the centre of the button, so it is not left to the button's own
/// layout.
pub fn centred_bar_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    let height = bar_height(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    // What a screen reader and the tests see: a button with this label.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    let visuals = *ui.style().interact(&response);
    let painter = ui.painter();
    painter.rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let at = rect.center() - galley.size() / 2.0;
    painter.galley(at, galley, visuals.text_color());
    response
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

/// The size of the title of a part of a window.
pub fn section_size(ui: &egui::Ui) -> f32 {
    egui::TextStyle::Body.resolve(ui.style()).size * 1.15
}

/// The title of a part of a window, set apart from the part above.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(SPACE_L);
    let size = section_size(ui);
    ui.label(RichText::new(title).strong().size(size));
}

/// A line of secondary explanation.
pub fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).weak());
}

/// A one-line text field with its text centred vertically. egui puts the text at the top of a field taller than it when the layout runs top to bottom (a grid, a dialog) and centres it only inside `ui.horizontal`; fields are `interact_size.y` high, so all of them go through here.
pub fn single_line(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).align(egui::Align2::LEFT_CENTER)
}

/// A selectable list row as wide as the space: the name on the left,
/// shortened if need be, and weaker details on the right when they fit.
pub fn list_row(ui: &mut egui::Ui, selected: bool, name: &str, details: &str) -> egui::Response {
    let padding = ui.spacing().button_padding;
    let text_style = egui::TextStyle::Button;
    let height = ui.text_style_height(&text_style) + 2.0 * padding.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    // Screen readers (and tests) get the details too.
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, format!("{name}  {details}")));
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
    (ctx.content_rect().height() - 330.0).clamp(80.0, 260.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn single_line_text_is_centred_in_a_tall_field() {
        let mut text = String::from("My mods");
        let mut centres = Vec::new();
        // The field has no label here, which the harness's accessibility check would refuse.
        let mut harness = egui_kittest::Harness::builder().with_accessibility_check(false).build_ui(|ui| {
            ui.spacing_mut().interact_size.y = 28.0;
            // A top-down layout, where egui would put the text at the top.
            let output = super::single_line(&mut text).show(ui);
            centres.push((output.galley_pos.y + output.galley.size().y / 2.0, output.response.rect.center().y));
        });
        harness.run_steps(3);
        drop(harness);
        let (text_y, field_y) = *centres.last().expect("a frame ran");
        assert!((text_y - field_y).abs() <= 1.0, "text centre {text_y}, field centre {field_y}");
    }

    #[test]
    fn single_line_fields_go_through_the_helper() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("widgets.rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                for forbidden in ["TextEdit::singleline(", "text_edit_singleline("] {
                    assert!(!source.contains(forbidden), "{}: use widgets::single_line, which centres the text", path.display());
                }
            }
        }
    }
}
