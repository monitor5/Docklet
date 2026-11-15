//! The confirmation dialog and the result message, as task dialogs: stock,
//! keyboard-driven and read by screen readers. Their text comes from outside.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON, TD_INFORMATION_ICON,
    TD_WARNING_ICON, TDCBF_CANCEL_BUTTON, TDCBF_OK_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TaskDialogIndirect,
};
use windows::Win32::UI::WindowsAndMessaging::IDCANCEL;
use windows::core::PCWSTR;

use crate::boundary::{Confirm, Notice, Tone};
use crate::win::{instance, wide};

const ACCEPT: i32 = 1000;

fn base(owner: Option<HWND>, title: &[u16]) -> TASKDIALOGCONFIG {
    TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner.unwrap_or_default(),
        hInstance: instance(),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        ..Default::default()
    }
}

/// Asks `confirm`'s question. True when the user chose the accept button;
/// Cancel is the default, so Enter alone removes nothing.
pub fn confirm(owner: Option<HWND>, confirm: &Confirm) -> bool {
    let window = wide("Docklet");
    let title = wide(&confirm.title);
    let message = wide(&confirm.message);
    let accept = wide(&confirm.accept);
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: ACCEPT,
        pszButtonText: PCWSTR(accept.as_ptr()),
    }];
    let config = TASKDIALOGCONFIG {
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_WARNING_ICON,
        },
        pszMainInstruction: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(message.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: IDCANCEL.0,
        ..base(owner, &window)
    };
    let mut pressed = 0;
    // SAFETY: every string and the button array outlive the call, which
    // runs a modal loop and returns.
    let shown = unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) };
    shown.is_ok() && pressed == ACCEPT
}

/// Shows a result. Returns when the user closes it.
pub fn notice(owner: Option<HWND>, notice: &Notice) {
    let window = wide("Docklet");
    let title = wide(&notice.title);
    let message = wide(&notice.message);
    let detail = notice.detail.as_deref().map(wide);
    let icon = match notice.tone {
        Tone::Info => TD_INFORMATION_ICON,
        Tone::Warning => TD_WARNING_ICON,
        Tone::Error => TD_ERROR_ICON,
    };
    let config = TASKDIALOGCONFIG {
        dwCommonButtons: TDCBF_OK_BUTTON,
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon },
        pszMainInstruction: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(message.as_ptr()),
        pszExpandedInformation: detail
            .as_ref()
            .map_or(PCWSTR::null(), |detail| PCWSTR(detail.as_ptr())),
        ..base(owner, &window)
    };
    // SAFETY: as in `confirm`.
    unsafe {
        let _ = TaskDialogIndirect(&config, None, None, None);
    }
}
