//! RFB wire format (RFC 6143): the handshake and the client to server messages.

use crate::{PasswordSource, RfbError, des};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const ENC_RAW: i32 = 0;
pub(crate) const ENC_COPY_RECT: i32 = 1;
pub(crate) const ENC_RRE: i32 = 2;
pub(crate) const ENC_HEXTILE: i32 = 5;
pub(crate) const ENC_ZRLE: i32 = 16;
pub(crate) const ENC_DESKTOP_SIZE: i32 = -223;
pub(crate) const ENC_CURSOR: i32 = -239;

/// Encodings in the client's order of preference.
pub(crate) const ENCODINGS: [i32; 7] = [
    ENC_COPY_RECT,
    ENC_ZRLE,
    ENC_HEXTILE,
    ENC_RRE,
    ENC_RAW,
    ENC_CURSOR,
    ENC_DESKTOP_SIZE,
];

/// The pixel format the client asks for: 32 bits per pixel, depth 24, little
/// endian true color with red in the lowest byte. A pixel on the wire is then
/// `R G B X`, and a ZRLE CPIXEL is `R G B`.
pub(crate) const PIXEL_FORMAT: [u8; 16] = [32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0];

/// Bytes per pixel in [`PIXEL_FORMAT`].
pub(crate) const BPP: usize = 4;

const SEC_INVALID: u32 = 0;
const SEC_NONE: u8 = 1;
const SEC_VNC_AUTH: u8 = 2;

/// Upper bound for server supplied strings (reasons, desktop name).
const MAX_STRING: u32 = 1 << 16;

/// What the server says about the desktop after the handshake.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
}

/// Protocol minor versions the client speaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Version {
    V3,
    V7,
    V8,
}

/// Runs the handshake up to ServerInit, then sends SetPixelFormat,
/// SetEncodings and a full FramebufferUpdateRequest.
pub(crate) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &str,
    passwords: &dyn PasswordSource,
    shared: bool,
) -> Result<ServerInit, RfbError> {
    let mut banner = [0u8; 12];
    stream.read_exact(&mut banner).await?;
    let version = parse_version(&banner)?;
    let reply: &[u8; 12] = match version {
        Version::V3 => b"RFB 003.003\n",
        Version::V7 => b"RFB 003.007\n",
        Version::V8 => b"RFB 003.008\n",
    };
    stream.write_all(reply).await?;

    let security = if version == Version::V3 {
        match stream.read_u32().await? {
            SEC_INVALID => return Err(RfbError::Refused(read_string(stream).await?)),
            t => u8::try_from(t).map_err(|_| RfbError::Security(Vec::new()))?,
        }
    } else {
        let count = stream.read_u8().await?;
        if count == 0 {
            return Err(RfbError::Refused(read_string(stream).await?));
        }
        let mut types = vec![0u8; usize::from(count)];
        stream.read_exact(&mut types).await?;
        let chosen = [SEC_NONE, SEC_VNC_AUTH]
            .into_iter()
            .find(|t| types.contains(t));
        let chosen = chosen.ok_or(RfbError::Security(types))?;
        stream.write_all(&[chosen]).await?;
        chosen
    };

    match security {
        SEC_NONE => {
            if version == Version::V8 {
                security_result(stream, version).await?;
            }
        }
        SEC_VNC_AUTH => {
            let mut challenge = [0u8; 16];
            stream.read_exact(&mut challenge).await?;
            let password = passwords
                .password(endpoint)
                .await
                .ok_or_else(|| RfbError::NoPassword(endpoint.to_owned()))?;
            let response = des::vnc_response(password.as_bytes(), &challenge);
            drop(password);
            stream.write_all(&response).await?;
            security_result(stream, version).await?;
        }
        other => return Err(RfbError::Security(vec![other])),
    }

    stream.write_all(&[u8::from(shared)]).await?;
    let width = stream.read_u16().await?;
    let height = stream.read_u16().await?;
    let mut server_format = [0u8; 16];
    stream.read_exact(&mut server_format).await?;
    let name = read_string(stream).await?;

    let mut setup = Vec::with_capacity(20 + 4 + 4 * ENCODINGS.len() + 10);
    set_pixel_format(&mut setup);
    set_encodings(&mut setup);
    update_request(&mut setup, false, width, height);
    stream.write_all(&setup).await?;
    Ok(ServerInit {
        width,
        height,
        name,
    })
}

fn parse_version(banner: &[u8; 12]) -> Result<Version, RfbError> {
    let bad = || {
        RfbError::Protocol(format!(
            "bad version banner {:?}",
            String::from_utf8_lossy(banner)
        ))
    };
    let text = std::str::from_utf8(banner).map_err(|_| bad())?;
    let rest = text
        .strip_prefix("RFB ")
        .and_then(|r| r.strip_suffix('\n'))
        .ok_or_else(bad)?;
    let (major, minor) = rest.split_once('.').ok_or_else(bad)?;
    let major: u32 = major.parse().map_err(|_| bad())?;
    let minor: u32 = minor.parse().map_err(|_| bad())?;
    match (major, minor) {
        (3, 8..) => Ok(Version::V8),
        (3, 7) => Ok(Version::V7),
        (3, 3..=6) => Ok(Version::V3),
        _ => Err(bad()),
    }
}

async fn security_result<S: AsyncRead + Unpin>(
    stream: &mut S,
    version: Version,
) -> Result<(), RfbError> {
    match stream.read_u32().await? {
        0 => Ok(()),
        _ if version == Version::V8 => Err(RfbError::Auth(read_string(stream).await?)),
        _ => Err(RfbError::Auth(
            "the server rejected the credentials".to_owned(),
        )),
    }
}

/// Reads a u32 length prefixed string.
async fn read_string<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, RfbError> {
    let len = stream.read_u32().await?;
    if len > MAX_STRING {
        return Err(RfbError::Protocol(format!("string of {len} bytes")));
    }
    let mut bytes = vec![0u8; len as usize];
    stream.read_exact(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn set_pixel_format(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&PIXEL_FORMAT);
}

pub(crate) fn set_encodings(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&[2, 0]);
    buf.extend_from_slice(&(ENCODINGS.len() as u16).to_be_bytes());
    for e in ENCODINGS {
        buf.extend_from_slice(&e.to_be_bytes());
    }
}

/// FramebufferUpdateRequest for the whole `width` x `height` framebuffer.
pub(crate) fn update_request(buf: &mut Vec<u8>, incremental: bool, width: u16, height: u16) {
    buf.extend_from_slice(&[3, u8::from(incremental), 0, 0, 0, 0]);
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
}

pub(crate) fn key_event(buf: &mut Vec<u8>, down: bool, keysym: u32) {
    buf.extend_from_slice(&[4, u8::from(down), 0, 0]);
    buf.extend_from_slice(&keysym.to_be_bytes());
}

pub(crate) fn pointer_event(buf: &mut Vec<u8>, mask: u8, x: u16, y: u16) {
    buf.extend_from_slice(&[5, mask]);
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_negotiation() {
        assert_eq!(parse_version(b"RFB 003.008\n").unwrap(), Version::V8);
        assert_eq!(parse_version(b"RFB 003.889\n").unwrap(), Version::V8);
        assert_eq!(parse_version(b"RFB 003.007\n").unwrap(), Version::V7);
        assert_eq!(parse_version(b"RFB 003.003\n").unwrap(), Version::V3);
        assert!(parse_version(b"RFB 004.000\n").is_err());
        assert!(parse_version(b"HTTP/1.1 200").is_err());
    }
}
