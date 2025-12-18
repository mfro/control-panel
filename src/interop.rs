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

#[allow(non_upper_case_globals)]

pub const CLSID_PolicyConfigClient: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

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
