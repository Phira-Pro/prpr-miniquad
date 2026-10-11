//! CPU-only transfer guards; this file can be tested directly with rustc.
pub const MAX_CAPACITY: usize = 1024 * 1024;
pub fn capacity_valid(capacity: usize) -> bool {
    capacity != 0 && capacity <= MAX_CAPACITY && capacity <= isize::MAX as usize
}
pub fn es3_version(version: &[u8]) -> bool {
    let Some(version) = version.strip_prefix(b"OpenGL ES ") else {
        return false;
    };
    let Some(dot) = version.iter().position(|&byte| byte == b'.') else {
        return false;
    };
    if dot == 0 {
        return false;
    }
    let mut major = 0u32;
    for &digit in &version[..dot] {
        if !digit.is_ascii_digit() {
            return false;
        }
        let Some(next) = major
            .checked_mul(10)
            .and_then(|value| value.checked_add((digit - b'0') as u32))
        else {
            return false;
        };
        major = next;
    }
    major >= 3
        && version
            .get(dot + 1)
            .is_some_and(|digit| digit.is_ascii_digit())
}
pub fn texture_valid(name: u32, width: u32, height: u32, rgba8: bool) -> bool {
    name != 0
        && rgba8
        && width != 0
        && height != 0
        && width <= i32::MAX as u32
        && height <= i32::MAX as u32
}
fn range_end(capacity: usize, offset: usize, len: usize) -> Option<usize> {
    if !capacity_valid(capacity) || len == 0 {
        return None;
    }
    offset
        .checked_add(len)
        .filter(|&end| end <= capacity && end <= isize::MAX as usize)
}
fn rect_len(
    atlas_width: u32,
    atlas_height: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Option<usize> {
    if x < 0 || y < 0 || width <= 0 || height <= 0 {
        return None;
    }
    if (x as u32).checked_add(width as u32)? > atlas_width
        || (y as u32).checked_add(height as u32)? > atlas_height
    {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}
#[derive(Default)]
pub struct Cursor {
    committed_end: usize,
    pending: Option<(usize, usize)>,
}
impl Cursor {
    pub fn can_write(&self, capacity: usize, offset: usize, len: usize) -> bool {
        self.pending.is_none()
            && offset >= self.committed_end
            && range_end(capacity, offset, len).is_some()
    }
    pub fn written(&mut self, offset: usize, len: usize) {
        self.pending = Some((offset, len));
    }
    pub fn can_upload(
        &self,
        capacity: usize,
        atlas_width: u32,
        atlas_height: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        offset: usize,
        len: usize,
    ) -> bool {
        self.pending == Some((offset, len))
            && range_end(capacity, offset, len).is_some()
            && rect_len(atlas_width, atlas_height, x, y, width, height) == Some(len)
    }
    pub fn uploaded(&mut self, offset: usize, len: usize) {
        self.committed_end = offset + len;
        self.pending = None;
    }
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_es_version_required() {
        for version in [
            b"OpenGL ES 3.0 Vendor".as_slice(),
            b"OpenGL ES 3.2",
            b"OpenGL ES 4.0",
        ] {
            assert!(es3_version(version));
        }
        for version in [
            b"OpenGL ES 2.0".as_slice(),
            b"4.6 Desktop",
            b"WebGL 2.0",
            b"OpenGL ES-CM 3.0",
            b"OpenGL ES 3.",
            b"OpenGL ES x.0",
            b"OpenGL ES 42949672960.0",
            b"",
        ] {
            assert!(!es3_version(version));
        }
    }
    #[test]
    fn bounded_capacity_and_texture_metadata() {
        assert!(capacity_valid(1));
        assert!(capacity_valid(MAX_CAPACITY));
        assert!(!capacity_valid(0));
        assert!(!capacity_valid(MAX_CAPACITY + 1));
        assert!(!capacity_valid(usize::MAX));
        assert!(texture_valid(1, 2048, 2048, true));
        for (name, w, h, rgba) in [
            (0, 2, 2, true),
            (1, 0, 2, true),
            (1, 2, 0, true),
            (1, 2, 2, false),
            (1, u32::MAX, 1, true),
        ] {
            assert!(!texture_valid(name, w, h, rgba));
        }
    }
    #[test]
    fn no_write_without_capacity_or_before_upload() {
        let mut cursor = Cursor::default();
        for (offset, len) in [
            (0, 0),
            (MAX_CAPACITY, 1),
            (usize::MAX, 4),
            (MAX_CAPACITY - 3, 4),
        ] {
            assert!(!cursor.can_write(MAX_CAPACITY, offset, len));
        }
        assert!(cursor.can_write(MAX_CAPACITY, MAX_CAPACITY - 4, 4));
        cursor.written(0, 16);
        assert!(cursor.has_pending());
        assert!(!cursor.can_write(MAX_CAPACITY, 16, 4));
        assert!(!cursor.can_upload(MAX_CAPACITY, 8, 8, 0, 0, 1, 1, 0, 4));
        assert!(!cursor.can_upload(MAX_CAPACITY, 8, 8, 0, 0, 2, 2, 4, 16));
        assert!(cursor.can_upload(MAX_CAPACITY, 8, 8, 0, 0, 2, 2, 0, 16));
    }
    #[test]
    fn committed_bytes_never_overwritten_within_session() {
        let mut cursor = Cursor::default();
        cursor.written(0, 16);
        cursor.uploaded(0, 16);
        assert!(!cursor.has_pending());
        assert!(!cursor.can_write(MAX_CAPACITY, 0, 16));
        assert!(!cursor.can_write(MAX_CAPACITY, 15, 1));
        assert!(cursor.can_write(MAX_CAPACITY, 16, 4));
        assert!(cursor.can_write(MAX_CAPACITY, 128, 4));
        assert!(!cursor.can_upload(MAX_CAPACITY, 8, 8, 0, 0, 2, 2, 0, 16));
    }
    #[test]
    fn rgba_rect_bounds_and_exact_size() {
        let mut cursor = Cursor::default();
        cursor.written(0, 16);
        for (x, y, w, h) in [
            (-1, 0, 2, 2),
            (0, -1, 2, 2),
            (0, 0, 0, 2),
            (0, 0, 2, 0),
            (0, 0, -1, 2),
            (7, 0, 2, 2),
            (0, 7, 2, 2),
            (i32::MAX, 0, i32::MAX, 2),
            (0, 0, 4, 2),
        ] {
            assert!(!cursor.can_upload(MAX_CAPACITY, 8, 8, x, y, w, h, 0, 16));
        }
        assert!(cursor.can_upload(MAX_CAPACITY, 8, 8, 6, 6, 2, 2, 0, 16));
    }
    #[test]
    fn total_publication_capacity_not_only_rect_capacity() {
        let mut cursor = Cursor::default();
        let size = MAX_CAPACITY / 2;
        cursor.written(0, size);
        cursor.uploaded(0, size);
        assert!(cursor.can_write(MAX_CAPACITY, size, size));
        cursor.written(size, size);
        cursor.uploaded(size, size);
        assert!(!cursor.can_write(MAX_CAPACITY, MAX_CAPACITY, 4));
    }
}
