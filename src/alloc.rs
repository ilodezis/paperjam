//! Allocator settings that keep the resident size close to what is in use.
//!
//! A page in the pipeline needs a few hundred megabytes for a second, then
//! nothing. Two defaults kept that memory resident for hours afterwards:
//!
//! - mimalloc returns freed memory after a delay, and only when the thread
//!   that freed it allocates again. The blocking thread that decoded a page
//!   goes idle right after, so the memory stayed resident until a restart.
//! - With transparent huge pages set to `always` (common on Linux servers),
//!   the kernel collapses the ranges mimalloc returned back into 2 MiB pages
//!   as soon as one 4 KiB page in them is in use, refilling them with zeros.

/// `mi_option_purge_delay` in mimalloc.h (milliseconds; 0 returns freed
/// memory at once). The Rust bindings do not export it.
const PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;

/// Applies the settings. Called first thing in `main`; the huge-page flag is
/// inherited by child processes (Tesseract).
pub fn tune() {
    // SAFETY: setting an option has no preconditions; mimalloc reads it on
    // every purge decision.
    unsafe { libmimalloc_sys::mi_option_set(PURGE_DELAY, 0) };
    #[cfg(target_os = "linux")]
    // SAFETY: PR_SET_THP_DISABLE takes plain integers and only sets a flag
    // on this process; a kernel without it returns an error, which is fine.
    unsafe {
        libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn freed_memory_is_returned_at_once() {
        super::tune();
        // SAFETY: reading an option has no preconditions.
        let delay = unsafe { libmimalloc_sys::mi_option_get(super::PURGE_DELAY) };
        assert_eq!(delay, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn huge_pages_are_off_for_the_process() {
        super::tune();
        // SAFETY: PR_GET_THP_DISABLE only reads the flag.
        let disabled = unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) };
        assert_eq!(disabled, 1);
    }
}
