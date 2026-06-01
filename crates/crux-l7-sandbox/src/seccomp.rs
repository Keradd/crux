use std::io;

use crate::types::RuntimeKind;

const SYS_READ: u32 = 0;
const SYS_WRITE: u32 = 1;
const SYS_OPEN: u32 = 2;
const SYS_CLOSE: u32 = 3;
const SYS_STAT: u32 = 4;
const SYS_FSTAT: u32 = 5;
const SYS_LSTAT: u32 = 6;
const SYS_POLL: u32 = 7;
const SYS_LSEEK: u32 = 8;
const SYS_ACCESS: u32 = 21;
const SYS_PIPE: u32 = 22;
const SYS_SELECT: u32 = 23;
const SYS_DUP: u32 = 32;
const SYS_DUP2: u32 = 33;
const SYS_FCNTL: u32 = 72;
const SYS_GETDENTS64: u32 = 217;
const SYS_OPENAT: u32 = 257;
const SYS_NEWFSTATAT: u32 = 262;
const SYS_FACCESSAT: u32 = 269;
const SYS_READLINK: u32 = 89;
const SYS_READLINKAT: u32 = 267;

const SYS_MMAP: u32 = 9;
const SYS_MPROTECT: u32 = 10;
const SYS_MUNMAP: u32 = 11;
const SYS_BRK: u32 = 12;
const SYS_MREMAP: u32 = 25;

const SYS_GETPID: u32 = 39;
const SYS_GETUID: u32 = 102;
const SYS_GETGID: u32 = 104;
const SYS_GETEUID: u32 = 107;
const SYS_GETEGID: u32 = 108;
const SYS_GETPPID: u32 = 110;
const SYS_GETTID: u32 = 186;
const SYS_EXIT: u32 = 60;
const SYS_EXIT_GROUP: u32 = 231;
const SYS_WAIT4: u32 = 61;
const SYS_SCHED_YIELD: u32 = 24;
const SYS_CLONE: u32 = 56;

const SYS_RT_SIGACTION: u32 = 13;
const SYS_RT_SIGPROCMASK: u32 = 14;
const SYS_RT_SIGRETURN: u32 = 15;
const SYS_SIGALTSTACK: u32 = 131;
const SYS_KILL: u32 = 62;
const SYS_TKILL: u32 = 200;
const SYS_RT_SIGSUSPEND: u32 = 130;

const SYS_CLOCK_GETTIME: u32 = 228;
const SYS_CLOCK_GETRES: u32 = 229;
const SYS_GETTIMEOFDAY: u32 = 96;
const SYS_NANOSLEEP: u32 = 35;

const SYS_IOCTL: u32 = 16;
const SYS_GETRANDOM: u32 = 318;
const SYS_STATX: u32 = 332;
const SYS_FSTATFS: u32 = 138;
const SYS_GETCWD: u32 = 79;
const SYS_CHDIR: u32 = 80;
const SYS_RENAMEAT2: u32 = 316;
const SYS_UNLINKAT: u32 = 263;
const SYS_MKDIRAT: u32 = 258;
const SYS_FCHMOD: u32 = 91;
const SYS_FCHMODAT: u32 = 268;
const SYS_FCHOWN: u32 = 93;
const SYS_FCHOWNAT: u32 = 260;
const SYS_FTRUNCATE: u32 = 77;
const SYS_GETRUSAGE: u32 = 98;
const SYS_SYSINFO: u32 = 99;
const SYS_TIMES: u32 = 100;
const SYS_UNAME: u32 = 63;

const SYS_PIPE2: u32 = 293;
const SYS_DUP3: u32 = 292;
const SYS_EXECVE: u32 = 59;
const SYS_SETPGID: u32 = 109;
const SYS_SETSID: u32 = 112;
const SYS_PRCTL: u32 = 157;
const SYS_WAITID: u32 = 247;
const SYS_GETPGRP: u32 = 111;
const SYS_SETUID: u32 = 105;
const SYS_SETGID: u32 = 106;
const SYS_SETEUID: u32 = 145;
const SYS_SETEGID: u32 = 146;

const SYS_GETPRIORITY: u32 = 140;
const SYS_SETPRIORITY: u32 = 141;

const SYS_EPOLL_CREATE1: u32 = 291;
const SYS_EPOLL_CTL: u32 = 233;
const SYS_EPOLL_WAIT: u32 = 232;
const SYS_EVENTFD2: u32 = 290;
const SYS_SIGNALFD4: u32 = 289;
const SYS_TIMERFD_CREATE: u32 = 283;
const SYS_TIMERFD_SETTIME: u32 = 286;
const SYS_TIMERFD_GETTIME: u32 = 287;
const SYS_INOTIFY_INIT1: u32 = 294;
const SYS_INOTIFY_ADD_WATCH: u32 = 254;
const SYS_INOTIFY_RM_WATCH: u32 = 255;
const SYS_SOCKET: u32 = 41;
const SYS_CONNECT: u32 = 42;
const SYS_BIND: u32 = 49;
const SYS_LISTEN: u32 = 50;
const SYS_ACCEPT: u32 = 43;
const SYS_ACCEPT4: u32 = 288;
const SYS_SENDTO: u32 = 44;
const SYS_RECVFROM: u32 = 45;
const SYS_SENDMSG: u32 = 46;
const SYS_RECVMSG: u32 = 47;
const SYS_SHUTDOWN: u32 = 48;
const SYS_SETSOCKOPT: u32 = 54;
const SYS_GETSOCKOPT: u32 = 55;
const SYS_GETSOCKNAME: u32 = 51;
const SYS_GETPEERNAME: u32 = 52;
const SYS_SOCKETPAIR: u32 = 53;
const SYS_CLONE3: u32 = 435;
const SYS_CLOSE_RANGE: u32 = 436;

const BLOCKED_SYSCALLS: &[u32] = &[
    101, // ptrace
    165, // mount
    166, // umount2
    246, // kexec_load
    169, // reboot
    175, // init_module
    176, // delete_module
    167, // swapon
    168, // swapoff
    170, // sethostname
    171, // setdomainname
    172, // ioperm
    173, // iopl
    103, // syslog
    317, // seccomp (prevent child from modifying its own filter)
];

const COMMON_SYSCALLS: &[u32] = &[
    SYS_READ,
    SYS_WRITE,
    SYS_OPEN,
    SYS_CLOSE,
    SYS_STAT,
    SYS_FSTAT,
    SYS_LSTAT,
    SYS_POLL,
    SYS_LSEEK,
    SYS_ACCESS,
    SYS_READLINK,
    SYS_GETDENTS64,
    SYS_MMAP,
    SYS_MPROTECT,
    SYS_MUNMAP,
    SYS_BRK,
    SYS_MREMAP,
    SYS_GETPID,
    SYS_GETUID,
    SYS_GETGID,
    SYS_GETEUID,
    SYS_GETEGID,
    SYS_GETPPID,
    SYS_GETTID,
    SYS_EXIT,
    SYS_EXIT_GROUP,
    SYS_SCHED_YIELD,
    SYS_RT_SIGACTION,
    SYS_RT_SIGPROCMASK,
    SYS_RT_SIGRETURN,
    SYS_SIGALTSTACK,
    SYS_KILL,
    SYS_TKILL,
    SYS_RT_SIGSUSPEND,
    SYS_CLOCK_GETTIME,
    SYS_CLOCK_GETRES,
    SYS_GETTIMEOFDAY,
    SYS_NANOSLEEP,
    SYS_IOCTL,
    SYS_FCNTL,
    SYS_GETRANDOM,
    SYS_STATX,
    SYS_FSTATFS,
    SYS_GETCWD,
    SYS_CHDIR,
    SYS_PIPE,
    SYS_DUP,
    SYS_DUP2,
    SYS_SELECT,
    SYS_WAIT4,
    SYS_RENAMEAT2,
    SYS_UNLINKAT,
    SYS_MKDIRAT,
    SYS_FCHMOD,
    SYS_FCHMODAT,
    SYS_FCHOWN,
    SYS_FCHOWNAT,
    SYS_FTRUNCATE,
    SYS_GETRUSAGE,
    SYS_SYSINFO,
    SYS_TIMES,
    SYS_UNAME,
    SYS_READLINKAT,
    SYS_PRCTL,
];

const PYTHON_EXTRA: &[u32] = &[
    SYS_OPENAT,
    SYS_NEWFSTATAT,
    SYS_FACCESSAT,
    SYS_CLONE,
    SYS_WAIT4,
    SYS_GETPRIORITY,
    SYS_SETPRIORITY,
];

const BASH_EXTRA: &[u32] = &[
    SYS_PIPE2,
    SYS_DUP3,
    SYS_EXECVE,
    SYS_SETPGID,
    SYS_SETSID,
    SYS_WAITID,
    SYS_GETPGRP,
    SYS_SETUID,
    SYS_SETGID,
    SYS_SETEUID,
    SYS_SETEGID,
];

const NODE_EXTRA: &[u32] = &[
    SYS_OPENAT,
    SYS_NEWFSTATAT,
    SYS_FACCESSAT,
    SYS_EPOLL_CREATE1,
    SYS_EPOLL_CTL,
    SYS_EPOLL_WAIT,
    SYS_EVENTFD2,
    SYS_SIGNALFD4,
    SYS_TIMERFD_CREATE,
    SYS_TIMERFD_SETTIME,
    SYS_TIMERFD_GETTIME,
    SYS_INOTIFY_INIT1,
    SYS_INOTIFY_ADD_WATCH,
    SYS_INOTIFY_RM_WATCH,
    SYS_SOCKET,
    SYS_CONNECT,
    SYS_BIND,
    SYS_LISTEN,
    SYS_ACCEPT,
    SYS_ACCEPT4,
    SYS_SENDTO,
    SYS_RECVFROM,
    SYS_SENDMSG,
    SYS_RECVMSG,
    SYS_SHUTDOWN,
    SYS_SETSOCKOPT,
    SYS_GETSOCKOPT,
    SYS_GETSOCKNAME,
    SYS_GETPEERNAME,
    SYS_SOCKETPAIR,
    SYS_CLONE,
    SYS_CLONE3,
    SYS_CLOSE_RANGE,
];

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff0000;
const SECCOMP_RET_TRAP: u32 = 0x00030000;

const SECCOMP_DATA_NR_OFFSET: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct sock_filter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct sock_fprog {
    len: u16,
    filter: *const sock_filter,
}

// SAFETY: sock_filter is #[repr(C)] with only integer fields (u16, u8, u8, u32).
// The padding between jf:u8 and k:u32 matches the kernel's struct sock_filter layout.
// The struct is accessed only from one thread — no shared mutable aliasing exists.
unsafe impl Send for sock_filter {}
unsafe impl Sync for sock_filter {}

fn build_bpf_filter(allowed: &[u32]) -> Vec<sock_filter> {
    let mut prog: Vec<sock_filter> = Vec::new();

    prog.push(sock_filter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: SECCOMP_DATA_NR_OFFSET,
    });

    for (i, &sysno) in allowed.iter().enumerate() {
        let remaining = allowed.len() - i - 1;
        debug_assert!(
            remaining + 1 <= 255,
            "BPF jf field overflow: {} remaining syscalls (max 254)",
            remaining
        );
        prog.push(sock_filter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: (remaining + 1) as u8, // +1 for the TRAP at end
            k: sysno,
        });
    }

    prog.push(sock_filter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_TRAP,
    });

    prog.push(sock_filter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });

    prog
}

pub fn install_seccomp_filter(runtime: RuntimeKind) -> io::Result<()> {
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut allowed: Vec<u32> = COMMON_SYSCALLS.to_vec();
    match runtime {
        RuntimeKind::Python => allowed.extend_from_slice(PYTHON_EXTRA),
        RuntimeKind::Bash => allowed.extend_from_slice(BASH_EXTRA),
        RuntimeKind::Node => allowed.extend_from_slice(NODE_EXTRA),
    }
    allowed.retain(|s| !BLOCKED_SYSCALLS.contains(s));
    allowed.sort_unstable();
    allowed.dedup();

    let filter = build_bpf_filter(&allowed);
    let prog = sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };

    const SYS_SECCOMP: libc::c_long = 317;
    const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
    const SECCOMP_FILTER_FLAG_LOG: libc::c_ulong = 1;

    let rc = unsafe {
        libc::syscall(
            SYS_SECCOMP,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_LOG,
            &prog as *const sock_fprog,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_syscalls_not_in_common() {
        for &blocked in BLOCKED_SYSCALLS {
            assert!(
                !COMMON_SYSCALLS.contains(&blocked),
                "blocked syscall {blocked} found in COMMON_SYSCALLS"
            );
        }
    }

    #[test]
    fn blocked_syscalls_not_in_extras() {
        for &blocked in BLOCKED_SYSCALLS {
            assert!(
                !PYTHON_EXTRA.contains(&blocked),
                "blocked syscall {blocked} found in PYTHON_EXTRA"
            );
            assert!(
                !BASH_EXTRA.contains(&blocked),
                "blocked syscall {blocked} found in BASH_EXTRA"
            );
            assert!(
                !NODE_EXTRA.contains(&blocked),
                "blocked syscall {blocked} found in NODE_EXTRA"
            );
        }
    }

    #[test]
    fn bpf_filter_ends_with_allow() {
        let allowed = vec![0, 1, 2];
        let prog = build_bpf_filter(&allowed);
        let last = prog.last().expect("program should not be empty");
        assert_eq!(last.k, SECCOMP_RET_ALLOW, "last instruction must be ALLOW");
    }

    #[test]
    fn bpf_filter_default_is_trap() {
        let allowed = vec![0, 1, 2];
        let prog = build_bpf_filter(&allowed);
        let trap = &prog[prog.len() - 2];
        assert_eq!(trap.k, SECCOMP_RET_TRAP, "default branch must be TRAP");
    }

    #[test]
    fn bpf_filter_length_matches_syscalls() {
        let allowed = vec![0, 1, 2, 3, 4];
        let prog = build_bpf_filter(&allowed);
        assert_eq!(prog.len(), 1 + allowed.len() + 2);
    }
}
