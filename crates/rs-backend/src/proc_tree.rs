//! Killing a whole process tree.
//!
//! umu/pressure-vessel builds a tree (`umu-run` → `bwrap` → `pv-adverb` → Proton →
//! Wine processes) in which every layer starts its own session, so neither process
//! groups nor sessions cover it. The only reliable way to take it down is to list
//! every descendant first and then kill them all.

use std::collections::HashMap;

/// All descendants of `root` (not including `root`), from `/proc`.
pub fn descendants(root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if let Some(ppid) = parent_of(pid) {
                children.entry(ppid).or_default().push(pid);
            }
        }
    }
    collect(root, &children)
}

fn collect(root: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                if !out.contains(&kid) {
                    out.push(kid);
                    queue.push(kid);
                }
            }
        }
    }
    out
}

fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // "pid (comm) state ppid ..." — comm may contain spaces and parentheses.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Pids (other than our own) whose command line contains `args` as consecutive arguments.
/// Wine gives its processes their Windows command line, so an instance's processes can be
/// found by `-setInstanceName <name>` even when they aren't our descendants (distrobox).
pub fn pids_with_args(args: &[&str]) -> Vec<u32> {
    let me = std::process::id();
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return out };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else { continue };
        let parts: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
        if !args.is_empty()
            && parts
                .windows(args.len())
                .any(|w| w.iter().zip(args).all(|(have, want)| *have == want.as_bytes()))
        {
            out.push(pid);
        }
    }
    out
}

/// SIGKILL every process found by [`pids_with_args`].
pub fn kill_with_args(args: &[&str]) {
    for pid in pids_with_args(args) {
        // SAFETY: as in `kill_tree`.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// SIGKILL `root` and everything below it. Descendants are collected before any
/// signal is sent, so reparenting during the kill can't hide anything.
pub fn kill_tree(root: u32) {
    let mut pids = descendants(root);
    pids.push(root);
    for pid in pids {
        // SAFETY: kill(2) on pids we enumerated; a stale pid at worst hits a
        // process we don't own, which the kernel refuses (EPERM).
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_all_levels_once() {
        let children = HashMap::from([(1, vec![2, 3]), (2, vec![4]), (4, vec![5, 6]), (3, vec![7])]);
        let mut got = collect(1, &children);
        got.sort();
        assert_eq!(got, vec![2, 3, 4, 5, 6, 7]);
        assert!(collect(5, &children).is_empty());
    }

    #[test]
    fn finds_processes_by_arguments() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 5; exit 0", "rsgtk-marker-test", "unique-argument"])
            .spawn()
            .expect("sh");
        let found = pids_with_args(&["rsgtk-marker-test", "unique-argument"]);
        assert!(found.contains(&child.id()), "{found:?}");
        assert!(pids_with_args(&["rsgtk-marker-test", "something-else"]).is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn reads_our_own_parent() {
        let me = std::process::id();
        let ppid = parent_of(me).expect("own ppid");
        assert_ne!(ppid, 0);
        assert!(descendants(ppid).contains(&me));
    }
}
