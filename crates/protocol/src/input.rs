//! Bounded records for an opt-in control stream. Each stream is one focus session.

use std::io;
use std::num::NonZeroU64;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use unikvm_core::geometry::Edge;
use unikvm_core::input::{ButtonState, InputEvent, KeyUsage, PointerButton};

pub const REQUEST: &[u8; 8] = b"INPUT/1\n";
pub const EDGE_REQUEST: &[u8; 8] = b"EDGE/1\r\n";
pub const DRAG_REQUEST: &[u8; 8] = b"DRAG/1\r\n";
pub const DRAG_PREPARE: u8 = 254;
pub const GRANT: u8 = 1;
pub const DENY: u8 = 0;
pub const HEARTBEAT_ACK: u8 = 2;
pub const HEARTBEAT_TOKENS: u8 = 250;
pub const EDGE_RETURN: u8 = HEARTBEAT_ACK + HEARTBEAT_TOKENS;
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(250);
pub const LEASE: Duration = Duration::from_millis(1250);

/// First request bytes are always eight bytes so older peers can reject edge mode.
pub fn edge_packet(side: Edge, height: u16) -> [u8; 11] {
    let mut packet = [0; 11];
    packet[..8].copy_from_slice(EDGE_REQUEST);
    packet[8] = match side {
        Edge::Left => 0,
        Edge::Right => 1,
        _ => unreachable!("edge sessions require a horizontal exit"),
    };
    packet[9..].copy_from_slice(&height.to_be_bytes());
    packet
}

/// Start an edge session with heartbeat zero in the same write as the request.
pub fn edge_start(side: Edge, height: u16) -> [u8; 20] {
    let mut packet = [0; 20];
    packet[..11].copy_from_slice(&edge_packet(side, height));
    packet[11..].copy_from_slice(&encode(Frame::Heartbeat(0)));
    packet
}

pub fn drag_start(side: Edge, height: u16, nonce: NonZeroU64) -> [u8; 19] {
    let mut packet = [0; 19];
    packet[..11].copy_from_slice(&edge_packet(side, height));
    packet[..8].copy_from_slice(DRAG_REQUEST);
    packet[11..].copy_from_slice(&nonce.get().to_be_bytes());
    packet
}

pub fn edge_entry([side, hi, lo]: [u8; 3]) -> Option<(Edge, u16)> {
    let entry = match side {
        0 => Edge::Right,
        1 => Edge::Left,
        _ => return None,
    };
    Some((entry, u16::from_be_bytes([hi, lo])))
}

pub fn edge_return(height: u16) -> [u8; 3] {
    let [hi, lo] = height.to_be_bytes();
    [EDGE_RETURN, hi, lo]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Heartbeat(u8),
    Release,
    Event(InputEvent),
}

pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Frame> {
    let mut bytes = [0; 9];
    reader.read_exact(&mut bytes).await?;
    let frame = match bytes[0] {
        0 if bytes[1] < HEARTBEAT_TOKENS && bytes[2..] == [0; 7] => Frame::Heartbeat(bytes[1]),
        1 if bytes[1..] == [0; 8] => Frame::Release,
        2 if bytes[4..] == [0; 5] => {
            let usage = u16::from_be_bytes([bytes[1], bytes[2]]);
            if !(0x04..=0xe7).contains(&usage) {
                return Err(invalid());
            }
            Frame::Event(InputEvent::Key {
                usage: KeyUsage(usage),
                state: state(bytes[3])?,
            })
        }
        3 if bytes[3..] == [0; 6] => {
            let button = match bytes[1] {
                0 => PointerButton::Primary,
                1 => PointerButton::Secondary,
                2 => PointerButton::Middle,
                3 => PointerButton::Back,
                4 => PointerButton::Forward,
                _ => return Err(invalid()),
            };
            Frame::Event(InputEvent::PointerButton {
                button,
                state: state(bytes[2])?,
            })
        }
        tag @ (4 | 5) => {
            let x = i32::from_be_bytes(bytes[1..5].try_into().unwrap());
            let y = i32::from_be_bytes(bytes[5..9].try_into().unwrap());
            if x.unsigned_abs() > 32768 || y.unsigned_abs() > 32768 {
                return Err(invalid());
            }
            Frame::Event(if tag == 4 {
                InputEvent::PointerMove { dx: x, dy: y }
            } else {
                InputEvent::Wheel {
                    horizontal: x,
                    vertical: y,
                }
            })
        }
        _ => return Err(invalid()),
    };
    Ok(frame)
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), frame: Frame) -> io::Result<()> {
    writer.write_all(&encode(frame)).await
}

fn encode(frame: Frame) -> [u8; 9] {
    let mut bytes = [0; 9];
    match frame {
        Frame::Heartbeat(token) => bytes[1] = token,
        Frame::Release => bytes[0] = 1,
        Frame::Event(InputEvent::Key { usage, state }) => {
            bytes[0] = 2;
            bytes[1..3].copy_from_slice(&usage.0.to_be_bytes());
            bytes[3] = u8::from(state == ButtonState::Pressed);
        }
        Frame::Event(InputEvent::PointerButton { button, state }) => {
            bytes[0] = 3;
            bytes[1] = match button {
                PointerButton::Primary => 0,
                PointerButton::Secondary => 1,
                PointerButton::Middle => 2,
                PointerButton::Back => 3,
                PointerButton::Forward => 4,
            };
            bytes[2] = u8::from(state == ButtonState::Pressed);
        }
        Frame::Event(InputEvent::PointerMove { dx, dy })
        | Frame::Event(InputEvent::Wheel {
            horizontal: dx,
            vertical: dy,
        }) => {
            bytes[0] = if matches!(frame, Frame::Event(InputEvent::PointerMove { .. })) {
                4
            } else {
                5
            };
            bytes[1..5].copy_from_slice(&dx.to_be_bytes());
            bytes[5..9].copy_from_slice(&dy.to_be_bytes());
        }
    }
    bytes
}

fn state(value: u8) -> io::Result<ButtonState> {
    match value {
        0 => Ok(ButtonState::Released),
        1 => Ok(ButtonState::Pressed),
        _ => Err(invalid()),
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid input frame")
}

#[cfg(test)]
mod tests {
    use super::{Frame, read, write};
    use unikvm_core::input::{ButtonState, InputEvent, KeyUsage, PointerButton};

    #[tokio::test]
    async fn round_trips_fixed_frames_and_rejects_invalid_input() {
        for frame in [
            Frame::Heartbeat(7),
            Frame::Release,
            Frame::Event(InputEvent::Key {
                usage: KeyUsage(0xe1),
                state: ButtonState::Released,
            }),
            Frame::Event(InputEvent::PointerButton {
                button: PointerButton::Forward,
                state: ButtonState::Pressed,
            }),
            Frame::Event(InputEvent::PointerMove { dx: -10, dy: 200 }),
            Frame::Event(InputEvent::Wheel {
                horizontal: 120,
                vertical: -240,
            }),
        ] {
            let mut bytes = Vec::new();
            write(&mut bytes, frame).await.unwrap();
            assert_eq!(read(&mut bytes.as_slice()).await.unwrap(), frame);
        }
        for invalid in [
            [9; 9],
            [0, 250, 0, 0, 0, 0, 0, 0, 0],
            [2, 0, 4, 2, 0, 0, 0, 0, 0],
            [3, 5, 1, 0, 0, 0, 0, 0, 0],
            [4, 127, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(
                read(&mut invalid.as_slice()).await.unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }
}
#[test]
fn edge_request_requires_an_opposite_horizontal_side_and_preserves_height() {
    let request = edge_packet(Edge::Left, 0x1234);
    assert_eq!(&request[..8], EDGE_REQUEST);
    assert_eq!(
        edge_entry(request[8..].try_into().unwrap()),
        Some((Edge::Right, 0x1234))
    );
    assert_eq!(edge_entry([1, 0, 0]), Some((Edge::Left, 0)));
    assert_eq!(edge_entry([2, 0, 0]), None);
    assert_eq!(edge_return(0x1234), [EDGE_RETURN, 0x12, 0x34]);
    let drag = drag_start(Edge::Right, 0x1234, NonZeroU64::new(83).unwrap());
    assert_eq!(&drag[..8], DRAG_REQUEST);
    assert_eq!(
        edge_entry(drag[8..11].try_into().unwrap()),
        Some((Edge::Left, 0x1234))
    );
    assert_eq!(u64::from_be_bytes(drag[11..].try_into().unwrap()), 83);
}
#[cfg(test)]
#[tokio::test]
async fn edge_start_appends_a_heartbeat_to_the_existing_request() {
    let packet = edge_start(Edge::Right, 0x7654);
    let (request, mut frames) = packet.split_at(11);
    assert_eq!(request, edge_packet(Edge::Right, 0x7654));
    assert_eq!(read(&mut frames).await.unwrap(), Frame::Heartbeat(0));
    assert!(frames.is_empty());
}
