//! gzip 壓縮的 NBT 讀寫。
//!
//! `.litematic` 與 Sponge `.schem` 都是 gzip 過的 NBT。`fastnbt` 本身
//! **不處理 gzip**，所以壓縮層在這裡自己接。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nbt error: {0}")]
    Nbt(String),

    #[error("unsupported schematic version: {0}")]
    UnsupportedVersion(i32),

    #[error("missing required field: {0}")]
    MissingField(String),
}

/// 讀取一個 gzip 壓縮的 NBT 檔案並反序列化。
pub fn read_gzip_nbt<T: DeserializeOwned>(path: &Path) -> Result<T, FormatError> {
    let file = File::open(path)?;
    from_gzip_reader(file)
}

/// 從已在記憶體裡的 gzip NBT bytes 反序列化——瀏覽器 (wasm) 沒有檔案
/// 系統,fetch 回來的就是這個。
pub fn from_gzip_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, FormatError> {
    from_gzip_reader(bytes)
}

fn from_gzip_reader<T: DeserializeOwned, R: Read>(reader: R) -> Result<T, FormatError> {
    let mut decoder = GzDecoder::new(reader);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;
    fastnbt::from_bytes(&bytes).map_err(|e| FormatError::Nbt(e.to_string()))
}

/// 序列化並寫出成 gzip 壓縮的 NBT 檔案。
pub fn write_gzip_nbt<T: Serialize>(path: &Path, value: &T) -> Result<(), FormatError> {
    let bytes = fastnbt::to_bytes(value).map_err(|e| FormatError::Nbt(e.to_string()))?;
    let file = File::create(path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&bytes)?;
    encoder.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        name: String,
        count: i32,
    }

    #[test]
    fn gzip_nbt_roundtrips_through_a_temp_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("reda_nbt_roundtrip_test.nbt");

        let original = Sample {
            name: "test".to_string(),
            count: 42,
        };
        write_gzip_nbt(&path, &original).expect("write must succeed");

        let loaded: Sample = read_gzip_nbt(&path).expect("read must succeed");
        assert_eq!(loaded, original);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reading_a_missing_file_is_an_io_error() {
        let path = std::path::Path::new("/definitely/does/not/exist.nbt");
        let result: Result<Sample, _> = read_gzip_nbt(path);
        assert!(matches!(result, Err(FormatError::Io(_))));
    }
}
