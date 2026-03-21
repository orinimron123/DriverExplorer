use super::state::{AppState, GuiAction, LoadingState, StatusKind};
use super::ui;
use crate::drivers;
use crate::export::{ExportFormat, ExportOptions};
use crate::services;
// use std::io::Cursor;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use tiny_http::{Header, Response, Server};

pub struct DriverExplorerApp {
    state: AppState,
    is_elevated: bool,
}

impl DriverExplorerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            state: AppState::new(),
            is_elevated: Self::is_running_as_admin(),
        };

        // Load drivers on startup
        app.start_loading();

        app
    }

    /// Start background thread to enumerate drivers
    fn start_loading(&mut self) {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            if let Ok(drivers) = drivers::enumerate_all() {
                let _ = tx.send(drivers);
            }
        });

        self.state.set_receiver(rx);
    }

    /// Handle load driver button click - opens Driver Manager with file browser
    fn handle_load_driver(&mut self) {
        self.state.show_driver_manager = true;
        self.handle_dm_browse();
    }

    /// Handle unload driver button click
    fn handle_unload_driver(&mut self) {
        if let Some(driver) = self.state.selected() {
            let display_name = driver.name.clone();
            let svc_name = match driver.service_name.clone() {
                Some(s) => s,
                None => {
                    self.state.set_status(
                        format!("No service name found for '{}'", display_name),
                        StatusKind::Error,
                    );
                    return;
                }
            };
            // Stop first, then unregister
            let _ = services::stop_driver(&svc_name);
            match services::unregister_driver(&svc_name) {
                Ok(()) => {
                    self.state.set_status(
                        format!("Driver '{}' unloaded", display_name),
                        StatusKind::Success,
                    );
                    self.start_loading();
                }
                Err(e) => {
                    self.state.set_status(
                        format!("Failed to unload '{}': {}", display_name, e),
                        StatusKind::Error,
                    );
                }
            }
        }
    }

    /// Resolve driver filename to SCM service name
    fn resolve_service_name(&self, driver_filename: &str) -> Option<String> {
        self.state
            .drivers
            .iter()
            .find(|d| d.name == driver_filename)
            .and_then(|d| d.service_name.clone())
    }

    /// Handle start driver
    fn handle_start_driver(&mut self, name: &str) {
        let svc_name = match self.resolve_service_name(name) {
            Some(s) => s,
            None => {
                self.state.set_status(
                    format!("No service name found for '{}'", name),
                    StatusKind::Error,
                );
                return;
            }
        };
        match services::start_driver(&svc_name) {
            Ok(()) => {
                self.state
                    .set_status(format!("Driver '{}' started", name), StatusKind::Success);
                self.start_loading();
            }
            Err(e) => {
                self.state.set_status(
                    format!("Failed to start '{}': {}", name, e),
                    StatusKind::Error,
                );
            }
        }
    }

    /// Handle stop driver
    fn handle_stop_driver(&mut self, name: &str) {
        let svc_name = match self.resolve_service_name(name) {
            Some(s) => s,
            None => {
                self.state.set_status(
                    format!("No service name found for '{}'", name),
                    StatusKind::Error,
                );
                return;
            }
        };
        match services::stop_driver(&svc_name) {
            Ok(()) => {
                self.state
                    .set_status(format!("Driver '{}' stopped", name), StatusKind::Success);
                self.start_loading();
            }
            Err(e) => {
                self.state.set_status(
                    format!("Failed to stop '{}': {}", name, e),
                    StatusKind::Error,
                );
            }
        }
    }

    /// Handle register driver (re-register selected driver with its current path)
    fn handle_register_driver(&mut self, name: &str) {
        if let Some(driver) = self.state.drivers.iter().find(|d| d.name == name) {
            let path = driver.file_path.clone();
            let svc_name = driver.service_name.as_deref().unwrap_or(name);
            match services::register_driver(svc_name, svc_name, &path) {
                Ok(()) => {
                    self.state
                        .set_status(format!("Driver '{}' registered", name), StatusKind::Success);
                }
                Err(e) => {
                    self.state.set_status(
                        format!("Failed to register '{}': {}", name, e),
                        StatusKind::Error,
                    );
                }
            }
        }
    }

    /// Handle unregister driver
    fn handle_unregister_driver(&mut self, name: &str) {
        let svc_name = match self.resolve_service_name(name) {
            Some(s) => s,
            None => {
                self.state.set_status(
                    format!("No service name found for '{}'", name),
                    StatusKind::Error,
                );
                return;
            }
        };
        match services::unregister_driver(&svc_name) {
            Ok(()) => {
                self.state.set_status(
                    format!("Driver '{}' unregistered", name),
                    StatusKind::Success,
                );
                self.start_loading();
            }
            Err(e) => {
                self.state.set_status(
                    format!("Failed to unregister '{}': {}", name, e),
                    StatusKind::Error,
                );
            }
        }
    }

    // --- Driver Manager handlers ---

    fn dm_log(&mut self, text: &str, kind: StatusKind) {
        use super::state::LogEntry;
        self.state.driver_manager.log.push(LogEntry {
            text: text.to_string(),
            kind,
            timestamp: std::time::Instant::now(),
        });
    }

    fn handle_dm_browse(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Select Driver File")
            .add_filter("Driver Files", &["sys"])
            .add_filter("All Files", &["*"])
            .pick_file();

        if let Some(path) = file {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("driver")
                .to_string();
            self.state.driver_manager.driver_path = path.display().to_string();
            self.state.driver_manager.service_name = name;
            self.state.driver_manager.service_status = None;
        }
    }

    fn handle_dm_register(&mut self) {
        let dm = &self.state.driver_manager;
        if dm.service_name.is_empty() || dm.driver_path.is_empty() {
            self.dm_log(
                "Service name and driver path are required",
                StatusKind::Error,
            );
            return;
        }
        let name = dm.service_name.clone();
        let path = dm.driver_path.clone();
        let start_type = dm.start_type;
        let driver_type = dm.driver_type;

        match services::register_driver_ex(&name, &name, &path, start_type, driver_type) {
            Ok(()) => self.dm_log(
                &format!("Registered '{}' successfully", name),
                StatusKind::Success,
            ),
            Err(e) => self.dm_log(
                &format!("Register '{}' failed: {}", name, e),
                StatusKind::Error,
            ),
        }
    }

    fn handle_dm_start(&mut self) {
        let name = self.state.driver_manager.service_name.clone();
        if name.is_empty() {
            self.dm_log("Service name is required", StatusKind::Error);
            return;
        }
        match services::start_driver(&name) {
            Ok(()) => {
                self.dm_log(
                    &format!("Started '{}' successfully", name),
                    StatusKind::Success,
                );
                self.state.driver_manager.service_status = Some("Running".to_string());
            }
            Err(e) => self.dm_log(
                &format!("Start '{}' failed: {}", name, e),
                StatusKind::Error,
            ),
        }
    }

    fn handle_dm_stop(&mut self) {
        let name = self.state.driver_manager.service_name.clone();
        if name.is_empty() {
            self.dm_log("Service name is required", StatusKind::Error);
            return;
        }
        match services::stop_driver(&name) {
            Ok(()) => {
                self.dm_log(
                    &format!("Stopped '{}' successfully", name),
                    StatusKind::Success,
                );
                self.state.driver_manager.service_status = Some("Stopped".to_string());
            }
            Err(e) => self.dm_log(&format!("Stop '{}' failed: {}", name, e), StatusKind::Error),
        }
    }

    fn handle_dm_unregister(&mut self) {
        let name = self.state.driver_manager.service_name.clone();
        if name.is_empty() {
            self.dm_log("Service name is required", StatusKind::Error);
            return;
        }
        match services::unregister_driver(&name) {
            Ok(()) => {
                self.dm_log(
                    &format!("Unregistered '{}' successfully", name),
                    StatusKind::Success,
                );
                self.state.driver_manager.service_status = None;
            }
            Err(e) => self.dm_log(
                &format!("Unregister '{}' failed: {}", name, e),
                StatusKind::Error,
            ),
        }
    }

    fn handle_dm_query_status(&mut self) {
        let name = self.state.driver_manager.service_name.clone();
        if name.is_empty() {
            self.dm_log("Service name is required", StatusKind::Error);
            return;
        }
        match services::query_driver_status(&name) {
            Ok(status) => {
                self.dm_log(&format!("'{}': {}", name, status), StatusKind::Info);
                self.state.driver_manager.service_status = Some(status);
            }
            Err(e) => {
                self.dm_log(
                    &format!("Query '{}' failed: {}", name, e),
                    StatusKind::Error,
                );
                self.state.driver_manager.service_status = None;
            }
        }
    }

    /// Helper to check if a path resides within a protected Windows directory
    #[cfg(target_os = "windows")]
    fn is_secure_directory(exe_path: &Path) -> bool {
        // Standard secure directories on Windows where standard users cannot write
        let secure_dirs = [
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string()),
            std::env::var("ProgramFiles(x86)")
                .unwrap_or_else(|_| r"C:\Program Files (x86)".to_string()),
            std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string()),
        ];

        let exe_path_str = exe_path.to_string_lossy().to_uppercase();

        secure_dirs.iter().any(|dir| {
            let secure_dir_str = dir.to_uppercase();
            exe_path_str.starts_with(&secure_dir_str)
        })
    }

    /// Handle elevate to admin button click
    fn handle_elevate_to_admin() {
        log::info!("Admin elevation button clicked");

        if Self::is_running_as_admin() {
            log::info!("Process is already elevated; skipping relaunch");
            return;
        }

        #[cfg(target_os = "windows")]
        {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::UI::Shell::ShellExecuteW;
            use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
            use windows::core::PCWSTR;

            let to_wide = |s: &str| -> Vec<u16> {
                OsStr::new(s)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect()
            };

            if let Ok(exe) = std::env::current_exe() {
                // Verify the executable is running from a secure location
                if !Self::is_secure_directory(&exe) {
                    log::error!(
                        "Security violation: Cannot elevate from an insecure directory: {:?}",
                        exe
                    );

                    return;
                }

                if let Some(exe_str) = exe.to_str() {
                    log::info!("current_exe path: {}", exe_str);

                    let verb = to_wide("runas");
                    let file = to_wide(exe_str);
                    let empty = to_wide("");

                    unsafe {
                        let result = ShellExecuteW(
                            None,
                            PCWSTR(verb.as_ptr()),
                            PCWSTR(file.as_ptr()),
                            PCWSTR(empty.as_ptr()),
                            PCWSTR(empty.as_ptr()),
                            SW_SHOWNORMAL,
                        );
                        // ShellExecuteW returns HINSTANCE > 32 on success
                        if result.0 as usize > 32 {
                            std::process::exit(0);
                        } else {
                            log::error!("ShellExecuteW failed with code: {}", result.0 as usize);
                        }
                    }
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            log::warn!("Admin elevation only supported on Windows");
        }
    }

    fn is_running_as_admin() -> bool {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::Security::{
                GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
            };
            use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

            unsafe {
                let mut token = Default::default();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                    return false;
                }

                let mut elevation = TOKEN_ELEVATION::default();
                let mut returned = 0u32;
                let result = GetTokenInformation(
                    token,
                    TokenElevation,
                    Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                    std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                    &mut returned,
                );

                let _ = CloseHandle(token);
                result.is_ok() && elevation.TokenIsElevated != 0
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    /// Handle Save Selected Items - save to file via dialog
    fn handle_save_selected(&mut self) {
        let text = match self.state.format_selected_for_clipboard() {
            Some(t) => t,
            None => {
                self.state
                    .set_status("No driver selected", StatusKind::Error);
                return;
            }
        };

        let file = rfd::FileDialog::new()
            .set_title("Save Selected Items")
            .add_filter("Text Files", &["txt"])
            .add_filter("All Files", &["*"])
            .save_file();

        if let Some(path) = file {
            match std::fs::write(&path, &text) {
                Ok(()) => {
                    self.state
                        .set_status(format!("Saved to {}", path.display()), StatusKind::Success);
                }
                Err(e) => {
                    self.state
                        .set_status(format!("Failed to save: {}", e), StatusKind::Error);
                }
            }
        }
    }

    /// Handle File Properties - open Windows shell properties dialog
    fn handle_file_properties(&self) {
        if let Some(driver) = self.state.selected() {
            let path = driver.file_path.clone();
            #[cfg(target_os = "windows")]
            {
                use std::ffi::OsStr;
                use std::os::windows::ffi::OsStrExt;
                use windows::Win32::UI::Shell::{
                    SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW,
                };
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
                use windows::core::PCWSTR;

                let path_wide: Vec<u16> = OsStr::new(&path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                let verb: Vec<u16> = OsStr::new("properties")
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();

                unsafe {
                    let mut info = SHELLEXECUTEINFOW {
                        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                        fMask: SEE_MASK_INVOKEIDLIST,
                        lpVerb: PCWSTR(verb.as_ptr()),
                        lpFile: PCWSTR(path_wide.as_ptr()),
                        nShow: SW_SHOW.0 as i32,
                        ..std::mem::zeroed()
                    };
                    let _ = ShellExecuteExW(&mut info);
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = path;
            }
        }
    }

    /// Handle Google Search for selected driver
    fn handle_google_search(&self) {
        if let Some(driver) = self.state.selected() {
            let query = &driver.name;
            let url = format!("https://www.google.com/search?q={}", query);
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("cmd")
                    .raw_arg(format!("/C start \"\" \"{}\"", url))
                    .spawn();
            }
        }
    }

    /// Process all GUI actions
    fn process_actions(&mut self, ctx: &egui::Context, actions: Vec<GuiAction>) {
        for action in actions {
            match action {
                GuiAction::Refresh => self.start_loading(),
                GuiAction::LoadDriver => self.handle_load_driver(),
                GuiAction::UnloadDriver => self.handle_unload_driver(),
                GuiAction::ElevateToAdmin => Self::handle_elevate_to_admin(),
                GuiAction::StartDriver(name) => self.handle_start_driver(&name),
                GuiAction::StopDriver(name) => self.handle_stop_driver(&name),
                GuiAction::RegisterDriver(name) => self.handle_register_driver(&name),
                GuiAction::UnregisterDriver(name) => self.handle_unregister_driver(&name),
                GuiAction::SelectDriver(idx, ctrl, shift) => {
                    if shift {
                        self.state.range_select_driver(idx);
                    } else if ctrl {
                        self.state.toggle_select_driver(idx);
                    } else {
                        self.state.select_driver(idx);
                    }
                    self.state.scroll_to_cursor = true;
                }
                GuiAction::SaveSelectedItems => self.handle_save_selected(),
                GuiAction::FileProperties => self.handle_file_properties(),
                GuiAction::GoogleSearch => self.handle_google_search(),
                GuiAction::Exit => std::process::exit(0),
                GuiAction::FocusFind => {
                    self.state.focus_search = true;
                }
                GuiAction::CopySelectedItems => {
                    if let Some(text) = self.state.format_selected_for_clipboard() {
                        ctx.copy_text(text);
                        self.state
                            .set_status("Copied to clipboard", StatusKind::Success);
                    }
                }
                GuiAction::SelectAll => {
                    let names: Vec<String> = self
                        .state
                        .filtered_drivers()
                        .iter()
                        .map(|d| d.name.clone())
                        .collect();
                    for name in names {
                        self.state.selected_names.insert(name);
                    }
                }
                GuiAction::DeselectAll => {
                    self.state.selected_names.clear();
                }
                GuiAction::ExportJsonAll => self.handle_export(ExportFormat::Json, false),
                GuiAction::ExportJsonSelected => self.handle_export(ExportFormat::Json, true),
                GuiAction::ExportCsvAll => self.handle_export(ExportFormat::Csv, false),
                GuiAction::ExportCsvSelected => self.handle_export(ExportFormat::Csv, true),
                GuiAction::ExportTextAll => self.handle_export(ExportFormat::Text, false),
                GuiAction::ExportTextSelected => self.handle_export(ExportFormat::Text, true),
                GuiAction::ExportHtmlAll => self.handle_export(ExportFormat::Html, false),
                GuiAction::ExportHtmlSelected => self.handle_export(ExportFormat::Html, true),
                GuiAction::SaveSnapshot => self.handle_save_snapshot(),
                GuiAction::CompareSnapshot => self.handle_compare_snapshot(),
                GuiAction::VerifySignature => self.handle_verify_signature(),
                GuiAction::ShowAbout => {
                    self.state.show_about = true;
                }
                GuiAction::OpenDriverManager => {
                    self.state.show_driver_manager = true;
                }
                GuiAction::DmBrowseDriver => self.handle_dm_browse(),
                GuiAction::DmRegister => self.handle_dm_register(),
                GuiAction::DmStart => self.handle_dm_start(),
                GuiAction::DmStop => self.handle_dm_stop(),
                GuiAction::DmUnregister => self.handle_dm_unregister(),
                GuiAction::DmQuickLoad => {
                    self.handle_dm_register();
                    if self
                        .state
                        .driver_manager
                        .log
                        .last()
                        .map_or(false, |e| matches!(e.kind, StatusKind::Success))
                    {
                        self.handle_dm_start();
                    }
                    self.start_loading();
                }
                GuiAction::DmQuickUnload => {
                    self.handle_dm_stop();
                    self.handle_dm_unregister();
                    self.start_loading();
                }
                GuiAction::DmQueryStatus => self.handle_dm_query_status(),
            }
        }
    }

    /// Handle export to file with format selection
    fn handle_export(&mut self, format: ExportFormat, selected_only: bool) {
        let drivers: Vec<_> = if selected_only {
            let sel = self.state.selected_drivers_cloned();
            if sel.is_empty() {
                self.state
                    .set_status("No drivers selected", StatusKind::Error);
                return;
            }
            sel
        } else {
            self.state.drivers.clone()
        };

        let ext = format.extension();
        let filter_name = match format {
            ExportFormat::Csv => "CSV Files",
            ExportFormat::Json => "JSON Files",
            ExportFormat::Html => "HTML Files",
            ExportFormat::Text => "Text Files",
        };

        let suffix = if selected_only { "_selected" } else { "" };
        let file = rfd::FileDialog::new()
            .set_title(&format!("Export as {}", ext.to_uppercase()))
            .add_filter(filter_name, &[ext])
            .add_filter("All Files", &["*"])
            .set_file_name(&format!("driverexplorer{}.{}", suffix, ext))
            .save_file();

        if let Some(path) = file {
            let is_html = matches!(format, ExportFormat::Html);
            let options = ExportOptions {
                format,
                output_file: Some(path.clone()),
                open_in_browser: is_html,
            };

            match crate::export::export_drivers(&drivers, &options) {
                Ok(_) => {
                    self.state.set_status(
                        format!("Exported {} drivers to {}", drivers.len(), path.display()),
                        StatusKind::Success,
                    );
                }
                Err(e) => {
                    self.state
                        .set_status(format!("Export failed: {}", e), StatusKind::Error);
                }
            }
        }
    }

    /// Save a JSON snapshot of current driver state
    fn handle_save_snapshot(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Save Driver Snapshot")
            .add_filter("JSON Files", &["json"])
            .set_file_name("driverexplorer_snapshot.json")
            .save_file();

        if let Some(path) = file {
            let snapshot = serde_json::json!({
                "generatedAt": chrono::Utc::now().to_rfc3339(),
                "count": self.state.drivers.len(),
                "drivers": self.state.drivers,
            });

            match std::fs::write(
                &path,
                serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
            ) {
                Ok(()) => {
                    self.state.set_status(
                        format!("Snapshot saved to {}", path.display()),
                        StatusKind::Success,
                    );
                }
                Err(e) => {
                    self.state
                        .set_status(format!("Failed to save snapshot: {}", e), StatusKind::Error);
                }
            }
        }
    }

    /// Spins up a localhost HTTP server that serves `html` exactly once
    /// on a random OS-assigned port, then shuts itself down.
    /// No file ever touches disk — eliminates the temp file attack surface entirely.
    fn open_html_in_browser(&mut self, html: String) {
        self.state
            .set_status("Opening a One-Shot HTTP server", StatusKind::Info);

        let server = match Server::http("127.0.0.1:0") {
            Ok(s) => s,
            Err(e) => {
                self.state.set_status(
                    format!("Failed to bind local HTTP server: {}", e),
                    StatusKind::Error,
                );
                log::error!("Failed to bind local HTTP server: {}", e);
                return;
            }
        };

        // Retrieve the actual port the OS assigned
        let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);

        if port == 0 {
            self.state
                .set_status("OS assigned invalid port".to_string(), StatusKind::Error);
            log::error!("OS assigned invalid port — aborting");
            return;
        }

        let url = format!("http://127.0.0.1:{}/", port);
        log::info!("Serving compare view at {}", url);

        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .raw_arg(format!("/C start \"\" \"{}\"", url))
                .spawn();
        }

        // spawn a thread so the UI thread is not blocked.
        std::thread::spawn(move || {
            match server.recv() {
                Ok(request) => {
                    let content_type =
                        Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap();

                    // Serve the hardcoded html variable — no disk I/O at all
                    let response = Response::from_string(html)
                        .with_header(content_type)
                        .with_status_code(200);

                    if let Err(e) = request.respond(response) {
                        log::error!("Failed to send HTTP response: {}", e);
                    }
                }
                Err(e) => {
                    log::error!("HTTP server recv error: {}", e);
                }
            }

            // Server is dropped here — port is released, no further requests served
            log::info!("One-shot HTTP server shut down cleanly");
        });
    }

    /// Compare current state against a saved snapshot
    fn handle_compare_snapshot(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Open Baseline Snapshot")
            .add_filter("JSON Files", &["json"])
            .pick_file();

        if let Some(path) = file {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    self.state
                        .set_status(format!("Failed to read snapshot: {}", e), StatusKind::Error);
                    return;
                }
            };

            let baseline: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    self.state
                        .set_status(format!("Invalid snapshot JSON: {}", e), StatusKind::Error);
                    return;
                }
            };

            let baseline_drivers: Vec<crate::drivers::DriverInfo> = match serde_json::from_value(
                baseline
                    .get("drivers")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![])),
            ) {
                Ok(d) => d,
                Err(e) => {
                    self.state.set_status(
                        format!("Failed to parse snapshot drivers: {}", e),
                        StatusKind::Error,
                    );
                    return;
                }
            };

            // Compare
            use std::collections::BTreeSet;
            let baseline_names: BTreeSet<String> =
                baseline_drivers.iter().map(|d| d.name.clone()).collect();
            let current_names: BTreeSet<String> =
                self.state.drivers.iter().map(|d| d.name.clone()).collect();

            let added: Vec<_> = current_names.difference(&baseline_names).collect();
            let removed: Vec<_> = baseline_names.difference(&current_names).collect();
            let common = baseline_names.intersection(&current_names).count();

            // Build comparison report as HTML and open
            let mut html =
                String::from("<!DOCTYPE html><html><head><title>Snapshot Comparison</title>");
            html.push_str("<style>body{font-family:sans-serif;padding:20px;background:#f5f5f5}");
            html.push_str(".container{max-width:900px;margin:0 auto;background:white;padding:30px;border-radius:8px;box-shadow:0 2px 10px rgba(0,0,0,0.1)}");
            html.push_str("h1{color:#333}h2{color:#555;margin-top:20px}");
            html.push_str(".added{color:#28a745}.removed{color:#dc3545}.unchanged{color:#6c757d}");
            html.push_str("ul{list-style:none;padding:0}li{padding:4px 8px;border-bottom:1px solid #eee}</style>");
            html.push_str("</head><body><div class=\"container\">");
            html.push_str(&format!("<h1>Snapshot Comparison</h1>"));
            html.push_str(&format!(
                "<p>Baseline: {} ({} drivers)</p>",
                path.display(),
                baseline_drivers.len()
            ));
            html.push_str(&format!(
                "<p>Current: Live system ({} drivers)</p>",
                self.state.drivers.len()
            ));

            html.push_str(&format!(
                "<h2 class=\"added\">Added ({}):</h2><ul>",
                added.len()
            ));
            for name in &added {
                html.push_str(&format!("<li>+ {}</li>", name));
            }
            html.push_str("</ul>");

            html.push_str(&format!(
                "<h2 class=\"removed\">Removed ({}):</h2><ul>",
                removed.len()
            ));
            for name in &removed {
                html.push_str(&format!("<li>- {}</li>", name));
            }
            html.push_str("</ul>");

            html.push_str(&format!("<p class=\"unchanged\">Unchanged: {}</p>", common));
            html.push_str("</div></body></html>");

            self.open_html_in_browser(html);

            self.state.set_status(
                format!(
                    "Compared: +{} added, -{} removed, {} unchanged",
                    added.len(),
                    removed.len(),
                    common
                ),
                StatusKind::Success,
            );
        }
    }

    /// Verify signature of selected driver
    fn handle_verify_signature(&mut self) {
        if let Some(driver) = self.state.selected() {
            let path = driver.file_path.clone();
            let name = driver.name.clone();
            match crate::drivers::verify_signature(&path) {
                Ok((is_signed, signer)) => {
                    let status_text = if is_signed { "Signed" } else { "Unsigned" };
                    let signer_text = signer.as_deref().unwrap_or("N/A");
                    self.state.set_status(
                        format!("{}: {} (Signer: {})", name, status_text, signer_text),
                        if is_signed {
                            StatusKind::Success
                        } else {
                            StatusKind::Info
                        },
                    );
                }
                Err(e) => {
                    self.state.set_status(
                        format!("Signature check failed for '{}': {}", name, e),
                        StatusKind::Error,
                    );
                }
            }
        } else {
            self.state
                .set_status("No driver selected", StatusKind::Error);
        }
    }
}

impl eframe::App for DriverExplorerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Force dark mode regardless of OS theme
        ctx.set_theme(egui::Theme::Dark);

        // Check if drivers have finished loading
        self.state.check_receiver();

        // Auto-refresh logic
        if self.state.auto_refresh {
            if let Some(last) = self.state.auto_refresh_interval {
                if last.elapsed().as_secs() >= 5
                    && !matches!(self.state.loading_state, LoadingState::Loading)
                {
                    self.state.auto_refresh_interval = Some(std::time::Instant::now());
                    self.start_loading();
                }
            }
        }

        // Draw toolbar and get actions
        let mut all_actions = ui::draw_toolbar(ctx, &mut self.state, self.is_elevated);

        // Draw main area and collect actions
        all_actions.extend(ui::draw_main_area(ctx, &mut self.state));

        // Process all actions
        self.process_actions(ctx, all_actions);

        // Request repaint for loading state or auto-refresh
        if matches!(self.state.loading_state, LoadingState::Loading) || self.state.auto_refresh {
            ctx.request_repaint();
        }
    }
}
