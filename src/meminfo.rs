use std::fs;

/// Swap (used, total) in bytes from `/proc/meminfo`, or None when unreadable.
pub fn read_swap() -> Option<(u64, u64)> {
    swap(&fs::read_to_string("/proc/meminfo").ok()?)
}

/// Swap (used, total) in bytes from the text of `/proc/meminfo`. Used leaves
/// out SwapCached, which also sits in RAM, and Zswapped, which is compressed
/// in RAM rather than on the device, as htop does. SwapCached or Zswapped
/// counts as 0 when absent or unreadable; Zswapped is absent before Linux
/// 5.19. None without a readable SwapTotal and SwapFree.
fn swap(meminfo: &str) -> Option<(u64, u64)> {
    let (mut total, mut free, mut cached, mut zswapped) = (None, None, None, None);
    for line in meminfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "SwapTotal" => &mut total,
            "SwapFree" => &mut free,
            "SwapCached" => &mut cached,
            "Zswapped" => &mut zswapped,
            _ => continue,
        };
        *slot = kib(value);
    }
    let total = total?;
    let used = total
        .saturating_sub(free?)
        .saturating_sub(cached.unwrap_or(0))
        .saturating_sub(zswapped.unwrap_or(0));
    Some((used, total))
}

/// `   8388608 kB` in bytes.
fn kib(value: &str) -> Option<u64> {
    let n: u64 = value.trim().strip_suffix(" kB")?.parse().ok()?;
    n.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    // Trimmed from a 6.x kernel with zswap on.
    const MEMINFO: &str = "\
MemTotal:       32617072 kB
MemFree:         1203424 kB
SwapCached:        65536 kB
SwapTotal:       8388608 kB
SwapFree:        7340032 kB
Zswap:             81920 kB
Zswapped:         262144 kB
Dirty:               412 kB
";

    #[test]
    fn subtracts_cached_and_zswapped() {
        // 8192M total - 7168M free - 64M cached - 256M zswapped
        assert_eq!(swap(MEMINFO), Some((704 * MIB, 8192 * MIB)));
    }

    #[test]
    fn missing_zswapped_counts_as_zero() {
        let old = MEMINFO.replace("Zswapped:         262144 kB\n", "");
        assert_eq!(swap(&old), Some((960 * MIB, 8192 * MIB)));
    }

    #[test]
    fn used_floors_at_zero() {
        let cached = MEMINFO.replace("SwapCached:        65536 kB", "SwapCached:      1048576 kB");
        assert_eq!(swap(&cached), Some((0, 8192 * MIB)));
    }

    #[test]
    fn no_swap() {
        let none = "SwapCached: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n";
        assert_eq!(swap(none), Some((0, 0)));
    }

    #[test]
    fn unreadable_optional_field_counts_as_zero() {
        let odd = MEMINFO.replace("Zswapped:         262144 kB", "Zswapped:         262144");
        assert_eq!(swap(&odd), Some((960 * MIB, 8192 * MIB)));
    }

    #[test]
    fn needs_total_and_free() {
        let partial = MEMINFO.replace("SwapFree:        7340032 kB\n", "");
        assert_eq!(swap(&partial), None);
    }
}
