//! A defensive parser for RIFF/WAVE clips.
//!
//! Every length in the file is treated as hostile. Nothing is allocated from a
//! declared size: samples are only allocated after the data chunk has been
//! proven to lie inside the bytes actually received and the audio has passed
//! the duration limit. All offset arithmetic is checked.

pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 48_000;
pub const MAX_AUDIO_SECONDS: u64 = 30;

const FORMAT_PCM: u16 = 1;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const BYTES_PER_SAMPLE: usize = 2;
/// KSDATAFORMAT_SUBTYPE_PCM.
const PCM_SUBFORMAT: [u8; 16] =
    [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavError {
    /// The file is not a well-formed WAV file (HTTP 400).
    Malformed(&'static str),
    /// A valid WAV file in an encoding this service does not accept (HTTP 415).
    Unsupported(&'static str),
    /// Longer than the duration limit (HTTP 422).
    TooLong,
    /// No audio frames (HTTP 422).
    Empty,
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::Malformed(why) => write!(f, "malformed WAV: {why}"),
            WavError::Unsupported(why) => write!(f, "unsupported WAV: {why}"),
            WavError::TooLong => write!(f, "audio is longer than {MAX_AUDIO_SECONDS} seconds"),
            WavError::Empty => write!(f, "audio is empty"),
        }
    }
}

/// Decoded audio: one channel, samples in the range -1.0 to 1.0.
#[derive(Debug)]
pub struct Audio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub audio_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Format {
    channels: u16,
    sample_rate: u32,
}

/// Parse a complete WAV file held in memory.
pub fn parse(bytes: &[u8]) -> Result<Audio, WavError> {
    let (format, data) = locate_chunks(bytes)?;
    let frame_size = usize::from(format.channels) * BYTES_PER_SAMPLE;

    if data.len() % frame_size != 0 {
        return Err(WavError::Malformed("data chunk is not a whole number of frames"));
    }
    let frames = (data.len() / frame_size) as u64;
    if frames == 0 {
        return Err(WavError::Empty);
    }
    if frames > MAX_AUDIO_SECONDS * u64::from(format.sample_rate) {
        return Err(WavError::TooLong);
    }

    let mut samples = Vec::with_capacity(frames as usize);
    for frame in data.chunks_exact(frame_size) {
        let first = f32::from(i16::from_le_bytes([frame[0], frame[1]]));
        let mono = if format.channels == 2 {
            let second = f32::from(i16::from_le_bytes([frame[2], frame[3]]));
            (first + second) / 2.0
        } else {
            first
        };
        samples.push(mono / 32768.0);
    }

    Ok(Audio { sample_rate: format.sample_rate, samples, audio_ms: frames * 1000 / u64::from(format.sample_rate) })
}

/// Walk the RIFF chunks and return the format and the data bytes. The size in
/// the RIFF header itself is not used: only chunks that fit the received bytes
/// are ever read.
fn locate_chunks(bytes: &[u8]) -> Result<(Format, &[u8]), WavError> {
    let magic = bytes.get(..4).ok_or(WavError::Malformed("shorter than a RIFF header"))?;
    match magic {
        b"RIFF" => {}
        b"RIFX" | b"RF64" => return Err(WavError::Unsupported("only little-endian RIFF is accepted")),
        _ => return Err(WavError::Malformed("missing RIFF header")),
    }
    if bytes.len() < 12 {
        return Err(WavError::Malformed("shorter than a RIFF header"));
    }
    if bytes.get(8..12) != Some(b"WAVE".as_slice()) {
        return Err(WavError::Malformed("RIFF file is not WAVE"));
    }

    let mut position = 12_usize;
    let mut format = None;
    loop {
        let body_start = position.checked_add(8).ok_or(WavError::Malformed("chunk offset overflow"))?;
        let header = bytes.get(position..body_start).ok_or(WavError::Malformed("no data chunk"))?;
        let size = usize::try_from(u32::from_le_bytes([header[4], header[5], header[6], header[7]]))
            .map_err(|_| WavError::Malformed("chunk size overflow"))?;
        let body_end = body_start.checked_add(size).ok_or(WavError::Malformed("chunk size overflow"))?;
        let body =
            bytes.get(body_start..body_end).ok_or(WavError::Malformed("chunk extends past the end of the file"))?;

        match &header[..4] {
            b"fmt " => {
                if format.is_some() {
                    return Err(WavError::Malformed("more than one fmt chunk"));
                }
                format = Some(parse_format(body)?);
            }
            b"data" => {
                let format = format.ok_or(WavError::Malformed("data chunk before fmt chunk"))?;
                return Ok((format, body));
            }
            _ => {}
        }

        // Chunks are word aligned: an odd-sized chunk is followed by a pad byte.
        position = body_end.checked_add(size & 1).ok_or(WavError::Malformed("chunk offset overflow"))?;
    }
}

fn parse_format(body: &[u8]) -> Result<Format, WavError> {
    if body.len() < 16 {
        return Err(WavError::Malformed("fmt chunk is too small"));
    }
    let tag = u16::from_le_bytes([body[0], body[1]]);
    let channels = u16::from_le_bytes([body[2], body[3]]);
    let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let block_align = u16::from_le_bytes([body[12], body[13]]);
    let bits = u16::from_le_bytes([body[14], body[15]]);

    if channels == 0 {
        return Err(WavError::Malformed("zero channels"));
    }
    if sample_rate == 0 {
        return Err(WavError::Malformed("zero sample rate"));
    }

    let is_pcm = match tag {
        FORMAT_PCM => true,
        FORMAT_EXTENSIBLE => {
            let subformat = body.get(24..40).ok_or(WavError::Malformed("extensible fmt chunk is too small"))?;
            subformat == PCM_SUBFORMAT
        }
        _ => false,
    };
    if !is_pcm {
        return Err(WavError::Unsupported("only PCM audio is accepted"));
    }
    // After the format check: compressed formats such as MP3-in-WAV legitimately declare 0 bits.
    if bits == 0 {
        return Err(WavError::Malformed("zero bits per sample"));
    }
    if bits != 16 {
        return Err(WavError::Unsupported("only 16-bit samples are accepted"));
    }
    if channels > 2 {
        return Err(WavError::Unsupported("only mono or stereo audio is accepted"));
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(WavError::Unsupported("sample rate must be between 8 kHz and 48 kHz"));
    }
    if usize::from(block_align) != usize::from(channels) * BYTES_PER_SAMPLE {
        return Err(WavError::Malformed("block alignment does not match the format"));
    }

    Ok(Format { channels, sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_body(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let block = channels.wrapping_mul(bits / 8);
        let mut body = Vec::new();
        body.extend_from_slice(&tag.to_le_bytes());
        body.extend_from_slice(&channels.to_le_bytes());
        body.extend_from_slice(&rate.to_le_bytes());
        body.extend_from_slice(&rate.wrapping_mul(u32::from(block)).to_le_bytes());
        body.extend_from_slice(&block.to_le_bytes());
        body.extend_from_slice(&bits.to_le_bytes());
        body
    }

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let inner: Vec<u8> = chunks.concat();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((inner.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&inner);
        out
    }

    fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        riff(&[chunk(b"fmt ", &fmt_body(tag, channels, rate, bits)), chunk(b"data", data)])
    }

    fn pcm(channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        wav(FORMAT_PCM, channels, rate, 16, data)
    }

    fn samples(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn is_malformed(result: Result<Audio, WavError>) -> bool {
        matches!(result, Err(WavError::Malformed(_)))
    }

    fn is_unsupported(result: Result<Audio, WavError>) -> bool {
        matches!(result, Err(WavError::Unsupported(_)))
    }

    /// Overwrite the size field of the chunk `id` in `file`.
    fn patch_chunk_size(file: &mut [u8], id: &[u8; 4], size: u32) {
        let at = file.windows(4).skip(12).position(|w| w == id).unwrap() + 12;
        file[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
    }

    #[test]
    fn compressed_format_declaring_zero_bits_is_unsupported() {
        for tag in [0x55u16, 0x31] {
            let bytes = wav(tag, 1, 16_000, 0, &[1, 2, 3, 4]);
            assert!(matches!(parse(&bytes), Err(WavError::Unsupported(_))));
        }
    }

    #[test]
    fn pcm_declaring_zero_bits_is_malformed() {
        let bytes = wav(FORMAT_PCM, 1, 16_000, 0, &[1, 2, 3, 4]);
        assert!(matches!(parse(&bytes), Err(WavError::Malformed(_))));
    }

    #[test]
    fn parses_mono_pcm() {
        let audio = parse(&pcm(1, 16_000, &samples(&[0, 16384, -32768, 32767]))).unwrap();
        assert_eq!(audio.sample_rate, 16_000);
        assert_eq!(audio.samples.len(), 4);
        assert_eq!(audio.samples[0], 0.0);
        assert_eq!(audio.samples[1], 0.5);
        assert_eq!(audio.samples[2], -1.0);
        assert!(audio.samples[3] < 1.0 && audio.samples[3] > 0.999);
        assert_eq!(audio.audio_ms, 0);
    }

    #[test]
    fn mixes_stereo_to_mono() {
        let audio = parse(&pcm(2, 16_000, &samples(&[16384, 0, -16384, -16384]))).unwrap();
        assert_eq!(audio.samples, vec![0.25, -0.5]);
    }

    #[test]
    fn accepts_the_whole_sample_rate_range() {
        for rate in [8_000, 11_025, 16_000, 22_050, 44_100, 48_000] {
            let audio = parse(&pcm(1, rate, &samples(&[1; 64]))).unwrap();
            assert_eq!(audio.sample_rate, rate);
        }
    }

    #[test]
    fn computes_duration() {
        let audio = parse(&pcm(2, 8_000, &samples(&[0; 8_000]))).unwrap();
        assert_eq!(audio.audio_ms, 500);
        assert_eq!(audio.samples.len(), 4_000);
    }

    #[test]
    fn skips_unknown_chunks_and_honours_odd_padding() {
        // An odd chunk followed by its pad byte, then the real chunks.
        let file = riff(&[
            chunk(b"LIST", b"abc"),
            chunk(b"fmt ", &fmt_body(FORMAT_PCM, 1, 16_000, 16)),
            chunk(b"JUNK", b"x"),
            chunk(b"data", &samples(&[7, 8])),
        ]);
        assert_eq!(parse(&file).unwrap().samples.len(), 2);
    }

    #[test]
    fn accepts_extra_bytes_in_fmt_and_trailing_chunks() {
        let mut fmt = fmt_body(FORMAT_PCM, 1, 16_000, 16);
        fmt.extend_from_slice(&[0, 0]);
        let file = riff(&[chunk(b"fmt ", &fmt), chunk(b"data", &samples(&[5])), chunk(b"LIST", b"trailing")]);
        assert_eq!(parse(&file).unwrap().samples.len(), 1);
    }

    #[test]
    fn ignores_a_wrong_riff_size() {
        for size in [0_u32, 1, u32::MAX] {
            let mut file = pcm(1, 16_000, &samples(&[1, 2]));
            file[4..8].copy_from_slice(&size.to_le_bytes());
            assert_eq!(parse(&file).unwrap().samples.len(), 2);
        }
    }

    #[test]
    fn accepts_extensible_pcm_and_refuses_extensible_float() {
        let extensible = |subformat: [u8; 16]| {
            let mut fmt = fmt_body(FORMAT_EXTENSIBLE, 1, 16_000, 16);
            fmt.extend_from_slice(&22_u16.to_le_bytes());
            fmt.extend_from_slice(&16_u16.to_le_bytes());
            fmt.extend_from_slice(&4_u32.to_le_bytes());
            fmt.extend_from_slice(&subformat);
            riff(&[chunk(b"fmt ", &fmt), chunk(b"data", &samples(&[1, 2]))])
        };
        assert_eq!(parse(&extensible(PCM_SUBFORMAT)).unwrap().samples.len(), 2);
        let mut float = PCM_SUBFORMAT;
        float[0] = 3;
        assert!(is_unsupported(parse(&extensible(float))));
        // A truncated extensible header is malformed, not an out-of-bounds read.
        let short =
            riff(&[chunk(b"fmt ", &fmt_body(FORMAT_EXTENSIBLE, 1, 16_000, 16)), chunk(b"data", &samples(&[1]))]);
        assert!(is_malformed(parse(&short)));
    }

    #[test]
    fn refuses_bad_containers() {
        assert!(is_malformed(parse(b"")));
        assert!(is_malformed(parse(b"RI")));
        assert!(is_malformed(parse(b"RIFF")));
        assert!(is_malformed(parse(b"RIFF\0\0\0\0")));
        assert!(is_malformed(parse(b"RIFF\0\0\0\0WAV")));
        assert!(is_malformed(parse(b"this is definitely not a wav file")));
        assert!(is_malformed(parse(b"RIFF\x04\0\0\0AVI ")));
        assert!(is_malformed(parse(b"RIFF\x04\0\0\0WAVE")));
        assert!(is_unsupported(parse(b"RIFX\0\0\0\0WAVE")));
        assert!(is_unsupported(parse(b"RF64\0\0\0\0WAVE")));
    }

    #[test]
    fn refuses_missing_or_misordered_chunks() {
        assert!(is_malformed(parse(&riff(&[chunk(b"data", &samples(&[1]))]))));
        assert!(is_malformed(parse(&riff(&[chunk(b"fmt ", &fmt_body(1, 1, 16_000, 16))]))));
        assert!(is_malformed(parse(&riff(&[
            chunk(b"data", &samples(&[1])),
            chunk(b"fmt ", &fmt_body(1, 1, 16_000, 16)),
        ]))));
        assert!(is_malformed(parse(&riff(&[
            chunk(b"fmt ", &fmt_body(1, 1, 16_000, 16)),
            chunk(b"fmt ", &fmt_body(1, 1, 16_000, 16)),
            chunk(b"data", &samples(&[1])),
        ]))));
    }

    #[test]
    fn refuses_a_short_fmt_chunk() {
        for length in [0, 1, 15] {
            let file = riff(&[chunk(b"fmt ", &vec![1; length]), chunk(b"data", &samples(&[1]))]);
            assert!(is_malformed(parse(&file)), "length {length}");
        }
    }

    #[test]
    fn refuses_zero_fields() {
        assert!(is_malformed(parse(&pcm(0, 16_000, &samples(&[1])))));
        assert!(is_malformed(parse(&pcm(1, 0, &samples(&[1])))));
        assert!(is_malformed(parse(&wav(FORMAT_PCM, 1, 16_000, 0, &samples(&[1])))));
        // Zero fields are malformed whatever the format tag says.
        assert!(is_malformed(parse(&wav(3, 0, 16_000, 32, &samples(&[1])))));
    }

    #[test]
    fn refuses_a_wrong_block_alignment() {
        let mut fmt = fmt_body(FORMAT_PCM, 1, 16_000, 16);
        fmt[12] = 3;
        let file = riff(&[chunk(b"fmt ", &fmt), chunk(b"data", &samples(&[1]))]);
        assert!(is_malformed(parse(&file)));
    }

    #[test]
    fn refuses_unsupported_encodings() {
        let data = samples(&[1; 8]);
        assert!(is_unsupported(parse(&wav(3, 1, 16_000, 32, &data))), "float");
        assert!(is_unsupported(parse(&wav(6, 1, 8_000, 8, &data))), "a-law");
        assert!(is_unsupported(parse(&wav(7, 1, 8_000, 8, &data))), "mu-law");
        assert!(is_unsupported(parse(&wav(0x55, 1, 16_000, 16, &data))), "mp3");
        assert!(is_unsupported(parse(&wav(FORMAT_PCM, 1, 16_000, 8, &data))), "8 bit");
        assert!(is_unsupported(parse(&wav(FORMAT_PCM, 1, 16_000, 24, &data))), "24 bit");
        assert!(is_unsupported(parse(&wav(FORMAT_PCM, 1, 16_000, 32, &data))), "32 bit");
        assert!(is_unsupported(parse(&wav(FORMAT_PCM, 3, 16_000, 16, &data))), "3 channels");
        assert!(is_unsupported(parse(&wav(FORMAT_PCM, u16::MAX, 16_000, 16, &data))), "65535 channels");
        assert!(is_unsupported(parse(&pcm(1, 7_999, &data))), "below range");
        assert!(is_unsupported(parse(&pcm(1, 48_001, &data))), "above range");
        assert!(is_unsupported(parse(&pcm(1, u32::MAX, &data))), "max rate");
    }

    #[test]
    fn refuses_declared_sizes_past_the_end() {
        let base = pcm(1, 16_000, &samples(&[1, 2, 3, 4]));
        for size in [9, 1024, 0x7FFF_FFFF, u32::MAX - 1, u32::MAX] {
            let mut file = base.clone();
            patch_chunk_size(&mut file, b"data", size);
            assert!(is_malformed(parse(&file)), "data size {size}");
            let mut file = base.clone();
            patch_chunk_size(&mut file, b"fmt ", size);
            assert!(is_malformed(parse(&file)), "fmt size {size}");
        }
        let mut file =
            riff(&[chunk(b"JUNK", b"zz"), chunk(b"fmt ", &fmt_body(1, 1, 16_000, 16)), chunk(b"data", &samples(&[1]))]);
        patch_chunk_size(&mut file, b"JUNK", u32::MAX);
        assert!(is_malformed(parse(&file)));
    }

    #[test]
    fn refuses_truncated_files_at_every_length() {
        let file = pcm(2, 44_100, &samples(&[1, 2, 3, 4, 5, 6]));
        for length in 0..file.len() {
            assert!(parse(&file[..length]).is_err(), "length {length}");
        }
        assert!(parse(&file).is_ok());
    }

    #[test]
    fn refuses_data_that_is_not_whole_frames() {
        assert!(is_malformed(parse(&pcm(1, 16_000, &[1, 2, 3]))));
        assert!(is_malformed(parse(&pcm(2, 16_000, &[1, 2]))));
        assert!(is_malformed(parse(&pcm(2, 16_000, &[1, 2, 3, 4, 5, 6]))));
    }

    #[test]
    fn empty_data_is_reported_as_empty() {
        assert_eq!(parse(&pcm(1, 16_000, &[])).unwrap_err(), WavError::Empty);
        assert_eq!(parse(&pcm(2, 48_000, &[])).unwrap_err(), WavError::Empty);
    }

    #[test]
    fn enforces_the_duration_limit_exactly() {
        let rate = 8_000_u32;
        let limit = (MAX_AUDIO_SECONDS * u64::from(rate)) as usize;
        let exact = pcm(1, rate, &vec![0; limit * 2]);
        assert_eq!(parse(&exact).unwrap().audio_ms, 30_000);
        let over = pcm(1, rate, &vec![0; (limit + 1) * 2]);
        assert_eq!(parse(&over).unwrap_err(), WavError::TooLong);
        let stereo_over = pcm(2, rate, &vec![0; (limit + 1) * 4]);
        assert_eq!(parse(&stereo_over).unwrap_err(), WavError::TooLong);
    }

    /// Small deterministic generator, so a failure can be reproduced.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }

    #[test]
    fn survives_random_bytes() {
        let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..50_000 {
            let length = random.below(128);
            let mut bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
            if length >= 12 && random.below(2) == 0 {
                bytes[..4].copy_from_slice(b"RIFF");
                bytes[8..12].copy_from_slice(b"WAVE");
            }
            let _ = parse(&bytes);
        }
    }

    #[test]
    fn survives_mutations_of_a_valid_file() {
        let valid = riff(&[
            chunk(b"LIST", b"odd"),
            chunk(b"fmt ", &fmt_body(FORMAT_PCM, 2, 22_050, 16)),
            chunk(b"data", &samples(&[100; 64])),
        ]);
        let extreme = [0_u8, 1, 0x7F, 0x80, 0xFF];
        let mut random = XorShift(0xDEAD_BEEF_CAFE_F00D);
        for _ in 0..200_000 {
            let mut file = valid.clone();
            for _ in 0..=random.below(6) {
                let at = random.below(file.len());
                file[at] =
                    if random.below(2) == 0 { extreme[random.below(extreme.len())] } else { random.next() as u8 };
            }
            if random.below(4) == 0 {
                file.truncate(random.below(file.len() + 1));
            }
            // A mutated file that still parses must stay within what was sent.
            if let Ok(audio) = parse(&file) {
                assert!(audio.samples.len() <= file.len() / 2);
            }
        }
    }
}
