//! The on-disk envelope around the byte-preserved campaign XML.
//!
//! RC8 writes `campaign.zip` with Java's `ZipOutputStream`: one DEFLATE
//! member named `campaign.xml`, optionally followed by a data descriptor.
//! The archive is never extracted to the filesystem.

use crate::error::{CoreError, ErrorCode, Result};
use crate::xml::{XmlDocument, XmlLimits};
use flate2::{Decompress, FlushDecompress, Status};
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use std::ops::Range;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MEMBER_NAME: &str = "campaign.xml";
const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_DIRECTORY: u32 = 0x0605_4b50;
const DATA_DESCRIPTOR: u32 = 0x0807_4b50;

#[derive(Debug, Clone)]
pub(crate) enum CampaignEncoding {
    Xml,
    Zip {
        member_name: String,
        compression: CompressionMethod,
    },
}

pub(crate) fn campaign_file_name(compressed: bool) -> &'static str {
    if compressed {
        "campaign.zip"
    } else {
        "campaign.xml"
    }
}

pub(crate) fn decode_campaign(
    bytes: Vec<u8>,
    compressed: bool,
    limits: XmlLimits,
) -> Result<(XmlDocument, CampaignEncoding)> {
    if !compressed {
        return XmlDocument::parse(bytes, limits).map(|xml| (xml, CampaignEncoding::Xml));
    }
    check_size(bytes.len() as u64, limits.max_bytes, "campaign archive")?;
    let layout = validate_zip_layout(&bytes)?;
    check_size(layout.uncompressed_size, limits.max_bytes, "campaign XML")?;

    // The general-purpose ZIP reader intentionally ignores local headers and
    // deduplicates member names. Validate our single-member framing first, then
    // use its metadata parser to reject invalid extra fields and special files.
    let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(zip_error)?;
    if archive.len() != 1 || archive.offset() != 0 {
        return Err(CoreError::ambiguous("expected one campaign ZIP member"));
    }
    let member = archive.by_index_raw(0).map_err(zip_error)?;
    if member.compressed_size() != layout.data.len() as u64
        || member.size() != layout.uncompressed_size
        || member.crc32() != layout.crc32
        || member.data_start() != Some(layout.data.start as u64)
    {
        return Err(CoreError::validation(
            "campaign ZIP metadata disagrees with member framing",
        ));
    }
    if member.name_raw() != MEMBER_NAME.as_bytes() || member.name() != MEMBER_NAME {
        return Err(unsupported(
            "campaign ZIP member must be named campaign.xml",
        ));
    }
    if !member.is_file()
        || member
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000))
    {
        return Err(unsupported("campaign ZIP member must be a regular file"));
    }
    if member.encrypted() {
        return Err(unsupported(
            "encrypted campaign ZIP archives are unsupported",
        ));
    }
    let encoding = CampaignEncoding::Zip {
        member_name: member.name().to_owned(),
        compression: member.compression(),
    };
    let data = &bytes[layout.data];
    let xml = match member.compression() {
        CompressionMethod::Stored => {
            check_size(data.len() as u64, limits.max_bytes, "campaign XML")?;
            data.to_vec()
        }
        CompressionMethod::Deflated => inflate_campaign(data, limits.max_bytes)?,
        _ => return Err(unsupported("unsupported campaign ZIP compression method")),
    };
    if xml.len() as u64 != layout.uncompressed_size {
        return Err(CoreError::validation(
            "campaign ZIP uncompressed size mismatch",
        ));
    }
    if crc32fast::hash(&xml) != layout.crc32 {
        return Err(CoreError::validation("campaign ZIP checksum mismatch"));
    }
    XmlDocument::parse(xml, limits).map(|xml| (xml, encoding))
}

pub(crate) fn encode_campaign(
    xml: &[u8],
    encoding: &CampaignEncoding,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    check_size(xml.len() as u64, max_bytes, "campaign XML")?;
    let CampaignEncoding::Zip {
        member_name,
        compression,
    } = encoding
    else {
        return Ok(xml.to_vec());
    };
    if member_name != MEMBER_NAME
        || !matches!(
            compression,
            CompressionMethod::Stored | CompressionMethod::Deflated
        )
    {
        return Err(unsupported(
            "unsupported campaign ZIP member or compression method",
        ));
    }
    let output = BoundedCursor {
        inner: Cursor::new(Vec::new()),
        max_bytes,
    };
    let mut archive = ZipWriter::new(output);
    let mut options = SimpleFileOptions::default().compression_method(*compression);
    if *compression == CompressionMethod::Deflated {
        // RC8's ordinary save path uses level 1 to favor save speed.
        options = options.compression_level(Some(1));
    }
    archive
        .start_file(member_name, options)
        .map_err(zip_error)?;
    archive.write_all(xml).map_err(archive_io_error)?;
    let output = archive.finish().map_err(zip_error)?;
    Ok(output.inner.into_inner())
}

fn inflate_campaign(data: &[u8], max_bytes: u64) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(false);
    let mut xml = Vec::new();
    let mut buffer = [0_u8; 32 * 1024];
    let mut consumed = 0;
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        // Always allow one byte beyond the limit to detect a dishonest size
        // without allocating from untrusted archive metadata.
        let capacity = max_bytes
            .saturating_sub(xml.len() as u64)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let status = decoder
            .decompress(
                &data[consumed..],
                &mut buffer[..capacity],
                FlushDecompress::None,
            )
            .map_err(|_| CoreError::validation("invalid campaign DEFLATE stream"))?;
        let read = (decoder.total_in() - before_in) as usize;
        let written = (decoder.total_out() - before_out) as usize;
        check_size(decoder.total_out(), max_bytes, "campaign XML")?;
        xml.extend_from_slice(&buffer[..written]);
        consumed += read;
        if status == Status::StreamEnd {
            if consumed != data.len() {
                return Err(CoreError::validation(
                    "extra data after campaign DEFLATE stream",
                ));
            }
            return Ok(xml);
        }
        if read == 0 && written == 0 {
            return Err(CoreError::validation("truncated campaign DEFLATE stream"));
        }
    }
}

struct ZipLayout {
    data: Range<usize>,
    uncompressed_size: u64,
    crc32: u32,
}

/// Accept the ordinary ZIP32 envelope produced by the game, including Java's
/// streaming data descriptor. Every byte must belong to its one member,
/// central directory, or footer/comment; unrelated archives fail closed.
fn validate_zip_layout(bytes: &[u8]) -> Result<ZipLayout> {
    let earliest_footer = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let footer = (earliest_footer..bytes.len().saturating_sub(21))
        .rev()
        .find(|&offset| {
            read_u32(bytes, offset).ok() == Some(END_OF_DIRECTORY)
                && read_u16(bytes, offset + 20)
                    .is_ok_and(|size| offset + 22 + usize::from(size) == bytes.len())
        })
        .ok_or_else(|| CoreError::validation("missing or truncated campaign ZIP footer"))?;
    if read_u16(bytes, footer + 4)? != 0 || read_u16(bytes, footer + 6)? != 0 {
        return Err(unsupported(
            "multi-disk campaign ZIP archives are unsupported",
        ));
    }
    if read_u16(bytes, footer + 8)? != 1 || read_u16(bytes, footer + 10)? != 1 {
        return Err(CoreError::ambiguous(
            "expected exactly one campaign ZIP member",
        ));
    }
    let directory_size = zip32_size(read_u32(bytes, footer + 12)?)?;
    let directory_offset = zip32_size(read_u32(bytes, footer + 16)?)?;
    let directory = section(bytes, directory_offset, directory_size)?;
    if directory_offset.checked_add(directory_size) != Some(footer)
        || read_u32(directory, 0)? != CENTRAL_HEADER
    {
        return Err(CoreError::validation(
            "invalid campaign ZIP central directory",
        ));
    }
    let name_size = usize::from(read_u16(directory, 28)?);
    let extra_size = usize::from(read_u16(directory, 30)?);
    let comment_size = usize::from(read_u16(directory, 32)?);
    if 46 + name_size + extra_size + comment_size != directory.len()
        || read_u16(directory, 34)? != 0
        || read_u32(directory, 42)? != 0
    {
        return Err(CoreError::validation(
            "unexpected data in campaign ZIP directory",
        ));
    }
    if section(directory, 46, name_size)? != MEMBER_NAME.as_bytes() {
        return Err(unsupported(
            "campaign ZIP member must be named campaign.xml",
        ));
    }
    let flags = read_u16(directory, 8)?;
    if flags & 0x2041 != 0 {
        return Err(unsupported(
            "encrypted campaign ZIP archives are unsupported",
        ));
    }
    if flags & !0x080e != 0 {
        return Err(unsupported("unsupported campaign ZIP flags"));
    }
    let method = read_u16(directory, 10)?;
    if !matches!(method, 0 | 8) {
        return Err(unsupported("unsupported campaign ZIP compression method"));
    }
    let crc32 = read_u32(directory, 16)?;
    let compressed_size = zip32_size(read_u32(directory, 20)?)?;
    let uncompressed_size = u64::from(read_u32(directory, 24)?);
    if uncompressed_size == u64::from(u32::MAX) {
        return Err(unsupported("ZIP64 campaign archives are unsupported"));
    }

    if read_u32(bytes, 0)? != LOCAL_HEADER
        || read_u16(bytes, 6)? != flags
        || read_u16(bytes, 8)? != method
        || usize::from(read_u16(bytes, 26)?) != name_size
        || section(bytes, 30, name_size)? != MEMBER_NAME.as_bytes()
    {
        return Err(CoreError::validation(
            "campaign ZIP local and central headers disagree",
        ));
    }
    let data_start = 30 + name_size + usize::from(read_u16(bytes, 28)?);
    let data_end = data_start
        .checked_add(compressed_size)
        .filter(|&end| end <= directory_offset)
        .ok_or_else(|| CoreError::validation("truncated campaign ZIP member"))?;
    let expected = [crc32, compressed_size as u32, uncompressed_size as u32];
    let descriptor = flags & 8 != 0;
    for (index, expected) in expected.into_iter().enumerate() {
        let local = read_u32(bytes, 14 + index * 4)?;
        if local != expected && !(descriptor && local == 0) {
            return Err(CoreError::validation(
                "campaign ZIP local sizes or checksum disagree",
            ));
        }
    }
    if descriptor {
        let descriptor_bytes = section(bytes, data_end, directory_offset - data_end)?;
        let offset = match descriptor_bytes.len() {
            12 => 0,
            16 if read_u32(descriptor_bytes, 0)? == DATA_DESCRIPTOR => 4,
            _ => {
                return Err(CoreError::validation(
                    "invalid campaign ZIP data descriptor",
                ))
            }
        };
        for (index, expected) in expected.into_iter().enumerate() {
            if read_u32(descriptor_bytes, offset + index * 4)? != expected {
                return Err(CoreError::validation(
                    "campaign ZIP data descriptor disagrees",
                ));
            }
        }
    } else if data_end != directory_offset {
        return Err(CoreError::validation(
            "unexpected data after campaign ZIP member",
        ));
    }
    Ok(ZipLayout {
        data: data_start..data_end,
        uncompressed_size,
        crc32,
    })
}

fn section(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| CoreError::validation("truncated campaign ZIP header"))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let data = section(bytes, offset, 2)?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let data = section(bytes, offset, 4)?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

fn zip32_size(value: u32) -> Result<usize> {
    if value == u32::MAX {
        return Err(unsupported("ZIP64 campaign archives are unsupported"));
    }
    usize::try_from(value)
        .map_err(|_| CoreError::new(ErrorCode::ResourceLimit, "campaign ZIP offset overflow"))
}

fn check_size(size: u64, max_bytes: u64, kind: &str) -> Result<()> {
    if size > max_bytes {
        return Err(CoreError::new(
            ErrorCode::ResourceLimit,
            format!("{kind} exceeds {max_bytes} bytes"),
        ));
    }
    Ok(())
}

fn unsupported(message: &str) -> CoreError {
    CoreError::new(ErrorCode::UnsupportedCompression, message)
}

fn zip_error(error: ZipError) -> CoreError {
    match error {
        ZipError::Io(error) => archive_io_error(error),
        ZipError::UnsupportedArchive(_)
        | ZipError::InvalidPassword
        | ZipError::CompressionMethodNotSupported(_) => unsupported(&error.to_string()),
        _ => CoreError::validation(format!("invalid campaign ZIP archive: {error}")),
    }
}

fn archive_io_error(error: io::Error) -> CoreError {
    if error.kind() == io::ErrorKind::FileTooLarge {
        CoreError::new(
            ErrorCode::ResourceLimit,
            "campaign archive exceeds byte limit",
        )
    } else {
        CoreError::validation(format!("invalid campaign ZIP archive: {error}"))
    }
}

struct BoundedCursor {
    inner: Cursor<Vec<u8>>,
    max_bytes: u64,
}

impl Write for BoundedCursor {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .inner
            .position()
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.max_bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "campaign archive exceeds byte limit",
            ));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for BoundedCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const JAVA_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<Campaign id=\"synthetic\"><name>Renée &amp; Ludd</name><value>17</value></Campaign>\r\n";

    // Independently generated from JAVA_XML using Java 17 ZipOutputStream,
    // setLevel(1), new ZipEntry("campaign.xml"), and UTC entry time 2000-01-01.
    // This covers Java's UTF-8 flag and signed streaming data descriptor.
    // Contains only synthetic XML, with no game or user data.
    fn java_zip() -> Vec<u8> {
        hex::decode(concat!(
            "504b0304140008080800000021280000000000000000000000000c0000006361",
            "6d706169676e2e786d6cb3b1afc8cd51284b2d2acecccfb35532d433505248cd",
            "4bce4fc9cc4bb7550a0d71d3b550b2b7e3e5b2714ecc2d48cc4ccf53c84cb155",
            "2aaecc2bc9482dc94c56b2b3c94bcc4db50b4acd3bbc3255410da8c85ac1a734",
            "25c5461f2c6e539698539a6a67686ea30f61d9e8c30c021a0a00504b07088411",
            "e2a2700000007d000000504b01021400140008080800000021288411e2a27000",
            "00007d0000000c000000000000000000000000000000000063616d706169676e",
            "2e786d6c504b050600000000010001003a000000aa0000000000",
        ))
        .unwrap()
    }

    fn encoding(compression: CompressionMethod) -> CampaignEncoding {
        CampaignEncoding::Zip {
            member_name: MEMBER_NAME.to_owned(),
            compression,
        }
    }

    fn decode(bytes: Vec<u8>) -> Result<(XmlDocument, CampaignEncoding)> {
        decode_campaign(bytes, true, XmlLimits::default())
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn directory_offset(bytes: &[u8]) -> usize {
        read_u32(bytes, bytes.len() - 6).unwrap() as usize
    }

    // A separate, minimal ZIP encoder for stored test members. Deliberately
    // does not call our encoder or ZipWriter and permits duplicate entries.
    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut directory = Vec::new();
        for (name, data) in entries {
            let start = bytes.len() as u32;
            let size = data.len() as u32;
            let crc = crc32fast::hash(data);
            let mut local = [0_u8; 30];
            put_u32(&mut local, 0, LOCAL_HEADER);
            put_u16(&mut local, 4, 20);
            put_u16(&mut local, 12, 0x2821);
            put_u32(&mut local, 14, crc);
            put_u32(&mut local, 18, size);
            put_u32(&mut local, 22, size);
            put_u16(&mut local, 26, name.len() as u16);
            bytes.extend_from_slice(&local);
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(data);
            let mut central = [0_u8; 46];
            put_u32(&mut central, 0, CENTRAL_HEADER);
            put_u16(&mut central, 4, 20);
            put_u16(&mut central, 6, 20);
            put_u16(&mut central, 14, 0x2821);
            put_u32(&mut central, 16, crc);
            put_u32(&mut central, 20, size);
            put_u32(&mut central, 24, size);
            put_u16(&mut central, 28, name.len() as u16);
            put_u32(&mut central, 42, start);
            directory.extend_from_slice(&central);
            directory.extend_from_slice(name.as_bytes());
        }
        let mut footer = [0_u8; 22];
        put_u32(&mut footer, 0, END_OF_DIRECTORY);
        put_u16(&mut footer, 8, entries.len() as u16);
        put_u16(&mut footer, 10, entries.len() as u16);
        put_u32(&mut footer, 12, directory.len() as u32);
        put_u32(&mut footer, 16, bytes.len() as u32);
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(&footer);
        bytes
    }

    #[test]
    fn plain_campaign_is_byte_preserved_and_uses_descriptor_encoding() {
        let bytes = JAVA_XML.as_bytes().to_vec();
        let (xml, encoding) = decode_campaign(bytes.clone(), false, XmlLimits::default()).unwrap();
        assert!(matches!(encoding, CampaignEncoding::Xml));
        assert_eq!(
            encode_campaign(xml.bytes(), &encoding, 4096).unwrap(),
            bytes
        );
        assert!(decode(bytes).is_err());
        assert!(decode_campaign(java_zip(), false, XmlLimits::default()).is_err());
        assert_eq!(campaign_file_name(false), "campaign.xml");
        assert_eq!(campaign_file_name(true), "campaign.zip");
    }

    #[test]
    fn java_streaming_archive_preserves_exact_xml_and_roundtrips() {
        let (xml, encoding) = decode(java_zip()).unwrap();
        assert_eq!(xml.bytes(), JAVA_XML.as_bytes());
        assert!(matches!(
            encoding,
            CampaignEncoding::Zip {
                compression: CompressionMethod::Deflated,
                ..
            }
        ));
        let rewritten = encode_campaign(xml.bytes(), &encoding, 4096).unwrap();
        let (decoded, _) = decode(rewritten.clone()).unwrap();
        assert_eq!(decoded.bytes(), JAVA_XML.as_bytes());
        let mut archive = ZipArchive::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(archive.len(), 1);
        let mut member = archive.by_index(0).unwrap();
        assert_eq!(member.name(), MEMBER_NAME);
        assert_eq!(member.compression(), CompressionMethod::Deflated);
        let mut independently_read = Vec::new();
        member.read_to_end(&mut independently_read).unwrap();
        assert_eq!(independently_read, JAVA_XML.as_bytes());
    }

    #[test]
    fn stored_archive_preserves_member_and_method() {
        let original = stored_zip(&[(MEMBER_NAME, JAVA_XML.as_bytes())]);
        let (xml, encoding) = decode(original).unwrap();
        assert_eq!(xml.bytes(), JAVA_XML.as_bytes());
        assert!(matches!(
            encoding,
            CampaignEncoding::Zip {
                compression: CompressionMethod::Stored,
                ..
            }
        ));
        let encoded = encode_campaign(xml.bytes(), &encoding, 4096).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(encoded)).unwrap();
        let member = archive.by_index(0).unwrap();
        assert_eq!(member.name(), MEMBER_NAME);
        assert_eq!(member.compression(), CompressionMethod::Stored);
    }

    #[test]
    fn unsigned_streaming_data_descriptor_is_supported() {
        let mut bytes = java_zip();
        bytes.drain(154..158);
        let footer = bytes.len() - 22;
        put_u32(&mut bytes, footer + 16, 166);
        assert_eq!(decode(bytes).unwrap().0.bytes(), JAVA_XML.as_bytes());
    }

    #[test]
    fn every_archive_truncation_fails() {
        let bytes = java_zip();
        for length in 0..bytes.len() {
            assert!(
                decode(bytes[..length].to_vec()).is_err(),
                "accepted {length} bytes"
            );
        }
    }

    #[test]
    fn checksum_headers_and_data_descriptor_are_validated() {
        let mut bytes = java_zip();
        // Make the descriptor and central CRC agree, but differ from the XML.
        put_u32(&mut bytes, 158, 1);
        put_u32(&mut bytes, 186, 1);
        let error = decode(bytes).unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
        assert!(error.message.contains("checksum"));

        for offset in [0, 6, 8, 14, 18, 22, 26, 30, 154, 158, 162, 166] {
            let mut bytes = java_zip();
            bytes[offset] ^= 1;
            assert!(
                decode(bytes).is_err(),
                "accepted corrupt header at {offset}"
            );
        }
    }

    #[test]
    fn invalid_xml_and_xml_limits_still_apply_inside_archive() {
        let error = decode(stored_zip(&[(MEMBER_NAME, b"<root>")])).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidXml);
        let limits = XmlLimits {
            max_depth: 1,
            ..XmlLimits::default()
        };
        let bytes = stored_zip(&[(MEMBER_NAME, b"<root><child/></root>")]);
        let error = decode_campaign(bytes, true, limits).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceLimit);
    }

    #[test]
    fn multiple_members_including_duplicate_names_fail() {
        for second in [MEMBER_NAME, "another.xml"] {
            let bytes = stored_zip(&[(MEMBER_NAME, b"<root/>"), (second, b"<root/>")]);
            assert_eq!(
                decode(bytes).unwrap_err().code,
                ErrorCode::AmbiguousStructure
            );
        }
        let mut bytes = stored_zip(&[(MEMBER_NAME, b"<root/>"), (MEMBER_NAME, b"<root/>")]);
        let footer = bytes.len() - 22;
        put_u16(&mut bytes, footer + 8, 1);
        put_u16(&mut bytes, footer + 10, 1);
        assert!(decode(bytes).is_err());
    }

    #[test]
    fn encrypted_unsupported_or_foreign_members_fail() {
        let mut encrypted = java_zip();
        put_u16(&mut encrypted, 6, 0x0809);
        put_u16(&mut encrypted, 178, 0x0809);
        assert_eq!(
            decode(encrypted).unwrap_err().code,
            ErrorCode::UnsupportedCompression
        );

        let mut unsupported = java_zip();
        put_u16(&mut unsupported, 8, 99);
        put_u16(&mut unsupported, 180, 99);
        assert_eq!(
            decode(unsupported).unwrap_err().code,
            ErrorCode::UnsupportedCompression
        );

        for name in ["foreign.xml", "../campaign.xml", "campaign.xml/"] {
            let bytes = stored_zip(&[(name, b"<root/>")]);
            assert_eq!(
                decode(bytes).unwrap_err().code,
                ErrorCode::UnsupportedCompression
            );
        }
        let mut symlink = stored_zip(&[(MEMBER_NAME, b"<root/>")]);
        let directory = directory_offset(&symlink);
        put_u16(&mut symlink, directory + 4, 0x0314);
        put_u32(&mut symlink, directory + 38, 0o120777 << 16);
        assert_eq!(
            decode(symlink).unwrap_err().code,
            ErrorCode::UnsupportedCompression
        );
    }

    #[test]
    fn archive_and_actual_decompressed_bytes_are_bounded() {
        let limits = XmlLimits {
            max_bytes: 249,
            ..XmlLimits::default()
        };
        assert_eq!(
            decode_campaign(java_zip(), true, limits).unwrap_err().code,
            ErrorCode::ResourceLimit
        );
        let xml = format!("<root>{}</root>", "a".repeat(64 * 1024));
        let bytes = encode_campaign(
            xml.as_bytes(),
            &encoding(CompressionMethod::Deflated),
            128 * 1024,
        )
        .unwrap();
        let limits = XmlLimits {
            max_bytes: xml.len() as u64,
            ..XmlLimits::default()
        };
        assert_eq!(
            decode_campaign(bytes.clone(), true, limits)
                .unwrap()
                .0
                .bytes(),
            xml.as_bytes()
        );
        let limits = XmlLimits {
            max_bytes: 1024,
            ..XmlLimits::default()
        };
        assert!(bytes.len() < limits.max_bytes as usize);
        assert_eq!(
            decode_campaign(bytes.clone(), true, limits)
                .unwrap_err()
                .code,
            ErrorCode::ResourceLimit
        );
        // A false small advertised size must not bypass the actual output cap.
        let mut dishonest = bytes;
        let directory = directory_offset(&dishonest);
        put_u32(&mut dishonest, 22, 1);
        put_u32(&mut dishonest, directory + 24, 1);
        let error = decode_campaign(dishonest, true, limits).unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceLimit);
    }

    #[test]
    fn encoded_output_is_bounded_including_zip_overhead() {
        let xml = b"<root/>";
        for compression in [CompressionMethod::Stored, CompressionMethod::Deflated] {
            let encoding = encoding(compression);
            let bytes = encode_campaign(xml, &encoding, 4096).unwrap();
            assert_eq!(
                encode_campaign(xml, &encoding, bytes.len() as u64).unwrap(),
                bytes
            );
            assert_eq!(
                encode_campaign(xml, &encoding, bytes.len() as u64 - 1)
                    .unwrap_err()
                    .code,
                ErrorCode::ResourceLimit
            );
        }
        assert_eq!(
            encode_campaign(xml, &CampaignEncoding::Xml, 1)
                .unwrap_err()
                .code,
            ErrorCode::ResourceLimit
        );
    }

    #[test]
    fn deflate_stream_end_at_output_buffer_boundary_is_accepted() {
        for size in [32 * 1024 - 1, 32 * 1024, 32 * 1024 + 1, 64 * 1024] {
            let xml = format!("<root>{}</root>", "a".repeat(size - 13));
            assert_eq!(xml.len(), size);
            let encoding = encoding(CompressionMethod::Deflated);
            let bytes = encode_campaign(xml.as_bytes(), &encoding, size as u64).unwrap();
            let limits = XmlLimits {
                max_bytes: size as u64,
                ..XmlLimits::default()
            };
            assert_eq!(
                decode_campaign(bytes, true, limits).unwrap().0.bytes(),
                xml.as_bytes()
            );
        }
    }

    #[test]
    fn archive_junk_and_deflate_truncation_or_trailing_data_fail() {
        let mut trailing = java_zip();
        trailing.push(0);
        assert!(decode(trailing).is_err());
        let mut prefixed = java_zip();
        prefixed.insert(0, 0);
        assert!(decode(prefixed).is_err());

        // Keep ZIP offsets, sizes and CRCs consistent while changing only the
        // DEFLATE framing. An ordinary CRC-only reader can miss these cases.
        let mut truncated_stream = java_zip();
        truncated_stream.remove(153);
        put_u32(&mut truncated_stream, 161, 111);
        put_u32(&mut truncated_stream, 189, 111);
        let footer = truncated_stream.len() - 22;
        put_u32(&mut truncated_stream, footer + 16, 169);
        let error = decode(truncated_stream).unwrap_err();
        assert!(error.message.contains("DEFLATE"));

        let mut extra_stream_data = java_zip();
        extra_stream_data.insert(154, 0);
        put_u32(&mut extra_stream_data, 163, 113);
        put_u32(&mut extra_stream_data, 191, 113);
        let footer = extra_stream_data.len() - 22;
        put_u32(&mut extra_stream_data, footer + 16, 171);
        let error = decode(extra_stream_data).unwrap_err();
        assert!(error.message.contains("extra data after campaign DEFLATE"));
    }
}
