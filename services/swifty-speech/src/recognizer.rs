//! The speech model.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use sherpa_onnx::{LinearResampler, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig};

/// Anything that can turn audio into text. The decode worker depends on this
/// rather than on sherpa-onnx, so its failure handling can be tested.
pub trait Recognize: Send + 'static {
    fn recognize(&self, sample_rate: u32, samples: &[f32]) -> Result<String, &'static str>;
}

/// The model directory: the given one, or `model` beside the executable. The
/// default comes from the executable's own path, never the current directory.
pub fn resolve_model_directory(custom: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(directory) = custom {
        return Ok(directory.to_path_buf());
    }
    let executable = std::env::current_exe().map_err(|error| format!("cannot find this executable's path: {error}"))?;
    let parent = executable.parent().ok_or_else(|| format!("{} has no parent directory", executable.display()))?;
    Ok(parent.join("model"))
}

/// Paths of the four model files, checked to exist.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    encoder: String,
    decoder: String,
    joiner: String,
    tokens: String,
}

impl ModelFiles {
    pub fn locate(directory: &Path) -> Result<Self, String> {
        let file = |name: &str| -> Result<String, String> {
            let path = directory.join(name);
            if !path.is_file() {
                return Err(format!("model file {} is missing", path.display()));
            }
            path.into_os_string()
                .into_string()
                .map_err(|_| format!("model directory {} is not valid Unicode", directory.display()))
        };
        let files = Self {
            encoder: file("encoder.int8.onnx")?,
            decoder: file("decoder.int8.onnx")?,
            joiner: file("joiner.int8.onnx")?,
            tokens: file("tokens.txt")?,
        };
        check_token_list(Path::new(&files.tokens))?;
        Ok(files)
    }

    pub fn paths(&self) -> [&Path; 4] {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens].map(Path::new)
    }
}

/// Larger than any real token list, and small enough to read at startup.
const MAX_TOKEN_LIST_BYTES: u64 = 4 * 1024 * 1024;

/// sherpa-onnx ends the whole process on a token list it cannot parse, so no
/// error comes back to report. The list is checked here first.
fn check_token_list(path: &Path) -> Result<(), String> {
    let invalid = |why: String| format!("model file {} is not a valid token list: {why}", path.display());
    let size = fs::metadata(path).map_err(|error| invalid(error.to_string()))?.len();
    if size > MAX_TOKEN_LIST_BYTES {
        return Err(invalid("it is too large".to_string()));
    }
    let contents = fs::read(path).map_err(|error| invalid(error.to_string()))?;
    validate_token_list(&contents).map_err(invalid)
}

/// Accepts what the sherpa-onnx reader accepts: one `symbol id` per line, or
/// just `id` for the space symbol, with no symbol or id used twice.
fn validate_token_list(contents: &[u8]) -> Result<(), String> {
    if contents.is_empty() {
        return Err("it is empty".to_string());
    }
    let (mut symbols, mut ids) = (HashSet::new(), HashSet::new());
    for (index, line) in contents.strip_suffix(b"\n").unwrap_or(contents).split(|&byte| byte == b'\n').enumerate() {
        let number = index + 1;
        let fields: Vec<&[u8]> = line
            .split(|byte| matches!(byte, b' ' | b'\t' | b'\r' | 0x0B | 0x0C))
            .filter(|field| !field.is_empty())
            .collect();
        let (symbol, id) = match fields.as_slice() {
            [id] => (&b" "[..], *id),
            [symbol, id] => (*symbol, *id),
            _ => return Err(format!("line {number} does not hold a symbol and an id")),
        };
        let id: i32 = std::str::from_utf8(id)
            .ok()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| format!("line {number} has no numeric id"))?;
        if !symbols.insert(symbol) || !ids.insert(id) {
            return Err(format!("line {number} repeats a symbol or id"));
        }
    }
    Ok(())
}

const MODEL_SAMPLE_RATE: i32 = 16_000;

/// Clips shorter than this are padded with silence before decoding.
///
/// A clip too short to produce a feature frame makes ONNX Runtime throw a C++
/// exception. It crosses the sherpa-onnx C API, which Rust cannot catch, so the
/// process aborts. catch_unwind does not help: the clip must never get there.
const MIN_DECODE_MS: usize = 250;

/// The padded copy of `samples`, or `None` if it is long enough already.
fn pad_short_clip(samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
    let minimum = sample_rate as usize * MIN_DECODE_MS / 1000;
    if samples.len() >= minimum {
        return None;
    }
    let mut padded = samples.to_vec();
    padded.resize(minimum, 0.0);
    Some(padded)
}

/// Audio at the model's rate, or `None` if it is at that rate already. Done
/// here because the resampler inside accept_waveform writes a message to
/// stderr for every request. This one uses the same filter and is silent.
fn resample_for_model(samples: &[f32], sample_rate: i32) -> Result<Option<Vec<f32>>, &'static str> {
    if sample_rate == MODEL_SAMPLE_RATE {
        return Ok(None);
    }
    let resampler = LinearResampler::create(sample_rate, MODEL_SAMPLE_RATE).ok_or("cannot create the resampler")?;
    Ok(Some(resampler.resample(samples, true)))
}

pub struct SherpaRecognizer {
    inner: OfflineRecognizer,
}

impl SherpaRecognizer {
    pub fn load(files: &ModelFiles, threads: u16) -> Result<Self, String> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(files.encoder.clone()),
            decoder: Some(files.decoder.clone()),
            joiner: Some(files.joiner.clone()),
        };
        config.model_config.tokens = Some(files.tokens.clone());
        config.model_config.model_type = Some("nemo_transducer".to_string());
        config.model_config.num_threads = i32::from(threads);
        config.model_config.provider = Some("cpu".to_string());
        config.decoding_method = Some("greedy_search".to_string());

        OfflineRecognizer::create(&config)
            .map(|inner| Self { inner })
            .ok_or_else(|| "sherpa-onnx could not load the model".to_string())
    }
}

impl Recognize for SherpaRecognizer {
    fn recognize(&self, sample_rate: u32, samples: &[f32]) -> Result<String, &'static str> {
        let padded = pad_short_clip(samples, sample_rate);
        let clip = padded.as_deref().unwrap_or(samples);
        let sample_rate = i32::try_from(sample_rate).map_err(|_| "sample rate out of range")?;
        let resampled = resample_for_model(clip, sample_rate)?;

        let stream = self.inner.create_stream();
        stream.accept_waveform(MODEL_SAMPLE_RATE, resampled.as_deref().unwrap_or(clip));
        self.inner.decode(&stream);
        let result = stream.get_result().ok_or("the recognizer returned no result")?;
        Ok(result.text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_directory_is_beside_the_executable() {
        let directory = resolve_model_directory(None).unwrap();
        let executable = std::env::current_exe().unwrap();
        assert_eq!(directory, executable.parent().unwrap().join("model"));
        assert!(directory.is_absolute());
    }

    #[test]
    fn given_model_directory_is_used_as_is() {
        let given = Path::new(r"D:\some where\model");
        assert_eq!(resolve_model_directory(Some(given)).unwrap(), given);
    }

    #[test]
    fn short_clips_are_padded_with_silence() {
        let padded = pad_short_clip(&[0.5, -0.5], 16_000).unwrap();
        assert_eq!(padded.len(), 4_000);
        assert_eq!(&padded[..2], &[0.5, -0.5]);
        assert!(padded[2..].iter().all(|&s| s == 0.0));
        assert_eq!(pad_short_clip(&[0.1], 8_000).unwrap().len(), 2_000);
        assert_eq!(pad_short_clip(&[0.1], 48_000).unwrap().len(), 12_000);
    }

    #[test]
    fn long_enough_clips_are_not_copied() {
        assert!(pad_short_clip(&[0.0; 4_000], 16_000).is_none());
        assert!(pad_short_clip(&[0.0; 3_999], 16_000).is_some());
    }

    #[test]
    fn resampling_is_skipped_at_the_model_rate_and_changes_the_length_otherwise() {
        let second = vec![0.1_f32; 8_000];
        assert!(resample_for_model(&second, MODEL_SAMPLE_RATE).unwrap().is_none());
        let doubled = resample_for_model(&second, 8_000).unwrap().unwrap();
        assert!((15_900..=16_100).contains(&doubled.len()), "{} samples", doubled.len());
        let halved = resample_for_model(&vec![0.1_f32; 48_000], 48_000).unwrap().unwrap();
        assert!((15_900..=16_100).contains(&halved.len()), "{} samples", halved.len());
    }

    #[test]
    fn token_lists_are_checked_like_the_sherpa_reader() {
        assert!(validate_token_list(b"<unk> 0\n\xE2\x96\x81t 1\n<blk> 2\n").is_ok());
        assert!(validate_token_list(b"<unk> 0\r\na 1\r\n").is_ok());
        assert!(validate_token_list(b"<unk> 0\na 1").is_ok(), "no final newline");
        assert!(validate_token_list(b"0\na 1\n").is_ok(), "a lone id is the space symbol");

        let error = |text: &[u8]| validate_token_list(text).unwrap_err();
        assert!(error(b"").contains("empty"));
        assert!(error(b"a b c\n").contains("line 1"));
        assert!(error(b"a 0\nb one\n").contains("line 2"));
        assert!(error(b"a 0\n\nb 1\n").contains("line 2"));
        assert!(error(b"a 0\na 1\n").contains("repeats"));
        assert!(error(b"a 0\nb 0\n").contains("repeats"));
    }

    #[test]
    fn a_malformed_token_list_is_refused_before_loading() {
        let dir = std::env::temp_dir().join(format!("gs-tokens-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::write(dir.join("tokens.txt"), b"a b c\n").unwrap();
        let message = ModelFiles::locate(&dir).unwrap_err();
        assert!(message.contains("tokens.txt") && message.contains("line 1"), "{message}");
        assert!(!message.contains("a b c"));

        std::fs::write(dir.join("tokens.txt"), b"a 0\nb 1\n").unwrap();
        assert!(ModelFiles::locate(&dir).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_model_file_is_named() {
        let dir = std::env::temp_dir().join(format!("gs-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("encoder.int8.onnx"), b"x").unwrap();
        let message = ModelFiles::locate(&dir).unwrap_err();
        assert!(message.contains("decoder.int8.onnx"), "{message}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
