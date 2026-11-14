use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// A nonempty rectangle in logical desk coordinates, with exclusive right and bottom boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRect {
    origin: Point,
    width: NonZeroU32,
    height: NonZeroU32,
}

impl DisplayRect {
    pub fn new(origin: Point, width: NonZeroU32, height: NonZeroU32) -> Option<Self> {
        let rect = Self {
            origin,
            width,
            height,
        };
        i32::try_from(rect.right() - 1).ok()?;
        i32::try_from(rect.bottom() - 1).ok()?;
        Some(rect)
    }

    pub fn contains(self, point: Point) -> bool {
        (i64::from(self.origin.x)..self.right()).contains(&i64::from(point.x))
            && (i64::from(self.origin.y)..self.bottom()).contains(&i64::from(point.y))
    }

    /// Requires a shared segment of positive length. Corner contact alone is insufficient.
    pub fn adjacent_at(self, side: Edge, other: Self) -> bool {
        let (source, target) = (self.bounds(side), other.bounds(side.opposite()));
        source.boundary == target.boundary
            && source.start.max(target.start) < source.end.min(target.end)
    }

    /// Maps an exit pixel across a shared edge to the corresponding entry pixel.
    pub fn entry_at(self, side: Edge, exit: Point, other: Self) -> Option<Point> {
        if !self.contains(exit) || !self.adjacent_at(side, other) {
            return None;
        }
        let source = self.bounds(side);
        let target = other.bounds(side.opposite());
        let (normal, along) = side.axes(exit);
        if normal != source.pixel || !(target.start..target.end).contains(&along) {
            return None;
        }
        Some(side.point(target.pixel, along))
    }

    /// Maps the full exit edge proportionally to the opposite edge of another display.
    /// This is for explicit target selections. Seamless handoffs use `entry_at`.
    pub fn map_edge(self, side: Edge, exit: Point, other: Self) -> Option<Point> {
        if !self.contains(exit) {
            return None;
        }
        let source = self.bounds(side);
        let target = other.bounds(side.opposite());
        let (normal, along) = side.axes(exit);
        if normal != source.pixel {
            return None;
        }
        let source_span = (source.end - source.start - 1) as u64;
        let target_span = (target.end - target.start - 1) as u64;
        // Both spans are less than u32::MAX, so the product and rounding bias fit in u64.
        let scaled = (along - source.start) as u64 * target_span;
        let offset = (scaled + source_span / 2)
            .checked_div(source_span)
            .unwrap_or(target_span / 2);
        Some(side.point(target.pixel, target.start + offset as i64))
    }

    /// Normalized height of an outward motion that reaches a horizontal exit.
    pub fn horizontal_crossing(self, side: Edge, point: Point, dx: i32, dy: i32) -> Option<u16> {
        if !self.contains(point) {
            return None;
        }
        let edge = self.bounds(side);
        let x = i64::from(point.x) + i64::from(dx);
        match side {
            Edge::Left if dx < 0 && x <= edge.pixel => {}
            Edge::Right if dx > 0 && x >= edge.pixel => {}
            _ => return None,
        }
        let y = (i64::from(point.y) + i64::from(dy)).clamp(edge.start, edge.end - 1);
        let span = edge.end - edge.start - 1;
        Some(
            ((y - edge.start) * i64::from(u16::MAX) + span / 2)
                .checked_div(span)
                .unwrap_or(32768) as u16,
        )
    }

    pub fn horizontal_entry(self, side: Edge, height: u16, inset: u32) -> Point {
        let height_span = i64::from(self.height.get() - 1);
        let y = i64::from(self.origin.y)
            + (height_span * i64::from(height) + i64::from(u16::MAX) / 2) / i64::from(u16::MAX);
        let x = match side {
            Edge::Left => {
                i64::from(self.origin.x) + i64::from(inset.min((self.width.get() - 1) / 2))
            }
            Edge::Right => self.right() - 1 - i64::from(inset.min((self.width.get() - 1) / 2)),
            _ => unreachable!("horizontal entry requires a left or right edge"),
        };
        Point {
            x: x as i32,
            y: y as i32,
        }
    }

    fn right(self) -> i64 {
        i64::from(self.origin.x) + i64::from(self.width.get())
    }

    fn bottom(self) -> i64 {
        i64::from(self.origin.y) + i64::from(self.height.get())
    }

    fn bounds(self, side: Edge) -> EdgeBounds {
        let (left, top) = (i64::from(self.origin.x), i64::from(self.origin.y));
        let (right, bottom) = (self.right(), self.bottom());
        match side {
            Edge::Left => EdgeBounds {
                boundary: left,
                pixel: left,
                start: top,
                end: bottom,
            },
            Edge::Right => EdgeBounds {
                boundary: right,
                pixel: right - 1,
                start: top,
                end: bottom,
            },
            Edge::Top => EdgeBounds {
                boundary: top,
                pixel: top,
                start: left,
                end: right,
            },
            Edge::Bottom => EdgeBounds {
                boundary: bottom,
                pixel: bottom - 1,
                start: left,
                end: right,
            },
        }
    }
}

struct EdgeBounds {
    boundary: i64,
    pixel: i64,
    start: i64,
    end: i64,
}

impl Edge {
    fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    fn axes(self, point: Point) -> (i64, i64) {
        match self {
            Self::Left | Self::Right => (i64::from(point.x), i64::from(point.y)),
            Self::Top | Self::Bottom => (i64::from(point.y), i64::from(point.x)),
        }
    }

    fn point(self, normal: i64, along: i64) -> Point {
        // The rectangle constructor ensures each edge pixel fits in i32.
        match self {
            Self::Left | Self::Right => Point {
                x: normal as i32,
                y: along as i32,
            },
            Self::Top | Self::Bottom => Point {
                x: along as i32,
                y: normal as i32,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayRect, Edge, Point};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> DisplayRect {
        DisplayRect::new(
            Point { x, y },
            width.try_into().unwrap(),
            height.try_into().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn contains_negative_origins_and_excludes_far_boundaries() {
        let r = rect(-10, -20, 10, 20);
        assert!(r.contains(Point { x: -10, y: -20 }));
        assert!(r.contains(Point { x: -1, y: -1 }));
        assert!(!r.contains(Point { x: 0, y: -1 }));
        assert!(!r.contains(Point { x: -1, y: 0 }));
        assert!(!r.contains(Point { x: -11, y: -20 }));
    }

    #[test]
    fn accepts_last_i32_pixel_and_rejects_overflow() {
        let edge = Point {
            x: i32::MAX,
            y: i32::MAX,
        };
        assert!(rect(edge.x, edge.y, 1, 1).contains(edge));
        assert!(DisplayRect::new(edge, 2.try_into().unwrap(), 1.try_into().unwrap()).is_none());
        assert!(DisplayRect::new(edge, 1.try_into().unwrap(), 2.try_into().unwrap()).is_none());
    }

    #[test]
    fn maps_only_the_shared_segment_of_adjacent_displays() {
        let left = rect(-10, -8, 10, 12);
        let right = rect(0, -2, 20, 10);
        assert!(left.adjacent_at(Edge::Right, right));
        assert!(right.adjacent_at(Edge::Left, left));
        assert_eq!(
            left.entry_at(Edge::Right, Point { x: -1, y: -2 }, right),
            Some(Point { x: 0, y: -2 })
        );
        assert_eq!(
            right.entry_at(Edge::Left, Point { x: 0, y: 3 }, left),
            Some(Point { x: -1, y: 3 })
        );
        assert_eq!(
            left.entry_at(Edge::Right, Point { x: -1, y: -3 }, right),
            None
        );
        assert_eq!(
            left.entry_at(Edge::Right, Point { x: -2, y: -2 }, right),
            None
        );
        assert!(!left.adjacent_at(Edge::Right, rect(0, 4, 10, 10)));
        assert!(!left.adjacent_at(Edge::Right, rect(-1, -8, 10, 12)));
        assert!(!left.adjacent_at(Edge::Right, rect(1, -8, 10, 12)));
    }

    #[test]
    fn maps_vertical_transitions_in_both_directions() {
        let top = rect(-5, -10, 10, 10);
        let bottom = rect(-6, 0, 20, 12);
        assert_eq!(
            top.entry_at(Edge::Bottom, Point { x: -4, y: -1 }, bottom),
            Some(Point { x: -4, y: 0 })
        );
        assert_eq!(
            bottom.entry_at(Edge::Top, Point { x: 4, y: 0 }, top),
            Some(Point { x: 4, y: -1 })
        );
        assert_eq!(bottom.entry_at(Edge::Top, Point { x: 5, y: 0 }, top), None);
    }

    #[test]
    fn scales_spans_including_endpoints_and_single_pixels() {
        let source = rect(0, 0, 2, 3);
        let target = rect(10, 10, 5, 10);
        for (y, mapped) in [(0, 10), (1, 15), (2, 19)] {
            assert_eq!(
                source.map_edge(Edge::Right, Point { x: 1, y }, target),
                Some(Point { x: 10, y: mapped })
            );
        }
        assert_eq!(
            rect(0, 0, 2, 1).map_edge(Edge::Right, Point { x: 1, y: 0 }, target),
            Some(Point { x: 10, y: 14 })
        );
        assert_eq!(
            source.map_edge(Edge::Right, Point { x: 0, y: 0 }, target),
            None
        );
    }

    #[test]
    fn scales_maximum_spans_without_overflow() {
        let wide = rect(i32::MIN, 0, u32::MAX, 1);
        let exit = Point {
            x: i32::MAX - 1,
            y: 0,
        };
        assert_eq!(wide.map_edge(Edge::Bottom, exit, wide), Some(exit));
    }

    #[test]
    fn horizontal_handoff_preserves_proportional_height_on_different_displays() {
        let windows = rect(0, 0, 2560, 1441);
        let mac = rect(-1200, -250, 1200, 801);
        let height = windows
            .horizontal_crossing(Edge::Left, Point { x: 5, y: 700 }, -5, 20)
            .unwrap();
        assert_eq!(
            mac.horizontal_entry(Edge::Right, height, 16),
            Point { x: -17, y: 150 }
        );
        assert_eq!(
            windows.horizontal_crossing(Edge::Left, Point { x: 0, y: 720 }, 1, 0),
            None
        );
        assert_eq!(
            windows.horizontal_crossing(Edge::Left, Point { x: 0, y: 720 }, 0, 10),
            None
        );
        assert_eq!(
            windows.horizontal_crossing(Edge::Left, Point { x: -1, y: 720 }, -2, 0),
            None
        );
    }
}
