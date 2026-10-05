use anyhow::Result;
use std::{
    cell::Cell,
    panic::{self, AssertUnwindSafe},
    sync::Once,
};

thread_local! {
    static GUARDED: Cell<usize> = const { Cell::new(0) };
}

static QUIET_HOOK: Once = Once::new();

/// Runs one plugin call and turns a panic into an error, so a hostile input that trips a decoder
/// is skipped like any other failing input instead of aborting the run. The default panic message
/// is suppressed while a guarded call runs because stderr may carry NDJSON events.
pub fn catch_panic<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    QUIET_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if GUARDED.with(Cell::get) == 0 {
                previous(info);
            }
        }));
    });
    GUARDED.with(|g| g.set(g.get() + 1));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    GUARDED.with(|g| g.set(g.get() - 1));
    result.unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("unknown panic");
        Err(anyhow::anyhow!("panicked: {message}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_becomes_error_with_its_message() {
        let err = catch_panic::<()>(|| panic!("decoder exploded")).unwrap_err();
        assert_eq!(err.to_string(), "panicked: decoder exploded");
        let formatted = catch_panic::<()>(|| panic!("frame {}", 7)).unwrap_err();
        assert_eq!(formatted.to_string(), "panicked: frame 7");
    }

    #[test]
    fn values_and_errors_pass_through() {
        assert_eq!(catch_panic(|| Ok(3)).unwrap(), 3);
        let err = catch_panic::<()>(|| anyhow::bail!("plain")).unwrap_err();
        assert_eq!(err.to_string(), "plain");
    }
}
