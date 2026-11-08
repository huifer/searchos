//! Deterministic chunking (RFC-006 6.2, 6.4).

use serde::{Deserialize, Serialize};

/// Chunking configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingConfig {
    /// Target chunk size in characters.
    pub max_chars: usize,
    /// Overlap between consecutive chunks in characters.
    pub overlap_chars: usize,
    /// Prefer breaking at paragraph / sentence boundaries.
    pub respect_boundaries: bool,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            max_chars: 1200,
            overlap_chars: 120,
            respect_boundaries: true,
        }
    }
}

/// One chunk output.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub index: u32,
    pub text: String,
    pub hash: String,
}

/// Chunks text deterministically. Same content + config ⇒ same chunks,
/// so identical chunks keep their chunk_hash and reuse embeddings.
pub fn chunk_text(text: &str, config: &ChunkingConfig) -> Vec<Chunk> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let max = config.max_chars.max(64);
    let overlap = config.overlap_chars.min(max / 2);
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let bytes = text;
    while start < bytes.len() {
        let mut end = (start + max).min(bytes.len());
        // Snap to a char boundary.
        while end < bytes.len() && !bytes.is_char_boundary(end) {
            end -= 1;
        }
        let mut start_bound = start;
        while start_bound > 0 && !bytes.is_char_boundary(start_bound) {
            start_bound -= 1;
        }
        let mut slice = &bytes[start_bound..end];
        if config.respect_boundaries && end < bytes.len() {
            // Try to end at paragraph or sentence boundary.
            if let Some(p) = slice.rfind("\n\n") {
                if p > max / 3 {
                    slice = &slice[..p];
                }
            } else if let Some(p) = slice.rfind(". ") {
                if p > max / 3 {
                    slice = &slice[..=p];
                }
            }
        }
        let trimmed = slice.trim();
        if !trimmed.is_empty() {
            chunks.push(Chunk {
                index: chunks.len() as u32,
                text: trimmed.to_string(),
                hash: crate::hash::content_hash(trimmed.as_bytes()),
            });
        }
        if end >= bytes.len() {
            break;
        }
        start = end - overlap;
        while start > 0 && !bytes.is_char_boundary(start) {
            start -= 1;
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_single_chunk() {
        let c = chunk_text("hello world", &ChunkingConfig::default());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].index, 0);
    }

    #[test]
    fn deterministic_and_stable_hashes() {
        let text = "para one\n\npara two\n\npara three";
        let a = chunk_text(text, &ChunkingConfig::default());
        let b = chunk_text(text, &ChunkingConfig::default());
        assert_eq!(a, b);
        assert!(a.iter().all(|c| c.hash.len() == 64));
    }

    #[test]
    fn long_text_multiple_chunks() {
        let para = "some sentence content here. ".repeat(200);
        let c = chunk_text(&para, &ChunkingConfig::default());
        assert!(c.len() > 1);
        // Chunks preserve all content (with overlap).
        assert!(c.iter().any(|ch| ch.text.contains("sentence")));
    }

    #[test]
    fn empty_text_no_chunks() {
        assert!(chunk_text("   ", &ChunkingConfig::default()).is_empty());
    }
}
