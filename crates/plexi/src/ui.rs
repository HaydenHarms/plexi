//! Chrome around the canvas: the floating toolbars, page controls, command palette,
//! export/settings/help windows, toasts, and the hand-drawn icon set.

use egui::{
    Align2, Color32, Context, CornerRadius, Frame, Id, Key, Order, Painter, Pos2, Rect, Response, Sense, Shadow,
    Shape as EShape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use std::sync::mpsc::Receiver;

use plexi_core::export::ExportFormat;
use plexi_core::export::layout::{Area, Frame as OutFrame, PAPERS};
use plexi_core::{Background, Color, Element, PAPER_COLORS, PageSize, ShapeKind};

use crate::app::{ExportScope, Mode, Overlay, PaperChoice, PlexiApp, SnapshotAction, Tool};
use crate::canvas::color32;
use crate::history::Target;

pub const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x7c, 0xf6);

pub fn install_style(ctx: &Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(6.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.visuals.window_corner_radius = CornerRadius::same(12);
        s.visuals.menu_corner_radius = CornerRadius::same(10);
        s.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        s.visuals.hyperlink_color = ACCENT;
        s.interaction.tooltip_delay = 0.35;
    });
}

struct Theme {
    canvas: Color32,
    panel: Color32,
    border: Color32,
    ink: Color32,
    hover: Color32,
}

fn theme(ui: &Ui) -> Theme {
    if ui.visuals().dark_mode {
        Theme {
            canvas: Color32::from_rgb(0x1c, 0x1e, 0x23),
            panel: Color32::from_rgb(0x2a, 0x2d, 0x34),
            border: Color32::from_rgb(0x3b, 0x3f, 0x48),
            ink: Color32::from_rgb(0xe6, 0xe8, 0xec),
            hover: Color32::from_white_alpha(18),
        }
    } else {
        Theme {
            canvas: Color32::from_rgb(0xe6, 0xe8, 0xec),
            panel: Color32::from_rgb(0xfd, 0xfd, 0xfe),
            border: Color32::from_rgb(0xd5, 0xd9, 0xe0),
            ink: Color32::from_rgb(0x26, 0x2a, 0x33),
            hover: Color32::from_black_alpha(14),
        }
    }
}

fn pill(ui: &Ui) -> Frame {
    let t = theme(ui);
    Frame::new()
        .fill(t.panel)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(14))
        .shadow(Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(40) })
        .inner_margin(6)
}

// ----- icons ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Pen,
    Highlighter,
    Eraser,
    Lasso,
    Text,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Undo,
    Redo,
    Menu,
    Screen,
    Trash,
    Camera,
    ClickThrough,
    Plus,
    Left,
    Right,
    Notebook,
}

fn tool_icon(t: Tool) -> Icon {
    match t {
        Tool::Pen => Icon::Pen,
        Tool::Highlighter => Icon::Highlighter,
        Tool::Eraser => Icon::Eraser,
        Tool::Lasso => Icon::Lasso,
        Tool::Text => Icon::Text,
        Tool::Shape(ShapeKind::Line) => Icon::Line,
        Tool::Shape(ShapeKind::Arrow) => Icon::Arrow,
        Tool::Shape(ShapeKind::Rect) => Icon::Rect,
        Tool::Shape(ShapeKind::Ellipse) => Icon::Ellipse,
    }
}

/// Icons are drawn on a 16x16 grid mapped onto `r`.
pub fn paint_icon(p: &Painter, r: Rect, icon: Icon, c: Color32) {
    let s = r.width() / 16.0;
    let at = |x: f32, y: f32| pos2(r.left() + x * s, r.top() + y * s);
    let st = Stroke::new(1.6 * s.max(0.9), c);
    let line = |pts: &[(f32, f32)]| {
        p.add(EShape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), st));
    };
    let arc = |cx: f32, cy: f32, rad: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|k| {
                let a = a0 + (a1 - a0) * k as f32 / 16.0;
                at(cx + rad * a.cos(), cy + rad * a.sin())
            })
            .collect()
    };
    match icon {
        Icon::Pen => {
            line(&[(3.0, 13.0), (4.0, 9.5), (11.5, 2.0), (14.0, 4.5), (6.5, 12.0), (3.0, 13.0)]);
            line(&[(10.0, 3.5), (12.5, 6.0)]);
        }
        Icon::Highlighter => {
            p.add(EShape::line(vec![at(2.5, 13.0), at(13.5, 13.0)], Stroke::new(3.0 * s, c.gamma_multiply(0.45))));
            line(&[(5.0, 11.0), (6.0, 7.5), (11.5, 2.0), (14.0, 4.5), (8.5, 10.0), (5.0, 11.0)]);
        }
        Icon::Eraser => {
            line(&[(2.5, 10.0), (8.5, 4.0), (13.5, 9.0), (9.5, 13.0), (5.5, 13.0), (2.5, 10.0)]);
            line(&[(5.5, 7.0), (10.5, 12.0)]);
            line(&[(9.5, 13.0), (14.0, 13.0)]);
        }
        Icon::Lasso => {
            let pts = arc(8.0, 6.5, 5.5, 0.0, std::f32::consts::TAU);
            p.extend(EShape::dashed_line(&pts, st, 2.2 * s, 1.6 * s));
            line(&[(4.5, 10.5), (4.0, 13.0), (6.0, 14.5)]);
        }
        Icon::Text => {
            line(&[(3.0, 4.0), (3.0, 2.5), (13.0, 2.5), (13.0, 4.0)]);
            line(&[(8.0, 2.5), (8.0, 13.5)]);
            line(&[(6.0, 13.5), (10.0, 13.5)]);
        }
        Icon::Line => line(&[(3.0, 13.0), (13.0, 3.0)]),
        Icon::Arrow => {
            line(&[(3.0, 13.0), (13.0, 3.0)]);
            line(&[(7.0, 3.0), (13.0, 3.0), (13.0, 9.0)]);
        }
        Icon::Rect => {
            p.rect_stroke(Rect::from_min_max(at(2.5, 3.5), at(13.5, 12.5)), 1.5 * s, st, StrokeKind::Middle);
        }
        Icon::Ellipse => {
            p.add(EShape::ellipse_stroke(at(8.0, 8.0), vec2(5.8, 4.6) * s, st));
        }
        Icon::Undo | Icon::Redo => {
            let mirror = icon == Icon::Redo;
            let mx = |x: f32| if mirror { 16.0 - x } else { x };
            let pts: Vec<Pos2> = arc(8.5, 9.0, 4.5, -std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)
                .into_iter()
                .map(|q| if mirror { pos2(r.left() + r.right() - q.x, q.y) } else { q })
                .collect();
            let mut path = vec![at(mx(3.0), 4.5)];
            path.extend(pts);
            path.push(at(mx(5.0), 13.5));
            p.add(EShape::line(path, st));
            line(&[(mx(5.5), 2.0), (mx(3.0), 4.5), (mx(5.5), 7.0)]);
        }
        Icon::Menu => {
            for y in [4.0, 8.0, 12.0] {
                line(&[(2.5, y), (13.5, y)]);
            }
        }
        Icon::Screen => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 2.5), at(14.5, 11.5)), 1.5 * s, st, StrokeKind::Middle);
            line(&[(5.5, 14.0), (10.5, 14.0)]);
            line(&[(4.0, 9.0), (6.0, 6.0), (8.0, 8.0), (11.5, 4.5)]);
        }
        Icon::Trash => {
            line(&[(2.5, 4.0), (13.5, 4.0)]);
            line(&[(6.0, 4.0), (6.5, 2.0), (9.5, 2.0), (10.0, 4.0)]);
            line(&[(4.0, 4.0), (5.0, 14.0), (11.0, 14.0), (12.0, 4.0)]);
        }
        Icon::Camera => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 4.5), at(14.5, 13.5)), 2.0 * s, st, StrokeKind::Middle);
            line(&[(5.0, 4.5), (6.0, 2.5), (10.0, 2.5), (11.0, 4.5)]);
            p.circle_stroke(at(8.0, 9.0), 2.6 * s, st);
        }
        Icon::ClickThrough => {
            line(&[
                (4.0, 2.0),
                (4.0, 13.0),
                (6.8, 10.4),
                (8.8, 14.5),
                (10.6, 13.6),
                (8.6, 9.6),
                (12.5, 9.4),
                (4.0, 2.0),
            ]);
        }
        Icon::Plus => {
            line(&[(8.0, 3.0), (8.0, 13.0)]);
            line(&[(3.0, 8.0), (13.0, 8.0)]);
        }
        Icon::Left => line(&[(10.0, 3.0), (5.0, 8.0), (10.0, 13.0)]),
        Icon::Right => line(&[(6.0, 3.0), (11.0, 8.0), (6.0, 13.0)]),
        Icon::Notebook => {
            p.rect_stroke(Rect::from_min_max(at(3.0, 1.5), at(13.5, 14.5)), 1.5 * s, st, StrokeKind::Middle);
            line(&[(5.5, 1.5), (5.5, 14.5)]);
            line(&[(8.0, 5.5), (11.5, 5.5)]);
            line(&[(8.0, 8.5), (11.5, 8.5)]);
        }
    }
}

fn icon_button(ui: &mut Ui, icon: Icon, selected: bool, tip: &str) -> Response {
    let t = theme(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
    let bg = if selected {
        ACCENT.gamma_multiply(0.2)
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 9.0, bg);
    let c = if selected { ACCENT } else { t.ink };
    paint_icon(ui.painter(), rect.shrink(9.0), icon, c);
    resp.on_hover_text(tip)
}

fn swatch(ui: &mut Ui, c: Color, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 34.0), Sense::click());
    let t = theme(ui);
    let center = rect.center();
    if selected {
        ui.painter().circle_stroke(center, 10.5, Stroke::new(2.0, ACCENT));
    } else if resp.hovered() {
        ui.painter().circle_filled(center, 11.0, t.hover);
    }
    let fill = color32(c.with_alpha(c.a().max(150)));
    ui.painter().circle_filled(center, 7.5, fill);
    ui.painter().circle_stroke(center, 7.5, Stroke::new(1.0, t.border));
    resp.on_hover_text(c.hex_rgb())
}

fn width_dot(ui: &mut Ui, w: f32, selected: bool, ink: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 34.0), Sense::click());
    let t = theme(ui);
    if selected {
        ui.painter().rect_filled(rect.shrink2(vec2(0.0, 5.0)), 7.0, ACCENT.gamma_multiply(0.2));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect.shrink2(vec2(0.0, 5.0)), 7.0, t.hover);
    }
    ui.painter().circle_filled(rect.center(), (w * 0.5).clamp(1.2, 8.0), ink);
    resp
}

fn vsep(ui: &mut Ui) {
    let t = theme(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(7.0, 34.0), Sense::hover());
    ui.painter().line_segment(
        [rect.center_top() + vec2(0.0, 8.0), rect.center_bottom() - vec2(0.0, 8.0)],
        Stroke::new(1.0, t.border),
    );
}

// ----- toolbars ---------------------------------------------------------------------

fn tool_buttons(app: &mut PlexiApp, ui: &mut Ui, overlay: bool) {
    for tool in [Tool::Pen, Tool::Highlighter, Tool::Eraser, Tool::Lasso, Tool::Text] {
        if icon_button(ui, tool_icon(tool), app.tool == tool, tool.label()).clicked() {
            app.tool = tool;
        }
    }
    let shapes = [ShapeKind::Arrow, ShapeKind::Rect, ShapeKind::Ellipse, ShapeKind::Line];
    if overlay {
        for k in [ShapeKind::Arrow, ShapeKind::Rect] {
            let tool = Tool::Shape(k);
            if icon_button(ui, tool_icon(tool), app.tool == tool, tool.label()).clicked() {
                app.tool = tool;
            }
        }
    } else {
        let current = match app.tool {
            Tool::Shape(k) => k,
            _ => ui.data(|d| d.get_temp::<ShapeKind>(Id::unique("last-shape"))).unwrap_or(ShapeKind::Arrow),
        };
        let selected = matches!(app.tool, Tool::Shape(_));
        let resp = icon_button(ui, tool_icon(Tool::Shape(current)), selected, "Shapes");
        egui::Popup::menu(&resp).show(|ui| {
            ui.horizontal(|ui| {
                for k in shapes {
                    let tool = Tool::Shape(k);
                    if icon_button(ui, tool_icon(tool), app.tool == tool, tool.label()).clicked() {
                        app.tool = tool;
                        ui.data_mut(|d| d.insert_temp(Id::unique("last-shape"), k));
                        ui.close();
                    }
                }
            });
        });
    }
}

fn color_and_width(app: &mut PlexiApp, ui: &mut Ui) {
    let highlighter = app.tool == Tool::Highlighter;
    let colors = if highlighter { app.config.highlighter_colors.clone() } else { app.config.pen_colors.clone() };
    let current = if highlighter { app.highlighter_color } else { app.pen_color };
    for c in colors {
        if swatch(ui, c, c == current).clicked() {
            app.pick_color(c, highlighter);
        }
    }
    vsep(ui);
    let ink = color32(current.with_alpha(255));
    let (presets, value): (&[f32], f32) = match app.tool {
        Tool::Highlighter => (&[10.0, 20.0, 36.0], app.config.highlighter_width),
        Tool::Eraser => (&[8.0, 14.0, 32.0], app.config.eraser_size),
        _ => (&[1.5, 3.0, 6.0], app.config.pen_width),
    };
    for (i, w) in presets.iter().enumerate() {
        let shown = [3.0, 6.5, 11.0][i];
        if width_dot(ui, shown, (value - w).abs() < 0.01, ink).on_hover_text(format!("Width {w}")).clicked() {
            match app.tool {
                Tool::Highlighter => app.config.highlighter_width = *w,
                Tool::Eraser => app.config.eraser_size = *w,
                _ => app.config.pen_width = *w,
            }
        }
    }
}

pub fn notebook(app: &mut PlexiApp, ui: &mut Ui, rect: Rect) {
    let th = theme(ui);
    let ctx = ui.ctx().clone();
    app.canvas(ui, rect, Target::Page(app.page), th.canvas);

    egui::Area::new(Id::unique("nb-toolbar"))
        .anchor(Align2::CENTER_TOP, vec2(0.0, 10.0))
        .order(Order::Foreground)
        .show(&ctx, |ui| {
            pill(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    let menu = icon_button(ui, Icon::Menu, false, "Menu");
                    egui::Popup::menu(&menu).show(|ui| file_menu(app, ui));
                    vsep(ui);
                    tool_buttons(app, ui, false);
                    vsep(ui);
                    color_and_width(app, ui);
                    vsep(ui);
                    if icon_button(ui, Icon::Undo, false, "Undo (Ctrl+Z)").clicked() {
                        app.undo();
                    }
                    if icon_button(ui, Icon::Redo, false, "Redo (Ctrl+Y)").clicked() {
                        app.redo();
                    }
                    vsep(ui);
                    let tip = format!("Draw on your screen ({})", app.config.hotkey_draw);
                    if accent_button(ui, Icon::Screen, "Draw on screen").on_hover_text(tip).clicked() {
                        app.enter_overlay(&ctx, false);
                    }
                });
            });
        });

    page_bar(app, &ctx);

    egui::Area::new(Id::unique("nb-zoom"))
        .anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -12.0))
        .order(Order::Foreground)
        .show(&ctx, |ui| {
            pill(ui).inner_margin(2).show(ui, |ui| {
                let label = format!("{:.0}%", app.view.zoom * 100.0);
                if ui.add(egui::Button::new(label).frame(false)).on_hover_text("Fit page (Ctrl+0)").clicked() {
                    app.fit_pending = true;
                }
            });
        });
}

fn accent_button(ui: &mut Ui, icon: Icon, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(14.0), Color32::WHITE);
    let size = vec2(galley.size().x + 44.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { ACCENT.gamma_multiply(0.85) } else { ACCENT };
    ui.painter().rect_filled(rect, 9.0, fill);
    paint_icon(ui.painter(), Rect::from_min_size(rect.min + vec2(10.0, 9.0), vec2(16.0, 16.0)), icon, Color32::WHITE);
    ui.painter().galley(rect.min + vec2(34.0, (34.0 - galley.size().y) / 2.0), galley, Color32::WHITE);
    resp
}

fn page_bar(app: &mut PlexiApp, ctx: &Context) {
    egui::Area::new(Id::unique("nb-pages"))
        .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -12.0))
        .order(Order::Foreground)
        .show(ctx, |ui| {
            pill(ui).inner_margin(2).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    if icon_button(ui, Icon::Left, false, "Previous page (PgUp)").clicked() {
                        app.go_to_page(app.page.saturating_sub(1));
                    }
                    let label =
                        format!("{}  ·  {} / {}", app.doc.pages[app.page].name, app.page + 1, app.doc.pages.len());
                    let resp = ui.add(egui::Button::new(label).frame(false).min_size(vec2(0.0, 34.0)));
                    egui::Popup::menu(&resp).show(|ui| page_menu(app, ui));
                    if icon_button(ui, Icon::Right, false, "Next page (PgDn)").clicked() {
                        app.go_to_page(app.page + 1);
                    }
                    if icon_button(ui, Icon::Plus, false, "New page (Ctrl+Enter)").clicked() {
                        app.add_page();
                    }
                    let tip = if app.doc.pages.len() > 1 { "Delete this page" } else { "Clear this page" };
                    if icon_button(ui, Icon::Trash, false, tip).clicked() {
                        app.delete_page();
                    }
                });
            });
        });
}

fn page_menu(app: &mut PlexiApp, ui: &mut Ui) {
    ui.set_min_width(220.0);
    let i = app.page;
    ui.label("Name");
    if ui.text_edit_singleline(&mut app.doc.pages[i].name).changed() {
        app.touch();
    }
    ui.separator();
    ui.label("Background");
    ui.horizontal(|ui| {
        for bg in Background::ALL {
            if ui.selectable_label(app.doc.pages[i].background == bg, bg.label()).clicked() {
                app.checkpoint_pages();
                app.doc.pages[i].background = bg;
                app.config.new_page_background = bg;
            }
        }
    });
    ui.label("Paper color");
    paper_colors(app, ui, i);
    ui.label("Size");
    ui.horizontal(|ui| {
        for (size, label) in [(PageSize::Infinite, "Infinite"), (PageSize::Letter, "Letter"), (PageSize::A4, "A4")] {
            if ui.selectable_label(app.doc.pages[i].size == size, label).clicked() {
                app.checkpoint_pages();
                app.doc.pages[i].size = size;
                app.config.new_page_size = size;
                app.fit_pending = true;
            }
        }
    });
    if app.doc.pages[i].size == PageSize::Infinite {
        sheet_settings(app, ui);
    }
    ui.separator();
    if ui.button("Add page after this one").clicked() {
        app.add_page();
        ui.close();
    }
    let n = app.doc.pages.len();
    if ui.add_enabled(i > 0, egui::Button::new("Move page up")).clicked() {
        app.checkpoint_pages();
        app.doc.pages.swap(i, i - 1);
        app.page = i - 1;
    }
    if ui.add_enabled(i + 1 < n, egui::Button::new("Move page down")).clicked() {
        app.checkpoint_pages();
        app.doc.pages.swap(i, i + 1);
        app.page = i + 1;
    }
    let label = if n == 1 { "Clear page" } else { "Delete page" };
    if ui.button(label).clicked() {
        app.delete_page();
        ui.close();
    }
    ui.separator();
    ui.label(egui::RichText::new("Pages").weak());
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        let mut delete = None;
        for k in 0..n {
            let name = format!("{}. {}", k + 1, app.doc.pages[k].name);
            ui.horizontal(|ui| {
                let x = ui.add(egui::Button::new("✕").frame(false).small()).on_hover_text("Delete this page");
                if x.clicked() {
                    delete = Some(k);
                }
                if ui.selectable_label(k == app.page, name).clicked() {
                    app.go_to_page(k);
                    ui.close();
                }
            });
        }
        if let Some(k) = delete {
            app.delete_page_at(k);
        }
    });
}

/// Paper color swatches plus a custom picker for page `i`.
fn paper_colors(app: &mut PlexiApp, ui: &mut Ui, i: usize) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let current = app.doc.pages[i].paper;
        for (name, c) in PAPER_COLORS {
            if swatch(ui, c, c == current).on_hover_text(name).clicked() {
                app.checkpoint_pages();
                set_paper(app, i, c);
            }
        }
        ui.add_space(6.0);
        let mut rgba = color32(current);
        if ui.color_edit_button_srgba(&mut rgba).on_hover_text("Any color").changed() {
            let [r, g, b, _] = rgba.to_srgba_unmultiplied();
            set_paper(app, i, Color::rgb(r, g, b));
        }
    });
    if app.doc.pages.len() > 1 && ui.small_button("Use this color on every page").clicked() {
        app.checkpoint_pages();
        let c = app.doc.pages[i].paper;
        for p in &mut app.doc.pages {
            p.paper = c;
        }
    }
}

fn set_paper(app: &mut PlexiApp, i: usize, c: Color) {
    app.doc.pages[i].paper = c;
    app.config.new_page_paper = c;
    app.touch();
}

/// Sheet outlines on an infinite page: whether to show them and how big they are.
fn sheet_settings(app: &mut PlexiApp, ui: &mut Ui) {
    let c = &mut app.config;
    ui.checkbox(&mut c.sheet_guides, "Show sheet outlines")
        .on_hover_text("Outlines appear wherever you draw; PDF export prints one sheet per outline");
    if c.sheet_guides {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("sheet-paper")
                .selected_text(PAPERS[c.sheet_paper.min(PAPERS.len() - 1)].0)
                .show_ui(ui, |ui| {
                    for (k, (name, _)) in PAPERS.iter().enumerate() {
                        ui.selectable_value(&mut c.sheet_paper, k, *name);
                    }
                });
            ui.selectable_value(&mut c.sheet_landscape, false, "Portrait");
            ui.selectable_value(&mut c.sheet_landscape, true, "Landscape");
        });
    }
}

fn file_menu(app: &mut PlexiApp, ui: &mut Ui) {
    ui.set_min_width(230.0);
    let ctx = ui.ctx().clone();
    let item = |ui: &mut Ui, text: &str, shortcut: &str| {
        ui.add(egui::Button::new(text).shortcut_text(shortcut).min_size(vec2(220.0, 0.0)))
    };
    if item(ui, "New notes", "Ctrl+N").clicked() {
        ui.close();
        app.new_document();
    }
    if item(ui, "Open…", "Ctrl+O").clicked() {
        ui.close();
        app.open_dialog();
    }
    let recent: Vec<_> = app.config.recent.iter().filter(|p| p.exists()).cloned().collect();
    if !recent.is_empty() {
        ui.menu_button("Open recent", |ui| {
            for p in recent {
                let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                    ui.close();
                    app.open_path(&p);
                }
            }
        });
    }
    if item(ui, "Save", "Ctrl+S").clicked() {
        ui.close();
        app.save();
    }
    if item(ui, "Save as…", "Ctrl+Shift+S").clicked() {
        ui.close();
        app.save_as();
    }
    if item(ui, "Export…", "Ctrl+E").clicked() {
        ui.close();
        app.open_export_dialog();
    }
    ui.separator();
    if item(ui, "Draw on screen", &app.config.hotkey_draw.clone()).clicked() {
        ui.close();
        app.enter_overlay(&ctx, false);
    }
    if item(ui, "Command palette", "Ctrl+K").clicked() {
        ui.close();
        app.palette.open = true;
    }
    if item(ui, "Settings", "Ctrl+,").clicked() {
        ui.close();
        app.settings_open = true;
    }
    if item(ui, "Keyboard shortcuts", "F1").clicked() {
        ui.close();
        app.help_open = true;
    }
    ui.separator();
    if item(ui, "Quit", "Alt+F4").clicked() {
        app.quit_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

pub fn overlay(app: &mut PlexiApp, ui: &mut Ui, rect: Rect, o: &Overlay) {
    let ctx = ui.ctx().clone();
    if !o.passthrough {
        // An almost invisible wash so clicks on empty screen land on the overlay.
        ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(1));
    }
    app.canvas(ui, rect, Target::Overlay, Color32::TRANSPARENT);
    let now = ui.input(|i| i.time);

    if o.passthrough {
        if now < o.hint_until {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            egui::Area::new(Id::unique("ov-hint"))
                .anchor(Align2::CENTER_TOP, vec2(0.0, 14.0))
                .order(Order::Foreground)
                .show(&ctx, |ui| {
                    pill(ui).show(ui, |ui| {
                        ui.label(format!(
                            "Click-through — your ink stays put.  {} to draw again",
                            app.config.hotkey_draw
                        ));
                    });
                });
        }
        return;
    }
    if o.snapshot.is_some() {
        return;
    }
    // A thin frame shows that the screen is in drawing mode.
    ui.painter().rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0, ACCENT.gamma_multiply(0.7)), StrokeKind::Inside);

    egui::Area::new(Id::unique("ov-toolbar"))
        .anchor(Align2::CENTER_TOP, vec2(0.0, 12.0))
        .order(Order::Foreground)
        .show(&ctx, |ui| {
            pill(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    tool_buttons(app, ui, true);
                    vsep(ui);
                    color_and_width(app, ui);
                    vsep(ui);
                    if icon_button(ui, Icon::Undo, false, "Undo (Ctrl+Z)").clicked() {
                        app.undo();
                    }
                    if icon_button(
                        ui,
                        Icon::Trash,
                        false,
                        format!("Clear screen ink (C, or {} anywhere)", app.config.hotkey_clear).as_str(),
                    )
                    .clicked()
                    {
                        app.clear_overlay();
                    }
                    let snap = icon_button(ui, Icon::Camera, false, "Snapshot");
                    egui::Popup::menu(&snap).show(|ui| {
                        ui.set_min_width(240.0);
                        if ui.add(egui::Button::new("Copy screenshot with ink").shortcut_text("Ctrl+Shift+C")).clicked()
                        {
                            ui.close();
                            app.request_snapshot(SnapshotAction::Copy);
                        }
                        if ui.button("Save as a notebook page").clicked() {
                            ui.close();
                            app.request_snapshot(SnapshotAction::ToPage);
                        }
                        if ui.button("Save image…").clicked() {
                            ui.close();
                            app.request_snapshot(SnapshotAction::SaveImage);
                        }
                        if ui.button("Export ink only…").clicked() {
                            ui.close();
                            app.open_export_dialog();
                        }
                    });
                    vsep(ui);
                    let tip = format!(
                        "Click-through: keep the ink, use your desktop (Esc). {} to draw again",
                        app.config.hotkey_draw
                    );
                    if icon_button(ui, Icon::ClickThrough, false, &tip).clicked() {
                        app.set_passthrough(&ctx, true);
                    }
                    if icon_button(ui, Icon::Notebook, false, "Back to the notebook (N)").clicked() {
                        app.exit_overlay(&ctx);
                    }
                });
            });
        });
}

// ----- dialogs ----------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    Export,
    DrawOnScreen,
    ClickThrough,
    Notebook,
    ClearScreen,
    SnapCopy,
    SnapPage,
    AddPage,
    DeletePage,
    NextPage,
    PrevPage,
    Undo,
    Redo,
    Tool(Tool),
    Background(Background),
    Size(PageSize),
    SheetGuides,
    Fit,
    SelectAll,
    Delete,
    Duplicate,
    PartialEraser,
    Settings,
    Shortcuts,
    Quit,
}

fn commands(app: &PlexiApp) -> Vec<(Cmd, String, String)> {
    let hk = &app.config;
    let mut v: Vec<(Cmd, String, String)> = vec![
        (Cmd::DrawOnScreen, "Draw on screen".into(), hk.hotkey_draw.clone()),
        (Cmd::ClickThrough, "Click-through (keep screen ink, use desktop)".into(), "Esc".into()),
        (Cmd::Notebook, "Back to notebook".into(), hk.hotkey_notebook.clone()),
        (Cmd::ClearScreen, "Clear screen ink".into(), hk.hotkey_clear.clone()),
        (Cmd::SnapCopy, "Copy screenshot with ink".into(), "Ctrl+Shift+C".into()),
        (Cmd::SnapPage, "Save screen as notebook page".into(), String::new()),
        (Cmd::New, "New notes".into(), "Ctrl+N".into()),
        (Cmd::Open, "Open…".into(), "Ctrl+O".into()),
        (Cmd::Save, "Save".into(), "Ctrl+S".into()),
        (Cmd::SaveAs, "Save as…".into(), "Ctrl+Shift+S".into()),
        (Cmd::Export, "Export… (PDF, PNG, SVG, Markdown, …)".into(), "Ctrl+E".into()),
        (Cmd::AddPage, "Add page".into(), "Ctrl+Enter".into()),
        (Cmd::DeletePage, "Delete page".into(), String::new()),
        (Cmd::NextPage, "Next page".into(), "PgDn".into()),
        (Cmd::PrevPage, "Previous page".into(), "PgUp".into()),
        (Cmd::Undo, "Undo".into(), "Ctrl+Z".into()),
        (Cmd::Redo, "Redo".into(), "Ctrl+Y".into()),
        (Cmd::Fit, "Fit page to window".into(), "Ctrl+0".into()),
        (Cmd::SelectAll, "Select all".into(), "Ctrl+A".into()),
        (Cmd::Delete, "Delete selection".into(), "Del".into()),
        (Cmd::Duplicate, "Duplicate selection".into(), "Ctrl+D".into()),
        (
            Cmd::PartialEraser,
            format!("Eraser: {}", if app.config.eraser_partial { "erase whole strokes" } else { "erase partially" }),
            String::new(),
        ),
        (Cmd::Settings, "Settings".into(), "Ctrl+,".into()),
        (Cmd::Shortcuts, "Keyboard shortcuts".into(), "F1".into()),
        (Cmd::Quit, "Quit Plexi".into(), String::new()),
    ];
    for t in [
        Tool::Pen,
        Tool::Highlighter,
        Tool::Eraser,
        Tool::Lasso,
        Tool::Text,
        Tool::Shape(ShapeKind::Line),
        Tool::Shape(ShapeKind::Arrow),
        Tool::Shape(ShapeKind::Rect),
        Tool::Shape(ShapeKind::Ellipse),
    ] {
        let l = t.label();
        let (name, key) = match l.find(" (") {
            Some(i) => (&l[..i], l[i + 2..].split(')').next().unwrap_or("")),
            None => (l, ""),
        };
        v.push((Cmd::Tool(t), format!("Tool: {name}"), key.to_owned()));
    }
    for bg in Background::ALL {
        v.push((Cmd::Background(bg), format!("Page background: {}", bg.label()), String::new()));
    }
    for (s, l) in [(PageSize::Infinite, "Infinite"), (PageSize::Letter, "Letter"), (PageSize::A4, "A4")] {
        v.push((Cmd::Size(s), format!("Page size: {l}"), String::new()));
    }
    let guides = if app.config.sheet_guides { "Hide sheet outlines" } else { "Show sheet outlines" };
    v.push((Cmd::SheetGuides, guides.into(), String::new()));
    v
}

fn run_cmd(app: &mut PlexiApp, ctx: &Context, cmd: Cmd) {
    match cmd {
        Cmd::New => app.new_document(),
        Cmd::Open => app.open_dialog(),
        Cmd::Save => app.save(),
        Cmd::SaveAs => {
            app.save_as();
        }
        Cmd::Export => app.open_export_dialog(),
        Cmd::DrawOnScreen => app.enter_overlay(ctx, false),
        Cmd::ClickThrough => {
            if !app.is_overlay() {
                app.enter_overlay(ctx, true);
            } else {
                app.set_passthrough(ctx, true);
            }
        }
        Cmd::Notebook => app.exit_overlay(ctx),
        Cmd::ClearScreen => app.clear_overlay(),
        Cmd::SnapCopy | Cmd::SnapPage => {
            if !app.is_overlay() {
                app.enter_overlay(ctx, false);
            }
            let a = if matches!(cmd, Cmd::SnapCopy) { SnapshotAction::Copy } else { SnapshotAction::ToPage };
            app.request_snapshot(a);
        }
        Cmd::AddPage => app.add_page(),
        Cmd::DeletePage => app.delete_page(),
        Cmd::NextPage => app.go_to_page(app.page + 1),
        Cmd::PrevPage => app.go_to_page(app.page.saturating_sub(1)),
        Cmd::Undo => app.undo(),
        Cmd::Redo => app.redo(),
        Cmd::Tool(t) => app.tool = t,
        Cmd::Background(bg) => {
            app.checkpoint_pages();
            let p = app.page;
            app.doc.pages[p].background = bg;
        }
        Cmd::Size(s) => {
            app.checkpoint_pages();
            let p = app.page;
            app.doc.pages[p].size = s;
            app.fit_pending = true;
        }
        Cmd::SheetGuides => app.config.sheet_guides = !app.config.sheet_guides,
        Cmd::Fit => app.fit_pending = true,
        Cmd::SelectAll => app.select_all(),
        Cmd::Delete => app.delete_selection(),
        Cmd::Duplicate => app.duplicate_selection(),
        Cmd::PartialEraser => app.config.eraser_partial = !app.config.eraser_partial,
        Cmd::Settings => app.settings_open = true,
        Cmd::Shortcuts => app.help_open = true,
        Cmd::Quit => {
            app.quit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn fuzzy(hay: &str, needle: &str) -> bool {
    let hay = hay.to_lowercase();
    let mut it = hay.chars();
    needle.to_lowercase().chars().filter(|c| !c.is_whitespace()).all(|c| it.any(|h| h == c))
}

fn palette(app: &mut PlexiApp, ctx: &Context) {
    if !app.palette.open {
        return;
    }
    let cmds = commands(app);
    let q = app.palette.query.clone();
    let shown: Vec<_> = cmds.into_iter().filter(|(_, l, _)| q.is_empty() || fuzzy(l, &q)).collect();
    let (up, down, enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
        )
    });
    if esc {
        app.palette.open = false;
        return;
    }
    if down {
        app.palette.selected = (app.palette.selected + 1).min(shown.len().saturating_sub(1));
    }
    if up {
        app.palette.selected = app.palette.selected.saturating_sub(1);
    }
    app.palette.selected = app.palette.selected.min(shown.len().saturating_sub(1));
    let mut run = if enter { shown.get(app.palette.selected).map(|c| c.0) } else { None };

    egui::Area::new(Id::unique("palette")).anchor(Align2::CENTER_TOP, vec2(0.0, 70.0)).order(Order::Tooltip).show(
        ctx,
        |ui| {
            pill(ui).inner_margin(10).show(ui, |ui| {
                ui.set_width(460.0);
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut app.palette.query)
                        .hint_text("Type a command…")
                        .desired_width(f32::INFINITY),
                );
                edit.request_focus();
                if edit.changed() {
                    app.palette.selected = 0;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    for (i, (cmd, label, key)) in shown.iter().enumerate() {
                        let sel = i == app.palette.selected;
                        let resp = ui.add(
                            egui::Button::selectable(sel, label.as_str())
                                .shortcut_text(key.as_str())
                                .min_size(vec2(450.0, 26.0)),
                        );
                        if sel && (up || down) {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            run = Some(*cmd);
                        }
                    }
                    if shown.is_empty() {
                        ui.weak("No matching command");
                    }
                });
            });
        },
    );
    if let Some(cmd) = run {
        app.palette.open = false;
        run_cmd(app, ctx, cmd);
    }
}

// ----- export dialog ------------------------------------------------------------

/// Side of the preview box in points.
const PREVIEW_SIDE: f32 = 340.0;

type PreviewResult = Result<image::RgbaImage, String>;

/// The export dialog's live preview: the output pages the current settings produce, and
/// a rendering of the one being looked at (made on a worker thread).
#[derive(Default)]
pub struct Preview {
    /// Settings the frame list was built for.
    key: String,
    /// Every output page: which page it shows (`None` is the screen ink) and its frame.
    frames: Vec<(Option<usize>, OutFrame)>,
    pub index: usize,
    texture: Option<egui::TextureHandle>,
    /// `key#index` of the rendering in `texture`.
    shown: String,
    pending: Option<(String, Receiver<PreviewResult>)>,
    error: Option<String>,
}

#[cfg(test)]
impl Preview {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// A rendering of the current settings is on screen.
    pub fn ready(&self) -> bool {
        self.texture.is_some() && self.shown == format!("{}#{}", self.key, self.index)
    }
}

/// Formats whose output is a picture of the page, so sheet size and margins apply.
fn is_visual(f: ExportFormat) -> bool {
    f.one_page_per_file() || matches!(f, ExportFormat::Pdf | ExportFormat::Html | ExportFormat::Markdown)
}

fn refresh_preview(app: &mut PlexiApp, ctx: &Context) {
    let Some(scope) = app.export.as_ref().map(|d| d.scope) else { return };
    let pages = app.export_pages(scope);
    let Some(dlg) = app.export.as_mut() else { return };
    let mut opts = dlg.render_options();
    if !is_visual(dlg.format) {
        opts.layout = Default::default();
    }
    let doc = &app.doc;
    let page_of = |i: Option<usize>| i.and_then(|i| doc.pages.get(i)).unwrap_or(&doc.overlay);
    let key = format!("{:?}|{:?}|{}|{}", opts.layout, pages, app.revision, opts.background);
    let pv = &mut dlg.preview;
    if pv.key != key {
        pv.frames = pages.iter().flat_map(|&i| opts.frames(page_of(i)).into_iter().map(move |f| (i, f))).collect();
        pv.key = key;
    }
    pv.index = pv.index.min(pv.frames.len().saturating_sub(1));
    let Some(&(page_idx, frame)) = pv.frames.get(pv.index) else { return };
    let want = format!("{}#{}", pv.key, pv.index);

    if let Some((k, rx)) = &pv.pending {
        match rx.try_recv() {
            Ok(r) => {
                let k = k.clone();
                pv.pending = None;
                match r {
                    Ok(img) => {
                        let ci = egui::ColorImage::from_rgba_unmultiplied(
                            [img.width() as usize, img.height() as usize],
                            img.as_raw(),
                        );
                        pv.texture = Some(ctx.load_texture("export-preview", ci, egui::TextureOptions::LINEAR));
                        pv.error = None;
                    }
                    Err(e) => pv.error = Some(e),
                }
                pv.shown = k;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => pv.pending = None,
        }
    }
    let busy = pv.pending.as_ref().is_some_and(|(k, _)| *k == want);
    if pv.shown != want && !busy && pv.pending.is_none() {
        let page = page_of(page_idx).clone();
        let assets = doc.assets.clone();
        let px = PREVIEW_SIDE * ctx.pixels_per_point();
        opts.scale = (px / frame.size[0].max(frame.size[1]).max(1.0)).min(4.0);
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let r = plexi_core::export::raster::frame_rgba(&page, &assets, &opts, &frame).map_err(|e| e.to_string());
            let _ = tx.send(r);
            repaint.request_repaint();
        });
        pv.pending = Some((want, rx));
    }
}

/// A light/dark checkerboard, shown under transparent previews.
fn checkerboard(p: &Painter, r: Rect) {
    p.rect_filled(r, 0.0, Color32::from_gray(250));
    let s = 8.0;
    let (nx, ny) = ((r.width() / s).ceil() as i32, (r.height() / s).ceil() as i32);
    for y in 0..ny {
        for x in (y % 2..nx).step_by(2) {
            let cell = Rect::from_min_size(r.min + vec2(x as f32 * s, y as f32 * s), vec2(s, s)).intersect(r);
            p.rect_filled(cell, 0.0, Color32::from_gray(225));
        }
    }
}

fn fmt_in(units: f32) -> String {
    let v = units / 96.0;
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn export_window(app: &mut PlexiApp, ctx: &Context) {
    if app.export.is_none() {
        return;
    }
    refresh_preview(app, ctx);
    let fixed_page = app.doc.pages[app.page].size.dims().is_some();
    let has_pages = app.doc.pages.len() > 1;
    let th_canvas = ctx.global_style().visuals.extreme_bg_color;
    let Some(dlg) = &mut app.export else { return };
    let mut open = true;
    let mut go = false;
    egui::Window::new("Export")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // ---- preview ----
                ui.vertical(|ui| {
                    let (area, _) = ui.allocate_exact_size(vec2(PREVIEW_SIDE, PREVIEW_SIDE), Sense::hover());
                    let p = ui.painter_at(area);
                    p.rect_filled(area, 8.0, th_canvas);
                    let pv = &dlg.preview;
                    let frame = pv.frames.get(pv.index).map(|f| f.1);
                    if let (Some(tex), Some(frame)) = (&pv.texture, frame) {
                        let inner = area.shrink(14.0);
                        let aspect = frame.size[0] / frame.size[1].max(1.0);
                        let size = if aspect > inner.width() / inner.height() {
                            vec2(inner.width(), inner.width() / aspect)
                        } else {
                            vec2(inner.height() * aspect, inner.height())
                        };
                        let r = Rect::from_center_size(inner.center(), size);
                        p.rect_filled(r.translate(vec2(0.0, 2.0)).expand(1.0), 2.0, Color32::from_black_alpha(40));
                        checkerboard(&p, r);
                        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        if pv.pending.is_some() {
                            p.rect_filled(r, 0.0, Color32::from_white_alpha(60));
                        }
                    } else if let Some(e) = &pv.error {
                        p.text(area.center(), Align2::CENTER_CENTER, e, egui::FontId::proportional(13.0), Color32::RED);
                    } else {
                        ui.put(Rect::from_center_size(area.center(), vec2(24.0, 24.0)), egui::Spinner::new());
                    }
                    let n = dlg.preview.frames.len();
                    ui.horizontal(|ui| {
                        let one_file = dlg.format.one_page_per_file() || dlg.format == ExportFormat::Markdown;
                        let what = if one_file { "Image" } else { "Page" };
                        if n > 1 {
                            if icon_button(ui, Icon::Left, false, "Previous").clicked() {
                                dlg.preview.index = dlg.preview.index.saturating_sub(1);
                            }
                            ui.label(format!("{what} {} of {n}", dlg.preview.index + 1));
                            if icon_button(ui, Icon::Right, false, "Next").clicked() {
                                dlg.preview.index = (dlg.preview.index + 1).min(n - 1);
                            }
                        }
                        if let Some(f) = frame {
                            let dims = if dlg.format.uses_scale() || dlg.format == ExportFormat::Markdown {
                                let s = dlg.scale;
                                format!("{} × {} px", (f.size[0] * s).ceil(), (f.size[1] * s).ceil())
                            } else {
                                format!("{} × {} in", fmt_in(f.size[0]), fmt_in(f.size[1]))
                            };
                            ui.weak(dims);
                        }
                    });
                });
                ui.add_space(8.0);

                // ---- options ----
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    egui::Grid::new("export-grid").num_columns(2).spacing(vec2(12.0, 10.0)).show(ui, |ui| {
                        ui.label("Format");
                        egui::ComboBox::from_id_salt("export-format")
                            .selected_text(format!("{}  (.{})", dlg.format.label(), dlg.format.extension()))
                            .width(210.0)
                            .show_ui(ui, |ui| {
                                for f in ExportFormat::ALL {
                                    let l = format!("{}  (.{})", f.label(), f.extension());
                                    ui.selectable_value(&mut dlg.format, f, l);
                                }
                            });
                        ui.end_row();
                        ui.label("What");
                        ui.vertical(|ui| {
                            ui.radio_value(&mut dlg.scope, ExportScope::CurrentPage, "This page");
                            if has_pages {
                                ui.radio_value(&mut dlg.scope, ExportScope::AllPages, "All pages");
                            }
                            ui.radio_value(&mut dlg.scope, ExportScope::ScreenInk, "Screen ink (overlay)");
                        });
                        ui.end_row();

                        if is_visual(dlg.format) {
                            ui.label("Area");
                            ui.vertical(|ui| {
                                let whole = if fixed_page { "Whole page" } else { "Everything drawn" };
                                ui.radio_value(&mut dlg.area, Area::Page, whole);
                                ui.radio_value(&mut dlg.area, Area::Content, "Trim to the drawing");
                            });
                            ui.end_row();

                            ui.label("Paper");
                            let fit_label = if fixed_page { "Same as the page" } else { "Fit to the drawing" };
                            let current = match dlg.paper {
                                PaperChoice::Fit => fit_label.to_owned(),
                                PaperChoice::Preset(i) => PAPERS[i.min(PAPERS.len() - 1)].0.to_owned(),
                                PaperChoice::Custom => "Custom".to_owned(),
                            };
                            egui::ComboBox::from_id_salt("export-paper").selected_text(current).width(210.0).show_ui(
                                ui,
                                |ui| {
                                    ui.selectable_value(&mut dlg.paper, PaperChoice::Fit, fit_label);
                                    for (k, (name, d)) in PAPERS.iter().enumerate() {
                                        let l = format!("{name}  ({} × {} in)", fmt_in(d[0]), fmt_in(d[1]));
                                        if ui.selectable_value(&mut dlg.paper, PaperChoice::Preset(k), l).clicked()
                                            && name.starts_with("Slide")
                                        {
                                            dlg.landscape = true;
                                        }
                                    }
                                    ui.selectable_value(&mut dlg.paper, PaperChoice::Custom, "Custom…");
                                },
                            );
                            ui.end_row();

                            match dlg.paper {
                                PaperChoice::Fit => {}
                                PaperChoice::Custom => {
                                    ui.label("Size");
                                    ui.horizontal(|ui| {
                                        let [w, h] = &mut dlg.custom_in;
                                        ui.add(egui::DragValue::new(w).range(1.0..=100.0).speed(0.05).suffix(" in"));
                                        ui.label("×");
                                        ui.add(egui::DragValue::new(h).range(1.0..=100.0).speed(0.05).suffix(" in"));
                                    });
                                    ui.end_row();
                                }
                                PaperChoice::Preset(_) => {
                                    ui.label("Orientation");
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(&mut dlg.landscape, false, "Portrait");
                                        ui.selectable_value(&mut dlg.landscape, true, "Landscape");
                                    });
                                    ui.end_row();
                                }
                            }
                            if dlg.paper != PaperChoice::Fit {
                                ui.label("Fit");
                                ui.vertical(|ui| {
                                    ui.radio_value(&mut dlg.tile, false, "Shrink onto one sheet");
                                    ui.radio_value(&mut dlg.tile, true, "Split across sheets, full size")
                                        .on_hover_text("One sheet per outline on the canvas; empty sheets are skipped");
                                });
                                ui.end_row();
                            }
                            ui.label("Margin");
                            ui.add_enabled(
                                !(dlg.tile && dlg.paper != PaperChoice::Fit),
                                egui::Slider::new(&mut dlg.margin_in, 0.0..=2.0).step_by(0.05).suffix(" in"),
                            );
                            ui.end_row();
                        }

                        if dlg.format.uses_scale() || dlg.format == ExportFormat::Markdown {
                            ui.label("Resolution");
                            ui.horizontal(|ui| {
                                for s in [1.0, 2.0, 3.0, 4.0] {
                                    ui.selectable_value(&mut dlg.scale, s, format!("{s}×"));
                                }
                            });
                            ui.end_row();
                            if let Some(f) = dlg.preview.frames.get(dlg.preview.index).map(|f| f.1) {
                                ui.label("Width");
                                let mut px = (f.size[0] * dlg.scale).round();
                                let r = ui
                                    .add(egui::DragValue::new(&mut px).range(16.0..=16384.0).speed(4.0).suffix(" px"));
                                if r.changed() {
                                    dlg.scale = (px / f.size[0].max(1.0)).clamp(0.05, 16.0);
                                }
                                ui.end_row();
                            }
                        }
                        ui.label("Background");
                        ui.checkbox(&mut dlg.background, "Paper and lines");
                        ui.end_row();
                    });
                    if !is_visual(dlg.format) {
                        ui.weak("This format keeps the drawing itself, so paper and size don't apply.");
                    } else if dlg.format.one_page_per_file() && dlg.preview.frames.len() > 1 {
                        ui.weak("One file per image: name-1, name-2, …");
                    }
                    ui.add_space(6.0);
                    let label = egui::RichText::new("Export…").color(Color32::WHITE);
                    if ui.add(egui::Button::new(label).fill(ACCENT).min_size(vec2(96.0, 30.0))).clicked() {
                        go = true;
                    }
                });
            });
        });
    if !open {
        app.export = None;
    } else if go {
        app.run_export();
    }
}

// ----- right-click menu -----------------------------------------------------------

fn context_menu(app: &mut PlexiApp, ctx: &Context) {
    let Some(menu) = app.context_menu.clone() else { return };
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        app.context_menu = None;
        return;
    }
    let t = menu.target;
    let hit = menu.hit.and_then(|id| app.page_ref(t).elements.iter().find(|e| e.id() == id).cloned());
    let locked_ids: std::collections::HashSet<u64> =
        app.page_ref(t).elements.iter().filter(|e| e.locked()).map(Element::id).collect();
    let mut close = false;
    let resp = egui::Area::new(Id::unique("context-menu"))
        .fixed_pos(menu.pos)
        .order(Order::Foreground)
        .constrain(true)
        .show(ctx, |ui| {
            pill(ui).inner_margin(6).show(ui, |ui| {
                ui.set_min_width(200.0);
                ui.spacing_mut().button_padding = vec2(10.0, 4.0);
                let item = |ui: &mut Ui, text: &str| {
                    ui.add(egui::Button::new(text).frame(false).min_size(vec2(190.0, 24.0))).clicked()
                };
                if let Some(Element::Image(img)) = &hit {
                    let ids = [img.id].into();
                    if img.locked {
                        if item(ui, "Unlock image") {
                            app.set_locked(t, &ids, false);
                            app.selection = ids;
                            app.tool = Tool::Lasso;
                            close = true;
                        }
                    } else {
                        if item(ui, "Lock image") {
                            app.set_locked(t, &ids, true);
                            close = true;
                        }
                        if item(ui, "Reset to original size") {
                            app.reset_image_size(t, img.id, ctx.pixels_per_point());
                            close = true;
                        }
                    }
                    ui.separator();
                }
                if !app.selection.is_empty() {
                    if item(ui, "Bring to front") {
                        app.restack_selection(true);
                        close = true;
                    }
                    if item(ui, "Send to back") {
                        app.restack_selection(false);
                        close = true;
                    }
                    if item(ui, "Duplicate") {
                        app.duplicate_selection();
                        close = true;
                    }
                    if item(ui, "Delete") {
                        app.delete_selection();
                        close = true;
                    }
                    ui.separator();
                }
                if !locked_ids.is_empty() && item(ui, &format!("Unlock all images ({})", locked_ids.len())) {
                    app.set_locked(t, &locked_ids, false);
                    close = true;
                }
                if item(ui, "Select all") {
                    app.select_all();
                    close = true;
                }
                match t {
                    crate::history::Target::Page(i) => {
                        ui.separator();
                        ui.weak("Paper");
                        paper_colors(app, ui, i);
                        ui.horizontal(|ui| {
                            for bg in Background::ALL {
                                if ui.selectable_label(app.doc.pages[i].background == bg, bg.label()).clicked() {
                                    app.checkpoint_pages();
                                    app.doc.pages[i].background = bg;
                                    app.config.new_page_background = bg;
                                }
                            }
                        });
                        ui.separator();
                        let label = if app.doc.pages.len() > 1 { "Delete this page" } else { "Clear this page" };
                        if item(ui, label) {
                            app.delete_page_at(i);
                            close = true;
                        }
                    }
                    crate::history::Target::Overlay => {
                        ui.separator();
                        if item(ui, "Clear screen ink") {
                            app.clear_overlay();
                            close = true;
                        }
                    }
                }
            });
        });
    let r = resp.response.rect;
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !r.contains(p)));
    // Pickers inside the menu open their own popups; don't treat those clicks as outside.
    let popup_open = egui::Popup::is_any_open(ctx);
    if close || (outside && !popup_open) {
        app.context_menu = None;
    }
}

fn settings_window(app: &mut PlexiApp, ctx: &Context) {
    let mut open = app.settings_open;
    egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        let c = &mut app.config;
        ui.heading("Ink");
        ui.add(egui::Slider::new(&mut c.smoothing, 0.0..=1.0).text("Pen smoothing"));
        ui.weak("0 is raw input. Smoothing only steadies slow movement; fast writing stays sharp.");
        ui.add(egui::Slider::new(&mut c.pen_width, 0.5..=40.0).logarithmic(true).text("Pen width"));
        ui.add(egui::Slider::new(&mut c.highlighter_width, 4.0..=80.0).logarithmic(true).text("Highlighter width"));
        ui.add(egui::Slider::new(&mut c.eraser_size, 4.0..=120.0).logarithmic(true).text("Eraser size"));
        ui.checkbox(&mut c.eraser_partial, "Eraser removes only the part it touches");
        ui.separator();
        ui.heading("Draw on screen");
        ui.checkbox(&mut c.keep_overlay_ink, "Keep screen ink when going back to the notebook");
        egui::Grid::new("hotkeys").num_columns(2).show(ui, |ui| {
            ui.label("Draw / click-through");
            ui.text_edit_singleline(&mut c.hotkey_draw);
            ui.end_row();
            ui.label("Clear screen ink");
            ui.text_edit_singleline(&mut c.hotkey_clear);
            ui.end_row();
            ui.label("Back to notebook");
            ui.text_edit_singleline(&mut c.hotkey_notebook);
            ui.end_row();
        });
        ui.weak("Hotkey changes apply the next time Plexi starts.");
        ui.separator();
        ui.heading("Files");
        ui.checkbox(&mut c.autosave, "Save automatically");
        let dir = crate::config::data_dir();
        ui.horizontal(|ui| {
            ui.label(if crate::config::is_portable() {
                "Portable mode — data beside the exe"
            } else {
                "Data folder"
            });
            if ui.button("Open").clicked() {
                let _ =
                    std::process::Command::new(if cfg!(windows) { "explorer" } else { "xdg-open" }).arg(&dir).spawn();
            }
        });
        ui.weak(format!("Plexi {}", env!("CARGO_PKG_VERSION")));
    });
    if !open && app.settings_open {
        app.config.save();
    }
    app.settings_open = open;
}

fn help_window(app: &mut PlexiApp, ctx: &Context) {
    let mut open = app.help_open;
    egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        let rows: &[(&str, String)] = &[
            ("Draw on screen / click-through", app.config.hotkey_draw.clone()),
            ("Clear screen ink", app.config.hotkey_clear.clone()),
            ("Back to notebook", app.config.hotkey_notebook.clone()),
            ("Command palette", "Ctrl+K".into()),
            ("Pen · Highlighter · Eraser", "P · H · E".into()),
            ("Select · Text", "S · T".into()),
            ("Line · Arrow · Rect · Ellipse", "L · A · R · O".into()),
            ("Colors", "1 – 9".into()),
            ("Thinner / thicker", "[ · ]".into()),
            ("Erase with any tool", "Right-drag (or pen barrel button)".into()),
            ("Menu: lock image, arrange, paper, delete page", "Right-click".into()),
            ("Resize an image", "Select it, drag a corner (Shift: free)".into()),
            ("Pan", "Scroll · Space+drag · middle-drag · two fingers".into()),
            ("Zoom", "Ctrl+scroll · pinch · Ctrl+= / Ctrl+-".into()),
            ("Fit page", "Ctrl+0".into()),
            ("Undo · Redo", "Ctrl+Z · Ctrl+Y".into()),
            ("New page · Next · Previous", "Ctrl+Enter · PgDn · PgUp".into()),
            ("Select all · Duplicate · Delete", "Ctrl+A · Ctrl+D · Del".into()),
            ("Straight lines, squares", "hold Shift".into()),
            ("Paste image or text", "Ctrl+V".into()),
            ("Save · Open · Export", "Ctrl+S · Ctrl+O · Ctrl+E".into()),
            ("On screen: click-through · clear · snapshot", "Esc · C · Ctrl+Shift+C".into()),
        ];
        egui::Grid::new("keys").num_columns(2).spacing(vec2(24.0, 6.0)).striped(true).show(ui, |ui| {
            for (what, keys) in rows {
                ui.label(*what);
                ui.label(egui::RichText::new(keys).monospace());
                ui.end_row();
            }
        });
    });
    app.help_open = open;
}

fn toast(app: &mut PlexiApp, ctx: &Context) {
    let Some(t) = &app.toast else { return };
    if std::time::Instant::now() > t.until {
        app.toast = None;
        return;
    }
    let (text, error) = (t.text.clone(), t.error);
    let y = if matches!(app.mode, Mode::Notebook) { -62.0 } else { -24.0 };
    egui::Area::new(Id::unique("toast"))
        .anchor(Align2::CENTER_BOTTOM, vec2(0.0, y))
        .order(Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            pill(ui).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                let c = if error { Color32::from_rgb(0xd6, 0x3a, 0x3a) } else { theme(ui).ink };
                ui.label(egui::RichText::new(text).color(c));
            });
        });
}

pub fn dialogs(app: &mut PlexiApp, ctx: &Context) {
    context_menu(app, ctx);
    palette(app, ctx);
    export_window(app, ctx);
    settings_window(app, ctx);
    help_window(app, ctx);
    toast(app, ctx);
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_matches_subsequences() {
        assert!(fuzzy("Draw on screen", "drw scr"));
        assert!(fuzzy("Export…", "exp"));
        assert!(!fuzzy("Undo", "redo"));
    }
}
