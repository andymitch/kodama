//! H.264 NAL unit parsing utilities
//!
//! Provides basic parsing of H.264 bitstreams to:
//! - Split raw byte streams into individual NAL units
//! - Detect keyframes (IDR frames)
//! - Extract frame metadata

use bytes::{Bytes, BytesMut};

/// NAL unit types (5 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NalUnitType {
    /// Non-IDR slice (P or B frame)
    SliceNonIdr = 1,
    /// Slice data partition A
    SliceDataPartA = 2,
    /// Slice data partition B
    SliceDataPartB = 3,
    /// Slice data partition C
    SliceDataPartC = 4,
    /// IDR slice (keyframe)
    SliceIdr = 5,
    /// Supplemental enhancement information
    Sei = 6,
    /// Sequence parameter set
    Sps = 7,
    /// Picture parameter set
    Pps = 8,
    /// Access unit delimiter
    Aud = 9,
    /// End of sequence
    EndSeq = 10,
    /// End of stream
    EndStream = 11,
    /// Filler data
    Filler = 12,
    /// Unknown/reserved
    Unknown(u8),
}

impl From<u8> for NalUnitType {
    fn from(value: u8) -> Self {
        match value & 0x1F {
            1 => NalUnitType::SliceNonIdr,
            2 => NalUnitType::SliceDataPartA,
            3 => NalUnitType::SliceDataPartB,
            4 => NalUnitType::SliceDataPartC,
            5 => NalUnitType::SliceIdr,
            6 => NalUnitType::Sei,
            7 => NalUnitType::Sps,
            8 => NalUnitType::Pps,
            9 => NalUnitType::Aud,
            10 => NalUnitType::EndSeq,
            11 => NalUnitType::EndStream,
            12 => NalUnitType::Filler,
            n => NalUnitType::Unknown(n),
        }
    }
}

impl NalUnitType {
    /// Check if this NAL unit type indicates a keyframe
    pub fn is_keyframe(&self) -> bool {
        matches!(self, NalUnitType::SliceIdr)
    }

    /// Check if this is a parameter set (SPS/PPS)
    pub fn is_parameter_set(&self) -> bool {
        matches!(self, NalUnitType::Sps | NalUnitType::Pps)
    }

    /// Check if this is a slice (actual video data)
    ///
    /// Note: Excludes data partition slices (PartA/B/C) as they're not well
    /// supported by modern browsers and cause MEDIA_ERR_DECODE errors.
    pub fn is_slice(&self) -> bool {
        matches!(
            self,
            NalUnitType::SliceNonIdr
                | NalUnitType::SliceIdr
        )
    }
}

/// A parsed NAL unit
#[derive(Debug, Clone)]
pub struct NalUnit {
    /// The NAL unit type
    pub nal_type: NalUnitType,
    /// Reference IDC (2 bits) - importance for reference
    pub nal_ref_idc: u8,
    /// Raw NAL unit data including the header byte
    pub data: Bytes,
}

impl NalUnit {
    /// Parse a NAL unit from raw bytes (without start code)
    pub fn parse(data: Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        let header = data[0];
        let nal_ref_idc = (header >> 5) & 0x03;
        let nal_type = NalUnitType::from(header);

        Some(Self {
            nal_type,
            nal_ref_idc,
            data,
        })
    }

    /// Check if this NAL unit is a keyframe
    pub fn is_keyframe(&self) -> bool {
        self.nal_type.is_keyframe()
    }
}

/// Maximum NAL parser buffer size (2 MB) to prevent unbounded memory growth
const MAX_NAL_BUFFER_SIZE: usize = 2 * 1024 * 1024;

/// Parser for splitting H.264 byte streams into NAL units
///
/// Handles both Annex B format (start codes: 0x000001 or 0x00000001)
pub struct NalParser {
    buffer: BytesMut,
}

impl NalParser {
    /// Create a new NAL parser
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    /// Feed data into the parser and extract complete NAL units
    pub fn feed(&mut self, data: &[u8]) -> Vec<NalUnit> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > MAX_NAL_BUFFER_SIZE {
            tracing::warn!("NAL parser buffer exceeded {} bytes, resetting", MAX_NAL_BUFFER_SIZE);
            self.buffer.clear();
            return Vec::new();
        }
        self.extract_nals()
    }

    /// Extract all complete NAL units from the buffer
    fn extract_nals(&mut self) -> Vec<NalUnit> {
        let mut nals = Vec::new();

        loop {
            // Find start code
            let start = match self.find_start_code(0) {
                Some(pos) => pos,
                None => break,
            };

            // Skip the start code
            let start_code_len = self.start_code_len(start);
            let nal_start = start + start_code_len;

            // Find the next start code (or end of buffer)
            let nal_end = match self.find_start_code(nal_start) {
                Some(pos) => pos,
                None => {
                    // No more start codes - keep remaining data in buffer
                    if start > 0 {
                        let _ = self.buffer.split_to(start);
                    }
                    break;
                }
            };

            // Extract this NAL unit
            let _ = self.buffer.split_to(nal_start); // Remove start code
            let nal_data = self.buffer.split_to(nal_end - nal_start);

            let nal_bytes = nal_data.freeze();
            if let Some(nal) = NalUnit::parse(nal_bytes) {
                nals.push(nal);
            }
        }

        nals
    }

    /// Find the position of a start code starting from `offset`
    fn find_start_code(&self, offset: usize) -> Option<usize> {
        if self.buffer.len() < offset + 3 {
            return None;
        }

        for i in offset..self.buffer.len().saturating_sub(2) {
            // Check for 0x000001
            if self.buffer[i] == 0 && self.buffer[i + 1] == 0 && self.buffer[i + 2] == 1 {
                return Some(i);
            }
            // Check for 0x00000001
            if i + 3 < self.buffer.len()
                && self.buffer[i] == 0
                && self.buffer[i + 1] == 0
                && self.buffer[i + 2] == 0
                && self.buffer[i + 3] == 1
            {
                return Some(i);
            }
        }

        None
    }

    /// Get the length of the start code at the given position
    fn start_code_len(&self, pos: usize) -> usize {
        if pos + 3 < self.buffer.len()
            && self.buffer[pos] == 0
            && self.buffer[pos + 1] == 0
            && self.buffer[pos + 2] == 0
            && self.buffer[pos + 3] == 1
        {
            4
        } else {
            3
        }
    }

    /// Flush any remaining data in the buffer as a NAL unit
    pub fn flush(&mut self) -> Option<NalUnit> {
        if self.buffer.is_empty() {
            return None;
        }

        // Find and skip the start code if present
        let start = if let Some(pos) = self.find_start_code(0) {
            pos + self.start_code_len(pos)
        } else {
            0
        };

        if start >= self.buffer.len() {
            self.buffer.clear();
            return None;
        }

        let _ = self.buffer.split_to(start);
        let data = self.buffer.split().freeze();
        NalUnit::parse(data)
    }
}

impl Default for NalParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a raw H.264 chunk contains a keyframe (without full parsing)
///
/// This is a quick heuristic check that looks for IDR NAL units.
pub fn contains_keyframe(data: &[u8]) -> bool {
    // Look for start code followed by IDR NAL type
    for i in 0..data.len().saturating_sub(4) {
        // Check for 0x000001 or 0x00000001
        let is_start_code = (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
            || (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1);

        if is_start_code {
            let nal_offset = if data[i + 2] == 1 { i + 3 } else { i + 4 };
            if nal_offset < data.len() {
                let nal_type = data[nal_offset] & 0x1F;
                if nal_type == 5 {
                    // IDR slice
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nal_type_parsing() {
        assert_eq!(NalUnitType::from(0x65), NalUnitType::SliceIdr);
        assert_eq!(NalUnitType::from(0x67), NalUnitType::Sps);
        assert_eq!(NalUnitType::from(0x68), NalUnitType::Pps);
        assert_eq!(NalUnitType::from(0x41), NalUnitType::SliceNonIdr);
    }

    #[test]
    fn test_keyframe_detection() {
        assert!(NalUnitType::SliceIdr.is_keyframe());
        assert!(!NalUnitType::SliceNonIdr.is_keyframe());
        assert!(!NalUnitType::Sps.is_keyframe());
    }

    #[test]
    fn test_nal_parser() {
        // Simulate H.264 stream with SPS, PPS, and IDR
        let data = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80, // PPS
            0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, // IDR
        ];

        let mut parser = NalParser::new();
        let nals = parser.feed(&data);

        assert_eq!(nals.len(), 2); // SPS and PPS complete, IDR waiting for next start code

        // Check types
        assert_eq!(nals[0].nal_type, NalUnitType::Sps);
        assert_eq!(nals[1].nal_type, NalUnitType::Pps);

        // Flush remaining
        let remaining = parser.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().nal_type, NalUnitType::SliceIdr);
    }

    #[test]
    fn test_contains_keyframe() {
        // IDR frame
        let idr_data = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84];
        assert!(contains_keyframe(&idr_data));

        // P-frame
        let p_data = vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x24];
        assert!(!contains_keyframe(&p_data));

        // SPS (not a keyframe)
        let sps_data = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00];
        assert!(!contains_keyframe(&sps_data));
    }
}
