// Copyright (C) 2026  Jan Novák
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Decoder and output processes for one track.
//!
//! `madplay` (the decoder the device already ships for its own sounds) reads
//! the MP3 stream on stdin and writes WAV to stdout; the player scales the
//! samples by the volume and writes them to `aplay`. Passing the PCM through
//! the player is what gives an exact position (bytes written), an instant
//! volume and a pause that only stops writing.

use std::io::{self, Read};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// PCM layout from the decoder's WAV header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl PcmFormat {
    /// 16-bit samples, the only depth madplay's WAV output uses.
    pub(crate) fn bytes_per_second(self) -> u64 {
        u64::from(self.sample_rate) * u64::from(self.channels) * 2
    }
}

/// Read a streamed WAV header up to the start of the sample data.
///
/// A decoder writing to a pipe can't know the lengths up front, so the size
/// fields are ignored; only `fmt ` matters, and the data runs to EOF.
pub(crate) fn read_wav_header(reader: &mut impl Read) -> io::Result<PcmFormat> {
    let bad =
        |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("WAV header: {what}"));
    let mut riff = [0_u8; 12];
    reader.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(bad("not RIFF/WAVE"));
    }
    let mut format = None;
    loop {
        let mut chunk = [0_u8; 8];
        reader.read_exact(&mut chunk)?;
        let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        match &chunk[0..4] {
            b"data" => return format.ok_or_else(|| bad("data before fmt")),
            b"fmt " => {
                if !(16..=64).contains(&len) {
                    return Err(bad("fmt length"));
                }
                let mut fmt = vec![0_u8; usize::try_from(len).expect("BUG: bounded above")];
                reader.read_exact(&mut fmt)?;
                let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
                let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                let sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                if tag != 1 || bits != 16 || !(1..=2).contains(&channels) || sample_rate == 0 {
                    return Err(bad("not 16-bit PCM, mono or stereo"));
                }
                format = Some(PcmFormat {
                    sample_rate,
                    channels,
                });
                if len % 2 == 1 {
                    reader.read_exact(&mut [0_u8; 1])?;
                }
            }
            _ => {
                // Skip a chunk we don't need, padded to even length.
                let skip = u64::from(len) + u64::from(len % 2);
                if io::copy(&mut reader.take(skip), &mut io::sink())? != skip {
                    return Err(bad("truncated chunk"));
                }
            }
        }
    }
}

/// Volume 0–100 as a Q15 gain on a square curve, closer to how loudness is
/// heard than a straight line.
#[expect(
    clippy::integer_division,
    reason = "a gain step below 1/32768 is inaudible"
)]
pub(crate) fn gain_q15(volume: u8) -> i32 {
    let v = i32::from(volume.min(100));
    v * v * 32_768 / 10_000
}

/// Scale little-endian 16-bit samples in place. A trailing odd byte (a
/// sample split across reads) is left for the caller to carry over.
pub(crate) fn apply_gain(pcm: &mut [u8], gain_q15: i32) {
    if gain_q15 >= 32_768 {
        return;
    }
    for sample in pcm.chunks_exact_mut(2) {
        let s = i32::from(i16::from_le_bytes([sample[0], sample[1]]));
        let scaled =
            i16::try_from((s * gain_q15) >> 15).expect("BUG: gain below 1 keeps i16 range");
        sample.copy_from_slice(&scaled.to_le_bytes());
    }
}

/// `madplay` from `skip_ms` into the stream, MP3 in on stdin, WAV out.
#[expect(
    clippy::integer_division,
    reason = "whole seconds, the rest goes into the fraction"
)]
pub(crate) fn spawn_decoder(skip_ms: u64) -> io::Result<(Child, ChildStdin, ChildStdout)> {
    let mut cmd = Command::new("madplay");
    cmd.args(["-q", "-o", "wave:-"]);
    if skip_ms > 0 {
        cmd.arg(format!("--start={}.{:03}", skip_ms / 1000, skip_ms % 1000));
    }
    let mut child = cmd
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().expect("BUG: stdin was piped");
    let stdout = child.stdout.take().expect("BUG: stdout was piped");
    Ok((child, stdin, stdout))
}

/// `aplay` for raw 16-bit PCM in `format` on stdin, to the default device.
pub(crate) fn spawn_output(format: PcmFormat) -> io::Result<(Child, ChildStdin)> {
    let mut child = Command::new("aplay")
        .args(["-q", "-t", "raw", "-f", "S16_LE"])
        .arg(format!("-r{}", format.sample_rate))
        .arg(format!("-c{}", format.channels))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().expect("BUG: stdin was piped");
    Ok((child, stdin))
}

/// Child processes killed and reaped when dropped, whatever ended the track.
#[derive(Default)]
pub(crate) struct Children(Vec<Child>);

impl Children {
    pub(crate) fn push(&mut self, child: Child) {
        self.0.push(child);
    }

    /// Let the output finish what it was given (end of track): close its
    /// input and wait, instead of killing it with audio still buffered.
    pub(crate) fn drain_output(&mut self, output: ChildStdin) {
        drop(output);
        if let Some(mut out) = self.0.pop() {
            let _ = out.wait();
        }
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PcmFormat, apply_gain, gain_q15, read_wav_header};

    fn wav(fmt_extra: &[u8], extra_chunk: bool) -> Vec<u8> {
        let mut v = b"RIFF\xff\xff\xff\xffWAVE".to_vec();
        if extra_chunk {
            v.extend_from_slice(b"LIST\x03\x00\x00\x00abc\x00");
        }
        let fmt_len = 16 + fmt_extra.len();
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(
            &u32::try_from(fmt_len)
                .expect("BUG: fmt length fits u32")
                .to_le_bytes(),
        );
        v.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        v.extend_from_slice(&2_u16.to_le_bytes());
        v.extend_from_slice(&44_100_u32.to_le_bytes());
        v.extend_from_slice(&(44_100_u32 * 4).to_le_bytes());
        v.extend_from_slice(&4_u16.to_le_bytes());
        v.extend_from_slice(&16_u16.to_le_bytes());
        v.extend_from_slice(fmt_extra);
        v.extend_from_slice(b"data\xff\xff\xff\xff");
        v.extend_from_slice(&[1, 2, 3, 4]);
        v
    }

    #[test]
    fn a_streamed_header_yields_the_format_and_stops_at_the_samples() {
        let bytes = wav(&[], false);
        let mut reader = bytes.as_slice();
        let format = read_wav_header(&mut reader).expect("BUG: a valid header");
        assert_eq!(
            format,
            PcmFormat {
                sample_rate: 44_100,
                channels: 2
            }
        );
        assert_eq!(format.bytes_per_second(), 176_400);
        assert_eq!(reader, [1, 2, 3, 4]);
    }

    #[test]
    fn unknown_chunks_and_a_longer_fmt_are_skipped() {
        let bytes = wav(&[0, 0], true);
        let mut reader = bytes.as_slice();
        read_wav_header(&mut reader).expect("BUG: extra chunks are skipped");
        assert_eq!(reader, [1, 2, 3, 4]);
    }

    #[test]
    fn anything_but_16_bit_pcm_wave_is_refused() {
        assert!(read_wav_header(&mut b"RIFX\0\0\0\0WAVE".as_slice()).is_err());
        let mut eight_bit = wav(&[], false);
        eight_bit[34] = 8;
        assert!(read_wav_header(&mut eight_bit.as_slice()).is_err());
        assert!(read_wav_header(&mut &wav(&[], false)[..20]).is_err());
    }

    #[test]
    fn volume_follows_a_square_curve() {
        assert_eq!(gain_q15(100), 32_768);
        assert_eq!(gain_q15(50), 8_192);
        assert_eq!(gain_q15(0), 0);
        assert_eq!(gain_q15(250), 32_768);
    }

    #[test]
    fn gain_scales_each_sample_and_full_volume_is_untouched() {
        let mut pcm = [0x00, 0x40, 0x00, 0xc0]; // 16384, -16384
        apply_gain(&mut pcm, gain_q15(50));
        assert_eq!(pcm, [0x00, 0x10, 0x00, 0xf0]); // 4096, -4096
        let mut loud = [0xff, 0x7f];
        apply_gain(&mut loud, gain_q15(100));
        assert_eq!(loud, [0xff, 0x7f]);
    }
}
