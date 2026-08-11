//! BCM2712 HEVC decode block — bare-metal bring-up probe.
//!
//! The first bare-metal contact with the hardware HEVC decoder. It reads the
//! block's version register over the mediated `MMIO_READ32` syscall and logs
//! the value. That single read answers the two questions that gate everything
//! downstream:
//!
//! 1. **Is the block's MMIO reachable from bare metal?** The read runs in
//!    kernel context (the syscall), so a clean return means the peripheral
//!    aperture covers the block. A fault means it does not.
//! 2. **Is the block clocked at handoff?** The Linux driver enables a firmware
//!    clock; but the VPU firmware leaves other blocks (the GEM NIC) clocked and
//!    out of reset before the ARM cores boot. If this reads the expected
//!    `0x202` with no clock setup, the block is already clocked and the
//!    bare-metal path is open. `0x0` / `0xFFFFFFFF` means it is gated off and
//!    the clock must be enabled first.
//!
//! Addresses and the expected version are from the released clean-room spec
//! (r01/r02 §2, §6.1). Reads only — no decode, DMA, or interrupts.

#![no_std]
#![allow(
    dead_code,
    unused_imports,
    unreachable_patterns,
    reason = "PIC build path-mounts the SDK via include!/mod; each module's compile sees the full ABI surface and uses a subset"
)]

use core::ffi::c_void;

#[path = "../../../target/fluxor/fluxor-abi/sdk/abi.rs"]
mod abi;
use abi::SyscallTable;

include!("../../../target/fluxor/fluxor-abi/sdk/runtime.rs");

/// `mmio_dma` opcodes — mediated register + DMA operations performed in kernel
/// context [Fluxor SDK `platform/bcm2712/mmio_dma.rs`]. MMIO_READ/WRITE32 read
/// and write any 32-bit location (register or DRAM) from the kernel, so the
/// probe touches DMA buffers without depending on the module's own page table.
const MMIO_READ32: u32 = 0x0CE4;
const MMIO_WRITE32: u32 = 0x0CE5;
const DMA_ALLOC_CONTIG: u32 = 0x0CE6;
const DMA_FLUSH: u32 = 0x0CEA;
const DMA_INVALIDATE: u32 = 0x0CEB;

/// HEVC main register block base [spec r01/r02 §2].
const HEVC_MAIN_BASE: u64 = 0x10_0080_0000;
/// Version register offset within the main block [spec r01/r02 §6.1].
const P1_VERSION: u64 = 0x3C;
/// Expected value on supported silicon [spec r01/r02 §2, §6.1].
const HW_VERSION: u32 = 0x202;

/// A distinctive pattern written to and read back from a DMA buffer, to prove
/// the allocation is CPU-accessible and the cache maintenance round-trips.
const DMA_PATTERN: u32 = 0xC0DE_2712;

/// Log the version roughly once a second at a 1 ms tick, so the reading is a
/// steady-state recurring line the UDP monitor is guaranteed to observe
/// (standards/rig.md §5) rather than a one-shot boot line it attaches too late
/// to see.
const LOG_EVERY: u32 = 1000;

#[repr(C)]
struct ProbeState {
    syscalls: *const SyscallTable,
    out_chan: i32,
    steps: u32,
    /// DMA test outcome, computed once on the first step. `dma_done` guards it
    /// so the (allocating) test runs exactly once.
    dma_done: bool,
    dma_phys: u64,
    dma_readback: u32,
    dma_ok: bool,
}

/// Mediated 32-bit MMIO read. Returns 0 on syscall error, matching how the
/// SMMU module reports an inaccessible register.
unsafe fn mmio_read32(sys: &SyscallTable, addr: u64) -> u32 {
    let mut buf = [0u8; 12];
    let ab = addr.to_le_bytes();
    buf[..8].copy_from_slice(&ab);
    let rc = (sys.provider_call)(-1, MMIO_READ32, buf.as_mut_ptr(), 12);
    if rc < 0 {
        return 0;
    }
    u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]])
}

/// Mediated 32-bit write (register or DRAM), performed in kernel context.
unsafe fn mmio_write32(sys: &SyscallTable, addr: u64, val: u32) {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    buf[8..12].copy_from_slice(&val.to_le_bytes());
    (sys.provider_call)(-1, MMIO_WRITE32, buf.as_mut_ptr(), 12);
}

/// Allocate a contiguous DMA buffer. Returns its physical address, or 0 on
/// failure. Arg layout: `[size:u32][align:u32][phys:u64 out]`.
unsafe fn dma_alloc_contig(sys: &SyscallTable, size: u32, align: u32) -> u64 {
    let mut buf = [0u8; 16];
    buf[..4].copy_from_slice(&size.to_le_bytes());
    buf[4..8].copy_from_slice(&align.to_le_bytes());
    let rc = (sys.provider_call)(-1, DMA_ALLOC_CONTIG, buf.as_mut_ptr(), 16);
    if rc < 0 {
        return 0;
    }
    u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ])
}

/// Clean a DRAM range to the point of coherency before a device reads it, or
/// invalidate it before the CPU reads what a device wrote. Arg: `[addr:u64][size:u32]`.
unsafe fn dma_cache_op(sys: &SyscallTable, op: u32, addr: u64, size: u32) {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    buf[8..12].copy_from_slice(&size.to_le_bytes());
    (sys.provider_call)(-1, op, buf.as_mut_ptr(), 12);
}

/// Allocate a DMA buffer and round-trip a pattern through it — CPU write,
/// clean, invalidate, CPU read-back — proving the allocator returns usable,
/// cache-coherent memory. Whether the HEVC block itself can *reach* this
/// address is a separate question, answered only when the block DMAs during a
/// real decode; this rung validates the allocator and the cache-maintenance
/// path the driver will depend on.
unsafe fn run_dma_test(s: &mut ProbeState) {
    let sys = &*s.syscalls;
    let phys = dma_alloc_contig(sys, 4096, 64);
    s.dma_phys = phys;
    if phys == 0 {
        s.dma_ok = false;
        s.dma_done = true;
        return;
    }
    mmio_write32(sys, phys, DMA_PATTERN);
    dma_cache_op(sys, DMA_FLUSH, phys, 4);
    dma_cache_op(sys, DMA_INVALIDATE, phys, 4);
    let rb = mmio_read32(sys, phys);
    s.dma_readback = rb;
    s.dma_ok = rb == DMA_PATTERN;
    s.dma_done = true;
}

/// Write `[hevc] ver=0xXXXXXXXX ok=<0|1>` into `out` and return its length.
/// Hand-rolled hex — a PIC module has no `core::fmt` allocator path worth
/// pulling in for one line.
fn format_line(out: &mut [u8; 32], value: u32) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b"[hevc] ver=0x";
    let mut n = 0;
    for &b in prefix {
        out[n] = b;
        n += 1;
    }
    for shift in (0..8).rev() {
        out[n] = HEX[((value >> (shift * 4)) & 0xF) as usize];
        n += 1;
    }
    let tail: &[u8] = if value == HW_VERSION {
        b" ok=1"
    } else {
        b" ok=0"
    };
    for &b in tail {
        out[n] = b;
        n += 1;
    }
    n
}

#[no_mangle]
#[link_section = ".text.module_state_size"]
pub extern "C" fn module_state_size() -> u32 {
    core::mem::size_of::<ProbeState>() as u32
}

/// # Safety
///
/// A module ABI entry point: the kernel's loader is the only caller, and it
/// passes the syscall table it built for this instance. Not callable from Rust.
#[no_mangle]
#[link_section = ".text.module_init"]
pub unsafe extern "C" fn module_init(_syscalls: *const c_void) {}

#[no_mangle]
#[link_section = ".text.module_new"]
pub extern "C" fn module_new(
    _in_chan: i32,
    out_chan: i32,
    _ctrl_chan: i32,
    _params: *const u8,
    _params_len: usize,
    state: *mut u8,
    state_size: usize,
    syscalls: *const c_void,
) -> i32 {
    unsafe {
        if syscalls.is_null() || state.is_null() {
            return -1;
        }
        if state_size < core::mem::size_of::<ProbeState>() {
            return -2;
        }
        let s = &mut *(state as *mut ProbeState);
        s.syscalls = syscalls as *const SyscallTable;
        s.out_chan = out_chan;
        s.steps = 0;
        s.dma_done = false;
        s.dma_phys = 0;
        s.dma_readback = 0;
        s.dma_ok = false;
        0
    }
}

/// Write `[hevc] dma phys=0x............ rb=0xXXXXXXXX ok=<0|1>` into `out`.
fn format_dma(out: &mut [u8; 64], phys: u64, readback: u32, ok: bool) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut n = 0;
    let emit = |bytes: &[u8], out: &mut [u8; 64], n: &mut usize| {
        for &b in bytes {
            out[*n] = b;
            *n += 1;
        }
    };
    emit(b"[hevc] dma phys=0x", out, &mut n);
    for shift in (0..16).rev() {
        out[n] = HEX[((phys >> (shift * 4)) & 0xF) as usize];
        n += 1;
    }
    emit(b" rb=0x", out, &mut n);
    for shift in (0..8).rev() {
        out[n] = HEX[((readback >> (shift * 4)) & 0xF) as usize];
        n += 1;
    }
    emit(if ok { b" ok=1" } else { b" ok=0" }, out, &mut n);
    n
}

#[no_mangle]
#[link_section = ".text.module_step"]
pub extern "C" fn module_step(state: *mut u8) -> i32 {
    unsafe {
        if state.is_null() {
            return -1;
        }
        let s = &mut *(state as *mut ProbeState);
        s.steps = s.steps.wrapping_add(1);
        if s.syscalls.is_null() {
            return 0;
        }
        // The DMA round-trip allocates, so it runs exactly once, on the first
        // step, before any logging depends on it.
        if !s.dma_done {
            run_dma_test(s);
        }
        // Log on the first step (so a fast pass sees it) and then periodically,
        // as steady-state recurring lines the UDP monitor is sure to observe.
        if s.steps == 1 || s.steps.is_multiple_of(LOG_EVERY) {
            let sys = &*s.syscalls;
            let value = mmio_read32(sys, HEVC_MAIN_BASE + P1_VERSION);
            let mut line = [0u8; 32];
            let len = format_line(&mut line, value);
            dev_log(sys, 3, line.as_ptr(), len);

            let mut dma_line = [0u8; 64];
            let dlen = format_dma(&mut dma_line, s.dma_phys, s.dma_readback, s.dma_ok);
            dev_log(sys, 3, dma_line.as_ptr(), dlen);
        }
        0
    }
}

include!("../../../target/fluxor/fluxor-abi/sdk/runtime/wasm_entry.rs");
