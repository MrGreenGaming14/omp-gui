//! RFB wire format (RFC 6143): the handshake and the client to server messages.

use crate::vnc::{PasswordSource, RfbError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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
            let response = vnc_response(password.as_bytes(), &challenge);
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

    let mut setup = Vec::with_capacity(64);
    let encodings = crate::vnc::decode::encodings();
    for msg in [Msg::SetPixelFormat, Msg::SetEncodings(&encodings), Msg::UpdateRequest { incremental: false, width, height }] {
        msg.write(&mut setup);
    }
    stream.write_all(&setup).await?;
    Ok(ServerInit { width, height, name })
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

/// A client to server message.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Msg<'a> {
    SetPixelFormat,
    SetEncodings(&'a [i32]),
    /// FramebufferUpdateRequest for the whole `width` x `height` framebuffer.
    UpdateRequest { incremental: bool, width: u16, height: u16 },
    Key { down: bool, keysym: u32 },
    Pointer { mask: u8, x: u16, y: u16 },
}

impl Msg<'_> {
    /// Appends the message's bytes to `buf`.
    pub fn write(self, buf: &mut Vec<u8>) {
        match self {
            Self::SetPixelFormat => buf.extend([0, 0, 0, 0].iter().chain(&PIXEL_FORMAT)),
            Self::SetEncodings(list) => {
                buf.extend([2, 0].into_iter().chain((list.len() as u16).to_be_bytes()));
                buf.extend(list.iter().flat_map(|e| e.to_be_bytes()));
            }
            Self::UpdateRequest { incremental, width, height } => {
                buf.extend([3, u8::from(incremental), 0, 0, 0, 0].into_iter().chain(width.to_be_bytes()).chain(height.to_be_bytes()));
            }
            Self::Key { down, keysym } => buf.extend([4, u8::from(down), 0, 0].into_iter().chain(keysym.to_be_bytes())),
            Self::Pointer { mask, x, y } => buf.extend([5, mask].into_iter().chain(x.to_be_bytes()).chain(y.to_be_bytes())),
        }
    }

    /// The message's bytes.
    #[cfg(test)]
    pub fn bytes(self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.write(&mut buf);
        buf
    }
}

/// The response to a VNC Authentication challenge: the challenge encrypted
/// with DES in ECB mode, keyed by the first eight bytes of `password` (zero
/// padded) with every key byte bit-mirrored as VNC servers expect.
pub(crate) fn vnc_response(password: &[u8], challenge: &[u8; 16]) -> [u8; 16] {
    use des::cipher::{BlockCipherEncrypt, KeyInit};
    let mut key = [0u8; 8];
    for (k, &b) in key.iter_mut().zip(password) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new(&key.into());
    let mut out = *challenge;
    for block in out.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(block.into());
    }
    out
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

    #[test]
    fn vnc_response_mirrors_and_pads_the_key() {
        // "\x80" mirrors to the key 0100000000000000, zero up to parity: the
        // known DES answer for a zero key and block, twice (ECB).
        let zero = 0x8CA6_4DE9_C1B1_23A7u64.to_be_bytes();
        assert_eq!(vnc_response(b"\x80", &[0; 16]), [zero, zero].concat()[..]);
        // FIPS 81 "Now is t" vector: key 0123456789ABCDEF, mirrored back.
        let key: Vec<u8> = 0x0123_4567_89AB_CDEFu64.to_be_bytes().iter().map(|b| b.reverse_bits()).collect();
        let mut challenge = [0; 16];
        challenge[..8].copy_from_slice(b"Now is t");
        assert_eq!(vnc_response(&key, &challenge)[..8], 0x3FA4_0E8A_984D_4815u64.to_be_bytes());
    }
}
