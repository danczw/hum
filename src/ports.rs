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
    let owners = socket_owners(&wanted);
    let mut listeners: Vec<Listener> = sockets
        .into_iter()
        .map(|(port, inode)| Listener {
            port,
            pid: owners
                .get(&inode)
                .map(|&pid| topmost_holder(sys, pid, &inode)),
        })
        .collect();
    listeners.sort_by_key(|l| (l.port, l.pid));
    listeners.dedup_by_key(|l| (l.port, l.pid));
    listeners
}

/// Maps the wanted socket inodes to the pid holding them, stopping once all
/// are found. Sockets held by other users' processes stay unresolved, since
/// their fd tables are not readable.
fn socket_owners(wanted: &HashSet<&str>) -> HashMap<String, Pid> {
    let mut owners = HashMap::new();
    if wanted.is_empty() {
        return owners;
    }
    let Ok(entries) = fs::read_dir("/proc") else {
        return owners;
    };
    for entry in entries.flatten() {
        if owners.len() == wanted.len() {
            break;
        }
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
                owners
                    .entry(inode.to_string())
                    .or_insert(Pid::from_u32(pid));
            }
        }
    }
    owners
}

/// Prefork servers (nginx, gunicorn) share one listen socket between a master
/// and its workers. Climb from whichever holder was found to the topmost
/// ancestor still holding it, so the port always shows the master.
fn topmost_holder(sys: &System, mut pid: Pid, inode: &str) -> Pid {
    while let Some(parent) = sys.process(pid).and_then(|p| p.parent())
        && holds_socket(parent, inode)
    {
        pid = parent;
    }
    pid
}

fn holds_socket(pid: Pid, inode: &str) -> bool {
    let target = format!("socket:[{inode}]");
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    fds.flatten()
        .any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t.as_os_str() == target.as_str()))
}
