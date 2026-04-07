//! Seccomp BPF filter for defense-in-depth inside the sandbox.
//!
//! Blocks syscalls that could be used to escape the sandbox (creating new
//! namespaces, mounting filesystems, tracing processes). This runs after
//! namespace setup, pivot_root, and Landlock — it is the innermost security
//! layer.
//!
//! Uses raw BPF instructions via libc. The filter is small (~12 instructions)
//! and avoids adding a seccomp crate dependency.

use anyhow::Result;

// Seccomp return values (not always exported by libc)
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

// seccomp_data field offsets
const OFFSET_NR: u32 = 0;   // syscall number
const OFFSET_ARCH: u32 = 4; // audit architecture

// Audit architecture constants
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_NATIVE: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64

#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_NATIVE: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64

/// Syscalls blocked inside the sandbox.
///
/// These could be used to escape namespace/mount isolation or
/// interfere with other processes.
const BLOCKED_SYSCALLS: &[libc::c_long] = &[
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_unshare,
    libc::SYS_ptrace,
    libc::SYS_kexec_load,
    libc::SYS_open_by_handle_at,
];

/// Build a BPF instruction.
const fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

/// Build a BPF jump instruction.
const fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Build the seccomp BPF filter program.
///
/// Layout (n = number of blocked syscalls):
///   [0]     Load architecture
///   [1]     Check architecture — kill if wrong
///   [2]     Load syscall number
///   [3..3+n) Check each blocked syscall — jump to DENY
///   [3+n]   ALLOW (default)
///   [4+n]   DENY (return EPERM)
///   [5+n]   KILL (wrong architecture)
fn build_filter() -> Vec<libc::sock_filter> {
    let n = BLOCKED_SYSCALLS.len();
    let mut filter = Vec::with_capacity(n + 6);

    // [0] Load architecture from seccomp_data.arch
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        OFFSET_ARCH,
    ));

    // [1] If architecture matches, continue; otherwise jump to KILL at [5+n].
    // jf offset = (5+n) - (1+1) = n+3
    filter.push(bpf_jump(
        (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
        AUDIT_ARCH_NATIVE,
        0,                   // jt: fall through to [2]
        (n + 3) as u8,       // jf: jump to KILL
    ));

    // [2] Load syscall number from seccomp_data.nr
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        OFFSET_NR,
    ));

    // [3..3+n) Check each blocked syscall.
    // DENY is at index [4+n]. From [3+i], jt offset = (4+n) - (3+i+1) = n-i.
    for (i, &syscall) in BLOCKED_SYSCALLS.iter().enumerate() {
        let jt = (n - i) as u8;
        filter.push(bpf_jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            syscall as u32,
            jt,  // jump to DENY
            0,   // fall through to next check
        ));
    }

    // [3+n] ALLOW — default action
    filter.push(bpf_stmt(
        libc::BPF_RET as u16,
        SECCOMP_RET_ALLOW,
    ));

    // [4+n] DENY — return EPERM
    filter.push(bpf_stmt(
        libc::BPF_RET as u16,
        SECCOMP_RET_ERRNO | libc::EPERM as u32,
    ));

    // [5+n] KILL — wrong architecture
    filter.push(bpf_stmt(
        libc::BPF_RET as u16,
        SECCOMP_RET_KILL_PROCESS,
    ));

    filter
}

/// Install the seccomp BPF filter.
///
/// Must be called after `prctl(PR_SET_NO_NEW_PRIVS, 1)` — the kernel
/// requires this before installing a seccomp filter as an unprivileged user.
///
/// After this call, the blocked syscalls return `EPERM` instead of executing.
pub fn apply_seccomp_filter() -> Result<()> {
    let filter = build_filter();

    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };

    let ret = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
        )
    };

    if ret != 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("seccomp filter installation failed: {}", err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_has_correct_structure() {
        let filter = build_filter();
        let n = BLOCKED_SYSCALLS.len();

        // Expected: 3 (arch check + load nr) + n (syscall checks) + 3 (allow + deny + kill)
        assert_eq!(filter.len(), n + 6);

        // First instruction loads architecture
        assert_eq!(filter[0].code, (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16);
        assert_eq!(filter[0].k, OFFSET_ARCH);

        // Second instruction checks architecture
        assert_eq!(filter[1].code, (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16);
        assert_eq!(filter[1].k, AUDIT_ARCH_NATIVE);

        // Third instruction loads syscall number
        assert_eq!(filter[2].code, (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16);
        assert_eq!(filter[2].k, OFFSET_NR);

        // Syscall checks
        for (i, &syscall) in BLOCKED_SYSCALLS.iter().enumerate() {
            let inst = &filter[3 + i];
            assert_eq!(inst.code, (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16);
            assert_eq!(inst.k, syscall as u32);
        }

        // Last three: ALLOW, DENY (EPERM), KILL
        let allow = &filter[n + 3];
        assert_eq!(allow.code, libc::BPF_RET as u16);
        assert_eq!(allow.k, SECCOMP_RET_ALLOW);

        let deny = &filter[n + 4];
        assert_eq!(deny.code, libc::BPF_RET as u16);
        assert_eq!(deny.k, SECCOMP_RET_ERRNO | libc::EPERM as u32);

        let kill = &filter[n + 5];
        assert_eq!(kill.code, libc::BPF_RET as u16);
        assert_eq!(kill.k, SECCOMP_RET_KILL_PROCESS);
    }

    #[test]
    fn blocked_syscalls_includes_critical_calls() {
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_mount));
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_unshare));
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_ptrace));
        assert!(BLOCKED_SYSCALLS.contains(&libc::SYS_kexec_load));
    }

    #[test]
    fn jump_offsets_are_correct() {
        let filter = build_filter();
        let n = BLOCKED_SYSCALLS.len();

        // BPF jump offsets are relative to PC+1.
        // Layout: [0]LD arch [1]JEQ arch [2]LD nr [3..3+n)checks [n+3]ALLOW [n+4]DENY [n+5]KILL

        // Verify arch check jf reaches KILL (last instruction)
        let arch_check = &filter[1];
        let kill_idx = filter.len() - 1; // KILL is last
        let expected_jf = (kill_idx - 2) as u8; // from instruction 1, jf = target - (current+1) = (n+5) - 2 = n+3
        assert_eq!(
            arch_check.jf, expected_jf,
            "arch check jf should jump to KILL (index {}), got offset {} (reaches index {})",
            kill_idx,
            arch_check.jf,
            2 + arch_check.jf as usize
        );

        // Verify each syscall check jt reaches DENY (second-to-last instruction)
        let deny_idx = filter.len() - 2;
        for i in 0..n {
            let check = &filter[3 + i];
            let expected_jt = (deny_idx - (3 + i + 1)) as u8;
            assert_eq!(
                check.jt, expected_jt,
                "syscall check {} jt should jump to DENY (index {}), got offset {} (reaches index {})",
                i,
                deny_idx,
                check.jt,
                3 + i + 1 + check.jt as usize
            );
        }
    }
}
