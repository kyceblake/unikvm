use std::{
    io,
    time::{Duration, Instant},
};

use unikvm_core::geometry::{DisplayRect, Edge, Point};

#[cfg(target_os = "macos")]
use super::macos_edge as native;
#[cfg(target_os = "windows")]
use super::windows_edge as native;

#[derive(Clone, Copy)]
pub struct EdgeLayout {
    pub side: Edge,
    display: DisplayRect,
}

impl EdgeLayout {
    pub fn primary(side: Edge) -> io::Result<Self> {
        if !matches!(side, Edge::Left | Edge::Right) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "edge switching requires a left or right side",
            ));
        }
        Ok(Self {
            side,
            display: native::primary()?,
        })
    }

    pub(super) fn crossing(self, current: Point, dx: i32, dy: i32) -> Option<u16> {
        let height = self
            .display
            .horizontal_crossing(self.side, current, dx, dy)?;
        if native::primary().ok()? != self.display {
            return None;
        }
        let outside = self.display.horizontal_entry(self.side, height, 0);
        let x = match self.side {
            Edge::Left => outside.x.checked_sub(1)?,
            Edge::Right => outside.x.checked_add(1)?,
            _ => return None,
        };
        (!native::has_display(Point { x, y: outside.y })).then_some(height)
    }

    pub(super) fn place(self, height: u16) -> io::Result<Point> {
        let point = self.entry(height)?;
        native::place(point)?;
        Ok(point)
    }

    pub(super) fn entry(self, height: u16) -> io::Result<Point> {
        // Refresh the destination coordinates if its mode changed since startup.
        Ok(native::primary()?.horizontal_entry(self.side, height, 2))
    }
}

pub(super) struct EdgeTrigger {
    pub layout: EdgeLayout,
    last: Option<Instant>,
}

impl EdgeTrigger {
    pub(super) fn new(layout: EdgeLayout) -> Self {
        Self { layout, last: None }
    }

    pub(super) fn attempt(&mut self, point: Point, dx: i32, dy: i32) -> Option<u16> {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(400))
        {
            return None;
        }
        let height = self.layout.crossing(point, dx, dy)?;
        self.last = Some(now);
        Some(height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive input desktop"]
    fn places_the_pointer_inside_the_primary_edge_without_takeover() {
        struct Restore(Point);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = native::place(self.0);
            }
        }

        let _restore = Restore(native::position().unwrap());
        let edge = EdgeLayout::primary(Edge::Left).unwrap();
        let capture = super::super::Capture::with_edge(true, Some(edge)).unwrap();
        let baseline = capture.shortcuts.borrow().activity;
        let height = u16::MAX / 4;
        let point = edge.entry(height).unwrap();
        let mut sink = super::super::InputSink::new().unwrap();
        sink.enter_edge(edge, height).unwrap();
        assert_eq!(native::position().unwrap(), point);
        assert!(edge.display.contains(point));
        assert_eq!(
            point,
            edge.display.horizontal_entry(Edge::Left, u16::MAX / 4, 2)
        );
        assert_eq!(capture.shortcuts.borrow().activity, baseline);
    }
}
