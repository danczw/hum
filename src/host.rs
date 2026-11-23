//! Every read of /proc goes through [`Host`], so tests run on [`MemHost`]
//! without real pids.

use std::fs;

use sysinfo::Pid;

/// The kernel state hum reads, injected so tests feed fixtures.
pub trait Host {
    /// `/proc/<pid>/stat`; None when unreadable, e.g. the process exited.
    fn stat(&self, pid: Pid) -> Option<Stat>;
    /// Raw `/proc/<pid>/environ`; None when unreadable, as another user's is.
    fn environ(&self, pid: Pid) -> Option<Vec<u8>>;
    /// The text of each TCP socket table, IPv4 and IPv6; an unreadable one
    /// is left out.
    fn tcp_tables(&self) -> Vec<String>;
    /// The socket inodes among `pid`'s fds; empty when its fd table is
    /// unreadable, as another user's is.
    fn socket_inodes(&self, pid: Pid) -> Vec<u64>;
    /// The text of `/proc/meminfo`.
    fn meminfo(&self) -> Option<String>;
}

/// The fields hum uses from a `/proc/<pid>/stat` line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stat {
    /// Start time after boot, in clock ticks.
    pub start: u64,
    /// utime + stime + cutime + cstime in clock ticks: the process's own cpu
    /// plus that of the children it reaped.
    pub cpu: u64,
}

impl Stat {
    pub fn parse(line: &str) -> Option<Self> {
        let cpu = (14..=17)
            .map(|n| {
                // cutime and cstime are signed in the kernel
                let ticks: i64 = field(line, n)?.parse().ok()?;
                Some(u64::try_from(ticks).unwrap_or(0))
            })
            .sum::<Option<u64>>()?;
        Some(Self {
            start: field(line, 22)?.parse().ok()?,
            cpu,
        })
    }
}

/// Field `n` of a stat line, numbered from 1 as in proc(5), for `n` of 3 on;
/// None below. The comm in field 2 may hold spaces and `)`, so fields are
/// counted from the last `)`.
fn field(line: &str, n: usize) -> Option<&str> {
    let (_, rest) = line.rsplit_once(')')?;
    rest.split_whitespace().nth(n.checked_sub(3)?)
}

/// Reads /proc. Linux only.
#[derive(Clone, Copy, Default)]
pub struct ProcHost;

impl Host for ProcHost {
    fn stat(&self, pid: Pid) -> Option<Stat> {
        Stat::parse(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    fn environ(&self, pid: Pid) -> Option<Vec<u8>> {
        fs::read(format!("/proc/{pid}/environ")).ok()
    }

    fn tcp_tables(&self) -> Vec<String> {
        ["/proc/net/tcp", "/proc/net/tcp6"]
            .into_iter()
            .filter_map(|table| fs::read_to_string(table).ok())
            .collect()
    }

    /// Fd links read `socket:[4296]`.
    fn socket_inodes(&self, pid: Pid) -> Vec<u64> {
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        fds.flatten()
            .filter_map(|fd| {
                let target = fs::read_link(fd.path()).ok()?;
                target
                    .to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse()
                    .ok()
            })
            .collect()
    }

    fn meminfo(&self) -> Option<String> {
        fs::read_to_string("/proc/meminfo").ok()
    }
}

#[cfg(test)]
pub use mem::MemHost;

#[cfg(test)]
mod mem {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// Host state held in memory; anything not set reads as absent.
    #[derive(Default)]
    pub struct MemHost {
        pub stats: HashMap<Pid, Stat>,
        pub environs: HashMap<Pid, Vec<u8>>,
        /// (file, pid) of each per-pid read, e.g. `("environ", 12)`, in
        /// read order.
        pub reads: RefCell<Vec<(&'static str, u32)>>,
        /// (port, inode) of each LISTEN row on 127.0.0.1.
        pub listen: Vec<(u16, u64)>,
        pub sockets: HashMap<Pid, Vec<u64>>,
        pub meminfo: Option<String>,
    }

    impl MemHost {
        /// Sets `pid`'s cpu to `ticks`, with start time 0.
        pub fn cpu(&mut self, pid: u32, ticks: u64) {
            self.stats.insert(
                Pid::from_u32(pid),
                Stat {
                    start: 0,
                    cpu: ticks,
                },
            );
        }

        /// `pids` each hold socket `inode`, beside an unwanted one.
        pub fn hold(&mut self, inode: u64, pids: &[u32]) {
            for &pid in pids {
                self.sockets
                    .entry(Pid::from_u32(pid))
                    .or_default()
                    .extend([99, inode]);
            }
        }
    }

    impl Host for MemHost {
        fn stat(&self, pid: Pid) -> Option<Stat> {
            self.reads.borrow_mut().push(("stat", pid.as_u32()));
            self.stats.get(&pid).copied()
        }

        fn environ(&self, pid: Pid) -> Option<Vec<u8>> {
            self.reads.borrow_mut().push(("environ", pid.as_u32()));
            self.environs.get(&pid).cloned()
        }

        fn tcp_tables(&self) -> Vec<String> {
            let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";
            let rows = self.listen.iter().map(|(port, inode)| {
                format!("   0: 0100007F:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0")
            });
            vec![
                std::iter::once(header.to_string())
                    .chain(rows)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ]
        }

        fn socket_inodes(&self, pid: Pid) -> Vec<u64> {
            self.sockets.get(&pid).cloned().unwrap_or_default()
        }

        fn meminfo(&self) -> Option<String> {
            self.meminfo.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line of `comm` with the given utime, stime, cutime and cstime,
    /// started at tick 100.
    fn line(comm: &str, [utime, stime, cutime, cstime]: [i64; 4]) -> String {
        format!(
            "42 ({comm}) S 1 42 42 0 -1 4194304 0 0 0 0 {utime} {stime} {cutime} {cstime} 20 0 1 0 100"
        )
    }

    #[test]
    fn cpu_adds_own_and_reaped_time() {
        let cpu = |l: String| Stat::parse(&l).map(|s| s.cpu);
        assert_eq!(cpu(line("sh", [1, 2, 30, 40])), Some(73));
        assert_eq!(cpu(line("sh", [1, 2, -3, 4])), Some(7));
    }

    #[test]
    fn comm_may_hold_spaces_and_parens() {
        assert_eq!(
            Stat::parse(&line("a) b", [1, 2, 3, 4])),
            Some(Stat {
                start: 100,
                cpu: 10
            })
        );
    }

    #[test]
    fn short_or_garbage_line_is_none() {
        assert_eq!(Stat::parse("42 (sh) S 1 42"), None);
        assert_eq!(Stat::parse("garbage"), None);
        assert_eq!(field("42 (sh) S", 2), None);
    }
}
