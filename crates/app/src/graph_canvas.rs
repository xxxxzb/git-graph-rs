//! 提交图谱「图形」列绘制：基于 GPUI `canvas` + `PathBuilder` 精确渲染泳道曲线与节点。

use git_graph_layout::{LaneSegment, NodeKind, RowLayout, SegmentKind};
use gpui::{
    AnyElement, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled, canvas, fill, point,
    px, rgb, size,
};

/// 10 色浅色主题分支泳道调色板（高辨识度且温润克制）。
const LIGHT_PALETTE: [u32; 10] = [
    0x2563EB, // 钴蓝
    0x0D9488, // 青碧
    0xD97706, // 琥珀
    0xDB2777, // 玫红
    0x7C3AED, // 紫罗兰
    0x16A34A, // 翠绿
    0xEA580C, // 赭橙
    0x0284C7, // 天蓝
    0x9333EA, // 靛紫
    0x4F46E5, // 靛蓝
];

/// 10 色深色主题分支泳道调色板（提升亮度保证暗底对比度）。
const DARK_PALETTE: [u32; 10] = [
    0x60A5FA, // 亮蓝
    0x2DD4BF, // 亮青
    0xFBBF24, // 明黄
    0xF472B6, // 粉红
    0xA78BFA, // 淡紫
    0x4ADE80, // 亮绿
    0xFB923C, // 亮橙
    0x38BDF8, // 浅天蓝
    0xC084FC, // 薰衣草
    0x818CF8, // 浅靛
];

/// 根据明暗主题返回泳道调色板颜色。
pub fn lane_color(color_index: usize, dark: bool) -> Hsla {
    let hex = if dark {
        DARK_PALETTE[color_index % DARK_PALETTE.len()]
    } else {
        LIGHT_PALETTE[color_index % LIGHT_PALETTE.len()]
    };
    rgb(hex).into()
}

/// 绘制单行图谱单元的尺寸与配色参数。
pub struct GraphCellStyle {
    pub col_width: Pixels,
    pub row_height: Pixels,
    pub lane_width: Pixels,
    pub scale: f32,
    pub scroll_x: f32,
    /// 左右触边橡皮筋弹性偏移（px，正值表示左触边向右过冲，负值表示右触边向左过冲）。
    pub overscroll_x: f32,
    pub has_overflow_left: bool,
    pub has_overflow_right: bool,
    pub dark: bool,
    pub row_bg: Hsla,
    pub muted_color: Hsla,
    pub stash_color: Hsla,
    pub border_color: Hsla,
    pub overflow_hint_color: Hsla,
    pub overscroll_glow_color: Hsla,
}

/// 渲染单行的图形列单元格。
pub fn paint_graph_cell(row: RowLayout, style: GraphCellStyle) -> AnyElement {
    let GraphCellStyle {
        col_width,
        row_height,
        lane_width,
        scale,
        scroll_x,
        overscroll_x,
        has_overflow_left,
        has_overflow_right,
        dark,
        row_bg,
        muted_color,
        stash_color,
        border_color,
        overflow_hint_color,
        overscroll_glow_color,
    } = style;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                let left_pad = px(10.0 * scale) - px(scroll_x.max(0.0)) + px(overscroll_x);
                let lane_x = |lane: usize| -> Pixels {
                    bounds.origin.x + left_pad + lane_width * (lane as f32 + 0.5)
                };
                let top = bounds.origin.y;
                let mid = bounds.origin.y + bounds.size.height * 0.5;
                let bottom = bounds.origin.y + bounds.size.height;
                let stroke_width = px(1.8 * scale);

                // 1. 先绘制所有泳道连接线
                for seg in &row.segments {
                    let color = if seg.dashed {
                        muted_color
                    } else {
                        lane_color(seg.color_index, dark)
                    };
                    paint_segment(
                        window,
                        seg,
                        lane_x,
                        top,
                        mid,
                        bottom,
                        stroke_width,
                        color,
                    );
                }

                // 2. 再绘制提交圆点（覆盖在连接线之上）
                let cx_pt = point(lane_x(row.lane), mid);
                let node_color = lane_color(row.color_index, dark);
                paint_node(
                    window,
                    cx_pt,
                    row.node_kind,
                    row.is_head,
                    row.is_merge,
                    node_color,
                    row_bg,
                    muted_color,
                    stash_color,
                    scale,
                );

                // 3. 左右存在可滚动泳道时，在列边缘绘制极淡的溢出提示
                let hint_w = px(2.0 * scale);
                if has_overflow_left {
                    window.paint_quad(fill(
                        Bounds::new(bounds.origin, size(hint_w, bounds.size.height)),
                        overflow_hint_color,
                    ));
                }
                if has_overflow_right {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.origin.x + bounds.size.width - hint_w, bounds.origin.y),
                            size(hint_w, bounds.size.height),
                        ),
                        overflow_hint_color,
                    ));
                }

                // 4. 左右触边橡皮筋回弹时，在对应边缘绘制柔和动态呼吸光晕（严格裁剪在 ContentMask 内）
                if overscroll_x.abs() > 0.25 {
                    let ratio = (overscroll_x.abs() / 28.0).clamp(0.0, 1.0);
                    let glow_w = px((6.0 + 14.0 * ratio) * scale);
                    let bands = 4usize;
                    let band_w = glow_w / bands as f32;
                    for i in 0..bands {
                        let t = i as f32 / bands as f32;
                        let alpha = 0.24 * ratio * (1.0 - t) * (1.0 - t);
                        let x = if overscroll_x > 0.0 {
                            bounds.origin.x + band_w * i as f32
                        } else {
                            bounds.origin.x + bounds.size.width - band_w * (i + 1) as f32
                        };
                        window.paint_quad(fill(
                            Bounds::new(point(x, bounds.origin.y), size(band_w, bounds.size.height)),
                            overscroll_glow_color.opacity(alpha),
                        ));
                    }
                    let edge_w = px(1.5 * scale);
                    let edge_x = if overscroll_x > 0.0 {
                        bounds.origin.x
                    } else {
                        bounds.origin.x + bounds.size.width - edge_w
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(edge_x, bounds.origin.y),
                            size(edge_w, bounds.size.height),
                        ),
                        overscroll_glow_color.opacity(0.42 * ratio),
                    ));
                }
            });
        },
    )
    .flex_none()
    .w(col_width)
    .h(row_height)
    .border_r_1()
    .border_color(border_color)
    .overflow_hidden()
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn paint_segment(
    window: &mut gpui::Window,
    seg: &LaneSegment,
    lane_x: impl Fn(usize) -> Pixels,
    top: Pixels,
    mid: Pixels,
    bottom: Pixels,
    stroke_width: Pixels,
    color: Hsla,
) {
    let x1 = lane_x(seg.from_lane);
    let x2 = lane_x(seg.to_lane);

    match seg.kind {
        SegmentKind::PassThrough => {
            let from = point(x1, top);
            let to = point(x1, bottom);
            if seg.dashed {
                paint_dashed_line(window, from, to, stroke_width, color);
            } else {
                paint_line(window, from, to, stroke_width, color);
            }
        }
        SegmentKind::TopToNode => {
            let from = point(x1, top);
            let to = point(x2, mid);
            if seg.from_lane == seg.to_lane {
                if seg.dashed {
                    paint_dashed_line(window, from, to, stroke_width, color);
                } else {
                    paint_line(window, from, to, stroke_width, color);
                }
            } else {
                let dy = mid - top;
                let ctrl1 = point(x1, top + dy * 0.55);
                let ctrl2 = point(x2, top + dy * 0.45);
                if seg.dashed {
                    paint_dashed_cubic(window, from, ctrl1, ctrl2, to, stroke_width, color);
                } else {
                    paint_cubic(window, from, ctrl1, ctrl2, to, stroke_width, color);
                }
            }
        }
        SegmentKind::NodeToBottom => {
            let from = point(x1, mid);
            let to = point(x2, bottom);
            if seg.from_lane == seg.to_lane {
                if seg.dashed {
                    paint_dashed_line(window, from, to, stroke_width, color);
                } else {
                    paint_line(window, from, to, stroke_width, color);
                }
            } else {
                let dy = bottom - mid;
                let ctrl1 = point(x1, mid + dy * 0.55);
                let ctrl2 = point(x2, mid + dy * 0.45);
                if seg.dashed {
                    paint_dashed_cubic(window, from, ctrl1, ctrl2, to, stroke_width, color);
                } else {
                    paint_cubic(window, from, ctrl1, ctrl2, to, stroke_width, color);
                }
            }
        }
    }
}

fn paint_line(
    window: &mut gpui::Window,
    from: Point<Pixels>,
    to: Point<Pixels>,
    width: Pixels,
    color: Hsla,
) {
    let mut path = PathBuilder::stroke(width);
    path.move_to(from);
    path.line_to(to);
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

fn paint_cubic(
    window: &mut gpui::Window,
    from: Point<Pixels>,
    ctrl1: Point<Pixels>,
    ctrl2: Point<Pixels>,
    to: Point<Pixels>,
    width: Pixels,
    color: Hsla,
) {
    let mut path = PathBuilder::stroke(width);
    path.move_to(from);
    path.cubic_bezier_to(to, ctrl1, ctrl2);
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

fn paint_dashed_line(
    window: &mut gpui::Window,
    from: Point<Pixels>,
    to: Point<Pixels>,
    width: Pixels,
    color: Hsla,
) {
    let steps = 5usize;
    let mut path = PathBuilder::stroke(width);
    for i in 0..steps {
        if i % 2 == 1 {
            continue;
        }
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let p0 = point(
            from.x + (to.x - from.x) * t0,
            from.y + (to.y - from.y) * t0,
        );
        let p1 = point(
            from.x + (to.x - from.x) * t1,
            from.y + (to.y - from.y) * t1,
        );
        path.move_to(p0);
        path.line_to(p1);
    }
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

fn paint_dashed_cubic(
    window: &mut gpui::Window,
    p0: Point<Pixels>,
    p1: Point<Pixels>,
    p2: Point<Pixels>,
    p3: Point<Pixels>,
    width: Pixels,
    color: Hsla,
) {
    let eval = |t: f32| -> Point<Pixels> {
        let u = 1.0 - t;
        let tt = t * t;
        let uu = u * u;
        let uuu = uu * u;
        let ttt = tt * t;
        let x = p0.x * uuu + p1.x * (3.0 * uu * t) + p2.x * (3.0 * u * tt) + p3.x * ttt;
        let y = p0.y * uuu + p1.y * (3.0 * uu * t) + p2.y * (3.0 * u * tt) + p3.y * ttt;
        point(x, y)
    };

    let segments = 9usize;
    let mut path = PathBuilder::stroke(width);
    for i in 0..segments {
        if i % 2 == 1 {
            continue;
        }
        let a = eval(i as f32 / segments as f32);
        let b = eval((i + 1) as f32 / segments as f32);
        path.move_to(a);
        path.line_to(b);
    }
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    window: &mut gpui::Window,
    center: Point<Pixels>,
    kind: NodeKind,
    is_head: bool,
    is_merge: bool,
    lane_color: Hsla,
    row_bg: Hsla,
    muted_color: Hsla,
    stash_color: Hsla,
    scale: f32,
) {
    let paint_circle = |window: &mut gpui::Window, diameter: Pixels, color: Hsla| {
        let r = diameter * 0.5;
        window.paint_quad(fill(Bounds::new(center - point(r, r), size(diameter, diameter)), color).corner_radii(r));
    };

    match kind {
        NodeKind::Uncommitted => {
            paint_circle(window, px(9.5 * scale), muted_color);
            paint_circle(window, px(5.5 * scale), row_bg);
        }
        NodeKind::Stash => {
            paint_circle(window, px(10.5 * scale), stash_color);
            paint_circle(window, px(5.0 * scale), row_bg);
            paint_circle(window, px(2.5 * scale), stash_color);
        }
        NodeKind::Head => {
            paint_circle(window, px(15.0 * scale), lane_color.opacity(0.24));
            paint_circle(window, px(10.0 * scale), lane_color);
            if is_merge {
                paint_circle(window, px(5.2 * scale), row_bg);
                paint_circle(window, px(2.4 * scale), lane_color);
            } else {
                paint_circle(window, px(3.4 * scale), row_bg);
            }
        }
        NodeKind::Merge => {
            paint_circle(window, px(9.6 * scale), lane_color);
            paint_circle(window, px(5.2 * scale), row_bg);
            if is_head {
                paint_circle(window, px(2.6 * scale), lane_color);
            }
        }
        NodeKind::Normal => {
            if is_head {
                paint_circle(window, px(14.5 * scale), lane_color.opacity(0.24));
            }
            paint_circle(window, px(8.4 * scale), lane_color);
            if is_merge {
                paint_circle(window, px(4.2 * scale), row_bg);
            }
        }
    }
}
