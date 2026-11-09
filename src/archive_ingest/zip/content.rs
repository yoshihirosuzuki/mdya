//! Reads an entry's content on the second pass.

use std::io::Read;

use flate2::Crc;
use flate2::bufread::DeflateDecoder;

use super::header::Header;
use super::source::Source;
use crate::types::BoxError;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATED: u16 = 8;

/// The entry's content, checked against what the directory says about it.
pub(super) fn read_content(
    source: &mut Source,
    header: &Header,
    data_start: u64,
    max_content: u64,
) -> Result<Vec<u8>, BoxError> {
    if header.encrypted {
        return Err("the entry is encrypted".into());
    }
    let data = source.section(data_start, header.compressed)?;
    let content = match header.method {
        METHOD_STORED => read_limited(data, max_content)?,
        METHOD_DEFLATED => read_limited(DeflateDecoder::new(data), max_content)?,
        method => return Err(format!("compression method {method} is not supported").into()),
    };
    check_against_header(&content, header)?;
    Ok(content)
}

/// Counts while reading, because the sizes in the directory can be false.
fn read_limited(reader: impl Read, max_content: u64) -> Result<Vec<u8>, BoxError> {
    let mut content = Vec::new();
    reader
        .take(max_content.saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() as u64 > max_content {
        return Err("the entry's content is larger than the limit".into());
    }
    Ok(content)
}

fn check_against_header(content: &[u8], header: &Header) -> Result<(), BoxError> {
    if content.len() as u64 != header.uncompressed {
        return Err("the entry's size differs from the central directory".into());
    }
    let mut crc = Crc::new();
    crc.update(content);
    if crc.sum() != header.crc32 {
        return Err("the entry's CRC-32 differs from the central directory".into());
    }
    Ok(())
}
