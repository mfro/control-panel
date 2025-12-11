#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell, collections::HashMap, ffi::CString, ptr::null_mut, str::FromStr, sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use windows::{
    self,
    Win32::{
        Devices::FunctionDiscovery::{PKEY_Device_FriendlyName, PKEY_DeviceClass_IconPath},
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
        Graphics::{
            Gdi::{
                AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleBitmap,
                CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, InvalidateRect, SelectObject,
            },
            GdiPlus::{
                GdipCreateFromHDC, GdipCreatePen1, GdipDrawLine, GdipSetPenEndCap,
                GdipSetPenStartCap, GdiplusStartup, GdiplusStartupInput, LineCapTriangle,
                UnitPixel,
            },
        },
        Media::{
            Audio::{
                EDataFlow, ERole,
                Endpoints::{
                    IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                    IAudioEndpointVolumeCallback_Impl,
                },
                IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
                IMMNotificationClient_Impl, MMDeviceEnumerator, eCapture, eMultimedia, eRender,
            },
            KernelStreaming::{
                IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSPROPERTY_ONESHOT_RECONNECT,
                KSPROPERTY_TYPE_GET, KSPROPSETID_BtAudio,
            },
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
                GetMessageA, GetWindowRect, HICON, HMENU, HWND_DESKTOP, HWND_TOPMOST, IDC_ARROW,
                LoadCursorW, MSG, PostQuitMessage, RegisterClassA, SWP_NOMOVE, SWP_NOSIZE,
                SendMessageA, SetWindowPos, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE,
                WINDOW_STYLE, WM_DESTROY, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_PAINT, WM_QUIT,
                WM_WINDOWPOSCHANGING, WM_WTSSESSION_CHANGE, WNDCLASSA, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WTS_SESSION_LOCK,
                WTS_SESSION_UNLOCK,
            },
        },
    },
    core::implement,
};
use windows_core::{PCSTR, PCWSTR, s, w};

const AIRPODS_AUDIO_DEVICE: PCWSTR = w!("{0.0.0.00000000}.{2b32d6ca-aea8-4697-b828-64b4cd31efcb}");

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

    device_callback: IMMNotificationClient,
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

            let callback = DeviceCallback { redraw_handle };
            let device_callback = callback.into();
            device_enumerator.RegisterEndpointNotificationCallback(&device_callback)?;

            let controls_callback = VolumeCallback { redraw_handle };
            let controls_callback = controls_callback.into();

            Ok(Self {
                device_enumerator,
                controls_callback,
                device_callback,
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
                let icon = load_icon(&icon_path).context(icon_path)?;

                log!("start tracking device: {} {}", id, name);

                let controls: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
                controls.RegisterControlChangeNotify(&self.controls_callback)?;

                let info = AudioDevice::new(controls, icon);
                self.devices.insert(id.clone(), info);
            }

            Ok(&self.devices[&id])
        }
    }

    pub fn destroy(self) -> Result<()> {
        unsafe {
            self.device_enumerator
                .UnregisterEndpointNotificationCallback(&self.device_callback)?;

            for (_, device) in self.devices {
                device
                    .controls
                    .UnregisterControlChangeNotify(&self.controls_callback)?;

                DestroyIcon(device.icon)?;
            }
        }

        Ok(())
    }
}

fn load_icon(icon_path: &str) -> Result<HICON> {
    unsafe {
        let mut parts = icon_path.split(",");
        let path = parts.next().context("invalid icon path")?;

        let index = match parts.next() {
            Some(i) => i.parse()?,
            None => 0,
        };

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

struct WindowHelper {
    audio: AudioManager,
}

impl WindowHelper {
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

    fn connect_airpods(&mut self) -> Result<()> {
        unsafe {
            let airpods_audio_device = self
                .audio
                .device_enumerator
                .GetDevice(AIRPODS_AUDIO_DEVICE)?;
            let topology: IDeviceTopology = airpods_audio_device.Activate(CLSCTX_ALL, None)?;
            assert_eq!(1, topology.GetConnectorCount()?);
            let connector = topology.GetConnector(0)?;
            let airpods_bluetooth_device = connector.GetDeviceIdConnectedTo()?;
            let airpods_bluetooth_device = self
                .audio
                .device_enumerator
                .GetDevice(airpods_bluetooth_device)?;

            let control: IKsControl = airpods_bluetooth_device.Activate(CLSCTX_ALL, None)?;

            let property = KSIDENTIFIER {
                Anonymous: KSIDENTIFIER_0 {
                    Anonymous: windows::Win32::Media::KernelStreaming::KSIDENTIFIER_0_0 {
                        Set: KSPROPSETID_BtAudio,
                        Id: KSPROPERTY_ONESHOT_RECONNECT.0 as u32,
                        Flags: KSPROPERTY_TYPE_GET,
                    },
                },
            };

            let mut out = 0;
            control.KsProperty(
                &property,
                size_of_val(&property) as u32,
                null_mut(),
                0,
                &mut out,
            )?;
        }

        Ok(())
    }
}

fn wrap(function: impl FnOnce(&mut WindowHelper) -> Result<()>) {
    WINDOW_HELPER.with(|state| {
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

fn message_name(event: u32) -> &'static str {
    match event {
        0x0000 => "WM_NULL",
        0x0001 => "WM_CREATE",
        0x0002 => "WM_DESTROY",
        0x0003 => "WM_MOVE",
        0x0005 => "WM_SIZE",
        0x0006 => "WM_ACTIVATE",
        0x0007 => "WM_SETFOCUS",
        0x0008 => "WM_KILLFOCUS",
        0x000A => "WM_ENABLE",
        0x000B => "WM_SETREDRAW",
        0x000C => "WM_SETTEXT",
        0x000D => "WM_GETTEXT",
        0x000E => "WM_GETTEXTLENGTH",
        0x000F => "WM_PAINT",
        0x0010 => "WM_CLOSE",
        0x0011 => "WM_QUERYENDSESSION",
        0x0012 => "WM_QUIT",
        0x0013 => "WM_QUERYOPEN",
        0x0014 => "WM_ERASEBKGND",
        0x0015 => "WM_SYSCOLORCHANGE",
        0x0016 => "WM_ENDSESSION",
        0x0017 => "WM_SYSTEMERROR",
        0x0018 => "WM_SHOWWINDOW",
        0x0019 => "WM_CTLCOLOR",
        0x001A => "WM_SETTINGCHANGE",
        0x001B => "WM_DEVMODECHANGE",
        0x001C => "WM_ACTIVATEAPP",
        0x001D => "WM_FONTCHANGE",
        0x001E => "WM_TIMECHANGE",
        0x001F => "WM_CANCELMODE",
        0x0020 => "WM_SETCURSOR",
        0x0021 => "WM_MOUSEACTIVATE",
        0x0022 => "WM_CHILDACTIVATE",
        0x0023 => "WM_QUEUESYNC",
        0x0024 => "WM_GETMINMAXINFO",
        0x0026 => "WM_PAINTICON",
        0x0027 => "WM_ICONERASEBKGND",
        0x0028 => "WM_NEXTDLGCTL",
        0x002A => "WM_SPOOLERSTATUS",
        0x002B => "WM_DRAWITEM",
        0x002C => "WM_MEASUREITEM",
        0x002D => "WM_DELETEITEM",
        0x002E => "WM_VKEYTOITEM",
        0x002F => "WM_CHARTOITEM",
        0x0030 => "WM_SETFONT",
        0x0031 => "WM_GETFONT",
        0x0032 => "WM_SETHOTKEY",
        0x0033 => "WM_GETHOTKEY",
        0x0037 => "WM_QUERYDRAGICON",
        0x0039 => "WM_COMPAREITEM",
        0x0041 => "WM_COMPACTING",
        0x0046 => "WM_WINDOWPOSCHANGING",
        0x0047 => "WM_WINDOWPOSCHANGED",
        0x0048 => "WM_POWER",
        0x004A => "WM_COPYDATA",
        0x004B => "WM_CANCELJOURNAL",
        0x004E => "WM_NOTIFY",
        0x0050 => "WM_INPUTLANGCHANGEREQUEST",
        0x0051 => "WM_INPUTLANGCHANGE",
        0x0052 => "WM_TCARD",
        0x0053 => "WM_HELP",
        0x0054 => "WM_USERCHANGED",
        0x0055 => "WM_NOTIFYFORMAT",
        0x007B => "WM_CONTEXTMENU",
        0x007C => "WM_STYLECHANGING",
        0x007D => "WM_STYLECHANGED",
        0x007E => "WM_DISPLAYCHANGE",
        0x007F => "WM_GETICON",
        0x0080 => "WM_SETICON",
        0x0081 => "WM_NCCREATE",
        0x0082 => "WM_NCDESTROY",
        0x0083 => "WM_NCCALCSIZE",
        0x0084 => "WM_NCHITTEST",
        0x0085 => "WM_NCPAINT",
        0x0086 => "WM_NCACTIVATE",
        0x0087 => "WM_GETDLGCODE",
        0x00A0 => "WM_NCMOUSEMOVE",
        0x00A1 => "WM_NCLBUTTONDOWN",
        0x00A2 => "WM_NCLBUTTONUP",
        0x00A3 => "WM_NCLBUTTONDBLCLK",
        0x00A4 => "WM_NCRBUTTONDOWN",
        0x00A5 => "WM_NCRBUTTONUP",
        0x00A6 => "WM_NCRBUTTONDBLCLK",
        0x00A7 => "WM_NCMBUTTONDOWN",
        0x00A8 => "WM_NCMBUTTONUP",
        0x00A9 => "WM_NCMBUTTONDBLCLK",
        0x0100 => "WM_KEYDOWN",
        0x0101 => "WM_KEYUP",
        0x0102 => "WM_CHAR",
        0x0103 => "WM_DEADCHAR",
        0x0104 => "WM_SYSKEYDOWN",
        0x0105 => "WM_SYSKEYUP",
        0x0106 => "WM_SYSCHAR",
        0x0107 => "WM_SYSDEADCHAR",
        0x0108 => "WM_KEYLAST",
        0x010D => "WM_IME_STARTCOMPOSITION",
        0x010E => "WM_IME_ENDCOMPOSITION",
        0x010F => "WM_IME_COMPOSITION",
        0x0110 => "WM_INITDIALOG",
        0x0111 => "WM_COMMAND",
        0x0112 => "WM_SYSCOMMAND",
        0x0113 => "WM_TIMER",
        0x0114 => "WM_HSCROLL",
        0x0115 => "WM_VSCROLL",
        0x0116 => "WM_INITMENU",
        0x0117 => "WM_INITMENUPOPUP",
        0x011F => "WM_MENUSELECT",
        0x0120 => "WM_MENUCHAR",
        0x0121 => "WM_ENTERIDLE",
        0x0132 => "WM_CTLCOLORMSGBOX",
        0x0133 => "WM_CTLCOLOREDIT",
        0x0134 => "WM_CTLCOLORLISTBOX",
        0x0135 => "WM_CTLCOLORBTN",
        0x0136 => "WM_CTLCOLORDLG",
        0x0137 => "WM_CTLCOLORSCROLLBAR",
        0x0138 => "WM_CTLCOLORSTATIC",
        0x0200 => "WM_MOUSEMOVE",
        0x0201 => "WM_LBUTTONDOWN",
        0x0202 => "WM_LBUTTONUP",
        0x0203 => "WM_LBUTTONDBLCLK",
        0x0204 => "WM_RBUTTONDOWN",
        0x0205 => "WM_RBUTTONUP",
        0x0206 => "WM_RBUTTONDBLCLK",
        0x0207 => "WM_MBUTTONDOWN",
        0x0208 => "WM_MBUTTONUP",
        0x0209 => "WM_MBUTTONDBLCLK",
        0x020A => "WM_MOUSEWHEEL",
        0x020E => "WM_MOUSEHWHEEL",
        0x0210 => "WM_PARENTNOTIFY",
        0x0211 => "WM_ENTERMENULOOP",
        0x0212 => "WM_EXITMENULOOP",
        0x0213 => "WM_NEXTMENU",
        0x0214 => "WM_SIZING",
        0x0215 => "WM_CAPTURECHANGED",
        0x0216 => "WM_MOVING",
        0x0218 => "WM_POWERBROADCAST",
        0x0219 => "WM_DEVICECHANGE",
        0x0220 => "WM_MDICREATE",
        0x0221 => "WM_MDIDESTROY",
        0x0222 => "WM_MDIACTIVATE",
        0x0223 => "WM_MDIRESTORE",
        0x0224 => "WM_MDINEXT",
        0x0225 => "WM_MDIMAXIMIZE",
        0x0226 => "WM_MDITILE",
        0x0227 => "WM_MDICASCADE",
        0x0228 => "WM_MDIICONARRANGE",
        0x0229 => "WM_MDIGETACTIVE",
        0x0230 => "WM_MDISETMENU",
        0x0231 => "WM_ENTERSIZEMOVE",
        0x0232 => "WM_EXITSIZEMOVE",
        0x0233 => "WM_DROPFILES",
        0x0234 => "WM_MDIREFRESHMENU",
        0x0281 => "WM_IME_SETCONTEXT",
        0x0282 => "WM_IME_NOTIFY",
        0x0283 => "WM_IME_CONTROL",
        0x0284 => "WM_IME_COMPOSITIONFULL",
        0x0285 => "WM_IME_SELECT",
        0x0286 => "WM_IME_CHAR",
        0x0290 => "WM_IME_KEYDOWN",
        0x0291 => "WM_IME_KEYUP",
        0x02A1 => "WM_MOUSEHOVER",
        0x02A2 => "WM_NCMOUSELEAVE",
        0x02A3 => "WM_MOUSELEAVE",
        0x0300 => "WM_CUT",
        0x0301 => "WM_COPY",
        0x0302 => "WM_PASTE",
        0x0303 => "WM_CLEAR",
        0x0304 => "WM_UNDO",
        0x0305 => "WM_RENDERFORMAT",
        0x0306 => "WM_RENDERALLFORMATS",
        0x0307 => "WM_DESTROYCLIPBOARD",
        0x0308 => "WM_DRAWCLIPBOARD",
        0x0309 => "WM_PAINTCLIPBOARD",
        0x030A => "WM_VSCROLLCLIPBOARD",
        0x030B => "WM_SIZECLIPBOARD",
        0x030C => "WM_ASKCBFORMATNAME",
        0x030D => "WM_CHANGECBCHAIN",
        0x030E => "WM_HSCROLLCLIPBOARD",
        0x030F => "WM_QUERYNEWPALETTE",
        0x0310 => "WM_PALETTEISCHANGING",
        0x0311 => "WM_PALETTECHANGED",
        0x0312 => "WM_HOTKEY",
        0x0317 => "WM_PRINT",
        0x0318 => "WM_PRINTCLIENT",
        0x0358 => "WM_HANDHELDFIRST",
        0x035F => "WM_HANDHELDLAST",
        0x0380 => "WM_PENWINFIRST",
        0x038F => "WM_PENWINLAST",
        0x0390 => "WM_COALESCE_FIRST",
        0x039F => "WM_COALESCE_LAST",
        0x03E0 => "WM_DDE_INITIATE",
        0x03E1 => "WM_DDE_TERMINATE",
        0x03E2 => "WM_DDE_ADVISE",
        0x03E3 => "WM_DDE_UNADVISE",
        0x03E4 => "WM_DDE_ACK",
        0x03E5 => "WM_DDE_DATA",
        0x03E6 => "WM_DDE_REQUEST",
        0x03E7 => "WM_DDE_POKE",
        0x03E8 => "WM_DDE_EXECUTE",
        0x0400 => "WM_USER",
        0x8000 => "WM_APP",
        _ => "unknown",
    }
}

thread_local! {
    static WINDOW_HELPER: RefCell<Option<Mutex<WindowHelper>>> = RefCell::new(None);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    event: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match event {
            WM_WINDOWPOSCHANGING => {}

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

            WM_LBUTTONDOWN => {
                wrap(|state| state.connect_airpods());
            }

            _ => {
                #[cfg(debug_assertions)]
                println!("event: {:x} {}", event, message_name(event));
            }
        }

        DefWindowProcA(hwnd, event, wparam, lparam)
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback {
    redraw_handle: RedrawHandle,
}

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
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

fn initialize_gdip() {
    let mut token = 0;
    let mut input = GdiplusStartupInput::default();
    input.GdiplusVersion = 1;
    let mut output = default();
    unsafe { GdiplusStartup(&mut token, &input, &mut output) };
}

fn create_window() -> Result<HWND> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleA(None)?.into();

        let window_class_name = s!("mfro window class");

        let mut wc = WNDCLASSA::default();
        wc.hInstance = hinstance;
        wc.lpfnWndProc = Some(window_proc);
        wc.lpszClassName = window_class_name;
        wc.hCursor = LoadCursorW(None, IDC_ARROW)?;

        if 0 == RegisterClassA(&wc) {
            bail!("failed to register window class")
        }

        let hwnd = CreateWindowExA(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            window_class_name,
            s!("mfro window name"),
            WS_POPUP | WS_VISIBLE,
            -800,
            1440 - 48,
            400,
            48,
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
        initialize_gdip();

        let hwnd = create_window()?;

        // register for WM_WTSSESSION_CHANGE events
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_ALL_SESSIONS)?;

        let redraw_handle = RedrawHandle::new(hwnd);
        let audio_manager = AudioManager::new(redraw_handle)?;

        WINDOW_HELPER.set(Some(Mutex::new(WindowHelper {
            audio: audio_manager,
        })));

        std::thread::spawn(move || {
            loop {
                redraw_handle.redraw();

                std::thread::sleep(Duration::from_secs(30));
            }
        });

        let mut message = MSG::default();

        while GetMessageA(&mut message, None, 0, 0).into() {
            DispatchMessageA(&message);
        }

        let state = WINDOW_HELPER.take().context("state missing")?;
        let state = state.into_inner().unwrap();

        state.audio.destroy()?;

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
