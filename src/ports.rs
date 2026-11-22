use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use sysinfo::Pid;

use crate::agents::{ProcInfo, display_name, live};

/// Socket state code for LISTEN in /proc/net/tcp.
const TCP_LISTEN: u8 = 0x0A;

pub struct Listener {
    pub port: u16,
    /// None when the socket's holder is unreadable, e.g. another user's.
    pub holder: Option<Holder>,
    /// Bound to a non-loopback address, such as 0.0.0.0 or a LAN IP.
    pub public: bool,
}

/// The process holding a listen socket; start time guards against pid
/// reuse.
#[derive(Clone)]
pub struct Holder {
    pub pid: Pid,
    pub start: u64,
    pub name: String,
}

impl Listener {
    pub fn pid(&self) -> Option<Pid> {
        self.holder.as_ref().map(|h| h.pid)
    }
}

/// Listening TCP ports (IPv4 and IPv6) read from /proc. Linux only.
pub fn listening(procs: &HashMap<Pid, ProcInfo>) -> Vec<Listener> {
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
                .map(|s| (s.port, s.inode, is_public(s.addr))),
        );
    }

    let wanted: HashSet<u64> = sockets.iter().map(|&(_, inode, _)| inode).collect();
    let holders = socket_holders(&wanted);
    let mut listeners: Vec<Listener> = sockets
        .into_iter()
        .map(|(port, inode, public)| Listener {
            port,
            public,
            holder: holders
                .get(&inode)
                .and_then(|pids| master(pids, procs))
                .and_then(|pid| procs.get(&pid))
                .map(|p| Holder {
                    pid: p.pid,
                    start: p.start,
                    name: display_name(&p.name, &p.cmd),
                }),
        })
        .collect();
    listeners.sort_by_key(|l| (l.port, l.pid()));
    merge(listeners)
}

/// Prefork servers (nginx, gunicorn) share one listen socket between a
/// master and its workers; picks the master, the first holder whose parent
/// holds none of it. Skips holders started after `procs` was read: a
/// worker forked since then would pass for the master.
fn master(pids: &[Pid], procs: &HashMap<Pid, ProcInfo>) -> Option<Pid> {
    pids.iter().copied().find(|pid| {
        procs
            .get(pid)
            .is_some_and(|p| p.parent.is_none_or(|parent| !pids.contains(&parent)))
    })
}

/// Drops listeners whose holder exited since [`listening`], so a reused
/// pid does not inherit the port, and renames the rest after what they run
/// now. Ports are read every few ticks, `procs` every tick.
pub fn join(ports: &mut Vec<Listener>, procs: &HashMap<Pid, ProcInfo>) {
    ports.retain_mut(|l| {
        let Some(holder) = &mut l.holder else {
            return true;
        };
        let Some(p) = live(procs, holder.pid, holder.start) else {
            return false;
        };
        holder.name = display_name(&p.name, &p.cmd);
        true
    });
}

/// Folds sorted listeners of one port and pid into one, public if any of
/// them is: a server bound to both 127.0.0.1 and 0.0.0.0 shows once.
fn merge(listeners: Vec<Listener>) -> Vec<Listener> {
    let mut merged: Vec<Listener> = Vec::with_capacity(listeners.len());
    for l in listeners {
        match merged.last_mut() {
            Some(last) if (last.port, last.pid()) == (l.port, l.pid()) => last.public |= l.public,
            _ => merged.push(l),
        }
    }
    merged
}

/// Anything but 127.0.0.0/8 and ::1, IPv4-mapped loopback included.
fn is_public(addr: IpAddr) -> bool {
    !addr.to_canonical().is_loopback()
}

/// One row of /proc/net/tcp or /proc/net/tcp6.
#[derive(Debug, PartialEq)]
struct TcpSocket {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Rows from a little-endian host.
    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";
    const LOOPBACK: &str = "   1: 0100007F:1F40 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4296 1 0000000000000000 100 0 0 10 0";
    const ANY: &str = "   0: 00000000:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5120 1 0000000000000000 100 0 0 10 0";
    const ESTABLISHED: &str = "   2: 0100007F:1F40 0100007F:D2F0 01 00000000:00000000 00:00000000 00000000  1000        0 6001 1 0000000000000000 20 4 30 10 -1";
    const LOOPBACK6: &str = "   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 7002 1 0000000000000000 100 0 0 10 0";
    const ANY6: &str = "   1: 00000000000000000000000000000000:0016 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 7003 1 0000000000000000 100 0 0 10 0";

    #[test]
    #[cfg(target_endian = "little")]
    fn ipv4_rows() {
        assert_eq!(
            parse_line(LOOPBACK),
            Some(TcpSocket {
                addr: Ipv4Addr::LOCALHOST.into(),
                port: 8000,
                state: TCP_LISTEN,
                inode: 4296,
            })
        );
        let any = parse_line(ANY).unwrap();
        assert_eq!((any.addr, any.port), (Ipv4Addr::UNSPECIFIED.into(), 3000));
        assert_eq!(parse_line(ESTABLISHED).unwrap().state, 0x01);
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn ipv6_rows() {
        let lo = parse_line(LOOPBACK6).unwrap();
        assert_eq!(
            (lo.addr, lo.port, lo.inode),
            (Ipv6Addr::LOCALHOST.into(), 8080, 7002)
        );
        let any = parse_line(ANY6).unwrap();
        assert_eq!((any.addr, any.port), (Ipv6Addr::UNSPECIFIED.into(), 22));
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn loopback_binds_are_not_public() {
        let public = |line| is_public(parse_line(line).unwrap().addr);
        assert!(!public(LOOPBACK));
        assert!(!public(LOOPBACK6));
        assert!(public(ANY));
        assert!(public(ANY6));
        assert!(is_public(Ipv4Addr::new(192, 168, 1, 5).into()));
        assert!(!is_public(Ipv4Addr::new(127, 0, 1, 1).into()));
        assert!(!is_public(Ipv4Addr::LOCALHOST.to_ipv6_mapped().into()));
    }

    fn proc(pid: u32, parent: u32, start: u64, cmd: &str) -> (Pid, ProcInfo) {
        let info = ProcInfo {
            pid: Pid::from_u32(pid),
            parent: Some(Pid::from_u32(parent)),
            start,
            name: cmd.into(),
            cmd: vec![cmd.into()],
            cpu: 0.0,
            mem: 0,
            cwd: None,
        };
        (info.pid, info)
    }

    fn held(port: u16, pid: u32, start: u64) -> Listener {
        Listener {
            port,
            holder: Some(Holder {
                pid: Pid::from_u32(pid),
                start,
                name: "npm".into(),
            }),
            public: false,
        }
    }

    #[test]
    fn prefork_master_holds_the_port() {
        let procs = HashMap::from([
            proc(10, 1, 100, "nginx"),
            proc(11, 10, 101, "nginx"),
            proc(12, 10, 102, "nginx"),
        ]);
        let pids = |ps: &[u32]| ps.iter().map(|&p| Pid::from_u32(p)).collect::<Vec<_>>();
        assert_eq!(
            master(&pids(&[11, 10, 12]), &procs),
            Some(Pid::from_u32(10))
        );
        // a worker alone, its master gone from the holders
        assert_eq!(master(&pids(&[12]), &procs), Some(Pid::from_u32(12)));
        // 13 forked after procs was read
        assert_eq!(master(&pids(&[13, 10]), &procs), Some(Pid::from_u32(10)));
    }

    #[test]
    fn join_drops_a_port_whose_pid_was_reused() {
        let mut procs = HashMap::from([proc(10, 1, 100, "npm"), proc(20, 1, 200, "vite")]);
        let mut ports = vec![held(3000, 10, 100), held(5173, 20, 200)];
        // pid 10 exec'd into node; pid 20 exited and was reused
        procs.extend([proc(10, 1, 100, "node"), proc(20, 1, 300, "bash")]);
        join(&mut ports, &procs);
        let got: Vec<_> = ports
            .iter()
            .map(|l| (l.port, l.holder.as_ref().unwrap().name.as_str()))
            .collect();
        assert_eq!(got, [(3000, "node")]);
    }

    #[test]
    fn one_port_and_pid_shows_once_public_if_any_bind_is() {
        let l = |port, pid: u32, public| Listener {
            public,
            ..held(port, pid, 0)
        };
        let merged = merge(vec![l(3000, 7, false), l(3000, 7, true), l(3000, 8, false)]);
        let got: Vec<_> = merged.iter().map(|l| (l.port, l.pid(), l.public)).collect();
        assert_eq!(
            got,
            [
                (3000, Some(Pid::from_u32(7)), true),
                (3000, Some(Pid::from_u32(8)), false)
            ]
        );
    }

    #[test]
    fn header_and_short_rows() {
        assert_eq!(parse_line(HEADER), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   0: 0100007F:1F40 00000000:0000 0A"), None);
    }
}
