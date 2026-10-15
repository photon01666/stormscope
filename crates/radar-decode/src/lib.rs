//! radar-decode — NEXRAD Archive II bytes -> `radar_core::Volume`.
//!
//! Wraps the `nexrad-data`/`nexrad-decode`
//! crates (ICD 2620010H parsing) and maps their output onto the frozen
//! `radar_core` data model. Verified against real KTLX volume data.

use std::io::Read;

use chrono::{DateTime, Utc};
use nexrad_data::volume::{File, Header, Record};
use nexrad_decode::messages::MessageContents;
use nexrad_decode::messages::digital_radar_data::{GenericDataBlock, Message as Message31};
use nexrad_decode::messages::digital_radar_data_legacy::Message as Message1;
use radar_core::{Moment, MomentCalibration, MomentData, Sweep, Volume, VolumeMeta};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unsupported or corrupt volume: {0}")]
    Unsupported(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Gzip magic bytes (RFC 1952).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Archive II tape-filename magic ("AR2V") present at the start of a full
/// volume header. The Unidata real-time chunk stream includes this header.
const ARCHIVE_MAGIC: &[u8; 4] = b"AR2V";

/// Canonical moment ordering used for `VolumeMeta::moments` and per-sweep
/// moment ordering.
const MOMENT_ORDER: [Moment; 6] = [
    Moment::Reflectivity,
    Moment::Velocity,
    Moment::SpectrumWidth,
    Moment::DifferentialReflectivity,
    Moment::DifferentialPhase,
    Moment::CorrelationCoefficient,
];

/// Map a Level II (or L3 product-family) data block name to a [`Moment`].
///
/// Covers the modern block names (REF/VEL/SW/ZDR/PHI/RHO) emitted by
/// message 31 and the legacy/CF names (CZ/VR/SV/DR/DP/KD/RF) seen elsewhere.
pub fn map_block_name(name: &[u8; 3]) -> Option<Moment> {
    match name {
        b"REF" | b"CZ " => Some(Moment::Reflectivity),
        b"VEL" | b"VR " => Some(Moment::Velocity),
        b"SW " | b"SV " => Some(Moment::SpectrumWidth),
        b"DR " | b"ZDR" => Some(Moment::DifferentialReflectivity),
        b"DP " | b"PHI" => Some(Moment::DifferentialPhase),
        b"KD " | b"KDP" => Some(Moment::SpecificDifferentialPhase),
        b"CC " | b"RF " | b"RHO" => Some(Moment::CorrelationCoefficient),
        _ => None,
    }
}

/// Decode a full gzipped (or uncompressed) NEXRAD Archive II volume file.
pub fn decode_volume_file(bytes: &[u8]) -> Result<Volume, DecodeError> {
    if bytes.len() < size_of::<Header>() {
        return Err(DecodeError::Unsupported(format!(
            "input too small for an Archive II file ({} bytes)",
            bytes.len()
        )));
    }

    // Handles the pre-~2018 gzip wrapper; no-op for uncompressed archives.
    let file = File::new(bytes.to_vec())
        .decompress()
        .map_err(|e| DecodeError::Other(e.into()))?;

    let head = ArchiveHead::from_header(file.header());
    let records = file.records().map_err(|e| DecodeError::Other(e.into()))?;

    let mut collector = Collector::default();
    for record in &records {
        collector.consume(record);
    }
    collector.finish(head)
}

/// Decode an assembled real-time volume (concatenated LDM chunks, with or
/// without the 24-byte Archive II header, optionally gzip-compressed).
pub fn decode_realtime_volume(bytes: &[u8]) -> Result<Volume, DecodeError> {
    let data = gunzip_if_needed(bytes)?;

    // The Unidata real-time chunk stream is the archive byte stream including
    // the volume header ("AR2V..."); handle it via the archive path.
    if data.starts_with(ARCHIVE_MAGIC) {
        return decode_volume_file(&data);
    }

    let records = split_ldm_records(&data)?;
    let mut collector = Collector::default();
    for record in &records {
        collector.consume(record);
    }
    collector.finish(ArchiveHead::default())
}

/// Decompress gzip data if it carries the gzip magic, otherwise pass through.
fn gunzip_if_needed(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if data.len() >= 2 && data[..2] == GZIP_MAGIC {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(data)
            .read_to_end(&mut out)
            .map_err(|e| DecodeError::Other(e.into()))?;
        Ok(out)
    } else {
        Ok(data.to_vec())
    }
}

/// Split a stream of size-prefixed LDM records (modern) or CTM frames
/// (legacy, detected by an all-zero first size word) into records.
fn split_ldm_records(data: &[u8]) -> Result<Vec<Record<'_>>, DecodeError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    // Legacy CTM: first four bytes are all zeros (rpg_unknown message header
    // fields), not a valid LDM size prefix — treat the whole buffer as one
    // uncompressed record.
    if data.len() >= 4 && data[..4] == [0, 0, 0, 0] {
        return Ok(vec![Record::from_slice(data)]);
    }

    let mut records = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        if pos + 4 > data.len() {
            log::warn!(
                "trailing {} bytes without LDM record header",
                data.len() - pos
            );
            break;
        }
        let size =
            i32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()).unsigned_abs() as usize;
        if size == 0 {
            log::warn!("zero-size LDM record at byte {pos}; stopping");
            break;
        }
        let end = pos + 4 + size;
        if end > data.len() {
            log::warn!(
                "truncated LDM record at byte {pos} (need {end}, have {}); stopping",
                data.len()
            );
            break;
        }
        records.push(Record::from_slice(&data[pos..end]));
        pos = end;
    }
    Ok(records)
}

/// Metadata extracted from the Archive II volume header (absent for headerless
/// real-time streams).
#[derive(Default)]
struct ArchiveHead {
    site_id: Option<String>,
    volume_start: Option<DateTime<Utc>>,
    sequence_number: Option<u32>,
}

impl ArchiveHead {
    fn from_header(header: Option<&Header>) -> Self {
        let Some(header) = header else {
            return Self::default();
        };
        Self {
            site_id: header
                .icao_of_radar()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            volume_start: header.date_time(),
            sequence_number: header
                .extension_number()
                .and_then(|s| s.trim().parse::<u32>().ok()),
        }
    }
}

/// One moment (data block) of one radial, decoded to raw DNs.
struct RawMoment {
    kind: Moment,
    scale: f32,
    offset: f32,
    threshold: f32,
    range_start_m: f32,
    gate_size_m: f32,
    gates: Vec<i16>,
}

/// One radial (ray) of one elevation cut.
struct RawRadial {
    elevation_number: u8,
    /// Measured elevation angle of this ray (degrees).
    elevation_deg: f32,
    azimuth_deg: f32,
    azimuth_width_deg: f32,
    time: Option<DateTime<Utc>>,
    moments: Vec<RawMoment>,
}

/// Decoded Volume Coverage Pattern metadata (for requested tilt angles).
struct VcpInfo {
    pattern_number: u16,
    /// Requested elevation angle per cut (1-based elevation number - 1).
    cut_elevations_deg: Vec<f64>,
}

/// Accumulates radials and VCP info across LDM records, then builds a
/// [`Volume`].
#[derive(Default)]
struct Collector {
    site_hint: Option<String>,
    vcp: Option<VcpInfo>,
    vcp_hint: Option<u16>,
    radials: Vec<RawRadial>,
}

impl Collector {
    /// Decompress (bzip2) and decode one LDM record's messages. Individual
    /// corrupt records are logged and skipped so partial real-time volumes
    /// still decode; a volume with no radials at all errors in `finish`.
    fn consume(&mut self, raw: &Record<'_>) {
        let owned;
        let record: &Record<'_> = if raw.compressed() {
            match raw.decompress() {
                Ok(r) => {
                    owned = r;
                    &owned
                }
                Err(e) => {
                    log::warn!("failed to decompress LDM record: {e}");
                    return;
                }
            }
        } else {
            raw
        };

        let messages = match record.messages() {
            Ok(messages) => messages,
            Err(e) => {
                log::warn!("failed to decode LDM record messages: {e}");
                return;
            }
        };

        for message in messages {
            match message.into_contents() {
                MessageContents::VolumeCoveragePattern(m) => {
                    if self.vcp.is_none() {
                        self.vcp = Some(VcpInfo {
                            pattern_number: m.header().pattern_number(),
                            cut_elevations_deg: m
                                .elevations()
                                .iter()
                                .map(|e| e.elevation_angle())
                                .collect(),
                        });
                    }
                }
                MessageContents::DigitalRadarData(m) => {
                    if self.site_hint.is_none() {
                        let id = m.header().radar_identifier();
                        if !id.trim().is_empty() {
                            self.site_hint = Some(id);
                        }
                    }
                    if self.vcp_hint.is_none() {
                        self.vcp_hint = m
                            .volume_data_block()
                            .map(|b| b.volume_coverage_pattern_number());
                    }
                    self.radials.push(radial_from_m31(&m));
                }
                MessageContents::DigitalRadarDataLegacy(m) => {
                    self.radials.push(radial_from_legacy(&m));
                }
                _ => {}
            }
        }
    }

    fn finish(self, head: ArchiveHead) -> Result<Volume, DecodeError> {
        if self.radials.is_empty() {
            return Err(DecodeError::Unsupported(
                "volume contains no radar data messages".to_string(),
            ));
        }

        let site_id = head
            .site_id
            .or(self.site_hint)
            .unwrap_or_else(|| "UNKNOWN".to_string());

        let volume_start = head
            .volume_start
            .or_else(|| self.radials.iter().filter_map(|r| r.time).min());
        let Some(volume_start) = volume_start else {
            return Err(DecodeError::Unsupported(
                "volume has neither a header nor radial timestamps".to_string(),
            ));
        };

        let vcp_number = self
            .vcp
            .as_ref()
            .map(|v| v.pattern_number)
            .or(self.vcp_hint);

        // Group consecutive radials by elevation number (mirrors
        // `nexrad_model::data::Sweep::from_radials`).
        let mut sweeps: Vec<Sweep> = Vec::new();
        let mut group: Vec<RawRadial> = Vec::new();
        for radial in self.radials {
            if !group.is_empty() && radial.elevation_number != group[0].elevation_number {
                sweeps.push(build_sweep(group, self.vcp.as_ref()));
                group = Vec::new();
            }
            group.push(radial);
        }
        if !group.is_empty() {
            sweeps.push(build_sweep(group, self.vcp.as_ref()));
        }

        let moments: Vec<Moment> = MOMENT_ORDER
            .into_iter()
            .filter(|kind| {
                sweeps
                    .iter()
                    .any(|s| s.moments.iter().any(|m| m.kind == *kind))
            })
            .collect();

        Ok(Volume {
            meta: VolumeMeta {
                site_id,
                volume_start,
                vcp: vcp_number,
                sequence_number: head.sequence_number,
                sweep_count: sweeps.len(),
                moments,
            },
            sweeps,
        })
    }
}

/// Decode one message-31 (modern Digital Radar Data) radial.
fn radial_from_m31(m: &Message31<'_>) -> RawRadial {
    let header = m.header();
    let azimuth_width_deg = match header.azimuth_resolution_spacing_raw() {
        1 => 0.5,
        _ => 1.0,
    };

    let mut moments = Vec::new();
    for (kind, block) in [
        (Moment::Reflectivity, m.reflectivity_data_block()),
        (Moment::Velocity, m.velocity_data_block()),
        (Moment::SpectrumWidth, m.spectrum_width_data_block()),
        (
            Moment::DifferentialReflectivity,
            m.differential_reflectivity_data_block(),
        ),
        (Moment::DifferentialPhase, m.differential_phase_data_block()),
        (
            Moment::CorrelationCoefficient,
            m.correlation_coefficient_data_block(),
        ),
    ] {
        if let Some(raw) = block.and_then(|b| generic_to_raw(kind, b.inner())) {
            moments.push(raw);
        }
    }

    RawRadial {
        elevation_number: header.elevation_number(),
        elevation_deg: header.elevation_angle_raw(),
        azimuth_deg: header.azimuth_angle_raw(),
        azimuth_width_deg,
        time: header.date_time(),
        moments,
    }
}

/// Decode one legacy message-1 (Digital Radar Data Legacy) radial.
fn radial_from_legacy(m: &Message1<'_>) -> RawRadial {
    let mut moments = Vec::new();
    let surv_range_m = m.surveillance_first_gate_range().unsigned_abs() as f32;
    let surv_gate_m = m.surveillance_gate_interval() as f32;
    let dopp_range_m = m.doppler_first_gate_range().unsigned_abs() as f32;
    let dopp_gate_m = m.doppler_gate_interval() as f32;

    if let Some(gates) = m.reflectivity_gates() {
        moments.push(RawMoment {
            kind: Moment::Reflectivity,
            scale: 2.0,
            offset: 66.0,
            threshold: 0.0,
            range_start_m: surv_range_m,
            gate_size_m: surv_gate_m,
            gates: gates.iter().map(|&b| b as i16).collect(),
        });
    }
    // Legacy velocity uses 0.5 or 1.0 m/s resolution (scale 2.0 or 1.0).
    let vel_scale = if m.doppler_velocity_resolution() > 0.9 {
        1.0
    } else {
        2.0
    };
    if let Some(gates) = m.velocity_gates() {
        moments.push(RawMoment {
            kind: Moment::Velocity,
            scale: vel_scale,
            offset: 129.0,
            threshold: 0.0,
            range_start_m: dopp_range_m,
            gate_size_m: dopp_gate_m,
            gates: gates.iter().map(|&b| b as i16).collect(),
        });
    }
    if let Some(gates) = m.spectrum_width_gates() {
        moments.push(RawMoment {
            kind: Moment::SpectrumWidth,
            scale: 2.0,
            offset: 129.0,
            threshold: 0.0,
            range_start_m: dopp_range_m,
            gate_size_m: dopp_gate_m,
            gates: gates.iter().map(|&b| b as i16).collect(),
        });
    }

    RawRadial {
        elevation_number: m.elevation_number() as u8,
        elevation_deg: m.elevation_angle(),
        azimuth_deg: m.azimuth_angle(),
        azimuth_width_deg: 1.0,
        time: m.date_time(),
        moments,
    }
}

/// Convert a message-31 generic data block (REF/VEL/SW/ZDR/PHI/RHO) into a
/// [`RawMoment`] with raw i16 DNs and per-block calibration.
fn generic_to_raw(kind: Moment, block: &GenericDataBlock<'_>) -> Option<RawMoment> {
    let header = block.header();
    let word_size_bytes = header.data_word_size_raw() as usize / 8;
    if word_size_bytes != 1 && word_size_bytes != 2 {
        log::warn!(
            "skipping {kind:?} block with unusual word size {}",
            header.data_word_size_raw()
        );
        return None;
    }

    let gate_count = header.number_of_data_moment_gates() as usize;
    let mut gates = decode_gates(block.encoded_values(), word_size_bytes, gate_count);
    // Defensive: pad/truncate so the row length always matches gate_count.
    gates.resize(gate_count, 0);

    Some(RawMoment {
        kind,
        scale: header.scale(),
        offset: header.offset(),
        // SNR threshold is a signed value scaled by 10 (-12.0..20.0 dB).
        threshold: header.snr_threshold_raw() as f32 * 0.1,
        range_start_m: header.data_moment_range_raw() as f32,
        gate_size_m: header.data_moment_range_sample_interval_raw() as f32,
        gates,
    })
}

/// Decode big-endian gate DNs (8- or 16-bit words) to `i16`. 16-bit DNs are
/// unsigned in practice (offset/scale encoding keeps them < 32768); saturate
/// rather than wrap if that expectation is ever violated.
fn decode_gates(bytes: &[u8], word_size_bytes: usize, gate_count: usize) -> Vec<i16> {
    if word_size_bytes == 2 {
        bytes
            .chunks_exact(2)
            .take(gate_count)
            .map(|c| {
                let raw = u16::from_be_bytes([c[0], c[1]]);
                i16::try_from(raw).unwrap_or(i16::MAX)
            })
            .collect()
    } else {
        bytes.iter().take(gate_count).map(|&b| b as i16).collect()
    }
}

/// Build one [`Sweep`] from a group of radials sharing an elevation number.
fn build_sweep(radials: Vec<RawRadial>, vcp: Option<&VcpInfo>) -> Sweep {
    // Measured elevation: median over the cut (robust to transitional rays).
    let mut measured: Vec<f32> = radials.iter().map(|r| r.elevation_deg).collect();
    measured.sort_by(f32::total_cmp);
    let mid = measured.len() / 2;
    let elevation_measured_deg = if measured.len() % 2 == 1 {
        measured[mid]
    } else {
        (measured[mid - 1] + measured[mid]) / 2.0
    };

    // Requested tilt from the VCP when it plausibly matches this cut,
    // otherwise fall back to the measured median. (SAILS/MRLE supplemental
    // cuts may carry elevation numbers past the VCP list.)
    let elevation_deg = vcp
        .and_then(|v| {
            (radials[0].elevation_number as usize)
                .checked_sub(1)
                .and_then(|i| v.cut_elevations_deg.get(i).copied())
        })
        .map(|angle| angle as f32)
        .filter(|angle| (angle - elevation_measured_deg).abs() < 2.0)
        .map(|angle| (angle * 10.0).round() / 10.0)
        .unwrap_or(elevation_measured_deg);

    let azimuth_width_deg = radials[0].azimuth_width_deg;
    let azimuths_deg: Vec<f32> = radials.iter().map(|r| r.azimuth_deg).collect();

    // Ray-major moment assembly. Gate geometry is taken from the first ray
    // that carries the moment; rays missing it (or with different gate
    // counts, rare at cut transitions) are padded with DN 0 = "no data".
    let mut moments = Vec::new();
    for kind in MOMENT_ORDER {
        let Some(template) = radials
            .iter()
            .find_map(|r| r.moments.iter().find(|m| m.kind == kind))
        else {
            continue;
        };
        let gate_count = template.gates.len();
        let mut data: Vec<i16> = Vec::with_capacity(gate_count * radials.len());
        for radial in &radials {
            let start = data.len();
            if let Some(m) = radial.moments.iter().find(|m| m.kind == kind) {
                data.extend(m.gates.iter().take(gate_count).copied());
            }
            data.resize(start + gate_count, 0);
        }
        moments.push(MomentData {
            kind,
            calibration: MomentCalibration {
                scale: template.scale,
                offset: template.offset,
                threshold: template.threshold,
            },
            range_start_m: template.range_start_m,
            gate_size_m: template.gate_size_m,
            gate_count,
            ray_count: radials.len(),
            data,
        });
    }

    Sweep {
        elevation_deg,
        elevation_measured_deg,
        azimuth_width_deg,
        azimuths_deg,
        moments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_name_mapping_covers_aliases() {
        assert_eq!(map_block_name(b"REF"), Some(Moment::Reflectivity));
        assert_eq!(map_block_name(b"CZ "), Some(Moment::Reflectivity));
        assert_eq!(map_block_name(b"VEL"), Some(Moment::Velocity));
        assert_eq!(map_block_name(b"VR "), Some(Moment::Velocity));
        assert_eq!(map_block_name(b"SW "), Some(Moment::SpectrumWidth));
        assert_eq!(map_block_name(b"SV "), Some(Moment::SpectrumWidth));
        assert_eq!(
            map_block_name(b"ZDR"),
            Some(Moment::DifferentialReflectivity)
        );
        assert_eq!(
            map_block_name(b"DR "),
            Some(Moment::DifferentialReflectivity)
        );
        assert_eq!(map_block_name(b"PHI"), Some(Moment::DifferentialPhase));
        assert_eq!(map_block_name(b"DP "), Some(Moment::DifferentialPhase));
        assert_eq!(
            map_block_name(b"KDP"),
            Some(Moment::SpecificDifferentialPhase)
        );
        assert_eq!(
            map_block_name(b"KD "),
            Some(Moment::SpecificDifferentialPhase)
        );
        assert_eq!(map_block_name(b"RHO"), Some(Moment::CorrelationCoefficient));
        assert_eq!(map_block_name(b"CC "), Some(Moment::CorrelationCoefficient));
        assert_eq!(map_block_name(b"RF "), Some(Moment::CorrelationCoefficient));
        assert_eq!(map_block_name(b"CFP"), None);
    }

    #[test]
    fn decode_gates_word_sizes() {
        assert_eq!(decode_gates(&[0, 1, 255], 1, 3), vec![0, 1, 255]);
        // 16-bit big-endian, including a value above i16::MAX (saturates).
        assert_eq!(
            decode_gates(&[0x01, 0x02, 0x7f, 0xff, 0xff, 0xff], 2, 3),
            vec![0x0102, 0x7fff, i16::MAX]
        );
        // Truncates to the declared gate count.
        assert_eq!(decode_gates(&[1, 2, 3], 1, 2), vec![1, 2]);
    }

    #[test]
    fn split_ldm_records_parses_size_prefixed_records() {
        let mut data = Vec::new();
        for payload in [b"abc".as_slice(), b"defgh".as_slice()] {
            data.extend_from_slice(&(payload.len() as i32).to_be_bytes());
            data.extend_from_slice(payload);
        }
        let records = split_ldm_records(&data).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].data(), [0, 0, 0, 3, b'a', b'b', b'c']);
        assert_eq!(records[1].data().len(), 9);
    }

    #[test]
    fn split_ldm_records_tolerates_trailing_junk() {
        let mut data = Vec::new();
        data.extend_from_slice(&3i32.to_be_bytes());
        data.extend_from_slice(b"abc");
        data.extend_from_slice(&[0xde, 0xad]); // truncated header -> dropped
        let records = split_ldm_records(&data).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn empty_input_is_unsupported() {
        assert!(decode_volume_file(&[]).is_err());
        assert!(decode_realtime_volume(&[]).is_err());
    }
}
