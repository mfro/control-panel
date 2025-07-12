#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell, collections::HashMap, ffi::CString, ptr::null_mut, str::FromStr, sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result};
use windows::{
    self,
    Win32::{
        Devices::FunctionDiscovery::{PKEY_Device_FriendlyName, PKEY_DeviceClass_IconPath},
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
        Graphics::{
            Gdi::{
                AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleBitmap,
                CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, GetDC,
                InvalidateRect, SelectObject,
            },
            GdiPlus::{
                GdipCreateFromHDC, GdipCreatePen1, GdipDrawLine, GdipSetPenEndCap,
                GdipSetPenStartCap, GdiplusStartup, GdiplusStartupInput, LineCapTriangle,
                UnitPixel,
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
                GetMessageA, GetWindowRect, HICON, HMENU, HWND_DESKTOP, HWND_TOPMOST, MSG,
                PostQuitMessage, RegisterClassExA, SWP_NOMOVE, SWP_NOSIZE, SendMessageA,
                SetWindowPos, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_DESTROY, WM_KILLFOCUS, WM_PAINT, WM_QUIT, WM_WTSSESSION_CHANGE, WNDCLASSEXA,
                WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
                WS_VISIBLE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
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
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }
}

struct AudioDevice {
    controls: IAudioEndpointVolume,
    icon: HICON,
}

impl AudioDevice {
    pub fn new(controls: IAudioEndpointVolume, icon: HICON) -> Self {
        Self { controls, icon }
    }

    pub fn is_mute(&self) -> Result<bool> {
        let value = unsafe { self.controls.GetMute() }?;
        Ok(value.as_bool())
    }

    pub fn set_mute(&self, value: bool) -> Result<()> {
        unsafe {
            self.controls.SetMute(value, null_mut())?;
        }
        Ok(())
    }
}

struct AudioManager {
    device_enumerator: IMMDeviceEnumerator,
    controls_callback: IAudioEndpointVolumeCallback,
    devices: HashMap<String, AudioDevice>,
    unlock_mute_output: bool,
    unlock_mute_input: bool,
}

impl AudioManager {
    fn new(redraw_handle: RedrawHandle) -> Result<Self> {
        unsafe {
            let device_enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

            let callback = VolumeCallback { redraw_handle };

            Ok(Self {
                device_enumerator,
                controls_callback: callback.into(),
                devices: HashMap::new(),
                unlock_mute_output: false,
                unlock_mute_input: false,
            })
        }
    }

    pub fn get_default_device(&self, flow: EDataFlow) -> Result<IMMDevice> {
        unsafe {
            let device = self
                .device_enumerator
                .GetDefaultAudioEndpoint(flow, eMultimedia)?;

            Ok(device)
        }
    }

    pub fn get_device(&mut self, device: &IMMDevice) -> Result<&AudioDevice> {
        unsafe {
            let props = device.OpenPropertyStore(STGM_READ)?;

            let id = device.GetId()?.to_string()?;

            if !self.devices.contains_key(&id) {
                let name: String = props.GetValue(&PKEY_Device_FriendlyName)?.to_string();
                let icon_path = props.GetValue(&PKEY_DeviceClass_IconPath)?.to_string();
                let icon = load_icon(&icon_path)?;

                log!("start tracking device: {} {}", id, name);

                let controls: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
                controls.RegisterControlChangeNotify(&self.controls_callback)?;

                let info = AudioDevice::new(controls, icon);
                self.devices.insert(id.clone(), info);
            }

            Ok(&self.devices[&id])
        }
    }
}

fn load_icon(icon_path: &str) -> Result<HICON> {
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

struct WindowManager {
    audio: AudioManager,
}

impl WindowManager {
    fn on_paint(&mut self, hwnd: HWND) -> Result<()> {
        unsafe {
            let mut window_rect = RECT::default();
            GetWindowRect(hwnd, &mut window_rect)?;

            let size = SIZE {
                cx: window_rect.right - window_rect.left,
                cy: window_rect.bottom - window_rect.top,
            };

            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, size.cx, size.cy);
            let _ = DeleteObject(SelectObject(dc, bitmap.into()));

            let mut graphics = std::ptr::null_mut();
            GdipCreateFromHDC(dc, &mut graphics);

            let mut pen = std::ptr::null_mut();
            GdipCreatePen1(0xffff0000, 8.0, UnitPixel, &mut pen);
            GdipSetPenEndCap(pen, LineCapTriangle);
            GdipSetPenStartCap(pen, LineCapTriangle);

            let output = self.audio.get_default_device(eRender)?;
            let output = self.audio.get_device(&output)?;

            DrawIcon(dc, 8, 8, output.icon)?;
            if output.is_mute()? {
                GdipDrawLine(graphics, pen, 8.0, 8.0, 40.0, 40.0);
                GdipDrawLine(graphics, pen, 40.0, 8.0, 8.0, 40.0);
            }

            let input = self.audio.get_default_device(eCapture)?;
            let input = self.audio.get_device(&input)?;

            DrawIcon(dc, 48, 8, input.icon)?;
            if input.is_mute()? {
                GdipDrawLine(graphics, pen, 48.0, 8.0, 80.0, 40.0);
                GdipDrawLine(graphics, pen, 80.0, 8.0, 48.0, 40.0);
            }

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as _,
                BlendFlags: 0,
                SourceConstantAlpha: 0xff,
                AlphaFormat: AC_SRC_ALPHA as _,
            };

            UpdateLayeredWindow(
                hwnd,
                Some(screen),
                None,
                Some(&size),
                Some(dc),
                Some(&default()),
                default(),
                Some(&blend),
                ULW_ALPHA,
            )?;

            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
        }

        Ok(())
    }

    fn on_lock(&mut self) -> Result<()> {
        let output = self.audio.get_default_device(eRender)?;
        let device = self.audio.get_device(&output)?;

        if !device.is_mute()? {
            device.set_mute(true)?;
            self.audio.unlock_mute_output = true;
        }

        let input = self.audio.get_default_device(eCapture)?;
        let device = self.audio.get_device(&input)?;

        if !device.is_mute()? {
            device.set_mute(true)?;
            self.audio.unlock_mute_input = true;
        }

        Ok(())
    }

    fn on_unlock(&mut self) -> Result<()> {
        if self.audio.unlock_mute_output {
            let output = self.audio.get_default_device(eRender)?;
            let device = self.audio.get_device(&output)?;

            if device.is_mute()? {
                device.set_mute(false)?;
            }

            self.audio.unlock_mute_output = false;
        }

        if self.audio.unlock_mute_input {
            let input = self.audio.get_default_device(eCapture)?;
            let device = self.audio.get_device(&input)?;

            if device.is_mute()? {
                device.set_mute(false)?;
            }

            self.audio.unlock_mute_input = false;
        }

        Ok(())
    }
}

fn wrap(function: impl FnOnce(&mut WindowManager) -> Result<()>) {
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

thread_local! {
    static WINDOW_STATE: RefCell<Option<Mutex<WindowManager>>> = RefCell::new(None);
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

            WM_PAINT => wrap(|state| state.on_paint(hwnd)),

            WM_WTSSESSION_CHANGE => match wparam.0 as _ {
                WTS_SESSION_LOCK => wrap(|state| state.on_lock()),
                WTS_SESSION_UNLOCK => wrap(|state| state.on_unlock()),

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

fn initialize_gdp() {
    let mut token = 0;
    let mut input = GdiplusStartupInput::default();
    input.GdiplusVersion = 1;
    let mut output = default();
    unsafe { GdiplusStartup(&mut token, &input, &mut output) };
}

fn create_window() -> Result<HWND> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleA(None)?.into();

        let window_class_name = s!("mfro test window class");

        let mut wc = WNDCLASSEXA::default();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXA>() as _;
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = hinstance;
        wc.lpszClassName = window_class_name;
        wc.hbrBackground = CreateSolidBrush(COLORREF(0xff0000ff));

        RegisterClassExA(&wc);

        let style = WS_POPUP | WS_VISIBLE;
        let exstyle = WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT;

        let hwnd = CreateWindowExA(
            exstyle,
            window_class_name,
            s!("test name 2"),
            style,
            -800,
            1415,
            600,
            500,
            HWND_DESKTOP,
            default(),
            hinstance,
            std::ptr::null(),
        );

        Ok(hwnd)
    }
}

fn run() -> Result<()> {
    unsafe {
        log!("launch attempt");
        CoInitialize(None).ok()?;
        initialize_gdp();

        let hwnd = create_window()?;

        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_ALL_SESSIONS)?;

        let redraw_handle = RedrawHandle::new(hwnd);
        let audio_manager = AudioManager::new(redraw_handle)?;

        WINDOW_STATE.set(Some(Mutex::new(WindowManager {
            audio: AudioManager::new(redraw_handle)?,
        })));

        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(30));

                redraw_handle.redraw();
            }
        });

        let callback = DeviceNotificationCallback {
            redraw_handle: redraw_handle.clone(),
        };
        let callback: IMMNotificationClient = callback.into();
        audio_manager
            .device_enumerator
            .RegisterEndpointNotificationCallback(&callback)?;

        redraw_handle.redraw();

        let mut message = MSG::default();

        while GetMessageA(&mut message, None, 0, 0).into() {
            DispatchMessageA(&message);
        }

        audio_manager
            .device_enumerator
            .UnregisterEndpointNotificationCallback(&callback)?;

        let state = WINDOW_STATE.take().context("state missing")?;
        let state = state.into_inner().unwrap();

        for (_, device) in state.audio.devices {
            device
                .controls
                .UnregisterControlChangeNotify(&state.audio.controls_callback)?;

            DestroyIcon(device.icon)?;
        }

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
