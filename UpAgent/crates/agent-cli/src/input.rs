use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use serde::de::DeserializeOwned;

const MAXIMUM_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn read_json<T: DeserializeOwned>(source: &str) -> Result<T, String> {
    let reader: Box<dyn Read> = if source == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(Path::new(source)).map_err(|error| error.to_string())?)
    };
    let bounded = BoundedReader::new(reader, MAXIMUM_REQUEST_BYTES);
    let mut deserializer = serde_json::Deserializer::from_reader(bounded);
    let value = T::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

pub(super) struct BoundedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> BoundedReader<R> {
    pub(super) const fn new(inner: R, maximum_bytes: u64) -> Self {
        Self {
            inner,
            remaining: maximum_bytes,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut extra = [0_u8; 1];
            return match self.inner.read(&mut extra)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("request exceeds the {MAXIMUM_REQUEST_BYTES} byte limit"),
                )),
            };
        }
        let buffer_length = u64::try_from(buffer.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "request buffer length overflowed",
            )
        })?;
        let allowed = usize::try_from(self.remaining.min(buffer_length)).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "request byte bound overflowed")
        })?;
        let count = self.inner.read(&mut buffer[..allowed])?;
        let count_u64 = u64::try_from(count).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "request read length overflowed")
        })?;
        self.remaining = self.remaining.saturating_sub(count_u64);
        Ok(count)
    }
}
