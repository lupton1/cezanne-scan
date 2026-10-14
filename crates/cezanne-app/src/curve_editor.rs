use cezanne_core::image::Curve;
use eframe::egui::{self, Color32, Pos2, Sense, Stroke, Vec2};

/// Square curve editor. Returns true when the curve changed.
pub fn curve_editor(ui: &mut egui::Ui, curve: &mut Curve, size: f32) -> bool {
    let (resp, painter) = ui.allocate_painter(Vec2::splat(size), Sense::click_and_drag());
    let r = resp.rect;
    let to_screen = |p: [f32; 2]| Pos2::new(r.min.x + p[0] * r.width(), r.max.y - p[1] * r.height());
    let to_curve = |p: Pos2| [((p.x - r.min.x) / r.width()).clamp(0.0, 1.0), ((r.max.y - p.y) / r.height()).clamp(0.0, 1.0)];

    painter.rect_filled(r, 2.0, Color32::from_gray(24));
    let grid = Stroke::new(1.0_f32, Color32::from_gray(50));
    for k in 1..4 {
        let t = k as f32 / 4.0;
        painter.line_segment([Pos2::new(r.min.x + t * r.width(), r.min.y), Pos2::new(r.min.x + t * r.width(), r.max.y)], grid);
        painter.line_segment([Pos2::new(r.min.x, r.min.y + t * r.height()), Pos2::new(r.max.x, r.min.y + t * r.height())], grid);
    }
    painter.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0_f32, Color32::from_gray(70)));

    let mut changed = false;
    let id = resp.id.with("drag_point");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(id));

    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            dragging = curve
                .points
                .iter()
                .enumerate()
                .map(|(i, q)| (i, to_screen(*q).distance(p)))
                .filter(|(_, d)| *d < 10.0)
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(i, _)| i);
        }
    }
    if let (Some(i), Some(p)) = (dragging, resp.interact_pointer_pos()) {
        if resp.dragged() && i < curve.points.len() {
            let mut np = to_curve(p);
            let last = curve.points.len() - 1;
            // keep the ends on the left and right edges and the points in order
            if i == 0 {
                np[0] = 0.0;
            } else if i == last {
                np[0] = 1.0;
            } else {
                let lo = curve.points[i - 1][0] + 0.01;
                let hi = curve.points[i + 1][0] - 0.01;
                np[0] = np[0].clamp(lo, hi.max(lo));
            }
            curve.points[i] = np;
            changed = true;
        }
    }
    if resp.drag_stopped() {
        dragging = None;
    }
    ui.data_mut(|d| match dragging {
        Some(i) => d.insert_temp(id, i),
        None => d.remove::<usize>(id),
    });

    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let np = to_curve(p);
            if np[0] > 0.01 && np[0] < 0.99 {
                let at = curve.points.iter().position(|q| q[0] > np[0]).unwrap_or(curve.points.len());
                curve.points.insert(at, [np[0], curve.eval(np[0] as f64) as f32]);
                changed = true;
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let last = curve.points.len() - 1;
            if let Some(i) = (1..last).find(|&i| to_screen(curve.points[i]).distance(p) < 10.0) {
                curve.points.remove(i);
                changed = true;
            }
        }
    }

    let xs: Vec<f64> = (0..=96).map(|i| i as f64 / 96.0).collect();
    let ys = curve.eval_many(&xs);
    let pts: Vec<Pos2> = xs.iter().zip(&ys).map(|(&x, &y)| to_screen([x as f32, y as f32])).collect();
    painter.add(egui::Shape::line(pts, Stroke::new(2.0_f32, Color32::from_rgb(230, 230, 230))));
    for q in &curve.points {
        painter.circle_filled(to_screen(*q), 4.5, Color32::from_rgb(80, 200, 255));
    }
    changed
}
