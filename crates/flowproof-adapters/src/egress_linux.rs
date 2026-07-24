//! Egress containment on Linux: a real, unprivileged, default-deny seccomp
//! filter serviced by a supervisor in the parent. This is the mechanism the
//! cross-platform `egress` module reports as "enforced".
//!
//! # How it works
//!
//! The child installs, in `pre_exec` before the agent's real program takes
//! over, a seccomp filter that:
//!
//! - default `SECCOMP_RET_ALLOW` (a testing sandbox, not a jail - only the
//!   network-egress syscalls are examined),
//! - `SECCOMP_RET_USER_NOTIF` on `connect`, `sendto`, `sendmsg`, `sendmmsg`
//!   and `listen`, handing each to the supervisor,
//! - `SECCOMP_RET_ERRNO(EPERM)` on `io_uring_setup` and `socket(AF_PACKET)`,
//!   closing the two paths that reach the network under the notifier.
//!
//! The filter is installed with `SECCOMP_FILTER_FLAG_NEW_LISTENER`, whose
//! return value is a notify fd. That fd lives in the child, so it is handed to
//! the parent - but NOT with `SCM_RIGHTS`/`sendmsg`, since `sendmsg` is one of
//! the syscalls the filter traps, so using it here would suspend the child on
//! the very notifier the parent has not started servicing yet (a deadlock).
//! Instead the child WRITES its `(pid, notify-fd-number)` over a pre-created
//! `socketpair` (`getpid`/`write` are not trapped), a PARENT HANDOFF THREAD
//! acquires the actual fd with `pidfd_open`+`pidfd_getfd` (the same infra the
//! supervisor uses to act on the child's sockets) and WRITES back a one-byte
//! ack so the child may close its copy and proceed to exec. A supervisor
//! thread then services the notify fd for the whole run.
//!
//! The handoff runs on its own thread, started BEFORE `Command::spawn`,
//! because `spawn` blocks until the child execs and the child cannot exec
//! until it gets the ack: the read+ack must happen concurrently with `spawn`,
//! not after it. The child sends its own pid because `child.id()` is not yet
//! available while `spawn` is still blocked.
//!
//! # The TOCTOU-safe pattern
//!
//! For an address-bearing syscall the supervisor NEVER replies
//! `SECCOMP_USER_NOTIF_FLAG_CONTINUE`: on CONTINUE the kernel re-reads the
//! child's memory, and a sibling thread can rewrite the address between our
//! check and that re-read, which `ID_VALID` cannot detect. CONTINUE is only
//! safe for value-argument syscalls, which `connect` is not. Instead:
//!
//! 1. `SECCOMP_IOCTL_NOTIF_RECV` yields `{id, pid, data.args}`; the sockaddr
//!    is a POINTER into child memory.
//! 2. Copy the sockaddr into supervisor memory with `process_vm_readv`,
//!    bounded by the syscall's own `addrlen`.
//! 3. `SECCOMP_IOCTL_NOTIF_ID_VALID` AFTER the read, proving the notif is
//!    still alive and the pid was not reused.
//! 4. Decide on the COPY.
//! 5. An ALLOWED destination is connected by the supervisor ITSELF:
//!    `pidfd_open`+`pidfd_getfd` dups the child's socket (same file
//!    description) into the supervisor, which acts on its own VERIFIED copy
//!    of the address and replies via `SECCOMP_IOCTL_NOTIF_SEND`. The kernel
//!    never re-reads child memory, so the race is structurally gone.
//! 6. A DENIED destination is recorded and answered `-ECONNREFUSED`.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use flowproof_trace::egress::{is_loopback, EgressEvent};

use crate::egress::{AllowSet, Containment, EgressLog};

// ---- seccomp / notify constants (not exposed by libc) ----

const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
const SECCOMP_GET_NOTIF_SIZES: libc::c_uint = 3;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;

// The notify ioctls, `_IOWR('!', n, struct)` / `_IOW('!', n, __u64)`. The
// struct sizes are fixed by the kernel ABI: seccomp_notif is 80 bytes,
// seccomp_notif_resp 24, and ID_VALID carries a bare __u64 (8).
const SECCOMP_IOCTL_NOTIF_RECV: libc::c_ulong = 0xc050_2100;
const SECCOMP_IOCTL_NOTIF_SEND: libc::c_ulong = 0xc018_2101;
const SECCOMP_IOCTL_NOTIF_ID_VALID: libc::c_ulong = 0x4008_2102;

// classic-BPF opcodes for the filter program.
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

// Offsets into `struct seccomp_data`: nr at 0, arch at 4, args[0] low word at
// 16 (little-endian). BPF loads are 32-bit words.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0_LO: u32 = 16;

// The audit arch of the process the filter runs in - the same arch flowproof
// itself was built for, since the child is native.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

// ---- kernel ABI structs (not exposed by libc) ----

// The kernel fills these by the fixed ABI layout; several fields exist only
// to pin that layout and are never read in Rust, hence `allow(dead_code)`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct SeccompNotif {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SeccompNotifResp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct SeccompNotifSizes {
    seccomp_notif: u16,
    seccomp_notif_resp: u16,
    seccomp_data: u16,
}

fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Build the default-deny egress BPF program. `A` (the accumulator) holds the
/// syscall number through every `nr ==` block below, since a matching block
/// exits via `RET` and a non-matching one only skips forward - so the socket
/// domain check, which reloads `args[0]`, must come LAST.
fn build_filter() -> Vec<libc::sock_filter> {
    let notif = SECCOMP_RET_USER_NOTIF;
    let eperm = SECCOMP_RET_ERRNO | (libc::EPERM as u32 & 0xffff);
    let allow = SECCOMP_RET_ALLOW;

    // `nr == X ? fall to RET : skip the RET` - two instructions per syscall,
    // order-independent.
    let guard = |nr: libc::c_long, action: u32| {
        vec![
            bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr as u32, 0, 1),
            bpf_stmt(BPF_RET | BPF_K, action),
        ]
    };

    // Prologue: reject a foreign arch (out of scope for v1 - a 32-bit compat
    // call is a documented punt) by allowing it, then load the syscall nr.
    let mut f = vec![
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARCH),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0),
        bpf_stmt(BPF_RET | BPF_K, allow),
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFF_NR),
    ];
    f.extend(guard(libc::SYS_io_uring_setup, eperm));
    f.extend(guard(libc::SYS_connect, notif));
    f.extend(guard(libc::SYS_sendto, notif));
    f.extend(guard(libc::SYS_sendmsg, notif));
    f.extend(guard(libc::SYS_sendmmsg, notif));
    f.extend(guard(libc::SYS_listen, notif));

    // socket(domain, ...): deny AF_PACKET, allow the rest. Reloads `args[0]`,
    // so it is last. If nr != socket, jump the 3 following insns to the
    // default allow.
    f.push(bpf_jump(
        BPF_JMP | BPF_JEQ | BPF_K,
        libc::SYS_socket as u32,
        0,
        3,
    ));
    f.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARG0_LO));
    f.push(bpf_jump(
        BPF_JMP | BPF_JEQ | BPF_K,
        libc::AF_PACKET as u32,
        0,
        1,
    ));
    f.push(bpf_stmt(BPF_RET | BPF_K, eperm));

    f.push(bpf_stmt(BPF_RET | BPF_K, allow));
    f
}

/// Probe whether this kernel can enforce containment: it needs seccomp
/// user-notification (>= 5.0) and `pidfd_getfd` (>= 5.6) for the supervisor
/// to act on the child's socket. `no_new_privs` is set on the child, which
/// breaks a setuid child - an accepted, documented limitation.
pub fn probe_containment() -> Containment {
    let not = |why: &str| Containment::NotContained(why.to_string());

    // `pidfd_getfd` (5.6): probe with bad args; ENOSYS means unsupported.
    let r = unsafe { libc::syscall(libc::SYS_pidfd_getfd, -1, -1, 0) };
    if r < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS) {
        return not("kernel lacks pidfd_getfd (needs >= 5.6)");
    }

    // seccomp user-notification: `GET_NOTIF_SIZES` succeeds iff the kernel
    // knows the notify ABI.
    let mut sizes = SeccompNotifSizes::default();
    let r = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_GET_NOTIF_SIZES as libc::c_long,
            0,
            &mut sizes as *mut SeccompNotifSizes,
        )
    };
    if r < 0 {
        return not("kernel lacks seccomp user-notification (needs >= 5.0)");
    }
    Containment::Enforced
}

/// A prepared filter plus the in-flight parent-side handoff, wired into a
/// `Command`'s `pre_exec`. Created BEFORE spawn; turned into a live supervisor
/// AFTER spawn.
///
/// The handoff runs on its OWN thread, started here in [`install`] rather than
/// after spawn, and this is load-bearing: `Command::spawn` BLOCKS until the
/// child execs, and the child cannot exec until the parent acks its notify fd.
/// If the parent tried to do the read+ack after `spawn` returned, it would
/// deadlock - the parent waiting in `spawn` for an exec that waits on an ack
/// the parent has not sent. So the ack must come from a concurrent thread.
pub struct EgressPrep {
    handoff: std::thread::JoinHandle<io::Result<OwnedFd>>,
    allow: AllowSet,
}

/// Install the egress filter into `cmd` via `pre_exec` and return the
/// parent-side handle. The filter and the child socket are moved into the
/// `pre_exec` closure; a handoff thread owns the parent's socket end and
/// acquires the notify fd out of the child while `spawn` runs.
pub fn install(cmd: &mut Command, allow: &AllowSet) -> io::Result<EgressPrep> {
    let filter = build_filter();

    // A stream socketpair carries the child's (pid, notify-fd-number) to the
    // parent and the parent's ack back. Both ends are close-on-exec: the child
    // sends before exec (pre_exec runs first), and the execed program must
    // inherit neither.
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let parent_sock = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let child_sock = fds[1];

    // SAFETY: the closure runs in the forked child before exec. It only makes
    // raw syscalls against data captured by value (the filter Vec, valid
    // across fork, and the child socket fd), allocating nothing - the
    // async-signal-safety contract `pre_exec` requires.
    unsafe {
        cmd.pre_exec(move || child_install(&filter, child_sock));
    }

    // The handoff thread: it blocks reading the child's message, so it must run
    // CONCURRENTLY with the caller's `cmd.spawn()` (see `EgressPrep`). It owns
    // `parent_sock` and yields the acquired notify fd.
    let handoff = std::thread::spawn(move || recv_notify_fd(parent_sock));

    Ok(EgressPrep {
        handoff,
        allow: allow.clone(),
    })
}

impl EgressPrep {
    /// After spawn: collect the notify fd the handoff thread acquired from the
    /// child, and start the supervisor thread that services it for the run.
    /// `spawn` is the instant the child was launched, the zero for every
    /// event's monotonic `at_ms`.
    pub fn into_supervisor(self, spawn: Instant) -> io::Result<Supervisor> {
        let notify_fd = self
            .handoff
            .join()
            .map_err(|_| io::Error::other("egress handoff thread panicked"))??;
        Ok(Supervisor::start(notify_fd, self.allow, spawn))
    }
}

/// Runs in the forked child before exec. Async-signal-safe: only raw
/// syscalls, no allocation.
fn child_install(filter: &[libc::sock_filter], child_sock: RawFd) -> io::Result<()> {
    // 1. No new privileges - required for an unprivileged filter.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    // 2. Install the filter with NEW_LISTENER; the return value is the notify
    //    fd, which the parent will service. The filter is active IMMEDIATELY,
    //    so from here on the trapped syscalls (connect/send*/listen) would
    //    suspend us on the notifier - hence the handoff below uses only
    //    `write`/`read`, which the filter does NOT trap.
    let notify_fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER as libc::c_long,
            SECCOMP_FILTER_FLAG_NEW_LISTENER as libc::c_long,
            &prog as *const libc::sock_fprog,
        )
    };
    if notify_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let notify_fd = notify_fd as RawFd;
    // 3. Hand the notify fd to the parent WITHOUT a trapped syscall: write our
    //    PID and the notify fd's NUMBER, then block on the parent's one-byte
    //    ack. The pid lets the handoff thread `pidfd_open`+`pidfd_getfd` the fd
    //    (it cannot use `child.id()` - `spawn` has not returned yet, since it
    //    blocks until we exec). We must NOT close `notify_fd` until the parent
    //    has acquired its own reference to the same open file description;
    //    closing early would tear down the listener. `getpid`/`write`/`read`
    //    are not in the filter, so none traps and the child never blocks on the
    //    un-serviced notifier.
    let pid = unsafe { libc::getpid() };
    let handoff = write_handoff(child_sock, pid, notify_fd).and_then(|()| wait_ack(child_sock));
    // 4. Whether the handoff succeeded or failed, drop both fds before exec so
    //    the child's real program (python) inherits NEITHER the notify fd nor
    //    the socketpair. The socketpair is already CLOEXEC, but the notify fd
    //    is not, so this explicit close is what keeps it out of the exec.
    unsafe {
        libc::close(notify_fd);
        libc::close(child_sock);
    }
    handoff
}

/// Write the child's `(pid, notify-fd-number)` as two native-endian `i32`s to
/// the socketpair. Used in place of an `SCM_RIGHTS`/`sendmsg` fd passage,
/// because `sendmsg` is trapped by the filter that is already installed;
/// `write` is not.
fn write_handoff(sock: RawFd, pid: libc::pid_t, fd: RawFd) -> io::Result<()> {
    // RawFd and pid_t are both i32 on Linux: a fixed 8-byte message the parent
    // reads back with `i32::from_ne_bytes`.
    let mut msg = [0u8; 8];
    msg[0..4].copy_from_slice(&pid.to_ne_bytes());
    msg[4..8].copy_from_slice(&fd.to_ne_bytes());
    write_all(sock, &msg)
}

/// Block until the parent writes its one-byte ack, meaning it has acquired the
/// notify fd and the child may close its copy and proceed to exec. `read` is
/// not trapped by the filter.
fn wait_ack(sock: RawFd) -> io::Result<()> {
    let mut ack = [0u8; 1];
    read_exact(sock, &mut ack)
}

/// Parent side of the handoff, run on its own thread (see [`install`]): read
/// the child's `(pid, notify-fd-number)`, acquire the actual fd out of the
/// child with `pidfd_open`+`pidfd_getfd` (the same mechanism [`dup_child_fd`]
/// uses for the child's sockets), then write the one-byte ack so the child can
/// close its copy and exec. Owns `sock` and drops it on return.
fn recv_notify_fd(sock: OwnedFd) -> io::Result<OwnedFd> {
    let raw = sock.as_raw_fd();
    // Bound the wait for the child's message. On the happy path the child
    // writes within milliseconds; a bound means that if `cmd.spawn` FAILS (the
    // child never runs) this thread errors out instead of blocking forever on a
    // socketpair whose peer never speaks.
    if !poll_readable(raw, HANDOFF_TIMEOUT_MS)? {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "egress handoff: child never sent its notify fd",
        ));
    }
    let mut msg = [0u8; 8];
    read_exact(raw, &mut msg)?;
    let child_pid = i32::from_ne_bytes([msg[0], msg[1], msg[2], msg[3]]) as u32;
    let fd_number = i32::from_ne_bytes([msg[4], msg[5], msg[6], msg[7]]) as RawFd;
    // Acquire OUR OWN reference to the child's notify file description. Once we
    // hold it, the child closing its copy does not tear down the listener.
    // pidfd_getfd sets CLOEXEC on the returned fd, so flowproof's own future
    // execs do not leak it.
    let notify_fd = dup_child_fd(child_pid, fd_number)?;
    // Ack: the child is blocked on this one byte before it closes and execs.
    write_all(raw, &[1u8])?;
    Ok(notify_fd)
}

/// How long the parent handoff thread waits for the child's message before
/// giving up. Generous: the child writes within milliseconds on success, so
/// this only fires when the child never ran (a failed `cmd.spawn`).
const HANDOFF_TIMEOUT_MS: libc::c_int = 30_000;

/// `poll` a fd for readability with a millisecond timeout. `Ok(true)` means
/// readable (or an error condition the following read will surface),
/// `Ok(false)` means the timeout elapsed with nothing to read.
fn poll_readable(sock: RawFd, timeout_ms: libc::c_int) -> io::Result<bool> {
    loop {
        let mut pfd = libc::pollfd {
            fd: sock,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(rc > 0);
    }
}

/// `write` the whole buffer, retrying short writes and EINTR. Async-signal-safe
/// (used in the child): no allocation, so failures carry a bare errno rather
/// than a formatted message.
fn write_all(sock: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(sock, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::Error::from_raw_os_error(libc::EPIPE));
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// `read` exactly `buf.len()` bytes, retrying short reads and EINTR. A closed
/// peer (0 bytes) before the buffer is filled is an error. Async-signal-safe.
fn read_exact(sock: RawFd, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::read(sock, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::Error::from_raw_os_error(libc::ECONNRESET));
        }
        buf = &mut buf[n as usize..];
    }
    Ok(())
}

/// The live supervisor: a thread servicing the notify fd, plus the shared log
/// it fills and a stop flag to end the loop when the child is gone.
pub struct Supervisor {
    handle: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<EgressLog>>,
}

impl Supervisor {
    fn start(notify_fd: OwnedFd, allow: AllowSet, spawn: Instant) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(EgressLog::default()));
        let handle = {
            let (stop, log) = (Arc::clone(&stop), Arc::clone(&log));
            std::thread::spawn(move || serve(notify_fd, &allow, spawn, &stop, &log))
        };
        Supervisor {
            handle: Some(handle),
            stop,
            log,
        }
    }

    /// Stop servicing and return everything the run attempted. Called after
    /// the child has exited.
    pub fn stop_and_collect(mut self) -> EgressLog {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        std::mem::take(&mut self.log.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The supervisor loop: poll the notify fd, receive one notification, decide,
/// and reply. Exits when `stop` is set (the child has been waited on).
fn serve(
    notify_fd: OwnedFd,
    allow: &AllowSet,
    spawn: Instant,
    stop: &AtomicBool,
    log: &Mutex<EgressLog>,
) {
    let fd = notify_fd.as_raw_fd();
    loop {
        // Poll with a short timeout so the loop notices `stop` even when the
        // child is idle and no notification is pending.
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if ready <= 0 {
            continue;
        }

        let mut req: SeccompNotif = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::ioctl(fd, SECCOMP_IOCTL_NOTIF_RECV, &mut req) };
        if rc != 0 {
            // ENOENT: the notification vanished (target died) - fine, retry.
            // Anything else on a torn-down fd ends the loop.
            let e = io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if e == libc::ENOENT || e == libc::EINTR {
                continue;
            }
            break;
        }

        if let Some(resp) = decide(fd, &req, allow, spawn, log) {
            let mut resp = resp;
            resp.id = req.id;
            // A failed SEND means the notif died between decide and reply -
            // harmless; the child was killed or the syscall interrupted.
            unsafe {
                libc::ioctl(fd, SECCOMP_IOCTL_NOTIF_SEND, &mut resp);
            }
        }
    }
}

/// Decide one notification. `None` means "do not reply" (the notif is no
/// longer valid, so any reply would fail anyway).
fn decide(
    fd: RawFd,
    req: &SeccompNotif,
    allow: &AllowSet,
    spawn: Instant,
    log: &Mutex<EgressLog>,
) -> Option<SeccompNotifResp> {
    let nr = req.data.nr as libc::c_long;
    if nr == libc::SYS_connect {
        Some(handle_connect(fd, req, allow, spawn, log))
    } else if nr == libc::SYS_sendto {
        Some(handle_sendto(fd, req, allow, spawn, log))
    } else if nr == libc::SYS_sendmsg || nr == libc::SYS_sendmmsg {
        Some(handle_sendmsg(fd, req, allow, spawn, log))
    } else if nr == libc::SYS_listen {
        Some(handle_listen(req))
    } else {
        // Unreachable given the filter, but fail safe: deny.
        Some(errno_resp(libc::EPERM))
    }
}

/// `connect(fd, addr, addrlen)`: the core TOCTOU-safe path.
fn handle_connect(
    fd: RawFd,
    req: &SeccompNotif,
    allow: &AllowSet,
    spawn: Instant,
    log: &Mutex<EgressLog>,
) -> SeccompNotifResp {
    let sockfd = req.data.args[0] as RawFd;
    let addr_ptr = req.data.args[1];
    let addr_len = req.data.args[2] as usize;

    let mut buf = [0u8; 128];
    let read = read_child(req.pid, addr_ptr, &mut buf, addr_len);
    // ID_VALID AFTER the read: proves the notif is still alive and the pid
    // was not reused before we decide on the copy.
    if !notif_id_valid(fd, req.id) {
        return errno_resp(libc::EPERM);
    }
    let Ok(n) = read else {
        return errno_resp(libc::EPERM);
    };

    match parse_sockaddr(&buf[..n]) {
        // AF_UNIX is exempt (allowed): the supervisor performs it.
        Some(Dest::Unix) => perform_connect(req.pid, sockfd, &buf[..n]),
        Some(Dest::Inet(ip, port)) => {
            let ip = normalize(ip);
            if allow.allows(ip, port) {
                perform_connect(req.pid, sockfd, &buf[..n])
            } else {
                record(log, spawn, &format!("{ip}:{port}"), "tcp");
                errno_resp(libc::ECONNREFUSED)
            }
        }
        // An address family we do not model: deny rather than guess.
        None => errno_resp(libc::EPERM),
    }
}

/// `sendto(fd, buf, len, flags, dest_addr, addrlen)`. A NULL dest is a send
/// on a connected socket, allowed. A present dest is vetted like connect;
/// off-host unconnected UDP is denied (v1).
fn handle_sendto(
    fd: RawFd,
    req: &SeccompNotif,
    allow: &AllowSet,
    spawn: Instant,
    log: &Mutex<EgressLog>,
) -> SeccompNotifResp {
    let sockfd = req.data.args[0] as RawFd;
    let buf_ptr = req.data.args[1];
    let buf_len = req.data.args[2] as usize;
    let flags = req.data.args[3] as libc::c_int;
    let dest_ptr = req.data.args[4];
    let dest_len = req.data.args[5] as usize;

    // The destination decision, made on a verified copy (or none).
    let dest = if dest_ptr == 0 || dest_len == 0 {
        None
    } else {
        let mut addr = [0u8; 128];
        let read = read_child(req.pid, dest_ptr, &mut addr, dest_len);
        if !notif_id_valid(fd, req.id) {
            return errno_resp(libc::EPERM);
        }
        let Ok(n) = read else {
            return errno_resp(libc::EPERM);
        };
        match parse_sockaddr(&addr[..n]) {
            Some(Dest::Inet(ip, port)) => {
                let ip = normalize(ip);
                if !is_loopback(ip) && !allow.allows(ip, port) {
                    record(log, spawn, &format!("{ip}:{port}"), "udp");
                    return errno_resp(libc::ECONNREFUSED);
                }
                Some((addr, n))
            }
            Some(Dest::Unix) => Some((addr, n)),
            None => return errno_resp(libc::EPERM),
        }
    };
    if dest_ptr != 0 && dest_len != 0 {
        // Re-validate liveness before acting on the buffer copy below.
        if !notif_id_valid(fd, req.id) {
            return errno_resp(libc::EPERM);
        }
    }
    perform_sendto(req.pid, sockfd, buf_ptr, buf_len, flags, dest)
}

/// `sendmsg`/`sendmmsg`: read the message header, vet its `msg_name`, and
/// re-perform the send from a verified copy. v1 punts control messages and
/// multi-message `sendmmsg` bodies: an off-host destination is always denied,
/// so no undeclared egress escapes; a complex-but-on-host message that we
/// cannot fully marshal is failed with EPERM rather than sent blindly.
fn handle_sendmsg(
    fd: RawFd,
    req: &SeccompNotif,
    allow: &AllowSet,
    spawn: Instant,
    log: &Mutex<EgressLog>,
) -> SeccompNotifResp {
    let sockfd = req.data.args[0] as RawFd;
    let msg_ptr = req.data.args[1];

    // Read the msghdr to find msg_name (the destination), if any.
    let mut hdr = [0u8; std::mem::size_of::<libc::msghdr>()];
    let hdr_len = hdr.len();
    let read = read_child(req.pid, msg_ptr, &mut hdr, hdr_len);
    if !notif_id_valid(fd, req.id) {
        return errno_resp(libc::EPERM);
    }
    if read.is_err() {
        return errno_resp(libc::EPERM);
    }
    let (name_ptr, name_len, control_len) = msghdr_fields(&hdr);

    if name_ptr != 0 && name_len != 0 {
        let mut addr = [0u8; 128];
        let read = read_child(req.pid, name_ptr, &mut addr, name_len as usize);
        if !notif_id_valid(fd, req.id) {
            return errno_resp(libc::EPERM);
        }
        let Ok(n) = read else {
            return errno_resp(libc::EPERM);
        };
        if let Some(Dest::Inet(ip, port)) = parse_sockaddr(&addr[..n]) {
            let ip = normalize(ip);
            if !is_loopback(ip) && !allow.allows(ip, port) {
                record(log, spawn, &format!("{ip}:{port}"), "udp");
                return errno_resp(libc::ECONNREFUSED);
            }
        }
    }
    // Control data (e.g. SCM_RIGHTS) is not marshalled in v1: refuse rather
    // than forward something we did not fully copy.
    if control_len != 0 {
        return errno_resp(libc::EPERM);
    }
    // On-host / connected: re-perform on the child's own socket. sendmmsg is
    // serviced as its first message; the child sees one message sent.
    perform_sendmsg(req.pid, sockfd, msg_ptr)
}

/// `listen(fd, backlog)`: a NON-loopback listener is the accept-based exfil
/// hole; deny and log it. A loopback listener is performed on the child's
/// socket.
fn handle_listen(req: &SeccompNotif) -> SeccompNotifResp {
    let sockfd = req.data.args[0] as RawFd;
    let backlog = req.data.args[1] as libc::c_int;
    let Ok(dup) = dup_child_fd(req.pid, sockfd) else {
        return errno_resp(libc::EPERM);
    };
    // getsockname on the dup (same file description) to see where it is bound.
    let mut addr = [0u8; 128];
    let mut len = addr.len() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockname(
            dup.as_raw_fd(),
            addr.as_mut_ptr() as *mut libc::sockaddr,
            &mut len,
        )
    };
    if rc != 0 {
        return errno_resp(libc::EPERM);
    }
    let bound_loopback = match parse_sockaddr(&addr[..len as usize]) {
        Some(Dest::Inet(ip, _)) => is_loopback(normalize(ip)),
        Some(Dest::Unix) => true,
        None => false,
    };
    if !bound_loopback {
        return errno_resp(libc::EACCES);
    }
    let rc = unsafe { libc::listen(dup.as_raw_fd(), backlog) };
    if rc == 0 {
        ok_resp(0)
    } else {
        errno_resp(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EPERM),
        )
    }
}

/// The supervisor performs `connect` on a dup of the child's socket, using
/// its own VERIFIED copy of the address. A nonblocking socket's EINPROGRESS
/// is returned to the child, which epolls its own fd (the same description) -
/// correct, and the kernel never re-reads child memory.
fn perform_connect(pid: u32, sockfd: RawFd, addr: &[u8]) -> SeccompNotifResp {
    let Ok(dup) = dup_child_fd(pid, sockfd) else {
        return errno_resp(libc::EPERM);
    };
    let rc = unsafe {
        libc::connect(
            dup.as_raw_fd(),
            addr.as_ptr() as *const libc::sockaddr,
            addr.len() as libc::socklen_t,
        )
    };
    if rc == 0 {
        ok_resp(0)
    } else {
        errno_resp(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EPERM),
        )
    }
}

/// Perform `sendto` from a supervisor-side copy of the payload and the
/// verified destination.
fn perform_sendto(
    pid: u32,
    sockfd: RawFd,
    buf_ptr: u64,
    buf_len: usize,
    flags: libc::c_int,
    dest: Option<([u8; 128], usize)>,
) -> SeccompNotifResp {
    let Ok(dup) = dup_child_fd(pid, sockfd) else {
        return errno_resp(libc::EPERM);
    };
    // A large payload is bounded; a genuinely huge datagram is rare and, if
    // truncated, the child simply sees a short send.
    let mut payload = vec![0u8; buf_len.min(256 * 1024)];
    let payload_len = payload.len();
    let n = read_child(pid, buf_ptr, &mut payload, payload_len).unwrap_or(0);
    let (addr_ptr, addr_len) = match &dest {
        Some((addr, len)) => (
            addr.as_ptr() as *const libc::sockaddr,
            *len as libc::socklen_t,
        ),
        None => (std::ptr::null(), 0),
    };
    let sent = unsafe {
        libc::sendto(
            dup.as_raw_fd(),
            payload.as_ptr() as *const libc::c_void,
            n,
            flags,
            addr_ptr,
            addr_len,
        )
    };
    if sent >= 0 {
        ok_resp(sent as i64)
    } else {
        errno_resp(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EPERM),
        )
    }
}

/// Perform a single `sendmsg` on the child's socket. The iovec buffers are
/// copied into supervisor memory; the destination and flags come from the
/// child's own header (already vetted by the caller).
fn perform_sendmsg(pid: u32, sockfd: RawFd, msg_ptr: u64) -> SeccompNotifResp {
    let Ok(dup) = dup_child_fd(pid, sockfd) else {
        return errno_resp(libc::EPERM);
    };
    let mut hdr = [0u8; std::mem::size_of::<libc::msghdr>()];
    let hdr_len = hdr.len();
    if read_child(pid, msg_ptr, &mut hdr, hdr_len).is_err() {
        return errno_resp(libc::EPERM);
    }
    let (name_ptr, name_len, _control_len) = msghdr_fields(&hdr);
    let (iov_ptr, iov_count) = msghdr_iov(&hdr);
    // Bound the number of iovecs marshalled; a normal send has a handful.
    let iov_count = iov_count.min(64);

    // Copy the destination name, if any.
    let mut name = [0u8; 128];
    let name_actual = if name_ptr != 0 && name_len != 0 {
        read_child(pid, name_ptr, &mut name, name_len as usize).unwrap_or(0)
    } else {
        0
    };

    // Copy each iovec descriptor, then its buffer.
    let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(iov_count);
    let mut iovs: Vec<libc::iovec> = Vec::with_capacity(iov_count);
    for i in 0..iov_count {
        let mut desc = [0u8; std::mem::size_of::<libc::iovec>()];
        let desc_len = desc.len();
        let at = iov_ptr + (i * std::mem::size_of::<libc::iovec>()) as u64;
        if read_child(pid, at, &mut desc, desc_len).is_err() {
            return errno_resp(libc::EPERM);
        }
        let (base, len) = iovec_fields(&desc);
        let len = len.min(256 * 1024);
        let mut data = vec![0u8; len];
        let got = read_child(pid, base, &mut data, len).unwrap_or(0);
        data.truncate(got);
        buffers.push(data);
    }
    for buf in &mut buffers {
        iovs.push(libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        });
    }

    let mut out: libc::msghdr = unsafe { std::mem::zeroed() };
    if name_actual != 0 {
        out.msg_name = name.as_mut_ptr() as *mut libc::c_void;
        out.msg_namelen = name_actual as libc::socklen_t;
    }
    out.msg_iov = iovs.as_mut_ptr();
    out.msg_iovlen = iovs.len() as _;
    let sent = unsafe { libc::sendmsg(dup.as_raw_fd(), &out, 0) };
    if sent >= 0 {
        ok_resp(sent as i64)
    } else {
        errno_resp(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EPERM),
        )
    }
}

/// A destination parsed from a `sockaddr` copy.
enum Dest {
    Inet(IpAddr, u16),
    Unix,
}

/// Parse the family off a `sockaddr` copy and, for AF_INET/AF_INET6, the
/// address and port. Reads bytes directly (the copy is unaligned).
fn parse_sockaddr(buf: &[u8]) -> Option<Dest> {
    if buf.len() < 2 {
        return None;
    }
    let family = u16::from_ne_bytes([buf[0], buf[1]]) as libc::c_int;
    if family == libc::AF_UNIX {
        return Some(Dest::Unix);
    }
    if family == libc::AF_INET {
        if buf.len() < 8 {
            return None;
        }
        let port = u16::from_be_bytes([buf[2], buf[3]]);
        let ip = Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
        return Some(Dest::Inet(IpAddr::V4(ip), port));
    }
    if family == libc::AF_INET6 {
        if buf.len() < 24 {
            return None;
        }
        let port = u16::from_be_bytes([buf[2], buf[3]]);
        let mut octets = [0u8; 16];
        octets.copy_from_slice(&buf[8..24]);
        return Some(Dest::Inet(IpAddr::V6(Ipv6Addr::from(octets)), port));
    }
    None
}

/// Collapse a v4-mapped-v6 address to its v4 form, so an allow-set entry and
/// a mapped destination compare equal (design step 4).
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// `process_vm_readv` a bounded region of child memory into `buf`. `want` is
/// the syscall-provided length; the read is clamped to the buffer.
fn read_child(pid: u32, remote: u64, buf: &mut [u8], want: usize) -> io::Result<usize> {
    let len = want.min(buf.len());
    if len == 0 {
        return Ok(0);
    }
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: len,
    };
    let remote_iov = libc::iovec {
        iov_base: remote as *mut libc::c_void,
        iov_len: len,
    };
    let n = unsafe { libc::process_vm_readv(pid as libc::pid_t, &local, 1, &remote_iov, 1, 0) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// Dup a fd out of the child into the supervisor (same file description), via
/// `pidfd_open` + `pidfd_getfd`.
fn dup_child_fd(pid: u32, fd: RawFd) -> io::Result<OwnedFd> {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if pidfd < 0 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) };
    let newfd = unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), fd, 0) };
    if newfd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(newfd as RawFd) })
}

/// Is the notification still valid? Must be checked AFTER a memory read and
/// BEFORE acting, to prove the pid was not reused.
fn notif_id_valid(fd: RawFd, id: u64) -> bool {
    unsafe { libc::ioctl(fd, SECCOMP_IOCTL_NOTIF_ID_VALID, &id as *const u64) == 0 }
}

/// Extract `(msg_name, msg_namelen, msg_controllen)` from a raw msghdr copy.
fn msghdr_fields(hdr: &[u8]) -> (u64, u32, usize) {
    let msg: libc::msghdr =
        unsafe { std::ptr::read_unaligned(hdr.as_ptr() as *const libc::msghdr) };
    (
        msg.msg_name as u64,
        msg.msg_namelen,
        msg.msg_controllen as usize,
    )
}

/// Extract `(msg_iov ptr, msg_iovlen)` from a raw msghdr copy.
fn msghdr_iov(hdr: &[u8]) -> (u64, usize) {
    let msg: libc::msghdr =
        unsafe { std::ptr::read_unaligned(hdr.as_ptr() as *const libc::msghdr) };
    (msg.msg_iov as u64, msg.msg_iovlen as usize)
}

/// Extract `(iov_base ptr, iov_len)` from a raw iovec copy.
fn iovec_fields(desc: &[u8]) -> (u64, usize) {
    let iov: libc::iovec = unsafe { std::ptr::read_unaligned(desc.as_ptr() as *const libc::iovec) };
    (iov.iov_base as u64, iov.iov_len)
}

/// Record a denied egress attempt into the shared log.
fn record(log: &Mutex<EgressLog>, spawn: Instant, destination: &str, protocol: &str) {
    let event = EgressEvent {
        destination: destination.to_string(),
        protocol: protocol.to_string(),
        at_ms: spawn.elapsed().as_millis() as u64,
    };
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .blocked
        .push(event);
}

/// A "the syscall returned `val`" reply.
fn ok_resp(val: i64) -> SeccompNotifResp {
    SeccompNotifResp {
        id: 0,
        val,
        error: 0,
        flags: 0,
    }
}

/// A "the syscall failed with `errno`" reply. The kernel returns `-errno` to
/// the child.
fn errno_resp(errno: libc::c_int) -> SeccompNotifResp {
    SeccompNotifResp {
        id: 0,
        val: 0,
        error: -errno,
        flags: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_is_well_formed_and_default_allows() {
        let f = build_filter();
        // A non-trivial program with a terminating default-allow.
        assert!(f.len() > 10);
        let last = f.last().expect("non-empty");
        assert_eq!(last.code, BPF_RET | BPF_K);
        assert_eq!(last.k, SECCOMP_RET_ALLOW);
    }

    #[test]
    fn parse_sockaddr_reads_v4_v6_and_unix() {
        // AF_INET 198.51.100.9:443.
        let mut v4 = [0u8; 16];
        v4[0..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
        v4[2..4].copy_from_slice(&443u16.to_be_bytes());
        v4[4..8].copy_from_slice(&[198, 51, 100, 9]);
        match parse_sockaddr(&v4) {
            Some(Dest::Inet(IpAddr::V4(ip), port)) => {
                assert_eq!(ip, Ipv4Addr::new(198, 51, 100, 9));
                assert_eq!(port, 443);
            }
            _ => panic!("v4 parse"),
        }

        // AF_UNIX.
        let mut un = [0u8; 4];
        un[0..2].copy_from_slice(&(libc::AF_UNIX as u16).to_ne_bytes());
        assert!(matches!(parse_sockaddr(&un), Some(Dest::Unix)));

        // A too-short buffer yields nothing.
        assert!(parse_sockaddr(&[1]).is_none());
    }

    #[test]
    fn v4_mapped_v6_normalizes_to_v4() {
        let mapped: IpAddr = "::ffff:198.51.100.9".parse().expect("ipv6");
        assert_eq!(
            normalize(mapped),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))
        );
    }

    #[test]
    fn errno_resp_returns_negative_errno() {
        assert_eq!(errno_resp(libc::ECONNREFUSED).error, -libc::ECONNREFUSED);
        assert_eq!(ok_resp(5).val, 5);
        assert_eq!(ok_resp(5).error, 0);
    }
}
