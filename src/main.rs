use clap::{Parser, Subcommand};
use std::fs;
use std::io;
use thiserror::Error;

use nexcomp::adaptive;
use nexcomp::crypto;

const ADAPTIVE_MAGIC: &[u8; 4] = b"NX13";
/// Formats written by nexcomp 1.5.0 and earlier research builds. Their LZMA,
/// PPM and BWT streams predate the current codecs and carry no checksums, so
/// decoding them here could return wrong data; they are refused instead.
const PRE_RELEASE_MAGICS: [(&[u8; 4], &str); 3] = [(b"NXC\x01", "NXC1"), (b"NX12", "NX12"), (b"NXE1", "NXE1")];

#[derive(Error, Debug)]
enum NexcompError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Truncated payload")]
    TruncatedPayload,
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("Adaptive container error: {0}")]
    Adaptive(#[from] adaptive::AdaptiveError),
    #[error("pre-release {0} file: read it with nexcomp 1.5.0 or earlier")]
    PreRelease(&'static str),
    #[error("Unknown format")]
    UnknownFormat,
}

fn pre_release(data: &[u8]) -> Option<&'static str> {
    PRE_RELEASE_MAGICS.iter().find(|(magic, _)| data.starts_with(*magic)).map(|&(_, name)| name)
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
        eprintln!(
            "  codec={} {:.3} bpb ({} -> {} bytes, {:.1}% ratio)",
            adaptive::codec_summary(&compressed).expect("freshly written container parses"),
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
    if data.starts_with(ADAPTIVE_MAGIC) {
        return Ok(adaptive::try_adaptive_decompress(data)?);
    }
    Err(pre_release(data).map_or(NexcompError::UnknownFormat, NexcompError::PreRelease))
}

// ---------------------------------------------------------------------------
// Encryption wrapper format for the new adaptive pipeline
//
// When --encrypt is used, the on-disk format is:
//   [4B "NXE2"][4B orig_file_len LE][encrypted blob of adaptive_compress output]
//
// The AAD for AEAD is the 8-byte header itself so the sizes are authenticated.
// ---------------------------------------------------------------------------

const ENCRYPT_MAGIC: &[u8; 4] = b"NXE2";
const ENCRYPT_HEADER_LEN: usize = 8;

/// Strip the encryption wrapper if the file has one, decrypting with `password`.
fn open_payload(file_data: Vec<u8>, password: Option<String>) -> Result<Vec<u8>, NexcompError> {
    if !file_data.starts_with(ENCRYPT_MAGIC) {
        return Ok(file_data);
    }
    if file_data.len() < ENCRYPT_HEADER_LEN {
        return Err(NexcompError::TruncatedPayload);
    }
    let pw = password
        .ok_or_else(|| io::Error::other("File is encrypted; provide --decrypt <password>"))?;
    let (aad, ciphertext) = file_data.split_at(ENCRYPT_HEADER_LEN);
    Ok(crypto::decrypt(ciphertext, pw.as_bytes(), aad)?)
}

fn main() {
    if let Err(err) = run(Cli::parse()) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), NexcompError> {

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
                let key = password.as_bytes();
                // Build a small header as AAD
                let mut aad = Vec::with_capacity(8);
                aad.extend_from_slice(ENCRYPT_MAGIC);
                aad.extend_from_slice(&(input_data.len() as u32).to_le_bytes());
                let encrypted = crypto::encrypt(&compressed, key, &aad)?;
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

            let payload = open_payload(file_data, decrypt)?;

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

            let payload = open_payload(file_data, decrypt)?;

            if !payload.starts_with(ADAPTIVE_MAGIC) {
                return Err(pre_release(&payload).map_or(NexcompError::UnknownFormat, NexcompError::PreRelease));
            }
            let (orig_len, _) = adaptive::parse_blocks(&payload)?;
            let codec = adaptive::codec_summary(&payload)?;
            if show_codec {
                println!("{codec}");
            } else {
                println!("version=1.3 size={orig_len} codec={codec}");
            }
        }
    }

    Ok(())
}
