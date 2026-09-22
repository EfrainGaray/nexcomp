use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, IsTerminal};
use thiserror::Error;
use zeroize::Zeroizing;

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
    #[error("Crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("Adaptive container error: {0}")]
    Adaptive(#[from] adaptive::AdaptiveError),
    #[error("pre-release {0} file: read it with nexcomp 1.5.0 or earlier")]
    PreRelease(&'static str),
    #[error("Unknown format")]
    UnknownFormat,
    #[error("encrypted input needs a password: use --password-file, {PASSWORD_ENV} or run in a terminal")]
    NoPassword,
    #[error("passwords do not match")]
    PasswordMismatch,
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
        /// Encrypt the output. The password comes from --password-file,
        /// NEXCOMP_PASSWORD or a prompt; giving it here is deprecated.
        #[arg(long, num_args = 0..=1, value_name = "PASSWORD")]
        encrypt: Option<Option<String>>,
        /// Read the password from this file (trailing newlines are ignored).
        #[arg(long, value_name = "FILE", requires = "encrypt")]
        password_file: Option<String>,
        #[arg(long)]
        verbose: bool,
    },
    Decompress {
        input: String,
        output: String,
        /// Deprecated: password of an encrypted input.
        #[arg(long, value_name = "PASSWORD")]
        decrypt: Option<String>,
        /// Read the password from this file (trailing newlines are ignored).
        #[arg(long, value_name = "FILE")]
        password_file: Option<String>,
    },
    Inspect {
        input: String,
        /// Deprecated: password of an encrypted input.
        #[arg(long, value_name = "PASSWORD")]
        decrypt: Option<String>,
        /// Read the password from this file (trailing newlines are ignored).
        #[arg(long, value_name = "FILE")]
        password_file: Option<String>,
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

/// Environment variable holding the password when no --password-file is given.
const PASSWORD_ENV: &str = "NEXCOMP_PASSWORD";

/// Resolve the password: the deprecated command-line value, then
/// --password-file, then NEXCOMP_PASSWORD, then a prompt on the terminal
/// (asked twice when encrypting).
fn password(argv: Option<String>, file: Option<&str>, confirm: bool) -> Result<Zeroizing<String>, NexcompError> {
    if let Some(pw) = argv {
        eprintln!(
            "warning: a password on the command line is visible to other users and kept in shell history; \
             use --password-file, {PASSWORD_ENV} or the prompt"
        );
        return Ok(Zeroizing::new(pw));
    }
    if let Some(path) = file {
        let mut pw = Zeroizing::new(fs::read_to_string(path)?);
        let len = pw.trim_end_matches(['\n', '\r']).len();
        pw.truncate(len);
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var(PASSWORD_ENV) {
        return Ok(Zeroizing::new(pw));
    }
    if !io::stdin().is_terminal() {
        return Err(NexcompError::NoPassword);
    }
    let pw = Zeroizing::new(rpassword::prompt_password("Password: ")?);
    if confirm && *pw != *Zeroizing::new(rpassword::prompt_password("Repeat password: ")?) {
        return Err(NexcompError::PasswordMismatch);
    }
    Ok(pw)
}

/// Strip the encryption wrapper (NXE2 or NXE3) if the file has one.
fn open_payload(file_data: Vec<u8>, argv: Option<String>, password_file: Option<&str>) -> Result<Vec<u8>, NexcompError> {
    if !crypto::is_sealed(&file_data) {
        return Ok(file_data);
    }
    let pw = password(argv, password_file, false)?;
    Ok(crypto::open(&file_data, pw.as_bytes())?)
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
            password_file,
            verbose,
        } => {
            let input_data = fs::read(&input)?;
            // Ask before compressing, which can take minutes.
            let pw = encrypt.map(|argv| password(argv, password_file.as_deref(), true)).transpose()?;
            eprintln!("Compressing {} ({} bytes)...", input, input_data.len());

            let compressed = compress_data(&input_data, verbose);

            let final_data = match pw {
                Some(pw) => crypto::seal(&compressed, pw.as_bytes(), input_data.len() as u64, crypto::DEFAULT_KDF)?,
                None => compressed,
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
            password_file,
        } => {
            let file_data = fs::read(&input)?;
            eprintln!("Decompressing to {} ...", output);

            let payload = open_payload(file_data, decrypt, password_file.as_deref())?;

            let decompressed = decompress_data(&payload)?;

            fs::write(&output, &decompressed)?;
            eprintln!("Done: {} bytes restored.", decompressed.len());
        }
        Commands::Inspect {
            input,
            decrypt,
            password_file,
            show_codec,
        } => {
            let file_data = fs::read(&input)?;

            let payload = open_payload(file_data, decrypt, password_file.as_deref())?;

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
