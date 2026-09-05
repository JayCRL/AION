//! Seccomp 适配器：seccomp-BPF 系统调用过滤（默认拒绝 + 允许清单）。

use async_trait::async_trait;

use crate::{AdapterError, AdapterResult};

/// 默认动作：清单之外的系统调用如何处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompDefault {
    /// 返回 EPERM（温和，便于调试）。
    Errno,
    /// 直接杀死进程（严格）。
    Kill,
}

/// Seccomp 策略：允许清单 + 默认动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeccompPolicy {
    /// 允许的系统调用号。
    pub allow: Vec<i64>,
    /// 清单之外的默认动作。
    pub default: SeccompDefault,
}

impl SeccompPolicy {
    pub fn new(allow: Vec<i64>, default: SeccompDefault) -> Self {
        SeccompPolicy { allow, default }
    }

    /// Agent 沙箱默认策略：常见安全系统调用白名单 + EPERM。
    pub fn default_allowlist() -> Self {
        SeccompPolicy {
            allow: default_allowlist(),
            default: SeccompDefault::Errno,
        }
    }
}

#[cfg(target_os = "linux")]
mod bpf {
    // BPF 常量（linux/filter.h）
    pub const BPF_LD: u16 = 0x00;
    pub const BPF_W: u16 = 0x00;
    pub const BPF_ABS: u16 = 0x20;
    pub const BPF_JMP: u16 = 0x05;
    pub const BPF_JEQ: u16 = 0x10;
    pub const BPF_RET: u16 = 0x06;
    pub const BPF_K: u16 = 0x00;

    // seccomp 返回值（linux/seccomp.h）
    pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    pub const EPERM: u32 = 1;

    // AUDIT_ARCH_X86_64 / AUDIT_ARCH_AARCH64
    #[cfg(target_arch = "x86_64")]
    pub const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    pub const AUDIT_ARCH: u32 = 0xc000_00b7;

    pub fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt: 0, jf: 0, k }
    }

    pub fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
}

/// 编译 seccomp-BPF 过滤器。
///
/// 程序结构：
/// 1. 校验体系结构（AUDIT_ARCH_*）
/// 2. 载入系统调用号
/// 3. 逐条与允许清单比较，命中则跳到 ALLOW
/// 4. 默认返回 `default` 动作
#[cfg(target_os = "linux")]
pub fn build_filter(policy: &SeccompPolicy) -> Vec<libc::sock_filter> {
    use bpf::*;

    let n = policy.allow.len();
    // [ld_arch, jeq_arch, ld_nr, n × jeq_nr, ret_default, ret_allow]
    let idx_arch_check = 1usize;
    let idx_nr = 2usize;
    let idx_ret_default = 2 + n;
    let idx_ret_allow = idx_ret_default + 1;

    let ret_default = match policy.default {
        SeccompDefault::Errno => SECCOMP_RET_ERRNO | EPERM,
        SeccompDefault::Kill => SECCOMP_RET_KILL_PROCESS,
    };

    let mut prog = Vec::with_capacity(idx_ret_allow + 1);
    // 载入 seccomp_data.arch（偏移 4）
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 4));
    // 架构不符 → 默认动作
    prog.push(jump(
        BPF_JMP | BPF_JEQ | BPF_K,
        AUDIT_ARCH,
        0,
        (idx_ret_default - idx_arch_check - 1) as u8,
    ));
    // 载入 seccomp_data.nr（偏移 0）
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, 0));
    for (i, nr) in policy.allow.iter().enumerate() {
        // 命中 → 跳到 ALLOW
        prog.push(jump(
            BPF_JMP | BPF_JEQ | BPF_K,
            *nr as u32,
            (idx_ret_allow - (idx_nr + i) - 1) as u8,
            0,
        ));
    }
    prog.push(stmt(BPF_RET | BPF_K, ret_default));
    prog.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    prog
}

#[cfg(target_os = "linux")]
const PR_SET_NO_NEW_PRIVS: i32 = 38;
#[cfg(target_os = "linux")]
const PR_SET_SECCOMP: i32 = 22;
#[cfg(target_os = "linux")]
const SECCOMP_MODE_FILTER: u32 = 2;

/// 同步安装 seccomp 过滤器，供 `pre_exec` 调用。
/// 调用前进程必须已设置 `no_new_privs`（本函数会一并设置）。
#[cfg(target_os = "linux")]
pub fn install(policy: &SeccompPolicy) -> AdapterResult<()> {
    // SAFETY: prctl 与 seccomp(2) 均为标准接口；filter 生命周期覆盖调用期。
    unsafe {
        let rc = libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        if rc != 0 {
            return Err(AdapterError::Io(std::io::Error::last_os_error()));
        }
    }
    let mut prog = build_filter(policy);
    let fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_mut_ptr(),
    };
    // SAFETY: fprog 指向的内存在本调用期间有效。
    unsafe {
        let rc = libc::prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER as libc::c_ulong,
            &fprog as *const libc::sock_fprog as libc::c_ulong,
            0,
            0,
        );
        if rc != 0 {
            return Err(AdapterError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// Seccomp 适配器 trait。
#[async_trait]
pub trait SeccompAdapter: Send + Sync {
    fn supported(&self) -> bool;

    /// 对当前线程/进程安装过滤器（不可逆，仅应在 exec 前调用）。
    async fn install_current(&self, policy: &SeccompPolicy) -> AdapterResult<()>;
}

/// 平台原生实现。
pub struct NativeSeccompAdapter;

#[async_trait]
impl SeccompAdapter for NativeSeccompAdapter {
    fn supported(&self) -> bool {
        cfg!(target_os = "linux")
    }

    async fn install_current(&self, policy: &SeccompPolicy) -> AdapterResult<()> {
        #[cfg(target_os = "linux")]
        {
            install(policy)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = policy;
            Err(AdapterError::Unsupported(
                "seccomp requires Linux".into(),
            ))
        }
    }
}

/// 沙箱默认允许的系统调用清单（x86_64 / aarch64 都可用）。
///
/// 覆盖动态链接 C 程序的完整启动路径（ld.so + glibc）：含 robust list /
/// rseq / prlimit64 / 系统信息查询等；清单策略为「默认拒绝 + EPERM」，
/// 实际生产工作负载应按需收紧。
///
/// 架构差异：x86_64 遗留了一批旧式系统调用（`open`/`access`/`readlink`/
/// `pipe`/`dup2`/`time`/`arch_prctl`），但 aarch64 等走 asm-generic 表的架构
/// **没有这些**（glibc 改用 `openat`/`faccessat`/`readlinkat`/`pipe2`/`dup3`），
/// `libc::SYS_*` 里根本不存在它们，直接引用会编不过。故把它们收进
/// `#[cfg(target_arch = "x86_64")]` 一段——x86_64 行为不变，其他架构自然拿到
/// 通用子集。
#[cfg(target_os = "linux")]
pub fn default_allowlist() -> Vec<i64> {
    let mut allow: Vec<i64> = vec![
        // 文件 IO
        libc::SYS_read as i64,
        libc::SYS_write as i64,
        libc::SYS_openat as i64,
        libc::SYS_close as i64,
        libc::SYS_fstat as i64,
        libc::SYS_newfstatat as i64,
        libc::SYS_lseek as i64,
        libc::SYS_pread64 as i64,
        libc::SYS_pwrite64 as i64,
        libc::SYS_readv as i64,
        libc::SYS_writev as i64,
        libc::SYS_fcntl as i64,
        libc::SYS_ioctl as i64,
        libc::SYS_getdents64 as i64,
        libc::SYS_faccessat as i64,
        libc::SYS_faccessat2 as i64,
        libc::SYS_statx as i64,
        libc::SYS_readlinkat as i64,
        libc::SYS_statfs as i64,
        libc::SYS_fstatfs as i64,
        // 内存
        libc::SYS_mmap as i64,
        libc::SYS_mprotect as i64,
        libc::SYS_munmap as i64,
        libc::SYS_mremap as i64,
        libc::SYS_madvise as i64,
        libc::SYS_brk as i64,
        // 信号 / 线程
        libc::SYS_rt_sigaction as i64,
        libc::SYS_rt_sigprocmask as i64,
        libc::SYS_rt_sigreturn as i64,
        libc::SYS_sigaltstack as i64,
        libc::SYS_set_robust_list as i64,
        libc::SYS_get_robust_list as i64,
        libc::SYS_set_tid_address as i64,
        libc::SYS_rseq as i64,
        libc::SYS_futex as i64,
        libc::SYS_pipe2 as i64,
        libc::SYS_dup as i64,
        libc::SYS_dup3 as i64,
        // 时间
        libc::SYS_nanosleep as i64,
        libc::SYS_clock_nanosleep as i64,
        libc::SYS_clock_gettime as i64,
        libc::SYS_gettimeofday as i64,
        // 进程 / 系统
        libc::SYS_getpid as i64,
        libc::SYS_getppid as i64,
        libc::SYS_getuid as i64,
        libc::SYS_geteuid as i64,
        libc::SYS_getgid as i64,
        libc::SYS_getegid as i64,
        libc::SYS_gettid as i64,
        libc::SYS_uname as i64,
        libc::SYS_getrandom as i64,
        libc::SYS_sysinfo as i64,
        libc::SYS_sched_getaffinity as i64,
        libc::SYS_getrusage as i64,
        libc::SYS_prlimit64 as i64,
        libc::SYS_getrlimit as i64,
        libc::SYS_getcwd as i64,
        libc::SYS_chdir as i64,
        libc::SYS_prctl as i64,
        libc::SYS_membarrier as i64,
        libc::SYS_execve as i64,
        libc::SYS_exit as i64,
        libc::SYS_exit_group as i64,
        libc::SYS_wait4 as i64,
        libc::SYS_waitid as i64,
    ];
    // x86_64 遗留系统调用（见上：asm-generic 表没有，glibc 不用，仅 x86_64 保留）
    #[cfg(target_arch = "x86_64")]
    allow.extend([
        libc::SYS_open as i64,
        libc::SYS_access as i64,
        libc::SYS_readlink as i64,
        libc::SYS_pipe as i64,
        libc::SYS_dup2 as i64,
        libc::SYS_time as i64,
        libc::SYS_arch_prctl as i64,
    ]);
    allow
}

#[cfg(not(target_os = "linux"))]
pub fn default_allowlist() -> Vec<i64> {
    Vec::new()
}
