//! BCM2712 HEVC decode block — bare-metal bring-up probe.
//!
//! The first bare-metal contact with the hardware HEVC decoder. It reads the
//! block's version register over the mediated `MMIO_READ32` call and logs
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

use core::ffi::c_void;

use abi::SyscallTable;
use fluxor_abi as abi;

use fluxor_rt::*;

use abi::contracts::hal::metal_device as md;

/// HEVC main register block base [spec r01/r02 §2].
const HEVC_MAIN_BASE: u64 = 0x10_0080_0000;
/// Version register offset within the main block [spec r01/r02 §6.1].
const P1_VERSION: u32 = 0x3C;
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
    /// The device aperture holding the HEVC block, and the block's offset in
    /// it; `regs` is negative until the aperture is open.
    regs: i32,
    regs_off: u32,
    dma_phys: u64,
    dma_readback: u32,
    dma_ok: bool,
}

/// Open the device aperture that holds `addr`: its handle and `addr`'s offset
/// in it, or `None` when no aperture of the machine covers it.
unsafe fn open_aperture_at(sys: &SyscallTable, addr: u64) -> Option<(i32, u32)> {
    let mut index = 0u8;
    loop {
        let mut arg = [0u8; md::mmio::OPEN_LEN];
        arg[0] = index;
        let h = (sys.provider_call)(
            -1,
            md::MMIO_OPEN,
            arg.as_mut_ptr(),
            arg.len(),
            core::ptr::null_mut(),
            0,
        );
        if h < 0 {
            return None;
        }
        let base = u64::from_le_bytes([
            arg[0], arg[1], arg[2], arg[3], arg[4], arg[5], arg[6], arg[7],
        ]);
        let len = u32::from_le_bytes([arg[8], arg[9], arg[10], arg[11]]);
        if addr >= base && addr - base < u64::from(len) {
            return Some((h, (addr - base) as u32));
        }
        index = index.checked_add(1)?;
    }
}

/// Mediated 32-bit register read at `offset` in aperture `regs`. Returns 0 on
/// error, matching how the SMMU module reports an inaccessible register.
unsafe fn mmio_read32(sys: &SyscallTable, regs: i32, offset: u32) -> u32 {
    let mut arg = md::mmio::arg(offset, 0);
    let rc = (sys.provider_call)(
        regs,
        md::MMIO_READ32,
        arg.as_mut_ptr(),
        arg.len(),
        core::ptr::null_mut(),
        0,
    );
    if rc < 0 {
        return 0;
    }
    u32::from_le_bytes([arg[4], arg[5], arg[6], arg[7]])
}

/// Allocate a DMA buffer and round-trip a pattern through it — CPU write,
/// clean, invalidate, CPU read-back — proving the allocator returns usable,
/// cache-coherent memory. Whether the HEVC block itself can *reach* this
/// address is a separate question, answered only when the block DMAs during a
/// real decode; this rung validates the allocator and the cache-maintenance
/// path the driver will depend on.
unsafe fn run_dma_test(s: &mut ProbeState) {
    let sys = &*s.syscalls;
    let Some(region) = dev_dma_alloc(sys, 4096, 64) else {
        s.dma_phys = 0;
        s.dma_ok = false;
        s.dma_done = true;
        return;
    };
    s.dma_phys = region.addr;
    let word = region.addr as usize as *mut u32;
    core::ptr::write_volatile(word, DMA_PATTERN);
    // Cache maintenance runs by whole lines: the invalidate needs a 64-byte
    // aligned range.
    dev_dma_flush(sys, region.handle, 0, 64);
    dev_dma_invalidate(sys, region.handle, 0, 64);
    let rb = core::ptr::read_volatile(word);
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

fluxor_rt::module_facts! { state: ProbeState }

#[no_mangle]
#[link_section = ".text.module_init"]
pub extern "C" fn module_init(_syscalls: *const c_void) {}

/// # Safety
/// Kernel module-ABI entry point: `state` is the loader-owned instance block
/// of `state_size` bytes, `syscalls` the kernel's syscall table, and `params`
/// the config TLV blob of `params_len` bytes (may be null when it is empty).
#[no_mangle]
#[link_section = ".text.module_new"]
pub unsafe extern "C" fn module_new(
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
        s.regs = -1;
        s.regs_off = 0;
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
pub extern "C" fn module_step(state: *mut u8) -> StepReturn {
    unsafe {
        if state.is_null() {
            return StepReturn::error(-1);
        }
        let s = &mut *(state as *mut ProbeState);
        s.steps = s.steps.wrapping_add(1);
        if s.syscalls.is_null() {
            return StepReturn::Continue;
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
            if s.regs < 0 {
                if let Some((regs, off)) = open_aperture_at(sys, HEVC_MAIN_BASE) {
                    s.regs = regs;
                    s.regs_off = off;
                }
            }
            let value = if s.regs < 0 {
                0
            } else {
                mmio_read32(sys, s.regs, s.regs_off + P1_VERSION)
            };
            let mut line = [0u8; 32];
            let len = format_line(&mut line, value);
            dev_log(sys, 3, line.as_ptr(), len);

            let mut dma_line = [0u8; 64];
            let dlen = format_dma(&mut dma_line, s.dma_phys, s.dma_readback, s.dma_ok);
            dev_log(sys, 3, dma_line.as_ptr(), dlen);
        }
        StepReturn::Continue
    }
}
