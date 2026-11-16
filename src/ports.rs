use std::collections::HashMap;
use std::fs;

use sysinfo::Pid;

/// Socket state code for LISTEN in /proc/net/tcp.
const TCP_LISTEN: &str = "0A";

pub struct Listener {
    pub port: u16,
    pub pid: Option<Pid>,
}

/// Listening TCP ports (IPv4 and IPv6) read from /proc. Linux only.
pub fn listening() -> Vec<Listener> {
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

    let owners = socket_owners();
    let mut listeners: Vec<Listener> = sockets
        .into_iter()
        .map(|(port, inode)| Listener {
            port,
            pid: owners.get(&inode).copied(),
        })
        .collect();
    listeners.sort_by_key(|l| (l.port, l.pid));
    listeners.dedup_by_key(|l| (l.port, l.pid));
    listeners
}

/// Maps socket inodes to the pid holding them. Sockets held by other users'
/// processes stay unresolved, since their fd tables are not readable.
fn socket_owners() -> HashMap<String, Pid> {
    let mut owners = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return owners;
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
            if let Some(inode) = inode {
                owners.insert(inode.to_string(), Pid::from_u32(pid));
            }
        }
    }
    owners
}
