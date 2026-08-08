//! The intentionally small C ABI used by Git's native diff-pretty adapter.
//!
//! Git owns process state and emits its existing diff-symbol stream. This
//! crate owns only the renderer, retained document, and terminal session.
//! Nothing in the ABI exposes a Rust allocation or a Git-private type.

use diff_pretty::RenderSession;
use diff_pretty::event::DiffEvent;
use scrl::{ExitReason, PagingMode, RunOptions, SessionOptions};
use std::ffi::{CString, c_char, c_uchar};
use std::ptr;

const ABI_VERSION: u32 = 1;
const STATUS_OK: i32 = 0;
const STATUS_QUIT: i32 = 1;
const STATUS_ERROR: i32 = -1;
const STATUS_INVALID: i32 = -2;
const PAGING_AUTO: u32 = 0;
const PAGING_ALWAYS: u32 = 1;
const PAGING_NEVER: u32 = 2;

#[repr(C)]
pub struct DiffPrettyConfig {
    pub version: u32,
    pub size: u32,
    pub paging: u32,
    pub output_fd: i32,
    pub tty_fd: i32,
}

struct NativeSession {
    renderer: Option<RenderSession>,
    document: Option<scrl::Document>,
    options: RunOptions,
    output_fd: i32,
    tty_fd: i32,
    error: Option<CString>,
    paged: bool,
}

pub struct DiffPrettySession {
    inner: NativeSession,
}

fn set_error(session: &mut NativeSession, error: impl std::fmt::Display) -> i32 {
    let message = error.to_string().replace('\0', "?");
    session.error = CString::new(message).ok();
    STATUS_ERROR
}

fn config_size_is_valid(config: &DiffPrettyConfig) -> bool {
    config.size as usize >= std::mem::size_of::<DiffPrettyConfig>()
}

fn paging_mode(value: u32) -> Option<PagingMode> {
    match value {
        PAGING_AUTO => Some(PagingMode::Auto),
        PAGING_ALWAYS => Some(PagingMode::Always),
        PAGING_NEVER => Some(PagingMode::Never),
        _ => None,
    }
}

fn session_mut<'a>(session: *mut DiffPrettySession) -> Result<&'a mut NativeSession, i32> {
    if session.is_null() {
        return Err(STATUS_INVALID);
    }
    // SAFETY: callers receive this pointer only from diff_pretty_begin and
    // must not use it after diff_pretty_abort.
    Ok(unsafe { &mut (*session).inner })
}

fn bytes<'a>(data: *const c_uchar, len: usize) -> Result<&'a [u8], i32> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(STATUS_INVALID);
    }
    // SAFETY: the caller promises that `data` points to `len` readable bytes
    // for the duration of the call, as specified by the C header.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}

fn finish_session(session: &mut NativeSession) -> i32 {
    if session.document.is_some() {
        return STATUS_OK;
    }
    let Some(renderer) = session.renderer.take() else {
        return set_error(session, "renderer is unavailable");
    };
    session.document = Some(renderer.finish());
    STATUS_OK
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_begin(
    config: *const DiffPrettyConfig,
) -> *mut DiffPrettySession {
    if config.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the null check above establishes that the config pointer is
    // readable for the fixed ABI prefix used here.
    let config = unsafe { &*config };
    if config.version != ABI_VERSION || !config_size_is_valid(config) {
        return ptr::null_mut();
    }
    let Some(paging) = paging_mode(config.paging) else {
        return ptr::null_mut();
    };
    if config.output_fd < 0 {
        return ptr::null_mut();
    }
    let inner = NativeSession {
        renderer: Some(RenderSession::new()),
        document: None,
        options: RunOptions {
            paging,
            session: SessionOptions {
                title: "diff-pretty".into(),
                search_history: Vec::new(),
                wrap: false,
                follow: false,
                filter: None,
            },
        },
        output_fd: config.output_fd,
        tty_fd: config.tty_fd,
        error: None,
        paged: false,
    };
    Box::into_raw(Box::new(DiffPrettySession { inner }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_push_patch(
    session: *mut DiffPrettySession,
    data: *const c_uchar,
    len: usize,
) -> i32 {
    let Ok(session) = session_mut(session) else {
        return STATUS_INVALID;
    };
    let Ok(data) = bytes(data, len) else {
        return set_error(session, "invalid patch buffer");
    };
    let text = match std::str::from_utf8(data) {
        Ok(text) => text,
        Err(error) => return set_error(session, error),
    };
    let Some(renderer) = session.renderer.as_mut() else {
        return set_error(session, "patch submitted after diff_pretty_finish");
    };
    match renderer.push_patch(text) {
        Ok(()) => STATUS_OK,
        Err(error) => set_error(session, error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_push_event(
    session: *mut DiffPrettySession,
    kind: u32,
    flags: u32,
    data: *const c_uchar,
    len: usize,
) -> i32 {
    let Ok(session) = session_mut(session) else {
        return STATUS_INVALID;
    };
    let Ok(data) = bytes(data, len) else {
        return set_error(session, "invalid event buffer");
    };
    let Some(renderer) = session.renderer.as_mut() else {
        return set_error(session, "event submitted after diff_pretty_finish");
    };
    match renderer.push_event(DiffEvent::new(kind, flags, data)) {
        Ok(()) => STATUS_OK,
        Err(error) => set_error(session, error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_finish(session: *mut DiffPrettySession) -> i32 {
    let Ok(session) = session_mut(session) else {
        return STATUS_INVALID;
    };
    finish_session(session)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_page(session: *mut DiffPrettySession) -> i32 {
    let Ok(session) = session_mut(session) else {
        return STATUS_INVALID;
    };
    if session.paged {
        return STATUS_OK;
    }
    if session.document.is_none() && finish_session(session) < STATUS_OK {
        return STATUS_ERROR;
    }
    let Some(document) = session.document.as_ref() else {
        return set_error(session, "document is unavailable");
    };
    #[cfg(unix)]
    let result = scrl::run_document_with_fds(
        document,
        session.options.clone(),
        session.output_fd,
        session.tty_fd,
    );
    #[cfg(not(unix))]
    let result = scrl::run_document(document, session.options.clone());
    match result {
        Ok(ExitReason::EndOfInput) => {
            session.paged = true;
            STATUS_OK
        }
        Ok(ExitReason::Quit) => {
            session.paged = true;
            STATUS_QUIT
        }
        Err(error) => set_error(session, error),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_last_error(
    session: *const DiffPrettySession,
) -> *const c_char {
    if session.is_null() {
        return ptr::null();
    }
    // SAFETY: the pointer is checked above and remains valid until abort.
    let session = unsafe { &(*session).inner };
    session
        .error
        .as_ref()
        .map_or(ptr::null(), |message| message.as_ptr())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn diff_pretty_abort(session: *mut DiffPrettySession) {
    if !session.is_null() {
        // SAFETY: ownership is transferred back exactly once by the C caller.
        drop(unsafe { Box::from_raw(session) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diff_pretty::event;

    #[test]
    fn semantic_events_can_be_finished_without_serializing_a_document() {
        let config = DiffPrettyConfig {
            version: ABI_VERSION,
            size: std::mem::size_of::<DiffPrettyConfig>() as u32,
            paging: PAGING_NEVER,
            output_fd: 1,
            tty_fd: -1,
        };
        let session = unsafe { diff_pretty_begin(&config) };
        assert!(!session.is_null());
        let header = b"diff --git a/a b/a\n";
        assert_eq!(
            unsafe {
                diff_pretty_push_event(session, event::HEADER, 0, header.as_ptr(), header.len())
            },
            STATUS_OK
        );
        assert_eq!(unsafe { diff_pretty_finish(session) }, STATUS_OK);
        unsafe { diff_pretty_abort(session) };
    }
}
