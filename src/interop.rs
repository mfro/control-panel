#![allow(non_snake_case, non_camel_case_types)]

use windows::Win32::{
    Foundation::{HINSTANCE, HWND},
    UI::WindowsAndMessaging::{HMENU, WINDOW_EX_STYLE, WINDOW_STYLE},
};
use windows_core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, interface};

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

// 870AF99C-171D-4F9E-AF0D-E63DF40C2BC9
#[allow(non_upper_case_globals)]
pub const CLSID_PolicyConfigClient: GUID = GUID {
    data1: 0x870AF99C,
    data2: 0x171D,
    data3: 0x4F9E,
    data4: [0xAF, 0x0D, 0xE6, 0x3D, 0xF4, 0x0C, 0x2B, 0xC9],
};

#[interface("F8679F50-850A-41CF-9C72-430F290290C8")]
pub unsafe trait IPolicyConfig: IUnknown {
    pub fn GetMixFormat(&self) -> HRESULT;
    pub fn GetDeviceFormat(&self) -> HRESULT;
    pub fn ResetDeviceFormat(&self) -> HRESULT;
    pub fn SetDeviceFormat(&self) -> HRESULT;
    pub fn GetProcessingPeriod(&self) -> HRESULT;
    pub fn SetProcessingPeriod(&self) -> HRESULT;
    pub fn GetShareMode(&self) -> HRESULT;
    pub fn SetShareMode(&self) -> HRESULT;
    pub fn GetPropertyValue(&self) -> HRESULT;
    pub fn SetPropertyValue(&self) -> HRESULT;
    pub fn SetDefaultEndpoint(&self, deviceID: *const u16, role: u32) -> HRESULT;
    pub fn SetEndpointVisibility(&self) -> HRESULT;
}
