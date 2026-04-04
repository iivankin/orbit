use std::io::{self, Read};

use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;

const XAR_MAGIC: [u8; 4] = *b"xar!";
const XAR_FIXED_HEADER_SIZE: usize = 28;

#[derive(Debug, Clone, PartialEq, Eq)]
struct XarMember {
    offset: u64,
    length: u64,
}

pub(crate) struct XarMemberStream<R> {
    reader: R,
    remaining: u64,
}

impl<R> XarMemberStream<R> {
    pub(crate) fn into_inner(self) -> R {
        self.reader
    }
}

impl<R: Read> Read for XarMemberStream<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }

        let limit = self.remaining.min(buffer.len() as u64) as usize;
        let read = self.reader.read(&mut buffer[..limit])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

pub(crate) fn open_member_stream<R: Read>(
    mut reader: R,
    member_name: &str,
) -> Result<XarMemberStream<R>> {
    let header = read_xar_header(&mut reader)?;
    if header.compressed_toc_len > usize::MAX as u64 {
        bail!("XAR TOC is too large to decode on this platform");
    }

    if header.header_size > XAR_FIXED_HEADER_SIZE as u16 {
        io::copy(
            &mut std::io::Read::by_ref(&mut reader)
                .take((header.header_size as usize - XAR_FIXED_HEADER_SIZE) as u64),
            &mut io::sink(),
        )
        .context("failed to skip extended XAR header bytes")?;
    }

    let mut compressed_toc = vec![0_u8; header.compressed_toc_len as usize];
    reader
        .read_exact(&mut compressed_toc)
        .context("failed to read XAR compressed TOC")?;

    let toc_xml = decode_zlib(&compressed_toc)?;
    let member = find_member(&toc_xml, member_name)
        .with_context(|| format!("XAR TOC did not contain member `{member_name}`"))?;

    io::copy(
        &mut std::io::Read::by_ref(&mut reader).take(member.offset),
        &mut io::sink(),
    )
    .with_context(|| format!("failed to skip to XAR member `{member_name}`"))?;

    Ok(XarMemberStream {
        reader,
        remaining: member.length,
    })
}

#[derive(Debug, Clone, Copy)]
struct XarHeader {
    header_size: u16,
    compressed_toc_len: u64,
}

fn read_xar_header(reader: &mut dyn Read) -> Result<XarHeader> {
    let mut bytes = [0_u8; XAR_FIXED_HEADER_SIZE];
    reader
        .read_exact(&mut bytes)
        .context("failed to read XAR header")?;

    if bytes[..4] != XAR_MAGIC {
        bail!("stream does not start with a XAR header");
    }

    let header_size = u16::from_be_bytes([bytes[4], bytes[5]]);
    let version = u16::from_be_bytes([bytes[6], bytes[7]]);
    if version != 1 {
        bail!("unsupported XAR version {version}");
    }

    Ok(XarHeader {
        header_size,
        compressed_toc_len: u64::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]),
    })
}

fn decode_zlib(bytes: &[u8]) -> Result<String> {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut xml = String::new();
    decoder
        .read_to_string(&mut xml)
        .context("failed to decompress the XAR TOC")?;
    Ok(xml)
}

fn find_member(toc_xml: &str, member_name: &str) -> Option<XarMember> {
    let mut search_start = 0;
    while let Some(relative_start) = toc_xml[search_start..].find("<file") {
        let start = search_start + relative_start;
        let end = start + toc_xml[start..].find("</file>")? + "</file>".len();
        let file_block = &toc_xml[start..end];
        if extract_last_tag_text(file_block, "name")? != member_name {
            search_start = end;
            continue;
        }

        let data_block = extract_block(file_block, "data")?;
        let offset = extract_tag_text(data_block, "offset")?.parse().ok()?;
        let length = extract_tag_text(data_block, "length")?.parse().ok()?;
        return Some(XarMember { offset, length });
    }

    None
}

fn extract_block<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(&text[start..end])
}

fn extract_tag_text<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    extract_block(text, tag).map(str::trim)
}

fn extract_last_tag_text<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.rfind(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(text[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::find_member;

    #[cfg(target_os = "macos")]
    use super::open_member_stream;
    #[cfg(target_os = "macos")]
    use anyhow::Result;
    #[cfg(target_os = "macos")]
    use std::fs;
    #[cfg(target_os = "macos")]
    use std::io::Read;
    #[cfg(target_os = "macos")]
    use std::process::{self, Command};
    #[cfg(target_os = "macos")]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_content_member_in_xar_toc() {
        let toc = r#"
            <?xml version="1.0" encoding="UTF-8"?>
            <xar>
              <toc>
                <file id="1">
                  <name>Content</name>
                  <data>
                    <offset>39</offset>
                    <length>15</length>
                  </data>
                </file>
              </toc>
            </xar>
        "#;

        let member = find_member(toc, "Content").expect("content member");
        assert_eq!(member.offset, 39);
        assert_eq!(member.length, 15);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn streams_named_member_from_real_xar() -> Result<()> {
        let temp_root = test_temp_dir("orbit-xar-stream");
        fs::create_dir_all(&temp_root)?;
        fs::write(temp_root.join("Content"), "payload")?;
        fs::write(temp_root.join("Metadata"), "metadata")?;

        run_test_command(
            "sh",
            &[
                "-c",
                &format!(
                    "cd '{}' && xar -cf '{}' --no-compress '.*' Content Metadata",
                    temp_root.display(),
                    temp_root.join("test.xip").display()
                ),
            ],
        )?;

        let mut archive = fs::File::open(temp_root.join("test.xip"))?;
        let mut payload = Vec::new();
        open_member_stream(&mut archive, "Content")?.read_to_end(&mut payload)?;
        assert_eq!(payload, b"payload");

        let _ = fs::remove_dir_all(&temp_root);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn run_test_command(program: &str, args: &[&str]) -> Result<()> {
        let status = Command::new(program).args(args).status()?;
        if !status.success() {
            anyhow::bail!("`{program}` exited with status {status}");
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn test_temp_dir(prefix: &str) -> std::path::PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        std::env::temp_dir().join(format!("{prefix}-{}-{millis}", process::id()))
    }
}
