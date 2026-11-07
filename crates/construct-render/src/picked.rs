//! Photos Android's picker returned (`content://` URIs, through the bridge),
//! read into files the engine can stage: JPEG, PNG, WebP and GIF are copied
//! as they are; other formats (HEIC, HEIF, AVIF) are decoded by Android's
//! `ImageDecoder` and written as JPEG. All of it through JNI, from Rust.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jni::objects::{JByteArray, JObject, JObjectArray, JValue};
use jni::{jni_sig, jni_str};

/// Formats the engine decodes itself (client-core's `image` features).
const COPIED: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
/// The largest photo read (the engine's own limit, client-core's
/// `MAX_SOURCE_BYTES`).
const MAX_BYTES: usize = 60 * 1024 * 1024;
/// Copies older than this are deleted when the next photos arrive (the
/// engine has its own copy once a photo is staged).
const MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Each picked URI as a local file, or why it couldn't be read.
pub fn read_all(
    env: &mut jni::Env,
    context: &JObject,
    uris: &JObjectArray,
) -> jni::errors::Result<Vec<Result<PathBuf, String>>> {
    let dir = cache_dir(env, context)?.join("picked");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::android::log(&format!("picked: {}: {e}", dir.display()));
    }
    sweep(&dir);
    let n = uris.len(env)?;
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = uris.get_element(env, i)?;
        let target = dir.join(format!("{stamp}-{i}"));
        let r = env.with_local_frame(16, |env| read_one(env, context, &s, &target));
        out.push(match r {
            Ok(path) => Ok(path),
            Err(e) => {
                env.exception_clear();
                crate::android::log(&format!("picked photo {i}: {e}"));
                Err("That photo couldn't be read.".to_owned())
            }
        });
    }
    Ok(out)
}

/// The app's cache directory.
fn cache_dir(env: &mut jni::Env, context: &JObject) -> jni::errors::Result<PathBuf> {
    let dir = env
        .call_method(
            context,
            jni_str!("getCacheDir"),
            jni_sig!("()Ljava/io/File;"),
            &[],
        )?
        .l()?;
    let path = env
        .call_method(
            &dir,
            jni_str!("getAbsolutePath"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let path = env.cast_local::<jni::objects::JString>(path)?;
    Ok(PathBuf::from(path.try_to_string(env)?))
}

/// Deletes copies older than [`MAX_AGE`].
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > MAX_AGE);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// One picked photo into `target` (with the extension of what it holds).
fn read_one(
    env: &mut jni::Env,
    context: &JObject,
    uri: &JObject,
    target: &Path,
) -> jni::errors::Result<PathBuf> {
    let resolver = env
        .call_method(
            context,
            jni_str!("getContentResolver"),
            jni_sig!("()Landroid/content/ContentResolver;"),
            &[],
        )?
        .l()?;
    let uri = env
        .call_static_method(
            jni_str!("android/net/Uri"),
            jni_str!("parse"),
            jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
            &[JValue::Object(uri)],
        )?
        .l()?;
    let mime = env
        .call_method(
            &resolver,
            jni_str!("getType"),
            jni_sig!("(Landroid/net/Uri;)Ljava/lang/String;"),
            &[JValue::Object(&uri)],
        )?
        .l()?;
    let mime = if mime.is_null() {
        String::new()
    } else {
        env.cast_local::<jni::objects::JString>(mime)?
            .try_to_string(env)?
    };
    // Unknown types are copied too: the engine says whether it can read them.
    if mime.is_empty() || COPIED.contains(&mime.as_str()) {
        let ext = mime.strip_prefix("image/").unwrap_or("img");
        let path = target.with_extension(if ext == "jpeg" { "jpg" } else { ext });
        copy(env, &resolver, &uri, &path)?;
        Ok(path)
    } else {
        let path = target.with_extension("jpg");
        to_jpeg(env, &resolver, &uri, &path)?;
        Ok(path)
    }
}

/// The URI's bytes, as they are, into `path`.
fn copy(
    env: &mut jni::Env,
    resolver: &JObject,
    uri: &JObject,
    path: &Path,
) -> jni::errors::Result<()> {
    let input = env
        .call_method(
            resolver,
            jni_str!("openInputStream"),
            jni_sig!("(Landroid/net/Uri;)Ljava/io/InputStream;"),
            &[JValue::Object(uri)],
        )?
        .l()?;
    if input.is_null() {
        return Err(jni::errors::Error::NullPtr("openInputStream"));
    }
    const CHUNK: usize = 256 * 1024;
    let buf: JByteArray = env.new_byte_array(CHUNK)?;
    let mut chunk = vec![0i8; CHUNK];
    let mut bytes: Vec<u8> = Vec::new();
    let read = loop {
        let n = match env.call_method(
            &input,
            jni_str!("read"),
            jni_sig!("([B)I"),
            &[JValue::Object(&buf)],
        ) {
            Ok(v) => v.i()?,
            Err(e) => break Err(e),
        };
        if n < 0 {
            break Ok(());
        }
        let n = n as usize;
        buf.get_region(env, 0, &mut chunk[..n])?;
        bytes.extend(chunk[..n].iter().map(|b| *b as u8));
        if bytes.len() > MAX_BYTES {
            break Ok(());
        }
    };
    // Closed whatever happened (an exception pending is cleared first).
    if read.is_err() {
        env.exception_clear();
    }
    let _ = env.call_method(&input, jni_str!("close"), jni_sig!("()V"), &[]);
    read?;
    std::fs::write(path, &bytes).map_err(|e| {
        crate::android::log(&format!("picked: {}: {e}", path.display()));
        jni::errors::Error::JniCall(jni::errors::JniError::Unknown)
    })
}

/// A format the engine can't decode, through Android's `ImageDecoder`
/// (upright, as Android shows it), as a JPEG into `path`.
fn to_jpeg(
    env: &mut jni::Env,
    resolver: &JObject,
    uri: &JObject,
    path: &Path,
) -> jni::errors::Result<()> {
    let source = env
        .call_static_method(
            jni_str!("android/graphics/ImageDecoder"),
            jni_str!("createSource"),
            jni_sig!("(Landroid/content/ContentResolver;Landroid/net/Uri;)Landroid/graphics/ImageDecoder$Source;"),
            &[JValue::Object(resolver), JValue::Object(uri)],
        )?
        .l()?;
    let decoded = env
        .call_static_method(
            jni_str!("android/graphics/ImageDecoder"),
            jni_str!("decodeBitmap"),
            jni_sig!("(Landroid/graphics/ImageDecoder$Source;)Landroid/graphics/Bitmap;"),
            &[JValue::Object(&source)],
        )?
        .l()?;
    // A hardware bitmap can't be compressed: copy it to memory first.
    let argb = env
        .get_static_field(
            jni_str!("android/graphics/Bitmap$Config"),
            jni_str!("ARGB_8888"),
            jni_sig!("Landroid/graphics/Bitmap$Config;"),
        )?
        .l()?;
    let bitmap = env
        .call_method(
            &decoded,
            jni_str!("copy"),
            jni_sig!("(Landroid/graphics/Bitmap$Config;Z)Landroid/graphics/Bitmap;"),
            &[JValue::Object(&argb), JValue::Bool(false)],
        )?
        .l()?;
    env.call_method(&decoded, jni_str!("recycle"), jni_sig!("()V"), &[])?;
    let jpeg = env
        .get_static_field(
            jni_str!("android/graphics/Bitmap$CompressFormat"),
            jni_str!("JPEG"),
            jni_sig!("Landroid/graphics/Bitmap$CompressFormat;"),
        )?
        .l()?;
    let name = env.new_string(path.to_string_lossy())?;
    let file = env.new_object(
        jni_str!("java/io/FileOutputStream"),
        jni_sig!("(Ljava/lang/String;)V"),
        &[JValue::Object(&name)],
    )?;
    let ok = env.call_method(
        &bitmap,
        jni_str!("compress"),
        jni_sig!("(Landroid/graphics/Bitmap$CompressFormat;ILjava/io/OutputStream;)Z"),
        &[
            JValue::Object(&jpeg),
            JValue::Int(92),
            JValue::Object(&file),
        ],
    );
    if ok.is_err() {
        env.exception_clear();
    }
    let _ = env.call_method(&file, jni_str!("close"), jni_sig!("()V"), &[]);
    let _ = env.call_method(&bitmap, jni_str!("recycle"), jni_sig!("()V"), &[]);
    if ok?.z()? {
        Ok(())
    } else {
        Err(jni::errors::Error::JniCall(jni::errors::JniError::Unknown))
    }
}
