//! Finite loader-backed checks of the real constructor's cleanup contract; no renderer.
use super::*;
use std::sync::Mutex;

#[derive(Clone)]
struct State {
    active: u32,
    binding: u32,
    unpack: [i32; 5],
    error: u32,
    storage_error: u32,
    upload_error: u32,
    setup_error: u32,
    restore_error: u32,
    delete_error: u32,
    version: &'static [u8],
    profile: i32,
    maximum: i32,
    calls: Vec<&'static str>,
    uploaded: Vec<u8>,
    tight_upload: bool,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);
fn with_state<T>(action: impl FnOnce(&mut State) -> T) -> T {
    action(STATE.lock().unwrap().as_mut().unwrap())
}
fn initial() -> State {
    State {
        active: GL_TEXTURE0 + 3,
        binding: 17,
        unpack: [29, 8, 13, 2, 3],
        error: 0,
        storage_error: 0,
        upload_error: 0,
        setup_error: 0,
        restore_error: 0,
        delete_error: 0,
        version: b"OpenGL ES 3.2 Vendor\0",
        profile: 1,
        maximum: 4096,
        calls: vec![],
        uploaded: vec![],
        tight_upload: false,
    }
}
extern "C" fn get_error() -> u32 {
    with_state(|s| std::mem::replace(&mut s.error, 0))
}
extern "C" fn get_string(_: u32) -> *const u8 {
    with_state(|s| s.version.as_ptr())
}
extern "C" fn get_integer(parameter: u32, result: *mut i32) {
    let value = with_state(|s| match parameter {
        0x84e0 => s.active as i32,
        0x8069 => s.binding as i32,
        0x88ef => s.unpack[0],
        GL_UNPACK_ALIGNMENT => s.unpack[1],
        0x0cf2 => s.unpack[2],
        0x0cf3 => s.unpack[3],
        0x0cf4 => s.unpack[4],
        0x9126 => s.profile,
        GL_MAX_TEXTURE_SIZE => s.maximum,
        _ => 0,
    });
    unsafe {
        *result = value;
    }
}
extern "C" fn active(unit: u32) {
    with_state(|s| s.active = unit);
}
extern "C" fn bind_texture(_: u32, name: u32) {
    with_state(|s| {
        s.binding = name;
        s.calls.push(if name == 17 {
            "restore texture"
        } else {
            "bind candidate"
        });
    });
}
extern "C" fn bind_buffer(_: u32, name: u32) {
    with_state(|s| {
        s.unpack[0] = name as i32;
        if name == 0 {
            s.error = s.setup_error;
        }
        if name == 29 {
            s.calls.push("restore unpack");
        }
    });
}
extern "C" fn pixel_store(parameter: u32, value: i32) {
    with_state(|s| {
        let index = match parameter {
            GL_UNPACK_ALIGNMENT => 1,
            0x0cf2 => 2,
            0x0cf3 => 3,
            0x0cf4 => 4,
            _ => unreachable!(),
        };
        s.unpack[index] = value;
        if parameter == GL_UNPACK_ALIGNMENT && value == 8 {
            s.error = s.restore_error;
        }
    });
}
extern "C" fn generate(_: i32, name: *mut u32) {
    unsafe {
        *name = 41;
    }
}
extern "C" fn bind_vao(_: u32) {}
extern "C" fn storage(_: u32, levels: i32, format: u32, width: i32, height: i32) {
    with_state(|s| {
        s.calls.push("storage");
        if (levels, format, width, height) != (1, GL_RGBA8, 2, 2) {
            s.error = 0x0501;
        } else {
            s.error = s.storage_error;
        }
    });
}
extern "C" fn upload(
    _: u32,
    level: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    pixel_type: u32,
    pointer: *const GLvoid,
) {
    with_state(|s| {
        s.calls.push("upload");
        s.error = s.upload_error;
        s.tight_upload = s.unpack == [0, 1, 0, 0, 0]
            && (level, x, y, width, height, format, pixel_type)
                == (0, 0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE);
        s.uploaded = unsafe { std::slice::from_raw_parts(pointer.cast(), 16) }.to_vec();
    });
}
extern "C" fn parameter(_: u32, parameter: u32, value: i32) {
    with_state(|s| {
        s.calls.push("parameter");
        let valid = match parameter {
            GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T => value == GL_CLAMP_TO_EDGE as i32,
            GL_TEXTURE_MIN_FILTER | GL_TEXTURE_MAG_FILTER => value == GL_LINEAR as i32,
            _ => false,
        };
        if !valid {
            s.error = 0x0501;
        }
    });
}
extern "C" fn delete(_: i32, name: *const u32) {
    with_state(|s| {
        s.calls.push("delete candidate");
        if unsafe { *name } != 41 {
            s.error = 0x0501;
        } else {
            s.error = s.delete_error;
        }
    });
}
fn load(missing_storage: bool) {
    crate::native::gl::load_gl_funcs(|name| {
        let pointer = match name {
            "glGetString" => get_string as *const (),
            "glGetError" => get_error as *const (),
            "glGetIntegerv" => get_integer as *const (),
            "glActiveTexture" => active as *const (),
            "glBindTexture" => bind_texture as *const (),
            "glBindBuffer" => bind_buffer as *const (),
            "glPixelStorei" => pixel_store as *const (),
            "glGenTextures" | "glGenVertexArrays" => generate as *const (),
            "glBindVertexArray" => bind_vao as *const (),
            "glDeleteTextures" => delete as *const (),
            "glTexStorage2D" if !missing_storage => storage as *const (),
            "glTexSubImage2D" => upload as *const (),
            "glTexParameteri" => parameter as *const (),
            _ => return None,
        };
        Some(unsafe { std::mem::transmute::<*const (), unsafe extern "C" fn()>(pointer) })
    });
}
struct ResetLoader;
impl Drop for ResetLoader {
    fn drop(&mut self) {
        crate::native::gl::load_gl_funcs(|_| None);
    }
}
#[test]
fn constructor_restores_real_state_before_commit_or_candidate_cleanup() {
    let _reset = ResetLoader;
    let bytes: Vec<u8> = (0..16).collect();
    // One loader-backed test keeps the global GL entries isolated from parallel tests.
    for scenario in 0..10 {
        let mut state = initial();
        match scenario {
            1 => state.storage_error = 0x0502,
            2 => {
                state.storage_error = 0x0502;
                state.restore_error = 0x0501;
            }
            3 => {
                state.storage_error = 0x0502;
                state.delete_error = 0x0501;
            }
            4 => state.storage_error = 0x0505,
            5 => {
                state.version = b"4.2 Vendor\0";
                state.profile = 2;
            }
            6 => state.maximum = 1,
            8 => {
                state.setup_error = 0x0502;
                state.restore_error = 0x0501;
            }
            9 => state.upload_error = 0x0502,
            _ => {}
        }
        *STATE.lock().unwrap() = Some(state.clone());
        load(scenario == 7);
        let mut ctx = Context::new(false);
        ctx.cache.textures[0] = 99; // Deliberately stale: preserve actual binding, repair cache.
        ctx.cache.stored_texture = 73;
        let result = Texture::try_from_rgba8_immutable(&mut ctx, 2, 2, &bytes);
        let actual = with_state(|s| s.clone());
        assert_eq!(actual.active, state.active);
        assert_eq!(actual.binding, state.binding);
        assert_eq!(actual.unpack, state.unpack);
        assert_eq!(ctx.cache.stored_texture, 73);
        match scenario {
            0 => {
                assert!(matches!(
                    result,
                    Ok(ImmutableTextureAttempt::Ready(Texture { texture: 41, .. }))
                ));
                assert_eq!(actual.uploaded, bytes);
                assert!(actual.tight_upload);
                assert_eq!(
                    actual
                        .calls
                        .iter()
                        .filter(|&&call| call == "parameter")
                        .count(),
                    4
                );
                assert!(!actual.calls.contains(&"delete candidate"));
            }
            1 => assert!(matches!(
                result,
                Ok(ImmutableTextureAttempt::Declined(
                    ImmutableTextureDecline::Gl {
                        stage: "immutable texture storage",
                        code: 0x0502
                    }
                ))
            )),
            2 | 3 | 8 => assert!(result.unwrap_err().is_cleanup_failed()),
            9 => assert!(matches!(
                result,
                Ok(ImmutableTextureAttempt::Declined(
                    ImmutableTextureDecline::Gl {
                        stage: "immutable full RGBA8 upload",
                        code: 0x0502
                    }
                ))
            )),
            4 => assert!(matches!(
                result.unwrap_err().failure,
                ImmutableTextureFailure::Gl { code: 0x0505, .. }
            )),
            5 => assert_eq!(
                result,
                Ok(ImmutableTextureAttempt::Declined(
                    ImmutableTextureDecline::UnsupportedApi
                ))
            ),
            6 => assert!(matches!(
                result.unwrap_err().failure,
                ImmutableTextureFailure::TextureTooLarge { maximum: 1 }
            )),
            7 => assert_eq!(
                result,
                Ok(ImmutableTextureAttempt::Declined(
                    ImmutableTextureDecline::MissingEntryPoints
                ))
            ),
            _ => unreachable!(),
        }
        if scenario <= 4 || scenario == 9 {
            assert_eq!(ctx.cache.textures[0], 17);
            let unpack = actual
                .calls
                .iter()
                .position(|&call| call == "restore unpack")
                .unwrap();
            let texture = actual
                .calls
                .iter()
                .position(|&call| call == "restore texture")
                .unwrap();
            assert!(unpack < texture);
            if scenario != 0 {
                let delete = actual
                    .calls
                    .iter()
                    .position(|&call| call == "delete candidate")
                    .unwrap();
                assert!(texture < delete);
            }
        } else if scenario == 8 {
            assert!(!actual.calls.contains(&"delete candidate"));
        } else {
            assert!(actual.calls.is_empty());
        }
    }
    *STATE.lock().unwrap() = Some(initial());
    load(false);
    let mut ctx = Context::new(false);
    with_state(|s| s.error = 0x0502);
    assert!(matches!(
        Texture::try_from_rgba8_immutable(&mut ctx, 2, 2, &bytes)
            .unwrap_err()
            .failure,
        ImmutableTextureFailure::Gl {
            stage: "preexisting immutable texture error",
            code: 0x0502
        }
    ));
    assert!(with_state(|s| s.calls.is_empty()));
}
