//! gamepulse.exe — GUI only, no console; the --once/--capture commands are in gamepulse-cli.exe
#![cfg_attr(windows, windows_subsystem = "windows")]
use std::process::ExitCode;

#[cfg(windows)]
fn show_error(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Gamepulse"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::{
            Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
            System::Threading::CreateMutexW,
        };
        if std::env::args().len() > 1 {
            show_error("gamepulse.exe only opens the GUI.\nUse gamepulse-cli.exe for --once, --capture or --capture-preview.");
            return ExitCode::from(3);
        }
        let instance = unsafe { CreateMutexW(None, false, w!("Local\\Gamepulse")) };
        if instance.is_ok() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // The running instance may be hidden in the tray: wake it to show its window instead of silently exiting
            gamepulse::gui::show_existing_instance();
            return ExitCode::SUCCESS;
        }
        let result = match gamepulse::gui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                show_error(&format!("Cannot start GUI: {error}"));
                ExitCode::from(3)
            }
        };
        if let Ok(handle) = instance {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
        result
    }
    #[cfg(not(windows))]
    {
        eprintln!("The GUI requires Windows 11 x64.");
        ExitCode::from(3)
    }
}
