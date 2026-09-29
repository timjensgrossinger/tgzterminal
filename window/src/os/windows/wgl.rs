use super::*;
use glium::backend::Backend;
use std::ffi::CStr;
use std::io::Error as IoError;
use std::os::raw::c_void;
use std::ptr::{null, null_mut};
use winapi::shared::windef::*;
use winapi::um::libloaderapi::{GetModuleHandleW, *};
use winapi::um::wingdi::*;
use winapi::um::winuser::*;

pub mod ffi {
    include!(concat!(env!("OUT_DIR"), "/wgl_bindings.rs"));
}
pub mod ffiextra {
    include!(concat!(env!("OUT_DIR"), "/wgl_extra_bindings.rs"));
}

struct WglWrapper {
    lib: libloading::Library,
    wgl: ffi::Wgl,
    ext: Option<ffiextra::Wgl>,
    /// true when `lib` is the bundled mesa software renderer rather
    /// than the system OpenGL stack
    is_mesa: bool,
}

type GetProcAddressFunc =
    unsafe extern "system" fn(*const std::os::raw::c_char) -> *const std::os::raw::c_void;

impl Drop for WglWrapper {
    fn drop(&mut self) {
        log::trace!("dropping WglWrapper and libloading {:?}", self.lib);
    }
}

type DescribePixelFormatFunc =
    unsafe extern "system" fn(HDC, i32, u32, *mut PIXELFORMATDESCRIPTOR) -> i32;
type SetPixelFormatFunc = unsafe extern "system" fn(HDC, i32, *const PIXELFORMATDESCRIPTOR) -> i32;

impl WglWrapper {
    /// `DescribePixelFormat` for this GL. gdi32's version dispatches into the
    /// module named opengl32.dll that the process loaded first -- the system
    /// one -- which knows nothing of the pixel formats the bundled mesa (loaded
    /// by path beside it) hands out, and fails with "The parameter is
    /// incorrect". Mesa exports its own.
    unsafe fn describe_pixel_format(
        &self,
        hdc: HDC,
        format: i32,
        pfd: &mut PIXELFORMATDESCRIPTOR,
    ) -> i32 {
        let size = std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u32;
        if self.is_mesa {
            match self
                .lib
                .get::<DescribePixelFormatFunc>(b"wglDescribePixelFormat\0")
            {
                Ok(func) => return func(hdc, format, size, pfd),
                Err(err) => log::warn!("mesa has no wglDescribePixelFormat: {err:#}"),
            }
        }
        DescribePixelFormat(hdc, format, size, pfd)
    }

    /// `SetPixelFormat` for this GL; see `describe_pixel_format`.
    unsafe fn set_pixel_format(&self, hdc: HDC, format: i32, pfd: &PIXELFORMATDESCRIPTOR) -> i32 {
        if self.is_mesa {
            match self.lib.get::<SetPixelFormatFunc>(b"wglSetPixelFormat\0") {
                Ok(func) => return func(hdc, format, pfd),
                Err(err) => log::warn!("mesa has no wglSetPixelFormat: {err:#}"),
            }
        }
        SetPixelFormat(hdc, format, pfd)
    }

    fn load() -> anyhow::Result<Self> {
        let class_name = wide_string("wezterm wgl extension probing window");
        let h_inst = unsafe { GetModuleHandleW(null()) };
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC,
            lpfnWndProc: Some(DefWindowProcW),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_inst,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        if unsafe { RegisterClassW(&class) } == 0 {
            let err = IoError::last_os_error();
            match err.raw_os_error() {
                Some(code)
                    if code == winapi::shared::winerror::ERROR_CLASS_ALREADY_EXISTS as i32 => {}
                _ => return Err(err.into()),
            }
        }

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1024,
                768,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if hwnd.is_null() {
            let err = IoError::last_os_error();
            anyhow::bail!("CreateWindowExW: {}", err);
        }

        let mut state = GlState::create_basic(WglWrapper::create()?, hwnd)?;

        unsafe {
            state.make_current();
        }

        let _ = state.wgl.as_mut().unwrap().load_ext();

        state.make_not_current();

        Ok(state.into_wrapper())
    }

    fn create() -> anyhow::Result<Self> {
        let swrast = crate::configuration::prefer_swrast();
        if swrast {
            let mesa_dir = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("mesa");
            let mesa_dir = wide_string(mesa_dir.to_str().unwrap());

            unsafe {
                AddDllDirectory(mesa_dir.as_ptr());
                SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
            }
        }

        // Load mesa by its explicit absolute path.
        // We can't rely on the DLL search path alone: the executable's
        // import table references opengl32.dll, so the system copy is
        // already loaded into the process by the time we get here, and
        // loading by name would simply return that already-loaded
        // module, which then only provides a GDI Generic OpenGL 1.1
        // context on systems without a hardware driver.
        let (lib, is_mesa) = if swrast {
            let mesa_dll = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("mesa")
                .join("opengl32.dll");
            match unsafe { libloading::Library::new(&mesa_dll) } {
                Ok(lib) => {
                    log::trace!("loaded mesa software OpenGL from {:?}", mesa_dll);
                    (lib, true)
                }
                Err(err) => {
                    log::warn!(
                        "failed to load {:?}: {:?}, falling back to the system opengl32.dll",
                        mesa_dll,
                        err
                    );
                    (
                        unsafe { libloading::Library::new("opengl32.dll") }.map_err(|e| {
                            log::error!("{:?}", e);
                            e
                        })?,
                        false,
                    )
                }
            }
        } else {
            let lib = unsafe { libloading::Library::new("opengl32.dll") }.map_err(|e| {
                log::error!("{:?}", e);
                e
            })?;
            (lib, false)
        };
        log::trace!("loaded {:?}", lib);

        let get_proc_address: libloading::Symbol<GetProcAddressFunc> =
            unsafe { lib.get(b"wglGetProcAddress\0")? };
        let wgl = ffi::Wgl::load_with(|s: &'static str| {
            let sym_name = std::ffi::CString::new(s).expect("symbol to be cstring compatible");
            if let Ok(sym) = unsafe { lib.get(sym_name.as_bytes_with_nul()) } {
                return *sym;
            }
            unsafe { get_proc_address(sym_name.as_ptr()) }
        });
        Ok(Self {
            lib,
            wgl,
            ext: None,
            is_mesa,
        })
    }

    fn load_ext(&mut self) -> anyhow::Result<()> {
        let get_proc_address: libloading::Symbol<GetProcAddressFunc> =
            unsafe { self.lib.get(b"wglGetProcAddress\0")? };

        self.ext
            .replace(ffiextra::Wgl::load_with(|s: &'static str| {
                let sym_name = std::ffi::CString::new(s).expect("symbol to be cstring compatible");
                if let Ok(sym) = unsafe { self.lib.get(sym_name.as_bytes_with_nul()) } {
                    return *sym;
                }
                unsafe { get_proc_address(sym_name.as_ptr()) }
            }));

        Ok(())
    }
}

pub struct GlState {
    wgl: Option<WglWrapper>,
    hdc: HDC,
    rc: ffi::types::HGLRC,
}

fn has_extension(extensions: &str, wanted: &str) -> bool {
    extensions.split(' ').find(|&ext| ext == wanted).is_some()
}

impl GlState {
    fn into_wrapper(mut self) -> WglWrapper {
        self.delete();
        self.wgl.take().unwrap()
    }

    pub fn is_mesa(&self) -> bool {
        self.wgl.as_ref().map(|w| w.is_mesa).unwrap_or(false)
    }

    pub fn create(window: HWND) -> anyhow::Result<Self> {
        let wgl = WglWrapper::load()?;

        if let Some(ext) = wgl.ext.as_ref() {
            let hdc = unsafe { GetDC(window) };

            fn cstr(data: *const i8) -> String {
                let data = unsafe { CStr::from_ptr(data).to_bytes().to_vec() };
                String::from_utf8(data).unwrap()
            }

            let extensions = if ext.GetExtensionsStringARB.is_loaded() {
                unsafe { cstr(ext.GetExtensionsStringARB(hdc as *const _)) }
            } else if ext.GetExtensionsStringEXT.is_loaded() {
                unsafe { cstr(ext.GetExtensionsStringEXT()) }
            } else {
                "".to_owned()
            };
            log::trace!("opengl extensions: {:?}", extensions);

            if has_extension(&extensions, "WGL_ARB_pixel_format") {
                return match Self::create_ext(wgl, extensions, hdc) {
                    Ok(state) => Ok(state),
                    Err(err) => {
                        log::warn!(
                            "failed to created extended OpenGL context \
                            ({}), fall back to basic",
                            err
                        );
                        let wgl = WglWrapper::load()?;
                        Self::create_basic(wgl, window)
                    }
                };
            }
        }

        Self::create_basic(wgl, window)
    }

    fn create_ext(wgl: WglWrapper, extensions: String, hdc: HDC) -> anyhow::Result<Self> {
        use ffiextra::*;

        let srgb_attrib = if has_extension(&extensions, "WGL_ARB_framebuffer_sRGB") {
            log::trace!("will request FRAMEBUFFER_SRGB_CAPABLE_ARB");
            Some(FRAMEBUFFER_SRGB_CAPABLE_ARB)
        } else if has_extension(&extensions, "WGL_EXT_framebuffer_sRGB") {
            log::trace!("will request FRAMEBUFFER_SRGB_CAPABLE_EXT");
            Some(FRAMEBUFFER_SRGB_CAPABLE_EXT)
        } else {
            None
        };

        // Most preferred first. Not every driver offers a 4x multisampled
        // format: the bundled mesa (llvmpipe, used for front_end = "Software"
        // and RDP) offers none, and failing here fell all the way back to a
        // basic context, which on mesa is a legacy OpenGL 3.1 that cannot run
        // our shaders. Only then give up sRGB as well.
        let mut attempts = vec![(true, true), (false, true)];
        if srgb_attrib.is_some() {
            attempts.push((false, false));
        }

        let mut format_id = 0;
        let mut last_failure = String::new();
        for (index, (multisample, srgb)) in attempts.into_iter().enumerate() {
            let mut attribs: Vec<i32> = vec![
                DRAW_TO_WINDOW_ARB as i32,
                1,
                SUPPORT_OPENGL_ARB as i32,
                1,
                DOUBLE_BUFFER_ARB as i32,
                1,
                PIXEL_TYPE_ARB as i32,
                TYPE_RGBA_ARB as i32,
                COLOR_BITS_ARB as i32,
                24,
                ALPHA_BITS_ARB as i32,
                8,
                DEPTH_BITS_ARB as i32,
                24,
                STENCIL_BITS_ARB as i32,
                8,
            ];
            if multisample {
                attribs.extend_from_slice(&[SAMPLE_BUFFERS_ARB as i32, 1, SAMPLES_ARB as i32, 4]);
            }
            if let (true, Some(attrib)) = (srgb, srgb_attrib) {
                attribs.extend_from_slice(&[attrib as i32, 1]);
            }
            attribs.push(0);

            let mut num_formats = 0;
            let res = unsafe {
                wgl.ext.as_ref().unwrap().ChoosePixelFormatARB(
                    hdc as _,
                    attribs.as_ptr(),
                    null(),
                    1,
                    &mut format_id,
                    &mut num_formats,
                )
            };
            last_failure = if res == 0 {
                "ChoosePixelFormatARB returned 0".to_string()
            } else if num_formats == 0 {
                "ChoosePixelFormatARB returned 0 formats".to_string()
            } else {
                if index > 0 {
                    log::info!(
                        "WGL pixel format without 4x multisampling{}",
                        if srgb { "" } else { " or sRGB" }
                    );
                }
                String::new()
            };
            if last_failure.is_empty() {
                break;
            }
        }
        if !last_failure.is_empty() {
            anyhow::bail!("{last_failure}");
        }

        let mut pfd: PIXELFORMATDESCRIPTOR = unsafe { std::mem::zeroed() };

        let res = unsafe { wgl.describe_pixel_format(hdc, format_id, &mut pfd) };
        if res == 0 {
            anyhow::bail!(
                "DescribePixelFormat function failed: {}",
                std::io::Error::last_os_error()
            );
        }

        let res = unsafe { wgl.set_pixel_format(hdc, format_id, &pfd) };
        if res == 0 {
            anyhow::bail!(
                "SetPixelFormat function failed: {}",
                std::io::Error::last_os_error()
            );
        }

        // 4.5 core first, as ever. A driver that tops out lower refuses it
        // outright -- the bundled mesa 20.1's llvmpipe does -- so also ask
        // for 3.3 core, the version our shaders are written against, and
        // only then without robustness, which not every driver supports.
        let robustness = has_extension(&extensions, "WGL_ARB_create_context_robustness");
        let mut attempts = vec![];
        for version in [(4, 5), (3, 3)] {
            if robustness {
                attempts.push((version, true));
            }
            attempts.push((version, false));
        }

        let mut rc = null();
        for (index, ((major, minor), robust)) in attempts.into_iter().enumerate() {
            let mut attribs = vec![
                CONTEXT_MAJOR_VERSION_ARB as i32,
                major,
                CONTEXT_MINOR_VERSION_ARB as i32,
                minor,
                CONTEXT_PROFILE_MASK_ARB as i32,
                CONTEXT_CORE_PROFILE_BIT_ARB as i32,
            ];
            if robust {
                log::trace!("requesting robustness features");
                attribs.push(CONTEXT_RESET_NOTIFICATION_STRATEGY_ARB as i32);
                attribs.push(LOSE_CONTEXT_ON_RESET_ARB as i32);
                attribs.push(CONTEXT_FLAGS_ARB as i32);
                attribs.push(CONTEXT_ROBUST_ACCESS_BIT_ARB as i32);
            }
            attribs.push(0);

            rc = unsafe {
                wgl.ext.as_ref().unwrap().CreateContextAttribsARB(
                    hdc as _,
                    null(),
                    attribs.as_ptr(),
                )
            };
            if !rc.is_null() {
                if index > 0 {
                    log::info!(
                        "WGL context: OpenGL {major}.{minor} core{}",
                        if robust { "" } else { " without robustness" }
                    );
                }
                break;
            }
        }

        if rc.is_null() {
            let err = unsafe { winapi::um::errhandlingapi::GetLastError() };
            anyhow::bail!(
                "CreateContextAttribsARB failed, GetLastError={} {:x}",
                err,
                err
            );
        }

        unsafe {
            wgl.wgl.MakeCurrent(hdc as *mut _, rc);
        }

        Ok(Self {
            wgl: Some(wgl),
            rc,
            hdc,
        })
    }

    fn create_basic(wgl: WglWrapper, window: HWND) -> anyhow::Result<Self> {
        let hdc = unsafe { GetDC(window) };

        let pfd = PIXELFORMATDESCRIPTOR {
            nSize: std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16,
            nVersion: 1,
            dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            iPixelType: PFD_TYPE_RGBA,
            cColorBits: 24,
            cRedBits: 0,
            cRedShift: 0,
            cGreenBits: 0,
            cGreenShift: 0,
            cBlueBits: 0,
            cBlueShift: 0,
            cAlphaBits: 8,
            cAlphaShift: 0,
            cAccumBits: 0,
            cAccumRedBits: 0,
            cAccumGreenBits: 0,
            cAccumBlueBits: 0,
            cAccumAlphaBits: 0,
            cDepthBits: 24,
            cStencilBits: 8,
            cAuxBuffers: 0,
            iLayerType: PFD_MAIN_PLANE,
            bReserved: 0,
            dwLayerMask: 0,
            dwVisibleMask: 0,
            dwDamageMask: 0,
        };
        let format = unsafe { ChoosePixelFormat(hdc, &pfd) };
        unsafe {
            SetPixelFormat(hdc, format, &pfd);
        }

        let rc = unsafe { wgl.wgl.CreateContext(hdc as *mut _) };
        unsafe {
            wgl.wgl.MakeCurrent(hdc as *mut _, rc);
        }

        Ok(Self {
            wgl: Some(wgl),
            rc,
            hdc,
        })
    }

    fn make_not_current(&self) {
        if let Some(wgl) = self.wgl.as_ref() {
            unsafe {
                wgl.wgl.MakeCurrent(self.hdc as *mut _, std::ptr::null());
            }
        }
    }

    fn delete(&mut self) {
        self.make_not_current();
        if let Some(wgl) = self.wgl.as_ref() {
            unsafe {
                wgl.wgl.DeleteContext(self.rc);
            }
        }
    }
}

impl Drop for GlState {
    fn drop(&mut self) {
        self.delete();
    }
}

unsafe impl glium::backend::Backend for GlState {
    fn resize(&self, _: (u32, u32)) {
        todo!()
    }

    fn swap_buffers(&self) -> Result<(), glium::SwapBuffersError> {
        unsafe {
            // The mesa software renderer doesn't support the plain
            // gdi32 SwapBuffers path; it exposes the WGL swap function
            // instead, which is the reliable way to present on that
            // stack. For the system OpenGL stack keep the original
            // SwapBuffers behavior.
            if self.is_mesa() {
                if let Some(wgl) = self.wgl.as_ref() {
                    if let Some(ext) = wgl.ext.as_ref() {
                        if ext.SwapLayerBuffers.is_loaded() {
                            // WGL_SWAP_MAIN_PLANE == 1
                            if ext.SwapLayerBuffers(self.hdc as _, 1) != 0 {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            SwapBuffers(self.hdc);
        }
        Ok(())
    }

    unsafe fn get_proc_address(&self, symbol: &str) -> *const c_void {
        let sym_name = std::ffi::CString::new(symbol).expect("symbol to be cstring compatible");
        if let Ok(sym) = self
            .wgl
            .as_ref()
            .unwrap()
            .lib
            .get(sym_name.as_bytes_with_nul())
        {
            //eprintln!("{} -> {:?}", symbol, sym);
            return *sym;
        }
        let res = self
            .wgl
            .as_ref()
            .unwrap()
            .wgl
            .GetProcAddress(sym_name.as_ptr()) as *const c_void;
        // eprintln!("{} -> {:?}", symbol, res);
        res
    }

    fn get_framebuffer_dimensions(&self) -> (u32, u32) {
        unimplemented!();
    }

    fn is_current(&self) -> bool {
        unsafe { self.wgl.as_ref().unwrap().wgl.GetCurrentContext() == self.rc }
    }

    unsafe fn make_current(&self) {
        self.wgl
            .as_ref()
            .unwrap()
            .wgl
            .MakeCurrent(self.hdc as *mut _, self.rc);
    }
}
