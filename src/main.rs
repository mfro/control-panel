#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::CString,
    ptr::null_mut,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use windows::{
    self,
    Win32::{
        Devices::FunctionDiscovery::{PKEY_Device_FriendlyName, PKEY_DeviceClass_IconPath},
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Gdi::{
                BeginPaint, CreateSolidBrush, EndPaint, FillRect, HDC, PAINTSTRUCT, RDW_INVALIDATE,
                RedrawWindow,
            },
            GdiPlus::{
                GdipCreateFromHDC, GdipCreatePen1, GdipDeleteGraphics, GdipDrawLine,
                GdipSetPenEndCap, GdipSetPenStartCap, GdiplusStartup, GdiplusStartupInput,
                LineCapTriangle, UnitPixel,
            },
        },
        Media::Audio::{
            EDataFlow, ERole,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDevice, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
            MMDeviceEnumerator, eCapture, eMultimedia, eRender,
        },
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoInitialize, STGM_READ},
            LibraryLoader::GetModuleHandleA,
            RemoteDesktop::{NOTIFY_FOR_ALL_SESSIONS, WTSRegisterSessionNotification},
        },
        UI::{
            Shell::ExtractIconExA,
            WindowsAndMessaging::{
                DefWindowProcA, DestroyIcon, DestroyWindow, DispatchMessageA, DrawIcon,
                GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetMessageA, HICON, HMENU, HWND_TOPMOST,
                LWA_ALPHA, MSG, PostQuitMessage, RegisterClassA, SWP_NOMOVE, SWP_NOSIZE,
                SendMessageA, SetLayeredWindowAttributes, SetWindowLongA, SetWindowPos,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY, WM_KILLFOCUS, WM_PAINT, WM_QUIT,
                WM_WTSSESSION_CHANGE, WNDCLASSA, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE, WTS_SESSION_LOCK,
                WTS_SESSION_UNLOCK,
            },
        },
    },
    core::implement,
};
use windows_core::{PCSTR, s};

windows_link::link!(
    "user32.dll" "system"
    fn CreateWindowExA(
        dwexstyle: WINDOW_EX_STYLE,
        lpclassname: windows_core::PCSTR,
        lpwindowname: windows_core::PCSTR,
        dwstyle: WINDOW_STYLE,
        x: i32,
        y: i32,
        nwidth: i32,
        nheight: i32,
        hwndparent: HWND,
        hmenu: HMENU,
        hinstance: HINSTANCE,
        lpparam: *const core::ffi::c_void) -> HWND
);

fn default<T: Default>() -> T {
    Default::default()
}

#[cfg(not(debug_assertions))]
fn log_to_file(str: &str) {
    use std::{fs::File, io::Write};
    let mut file = File::options()
        .write(true)
        .append(true)
        .create(true)
        .open(r"E:\persistent\code\control-panel\log.txt")
        .unwrap();

    file.write_all(str.as_bytes()).unwrap();
    file.write_all(b"\n").unwrap();
}

#[cfg(not(debug_assertions))]
macro_rules! log {
    ($expression:literal $(, $arg:expr)*) => {
        crate::log_to_file(&format!($expression $(, $arg )*))
    };
}

#[cfg(debug_assertions)]
macro_rules! log {
    ($expression:literal $(, $arg:expr)*) => {
        println!($expression $(, $arg )*)
    };
}

unsafe impl Sync for RedrawHandle {}
unsafe impl Send for RedrawHandle {}

#[derive(Clone, Copy)]
struct RedrawHandle {
    hwnd: HWND,
}

impl RedrawHandle {
    fn new(hwnd: HWND) -> Self {
        Self { hwnd }
    }

    fn redraw(&self) {
        unsafe {
            SendMessageA(self.hwnd, WM_KILLFOCUS, default(), default());
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE);
        }
    }
}

struct State {
    audio_helper: IMMDeviceEnumerator,
    volume_callback: IAudioEndpointVolumeCallback,
    devices: HashMap<String, IAudioEndpointVolume>,
    unlock_mute_output: bool,
    unlock_mute_input: bool,
}

impl State {
    pub fn get_device_info(&mut self, device: &IMMDevice) -> Result<DeviceInfo> {
        unsafe {
            let props = device.OpenPropertyStore(STGM_READ)?;
            let icon_path = props.GetValue(&PKEY_DeviceClass_IconPath)?.to_string();

            let id = device.GetId()?.to_string()?;

            if !self.devices.contains_key(&id) {
                let name: String = props.GetValue(&PKEY_Device_FriendlyName)?.to_string();
                log!("start tracking device: {} {}", id, name);

                let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
                volume.RegisterControlChangeNotify(&self.volume_callback)?;

                self.devices.insert(id.clone(), volume);
            }

            let volume = &self.devices[&id];
            let mute = volume.GetMute()?.into();

            Ok(DeviceInfo { icon_path, mute })
        }
    }
}

struct DeviceInfo {
    icon_path: String,
    mute: bool,
}

thread_local! {
    static WINDOW_STATE: RefCell<Option<Arc<Mutex<State>>>> = RefCell::new(None);
}

fn get_icon(icon_path: &str) -> Result<HICON> {
    unsafe {
        let mut parts = icon_path.split(",");
        let path = parts.next().context("invalid icon path")?;
        let index = parts.next().context("invalid icon path")?.parse()?;

        let mut icon = default();
        ExtractIconExA(
            PCSTR(CString::from_str(path)?.as_ptr() as *const u8),
            index,
            Some(&mut icon),
            None,
            1,
        );

        Ok(icon)
    }
}

struct PaintHandle {
    pub hwnd: HWND,
    pub hdc: HDC,
    pub paint: PAINTSTRUCT,
}

impl PaintHandle {
    pub fn new(hwnd: HWND) -> Self {
        let mut paint = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
        Self { hwnd, hdc, paint }
    }
}

impl Drop for PaintHandle {
    fn drop(&mut self) {
        unsafe {
            let result = EndPaint(self.hwnd, &self.paint);
            if let Err(e) = result.ok() {
                log!("end paint error: {:?}", e);
            }
        }
    }
}

fn paint_window(hwnd: HWND, state: &mut State) -> Result<()> {
    unsafe {
        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let output_device = state.get_device_info(&device)?;
        let output_icon = get_icon(&output_device.icon_path)?;

        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eCapture, eMultimedia)?;
        let input_device = state.get_device_info(&device)?;
        let input_icon = get_icon(&input_device.icon_path)?;

        let mut client_rect = RECT::default();
        GetClientRect(hwnd, &mut client_rect)?;

        let paint = PaintHandle::new(hwnd);

        let hbrush = CreateSolidBrush(COLORREF(0xffffff));
        FillRect(paint.hdc, &client_rect, hbrush);

        DrawIcon(paint.hdc, 8, 8, output_icon)?;
        DrawIcon(paint.hdc, 48, 8, input_icon)?;

        let mut graphics = std::ptr::null_mut();
        GdipCreateFromHDC(paint.hdc, &mut graphics);

        let mut pen = std::ptr::null_mut();
        GdipCreatePen1(0xffff0000, 8.0, UnitPixel, &mut pen);
        GdipSetPenEndCap(pen, LineCapTriangle);
        GdipSetPenStartCap(pen, LineCapTriangle);

        if output_device.mute {
            GdipDrawLine(graphics, pen, 8.0, 8.0, 40.0, 40.0);
            GdipDrawLine(graphics, pen, 40.0, 8.0, 8.0, 40.0);
        }

        if input_device.mute {
            GdipDrawLine(graphics, pen, 48.0, 8.0, 80.0, 40.0);
            GdipDrawLine(graphics, pen, 80.0, 8.0, 48.0, 40.0);
        }

        GdipDeleteGraphics(graphics);

        DestroyIcon(output_icon)?;
        DestroyIcon(input_icon)?;
    }

    Ok(())
}

fn on_lock(state: &mut State) -> Result<()> {
    unsafe {
        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eRender, eMultimedia)?;

        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let mute: bool = volume.GetMute()?.into();

        if !mute {
            volume.SetMute(true, null_mut())?;
            state.unlock_mute_output = true;
        }

        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eCapture, eMultimedia)?;

        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let mute: bool = volume.GetMute()?.into();

        if !mute {
            volume.SetMute(true, null_mut())?;
            state.unlock_mute_input = true;
        }
    }

    Ok(())
}

fn on_unlock(state: &mut State) -> Result<()> {
    unsafe {
        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eRender, eMultimedia)?;

        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let mute = volume.GetMute()?.into();

        if mute && state.unlock_mute_output {
            volume.SetMute(false, null_mut())?;
            state.unlock_mute_output = false;
        }

        let device = state
            .audio_helper
            .GetDefaultAudioEndpoint(eCapture, eMultimedia)?;

        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let mute = volume.GetMute()?.into();

        if mute && state.unlock_mute_input {
            volume.SetMute(false, null_mut())?;
            state.unlock_mute_input = false;
        }
    }

    Ok(())
}

fn wrap(function: impl FnOnce(&mut State) -> Result<()>) {
    WINDOW_STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let mut state = state.lock().unwrap();

            if let Err(e) = (function)(&mut *state) {
                log!("error: {:?}", e);
            }
        } else {
            log!("no window state");
        }
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    event: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match event {
            WM_DESTROY => {
                PostQuitMessage(WM_QUIT as _);
            }

            WM_KILLFOCUS => {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE,
                )
                .unwrap();
            }

            WM_PAINT => wrap(|state| paint_window(hwnd, state)),

            WM_WTSSESSION_CHANGE => match wparam.0 as _ {
                WTS_SESSION_LOCK => wrap(|state| on_lock(state)),
                WTS_SESSION_UNLOCK => wrap(|state| on_unlock(state)),

                _ => {}
            },

            _ => {}
        }

        DefWindowProcA(hwnd, event, wparam, lparam)
    }
}

#[implement(IMMNotificationClient)]
struct DeviceNotificationCallback {
    redraw_handle: RedrawHandle,
}

impl IMMNotificationClient_Impl for DeviceNotificationCallback_Impl {
    fn OnDeviceStateChanged(
        &self,
        _pwstrdeviceid: &windows_core::PCWSTR,
        _dwnewstate: windows::Win32::Media::Audio::DEVICE_STATE,
    ) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _pwstrdeviceid: &windows_core::PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _pwstrdeviceid: &windows_core::PCWSTR) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _pwstrdefaultdeviceid: &windows_core::PCWSTR,
    ) -> windows_core::Result<()> {
        self.redraw_handle.redraw();

        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &windows_core::PCWSTR,
        _key: &windows::Win32::Foundation::PROPERTYKEY,
    ) -> windows_core::Result<()> {
        Ok(())
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    redraw_handle: RedrawHandle,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(
        &self,
        _event: *mut windows::Win32::Media::Audio::AUDIO_VOLUME_NOTIFICATION_DATA,
    ) -> windows_core::Result<()> {
        self.redraw_handle.redraw();

        Ok(())
    }
}

fn run() -> Result<()> {
    unsafe {
        log!("launch attempt");
        CoInitialize(None).ok()?;

        let mut token = 0;
        let mut input = GdiplusStartupInput::default();
        input.GdiplusVersion = 1;
        let mut output: windows::Win32::Graphics::GdiPlus::GdiplusStartupOutput = default();
        GdiplusStartup(&mut token, &input, &mut output);

        let audio_helper: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let hinstance: HINSTANCE = GetModuleHandleA(None)?.into();

        let window_class_name = s!("mfro test window class");

        let mut wc = WNDCLASSA::default();
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = hinstance;
        wc.lpszClassName = window_class_name;

        RegisterClassA(&wc);

        let style = WS_VISIBLE | WS_POPUP;
        let exstyle =
            WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE;

        let host_hwnd = CreateWindowExA(
            WINDOW_EX_STYLE::default(),
            window_class_name,
            s!("test name"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            default(),
            default(),
            hinstance,
            std::ptr::null(),
        );

        let hwnd = CreateWindowExA(
            exstyle,
            window_class_name,
            s!("test name"),
            style,
            -800,
            1415,
            600,
            48,
            host_hwnd,
            default(),
            hinstance,
            std::ptr::null(),
        );

        SetWindowLongA(hwnd, GWL_STYLE, style.0 as _);
        SetWindowLongA(hwnd, GWL_EXSTYLE, exstyle.0 as _);
        SetLayeredWindowAttributes(hwnd, COLORREF(0x0), 255, LWA_ALPHA)?;

        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_ALL_SESSIONS)?;

        log!("{:?}", hwnd);

        let redraw_handle = RedrawHandle::new(hwnd);

        let callback = VolumeCallback { redraw_handle };

        WINDOW_STATE.set(Some(Arc::new(Mutex::new(State {
            audio_helper: audio_helper.clone(),
            volume_callback: callback.into(),
            devices: HashMap::new(),
            unlock_mute_output: false,
            unlock_mute_input: false,
        }))));

        let callback = DeviceNotificationCallback {
            redraw_handle: redraw_handle.clone(),
        };
        let callback: IMMNotificationClient = callback.into();
        audio_helper.RegisterEndpointNotificationCallback(&callback)?;

        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(30));

                redraw_handle.redraw();
            }
        });

        let mut message = MSG::default();

        while GetMessageA(&mut message, None, 0, 0).into() {
            DispatchMessageA(&message);
        }

        audio_helper.UnregisterEndpointNotificationCallback(&callback)?;
        DestroyWindow(hwnd)?;
    }

    Ok(())
}

fn main() {
    loop {
        match run() {
            Ok(()) => break,
            Err(e) => {
                log!("main error: {:?}", e);
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}
