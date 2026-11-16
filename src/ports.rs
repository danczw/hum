use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use sysinfo::{Pid, System};

/// Socket state code for LISTEN in /proc/net/tcp.
const TCP_LISTEN: u8 = 0x0A;

pub struct Listener {
    pub port: u16,
    pub pid: Option<Pid>,
}

/// Listening TCP ports (IPv4 and IPv6) read from /proc. Linux only.
pub fn listening(sys: &System) -> Vec<Listener> {
    let mut sockets = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(table) else {
            continue;
        };
        sockets.extend(
            text.lines()
                .skip(1)
                .filter_map(parse_line)
                .filter(|s| s.state == TCP_LISTEN)
                .map(|s| (s.port, s.inode)),
        );
    }

    let wanted: HashSet<u64> = sockets.iter().map(|&(_, inode)| inode).collect();
    let holders = socket_holders(&wanted);
    let mut listeners: Vec<Listener> = sockets
        .into_iter()
        .map(|(port, inode)| Listener {
            port,
            // Prefork servers (nginx, gunicorn) share one listen socket
            // between a master and its workers; show the master.
            pid: holders.get(&inode).and_then(|pids| {
                pids.iter().copied().find(|&pid| {
                    sys.process(pid)
                        .and_then(|p| p.parent())
                        .is_none_or(|parent| !pids.contains(&parent))
                })
            }),
        })
        .collect();
    listeners.sort_by_key(|l| (l.port, l.pid));
    listeners.dedup_by_key(|l| (l.port, l.pid));
    listeners
}

/// One row of /proc/net/tcp or /proc/net/tcp6.
#[derive(Debug, PartialEq)]
struct TcpSocket {
    #[allow(dead_code)] // read by bind scope, #12
    addr: IpAddr,
    port: u16,
    state: u8,
    inode: u64,
}

/// Parses a socket row, e.g. `0: 0100007F:1F40 00000000:0000 0A ... 4296 ...`.
/// Returns None for the header and malformed rows.
fn parse_line(line: &str) -> Option<TcpSocket> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 10 {
        return None;
    }
    let (addr, port) = fields[1].split_once(':')?;
    Some(TcpSocket {
        addr: parse_addr(addr)?,
        port: u16::from_str_radix(port, 16).ok()?,
        state: u8::from_str_radix(fields[3], 16).ok()?,
        inode: fields[9].parse().ok()?,
    })
}

/// The kernel prints addresses as 32-bit words in host byte order, so
/// 127.0.0.1 reads `0100007F` on little-endian machines.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    let word = |i: usize| -> Option<[u8; 4]> {
        let w = u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok()?;
        Some(w.to_ne_bytes())
    };
    match hex.len() {
        8 => Some(Ipv4Addr::from(word(0)?).into()),
        32 => {
            let mut bytes = [0; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            Some(Ipv6Addr::from(bytes).into())
        }
        _ => None,
    }
}

/// Maps the wanted socket inodes to every pid holding them. Sockets held by
/// other users' processes stay unresolved, since their fd tables are not
/// readable.
fn socket_holders(wanted: &HashSet<u64>) -> HashMap<u64, Vec<Pid>> {
    let mut holders: HashMap<u64, Vec<Pid>> = HashMap::new();
    if wanted.is_empty() {
        return holders;
    }
    let Ok(entries) = fs::read_dir("/proc") else {
        return holders;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let inode = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse().ok());
            if let Some(inode) = inode.filter(|i| wanted.contains(i)) {
                holders.entry(inode).or_default().push(Pid::from_u32(pid));
            }
        }
    }
    holders
}
