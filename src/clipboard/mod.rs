//! Shared clipboard adapters: platform routing here, process execution in
//! `command`, terminal writes in `osc52`, PowerShell handoff in `windows`,
//! and the native Win32 write in the in-repo `ahi-win32` crate (Windows only).
//! Never interpolate selected text into a command or error.
mod command;
mod osc52;
mod windows;
pub(crate) mod wsl;

pub(crate) fn copy(text: &str) -> bool {
    execute(
        &plan(&Environment::detect()),
        text,
        &mut |step| match step {
            Step::Command(args) => command::copy_with_commands(text, &[args]),
            Step::WindowsHandoff { translate_path } => windows::copy(text, *translate_path),
            #[cfg(windows)]
            Step::WindowsNative => ahi_win32::copy(text),
            #[cfg(not(windows))]
            Step::WindowsNative => false,
            Step::Osc52 => osc52::emit(text),
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Windows,
    Linux,
    Other,
}

/// Everything the routing decision depends on, gathered once per copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Environment {
    pub(crate) platform: Platform,
    pub(crate) wayland: bool,
    pub(crate) x11: bool,
    pub(crate) termux: bool,
    pub(crate) wsl: bool,
    /// Windows Terminal, which supports OSC 52 from WSL and native shells.
    pub(crate) windows_terminal: bool,
    /// SSH/mosh session, or `AHI_CLIPBOARD_OSC52=1` for clients without markers.
    pub(crate) remote: bool,
}

impl Environment {
    pub(crate) fn detect() -> Self {
        let present = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        Self {
            platform: if cfg!(windows) {
                Platform::Windows
            } else if cfg!(target_os = "macos") {
                Platform::MacOs
            } else if cfg!(target_os = "linux") {
                Platform::Linux
            } else {
                Platform::Other
            },
            wayland: present("WAYLAND_DISPLAY"),
            x11: present("DISPLAY"),
            termux: present("TERMUX_VERSION"),
            wsl: crate::clipboard::wsl::is_wsl(),
            windows_terminal: present("WT_SESSION"),
            remote: present("SSH_CONNECTION")
                || present("SSH_CLIENT")
                || present("MOSH_CONNECTION")
                || std::env::var("AHI_CLIPBOARD_OSC52").is_ok_and(|value| value == "1"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A local helper that reads the text from stdin.
    Command(&'static [&'static str]),
    /// Win32 `SetClipboardData` with `CF_UNICODETEXT`; native Windows only.
    WindowsNative,
    /// PowerShell `Set-Clipboard` reading a private file; `translate_path` under WSL.
    WindowsHandoff { translate_path: bool },
    /// Base64 escape sequence to the terminal; unverifiable.
    Osc52,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Tried in order; the first success ends the search.
    pub(crate) attempts: Vec<Step>,
    /// Remote sessions also send OSC 52 so the client terminal receives the
    /// text even when a local writer on the server succeeded.
    pub(crate) always_osc52: bool,
}

pub(crate) fn plan(env: &Environment) -> Plan {
    let mut attempts = Vec::new();
    match env.platform {
        Platform::Windows => {
            // The API write is instant; PowerShell covers a clipboard another
            // process keeps open longer than the retry window.
            attempts.push(Step::WindowsNative);
            attempts.push(Step::WindowsHandoff {
                translate_path: false,
            });
        }
        Platform::MacOs => attempts.push(Step::Command(&["pbcopy"])),
        Platform::Linux | Platform::Other => {
            if env.termux {
                attempts.push(Step::Command(&["termux-clipboard-set"]));
            }
            if env.wayland {
                attempts.push(Step::Command(&["wl-copy"]));
            }
            if env.x11 {
                attempts.push(Step::Command(&["xclip", "-selection", "clipboard"]));
                attempts.push(Step::Command(&["xsel", "--clipboard", "--input"]));
            }
            if env.wsl {
                // Avoid starting PowerShell when the terminal can take OSC 52.
                attempts.push(if env.windows_terminal {
                    Step::Osc52
                } else {
                    Step::WindowsHandoff {
                        translate_path: true,
                    }
                });
            }
            let headless = !env.wayland && !env.x11 && !env.termux;
            if headless && attempts.last() != Some(&Step::Osc52) {
                attempts.push(Step::Osc52);
            }
        }
    }
    Plan {
        attempts,
        always_osc52: env.remote,
    }
}

/// Runs a plan. Local writes finish before OSC 52 so they cannot race the terminal.
pub(crate) fn execute(plan: &Plan, _text: &str, run: &mut impl FnMut(&Step) -> bool) -> bool {
    let mut copied = false;
    let mut osc52_emitted = false;
    for step in &plan.attempts {
        if run(step) {
            copied = true;
            osc52_emitted = *step == Step::Osc52;
            break;
        }
    }
    if plan.always_osc52 && !osc52_emitted {
        // Preserve any successful local write.
        copied = run(&Step::Osc52) || copied;
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Environment {
        Environment {
            platform: Platform::Linux,
            wayland: false,
            x11: false,
            termux: false,
            wsl: false,
            windows_terminal: false,
            remote: false,
        }
    }

    fn steps(env: &Environment) -> Vec<Step> {
        plan(env).attempts
    }

    #[test]
    fn macos_and_windows_use_native_writers_then_osc52_only_when_remote() {
        let mac = Environment {
            platform: Platform::MacOs,
            ..linux()
        };
        assert_eq!(plan(&mac).attempts, [Step::Command(&["pbcopy"])]);
        assert!(!plan(&mac).always_osc52);
        let windows = Environment {
            platform: Platform::Windows,
            windows_terminal: true,
            remote: true,
            ..linux()
        };
        assert_eq!(
            plan(&windows),
            Plan {
                attempts: vec![
                    Step::WindowsNative,
                    Step::WindowsHandoff {
                        translate_path: false
                    },
                ],
                always_osc52: true,
            }
        );
    }

    #[test]
    fn native_linux_prefers_display_tools_and_falls_back_to_osc52_only_headless() {
        assert_eq!(steps(&linux()), [Step::Osc52]);
        let wayland = Environment {
            wayland: true,
            x11: true,
            ..linux()
        };
        assert_eq!(
            steps(&wayland),
            [
                Step::Command(&["wl-copy"]),
                Step::Command(&["xclip", "-selection", "clipboard"]),
                Step::Command(&["xsel", "--clipboard", "--input"]),
            ]
        );
        let termux = Environment {
            termux: true,
            ..linux()
        };
        assert_eq!(steps(&termux), [Step::Command(&["termux-clipboard-set"])]);
    }

    #[test]
    fn wsl_routes_by_terminal_after_display_tools() {
        let windows_terminal = Environment {
            wsl: true,
            windows_terminal: true,
            ..linux()
        };
        assert_eq!(steps(&windows_terminal), [Step::Osc52]);
        let other_terminal = Environment {
            wsl: true,
            ..linux()
        };
        assert_eq!(
            steps(&other_terminal),
            [
                Step::WindowsHandoff {
                    translate_path: true
                },
                Step::Osc52,
            ]
        );
        let wslg = Environment {
            wsl: true,
            wayland: true,
            ..linux()
        };
        assert_eq!(
            steps(&wslg),
            [
                Step::Command(&["wl-copy"]),
                Step::WindowsHandoff {
                    translate_path: true
                },
            ]
        );
    }

    #[test]
    fn execution_stops_at_first_success_and_remote_adds_one_osc52() {
        let plan = Plan {
            attempts: vec![
                Step::Command(&["wl-copy"]),
                Step::Command(&["xclip", "-selection", "clipboard"]),
            ],
            always_osc52: true,
        };
        let mut ran = Vec::new();
        let mut run = |step: &Step| {
            ran.push(step.clone());
            *step != Step::Command(&["wl-copy"])
        };
        assert!(execute(&plan, "", &mut run));
        assert_eq!(
            ran,
            [
                Step::Command(&["wl-copy"]),
                Step::Command(&["xclip", "-selection", "clipboard"]),
                Step::Osc52,
            ]
        );
        let osc52_only = Plan {
            attempts: vec![Step::Osc52],
            always_osc52: true,
        };
        let mut count = 0;
        assert!(execute(&osc52_only, "", &mut |_| {
            count += 1;
            true
        }));
        assert_eq!(count, 1);
        assert!(!execute(&plan, "", &mut |_| false));
    }
}
