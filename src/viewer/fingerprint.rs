//! The whole file as one picture: a map of what kind of bytes are where.
//!
//! Each cell is a span of the file, coloured by its **Shannon entropy** or by
//! what its bytes mostly *are*. Compressed and encrypted regions come out near
//! the top of the entropy scale and flat, padding and sparse holes come out at
//! the bottom, and the boundaries between a container's parts — a header, a
//! table, a payload — show up as visible seams. On a disk image or a firmware
//! blob that is a structural overview no hex dump gives you.
//!
//! **Sampled, not read whole.** A cell reads at most [`WINDOW`] bytes from the
//! start of its span, so the analysis costs the same bounded handful of
//! megabytes for a 4 MB file and a 40 GB one — which is the only way this can be
//! built synchronously when the user presses the key. Entropy over a 4 KiB
//! window is a sound estimate for the span it stands in; what it cannot do is
//! notice something small hiding in the middle of a large span, and the cell
//! count is chosen high enough to keep spans small on ordinary files.

/// Cells the map is divided into. A power of two so the row width can be one
/// too, which keeps the arithmetic from offset to cell exact.
pub const CELLS: usize = 4096;

/// Bytes sampled from the start of each cell's span.
pub const WINDOW: usize = 4096;

/// What a cell's bytes mostly are.
///
/// Deliberately coarse. These are read as *colours in a map*, not as a verdict
/// on a span, so a handful of clearly distinguishable classes is worth more than
/// a fine-grained taxonomy that all looks alike at one pixel per cell. (There is
/// no UTF-8 class for the same reason: telling multi-byte UTF-8 from arbitrary
/// high bytes needs real decoding, and both land in [`High`](ByteClass::High).)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteClass {
    /// Overwhelmingly `0x00` — padding, a sparse hole, an erased region.
    Zero,
    /// Printable ASCII and the usual whitespace: text, source, a config file.
    Ascii,
    /// Mostly bytes at or above `0x80`.
    High,
    /// Everything else — the ordinary look of machine code and binary records.
    Mixed,
}

/// One cell of the map.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    /// Shannon entropy of the sample, in bits per byte, `0.0..=8.0`. Reported
    /// in the header because it is the number people know.
    pub entropy: f32,
    /// [`entropy`](Cell::entropy) as a fraction of the most this sample *could*
    /// have scored, `0.0..=1.0`. This is what the colour ramp uses.
    ///
    /// A sample of `n` bytes cannot exceed `log2(n)` bits however random it is,
    /// so on a small file — where a cell spans only a few bytes — raw entropy
    /// tops out well below 8 and a wholly incompressible file would be painted
    /// as merely lukewarm. Dividing by the achievable maximum makes the picture
    /// mean the same thing at every file size, which for a *map* matters more
    /// than the absolute figure.
    pub density: f32,
    pub class: ByteClass,
    /// Byte offset this cell's span starts at.
    pub start: u64,
}

/// The analysed file.
#[derive(Debug, Clone)]
pub struct Fingerprint {
    pub cells: Vec<Cell>,
    pub len: u64,
}

impl Fingerprint {
    /// The cell holding byte `off`.
    pub fn cell_at(&self, off: u64) -> usize {
        if self.len == 0 {
            return 0;
        }
        let i =
            (off.min(self.len - 1) as u128 * self.cells.len() as u128 / self.len as u128) as usize;
        i.min(self.cells.len().saturating_sub(1))
    }
}

/// Analyse a file of `len` bytes, reading through `read(start, end)`.
///
/// Taking a reader rather than the viewer's `Source` keeps the arithmetic — the
/// part worth testing — free of any file at all.
pub fn analyze(len: u64, read: impl Fn(u64, u64) -> Vec<u8>) -> Fingerprint {
    if len == 0 {
        return Fingerprint { cells: Vec::new(), len };
    }
    // Never more cells than bytes, so a 40-byte file gets 40 cells rather than
    // 4096 cells of which all but 40 are empty.
    // (Compared as u64: on a 32-bit target `len as usize` would wrap.)
    let n = (CELLS as u64).min(len).max(1) as usize;
    let mut cells = Vec::with_capacity(n);
    for i in 0..n {
        let start = (i as u128 * len as u128 / n as u128) as u64;
        let end = ((i + 1) as u128 * len as u128 / n as u128) as u64;
        let end = end.max(start + 1).min(len);
        let sample = read(start, end.min(start + WINDOW as u64));
        cells.push(measure(&sample, start));
    }
    Fingerprint { cells, len }
}

/// Entropy and class of one sample.
fn measure(bytes: &[u8], start: u64) -> Cell {
    if bytes.is_empty() {
        return Cell { entropy: 0.0, density: 0.0, class: ByteClass::Zero, start };
    }
    let mut hist = [0u32; 256];
    for &b in bytes {
        hist[b as usize] += 1;
    }
    let total = bytes.len() as f32;
    let mut entropy = 0.0f32;
    for &c in hist.iter() {
        if c > 0 {
            let p = c as f32 / total;
            entropy -= p * p.log2();
        }
    }
    let zero = hist[0] as f32 / total;
    let ascii = (0x20..0x7f).map(|i| hist[i]).sum::<u32>() as f32
        + [0x09u8, 0x0a, 0x0d].iter().map(|&i| hist[i as usize]).sum::<u32>() as f32;
    let high = (0x80..0x100).map(|i| hist[i]).sum::<u32>() as f32 / total;
    let entropy = entropy.clamp(0.0, 8.0);
    // The ceiling a sample this size can reach: one distinct value per byte, and
    // never more than the 256 there are.
    let ceiling = (bytes.len().min(256) as f32).log2().max(1e-6);
    let class = if zero > 0.90 {
        ByteClass::Zero
    } else if ascii / total > 0.90 {
        ByteClass::Ascii
    } else if high > 0.50 {
        ByteClass::High
    } else {
        ByteClass::Mixed
    };
    Cell { entropy, density: (entropy / ceiling).clamp(0.0, 1.0), class, start }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader over an in-memory buffer, as `analyze` wants it.
    fn over(data: &[u8]) -> impl Fn(u64, u64) -> Vec<u8> + '_ {
        move |a, b| data[a as usize..(b as usize).min(data.len())].to_vec()
    }

    #[test]
    fn an_empty_file_has_no_cells() {
        let fp = analyze(0, |_, _| Vec::new());
        assert!(fp.cells.is_empty());
        // And asking where offset 0 lives must not divide by zero.
        assert_eq!(fp.cell_at(0), 0);
    }

    #[test]
    fn zeroes_read_as_empty_and_random_bytes_as_full() {
        let zeros = vec![0u8; 65536];
        let fp = analyze(zeros.len() as u64, over(&zeros));
        assert!(fp.cells.iter().all(|c| c.entropy < 0.01), "all-zero is zero entropy");
        assert!(fp.cells.iter().all(|c| c.class == ByteClass::Zero));

        // 4 MiB, so each of the 4096 cells samples 1 KiB — enough for entropy to
        // approach its ceiling of 8 bits.
        let noise: Vec<u8> =
            (0..(1u32 << 22)).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let fp = analyze(noise.len() as u64, over(&noise));
        let avg = fp.cells.iter().map(|c| c.entropy).sum::<f32>() / fp.cells.len() as f32;
        assert!(avg > 7.0, "spread bytes read as near-maximum entropy, got {avg}");
        assert!(fp.cells.iter().all(|c| c.density > 0.9), "and as near-maximum density");
    }

    #[test]
    fn text_is_classified_as_ascii_and_sits_mid_scale() {
        let text = "the quick brown fox jumps over the lazy dog\n".repeat(2000).into_bytes();
        let fp = analyze(text.len() as u64, over(&text));
        assert!(fp.cells.iter().all(|c| c.class == ByteClass::Ascii));
        let avg = fp.cells.iter().map(|c| c.entropy).sum::<f32>() / fp.cells.len() as f32;
        assert!((3.0..6.0).contains(&avg), "English prose sits mid-scale, got {avg}");
    }

    #[test]
    fn high_bytes_are_their_own_class() {
        let data = vec![0xe4u8; 40000];
        let fp = analyze(data.len() as u64, over(&data));
        assert!(fp.cells.iter().all(|c| c.class == ByteClass::High));
    }

    #[test]
    fn a_seam_between_two_regions_is_visible() {
        // Half zeroes, half spread bytes: the map must show two distinct halves,
        // which is the whole point of the view.
        let mut data = vec![0u8; 1 << 20];
        for (i, b) in data.iter_mut().enumerate().skip(1 << 19) {
            *b = (i.wrapping_mul(2654435761) >> 13) as u8;
        }
        let fp = analyze(data.len() as u64, over(&data));
        let half = fp.cells.len() / 2;
        assert!(fp.cells[..half - 1].iter().all(|c| c.entropy < 0.01));
        assert!(fp.cells[half + 1..].iter().all(|c| c.entropy > 6.0));
    }

    #[test]
    fn a_file_smaller_than_the_cell_count_gets_one_cell_per_byte() {
        let data = b"hello".to_vec();
        let fp = analyze(data.len() as u64, over(&data));
        assert_eq!(fp.cells.len(), 5, "never more cells than bytes");
        assert_eq!(fp.cells[0].start, 0);
        assert_eq!(fp.cells[4].start, 4);
    }

    #[test]
    fn every_cell_knows_where_it_starts_and_offsets_map_back() {
        let data = vec![7u8; 1 << 20];
        let fp = analyze(data.len() as u64, over(&data));
        assert_eq!(fp.cells.len(), CELLS);
        assert_eq!(fp.cells[0].start, 0);
        // Round-trip: the cell an offset lands in must start at or before it.
        for off in [0u64, 1, 4095, 100_000, (1 << 20) - 1] {
            let i = fp.cell_at(off);
            assert!(fp.cells[i].start <= off, "cell {i} starts after {off}");
            if i + 1 < fp.cells.len() {
                assert!(fp.cells[i + 1].start > off);
            }
        }
    }

    #[test]
    fn density_reads_the_same_at_any_file_size_though_raw_entropy_cannot() {
        // The same incompressible content, once tiny and once large. Raw entropy
        // is capped by the sample size; density is what stays comparable.
        let noise = |n: u32| -> Vec<u8> {
            (0..n).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()
        };
        let small = noise(1 << 16);
        let large = noise(1 << 22);
        let a = analyze(small.len() as u64, over(&small));
        let b = analyze(large.len() as u64, over(&large));
        let mean = |f: &Fingerprint, g: fn(&Cell) -> f32| {
            f.cells.iter().map(g).sum::<f32>() / f.cells.len() as f32
        };
        assert!(
            mean(&a, |c| c.entropy) < mean(&b, |c| c.entropy) - 2.0,
            "raw entropy is held down by the small file's tiny samples"
        );
        assert!((mean(&a, |c| c.density) - mean(&b, |c| c.density)).abs() < 0.1, "density is not");
    }

    #[test]
    fn a_huge_file_is_sampled_rather_than_read_whole() {
        use std::cell::Cell as C;
        let read_bytes = C::new(0u64);
        let len = 40u64 << 30; // 40 GiB
        let fp = analyze(len, |a, b| {
            read_bytes.set(read_bytes.get() + (b - a));
            vec![0u8; (b - a) as usize]
        });
        assert_eq!(fp.cells.len(), CELLS);
        let total = read_bytes.get();
        assert!(total <= (CELLS * WINDOW) as u64, "read {total} bytes of a 40 GiB file");
    }
}
