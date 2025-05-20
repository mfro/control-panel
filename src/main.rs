#![windows_subsystem = "windows"]

use std::{
    cell::RefCell,
    ffi::CString,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use windows::{
    self,
    Win32::{
        Devices::FunctionDiscovery::PKEY_DeviceClass_IconPath,
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Gdi::{
                BeginPaint, CreateSolidBrush, EndPaint, FillRect, PAINTSTRUCT, RDW_INVALIDATE,
                RedrawWindow,
            },
            GdiPlus::{
                GdipCreateFromHDC, GdipCreatePen1, GdipDeleteGraphics, GdipDrawLine,
                GdipSetPenEndCap, GdipSetPenStartCap, GdiplusStartup, GdiplusStartupInput,
                LineCapTriangle, UnitPixel,
            },
        },
        Media::Audio::{
            DEVICE_STATE_ACTIVE, EDataFlow, ERole,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDevice, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
            MMDeviceEnumerator, eAll, eCapture, eMultimedia, eRender,
        },
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoInitialize, STGM_READ},
            LibraryLoader::GetModuleHandleA,
        },
        UI::{
            Shell::ExtractIconExA,
            WindowsAndMessaging::{
                DefWindowProcA, DestroyWindow, DispatchMessageA, DrawIcon, GWL_EXSTYLE, GWL_STYLE,
                GetClientRect, GetMessageA, HICON, HMENU, HWND_TOPMOST, LWA_ALPHA, MSG,
                PostQuitMessage, RegisterClassA, SWP_NOMOVE, SWP_NOSIZE, SendMessageA,
                SetLayeredWindowAttributes, SetWindowLongA, SetWindowPos, WINDOW_EX_STYLE,
                WINDOW_STYLE, WM_DESTROY, WM_KILLFOCUS, WM_PAINT, WM_QUIT, WNDCLASSA,
                WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
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

unsafe impl Sync for RedrawHandle {}
unsafe impl Send for RedrawHandle {}

#[derive(Clone, Copy)]
struct RedrawHandle {
    hwnd: HWND,
}

impl RedrawHandle {
    fn redraw(&self) {
        unsafe {
            SendMessageA(self.hwnd, WM_KILLFOCUS, default(), default());
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE);
        }
    }
}

struct State {
    audio_helper: IMMDeviceEnumerator,
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

fn get_device_info(device: &IMMDevice) -> Result<DeviceInfo> {
    unsafe {
        let props = device.OpenPropertyStore(STGM_READ)?;

        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let mute = volume.GetMute()?.into();

        let icon_path = props.GetValue(&PKEY_DeviceClass_IconPath)?.to_string();

        Ok(DeviceInfo { icon_path, mute })
    }
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

            WM_PAINT => {
                WINDOW_STATE.with(|state| {
                    if let Some(state) = state.borrow_mut().as_mut() {
                        let state = state.lock().unwrap();

                        let device = state
                            .audio_helper
                            .GetDefaultAudioEndpoint(eRender, eMultimedia)
                            .unwrap();
                        let output_device = get_device_info(&device).unwrap();
                        let output_icon = get_icon(&output_device.icon_path).unwrap();

                        let device = state
                            .audio_helper
                            .GetDefaultAudioEndpoint(eCapture, eMultimedia)
                            .unwrap();
                        let input_device = get_device_info(&device).unwrap();
                        let input_icon = get_icon(&input_device.icon_path).unwrap();

                        let mut paint = PAINTSTRUCT::default();
                        let mut client_rect = RECT::default();
                        GetClientRect(hwnd, &mut client_rect).unwrap();

                        let hdc = BeginPaint(hwnd, &mut paint);

                        let hbrush = CreateSolidBrush(COLORREF(0xffffff));
                        FillRect(hdc, &client_rect, hbrush);

                        DrawIcon(hdc, 8, 8, output_icon).unwrap();
                        DrawIcon(hdc, 48, 8, input_icon).unwrap();

                        let mut graphics = std::ptr::null_mut();
                        GdipCreateFromHDC(hdc, &mut graphics);

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

                        EndPaint(hwnd, &paint).ok().unwrap();
                    }
                });
            }

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

fn main() -> Result<()> {
    std::thread::sleep(Duration::from_secs(10));

    unsafe {
        CoInitialize(None).ok()?;

        let mut token = 0;
        let mut input = GdiplusStartupInput::default();
        input.GdiplusVersion = 1;
        let mut output: windows::Win32::Graphics::GdiPlus::GdiplusStartupOutput = default();
        GdiplusStartup(&mut token, &input, &mut output);

        let audio_helper: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        WINDOW_STATE.set(Some(Arc::new(Mutex::new(State {
            audio_helper: audio_helper.clone(),
        }))));

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

        println!("{:?}", hwnd);

        let redraw_handle = RedrawHandle { hwnd };

        let callback = DeviceNotificationCallback {
            redraw_handle: redraw_handle.clone(),
        };
        let _client: IMMNotificationClient = callback.into();
        audio_helper.RegisterEndpointNotificationCallback(&_client)?;

        let devices = audio_helper.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)?;

        let mut cleanup = vec![];
        for i in 0..devices.GetCount()? {
            let device = devices.Item(i)?;
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;

            let callback = VolumeCallback {
                redraw_handle: redraw_handle.clone(),
            };
            let _client: IAudioEndpointVolumeCallback = callback.into();
            volume.RegisterControlChangeNotify(&_client)?;

            cleanup.push(move || volume.UnregisterControlChangeNotify(&_client));
        }

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

        audio_helper.UnregisterEndpointNotificationCallback(&_client)?;
        for cleanup in cleanup {
            cleanup()?;
        }
        DestroyWindow(hwnd)?;
    }

    Ok(())
}
