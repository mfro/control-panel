use std::io::Read;

use anyhow::Result;
use rouille::{Response, ResponseBody};
use windows::Win32::{
    Foundation::{HANDLE, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_TEXT,
    },
};

fn set_clipboard(hwnd: X, content: &[u8]) -> Result<()> {
    unsafe {
        OpenClipboard(Some(hwnd.0))?;
        EmptyClipboard()?;

        let memory = GlobalAlloc(GMEM_MOVEABLE, content.len() + 1)?;

        let pointer = GlobalLock(memory) as *mut u8;
        let slice = std::slice::from_raw_parts_mut(pointer, content.len());
        slice.copy_from_slice(content);
        *pointer.add(content.len()) = 0;
        let _ = GlobalUnlock(memory);

        SetClipboardData(CF_TEXT.0 as _, Some(HANDLE(memory.0)))?;

        CloseClipboard()?;
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct X(HWND);
unsafe impl Send for X {}
unsafe impl Sync for X {}

fn run(hwnd: X) {
    let addr = ("0.0.0.0", 25562);
    rouille::start_server(addr, move |request| {
        if request.url() == "/clip" {
            if let Some(mut body) = request.data() {
                let len = match request.header("content-length").map(str::parse).transpose() {
                    Ok(v) => v.unwrap_or_default(),
                    Err(e) => {
                        eprintln!("{e:?}");
                        return Response::empty_400();
                    }
                };

                let mut content = Vec::with_capacity(len);
                if let Err(e) = body.read_to_end(&mut content) {
                    eprintln!("{e:?}");
                    return Response::empty_400();
                }

                if let Err(e) = set_clipboard(hwnd, &content) {
                    eprintln!("{e:?}");
                    return Response {
                        status_code: 500,
                        headers: vec![],
                        data: ResponseBody::empty(),
                        upgrade: None,
                    };
                }

                Response {
                    status_code: 200,
                    headers: vec![],
                    data: ResponseBody::empty(),
                    upgrade: None,
                }
            } else {
                Response::empty_400()
            }
        } else {
            Response::empty_404()
        }
    });
}

pub fn spawn(hwnd: HWND) {
    let hwnd = X(hwnd);

    std::thread::spawn(move || {
        run(hwnd);
    });
}
