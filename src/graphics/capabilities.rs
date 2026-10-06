//! Optional capabilities must describe the active API and callable entries.
//! Vertex instancing says nothing about framebuffer copies or their formats.
use crate::native::gl;

fn blit_api(version: &str, extensions: &str, entries_loaded: bool) -> bool {
    if !entries_loaded {
        return false;
    }
    let (version, es) = if let Some(version) = version.strip_prefix("OpenGL ES ") {
        (version, true)
    } else {
        (version, false)
    };
    let Some(major) = version.split('.').next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    major >= 3 || (!es && (1..3).contains(&major) && extensions.split_whitespace().any(|name| name == "GL_ARB_framebuffer_object"))
}

pub(super) unsafe fn framebuffer_blit() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        return false;
    } // The bundled JS creates WebGL1 and has no blit entry.
    #[cfg(not(target_arch = "wasm32"))]
    unsafe {
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        let loaded = [
            "glGetString",
            "glGetIntegerv",
            "glBindFramebuffer",
            "glBlitFramebuffer",
            "glCheckFramebufferStatus",
            "glGetFramebufferAttachmentParameteriv",
        ]
        .iter()
        .all(|name| gl::is_function_loaded(name));
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let loaded = true; // Core entries are linked from the system GL library.
        if !loaded {
            return false;
        }
        let version = gl::glGetString(gl::GL_VERSION);
        if version.is_null() {
            return false;
        }
        let version = std::ffi::CStr::from_ptr(version as _).to_string_lossy();
        if blit_api(&version, "", loaded) {
            return true;
        }
        // GL2 extension queries are legal; avoid GL_EXTENSIONS on GL3 core.
        if version.starts_with("2.") || version.starts_with("1.") {
            let extensions = gl::glGetString(0x1F03);
            if !extensions.is_null() {
                return blit_api(&version, &std::ffi::CStr::from_ptr(extensions as _).to_string_lossy(), loaded);
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::blit_api;
    #[test]
    fn core_version_and_entries_are_both_required() {
        for version in ["OpenGL ES 3.0 Vendor", "OpenGL ES 3.2 Vendor", "3.2 Core", "4.6.0 Vendor"] {
            assert!(blit_api(version, "", true));
            assert!(!blit_api(version, "", false));
        }
    }
    #[test]
    fn old_apis_require_exact_framebuffer_extension() {
        assert!(blit_api("2.1 Vendor", "GL_ARB_framebuffer_object", true));
        assert!(blit_api("1.5 Vendor", "GL_ARB_framebuffer_object", true));
        for extension in [
            "",
            "GL_ARB_framebuffer_object_suffix",
            "GL_EXT_framebuffer_object",
            "GL_ANGLE_instanced_arrays",
        ] {
            assert!(!blit_api("2.1 Vendor", extension, true));
        }
        assert!(!blit_api("OpenGL ES 2.0 Vendor", "GL_ANGLE_instanced_arrays", true));
    }
    #[test]
    fn unknown_and_unimplemented_apis_keep_the_compatible_path() {
        for version in ["", "garbage", "OpenGL ES-CM 1.1", "WebGL 1.0", "WebGL 2.0"] {
            assert!(!blit_api(version, "GL_ARB_framebuffer_object", true));
        }
    }
}
