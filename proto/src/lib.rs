//! rpi-camilla-bridge wire protocol.
//!
//! 16-byte fixed header sent once at the start of a TCP connection, followed by
//! a continuous stream of interleaved little-endian PCM frames in the declared
//! format. No per-frame framing, no checksum — TCP is the integrity guarantee.
//!
//! ```text
//! offset  size  field
//! 0       4     magic     = b"CDSP"
//! 4       1     version   = 1
//! 5       1     format    (0=S16LE, 1=S32LE, 2=F32LE)
//! 6       2     channels  (u16 LE)
//! 8       4     sample_rate (u32 LE)
//! 12      4     reserved  (u32 LE, must be zero on send)
//! ```

use std::io::{Read, Write};

use thiserror::Error;

pub const MAGIC: &[u8; 4] = b"CDSP";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    S16LE = 0,
    S32LE = 1,
    F32LE = 2,
}

impl Format {
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::S16LE => 2,
            Self::S32LE | Self::F32LE => 4,
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for Format {
    type Error = ProtoError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::S16LE),
            1 => Ok(Self::S32LE),
            2 => Ok(Self::F32LE),
            other => Err(ProtoError::BadFormat(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub format: Format,
    pub channels: u16,
    pub sample_rate: u32,
}

impl Header {
    pub const fn frame_bytes(&self) -> usize {
        self.format.bytes_per_sample() * self.channels as usize
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(MAGIC);
        buf[4] = VERSION;
        buf[5] = self.format.as_u8();
        buf[6..8].copy_from_slice(&self.channels.to_le_bytes());
        buf[8..12].copy_from_slice(&self.sample_rate.to_le_bytes());
        // bytes 12..16 are reserved, already zero.
        w.write_all(&buf)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let mut buf = [0u8; HEADER_LEN];
        r.read_exact(&mut buf)?;

        if &buf[0..4] != MAGIC {
            return Err(ProtoError::BadMagic([buf[0], buf[1], buf[2], buf[3]]));
        }
        if buf[4] != VERSION {
            return Err(ProtoError::BadVersion(buf[4]));
        }

        let format = Format::try_from(buf[5])?;
        let channels = u16::from_le_bytes([buf[6], buf[7]]);
        let sample_rate = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);

        if channels == 0 {
            return Err(ProtoError::BadChannels(0));
        }
        if sample_rate == 0 {
            return Err(ProtoError::BadSampleRate(0));
        }

        Ok(Self {
            format,
            channels,
            sample_rate,
        })
    }
}

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("bad magic: expected {expected:?}, got {0:?}", expected = MAGIC)]
    BadMagic([u8; 4]),
    #[error("unsupported protocol version {0} (this build supports {VERSION})")]
    BadVersion(u8),
    #[error("unsupported format code {0}")]
    BadFormat(u8),
    #[error("invalid channel count {0}")]
    BadChannels(u16),
    #[error("invalid sample rate {0}")]
    BadSampleRate(u32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            format: Format::S32LE,
            channels: 2,
            sample_rate: 96_000,
        };
        let mut buf = Vec::new();
        h.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), HEADER_LEN);
        assert_eq!(&buf[0..4], MAGIC);
        assert_eq!(buf[4], VERSION);

        let mut cur = Cursor::new(buf);
        let parsed = Header::read_from(&mut cur).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn header_roundtrip_all_formats() {
        for fmt in [Format::S16LE, Format::S32LE, Format::F32LE] {
            for ch in [1u16, 2, 8] {
                for rate in [44_100u32, 48_000, 96_000, 192_000] {
                    let h = Header {
                        format: fmt,
                        channels: ch,
                        sample_rate: rate,
                    };
                    let mut buf = Vec::new();
                    h.write_to(&mut buf).unwrap();
                    let parsed = Header::read_from(&mut Cursor::new(buf)).unwrap();
                    assert_eq!(parsed, h);
                }
            }
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = vec![
            b'X', b'X', b'X', b'X', VERSION, 0, 2, 0, 0x80, 0xBB, 0, 0, 0, 0, 0, 0,
        ];
        let err = Header::read_from(&mut Cursor::new(&mut buf)).unwrap_err();
        assert!(matches!(err, ProtoError::BadMagic(m) if &m == b"XXXX"));
    }

    #[test]
    fn rejects_bad_version() {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(MAGIC);
        buf.push(99); // version
        buf.push(0); // format
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&48_000u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        let err = Header::read_from(&mut Cursor::new(buf)).unwrap_err();
        assert!(matches!(err, ProtoError::BadVersion(99)));
    }

    #[test]
    fn rejects_bad_format() {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.push(7); // bogus format
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&48_000u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        let err = Header::read_from(&mut Cursor::new(buf)).unwrap_err();
        assert!(matches!(err, ProtoError::BadFormat(7)));
    }

    #[test]
    fn rejects_zero_channels() {
        let h_buf = {
            let mut b = Vec::with_capacity(HEADER_LEN);
            b.extend_from_slice(MAGIC);
            b.push(VERSION);
            b.push(Format::S16LE.as_u8());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&48_000u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b
        };
        let err = Header::read_from(&mut Cursor::new(h_buf)).unwrap_err();
        assert!(matches!(err, ProtoError::BadChannels(0)));
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let h_buf = {
            let mut b = Vec::with_capacity(HEADER_LEN);
            b.extend_from_slice(MAGIC);
            b.push(VERSION);
            b.push(Format::S16LE.as_u8());
            b.extend_from_slice(&2u16.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b
        };
        let err = Header::read_from(&mut Cursor::new(h_buf)).unwrap_err();
        assert!(matches!(err, ProtoError::BadSampleRate(0)));
    }

    #[test]
    fn format_u8_roundtrip_exhaustive() {
        for f in [Format::S16LE, Format::S32LE, Format::F32LE] {
            assert_eq!(Format::try_from(f.as_u8()).unwrap(), f);
        }
        for v in 3u8..=255u8 {
            assert!(Format::try_from(v).is_err());
        }
    }

    #[test]
    fn truncated_header_is_io_error() {
        let buf = vec![b'C', b'D', b'S', b'P', VERSION]; // only 5 bytes
        let err = Header::read_from(&mut Cursor::new(buf)).unwrap_err();
        assert!(matches!(err, ProtoError::Io(_)));
    }

    #[test]
    fn frame_bytes_matches_format_and_channels() {
        let h = Header {
            format: Format::S32LE,
            channels: 2,
            sample_rate: 96_000,
        };
        assert_eq!(h.frame_bytes(), 8);
        assert_eq!(
            Header {
                format: Format::S16LE,
                channels: 8,
                sample_rate: 48_000,
            }
            .frame_bytes(),
            16
        );
    }
}
