use std::io;

use core_graphics::display::CGDisplay;
use core_graphics::geometry::CGPoint;
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
