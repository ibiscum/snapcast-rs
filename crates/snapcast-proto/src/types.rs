//! Shared types used across the protocol.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

/// Timestamp with second and microsecond components.
///
/// Matches the C++ `tv` struct used throughout the Snapcast protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeval {
    /// Seconds component.
    pub sec: i32,
    /// Microseconds component.
    pub usec: i32,
}

impl Timeval {
    /// Create from microseconds on a shared time timeline.
    ///
    /// The input may be wall-clock or monotonic, depending on caller context.
    pub fn from_usec(usec: i64) -> Self {
        Self::from_parts_i64(usec.div_euclid(1_000_000), usec.rem_euclid(1_000_000))
    }

    /// Convert to total microseconds on the same timeline.
    pub fn to_usec(self) -> i64 {
        self.sec as i64 * 1_000_000 + self.usec as i64
    }

    /// Read a Timeval (8 bytes, little-endian) from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            sec: r.read_i32::<LittleEndian>()?,
            usec: r.read_i32::<LittleEndian>()?,
        })
    }

    /// Write a Timeval (8 bytes, little-endian) to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_i32::<LittleEndian>(self.sec)?;
        w.write_i32::<LittleEndian>(self.usec)?;
        Ok(())
    }

    fn from_parts_i64(sec: i64, usec: i64) -> Self {
        let sec = i32::try_from(sec).expect("Timeval seconds out of i32 range");
        let usec = i32::try_from(usec).expect("Timeval microseconds out of i32 range");
        Self { sec, usec }
    }

    fn normalized_from_parts(sec: i64, usec: i64) -> Self {
        let carry = usec.div_euclid(1_000_000);
        let rem = usec.rem_euclid(1_000_000);
        Self::from_parts_i64(sec + carry, rem)
    }
}

impl std::ops::Add for Timeval {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::normalized_from_parts(
            i64::from(self.sec) + i64::from(rhs.sec),
            i64::from(self.usec) + i64::from(rhs.usec),
        )
    }
}

impl std::ops::Sub for Timeval {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self::normalized_from_parts(
            i64::from(self.sec) - i64::from(rhs.sec),
            i64::from(self.usec) - i64::from(rhs.usec),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("forced write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn timeval_add() {
        let a = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let b = Timeval {
            sec: 0,
            usec: 200_000,
        };
        let result = a + b;
        assert_eq!(
            result,
            Timeval {
                sec: 2,
                usec: 100_000
            }
        );
    }

    #[test]
    fn timeval_sub() {
        let a = Timeval {
            sec: 2,
            usec: 100_000,
        };
        let b = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let result = a - b;
        assert_eq!(
            result,
            Timeval {
                sec: 0,
                usec: 200_000
            }
        );
    }

    #[test]
    fn timeval_add_normalizes_negative_microseconds() {
        let a = Timeval { sec: 1, usec: 100 };
        let b = Timeval {
            sec: 0,
            usec: -200,
        };
        assert_eq!(
            a + b,
            Timeval {
                sec: 0,
                usec: 999_900
            }
        );
    }

    #[test]
    fn timeval_sub_normalizes_negative_microseconds() {
        let a = Timeval { sec: 1, usec: 0 };
        let b = Timeval { sec: 0, usec: 1 };
        assert_eq!(
            a - b,
            Timeval {
                sec: 0,
                usec: 999_999
            }
        );
    }

    #[test]
    fn timeval_round_trip() {
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 8);
        let mut cursor = io::Cursor::new(&buf);
        let decoded = Timeval::read_from(&mut cursor).unwrap();
        assert_eq!(tv, decoded);
    }

    #[test]
    fn timeval_known_bytes() {
        // sec=1000 (0x000003E8), usec=500000 (0x0007A120), little-endian
        let expected: [u8; 8] = [0xE8, 0x03, 0x00, 0x00, 0x20, 0xA1, 0x07, 0x00];
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), &expected);
    }

    #[test]
    fn from_usec_normalizes_negative_values() {
        let tv = Timeval::from_usec(-1);
        assert_eq!(
            tv,
            Timeval {
                sec: -1,
                usec: 999_999
            }
        );
        assert_eq!(tv.to_usec(), -1);
    }

    #[test]
    fn from_usec_to_usec_round_trip() {
        let values = [
            0_i64,
            1,
            -1,
            999_999,
            1_000_000,
            -1_000_000,
            1_234_567_890,
            -1_234_567_890,
        ];
        for v in values {
            assert_eq!(Timeval::from_usec(v).to_usec(), v);
        }
    }

    #[test]
    fn read_from_truncated_is_io_error() {
        let mut cursor = io::Cursor::new([0u8; 7]);
        assert!(Timeval::read_from(&mut cursor).is_err());
    }

    #[test]
    fn write_to_propagates_io_error() {
        let tv = Timeval { sec: 1, usec: 2 };
        let mut writer = FailingWriter;
        assert!(tv.write_to(&mut writer).is_err());
    }

    #[test]
    fn arithmetic_results_keep_usec_normalized() {
        let sum = Timeval {
            sec: 10,
            usec: 900_000,
        } + Timeval {
            sec: -5,
            usec: 300_000,
        };
        assert!((0..1_000_000).contains(&sum.usec));

        let diff = Timeval {
            sec: -2,
            usec: 100_000,
        } - Timeval {
            sec: 1,
            usec: 900_000,
        };
        assert!((0..1_000_000).contains(&diff.usec));
    }
}
