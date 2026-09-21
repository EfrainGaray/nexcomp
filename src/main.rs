use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use thiserror::Error;

use nexcomp::adaptive::{self, CodecId};
use nexcomp::classifier_v2::CodecChoice;
use nexcomp::crypto;
use nexcomp::entropy::{
    build_decode_table, build_table, rans_decode, RansError,
};
use nexcomp::grammar::{self, repair_decode, repair_deserialize};
use nexcomp::selector::{self, SelectorError};
use nexcomp::transform::inverse_transform;
use nexcomp::classifier::DomainType;
use nexcomp::lz77;

const LEGACY_MAGIC: &[u8; 4] = b"NXC\x01";
const ADAPTIVE_MAGIC: &[u8; 4] = b"NX13";
const LEGACY_VERSION: u8 = 0x01;
const VERSION: u8 = 0x02;

const FILE_MODE_BASELINE: u8 = 0;
const FILE_MODE_ADAPTIVE: u8 = 1;
const FILE_MODE_RAW: u8 = 2;

#[derive(Error, Debug)]
enum NexcompError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Invalid file: bad magic bytes")]
    BadMagic,
    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),
    #[error("Invalid file header")]
    InvalidHeader,
    #[error("Truncated payload")]
    TruncatedPayload,
    #[error("Invalid payload: {0}")]
    InvalidPayload(&'static str),
    #[error("Entropy coder error: {0}")]
    Entropy(#[from] RansError),
    #[error("Grammar error: {0}")]
    Grammar(#[from] grammar::RepairError),
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("Selector error: {0}")]
    Selector(#[from] SelectorError),
    #[error("LZ77 error: {0}")]
    Lz77(String),
    #[error("Decompression size mismatch: expected {expected}, got {got}")]
    SizeMismatch { expected: usize, got: usize },
    #[error("Unknown format")]
    UnknownFormat,
}

#[derive(Parser)]
#[command(name = "nexcomp", version = "1.2.0")]
#[command(about = "NEXCOMP v1.2 — adaptive lossless compressor")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Compress {
        input: String,
        output: String,
        #[arg(long)]
        encrypt: Option<String>,
        #[arg(long)]
        verbose: bool,
    },
    Decompress {
        input: String,
        output: String,
        #[arg(long)]
        decrypt: Option<String>,
    },
    Inspect {
        input: String,
        #[arg(long)]
        decrypt: Option<String>,
        #[arg(long)]
        show_codec: bool,
    },
}

// ---------------------------------------------------------------------------
// New adaptive v1.2 compress / decompress
// ---------------------------------------------------------------------------

fn compress_data(data: &[u8], verbose: bool) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    // Use the adaptive selector (output already includes "NX13" magic header)
    let compressed = adaptive::adaptive_compress(data);

    if verbose {
        let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
        let codec = CodecId::from_u8(compressed[8]);
        eprintln!(
            "  codec={} {:.3} bpb ({} -> {} bytes, {:.1}% ratio)",
            codec.name(),
            bpb,
            data.len(),
            compressed.len(),
            compressed.len() as f64 / data.len() as f64 * 100.0
        );
    }

    compressed
}

fn decompress_data(data: &[u8]) -> Result<Vec<u8>, NexcompError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    if data.len() >= 4 && &data[0..4] == ADAPTIVE_MAGIC {
        // Adaptive v1.2 format
        Ok(adaptive::adaptive_decompress(data))
    } else if data.len() >= 4 && &data[0..4] == LEGACY_MAGIC {
        // Legacy NXC\x01 format — delegate to the full legacy decompression
        decompress_legacy_file(data)
    } else {
        Err(NexcompError::UnknownFormat)
    }
}

// ---------------------------------------------------------------------------
// Legacy decompression support (NXC\x01 files)
// ---------------------------------------------------------------------------

struct ParsedLegacyHeader<'a> {
    version: u8,
    #[allow(dead_code)]
    encrypted: bool,
    stored: bool,
    original_size: usize,
    domain_map: &'a [u8],
    header_end: usize,
}

fn parse_legacy_header(file_data: &[u8]) -> Result<ParsedLegacyHeader<'_>, NexcompError> {
    if file_data.len() < 18 || &file_data[..4] != LEGACY_MAGIC {
        return Err(NexcompError::BadMagic);
    }

    let version = file_data[4];
    let flags = file_data[5];
    let encrypted = (flags & 0x01) != 0;
    let stored = (flags & 0x02) != 0;
    let dm_len = u32::from_le_bytes([file_data[6], file_data[7], file_data[8], file_data[9]]) as usize;
    let header_end = 18usize
        .checked_add(dm_len)
        .ok_or(NexcompError::InvalidHeader)?;
    if file_data.len() < header_end {
        return Err(NexcompError::InvalidHeader);
    }
    let domain_map = &file_data[10..10 + dm_len];
    let size_off = 10 + dm_len;
    let original_size = u64::from_le_bytes([
        file_data[size_off],
        file_data[size_off + 1],
        file_data[size_off + 2],
        file_data[size_off + 3],
        file_data[size_off + 4],
        file_data[size_off + 5],
        file_data[size_off + 6],
        file_data[size_off + 7],
    ]) as usize;

    Ok(ParsedLegacyHeader {
        version,
        encrypted,
        stored,
        original_size,
        domain_map,
        header_end,
    })
}

fn read_u32(payload: &[u8], cursor: &mut usize) -> Result<u32, NexcompError> {
    if payload.len().saturating_sub(*cursor) < 4 {
        return Err(NexcompError::TruncatedPayload);
    }
    let value = u32::from_le_bytes([
        payload[*cursor],
        payload[*cursor + 1],
        payload[*cursor + 2],
        payload[*cursor + 3],
    ]);
    *cursor += 4;
    Ok(value)
}

fn read_u64(payload: &[u8], cursor: &mut usize) -> Result<u64, NexcompError> {
    if payload.len().saturating_sub(*cursor) < 8 {
        return Err(NexcompError::TruncatedPayload);
    }
    let value = u64::from_le_bytes([
        payload[*cursor],
        payload[*cursor + 1],
        payload[*cursor + 2],
        payload[*cursor + 3],
        payload[*cursor + 4],
        payload[*cursor + 5],
        payload[*cursor + 6],
        payload[*cursor + 7],
    ]);
    *cursor += 8;
    Ok(value)
}

fn decompress_v2_payload(payload: &[u8]) -> Result<Vec<u8>, NexcompError> {
    if payload.is_empty() {
        return Err(NexcompError::InvalidPayload("missing file mode"));
    }

    match payload[0] {
        FILE_MODE_BASELINE => selector::decompress_lz77_huffman(&payload[1..]).map_err(Into::into),
        FILE_MODE_RAW => Ok(payload[1..].to_vec()),
        FILE_MODE_ADAPTIVE => {
            let mut cursor = 1;
            let block_count = read_u32(payload, &mut cursor)? as usize;
            let mut output = Vec::new();
            for _ in 0..block_count {
                let orig_len = read_u32(payload, &mut cursor)? as usize;
                if payload.len() <= cursor {
                    return Err(NexcompError::TruncatedPayload);
                }
                let codec = CodecChoice::from_codec_id(payload[cursor]);
                cursor += 1;
                let payload_len = read_u32(payload, &mut cursor)? as usize;
                if payload.len().saturating_sub(cursor) < payload_len {
                    return Err(NexcompError::TruncatedPayload);
                }
                let block_payload = &payload[cursor..cursor + payload_len];
                cursor += payload_len;
                let block = selector::decompress_block(codec, block_payload)?;
                if block.len() != orig_len {
                    return Err(NexcompError::SizeMismatch {
                        expected: orig_len,
                        got: block.len(),
                    });
                }
                output.extend_from_slice(&block);
            }
            Ok(output)
        }
        _ => Err(NexcompError::InvalidPayload("unknown file mode")),
    }
}

fn decompress_v1_legacy(
    payload: &[u8],
    domain_map: &[u8],
    _original_size: usize,
) -> Result<Vec<u8>, NexcompError> {
    let mut offset = 0;

    if payload.len() < 5 {
        return Err(NexcompError::TruncatedPayload);
    }

    let use_lz77 = payload[offset] == 0x01;
    offset += 1;

    let block_count = read_u32(payload, &mut offset)? as usize;
    let mut block_sizes = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        block_sizes.push(read_u32(payload, &mut offset)? as usize);
    }

    if payload.len() <= offset {
        return Err(NexcompError::TruncatedPayload);
    }
    let mode_flag = payload[offset];
    offset += 1;

    let grammar_bytes = if mode_flag == 0x01 {
        let grammar_len = read_u64(payload, &mut offset)? as usize;
        let p_sym_len = read_u32(payload, &mut offset)? as usize;
        let i_sym_len = read_u32(payload, &mut offset)? as usize;

        if payload.len().saturating_sub(offset) < 1024 {
            return Err(NexcompError::TruncatedPayload);
        }

        let mut freqs_p = vec![0u32; 256];
        for freq in &mut freqs_p {
            *freq = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as u32;
            offset += 2;
        }

        let mut freqs_i = vec![0u32; 256];
        for freq in &mut freqs_i {
            *freq = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as u32;
            offset += 2;
        }

        let encoded_p_len = read_u32(payload, &mut offset)? as usize;
        if payload.len().saturating_sub(offset) < encoded_p_len {
            return Err(NexcompError::TruncatedPayload);
        }
        let encoded_p = &payload[offset..offset + encoded_p_len];
        offset += encoded_p_len;
        let encoded_i = &payload[offset..];

        let table_p = build_table(&freqs_p)?;
        let dtable_p = build_decode_table(&table_p);
        let p_data = rans_decode(encoded_p, &dtable_p, p_sym_len)?;

        let table_i = build_table(&freqs_i)?;
        let dtable_i = build_decode_table(&table_i);
        let i_data = rans_decode(encoded_i, &dtable_i, i_sym_len)?;

        nexcomp::ajedrez::merge(
            &nexcomp::ajedrez::AjedrezStreams {
                p: p_data,
                i: i_data,
                original_len: grammar_len,
            },
            nexcomp::ajedrez::BLOCK_WIDTH,
        )
    } else {
        if payload.len().saturating_sub(offset) < 512 {
            return Err(NexcompError::TruncatedPayload);
        }
        let mut freqs = vec![0u32; 256];
        for freq in &mut freqs {
            *freq = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as u32;
            offset += 2;
        }

        let grammar_len = read_u64(payload, &mut offset)? as usize;
        let rans_data = &payload[offset..];

        let table = build_table(&freqs)?;
        let dtable = build_decode_table(&table);
        rans_decode(rans_data, &dtable, grammar_len)?
    };

    let grammar_result = repair_deserialize(&grammar_bytes)?;
    let transformed_data = repair_decode(&grammar_result);

    let mut working_data = Vec::new();
    let mut pos = 0;
    for (idx, &size) in block_sizes.iter().enumerate() {
        if transformed_data.len().saturating_sub(pos) < size {
            return Err(NexcompError::TruncatedPayload);
        }
        let block = &transformed_data[pos..pos + size];
        let domain = DomainType::from(*domain_map.get(idx).unwrap_or(&0));
        let original = inverse_transform(block, domain);
        working_data.extend_from_slice(&original);
        pos += size;
    }

    if use_lz77 {
        lz77::decompress(&working_data).map_err(|err| NexcompError::Lz77(err.to_string()))
    } else {
        Ok(working_data)
    }
}

/// Decompress a full legacy NXC\x01 file (with its own header).
fn decompress_legacy_file(file_data: &[u8]) -> Result<Vec<u8>, NexcompError> {
    let parsed = parse_legacy_header(file_data)?;
    if parsed.version != LEGACY_VERSION && parsed.version != VERSION {
        return Err(NexcompError::UnsupportedVersion(parsed.version));
    }

    let payload = &file_data[parsed.header_end..];

    let decompressed = match parsed.version {
        LEGACY_VERSION => {
            if parsed.stored {
                payload.to_vec()
            } else {
                decompress_v1_legacy(payload, parsed.domain_map, parsed.original_size)?
            }
        }
        VERSION => decompress_v2_payload(payload)?,
        other => return Err(NexcompError::UnsupportedVersion(other)),
    };

    if decompressed.len() != parsed.original_size {
        return Err(NexcompError::SizeMismatch {
            expected: parsed.original_size,
            got: decompressed.len(),
        });
    }

    Ok(decompressed)
}

// ---------------------------------------------------------------------------
// Inspect support (for legacy NXC\x01 files)
// ---------------------------------------------------------------------------

fn baseline_variant_label(tag: u8) -> &'static str {
    match tag {
        0 => "Passthrough",
        1 => "Lz77Huffman/blk4k",
        2 => "Lz77Huffman/blk8k",
        3 => "Lz77Huffman/blk16k",
        4 => "Lz77Huffman/ctx1",
        5 => "Lz77Huffman/split",
        6 => "Lz77Huffman/global",
        7 => "Lz77Huffman/blk32k",
        8 => "Lz77Huffman/blk64k",
        _ => "Lz77Huffman/unknown",
    }
}

fn inspect_codecs(payload: &[u8]) -> Result<String, NexcompError> {
    if payload.is_empty() {
        return Err(NexcompError::InvalidPayload("missing file mode"));
    }

    match payload[0] {
        FILE_MODE_RAW => Ok("Passthrough".to_string()),
        FILE_MODE_BASELINE => {
            if payload.len() < 2 {
                return Err(NexcompError::TruncatedPayload);
            }
            Ok(baseline_variant_label(payload[1]).to_string())
        }
        FILE_MODE_ADAPTIVE => {
            let mut cursor = 1;
            let block_count = read_u32(payload, &mut cursor)? as usize;
            let mut seen = [false; 5];

            for _ in 0..block_count {
                let _orig_len = read_u32(payload, &mut cursor)? as usize;
                if payload.len() <= cursor {
                    return Err(NexcompError::TruncatedPayload);
                }
                let codec = CodecChoice::from_codec_id(payload[cursor]);
                seen[codec.codec_id() as usize] = true;
                cursor += 1;
                let payload_len = read_u32(payload, &mut cursor)? as usize;
                if payload.len().saturating_sub(cursor) < payload_len {
                    return Err(NexcompError::TruncatedPayload);
                }
                cursor += payload_len;
            }

            let labels: Vec<String> = seen
                .iter()
                .enumerate()
                .filter_map(|(idx, present)| {
                    if *present {
                        Some(CodecChoice::from_codec_id(idx as u8).to_string())
                    } else {
                        None
                    }
                })
                .collect();

            if labels.len() == 1 {
                Ok(labels[0].clone())
            } else {
                Ok(format!("Mixed({})", labels.join("+")))
            }
        }
        _ => Err(NexcompError::InvalidPayload("unknown file mode")),
    }
}

// ---------------------------------------------------------------------------
// Encryption wrapper format for the new adaptive pipeline
//
// When --encrypt is used, the on-disk format is:
//   [4B "NXE1"][4B orig_file_len LE][encrypted blob of adaptive_compress output]
//
// The AAD for AEAD is the 8-byte header itself so the sizes are authenticated.
// ---------------------------------------------------------------------------

const ENCRYPT_MAGIC: &[u8; 4] = b"NXE1";

fn main() -> Result<(), NexcompError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Compress {
            input,
            output,
            encrypt,
            verbose,
        } => {
            let input_data = fs::read(&input)?;
            eprintln!("Compressing {} ({} bytes)...", input, input_data.len());

            let compressed = compress_data(&input_data, verbose);

            // Optionally encrypt
            let final_data = if let Some(password) = encrypt {
                let key = Sha256::digest(password.as_bytes());
                // Build a small header as AAD
                let mut aad = Vec::with_capacity(8);
                aad.extend_from_slice(ENCRYPT_MAGIC);
                aad.extend_from_slice(&(input_data.len() as u32).to_le_bytes());
                let encrypted = crypto::encrypt(&compressed, &key, &aad)?;
                let mut out = aad;
                out.extend_from_slice(&encrypted);
                out
            } else {
                compressed
            };

            fs::write(&output, &final_data)?;

            let ratio = if input_data.is_empty() {
                0.0
            } else {
                final_data.len() as f64 / input_data.len() as f64
            };
            let bpb = ratio * 8.0;
            eprintln!(
                "Done: {} -> {} bytes ({:.1}% ratio, {:.3} bpb)",
                input_data.len(),
                final_data.len(),
                ratio * 100.0,
                bpb
            );
        }
        Commands::Decompress {
            input,
            output,
            decrypt,
        } => {
            let file_data = fs::read(&input)?;
            eprintln!("Decompressing to {} ...", output);

            // Detect format
            let payload = if file_data.len() >= 4 && &file_data[0..4] == ENCRYPT_MAGIC {
                // Encrypted adaptive format
                let pw = decrypt.ok_or_else(|| {
                    io::Error::other("File is encrypted; provide --decrypt <password>")
                })?;
                let key = Sha256::digest(pw.as_bytes());
                let aad = &file_data[0..8];
                let ciphertext = &file_data[8..];
                crypto::decrypt(ciphertext, &key, aad)?
            } else {
                file_data.clone()
            };

            let decompressed = decompress_data(&payload)?;

            fs::write(&output, &decompressed)?;
            eprintln!("Done: {} bytes restored.", decompressed.len());
        }
        Commands::Inspect {
            input,
            decrypt,
            show_codec,
        } => {
            let file_data = fs::read(&input)?;

            // Handle encrypted files
            let payload = if file_data.len() >= 4 && &file_data[0..4] == ENCRYPT_MAGIC {
                let pw = decrypt.ok_or_else(|| {
                    io::Error::other("File is encrypted; provide --decrypt <password>")
                })?;
                let key = Sha256::digest(pw.as_bytes());
                let aad = &file_data[0..8];
                let ciphertext = &file_data[8..];
                crypto::decrypt(ciphertext, &key, aad)?
            } else {
                file_data.clone()
            };

            if payload.len() >= 4 && &payload[0..4] == ADAPTIVE_MAGIC {
                // New adaptive format
                let codec = if payload.len() >= 9 {
                    CodecId::from_u8(payload[8])
                } else {
                    CodecId::Passthrough
                };
                let orig_len = if payload.len() >= 8 {
                    u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize
                } else {
                    0
                };

                if show_codec {
                    println!("{}", codec.name());
                } else {
                    println!("version=1.2 size={} codec={}", orig_len, codec.name());
                }
            } else if payload.len() >= 4 && &payload[0..4] == LEGACY_MAGIC {
                // Legacy format — use the old inspect path
                let parsed = parse_legacy_header(&payload)?;
                let inner_payload = &payload[parsed.header_end..];

                let summary = match parsed.version {
                    LEGACY_VERSION => "LegacyV1".to_string(),
                    VERSION => inspect_codecs(inner_payload)?,
                    other => return Err(NexcompError::UnsupportedVersion(other)),
                };

                if show_codec {
                    println!("{summary}");
                } else {
                    println!(
                        "version={} size={} codec={}",
                        parsed.version, parsed.original_size, summary
                    );
                }
            } else {
                return Err(NexcompError::UnknownFormat);
            }
        }
    }

    Ok(())
}
