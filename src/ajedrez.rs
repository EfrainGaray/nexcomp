use std::cmp::Reverse;
// NEXCOMP — Separación P|I por paridad geométrica ("técnica ajedrez")
//
// Dado un stream de bytes en posición i dentro de un bloque de ancho W:
//   fila = i / W
//   col  = i % W
//   paridad = (fila + col) % 2
//
//   paridad == 0 → byte va al sub-stream P (pares), valor directo
//   paridad == 1 → byte va al sub-stream I (impares), valor XOR 0xFF
//
// XOR 0xFF en I: alinea sesgo de I hacia 0x00 igual que P,
// facilitando tabla rANS sesgada hacia frecuencias bajas.
//
// Reversible exacto. Sin pérdida.

/// Ancho por defecto para calcular (fila, col) en la separación.
pub const BLOCK_WIDTH: usize = 256;

/// Resultado de la separación ajedrez.
pub struct AjedrezStreams {
    /// Posiciones pares: valor directo
    pub p: Vec<u8>,
    /// Posiciones impares: valor XOR 0xFF
    pub i: Vec<u8>,
    /// Longitud del stream original (p.len() + i.len())
    pub original_len: usize,
}

/// Separa un stream en dos sub-streams por paridad posicional.
///
/// Para cada byte en `data[idx]`:
///   fila = idx / width
///   col  = idx % width
///   if (fila + col) % 2 == 0 → P (valor directo)
///   if (fila + col) % 2 == 1 → I (valor XOR 0xFF)
///
/// Complejidad: O(n) tiempo, O(n) espacio.
pub fn split(data: &[u8], width: usize) -> AjedrezStreams {
    let w = if width == 0 { 1 } else { width };
    let mut p = Vec::with_capacity(data.len() / 2 + 1);
    let mut i = Vec::with_capacity(data.len() / 2 + 1);

    for (idx, &byte) in data.iter().enumerate() {
        let fila = idx / w;
        let col = idx % w;
        let paridad = (fila + col) % 2;
        if paridad == 0 {
            p.push(byte);
        } else {
            // XOR 0xFF: alinea sesgo de I hacia 0x00
            // igual que P, facilita tabla rANS sesgada
            i.push(byte ^ 0xFF);
        }
    }

    AjedrezStreams {
        original_len: data.len(),
        p,
        i,
    }
}

/// Reconstruye el stream original desde P e I.
/// Inversa exacta de split().
///
/// Recorre posiciones 0..original_len, calcula paridad,
/// toma de P o I según corresponda (deshaciendo XOR 0xFF para I).
pub fn merge(streams: &AjedrezStreams, width: usize) -> Vec<u8> {
    let w = if width == 0 { 1 } else { width };
    let mut result = Vec::with_capacity(streams.original_len);
    let mut p_idx: usize = 0;
    let mut i_idx: usize = 0;

    for idx in 0..streams.original_len {
        let fila = idx / w;
        let col = idx % w;
        let paridad = (fila + col) % 2;
        if paridad == 0 {
            result.push(streams.p[p_idx]);
            p_idx += 1;
        } else {
            // Invertir XOR 0xFF: byte_original = stored ^ 0xFF
            result.push(streams.i[i_idx] ^ 0xFF);
            i_idx += 1;
        }
    }

    result
}

/// Calcula la entropía de Shannon en bits por byte.
/// H(X) = -Σ p(x) · log₂(p(x)) para x ∈ 0..=255
///
/// Retorna 0.0 para datos vacíos.
pub fn entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    let mut h = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.log2();
        }
    }
    h
}

/// Build frequency table from pre-computed u64 counts.
/// Same normalization as build_freq_table but takes counts directly.
pub fn build_freq_table_from_counts(counts: &[u64; 256]) -> [u32; 256] {
    const TOTAL: u32 = 4096;
    let total_count: u64 = counts.iter().sum();
    if total_count == 0 {
        let base = TOTAL / 256;
        let rem = TOTAL - base * 256;
        let mut freqs = [base; 256];
        for f in freqs.iter_mut().take(rem as usize) {
            *f += 1;
        }
        return freqs;
    }

    let mut freqs = [0u32; 256];
    let mut nonzero_count = 0u32;
    for i in 0..256 {
        if counts[i] > 0 {
            nonzero_count += 1;
        }
    }

    // If too many nonzero symbols to fit with minimum freq=1 each, use proportional only
    if nonzero_count > TOTAL {
        // Degenerate: more symbols than quanta. Give each nonzero symbol 1.
        // This shouldn't happen with 256 symbols and TOTAL=4096, but safety first.
        let mut assigned = 0u32;
        for i in 0..256 {
            if counts[i] > 0 {
                freqs[i] = 1;
                assigned += 1;
            }
        }
        // Distribute remainder to highest-count symbols
        let mut remaining = TOTAL.saturating_sub(assigned);
        let mut sorted: Vec<(u64, usize)> = (0..256).filter(|&i| counts[i] > 0).map(|i| (counts[i], i)).collect();
        sorted.sort_unstable_by_key(|&(count, idx)| (Reverse(count), idx));
        for &(_, idx) in &sorted {
            if remaining == 0 { break; }
            freqs[idx] += 1;
            remaining -= 1;
        }
        return freqs;
    }

    // Robust two-pass normalization:
    // Pass 1: Give every nonzero symbol exactly 1 quantum
    // Pass 2: Distribute remaining (TOTAL - nonzero_count) proportionally to counts
    // This guarantees sum == TOTAL and all nonzero counts get freq >= 1.

    // Start: every nonzero symbol gets 1
    for i in 0..256 {
        if counts[i] > 0 {
            freqs[i] = 1;
        }
    }

    let remaining_quanta = TOTAL.saturating_sub(nonzero_count);
    if remaining_quanta > 0 && total_count > 0 {
        // Distribute remaining_quanta proportionally
        let mut extra_assigned = 0u32;
        let mut fractionals: Vec<(u64, usize)> = Vec::new();

        for i in 0..256 {
            if counts[i] > 0 {
                let extra = (counts[i] as u128 * remaining_quanta as u128) / total_count as u128;
                let frac = (counts[i] as u128 * remaining_quanta as u128) % total_count as u128;
                freqs[i] += extra as u32;
                extra_assigned += extra as u32;
                fractionals.push((frac as u64, i));
            }
        }

        // Distribute remainder from fractional parts
        let mut leftover = remaining_quanta - extra_assigned;
        fractionals.sort_unstable_by_key(|&(count, idx)| (Reverse(count), idx));
        for &(_, idx) in &fractionals {
            if leftover == 0 { break; }
            freqs[idx] += 1;
            leftover -= 1;
        }
    }

    // Debug assertion: sum must be exactly TOTAL
    debug_assert_eq!(freqs.iter().sum::<u32>(), TOTAL,
        "build_freq_table_from_counts: sum={} != {}", freqs.iter().sum::<u32>(), TOTAL);

    freqs
}

/// Construye tabla de frecuencias normalizada a escala 4096 (scale_bits = 12).
/// Compatible con el rANS existente del proyecto.
///
/// Garantías:
/// - Suma exactamente 4096
/// - Ninguna frecuencia es 0 si el byte aparece en data
/// - Para datos vacíos: distribución uniforme
pub fn build_freq_table(data: &[u8]) -> [u32; 256] {
    const TOTAL: u32 = 4096;
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }

    let total_count: u64 = counts.iter().sum();
    if total_count == 0 {
        // Distribución uniforme para datos vacíos
        let base = TOTAL / 256;
        let rem = TOTAL - base * 256;
        let mut freqs = [base; 256];
        for f in freqs.iter_mut().take(rem as usize) {
            *f += 1;
        }
        return freqs;
    }

    let mut freqs = [0u32; 256];
    let mut assigned: u32 = 0;

    // Fase 1: escala proporcional con piso, mínimo 1 para nonzero
    for i in 0..256 {
        if counts[i] > 0 {
            let f = ((counts[i] as u128 * TOTAL as u128) / total_count as u128) as u32;
            freqs[i] = f.max(1);
            assigned += freqs[i];
        }
    }

    // Fase 2: distribuir o reclamar el residuo
    if assigned < TOTAL {
        let mut remainder = TOTAL - assigned;
        // Dar residuo a símbolos con mayor parte fraccional
        let mut fractional: Vec<(u64, usize)> = (0..256)
            .filter(|&i| counts[i] > 0)
            .map(|i| {
                let exact = (counts[i] as u128 * TOTAL as u128) % total_count as u128;
                (exact as u64, i)
            })
            .collect();
        fractional.sort_unstable_by_key(|&(count, idx)| (Reverse(count), idx));
        for &(_, idx) in &fractional {
            if remainder == 0 {
                break;
            }
            freqs[idx] += 1;
            remainder -= 1;
        }
    } else if assigned > TOTAL {
        let mut excess = assigned - TOTAL;
        let mut fractional: Vec<(u64, usize)> = (0..256)
            .filter(|&i| freqs[i] > 1)
            .map(|i| {
                let exact = (counts[i] as u128 * TOTAL as u128) % total_count as u128;
                (exact as u64, i)
            })
            .collect();
        fractional.sort_unstable_by_key(|&(frac, idx)| (frac, idx));
        for &(_, idx) in &fractional {
            if excess == 0 {
                break;
            }
            if freqs[idx] > 1 {
                freqs[idx] -= 1;
                excess -= 1;
            }
        }
    }

    freqs
}

/// Reporte de ganancia de la separación ajedrez.
pub struct GainReport {
    pub entropy_unified: f64,
    pub entropy_p: f64,
    pub entropy_i: f64,
    /// Bytes estimados rANS sin separar
    pub size_unified: usize,
    /// Bytes estimados rANS solo P
    pub size_p: usize,
    /// Bytes estimados rANS solo I
    pub size_i: usize,
    /// Overhead: dos freq tables de 256 × u16 = 2 × 512 = 1024 bytes
    pub overhead_models: usize,
    /// Negativo = ganamos bytes, positivo = perdemos
    pub net_gain_bytes: i64,
    pub net_gain_pct: f64,
    /// true si la ganancia neta supera 512 bytes
    pub worth_applying: bool,
}

/// Estima el tamaño rANS de un stream como:
///   size_bits = Σ -log₂(freq[b] / 4096) para cada byte b
///   size_bytes = ceil(size_bits / 8)
///
/// Esto es la entropía cruzada con la tabla cuantizada a 4096.
fn estimate_rans_size(data: &[u8], freqs: &[u32; 256]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let mut bits: f64 = 0.0;
    for &b in data {
        let f = freqs[b as usize];
        if f > 0 {
            // -log₂(freq / 4096) = log₂(4096) - log₂(freq) = 12 - log₂(freq)
            bits += 12.0 - (f as f64).log2();
        } else {
            // Símbolo imposible con esta tabla — penalización máxima
            bits += 12.0;
        }
    }
    ((bits / 8.0).ceil()) as usize
}

/// Mide si aplicar separación ajedrez vale la pena sobre un stream específico.
/// Llamar ANTES de comprimir para decidir si usar v1 o v2.
pub fn measure_gain(stream: &[u8]) -> GainReport {
    let entropy_unified = entropy(stream);
    let freq_unified = build_freq_table(stream);
    let size_unified = estimate_rans_size(stream, &freq_unified);

    let streams = split(stream, BLOCK_WIDTH);
    let entropy_p = entropy(&streams.p);
    let entropy_i = entropy(&streams.i);

    let freq_p = build_freq_table(&streams.p);
    let freq_i = build_freq_table(&streams.i);
    let size_p = estimate_rans_size(&streams.p, &freq_p);
    let size_i = estimate_rans_size(&streams.i, &freq_i);

    // Overhead: dos tablas de frecuencia de 256 × u16 = 1024 bytes
    // + 8 bytes para longitudes p_len, i_len
    let overhead_models: usize = 1024 + 8;
    let size_separated = size_p + size_i + overhead_models;

    let net_gain_bytes = size_separated as i64 - size_unified as i64;
    let net_gain_pct = if size_unified > 0 {
        net_gain_bytes as f64 / size_unified as f64 * 100.0
    } else {
        0.0
    };

    GainReport {
        entropy_unified,
        entropy_p,
        entropy_i,
        size_unified,
        size_p,
        size_i,
        overhead_models,
        net_gain_bytes,
        net_gain_pct,
        worth_applying: net_gain_bytes < -512,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_random() {
        // 65536 bytes pseudoaleatorios
        let data: Vec<u8> = (0..65536u32)
            .map(|i| ((i.wrapping_mul(2654435761)) >> 16) as u8)
            .collect();
        let streams = split(&data, BLOCK_WIDTH);
        let recovered = merge(&streams, BLOCK_WIDTH);
        assert_eq!(data, recovered, "Roundtrip random must be bit-perfect");
    }

    #[test]
    fn test_roundtrip_zeros() {
        // 65536 bytes todos 0x00
        let data = vec![0x00u8; 65536];
        let streams = split(&data, BLOCK_WIDTH);

        // P debe ser todo 0x00 (valor directo de posiciones pares)
        assert!(
            streams.p.iter().all(|&b| b == 0x00),
            "P stream for all-zero input must be all 0x00"
        );
        // I debe ser todo 0xFF (0x00 XOR 0xFF)
        assert!(
            streams.i.iter().all(|&b| b == 0xFF),
            "I stream for all-zero input must be all 0xFF (0x00 XOR 0xFF)"
        );

        let recovered = merge(&streams, BLOCK_WIDTH);
        assert_eq!(data, recovered, "Roundtrip zeros must recover all 0x00");
    }

    #[test]
    fn test_roundtrip_alternating() {
        // bytes alternando 0x00 0xFF 0x00 0xFF...
        let data: Vec<u8> = (0..65536).map(|i| if i % 2 == 0 { 0x00 } else { 0xFF }).collect();
        let streams = split(&data, BLOCK_WIDTH);
        let recovered = merge(&streams, BLOCK_WIDTH);
        assert_eq!(data, recovered, "Roundtrip alternating must be exact");
    }

    #[test]
    fn test_lengths() {
        // Para input de longitud N: len(P) + len(I) == N, |len(P) - len(I)| <= 1
        for n in [0, 1, 2, 255, 256, 257, 65535, 65536] {
            let data: Vec<u8> = (0..n).map(|i| (i & 0xFF) as u8).collect();
            let streams = split(&data, BLOCK_WIDTH);
            assert_eq!(
                streams.p.len() + streams.i.len(),
                n,
                "len(P) + len(I) must equal N={n}"
            );
            let diff = (streams.p.len() as i64 - streams.i.len() as i64).unsigned_abs();
            assert!(
                diff <= 1,
                "For N={n}: |len(P) - len(I)| = {diff}, expected <= 1"
            );
        }
    }

    #[test]
    fn test_inversion_property() {
        // Construir input donde:
        //   posiciones con (fila+col)%2==0 → 0x00
        //   posiciones con (fila+col)%2==1 → 0xFF
        let n = 65536;
        let data: Vec<u8> = (0..n)
            .map(|idx| {
                let fila = idx / BLOCK_WIDTH;
                let col = idx % BLOCK_WIDTH;
                if (fila + col) % 2 == 0 { 0x00 } else { 0xFF }
            })
            .collect();

        let streams = split(&data, BLOCK_WIDTH);

        // P debe ser todo 0x00 (posiciones pares contienen 0x00, valor directo)
        assert!(
            streams.p.iter().all(|&b| b == 0x00),
            "P must be all 0x00 for checkerboard-pattern input"
        );
        // I debe ser todo 0x00 (posiciones impares contienen 0xFF, XOR 0xFF = 0x00)
        assert!(
            streams.i.iter().all(|&b| b == 0x00),
            "I must be all 0x00 for checkerboard-pattern input (0xFF XOR 0xFF = 0x00)"
        );

        // Ambos sub-streams son constantes → entropía ≈ 0
        assert!(
            entropy(&streams.p) < 0.001,
            "Entropy of all-zero P should be ~0"
        );
        assert!(
            entropy(&streams.i) < 0.001,
            "Entropy of all-zero I should be ~0"
        );

        // Round-trip
        let recovered = merge(&streams, BLOCK_WIDTH);
        assert_eq!(data, recovered);
    }

    #[test]
    fn test_entropy_separation() {
        // Stream sesgado: 80% bytes 0x00, 20% pseudoaleatorio
        let data: Vec<u8> = (0..65536u32)
            .map(|i| {
                let hash = i.wrapping_mul(2654435761) >> 16;
                if hash % 5 == 0 {
                    (hash & 0xFF) as u8 // 20% aleatorio
                } else {
                    0x00 // 80% zeros
                }
            })
            .collect();

        let h_original = entropy(&data);
        let streams = split(&data, BLOCK_WIDTH);
        let h_p = entropy(&streams.p);
        let h_i = entropy(&streams.i);

        // Las entropías de P e I deben ser diferentes (distribuciones distintas)
        assert!(
            (h_p - h_i).abs() > 0.001 || h_p < h_original,
            "P and I should have different distributions or both lower than original"
        );

        // Al menos uno de los sub-streams debe tener menor entropía que el original
        let min_sub = h_p.min(h_i);
        assert!(
            min_sub <= h_original + 0.01, // tolerancia numérica
            "At least one sub-stream should have entropy <= original: \
             min({h_p:.4}, {h_i:.4}) vs {h_original:.4}"
        );
    }

    #[test]
    fn test_freq_table_sum() {
        // build_freq_table debe sumar exactamente 4096 para cualquier input no vacío
        let test_inputs: Vec<Vec<u8>> = vec![
            vec![0x00; 1000],
            vec![0xFF; 500],
            (0..=255).collect(),
            (0..65536u32).map(|i| (i.wrapping_mul(7) & 0xFF) as u8).collect(),
            vec![42],
        ];

        for (idx, input) in test_inputs.iter().enumerate() {
            let freqs = build_freq_table(input);
            let sum: u32 = freqs.iter().sum();
            assert_eq!(
                sum, 4096,
                "Freq table for test input {idx} must sum to 4096, got {sum}"
            );
            // Nonzero counts must have nonzero frequency
            let mut counts = [0u64; 256];
            for &b in input {
                counts[b as usize] += 1;
            }
            for i in 0..256 {
                if counts[i] > 0 {
                    assert!(
                        freqs[i] >= 1,
                        "Symbol {i} with count {} must have freq >= 1",
                        counts[i]
                    );
                }
            }
        }
    }
}
