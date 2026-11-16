use std::collections::{HashMap, HashSet};
use std::fs;

use sysinfo::{Pid, System};

/// Socket state code for LISTEN in /proc/net/tcp.
const TCP_LISTEN: &str = "0A";

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
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 || fields[3] != TCP_LISTEN {
                continue;
            }
            let port = fields[1]
                .rsplit(':')
                .next()
                .and_then(|p| u16::from_str_radix(p, 16).ok());
            if let Some(port) = port {
                sockets.push((port, fields[9].to_string()));
            }
        }
    }

    let wanted: HashSet<&str> = sockets.iter().map(|(_, inode)| inode.as_str()).collect();
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

/// Maps the wanted socket inodes to every pid holding them. Sockets held by
/// other users' processes stay unresolved, since their fd tables are not
/// readable.
fn socket_holders(wanted: &HashSet<&str>) -> HashMap<String, Vec<Pid>> {
    let mut holders: HashMap<String, Vec<Pid>> = HashMap::new();
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
                .and_then(|t| t.strip_suffix(']'));
            if let Some(inode) = inode.filter(|i| wanted.contains(i)) {
                holders
                    .entry(inode.to_string())
                    .or_default()
                    .push(Pid::from_u32(pid));
            }
        }
    }
    holders
}
