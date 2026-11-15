use std::io;

use core_graphics::display::CGDisplay;
use core_graphics::geometry::CGPoint;
use core_graphics::{
    event::CGEvent,
    event_source::{CGEventSource, CGEventSourceStateID},
};
use unikvm_core::geometry::{DisplayRect, Point};

pub(super) fn primary() -> io::Result<DisplayRect> {
    let bounds = CGDisplay::main().bounds();
    let width = (bounds.size.width as u32).try_into();
    let height = (bounds.size.height as u32).try_into();
    let origin = Point {
        x: bounds.origin.x as i32,
        y: bounds.origin.y as i32,
    };
    match (width, height) {
        (Ok(width), Ok(height)) => DisplayRect::new(origin, width, height)
            .ok_or_else(|| io::Error::other("invalid primary display bounds")),
        _ => Err(io::Error::other("invalid primary display dimensions")),
    }
}

pub(super) fn has_display(point: Point) -> bool {
    CGDisplay::displays_with_point(CGPoint::new(f64::from(point.x), f64::from(point.y)), 1)
        .map_or(true, |(_, count)| count != 0)
}

pub(super) fn place(point: Point) -> io::Result<()> {
    CGDisplay::warp_mouse_cursor_position(CGPoint::new(f64::from(point.x), f64::from(point.y)))
        .map_err(|status| io::Error::other(format!("could not place macOS cursor ({status})")))
}

pub(super) fn position() -> io::Result<Point> {
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|()| io::Error::other("could not read macOS cursor"))?;
    let point = CGEvent::new(source)
        .map_err(|()| io::Error::other("could not read macOS cursor"))?
        .location();
    Ok(pixel_position(point))
}

fn pixel_position(point: CGPoint) -> Point {
    Point {
        // Preserve the containing pixel at fractional right/bottom edges.
        x: point.x.floor() as i32,
        y: point.y.floor() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unikvm_core::geometry::Edge;

    #[test]
    fn fractional_last_pixel_still_crosses_the_display_edge() {
        let display = DisplayRect::new(
            Point { x: 0, y: 0 },
            1280.try_into().unwrap(),
            832.try_into().unwrap(),
        )
        .unwrap();
        let point = pixel_position(CGPoint::new(1279.75, 831.75));
        assert_eq!(
            display.horizontal_crossing(Edge::Right, point, 4, 0),
            Some(u16::MAX)
        );
        let outside = pixel_position(CGPoint::new(1280.0, 400.0));
        assert!(!display.contains(outside));
        assert_eq!(
            pixel_position(CGPoint::new(-0.25, -0.25)),
            Point { x: -1, y: -1 }
        );
    }
}
