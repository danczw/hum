use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use sysinfo::Pid;

use crate::agents::{Agent, Orphan, Owner, ProcInfo, display_name, live, owner};

/// Socket state code for LISTEN in /proc/net/tcp.
const TCP_LISTEN: u8 = 0x0A;
/// Ports change rarely and resolving holders reads every fd in /proc.
const PORT_TICKS: u32 = 5;

#[derive(Clone)]
pub struct Listener {
    pub port: u16,
    /// None when the socket's holder is unreadable, e.g. another user's.
    pub holder: Option<Holder>,
    /// Bound to a non-loopback address, such as 0.0.0.0 or a LAN IP.
    pub public: bool,
    /// The session or orphan holding the port, set each tick by
    /// [`Ports::observe`].
    pub owner: Option<Owner>,
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

    /// Empty when the holder is unreadable.
    pub fn name(&self) -> &str {
        self.holder.as_ref().map_or("", |h| &h.name)
    }
}

/// The kernel state ports are read from, injected so tests feed fixtures.
pub trait SocketReader {
    /// The text of each TCP socket table, IPv4 and IPv6; an unreadable one
    /// is left out.
    fn tables(&self) -> Vec<String>;
    /// The socket inodes among `pid`'s fds; empty when its fd table is
    /// unreadable, as another user's is.
    fn socket_inodes(&self, pid: Pid) -> Vec<u64>;
}

/// Reads /proc. Linux only.
pub struct ProcSockets;

impl SocketReader for ProcSockets {
    fn tables(&self) -> Vec<String> {
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
}

/// Fills listening ports from a process snapshot each tick: rereads the
/// socket tables every [`PORT_TICKS`] ticks and joins the last read to the
/// snapshot in between.
pub struct Ports<R = ProcSockets> {
    reader: R,
    ticks: u32,
    last: Vec<Listener>,
}

impl<R: SocketReader> Ports<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            ticks: 0,
            last: Vec::new(),
        }
    }

    /// This tick's listeners, each tagged with the agent or orphan holding
    /// it.
    pub fn observe(
        &mut self,
        procs: &HashMap<Pid, ProcInfo>,
        agents: &[Agent],
        orphans: &[Orphan],
    ) -> Vec<Listener> {
        if self.ticks.is_multiple_of(PORT_TICKS) {
            self.last = listening(&self.reader, procs);
        } else {
            join(&mut self.last, procs);
        }
        self.ticks = self.ticks.wrapping_add(1);
        for l in &mut self.last {
            l.owner = l
                .holder
                .as_ref()
                .and_then(|h| owner(agents, orphans, h.pid, h.start));
        }
        self.last.clone()
    }
}

/// Listening TCP ports (IPv4 and IPv6) with the process holding each.
fn listening(reader: &impl SocketReader, procs: &HashMap<Pid, ProcInfo>) -> Vec<Listener> {
    let mut sockets = Vec::new();
    for text in reader.tables() {
        sockets.extend(
            text.lines()
                .skip(1)
                .filter_map(parse_line)
                .filter(|s| s.state == TCP_LISTEN)
                .map(|s| (s.port, s.inode, is_public(s.addr))),
        );
    }

    let wanted: HashSet<u64> = sockets.iter().map(|&(_, inode, _)| inode).collect();
    // sorted, so the lowest pid wins among holders [`master`] cannot tell apart
    let mut pids: Vec<Pid> = procs.keys().copied().collect();
    pids.sort_unstable();
    let holders = socket_holders(reader, &wanted, &pids);
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
            owner: None,
        })
        .collect();
    listeners.sort_by_key(|l| (l.port, l.pid()));
    merge(listeners)
}

/// Prefork servers (nginx, gunicorn) share one listen socket between a
/// master and its workers; picks the master, the first holder whose parent
/// holds none of it.
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
fn join(ports: &mut Vec<Listener>, procs: &HashMap<Pid, ProcInfo>) {
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

/// Maps the wanted socket inodes to every pid in `pids` holding them, in
/// `pids` order. Only `pids` are scanned, so a worker forked since they were
/// read never passes for the master. Sockets held by other users' processes
/// stay unresolved, since their fd tables are not readable.
fn socket_holders(
    reader: &impl SocketReader,
    wanted: &HashSet<u64>,
    pids: &[Pid],
) -> HashMap<u64, Vec<Pid>> {
    let mut holders: HashMap<u64, Vec<Pid>> = HashMap::new();
    if wanted.is_empty() {
        return holders;
    }
    for &pid in pids {
        for inode in reader.socket_inodes(pid) {
            if wanted.contains(&inode) {
                holders.entry(inode).or_default().push(pid);
            }
        }
    }
    holders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{self, fixture::proc};

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

    fn held(port: u16, pid: u32, start: u64) -> Listener {
        Listener {
            port,
            holder: Some(Holder {
                pid: Pid::from_u32(pid),
                start,
                name: "npm".into(),
            }),
            public: false,
            owner: None,
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
    }

    /// Socket tables and fd links held in memory.
    #[derive(Default)]
    struct Fake {
        /// (port, inode) of each LISTEN row on 127.0.0.1.
        rows: Vec<(u16, u64)>,
        /// Socket inodes held per pid.
        fds: HashMap<Pid, Vec<u64>>,
    }

    impl Fake {
        /// `pids` each hold socket `inode`, beside an unwanted one.
        fn hold(&mut self, inode: u64, pids: &[u32]) {
            for &pid in pids {
                self.fds
                    .entry(Pid::from_u32(pid))
                    .or_default()
                    .extend([99, inode]);
            }
        }
    }

    impl SocketReader for Fake {
        fn tables(&self) -> Vec<String> {
            let rows = self.rows.iter().map(|(port, inode)| {
                format!("   0: 0100007F:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0")
            });
            vec![
                std::iter::once(HEADER.to_string())
                    .chain(rows)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ]
        }

        fn socket_inodes(&self, pid: Pid) -> Vec<u64> {
            self.fds.get(&pid).cloned().unwrap_or_default()
        }
    }

    /// Port, holder pid and holder name of each listener.
    fn rows(ports: &[Listener]) -> Vec<(u16, Option<u32>, &str)> {
        ports
            .iter()
            .map(|l| (l.port, l.pid().map(|p| p.as_u32()), l.name()))
            .collect()
    }

    #[test]
    fn reread_picks_the_prefork_master_and_else_the_lowest_pid() {
        let mut fake = Fake {
            rows: vec![(80, 1), (9000, 2)],
            ..Fake::default()
        };
        fake.hold(1, &[12, 11, 10]);
        // unrelated holders of one socket, e.g. one passed over a unix socket
        fake.hold(2, &[21, 20]);
        let mut ports = Ports::new(fake);
        // a fresh map each read, so its hash order varies
        for _ in 0..PORT_TICKS * 8 {
            let procs = HashMap::from([
                proc(12, 10, 102, "nginx"),
                proc(11, 10, 101, "nginx"),
                proc(10, 1, 100, "nginx"),
                proc(21, 1, 201, "worker"),
                proc(20, 1, 200, "worker"),
            ]);
            let got = ports.observe(&procs, &[], &[]);
            assert_eq!(
                rows(&got),
                [(80, Some(10), "nginx"), (9000, Some(20), "worker")]
            );
        }
    }

    #[test]
    fn a_pid_reused_between_rereads_does_not_inherit_the_port() {
        let mut fake = Fake {
            rows: vec![(3000, 1), (5173, 2)],
            ..Fake::default()
        };
        fake.hold(1, &[10]);
        fake.hold(2, &[20]);
        let mut ports = Ports::new(fake);
        let procs = HashMap::from([proc(10, 1, 100, "npm"), proc(20, 1, 200, "vite")]);
        assert_eq!(
            rows(&ports.observe(&procs, &[], &[])),
            [(3000, Some(10), "npm"), (5173, Some(20), "vite")]
        );

        // pid 10 exec'd into node; pid 20 exited and was reused
        let procs = HashMap::from([proc(10, 1, 100, "node"), proc(20, 1, 300, "bash")]);
        for _ in 1..PORT_TICKS {
            assert_eq!(
                rows(&ports.observe(&procs, &[], &[])),
                [(3000, Some(10), "node")]
            );
        }

        // the socket outlived vite in a holder whose fds are unreadable
        ports.reader.fds.remove(&Pid::from_u32(20));
        assert_eq!(
            rows(&ports.observe(&procs, &[], &[])),
            [(3000, Some(10), "node"), (5173, None, "")]
        );
    }

    #[test]
    fn owner_moves_to_the_orphan_on_a_join_tick() {
        let mut fake = Fake {
            rows: vec![(8123, 1)],
            ..Fake::default()
        };
        fake.hold(1, &[30]);
        let mut ports = Ports::new(fake);
        let procs = HashMap::from([proc(40, 1, 400, "claude"), proc(30, 40, 401, "python3")]);
        let agents = agents::scan(&procs, None, &[], |_| None);
        let owners = |got: &[Listener]| -> Vec<Option<(&str, bool)>> {
            got.iter()
                .map(|l| l.owner.as_ref().map(|o| (o.label.kind, o.orphan)))
                .collect()
        };
        assert_eq!(
            owners(&ports.observe(&procs, &agents, &[])),
            [Some(("claude", false))]
        );

        // claude exited and python3 outlived it; a reread would find no port
        ports.reader.rows.clear();
        let procs = HashMap::from([proc(30, 1, 401, "python3")]);
        let orphan = Orphan {
            pid: Pid::from_u32(30),
            name: "python3".into(),
            from: agents[0].label(),
            start: 401,
            cpu: 0.0,
            mem: 0,
        };
        let got = ports.observe(&procs, &[], &[orphan]);
        assert_eq!(rows(&got), [(8123, Some(30), "python3")]);
        assert_eq!(owners(&got), [Some(("claude", true))]);

        // stamped each tick, so the owner clears once no list claims the holder
        assert_eq!(owners(&ports.observe(&procs, &[], &[])), [None]);
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
