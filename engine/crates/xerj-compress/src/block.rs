//! Block compression for 128-document segments.
//!
//! Documents are accumulated into 128-doc blocks, compressed as a unit,
//! then emitted with a fixed header. Readers decompress on demand with
//! random access by block index.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io::{Cursor, Read, Write};
use xerj_common::XerjError;

use crate::codec::{get_codec, Codec, CompressionLevel};

/// Result alias for block operations.
pub type Result<T> = std::result::Result<T, XerjError>;

/// Number of documents per compressed block.
pub const BLOCK_SIZE: usize = 128;

/// Magic bytes at the start of every block header.
const BLOCK_MAGIC: u32 = 0x5A424C4B; // "ZBLK"

// ─────────────────────────────────────────────────────────────────────────────
// Block header on-disk layout (20 bytes)
// ─────────────────────────────────────────────────────────────────────────────
//
//  Offset  Size  Field
//  ──────  ────  ─────────────────────────────
//       0     4  magic (ZBLK)
//       4     4  doc_count   (u32 LE)
//       8     4  uncompressed_len (u32 LE)
//      12     4  compressed_len   (u32 LE)
//      16     1  codec_id    (0=none, 1=lz4, 2=zstd)
//      17     3  reserved
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BlockHeader {
    pub doc_count: u32,
    pub uncompressed_len: u32,
    pub compressed_len: u32,
    pub codec_id: u8,
}

impl BlockHeader {
    pub const SIZE: usize = 20;

    fn codec_id_for(level: CompressionLevel) -> u8 {
        match level {
            CompressionLevel::None => 0,
            CompressionLevel::Fast => 1,
            CompressionLevel::Balanced | CompressionLevel::Best => 2,
        }
    }

    fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_u32::<LittleEndian>(BLOCK_MAGIC)?;
        out.write_u32::<LittleEndian>(self.doc_count)?;
        out.write_u32::<LittleEndian>(self.uncompressed_len)?;
        out.write_u32::<LittleEndian>(self.compressed_len)?;
        out.write_u8(self.codec_id)?;
        out.write_all(&[0u8; 3])?; // reserved
        Ok(())
    }

    fn read_from(r: &mut impl Read) -> std::io::Result<Self> {
        let magic = r.read_u32::<LittleEndian>()?;
        if magic != BLOCK_MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad block magic: 0x{magic:08X}"),
            ));
        }
        let doc_count = r.read_u32::<LittleEndian>()?;
        let uncompressed_len = r.read_u32::<LittleEndian>()?;
        let compressed_len = r.read_u32::<LittleEndian>()?;
        let codec_id = r.read_u8()?;
        let mut _reserved = [0u8; 3];
        r.read_exact(&mut _reserved)?;
        Ok(Self {
            doc_count,
            uncompressed_len,
            compressed_len,
            codec_id,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BlockWriter
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulates raw serialized documents and emits compressed blocks of
/// [`BLOCK_SIZE`] documents each.
pub struct BlockWriter {
    codec: Box<dyn Codec>,
    codec_id: u8,
    #[allow(dead_code)]
    level: CompressionLevel,
    /// Length-prefixed raw document payloads for the current block.
    pending: Vec<u8>,
    pending_count: u32,
    /// Completed, compressed blocks ready for flushing.
    completed: Vec<u8>,
}

impl BlockWriter {
    pub fn new(level: CompressionLevel) -> Self {
        let codec_id = BlockHeader::codec_id_for(level);
        Self {
            codec: get_codec(level),
            codec_id,
            level,
            pending: Vec::new(),
            pending_count: 0,
            completed: Vec::new(),
        }
    }

    /// Add a serialized document to the current block.
    ///
    /// Each document is length-prefixed (u32 LE) inside the block payload so
    /// that individual records can be located after decompression.
    pub fn add_document(&mut self, doc: &[u8]) -> Result<()> {
        // Length prefix
        let len = doc.len() as u32;
        self.pending
            .write_u32::<LittleEndian>(len)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        self.pending.extend_from_slice(doc);
        self.pending_count += 1;

        if self.pending_count as usize >= BLOCK_SIZE {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Flush any remaining documents as a partial block.
    pub fn finish(&mut self) -> Result<Vec<u8>> {
        if self.pending_count > 0 {
            self.flush_block()?;
        }
        Ok(std::mem::take(&mut self.completed))
    }

    fn flush_block(&mut self) -> Result<()> {
        let raw = std::mem::take(&mut self.pending);
        let uncompressed_len = raw.len() as u32;
        let doc_count = self.pending_count;
        self.pending_count = 0;

        let compressed = self.codec.compress(&raw)?;
        let compressed_len = compressed.len() as u32;

        let header = BlockHeader {
            doc_count,
            uncompressed_len,
            compressed_len,
            codec_id: self.codec_id,
        };
        header
            .write_to(&mut self.completed)
            .map_err(|e| XerjError::internal(e.to_string()))?;
        self.completed.extend_from_slice(&compressed);
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BlockReader
// ─────────────────────────────────────────────────────────────────────────────

/// Reads compressed blocks written by [`BlockWriter`].
///
/// Decompression is lazy — only happens when [`next_block`] is called.
pub struct BlockReader<'a> {
    cursor: Cursor<&'a [u8]>,
}

impl<'a> BlockReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            cursor: Cursor::new(data),
        }
    }

    /// Read and decompress the next block, returning individual documents.
    ///
    /// Returns `None` when all blocks have been consumed.
    pub fn next_block(&mut self) -> Result<Option<Vec<Vec<u8>>>> {
        if self.cursor.position() as usize >= self.cursor.get_ref().len() {
            return Ok(None);
        }

        let header = BlockHeader::read_from(&mut self.cursor)
            .map_err(|e| XerjError::internal(format!("block header read: {e}")))?;

        let mut compressed = vec![0u8; header.compressed_len as usize];
        self.cursor
            .read_exact(&mut compressed)
            .map_err(|e| XerjError::internal(format!("block data read: {e}")))?;

        // Select decompressor by codec_id
        let decompressed = match header.codec_id {
            0 => {
                // None
                if compressed.len() != header.uncompressed_len as usize {
                    return Err(XerjError::internal("None codec length mismatch"));
                }
                compressed
            }
            1 => {
                // LZ4
                lz4_flex::decompress_size_prepended(&compressed)
                    .map_err(|e| XerjError::internal(format!("LZ4 decompress: {e}")))?
            }
            2 => {
                // Zstd
                zstd::bulk::decompress(&compressed, header.uncompressed_len as usize)
                    .map_err(|e| XerjError::internal(format!("Zstd decompress: {e}")))?
            }
            id => return Err(XerjError::internal(format!("unknown codec_id: {id}"))),
        };

        // Split individual documents by length prefix
        let mut docs = Vec::with_capacity(header.doc_count as usize);
        let mut rdr = Cursor::new(decompressed);
        for _ in 0..header.doc_count {
            let len = rdr
                .read_u32::<LittleEndian>()
                .map_err(|e| XerjError::internal(format!("doc len read: {e}")))? as usize;
            let mut doc = vec![0u8; len];
            rdr.read_exact(&mut doc)
                .map_err(|e| XerjError::internal(format!("doc data read: {e}")))?;
            docs.push(doc);
        }

        Ok(Some(docs))
    }

    /// Collect all documents from all blocks.
    pub fn read_all(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut all = Vec::new();
        while let Some(block) = self.next_block()? {
            all.extend(block);
        }
        Ok(all)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_docs(n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| format!(r#"{{"id":{i},"msg":"document number {i}"}}"#).into_bytes())
            .collect()
    }

    fn roundtrip(level: CompressionLevel, doc_count: usize) {
        let docs = make_docs(doc_count);

        let mut writer = BlockWriter::new(level);
        for doc in &docs {
            writer.add_document(doc).unwrap();
        }
        let compressed = writer.finish().unwrap();

        let mut reader = BlockReader::new(&compressed);
        let recovered = reader.read_all().unwrap();

        assert_eq!(recovered.len(), docs.len());
        for (orig, rec) in docs.iter().zip(recovered.iter()) {
            assert_eq!(orig, rec);
        }
    }

    #[test]
    fn roundtrip_none_exact_block() { roundtrip(CompressionLevel::None, BLOCK_SIZE); }

    #[test]
    fn roundtrip_lz4_partial_block() { roundtrip(CompressionLevel::Fast, 37); }

    #[test]
    fn roundtrip_lz4_multiple_blocks() { roundtrip(CompressionLevel::Fast, BLOCK_SIZE * 3 + 5); }

    #[test]
    fn roundtrip_zstd_balanced() { roundtrip(CompressionLevel::Balanced, 200); }

    #[test]
    fn empty_finish_produces_no_bytes() {
        let mut writer = BlockWriter::new(CompressionLevel::Fast);
        let out = writer.finish().unwrap();
        assert!(out.is_empty());
    }
}
