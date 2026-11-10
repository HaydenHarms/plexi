//! Writes a sample notebook (`cargo run -p plexi-core --example demo -- out.plexi`).
//! Handy for screenshots and for trying exports without drawing anything.

use plexi_core::*;

fn wave(x0: f32, y0: f32, len: f32, amp: f32, turns: f32, color: Color, width: f32) -> Element {
    let n = 120;
    let points = (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            let p = 0.25 + 0.6 * (t * std::f32::consts::PI).sin();
            InkPoint::new(x0 + t * len, y0 + (t * turns * std::f32::consts::TAU).sin() * amp, p)
        })
        .collect();
    Element::Stroke(Stroke { id: new_id(), kind: InkKind::Pen, color, width, points, group: 0, locked: false })
}

fn loops(x0: f32, y0: f32, count: usize, color: Color) -> Element {
    // Cursive-looking loops: a prolate cycloid.
    let n = count * 40;
    let points = (0..n)
        .map(|i| {
            let t = i as f32 / 40.0 * std::f32::consts::TAU;
            InkPoint::new(x0 + t * 5.0 - 12.0 * t.sin(), y0 - 14.0 * t.cos(), 0.35 + 0.3 * (t * 0.5).sin().abs())
        })
        .collect();
    Element::Stroke(Stroke { id: new_id(), kind: InkKind::Pen, color, width: 2.6, points, group: 0, locked: false })
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "demo.plexi".into());
    let mut doc = Document::new("Demo");
    let ink = Color::rgb(0x1d, 0x1f, 0x24);
    let blue = Color::rgb(0x1f, 0x5f, 0xd6);
    let red = Color::rgb(0xd6, 0x28, 0x28);
    let page = &mut doc.pages[0];
    page.name = "Lecture notes".into();
    page.background = Background::Lined;
    page.elements.push(Element::Text(TextBox::new([72.0, 56.0], "Week 3 — Revenue recognition", 30.0, ink)));
    page.elements.push(Element::Stroke(Stroke {
        id: new_id(),
        kind: InkKind::Highlighter,
        color: Color::rgba(0xff, 0xe0, 0x2e, 110),
        width: 22.0,
        points: vec![InkPoint::new(76.0, 166.0, 0.5), InkPoint::new(420.0, 166.0, 0.5)],
        group: 0,
        locked: false,
    }));
    for (i, y) in [160.0, 224.0, 288.0, 352.0].into_iter().enumerate() {
        page.elements.push(loops(84.0, y, 10 + i * 3, if i == 1 { blue } else { ink }));
    }
    page.elements.push(Element::Text(TextBox::new(
        [480.0, 140.0],
        "1. Identify the contract\n2. Performance obligations\n3. Transaction price\n4. Allocate\n5. Recognize",
        18.0,
        blue,
    )));
    page.elements.push(Element::Shape(Shape {
        id: new_id(),
        kind: ShapeKind::Ellipse,
        a: [455.0, 112.0],
        b: [800.0, 290.0],
        color: red,
        width: 2.5,
        group: 0,
        rotation: 0.0,
        locked: false,
    }));
    page.elements.push(Element::Shape(Shape {
        id: new_id(),
        kind: ShapeKind::Arrow,
        a: [430.0, 420.0],
        b: [520.0, 290.0],
        color: red,
        width: 3.0,
        group: 0,
        rotation: 0.0,
        locked: false,
    }));
    page.elements.push(wave(80.0, 470.0, 640.0, 24.0, 3.0, ink, 4.0));
    page.elements.push(Element::Shape(Shape {
        id: new_id(),
        kind: ShapeKind::Rect,
        a: [72.0, 540.0],
        b: [744.0, 700.0],
        color: blue,
        width: 2.0,
        group: 0,
        rotation: 0.0,
        locked: false,
    }));
    page.elements.push(Element::Text(TextBox::new(
        [92.0, 560.0],
        "Exam tip: step 5 happens when control transfers,\nnot when cash is received.",
        20.0,
        ink,
    )));

    // The page is an infinite canvas: this spills onto the sheet to the right.
    page.elements.push(Element::Text(TextBox::new([880.0, 140.0], "Side note, one sheet over", 22.0, blue)));
    page.elements.push(wave(880.0, 220.0, 520.0, 18.0, 3.0, blue, 3.0));

    let mut grid = Page::new("Sketches", PageSize::Letter);
    grid.background = Background::Grid;
    grid.paper = Color::rgb(0x2a, 0x2f, 0x38);
    let ink = Color::rgb(0xf2, 0xf2, 0xf2);
    let blue = Color::rgb(0x6c, 0xb4, 0xff);
    for k in 0..6 {
        grid.elements.push(wave(
            80.0,
            120.0 + k as f32 * 70.0,
            600.0,
            10.0 + k as f32 * 4.0,
            1.5 + k as f32,
            if k % 2 == 0 { ink } else { blue },
            2.0 + k as f32,
        ));
    }
    doc.pages.push(grid);

    doc.overlay.elements.push(Element::Shape(Shape {
        id: new_id(),
        kind: ShapeKind::Arrow,
        a: [300.0, 520.0],
        b: [560.0, 330.0],
        color: red,
        width: 5.0,
        group: 0,
        rotation: 0.0,
        locked: false,
    }));
    doc.overlay.elements.push(loops(240.0, 600.0, 12, red));
    doc.overlay.elements.push(Element::Stroke(Stroke {
        id: new_id(),
        kind: InkKind::Highlighter,
        color: Color::rgba(0xff, 0xe0, 0x2e, 120),
        width: 26.0,
        points: vec![InkPoint::new(600.0, 300.0, 0.5), InkPoint::new(1000.0, 300.0, 0.5)],
        group: 0,
        locked: false,
    }));

    format::save(&doc, std::path::Path::new(&out)).expect("save demo");
    println!("wrote {out}");
}
